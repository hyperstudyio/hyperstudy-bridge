use crate::devices::lsl::{
    FrenzLslManager, FrenzProcessManager, InletManager, NeonLslManager, OutletManager,
    StreamResolver, TimeSync,
};
use crate::devices::{BoxedDevice, DeviceInfo, DeviceStatus, DeviceType};
use crate::performance::PerformanceMonitor;
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{broadcast, RwLock};
use tracing::{info, warn};

/// Outcome of `AppState::device_for_disconnect`.
pub enum DisconnectTarget {
    /// A newer connect registered the device; the caller must not touch it.
    Stale,
    /// Nothing is registered under that id.
    Absent,
    /// The device the caller may disconnect (compare with `remove_device_if_same`).
    Device(Arc<RwLock<BoxedDevice>>),
}

#[derive(Clone)]
pub struct AppState {
    pub recording: Arc<crate::recording::RecordingManager>,
    pub devices: Arc<RwLock<HashMap<String, Arc<RwLock<BoxedDevice>>>>>,
    pub connections: Arc<DashMap<String, ConnectionInfo>>,
    pub metrics: Arc<RwLock<Metrics>>,
    pub performance_monitor: Arc<PerformanceMonitor>,
    pub start_time: Instant,
    pub message_count: Arc<AtomicU64>,
    pub last_error: Arc<RwLock<Option<String>>>,
    /// Neon LSL Manager for Pupil Labs Neon eye tracking via LSL
    pub neon_manager: Arc<NeonLslManager>,
    /// FRENZ LSL Manager for Earable FRENZ brainband via LSL
    pub frenz_manager: Arc<FrenzLslManager>,
    /// FRENZ Python bridge process manager (PyApp lifecycle)
    pub frenz_process: Arc<FrenzProcessManager>,
    /// The LSL inlets (streams Bridge receives) and outlets (streams it publishes) shared by the Neon and FRENZ
    /// managers, kept here so the LSL panel can list them.
    pub lsl_inlets: Arc<InletManager>,
    pub lsl_outlets: Arc<OutletManager>,
    /// EyeLink manager for SR Research EyeLink 1000 Plus eye tracking
    pub eyelink_manager: Arc<crate::devices::eyelink::EyeLinkManager>,
    /// Broadcast channel for device status change events
    /// WebSocket connections can subscribe to receive status updates
    device_status_tx: broadcast::Sender<DeviceStatusEvent>,
    /// Last successful `connect` payload per device id, so a device that
    /// dropped out (or was never re-added after a bridge restart) can be
    /// re-connected on demand when a client sends to it.
    pub last_connect_configs: Arc<DashMap<String, serde_json::Value>>,
    /// Neon phone device_id -> recording id this bridge started. Ownership
    /// otherwise lives only inside the PupilDevice instance and is lost on a
    /// disconnect/reconnect or bridge restart, after which the bridge's own
    /// recording reads as "busy" / "not owned" until someone passes force.
    pub owned_recordings: Arc<DashMap<String, String>>,
    /// Per device id, the ticket of the most recently *started* connect. A
    /// connect may only register its device if it still holds the latest
    /// ticket, so a slow connect from an older connection can never overwrite
    /// what a newer connect registered — regardless of whether the server has
    /// observed the old socket's close yet.
    pub connect_tickets: Arc<DashMap<String, u64>>,
    /// Per device id, the ticket of the connect that registered the device
    /// currently (or most recently) in the registry. A connection whose own
    /// connect ticket is older than this must not disconnect the device: a
    /// newer connect (possibly from a connection that replaced it) owns it
    /// now. Deliberately kept after the device is removed: "latest ticket ==
    /// registered ticket" then means "no explicit connect is pending", which
    /// is what lets auto-reconnect run again after a plain disconnect.
    pub registered_tickets: Arc<DashMap<String, u64>>,
    /// When each device's latest connect ticket was issued. Together with
    /// `registered_tickets` this tells whether an explicit connect is still in
    /// flight (issued recently, not yet registered), which the opportunistic
    /// auto-connect path must yield to.
    pub ticket_issued_at: Arc<DashMap<String, Instant>>,
    /// Highest connect ticket per device id known to have finished WITHOUT
    /// registering (failed, refused, superseded, or abandoned by its
    /// connection). Maintained by `ConnectAttempt`'s Drop, so every exit path
    /// of a connect handler counts. Lets auto-reconnect run right after a
    /// failed explicit connect instead of waiting out the window.
    pub finished_tickets: Arc<DashMap<String, u64>>,
}

/// RAII marker for one explicit connect attempt. Create it right after
/// `AppState::begin_connect`; call `registered()` once the device is in the
/// registry. If it is dropped without that — early return, error, superseded,
/// panic — the ticket is marked finished so it no longer counts as an
/// in-flight explicit connect.
pub struct ConnectAttempt {
    state: Arc<AppState>,
    id: String,
    ticket: u64,
    registered: bool,
}

impl ConnectAttempt {
    pub fn new(state: Arc<AppState>, id: &str, ticket: u64) -> Self {
        Self {
            state,
            id: id.to_string(),
            ticket,
            registered: false,
        }
    }

    pub fn registered(&mut self) {
        self.registered = true;
    }
}

impl Drop for ConnectAttempt {
    fn drop(&mut self) {
        if !self.registered {
            self.state.mark_connect_finished(&self.id, self.ticket);
        }
    }
}

/// An explicit connect that has not registered within this window is
/// considered finished (failed or abandoned): every connect path has a shorter
/// timeout than this, so auto-connect is never blocked indefinitely by a
/// connect that never completed.
pub const EXPLICIT_CONNECT_WINDOW: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectionInfo {
    pub id: String,
    pub client_id: String,
    pub connected_at: u64,
    pub last_activity: u64,
    pub message_count: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Metrics {
    pub total_messages: u64,
    pub total_errors: u64,
    pub total_connections: u64,
    pub uptime_seconds: u64,
    pub device_metrics: Vec<DeviceMetrics>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceMetrics {
    pub device_id: String,
    pub messages_sent: u64,
    pub messages_received: u64,
    pub errors: u64,
    pub last_latency_ms: f64,
}

/// Event broadcast when a device's status changes
#[derive(Debug, Clone, Serialize)]
pub struct DeviceStatusEvent {
    pub device_id: String,
    pub device_type: DeviceType,
    pub status: DeviceStatus,
    pub reason: String,
    pub timestamp: u64,
    /// Device metadata at the time of the change (Neon device_id/IP, recording
    /// state…) so every client — not just the one that issued the command —
    /// can see *which* hardware is behind the status.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub info: Option<serde_json::Value>,
}

impl DeviceStatusEvent {
    pub fn disconnected(device_id: String, device_type: DeviceType, reason: &str) -> Self {
        Self::with_status(
            device_id,
            device_type,
            DeviceStatus::Disconnected,
            reason,
            None,
        )
    }

    pub fn connected(device_id: String, device_type: DeviceType, reason: &str) -> Self {
        Self::with_status(
            device_id,
            device_type,
            DeviceStatus::Connected,
            reason,
            None,
        )
    }

    pub fn connected_with_info(
        device_id: String,
        device_type: DeviceType,
        reason: &str,
        info: serde_json::Value,
    ) -> Self {
        Self::with_status(
            device_id,
            device_type,
            DeviceStatus::Connected,
            reason,
            Some(info),
        )
    }

    fn with_status(
        device_id: String,
        device_type: DeviceType,
        status: DeviceStatus,
        reason: &str,
        info: Option<serde_json::Value>,
    ) -> Self {
        Self {
            device_id,
            device_type,
            status,
            reason: reason.to_string(),
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
            info,
        }
    }
}

/// Extract `(phone device_id, active recording id, owned)` from a Pupil
/// device's `get_info().metadata`. Pure so it can be tested without a device.
pub fn pupil_recording_state_from_info(
    info: &serde_json::Value,
) -> Option<(String, Option<String>, bool)> {
    let phone = info.get("device_id")?.as_str()?.to_string();
    let recording = info
        .get("recording_id")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let owned = info
        .get("recording_owned")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    Some((phone, recording, owned))
}

impl std::fmt::Debug for AppState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut s = f.debug_struct("AppState");
        s.field("devices", &self.devices)
            .field("connections", &self.connections)
            .field("metrics", &self.metrics)
            .field("start_time", &self.start_time)
            .field("message_count", &self.message_count)
            .field("last_error", &self.last_error)
            .field("neon_manager", &self.neon_manager)
            .field("frenz_manager", &self.frenz_manager)
            .field("frenz_process", &self.frenz_process)
            .field("eyelink_manager", &"EyeLinkManager");
        s.field(
            "device_status_subscribers",
            &self.device_status_tx.receiver_count(),
        )
        .finish()
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

impl AppState {
    /// Capacity for device status broadcast channel
    const STATUS_BROADCAST_CAPACITY: usize = 16;

    pub fn new() -> Self {
        // Create shared LSL infrastructure for Neon and FRENZ managers
        let time_sync = Arc::new(TimeSync::new(true));
        let resolver = Arc::new(StreamResolver::new(5.0));
        let inlet_manager = Arc::new(InletManager::new(time_sync.clone()));
        let outlet_manager = Arc::new(OutletManager::new(time_sync));
        let neon_manager = Arc::new(NeonLslManager::new(resolver.clone(), inlet_manager.clone()));
        let frenz_manager = Arc::new(FrenzLslManager::new(
            resolver,
            inlet_manager.clone(),
            outlet_manager.clone(),
        ));
        let frenz_process = Arc::new(FrenzProcessManager::new());

        let eyelink_manager = Arc::new(crate::devices::eyelink::EyeLinkManager::new());

        // Create broadcast channel for device status events
        let (device_status_tx, _) = broadcast::channel(Self::STATUS_BROADCAST_CAPACITY);

        Self {
            devices: Arc::new(RwLock::new(HashMap::new())),
            connections: Arc::new(DashMap::new()),
            last_connect_configs: Arc::new(DashMap::new()),
            owned_recordings: Arc::new(DashMap::new()),
            connect_tickets: Arc::new(DashMap::new()),
            registered_tickets: Arc::new(DashMap::new()),
            ticket_issued_at: Arc::new(DashMap::new()),
            finished_tickets: Arc::new(DashMap::new()),
            metrics: Arc::new(RwLock::new(Metrics::default())),
            performance_monitor: Arc::new(PerformanceMonitor::new()),
            start_time: Instant::now(),
            message_count: Arc::new(AtomicU64::new(0)),
            last_error: Arc::new(RwLock::new(None)),
            neon_manager,
            frenz_manager,
            frenz_process,
            lsl_inlets: inlet_manager,
            lsl_outlets: outlet_manager,
            recording: Arc::new(crate::recording::RecordingManager::default()),
            eyelink_manager,
            device_status_tx,
        }
    }

    /// What Bridge itself does on LSL: the streams it receives (FRENZ, Neon) and the ones it publishes (markers).
    pub async fn lsl_bridge_streams(&self) -> serde_json::Value {
        fn entries(stats: serde_json::Value, key: &str) -> Vec<serde_json::Value> {
            let mut list: Vec<_> = stats
                .get(key)
                .and_then(|m| m.as_object())
                .map(|m| m.values().cloned().collect())
                .unwrap_or_default();
            list.sort_by(|a: &serde_json::Value, b| a["name"].as_str().cmp(&b["name"].as_str()));
            list
        }
        serde_json::json!({
            "receiving": entries(self.lsl_inlets.get_all_stats().await, "inlets"),
            "publishing": entries(self.lsl_outlets.get_all_stats().await, "outlets"),
        })
    }

    /// Subscribe to device status change events.
    ///
    /// Returns a receiver that will receive `DeviceStatusEvent` notifications
    /// when devices connect, disconnect, or change status.
    pub fn subscribe_device_status(&self) -> broadcast::Receiver<DeviceStatusEvent> {
        self.device_status_tx.subscribe()
    }

    /// Broadcast a device status change event to all subscribers.
    ///
    /// This is used to notify WebSocket clients when device status changes.
    pub fn broadcast_device_status(&self, event: DeviceStatusEvent) {
        // It's OK if there are no subscribers - send returns error but we ignore it
        let _ = self.device_status_tx.send(event);
    }

    /// Register `device` under `id` unconditionally (native/Tauri paths). Takes
    /// a fresh connect ticket so that any WebSocket connection whose connect
    /// registered the previous device is no longer considered its owner.
    pub async fn add_device(&self, id: String, device: BoxedDevice) {
        let mut devices = self.devices.write().await;
        let ticket = self.issue_ticket_locked(&id);
        devices.insert(id.clone(), Arc::new(RwLock::new(device)));
        self.registered_tickets.insert(id.clone(), ticket);
        drop(devices);

        // Add device to performance monitoring
        self.performance_monitor.add_device(id).await;
    }

    /// Opportunistic registration (Kernel auto-reconnect on send). Registers
    /// `device` only if nothing is registered under `id` AND no explicit
    /// connect for `id` is in flight — whether that explicit connect started
    /// before or after the auto-connect. The operator's explicit connect (with
    /// the configuration they just entered) must never be pre-empted by a
    /// reconnect from remembered configuration. Hands `device` back (`Err`) when it
    /// yields so the caller can disconnect it cleanly. Takes no ticket of its own: it registers under
    /// the current latest ticket, so ownership semantics for disconnects are
    /// unchanged.
    pub async fn add_device_if_absent(
        &self,
        id: String,
        device: BoxedDevice,
    ) -> Result<(), BoxedDevice> {
        let mut devices = self.devices.write().await;
        if devices.contains_key(&id) || self.explicit_connect_in_flight_locked(&id) {
            return Err(device);
        }
        let latest = self.connect_tickets.get(&id).map(|v| *v).unwrap_or(0);
        devices.insert(id.clone(), Arc::new(RwLock::new(device)));
        self.registered_tickets.insert(id.clone(), latest);
        drop(devices);
        self.performance_monitor.add_device(id).await;
        Ok(())
    }

    /// Next connect ticket for `id`. Callers must hold the `devices` write lock
    /// so ticket issuance is ordered with registrations.
    fn issue_ticket_locked(&self, id: &str) -> u64 {
        let mut entry = self.connect_tickets.entry(id.to_string()).or_insert(0);
        *entry += 1;
        self.ticket_issued_at.insert(id.to_string(), Instant::now());
        *entry
    }

    /// True while an explicit connect for `id` may still be running: its
    /// ticket is newer than whatever is registered and was issued within
    /// `EXPLICIT_CONNECT_WINDOW`. Callers hold the `devices` lock.
    fn explicit_connect_in_flight_locked(&self, id: &str) -> bool {
        let latest = self.connect_tickets.get(id).map(|v| *v).unwrap_or(0);
        let registered = self.registered_tickets.get(id).map(|v| *v).unwrap_or(0);
        let finished = self.finished_tickets.get(id).map(|v| *v).unwrap_or(0);
        if latest <= registered.max(finished) {
            return false;
        }
        self.ticket_issued_at
            .get(id)
            .map(|t| t.elapsed() < EXPLICIT_CONNECT_WINDOW)
            .unwrap_or(false)
    }

    /// Track which Neon recording this bridge owns, from a Pupil device's
    /// metadata after a command (see `pupil_recording_state_from_info`).
    pub fn note_pupil_recording_state(&self, info: &serde_json::Value) {
        if let Some((phone, recording, owned)) = pupil_recording_state_from_info(info) {
            match recording {
                Some(id) if owned => {
                    self.owned_recordings.insert(phone, id);
                }
                Some(_) => {}
                None => {
                    self.owned_recordings.remove(&phone);
                }
            }
        }
    }

    /// The recording id this bridge started on `phone`, if still remembered.
    pub fn owned_recording(&self, phone: &str) -> Option<String> {
        self.owned_recordings.get(phone).map(|v| v.value().clone())
    }

    /// Start a connect for `id`: returns a ticket that identifies this attempt
    /// as the newest. Issued under the registry lock — the same lock
    /// `add_device_if_latest` checks under — so "newest ticket" and
    /// "registered device" are always observed consistently. Call only for a
    /// validated attempt (known device type, usable config) and before any
    /// await in the connect path.
    pub async fn begin_connect(&self, id: &str) -> u64 {
        let _registry = self.devices.write().await;
        self.issue_ticket_locked(id)
    }

    /// Record that connect `ticket` for `id` ended without registering.
    pub fn mark_connect_finished(&self, id: &str, ticket: u64) {
        let mut entry = self.finished_tickets.entry(id.to_string()).or_insert(0);
        if ticket > *entry {
            *entry = ticket;
        }
    }

    /// Ticket of the connect that registered the device currently under `id`.
    pub fn registered_ticket(&self, id: &str) -> Option<u64> {
        self.registered_tickets.get(id).map(|v| *v)
    }

    /// True when a connection that last connected `id` with `own_ticket` would
    /// be disconnecting a device that a *newer* connect registered — i.e. its
    /// disconnect is stale and must not touch the registry. A connection that
    /// never connected `id` (`None`) is never considered stale: a fresh page
    /// may legitimately disconnect a device left connected by a previous one.
    pub fn is_stale_disconnect(&self, id: &str, own_ticket: Option<u64>) -> bool {
        match (own_ticket, self.registered_ticket(id)) {
            (Some(own), Some(registered)) => registered > own,
            _ => false,
        }
    }

    /// Select the device a connection may disconnect. The staleness check and
    /// the device selection happen together under the registry lock, so a
    /// newer connect cannot register between "you may disconnect" and "here
    /// is the device" — registrations take the write lock and update
    /// `registered_tickets` while holding it.
    pub async fn device_for_disconnect(
        &self,
        id: &str,
        own_ticket: Option<u64>,
    ) -> DisconnectTarget {
        let devices = self.devices.read().await;
        let Some(device) = devices.get(id) else {
            return DisconnectTarget::Absent;
        };
        if self.is_stale_disconnect(id, own_ticket) {
            return DisconnectTarget::Stale;
        }
        DisconnectTarget::Device(device.clone())
    }

    /// Register `device` under `id` only if `ticket` is still the newest connect
    /// for that id and the owning connection has not closed. Decided under the
    /// registry lock. Overlapping connections are ordered by ticket, not by
    /// when the server happened to notice a socket close: a connect started
    /// later always wins over one started earlier, even if the earlier one
    /// finishes last. Returns the device back on refusal so the caller can
    /// release it. On success returns the device that was registered under
    /// `id` before (if any) so the caller can disconnect it — a replaced
    /// device must not keep its hardware connection open.
    pub async fn add_device_if_latest(
        &self,
        id: String,
        device: BoxedDevice,
        ticket: u64,
        closed: &AtomicBool,
    ) -> Result<Option<Arc<RwLock<BoxedDevice>>>, BoxedDevice> {
        let mut devices = self.devices.write().await;
        let latest = self.connect_tickets.get(&id).map(|v| *v).unwrap_or(0);
        if closed.load(std::sync::atomic::Ordering::Relaxed) || ticket != latest {
            return Err(device);
        }
        let replaced = devices.insert(id.clone(), Arc::new(RwLock::new(device)));
        self.registered_tickets.insert(id.clone(), ticket);
        drop(devices);
        self.performance_monitor.add_device(id).await;
        Ok(replaced)
    }

    /// Remove the registration for `id` only if it is still `expected` — the
    /// exact device a caller disconnected — so a slow disconnect from an old
    /// connection cannot unregister the fresh device a newer one registered.
    pub async fn remove_device_if_same(
        &self,
        id: &str,
        expected: &Arc<RwLock<BoxedDevice>>,
    ) -> bool {
        let mut devices = self.devices.write().await;
        match devices.get(id) {
            Some(current) if Arc::ptr_eq(current, expected) => {
                devices.remove(id);
                drop(devices);
                self.performance_monitor.remove_device(id).await;
                true
            }
            _ => false,
        }
    }

    pub async fn remove_device(&self, id: &str) -> Option<Arc<RwLock<BoxedDevice>>> {
        let mut devices = self.devices.write().await;
        let result = devices.remove(id);

        // Remove device from performance monitoring
        self.performance_monitor.remove_device(id).await;

        result
    }

    pub async fn get_device(&self, id: &str) -> Option<Arc<RwLock<BoxedDevice>>> {
        let devices = self.devices.read().await;
        devices.get(id).cloned()
    }

    pub async fn list_devices(&self) -> Vec<DeviceInfo> {
        let mut device_infos = Vec::new();
        let devices = self.devices.read().await;

        for (_, device_lock) in devices.iter() {
            let device = device_lock.read().await;
            device_infos.push(device.get_info());
        }

        device_infos
    }

    pub async fn get_device_status(&self, id: &str) -> Option<DeviceStatus> {
        if let Some(device_lock) = self.get_device(id).await {
            let device = device_lock.read().await;
            Some(device.get_status())
        } else {
            None
        }
    }

    pub fn add_connection(&self, id: String, client_id: String) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let info = ConnectionInfo {
            id: id.clone(),
            client_id,
            connected_at: now,
            last_activity: now,
            message_count: 0,
        };

        self.connections.insert(id, info);

        // Record WebSocket connection in performance monitoring
        self.performance_monitor.record_websocket_connection(true);
    }

    pub fn remove_connection(&self, id: &str) {
        self.connections.remove(id);

        // Record WebSocket disconnection in performance monitoring
        self.performance_monitor.record_websocket_connection(false);
    }

    pub fn update_connection_activity(&self, id: &str) {
        if let Some(mut entry) = self.connections.get_mut(id) {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();

            entry.last_activity = now;
            entry.message_count += 1;
        }
    }

    pub async fn update_metrics<F>(&self, updater: F)
    where
        F: FnOnce(&mut Metrics),
    {
        let mut metrics = self.metrics.write().await;
        updater(&mut metrics);
    }

    pub async fn get_metrics(&self) -> Metrics {
        self.metrics.read().await.clone()
    }

    pub async fn cleanup_stale_connections(&self, max_idle_seconds: u64) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let mut to_remove = Vec::new();

        for entry in self.connections.iter() {
            if now - entry.value().last_activity > max_idle_seconds {
                to_remove.push(entry.key().clone());
            }
        }

        for id in to_remove {
            self.remove_connection(&id);
        }
    }

    pub fn get_uptime(&self) -> Duration {
        self.start_time.elapsed()
    }

    pub async fn get_message_count(&self) -> u64 {
        self.message_count.load(Ordering::Relaxed)
    }

    pub fn increment_message_count(&self) {
        self.message_count.fetch_add(1, Ordering::Relaxed);

        // Record bridge message in performance monitoring
        self.performance_monitor.record_bridge_message();
    }

    pub async fn set_last_error(&self, error: Option<String>) {
        let mut last_error = self.last_error.write().await;
        *last_error = error;
    }

    pub async fn get_last_error(&self) -> Option<String> {
        self.last_error.read().await.clone()
    }

    pub async fn get_device_metrics(&self, device_id: &str) -> Option<DeviceMetrics> {
        let metrics = self.metrics.read().await;
        metrics
            .device_metrics
            .iter()
            .find(|m| m.device_id == device_id)
            .cloned()
    }

    /// Record device operation with performance tracking
    pub async fn record_device_operation(
        &self,
        device_id: &str,
        latency: Duration,
        bytes_sent: u64,
        bytes_received: u64,
    ) {
        self.performance_monitor
            .record_device_operation(device_id, latency, bytes_sent, bytes_received)
            .await;
    }

    /// Record device error with performance tracking
    pub async fn record_device_error(&self, device_id: &str, error_msg: &str) {
        self.performance_monitor
            .record_device_error(device_id, error_msg)
            .await;
        self.set_last_error(Some(error_msg.to_string())).await;
    }

    /// Record device connection attempt with performance tracking
    /// Remember the payload that last connected `device_id` successfully.
    pub fn remember_connect_config(&self, device_id: &str, config: serde_json::Value) {
        self.last_connect_configs
            .insert(device_id.to_string(), config);
    }

    /// The payload that last connected `device_id`, if any.
    pub fn last_connect_config(&self, device_id: &str) -> Option<serde_json::Value> {
        self.last_connect_configs
            .get(device_id)
            .map(|v| v.value().clone())
    }

    pub async fn record_connection_attempt(&self, device_id: &str, success: bool) {
        self.performance_monitor
            .record_connection_attempt(device_id, success)
            .await;
    }

    /// Get comprehensive performance metrics
    pub async fn get_performance_metrics(&self) -> crate::performance::PerformanceMetrics {
        self.performance_monitor.get_metrics().await
    }

    /// Get device-specific performance metrics
    pub async fn get_device_performance_metrics(
        &self,
        device_id: &str,
    ) -> Option<crate::performance::DevicePerformanceMetrics> {
        self.performance_monitor.get_device_metrics(device_id).await
    }

    /// Get performance summary for monitoring dashboard
    pub async fn get_performance_summary(&self) -> serde_json::Value {
        self.performance_monitor.get_performance_summary().await
    }

    /// Check TTL latency compliance (<1ms)
    pub async fn check_ttl_latency_compliance(&self, device_id: &str) -> Option<bool> {
        self.performance_monitor
            .check_ttl_latency_compliance(device_id)
            .await
    }

    /// Handle USB disconnect event for TTL device.
    ///
    /// This is called by the USB monitor when it detects that a TTL device
    /// has been physically unplugged. It updates the device status to Disconnected
    /// and removes the device from the active devices map.
    ///
    /// Returns true if a TTL device was found and updated, false otherwise.
    pub async fn handle_ttl_usb_disconnect(&self) -> bool {
        let devices = self.devices.read().await;

        // Find all TTL devices
        let ttl_device_ids: Vec<String> = {
            let mut ids = Vec::new();
            for (id, device_lock) in devices.iter() {
                let device = device_lock.read().await;
                if device.get_info().device_type == DeviceType::TTL {
                    ids.push(id.clone());
                }
            }
            ids
        };
        drop(devices);

        if ttl_device_ids.is_empty() {
            return false;
        }

        let mut any_updated = false;

        // Disconnect each TTL device
        for device_id in ttl_device_ids {
            // Re-verify device exists and is still TTL type (avoid race condition)
            let should_disconnect = if let Some(device_lock) = self.get_device(&device_id).await {
                let device = device_lock.read().await;
                device.get_info().device_type == DeviceType::TTL
            } else {
                // Device was already removed by another task
                false
            };

            if !should_disconnect {
                continue;
            }

            info!(
                device = "ttl",
                "USB disconnect detected, marking device {} as disconnected", device_id
            );

            // Try to call disconnect on the device (best effort - the hardware is gone)
            if let Some(device_lock) = self.get_device(&device_id).await {
                let mut device = device_lock.write().await;
                // The disconnect call may fail since the device is gone, but we try anyway
                let _ = device.disconnect().await;
            }

            // Remove the device from the registry
            self.remove_device(&device_id).await;

            // Record the error
            self.record_device_error(
                &device_id,
                "Device physically disconnected (USB unplug detected)",
            )
            .await;

            // Broadcast status change to WebSocket clients
            self.broadcast_device_status(DeviceStatusEvent::disconnected(
                device_id.clone(),
                DeviceType::TTL,
                "USB device unplugged",
            ));

            warn!(
                device = "ttl",
                "TTL device {} removed due to USB disconnect", device_id
            );

            any_updated = true;
        }

        any_updated
    }
}

#[cfg(test)]
mod device_identity_tests {
    use super::*;
    use crate::devices::mock::MockDevice;
    use serde_json::json;

    fn mock(name: &str) -> BoxedDevice {
        Box::new(MockDevice::new(name.to_string(), name.to_string()))
    }

    #[tokio::test]
    async fn a_later_connect_wins_even_if_an_earlier_one_finishes_last() {
        let state = AppState::new();
        let open = AtomicBool::new(false);
        // Old connection starts connecting first, new connection second.
        let old_ticket = state.begin_connect("pupil").await;
        let new_ticket = state.begin_connect("pupil").await;
        // The newer connect finishes first and registers phone B.
        assert!(state
            .add_device_if_latest("pupil".into(), mock("phone-b"), new_ticket, &open)
            .await
            .is_ok());
        // The older connect finishes last: refused, device handed back.
        let refused = state
            .add_device_if_latest("pupil".into(), mock("phone-a"), old_ticket, &open)
            .await;
        assert!(refused.is_err());
        let registered = state
            .get_device("pupil")
            .await
            .expect("phone-b stays registered");
        assert_eq!(registered.read().await.get_info().name, "phone-b");
    }

    #[tokio::test]
    async fn a_closed_connection_cannot_register_even_with_the_latest_ticket() {
        let state = AppState::new();
        let closed = AtomicBool::new(true);
        let ticket = state.begin_connect("kernel").await;
        assert!(state
            .add_device_if_latest("kernel".into(), mock("k"), ticket, &closed)
            .await
            .is_err());
        assert!(state.get_device("kernel").await.is_none());
    }

    #[tokio::test]
    async fn a_disconnect_from_an_older_connect_is_stale_once_a_newer_connect_registered() {
        let state = AppState::new();
        let open = AtomicBool::new(false);
        // Connection A connects the kernel.
        let a_ticket = state.begin_connect("kernel").await;
        state
            .add_device_if_latest("kernel".into(), mock("from-a"), a_ticket, &open)
            .await
            .unwrap();
        // A's own disconnect would be fine right now.
        assert!(!state.is_stale_disconnect("kernel", Some(a_ticket)));
        // Connection B (the reloaded page) reconnects before the server has
        // observed A's socket close; B's device replaces A's.
        let b_ticket = state.begin_connect("kernel").await;
        state
            .add_device_if_latest("kernel".into(), mock("from-b"), b_ticket, &open)
            .await
            .unwrap();
        // A's queued disconnect is now stale: it must not touch B's device.
        assert!(matches!(
            state.device_for_disconnect("kernel", Some(a_ticket)).await,
            DisconnectTarget::Stale
        ));
        // B itself, and a connection that never connected the kernel, get B's device.
        let b_device = match state.device_for_disconnect("kernel", Some(b_ticket)).await {
            DisconnectTarget::Device(d) => d,
            _ => panic!("B may disconnect its own device"),
        };
        assert_eq!(b_device.read().await.get_info().name, "from-b");
        assert!(matches!(
            state.device_for_disconnect("kernel", None).await,
            DisconnectTarget::Device(_)
        ));
        // Once B's device is gone there is nothing to be stale against.
        assert!(state.remove_device_if_same("kernel", &b_device).await);
        assert!(matches!(
            state.device_for_disconnect("kernel", Some(a_ticket)).await,
            DisconnectTarget::Absent
        ));
        assert!(state.get_device("kernel").await.is_none());
    }

    #[tokio::test]
    async fn a_native_replacement_makes_the_websocket_connects_disconnect_stale() {
        let state = AppState::new();
        let open = AtomicBool::new(false);
        let ws_ticket = state.begin_connect("kernel").await;
        state
            .add_device_if_latest("kernel".into(), mock("from-ws"), ws_ticket, &open)
            .await
            .unwrap();
        // The Tauri UI replaces the device through the unconditional path.
        state.add_device("kernel".into(), mock("from-ui")).await;
        assert!(matches!(
            state.device_for_disconnect("kernel", Some(ws_ticket)).await,
            DisconnectTarget::Stale
        ));
        // And an in-flight WebSocket connect that started before it is refused.
        assert!(state
            .add_device_if_latest("kernel".into(), mock("late"), ws_ticket, &open)
            .await
            .is_err());
        // The auto-connect path also takes ownership when it registers.
        let ui = state.get_device("kernel").await.unwrap();
        state.remove_device_if_same("kernel", &ui).await;
        assert!(state
            .add_device_if_absent("kernel".into(), mock("auto"))
            .await
            .is_ok());
        assert!(matches!(
            state.device_for_disconnect("kernel", Some(ws_ticket)).await,
            DisconnectTarget::Stale
        ));
        assert!(matches!(
            state.device_for_disconnect("kernel", None).await,
            DisconnectTarget::Device(_)
        ));
    }

    #[tokio::test]
    async fn an_auto_connect_yields_to_any_explicit_connect_in_flight() {
        let state = AppState::new();
        let open = AtomicBool::new(false);
        // Nothing registered, no explicit connect pending: auto may register.
        assert!(state
            .add_device_if_absent("kernel".into(), mock("auto-0"))
            .await
            .is_ok());
        let auto0 = state.get_device("kernel").await.unwrap();
        assert!(state.remove_device_if_same("kernel", &auto0).await);

        // An explicit connect is in flight (ticket issued, not registered)...
        let explicit_ticket = state.begin_connect("kernel").await;
        // ...so an auto-connect that finishes now must yield, whether it
        // started before or after the explicit one.
        assert!(state
            .add_device_if_absent("kernel".into(), mock("auto-1"))
            .await
            .is_err());
        assert!(state.get_device("kernel").await.is_none());
        // The explicit connect registers normally.
        assert!(state
            .add_device_if_latest("kernel".into(), mock("explicit"), explicit_ticket, &open)
            .await
            .is_ok());
        let registered = state.get_device("kernel").await.unwrap();
        assert_eq!(registered.read().await.get_info().name, "explicit");
        // A registered device is never replaced by an auto-connect.
        assert!(state
            .add_device_if_absent("kernel".into(), mock("auto-2"))
            .await
            .is_err());
        // Once the explicit device is gone (and no connect is pending),
        // auto-connect works again.
        assert!(state.remove_device_if_same("kernel", &registered).await);
        assert!(state
            .add_device_if_absent("kernel".into(), mock("auto-3"))
            .await
            .is_ok());
        // The auto-registered device is owned by the latest ticket: the
        // explicit connection's disconnect is still allowed (same ticket).
        assert!(matches!(
            state
                .device_for_disconnect("kernel", Some(explicit_ticket))
                .await,
            DisconnectTarget::Device(_)
        ));
    }

    #[tokio::test]
    async fn a_failed_explicit_connect_stops_blocking_auto_connect_after_the_window() {
        let state = AppState::new();
        let _abandoned = state.begin_connect("kernel").await;
        assert!(state
            .add_device_if_absent("kernel".into(), mock("auto"))
            .await
            .is_err());
        // Pretend the ticket was issued long ago.
        state.ticket_issued_at.insert(
            "kernel".into(),
            Instant::now() - EXPLICIT_CONNECT_WINDOW - Duration::from_secs(1),
        );
        assert!(state
            .add_device_if_absent("kernel".into(), mock("auto"))
            .await
            .is_ok());
    }

    #[tokio::test]
    async fn a_failed_explicit_connect_unblocks_auto_connect_immediately() {
        let state = Arc::new(AppState::new());
        let ticket = state.begin_connect("kernel").await;
        {
            // The handler's guard: dropped on any exit without registering.
            let _attempt = ConnectAttempt::new(state.clone(), "kernel", ticket);
            assert!(state
                .add_device_if_absent("kernel".into(), mock("auto"))
                .await
                .is_err());
        }
        // Explicit connect failed (guard dropped): markers must be able to
        // auto-reconnect right away, not after the 60 s window.
        assert!(state
            .add_device_if_absent("kernel".into(), mock("auto"))
            .await
            .is_ok());
        // A guard that registered does not mark its ticket finished, and a
        // later explicit connect is again respected while pending.
        let device = state.get_device("kernel").await.unwrap();
        state.remove_device_if_same("kernel", &device).await;
        let t2 = state.begin_connect("kernel").await;
        let mut attempt = ConnectAttempt::new(state.clone(), "kernel", t2);
        assert!(state
            .add_device_if_absent("kernel".into(), mock("auto"))
            .await
            .is_err());
        state
            .add_device_if_latest(
                "kernel".into(),
                mock("explicit"),
                t2,
                &AtomicBool::new(false),
            )
            .await
            .unwrap();
        attempt.registered();
        drop(attempt);
        assert_eq!(
            state.finished_tickets.get("kernel").map(|v| *v),
            Some(ticket)
        );
    }

    #[tokio::test]
    async fn a_latest_connect_hands_back_the_device_it_replaced() {
        let state = AppState::new();
        let open = AtomicBool::new(false);
        assert!(state
            .add_device_if_absent("kernel".into(), mock("auto"))
            .await
            .is_ok());
        let t = state.begin_connect("kernel").await;
        let replaced = state
            .add_device_if_latest("kernel".into(), mock("explicit"), t, &open)
            .await
            .unwrap()
            .expect("the auto-registered device is handed back");
        assert_eq!(replaced.read().await.get_info().name, "auto");
    }

    #[tokio::test]
    async fn disconnect_removes_only_the_exact_device_it_disconnected() {
        let state = AppState::new();
        let open = AtomicBool::new(false);
        let t1 = state.begin_connect("kernel").await;
        state
            .add_device_if_latest("kernel".into(), mock("first"), t1, &open)
            .await
            .unwrap();
        let first = state.get_device("kernel").await.unwrap();
        // A newer connection replaces it.
        let t2 = state.begin_connect("kernel").await;
        state
            .add_device_if_latest("kernel".into(), mock("second"), t2, &open)
            .await
            .unwrap();
        // The old connection's slow disconnect must not remove the replacement.
        assert!(!state.remove_device_if_same("kernel", &first).await);
        let second = state
            .get_device("kernel")
            .await
            .expect("second stays registered");
        assert_eq!(second.read().await.get_info().name, "second");
        assert!(state.remove_device_if_same("kernel", &second).await);
        assert!(state.get_device("kernel").await.is_none());
    }

    #[test]
    fn recording_state_is_read_from_pupil_metadata() {
        let info = json!({ "device_id": "a41fe4fe2bccf6c3", "recording_id": "rec-1", "recording_owned": true });
        assert_eq!(
            pupil_recording_state_from_info(&info),
            Some(("a41fe4fe2bccf6c3".into(), Some("rec-1".into()), true))
        );
        let none = json!({ "device_id": "a41fe4fe2bccf6c3", "recording_id": null, "recording_owned": false });
        assert_eq!(
            pupil_recording_state_from_info(&none),
            Some(("a41fe4fe2bccf6c3".into(), None, false))
        );
        assert_eq!(
            pupil_recording_state_from_info(&json!({ "recording_id": "x" })),
            None
        );
    }

    #[test]
    fn connected_event_carries_info_and_disconnected_does_not() {
        let with = DeviceStatusEvent::connected_with_info(
            "pupil".into(),
            DeviceType::Pupil,
            "connected",
            json!({ "device_id": "abc" }),
        );
        let value = serde_json::to_value(&with).unwrap();
        assert_eq!(value["info"]["device_id"], "abc");
        assert_eq!(value["status"], "Connected");

        let without = DeviceStatusEvent::disconnected("kernel".into(), DeviceType::Kernel, "bye");
        let value = serde_json::to_value(&without).unwrap();
        assert!(value.get("info").is_none());
    }
}

#[cfg(test)]
mod lsl_panel_tests {
    use super::*;

    /// The LSL panel listed nothing because nothing could see Bridge's own inlets and outlets. An outlet created by
    /// a device manager through the shared manager must appear in what the panel reads.
    #[tokio::test]
    async fn streams_bridge_publishes_through_the_shared_manager_are_listed() {
        let state = AppState::new();
        assert_eq!(
            state.lsl_bridge_streams().await["publishing"]
                .as_array()
                .unwrap()
                .len(),
            0
        );
        state
            .lsl_outlets
            .create_device_outlet("panel_test", "ttl", None)
            .await
            .unwrap();
        let listed = state.lsl_bridge_streams().await;
        let publishing = listed["publishing"].as_array().unwrap();
        assert_eq!(publishing.len(), 1);
        assert!(publishing[0]["name"]
            .as_str()
            .unwrap()
            .contains("panel_test"));
        assert_eq!(listed["receiving"].as_array().unwrap().len(), 0);
    }
}

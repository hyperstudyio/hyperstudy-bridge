//! Pupil Labs Neon LSL Integration
//!
//! This module provides automatic discovery and data consumption from Pupil Labs Neon
//! eye trackers streaming via Lab Streaming Layer (LSL).
//!
//! # Neon LSL Streams
//!
//! When "Stream over LSL" is enabled in the Neon Companion App, two streams are created:
//! - `{DeviceName}_Neon Gaze` - Float32 gaze data at 200Hz
//! - `{DeviceName}_Neon Events` - String event markers at irregular intervals
//!
//! # Usage
//!
//! ```ignore
//! let manager = NeonLslManager::new(time_sync, resolver, inlet_manager);
//!
//! // Discover Neon devices
//! let devices = manager.discover_neon_devices().await?;
//!
//! // Connect to gaze stream
//! let (mut gaze_rx, _task_id) = manager.connect_gaze_stream("MyNeon").await?;
//!
//! // Receive gaze data
//! while let Some(gaze) = gaze_rx.recv().await {
//!     println!("Gaze: ({}, {})", gaze.gaze_x, gaze.gaze_y);
//! }
//! ```

use super::inlet::{InletConfig, InletManager};
use super::resolver::{StreamFilter, StreamResolver};
use super::types::{DiscoveredNeonDevice, LslError, NeonEventData, NeonGazeData, SampleData};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, RwLock};
use tracing::{debug, info, warn};

/// Default buffer size for gaze data channel (200Hz * 5 seconds = 1000 samples)
const GAZE_CHANNEL_BUFFER_SIZE: usize = 1000;

/// Default buffer size for event data channel
const EVENT_CHANNEL_BUFFER_SIZE: usize = 100;

/// Neon LSL Manager
///
/// Manages discovery and data streaming from Pupil Labs Neon devices via LSL.
/// Uses the existing LSL infrastructure (resolver, inlet manager, time sync)
/// with Neon-specific discovery patterns and data parsing.
pub struct NeonLslManager {
    /// Stream resolver for discovery (shared to maintain stream cache)
    resolver: Arc<StreamResolver>,
    /// Inlet manager for consuming streams
    inlet_manager: Arc<InletManager>,
    /// Discovered Neon devices (device_name -> DiscoveredNeonDevice)
    discovered_devices: Arc<RwLock<HashMap<String, DiscoveredNeonDevice>>>,
    /// Active gaze stream tasks (device_name -> task handle)
    gaze_tasks: Arc<RwLock<HashMap<String, GazeStreamTask>>>,
    /// Active event stream tasks (device_name -> task handle)
    event_tasks: Arc<RwLock<HashMap<String, EventStreamTask>>>,
    /// Source of unique task ids (see `GazeStreamTask::task_id`).
    next_task_id: Arc<AtomicU64>,
}

/// Handle for an active gaze stream task
struct GazeStreamTask {
    /// Sender kept to check channel health via `is_closed()` if needed.
    /// The task uses a clone of this sender.
    #[allow(dead_code)]
    sender: mpsc::Sender<NeonGazeData>,
    /// LSL stream uid this task reads, so the same stream can't be opened
    /// twice under two device labels (labels change when a same-named phone
    /// appears and discovery re-keys entries by host).
    stream_uid: String,
    /// Unique id of this task, handed to the consumer at connect time. A
    /// consumer that wants to release "its" stream passes it back
    /// (`disconnect_gaze_stream_owned`), so a late teardown from an old
    /// consumer can never take down a stream a newer consumer has since
    /// opened under the same label.
    task_id: u64,
    /// Shutdown flag
    shutdown: Arc<AtomicBool>,
    /// Task join handle
    _handle: tokio::task::JoinHandle<()>,
}

/// Handle for an active event stream task
struct EventStreamTask {
    /// Sender kept to check channel health via `is_closed()` if needed.
    /// The task uses a clone of this sender.
    #[allow(dead_code)]
    sender: mpsc::Sender<NeonEventData>,
    /// LSL stream uid this task reads (see GazeStreamTask::stream_uid).
    stream_uid: String,
    /// Unique id of this task (see GazeStreamTask::task_id).
    task_id: u64,
    /// Shutdown flag
    shutdown: Arc<AtomicBool>,
    /// Task join handle
    _handle: tokio::task::JoinHandle<()>,
}

/// Group Neon LSL streams into devices.
///
/// Streams are grouped by *(device name, host)*, not by name alone: two phones
/// both called "Neon Companion" used to collapse into one entry whose hostname
/// was whichever stream arrived first — so a station could subscribe to the
/// other participant's gaze. When several devices share a name, each is keyed
/// and labelled `"<name> @ <host>"` so they stay distinguishable; a unique name
/// keeps its plain label.
pub(crate) fn group_neon_streams(
    streams: Vec<crate::devices::lsl::resolver::DiscoveredStream>,
    now: std::time::SystemTime,
) -> HashMap<String, DiscoveredNeonDevice> {
    let mut by_identity: Vec<((String, String), DiscoveredNeonDevice)> = Vec::new();

    for stream in streams {
        let stream_name = &stream.info.name;
        let Some(device_name) = StreamFilter::extract_neon_device_name(stream_name) else {
            continue;
        };
        let host = stream.info.hostname.clone();
        // Identity is (name, host). If a stream carries no hostname (should
        // not happen with liblsl, which always fills it), fall back to its
        // source_id so two same-named phones still never merge into one entry;
        // a stream with neither can only be grouped by name.
        let disambiguator = if !host.is_empty() {
            host.clone()
        } else if !stream.info.source_id.is_empty() {
            warn!(
                device = "neon",
                "Neon stream '{}' has no hostname; grouping by source_id '{}'",
                stream_name,
                stream.info.source_id
            );
            format!("source:{}", stream.info.source_id)
        } else {
            String::new()
        };
        let key = (device_name.clone(), disambiguator);

        let idx = match by_identity.iter().position(|(k, _)| *k == key) {
            Some(i) => i,
            None => {
                by_identity.push((
                    key,
                    DiscoveredNeonDevice {
                        device_name: device_name.clone(),
                        hostname: if host.is_empty() {
                            None
                        } else {
                            Some(host.clone())
                        },
                        has_gaze_stream: false,
                        has_events_stream: false,
                        gaze_channel_count: 0,
                        gaze_stream_uid: None,
                        events_stream_uid: None,
                        gaze_stream_ambiguous: false,
                        events_stream_ambiguous: false,
                        discovered_at: now,
                    },
                ));
                by_identity.len() - 1
            }
        };
        let entry = &mut by_identity[idx].1;

        // Distinct publishers with the same identity cannot be selected safely.
        // Keep ambiguity sticky through this discovery pass, regardless of order.
        if StreamFilter::is_neon_gaze_stream(stream_name) {
            entry.has_gaze_stream = true;
            if entry
                .gaze_stream_uid
                .as_deref()
                .is_some_and(|uid| uid != stream.uid)
            {
                entry.gaze_stream_ambiguous = true;
            }
            if entry.gaze_stream_ambiguous {
                entry.gaze_stream_uid = None;
                entry.gaze_channel_count = 0;
            } else {
                entry.gaze_channel_count = stream.info.channel_count;
                entry.gaze_stream_uid = Some(stream.uid.clone());
            }
            debug!(
                device = "neon",
                "Found Neon gaze stream: {} ({} channels) on {}",
                stream_name,
                stream.info.channel_count,
                host
            );
        } else if StreamFilter::is_neon_events_stream(stream_name) {
            entry.has_events_stream = true;
            if entry
                .events_stream_uid
                .as_deref()
                .is_some_and(|uid| uid != stream.uid)
            {
                entry.events_stream_ambiguous = true;
            }
            entry.events_stream_uid = if entry.events_stream_ambiguous {
                None
            } else {
                Some(stream.uid.clone())
            };
            debug!(
                device = "neon",
                "Found Neon events stream: {} on {}", stream_name, host
            );
        }
    }

    // Disambiguate labels for devices that share a display name.
    let mut name_counts: HashMap<String, usize> = HashMap::new();
    for ((name, _), _) in &by_identity {
        *name_counts.entry(name.clone()).or_insert(0) += 1;
    }

    let mut device_map: HashMap<String, DiscoveredNeonDevice> = HashMap::new();
    for ((name, disambiguator), mut device) in by_identity {
        let label = if name_counts.get(&name).copied().unwrap_or(0) > 1 && !disambiguator.is_empty()
        {
            warn!(
                device = "neon",
                "Several Neon devices are named '{}'; labelling by host so they stay distinct",
                name
            );
            format!("{} @ {}", name, disambiguator)
        } else {
            name
        };
        device.device_name = label.clone();
        device_map.insert(label, device);
    }
    device_map
}

impl NeonLslManager {
    /// Create a new Neon LSL manager
    ///
    /// # Arguments
    /// * `resolver` - Stream resolver for LSL discovery (shared to maintain cache)
    /// * `inlet_manager` - Inlet manager for consuming LSL streams
    pub fn new(resolver: Arc<StreamResolver>, inlet_manager: Arc<InletManager>) -> Self {
        Self {
            resolver,
            inlet_manager,
            discovered_devices: Arc::new(RwLock::new(HashMap::new())),
            gaze_tasks: Arc::new(RwLock::new(HashMap::new())),
            event_tasks: Arc::new(RwLock::new(HashMap::new())),
            next_task_id: Arc::new(AtomicU64::new(1)),
        }
    }

    /// Discover Neon devices streaming via LSL
    ///
    /// Scans the network for LSL streams matching Neon patterns and groups them
    /// by device name. Returns a list of discovered devices with their capabilities.
    ///
    /// This method uses the shared resolver to maintain stream cache, which is
    /// required for subsequent `connect_gaze_stream()` and `connect_events_stream()` calls.
    pub async fn discover_neon_devices(&self) -> Result<Vec<DiscoveredNeonDevice>, LslError> {
        info!(device = "neon", "Discovering Neon devices via LSL...");

        // Discover all streams using the shared resolver (maintains cache for connect calls)
        let all_streams = self.resolver.discover_streams().await?;

        // Filter to only Neon streams
        let streams: Vec<_> = all_streams
            .into_iter()
            .filter(|s| {
                StreamFilter::is_neon_gaze_stream(&s.info.name)
                    || StreamFilter::is_neon_events_stream(&s.info.name)
            })
            .collect();

        let device_map = group_neon_streams(streams, std::time::SystemTime::now());

        // Update our cache
        {
            let mut devices = self.discovered_devices.write().await;
            *devices = device_map.clone();
        }

        let device_list: Vec<DiscoveredNeonDevice> = device_map.into_values().collect();
        info!(
            device = "neon",
            "Discovered {} Neon device(s)",
            device_list.len()
        );

        for device in &device_list {
            info!(
                device = "neon",
                "  - {}: gaze={} ({}ch), events={}",
                device.device_name,
                device.has_gaze_stream,
                device.gaze_channel_count,
                device.has_events_stream
            );
        }

        Ok(device_list)
    }

    /// Get a previously discovered Neon device by its display name (the key
    /// returned by `discover_neon_devices`).
    pub async fn get_device(&self, device_name: &str) -> Option<DiscoveredNeonDevice> {
        let devices = self.discovered_devices.read().await;
        devices.get(device_name).cloned()
    }

    /// Get all discovered Neon devices
    pub async fn get_all_devices(&self) -> Vec<DiscoveredNeonDevice> {
        let devices = self.discovered_devices.read().await;
        devices.values().cloned().collect()
    }

    /// Connect to a Neon gaze stream
    ///
    /// Returns a channel receiver that will emit gaze data samples, plus the
    /// id of the task that feeds it (pass it to `disconnect_gaze_stream_owned`
    /// to release exactly this stream). The stream continues until
    /// `disconnect_gaze_stream` is called.
    pub async fn connect_gaze_stream(
        &self,
        device_name: &str,
    ) -> Result<(mpsc::Receiver<NeonGazeData>, u64), LslError> {
        info!(
            device = "neon",
            "Connecting to Neon gaze stream: {}", device_name
        );

        // Get the device info. Lock order: `discovered_devices` is never taken
        // while a task-registry lock is held (get_stats takes them the other
        // way round), so resolve everything from discovery BEFORE locking.
        let device = self.get_device(device_name).await.ok_or_else(|| {
            LslError::NeonDeviceNotFound(format!(
                "Device not found: {}. Run discover_neon_devices() first.",
                device_name
            ))
        })?;

        if !device.has_gaze_stream {
            return Err(LslError::NeonStreamNotAvailable(format!(
                "No gaze stream available for device: {}",
                device_name
            )));
        }

        let gaze_uid = device.gaze_stream_uid.ok_or_else(|| {
            LslError::NeonStreamNotAvailable(if device.gaze_stream_ambiguous {
                "Multiple gaze stream UIDs share this device name and host".to_string()
            } else {
                "Gaze stream UID not found".to_string()
            })
        })?;

        // Hold the task registry lock for the WHOLE connect — duplicate checks,
        // inlet creation, task registration. Inlets are keyed by stream uid, and
        // a disconnect closes the inlet by uid after stopping its task; if a
        // connect could interleave, the disconnect would close the inlet the
        // connect just installed for the same stream. Connects and disconnects
        // of gaze streams are therefore strictly serialized. Nothing awaited
        // below takes `discovered_devices`.
        //
        // Accepted trade-off: the lock is manager-wide, so a slow connect or a
        // 2 s teardown join for one phone delays connect/disconnect (not data
        // flow) of another phone on the SAME bridge. In the dyad setup each
        // station runs its own bridge with one phone, so this never bites; a
        // per-uid lock would be the refinement if one bridge ever serves
        // several phones.
        let mut tasks = self.gaze_tasks.write().await;

        // Check if already connected
        if tasks.contains_key(device_name) {
            return Err(LslError::LslLibraryError(format!(
                "Already connected to gaze stream for device: {}",
                device_name
            )));
        }

        // Refuse to open the same LSL stream twice under different labels
        // (discovery re-keys same-named phones by host, changing the label).
        if let Some((existing, _)) = tasks.iter().find(|(_, t)| t.stream_uid == gaze_uid) {
            return Err(LslError::LslLibraryError(format!(
                "Gaze stream for '{}' is already connected as '{}'",
                device_name, existing
            )));
        }
        let gaze_uid_for_task = gaze_uid.clone();
        let task_id = self.next_task_id.fetch_add(1, Ordering::Relaxed);

        // Find the stream in the resolver cache
        let stream = self.resolver.get_stream(&gaze_uid).await.ok_or_else(|| {
            LslError::StreamNotFound(format!("Gaze stream not found: {}", gaze_uid))
        })?;

        // Create inlet for this stream
        let inlet_config = InletConfig {
            buffer_size: GAZE_CHANNEL_BUFFER_SIZE,
            max_buflen: 360,
            max_chunklen: 0,
            recover: true,
            ..Default::default()
        };

        self.inlet_manager
            .create_inlet(stream.clone(), Some(inlet_config))
            .await?;

        // Open the inlet
        self.inlet_manager
            .open_inlet(&gaze_uid, Duration::from_secs(10))
            .await?;

        // Create channel for gaze data
        let (tx, rx) = mpsc::channel(GAZE_CHANNEL_BUFFER_SIZE);
        let shutdown = Arc::new(AtomicBool::new(false));

        // Clone tx for the task (we'll store the original)
        let task_tx = tx.clone();

        // Spawn task to read gaze data
        let task_shutdown = shutdown.clone();
        let task_inlet_manager = self.inlet_manager.clone();
        let task_uid = gaze_uid.clone();
        let task_device_name = device_name.to_string();
        let channel_count = device.gaze_channel_count;

        let handle = tokio::spawn(async move {
            info!(
                device = "neon",
                "Neon gaze stream task started for: {}", task_device_name
            );

            while !task_shutdown.load(Ordering::Relaxed) {
                // Pull samples with a short timeout
                match task_inlet_manager
                    .pull_sample(&task_uid, Duration::from_millis(100))
                    .await
                {
                    Ok(Some(sample)) => {
                        // Parse gaze data based on channel format
                        if let SampleData::Float32(data) = sample.data {
                            match NeonGazeData::from_lsl_sample(sample.timestamp, &data) {
                                Ok(gaze) => {
                                    if task_tx.send(gaze).await.is_err() {
                                        debug!(
                                            device = "neon",
                                            "Gaze receiver dropped, stopping task"
                                        );
                                        break;
                                    }
                                }
                                Err(e) => {
                                    warn!(device = "neon", "Failed to parse Neon gaze data: {}", e);
                                }
                            }
                        } else {
                            warn!(
                                device = "neon",
                                "Unexpected data format for Neon gaze (expected Float32, ch={})",
                                channel_count
                            );
                        }
                    }
                    Ok(None) => {
                        // No data available, continue
                    }
                    Err(e) => {
                        if !task_shutdown.load(Ordering::Relaxed) {
                            warn!(device = "neon", "Error pulling Neon gaze sample: {}", e);
                        }
                    }
                }
            }

            info!(
                device = "neon",
                "Neon gaze stream task stopped for: {}", task_device_name
            );
        });

        // Store the task (still under the registry lock taken above)
        tasks.insert(
            device_name.to_string(),
            GazeStreamTask {
                sender: tx,
                stream_uid: gaze_uid_for_task,
                task_id,
                shutdown,
                _handle: handle,
            },
        );
        drop(tasks);

        info!(
            device = "neon",
            "Connected to Neon gaze stream: {}", device_name
        );
        Ok((rx, task_id))
    }

    /// Connect to a Neon events stream
    ///
    /// Returns a channel receiver that will emit event markers.
    /// The stream continues until `disconnect_events_stream` is called.
    pub async fn connect_events_stream(
        &self,
        device_name: &str,
    ) -> Result<(mpsc::Receiver<NeonEventData>, u64), LslError> {
        info!(
            device = "neon",
            "Connecting to Neon events stream: {}", device_name
        );

        // Get the device info (before locking — see connect_gaze_stream)
        let device = self.get_device(device_name).await.ok_or_else(|| {
            LslError::NeonDeviceNotFound(format!(
                "Device not found: {}. Run discover_neon_devices() first.",
                device_name
            ))
        })?;

        if !device.has_events_stream {
            return Err(LslError::NeonStreamNotAvailable(format!(
                "No events stream available for device: {}",
                device_name
            )));
        }

        let events_uid = device.events_stream_uid.ok_or_else(|| {
            LslError::NeonStreamNotAvailable(if device.events_stream_ambiguous {
                "Multiple events stream UIDs share this device name and host".to_string()
            } else {
                "Events stream UID not found".to_string()
            })
        })?;

        // Serialized against disconnects for the whole connect — see
        // connect_gaze_stream for why.
        let mut tasks = self.event_tasks.write().await;

        // Check if already connected
        if tasks.contains_key(device_name) {
            return Err(LslError::LslLibraryError(format!(
                "Already connected to events stream for device: {}",
                device_name
            )));
        }

        if let Some((existing, _)) = tasks.iter().find(|(_, t)| t.stream_uid == events_uid) {
            return Err(LslError::LslLibraryError(format!(
                "Events stream for '{}' is already connected as '{}'",
                device_name, existing
            )));
        }
        let events_uid_for_task = events_uid.clone();
        let task_id = self.next_task_id.fetch_add(1, Ordering::Relaxed);

        // Find the stream in the resolver cache
        let stream = self.resolver.get_stream(&events_uid).await.ok_or_else(|| {
            LslError::StreamNotFound(format!("Events stream not found: {}", events_uid))
        })?;

        // Create inlet for this stream
        let inlet_config = InletConfig {
            buffer_size: EVENT_CHANNEL_BUFFER_SIZE,
            max_buflen: 360,
            max_chunklen: 0,
            recover: true,
            ..Default::default()
        };

        self.inlet_manager
            .create_inlet(stream.clone(), Some(inlet_config))
            .await?;

        // Open the inlet
        self.inlet_manager
            .open_inlet(&events_uid, Duration::from_secs(10))
            .await?;

        // Create channel for event data
        let (tx, rx) = mpsc::channel(EVENT_CHANNEL_BUFFER_SIZE);
        let shutdown = Arc::new(AtomicBool::new(false));

        // Clone tx for the task (we'll store the original)
        let task_tx = tx.clone();

        // Spawn task to read event data
        let task_shutdown = shutdown.clone();
        let task_inlet_manager = self.inlet_manager.clone();
        let task_uid = events_uid.clone();
        let task_device_name = device_name.to_string();

        let handle = tokio::spawn(async move {
            info!(
                device = "neon",
                "Neon events stream task started for: {}", task_device_name
            );

            while !task_shutdown.load(Ordering::Relaxed) {
                // Pull samples with a longer timeout (events are irregular)
                match task_inlet_manager
                    .pull_sample(&task_uid, Duration::from_millis(500))
                    .await
                {
                    Ok(Some(sample)) => {
                        // Parse event data
                        if let SampleData::String(data) = sample.data {
                            match NeonEventData::from_lsl_sample(sample.timestamp, &data) {
                                Ok(event) => {
                                    debug!(
                                        device = "neon",
                                        "Neon event: {} @ {}", event.event_name, event.timestamp
                                    );
                                    if task_tx.send(event).await.is_err() {
                                        debug!(
                                            device = "neon",
                                            "Events receiver dropped, stopping task"
                                        );
                                        break;
                                    }
                                }
                                Err(e) => {
                                    warn!(
                                        device = "neon",
                                        "Failed to parse Neon event data: {}", e
                                    );
                                }
                            }
                        } else {
                            warn!(
                                device = "neon",
                                "Unexpected data format for Neon events (expected String)"
                            );
                        }
                    }
                    Ok(None) => {
                        // No data available, continue
                    }
                    Err(e) => {
                        if !task_shutdown.load(Ordering::Relaxed) {
                            warn!(device = "neon", "Error pulling Neon event sample: {}", e);
                        }
                    }
                }
            }

            info!(
                device = "neon",
                "Neon events stream task stopped for: {}", task_device_name
            );
        });

        // Store the task (still under the registry lock taken above)
        tasks.insert(
            device_name.to_string(),
            EventStreamTask {
                sender: tx,
                stream_uid: events_uid_for_task,
                task_id,
                shutdown,
                _handle: handle,
            },
        );
        drop(tasks);

        info!(
            device = "neon",
            "Connected to Neon events stream: {}", device_name
        );
        Ok((rx, task_id))
    }

    /// Disconnect from a Neon gaze stream (whoever opened it).
    pub async fn disconnect_gaze_stream(&self, device_name: &str) -> Result<(), LslError> {
        self.disconnect_gaze_stream_inner(device_name, None).await
    }

    /// Release the gaze stream under `device_name` only if it is still the
    /// task `task_id` (as returned by `connect_gaze_stream`). A no-op if a
    /// newer consumer has since opened the label — that stream is theirs.
    pub async fn disconnect_gaze_stream_owned(
        &self,
        device_name: &str,
        task_id: u64,
    ) -> Result<(), LslError> {
        self.disconnect_gaze_stream_inner(device_name, Some(task_id))
            .await
    }

    async fn disconnect_gaze_stream_inner(
        &self,
        device_name: &str,
        owner: Option<u64>,
    ) -> Result<(), LslError> {
        // Resolve the discovery fallback before locking (lock order: never
        // take `discovered_devices` while holding a task-registry lock).
        let fallback_uid = self
            .get_device(device_name)
            .await
            .and_then(|d| d.gaze_stream_uid);
        // The registry lock is held across task removal, shutdown AND inlet
        // close so no connect can install a fresh inlet for the same stream
        // uid in between (connect holds the same lock for its whole duration).
        let mut tasks = self.gaze_tasks.write().await;
        if let Some(owner) = owner {
            match tasks.get(device_name) {
                Some(task) if task.task_id == owner => {}
                _ => {
                    info!(
                        device = "neon",
                        "Gaze stream '{}' is not owned by task {}; leaving it", device_name, owner
                    );
                    return Ok(());
                }
            }
        }
        info!(
            device = "neon",
            "Disconnecting Neon gaze stream: {}", device_name
        );

        // Close the inlet the task was actually reading — the discovery label
        // may have been re-keyed since it started, so never resolve the uid
        // through the (possibly stale) display name when a task exists.
        let uid = match tasks.remove(device_name) {
            Some(task) => {
                let uid = task.stream_uid.clone();
                task.shutdown.store(true, Ordering::Relaxed);
                let _ = tokio::time::timeout(Duration::from_secs(2), task._handle).await;
                Some(uid)
            }
            // No task under this label: only close an inlet that no other task
            // (under a re-keyed label) is still reading.
            None => fallback_uid.filter(|uid| !tasks.values().any(|t| &t.stream_uid == uid)),
        };
        if let Some(uid) = uid {
            let _ = self.inlet_manager.close_inlet(&uid).await;
            let _ = self.inlet_manager.remove_inlet(&uid).await;
        }
        drop(tasks);

        info!(
            device = "neon",
            "Disconnected Neon gaze stream: {}", device_name
        );
        Ok(())
    }

    /// Disconnect from a Neon events stream (whoever opened it).
    pub async fn disconnect_events_stream(&self, device_name: &str) -> Result<(), LslError> {
        self.disconnect_events_stream_inner(device_name, None).await
    }

    /// Release the events stream under `device_name` only if it is still the
    /// task `task_id` (see `disconnect_gaze_stream_owned`).
    pub async fn disconnect_events_stream_owned(
        &self,
        device_name: &str,
        task_id: u64,
    ) -> Result<(), LslError> {
        self.disconnect_events_stream_inner(device_name, Some(task_id))
            .await
    }

    async fn disconnect_events_stream_inner(
        &self,
        device_name: &str,
        owner: Option<u64>,
    ) -> Result<(), LslError> {
        let fallback_uid = self
            .get_device(device_name)
            .await
            .and_then(|d| d.events_stream_uid);
        let mut tasks = self.event_tasks.write().await;
        if let Some(owner) = owner {
            match tasks.get(device_name) {
                Some(task) if task.task_id == owner => {}
                _ => {
                    info!(
                        device = "neon",
                        "Events stream '{}' is not owned by task {}; leaving it",
                        device_name,
                        owner
                    );
                    return Ok(());
                }
            }
        }
        info!(
            device = "neon",
            "Disconnecting Neon events stream: {}", device_name
        );

        let uid = match tasks.remove(device_name) {
            Some(task) => {
                let uid = task.stream_uid.clone();
                task.shutdown.store(true, Ordering::Relaxed);
                let _ = tokio::time::timeout(Duration::from_secs(2), task._handle).await;
                Some(uid)
            }
            None => fallback_uid.filter(|uid| !tasks.values().any(|t| &t.stream_uid == uid)),
        };
        if let Some(uid) = uid {
            let _ = self.inlet_manager.close_inlet(&uid).await;
            let _ = self.inlet_manager.remove_inlet(&uid).await;
        }
        drop(tasks);

        info!(
            device = "neon",
            "Disconnected Neon events stream: {}", device_name
        );
        Ok(())
    }

    /// Disconnect all streams for a device
    pub async fn disconnect(&self, device_name: &str) -> Result<(), LslError> {
        let _ = self.disconnect_gaze_stream(device_name).await;
        let _ = self.disconnect_events_stream(device_name).await;
        Ok(())
    }

    /// Disconnect all Neon streams
    pub async fn disconnect_all(&self) -> Result<(), LslError> {
        // Walk the ACTIVE task labels, not just the current discovery cache:
        // a re-discovery can re-key devices (e.g. "Neon Companion" becomes
        // "Neon Companion @ host"), which would otherwise orphan tasks that
        // were started under the old label.
        let mut device_names: std::collections::BTreeSet<String> = {
            let devices = self.discovered_devices.read().await;
            devices.keys().cloned().collect()
        };
        device_names.extend(self.gaze_tasks.read().await.keys().cloned());
        device_names.extend(self.event_tasks.read().await.keys().cloned());

        for device_name in device_names {
            let _ = self.disconnect(&device_name).await;
        }

        Ok(())
    }

    /// Check if gaze stream is connected for a device
    pub async fn is_gaze_connected(&self, device_name: &str) -> bool {
        let tasks = self.gaze_tasks.read().await;
        tasks.contains_key(device_name)
    }

    /// Check if events stream is connected for a device
    pub async fn is_events_connected(&self, device_name: &str) -> bool {
        let tasks = self.event_tasks.read().await;
        tasks.contains_key(device_name)
    }

    /// Get statistics for Neon streams
    pub async fn get_stats(&self) -> serde_json::Value {
        let devices = self.discovered_devices.read().await;
        let gaze_tasks = self.gaze_tasks.read().await;
        let event_tasks = self.event_tasks.read().await;

        let device_stats: Vec<serde_json::Value> = devices
            .values()
            .map(|d| {
                serde_json::json!({
                    "device_name": d.device_name,
                    "has_gaze_stream": d.has_gaze_stream,
                    "has_events_stream": d.has_events_stream,
                    "gaze_channel_count": d.gaze_channel_count,
                    "gaze_connected": gaze_tasks.contains_key(&d.device_name),
                    "events_connected": event_tasks.contains_key(&d.device_name),
                })
            })
            .collect();

        serde_json::json!({
            "discovered_device_count": devices.len(),
            "active_gaze_streams": gaze_tasks.len(),
            "active_event_streams": event_tasks.len(),
            "devices": device_stats
        })
    }
}

impl std::fmt::Debug for NeonLslManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NeonLslManager").finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_neon_gaze_data_basic() {
        let gaze = NeonGazeData::from_basic(1.0, 0.5, 0.5);
        assert_eq!(gaze.gaze_x, 0.5);
        assert_eq!(gaze.gaze_y, 0.5);
        assert!(gaze.pupil_diameter.is_none());
        assert!(gaze.eyeball_center.is_none());
    }

    #[test]
    fn test_neon_gaze_data_full() {
        let gaze = NeonGazeData::from_full(1.0, 0.5, 0.5, 3.5, 0.0, 0.0, 0.5);
        assert_eq!(gaze.gaze_x, 0.5);
        assert_eq!(gaze.gaze_y, 0.5);
        assert_eq!(gaze.pupil_diameter, Some(3.5));
        assert_eq!(gaze.eyeball_center, Some((0.0, 0.0, 0.5)));
    }

    #[test]
    fn test_neon_gaze_from_lsl_sample_basic() {
        let data = vec![0.3, 0.7];
        let gaze = NeonGazeData::from_lsl_sample(1.5, &data).unwrap();
        assert_eq!(gaze.gaze_x, 0.3);
        assert_eq!(gaze.gaze_y, 0.7);
        assert!(gaze.pupil_diameter.is_none());
    }

    #[test]
    fn test_neon_gaze_from_lsl_sample_full() {
        let data = vec![0.3, 0.7, 4.0, 0.1, 0.2, 0.3];
        let gaze = NeonGazeData::from_lsl_sample(1.5, &data).unwrap();
        assert_eq!(gaze.gaze_x, 0.3);
        assert_eq!(gaze.gaze_y, 0.7);
        assert_eq!(gaze.pupil_diameter, Some(4.0));
        assert_eq!(gaze.eyeball_center, Some((0.1, 0.2, 0.3)));
    }

    #[test]
    fn test_neon_gaze_from_lsl_sample_invalid() {
        let data = vec![0.3, 0.7, 4.0]; // 3 channels - invalid
        let result = NeonGazeData::from_lsl_sample(1.5, &data);
        assert!(result.is_err());
    }

    #[test]
    fn test_neon_event_data() {
        let event = NeonEventData::new(2.5, "stimulus_start".to_string());
        assert_eq!(event.timestamp, 2.5);
        assert_eq!(event.event_name, "stimulus_start");
    }

    #[test]
    fn test_neon_event_from_lsl_sample() {
        let data = vec!["response_button".to_string()];
        let event = NeonEventData::from_lsl_sample(3.0, &data).unwrap();
        assert_eq!(event.event_name, "response_button");
    }

    #[test]
    fn test_neon_event_from_lsl_sample_empty() {
        let data: Vec<String> = vec![];
        let result = NeonEventData::from_lsl_sample(3.0, &data);
        assert!(result.is_err());
    }

    #[test]
    fn test_stream_filter_neon_gaze() {
        let filter = StreamFilter::neon_gaze();
        assert_eq!(filter.name_pattern, Some("_Neon Gaze".to_string()));
    }

    #[test]
    fn test_stream_filter_is_neon_stream() {
        assert!(StreamFilter::is_neon_gaze_stream("MyNeon_Neon Gaze"));
        assert!(!StreamFilter::is_neon_gaze_stream("MyNeon_Neon Events"));
        assert!(StreamFilter::is_neon_events_stream("MyNeon_Neon Events"));
        assert!(!StreamFilter::is_neon_events_stream("MyNeon_Neon Gaze"));
    }

    #[test]
    fn test_extract_neon_device_name() {
        assert_eq!(
            StreamFilter::extract_neon_device_name("MyNeon_Neon Gaze"),
            Some("MyNeon".to_string())
        );
        assert_eq!(
            StreamFilter::extract_neon_device_name("Lab Neon 1_Neon Events"),
            Some("Lab Neon 1".to_string())
        );
        assert_eq!(
            StreamFilter::extract_neon_device_name("SomeOtherStream"),
            None
        );
    }
}

#[cfg(test)]
mod grouping_tests_no_host {
    use super::*;
    use crate::devices::lsl::resolver::DiscoveredStream;
    use crate::devices::lsl::types::{ChannelFormat, StreamInfo, StreamType};

    fn stream(name: &str, source_id: &str, uid: &str) -> DiscoveredStream {
        DiscoveredStream {
            info: StreamInfo {
                name: name.to_string(),
                stream_type: StreamType::Gaze,
                channel_count: 3,
                nominal_srate: 200.0,
                channel_format: ChannelFormat::Float32,
                source_id: source_id.to_string(),
                hostname: String::new(),
                metadata: HashMap::new(),
            },
            discovered_at: std::time::SystemTime::now(),
            last_seen: std::time::SystemTime::now(),
            available: true,
            uid: uid.to_string(),
            session_id: "default".to_string(),
            data_loss: 0.0,
            time_stamps: (),
        }
    }

    #[test]
    fn same_named_phones_without_hostnames_do_not_merge() {
        let devices = group_neon_streams(
            vec![
                stream("Neon Companion_Neon Gaze", "phone-a", "g1"),
                stream("Neon Companion_Neon Gaze", "phone-b", "g2"),
            ],
            std::time::SystemTime::now(),
        );
        assert_eq!(
            devices.len(),
            2,
            "two phones must stay two entries: {:?}",
            devices.keys()
        );
        let uids: std::collections::BTreeSet<_> = devices
            .values()
            .filter_map(|d| d.gaze_stream_uid.clone())
            .collect();
        assert_eq!(uids.len(), 2);
    }
}

#[cfg(test)]
mod grouping_tests {
    use super::*;
    use crate::devices::lsl::resolver::DiscoveredStream;
    use crate::devices::lsl::types::{ChannelFormat, StreamInfo, StreamType};
    use std::time::SystemTime;

    fn stream(name: &str, host: &str, uid: &str) -> DiscoveredStream {
        let mut info = StreamInfo::new(
            name.to_string(),
            StreamType::Gaze,
            2,
            200.0,
            ChannelFormat::Float32,
            format!("src-{uid}"),
        );
        info.hostname = host.to_string();
        DiscoveredStream {
            info,
            discovered_at: SystemTime::now(),
            last_seen: SystemTime::now(),
            available: true,
            uid: uid.to_string(),
            session_id: "default".to_string(),
            data_loss: 0.0,
            time_stamps: (),
        }
    }

    #[test]
    fn distinct_gaze_publishers_on_same_host_have_no_selected_uid() {
        for uids in [["g1", "g2", "g1"], ["g2", "g1", "g3"]] {
            let mut streams: Vec<_> = uids
                .iter()
                .map(|uid| stream("Neon P1_Neon Gaze", "phone-a", uid))
                .collect();
            streams.push(stream("Neon P1_Neon Events", "phone-a", "e1"));
            let map = group_neon_streams(streams, SystemTime::now());
            let device = &map["Neon P1"];
            assert!(device.has_gaze_stream);
            assert!(device.gaze_stream_ambiguous);
            assert!(!device.events_stream_ambiguous);
            assert_eq!(device.gaze_stream_uid, None);
            assert_eq!(device.gaze_channel_count, 0);
            assert_eq!(device.events_stream_uid.as_deref(), Some("e1"));
            assert_eq!(device.hostname.as_deref(), Some("phone-a"));
        }
    }

    #[test]
    fn distinct_event_publishers_on_same_host_have_no_selected_uid() {
        let map = group_neon_streams(
            vec![
                stream("Neon P1_Neon Gaze", "phone-a", "g1"),
                stream("Neon P1_Neon Events", "phone-a", "e1"),
                stream("Neon P1_Neon Events", "phone-a", "e2"),
                stream("Neon P1_Neon Events", "phone-a", "e3"),
            ],
            SystemTime::now(),
        );
        let device = &map["Neon P1"];
        assert!(device.has_events_stream);
        assert!(device.events_stream_ambiguous);
        assert!(!device.gaze_stream_ambiguous);
        assert_eq!(device.events_stream_uid, None);
        assert_eq!(device.gaze_stream_uid.as_deref(), Some("g1"));
    }

    #[test]
    fn repeated_same_uid_does_not_create_ambiguity() {
        let map = group_neon_streams(
            vec![
                stream("Neon P1_Neon Gaze", "phone-a", "g1"),
                stream("Neon P1_Neon Gaze", "phone-a", "g1"),
            ],
            SystemTime::now(),
        );
        assert_eq!(map["Neon P1"].gaze_stream_uid.as_deref(), Some("g1"));
        assert_eq!(map["Neon P1"].gaze_channel_count, 2);
        assert!(!map["Neon P1"].gaze_stream_ambiguous);
    }

    #[test]
    fn unique_name_keeps_plain_label_and_merges_gaze_and_events() {
        let map = group_neon_streams(
            vec![
                stream("Neon P1_Neon Gaze", "phone-a", "g1"),
                stream("Neon P1_Neon Events", "phone-a", "e1"),
            ],
            SystemTime::now(),
        );
        assert_eq!(map.len(), 1);
        let d = &map["Neon P1"];
        assert!(d.has_gaze_stream && d.has_events_stream);
        assert_eq!(d.gaze_stream_uid.as_deref(), Some("g1"));
        assert_eq!(d.hostname.as_deref(), Some("phone-a"));
    }

    #[test]
    fn same_name_on_two_hosts_stays_two_devices_labelled_by_host() {
        let map = group_neon_streams(
            vec![
                stream("Neon Companion_Neon Gaze", "phone-a", "g1"),
                stream("Neon Companion_Neon Gaze", "phone-b", "g2"),
                stream("Neon Companion_Neon Events", "phone-b", "e2"),
            ],
            SystemTime::now(),
        );
        assert_eq!(map.len(), 2, "two phones must not collapse into one entry");
        let a = &map["Neon Companion @ phone-a"];
        let b = &map["Neon Companion @ phone-b"];
        assert_eq!(a.gaze_stream_uid.as_deref(), Some("g1"));
        assert_eq!(b.gaze_stream_uid.as_deref(), Some("g2"));
        assert!(b.has_events_stream && !a.has_events_stream);
    }

    #[test]
    fn non_neon_streams_are_ignored() {
        let map = group_neon_streams(vec![stream("FRENZ_EEG", "band", "x")], SystemTime::now());
        assert!(map.is_empty());
    }
}

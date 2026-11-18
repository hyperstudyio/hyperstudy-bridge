use super::{Device, DeviceConfig, DeviceError, DeviceInfo, DeviceStatus, DeviceType};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};
use tokio::time::{sleep, timeout};
use tracing::{debug, error, info, warn};

/// Default API port for Neon Companion App REST API.
const DEFAULT_API_PORT: u16 = 8080;

/// Default timeout for HTTP requests (milliseconds).
const DEFAULT_HTTP_TIMEOUT_MS: u64 = 5000;

/// How long to wait after `recording:start` for GET /api/status to report the
/// new recording as active before giving up and letting events flow anyway.
const RECORDING_ACTIVE_TIMEOUT_MS: u64 = 3000;

/// Poll interval while waiting for the recording to become active.
const RECORDING_ACTIVE_POLL_MS: u64 = 100;

// ---------------------------------------------------------------------------
// Neon REST API response types (matching OpenAPI spec v2.1.0)
// ---------------------------------------------------------------------------

/// Generic API response envelope. All Neon REST endpoints wrap responses in
/// `{"message": "...", "result": ...}`.
#[derive(Debug, Clone, Deserialize)]
struct ApiResponse<T> {
    #[allow(dead_code)]
    message: String,
    result: T,
}

/// A single component from the GET /api/status heterogeneous array.
/// The status endpoint returns `{"result": [{"model": "Phone", "data": {...}}, ...]}`.
#[derive(Debug, Clone, Deserialize)]
struct StatusItem {
    model: String,
    data: serde_json::Value,
}

/// Parsed status from Neon Companion App, assembled from the heterogeneous
/// status array returned by GET /api/status.
#[derive(Debug, Clone, Serialize)]
pub struct NeonStatus {
    pub phone: PhoneInfo,
    pub hardware: Option<HardwareInfo>,
    pub sensors: Vec<SensorInfo>,
    pub recording: Option<RecordingInfo>,
}

impl NeonStatus {
    /// Parse a NeonStatus from the heterogeneous array returned by GET /api/status.
    fn from_status_items(items: Vec<StatusItem>) -> Result<Self, DeviceError> {
        let mut phone = None;
        let mut hardware = None;
        let mut sensors = Vec::new();
        let mut recording = None;

        for item in items {
            match item.model.as_str() {
                "Phone" => {
                    phone = Some(serde_json::from_value(item.data).map_err(|e| {
                        DeviceError::InvalidData(format!("Failed to parse Phone data: {}", e))
                    })?);
                }
                "Hardware" => {
                    hardware = Some(serde_json::from_value(item.data).map_err(|e| {
                        DeviceError::InvalidData(format!("Failed to parse Hardware data: {}", e))
                    })?);
                }
                "Sensor" => {
                    sensors.push(serde_json::from_value(item.data).map_err(|e| {
                        DeviceError::InvalidData(format!("Failed to parse Sensor data: {}", e))
                    })?);
                }
                "Recording" => {
                    recording = Some(serde_json::from_value(item.data).map_err(|e| {
                        DeviceError::InvalidData(format!("Failed to parse Recording data: {}", e))
                    })?);
                }
                other => {
                    debug!(model = %other, "Unknown status component, skipping");
                }
            }
        }

        let phone = phone.ok_or_else(|| {
            DeviceError::InvalidData("Missing Phone component in status response".to_string())
        })?;

        Ok(NeonStatus {
            phone,
            hardware,
            sensors,
            recording,
        })
    }
}

/// Phone/device information from Neon status response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhoneInfo {
    pub ip: String,
    #[serde(default)]
    pub port: u16,
    pub device_id: String,
    pub device_name: String,
    pub battery_level: f32,
    pub battery_state: String,
    pub memory: u64,
    pub memory_state: String,
    #[serde(default)]
    pub time_echo_port: u16,
}

/// Hardware information (glasses/camera serials).
///
/// The Companion app sends these as explicit `null` when no glasses/module is
/// plugged into the phone, so every field must be `Option` — `#[serde(default)]`
/// alone only covers a *missing* key, not a `null` value.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HardwareInfo {
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub glasses_serial: Option<String>,
    #[serde(default)]
    pub world_camera_serial: Option<String>,
    #[serde(default)]
    pub module_serial: Option<String>,
}

/// Sensor connection information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SensorInfo {
    pub sensor: String,
    pub conn_type: String,
    #[serde(default)]
    pub protocol: String,
    #[serde(default)]
    pub ip: String,
    #[serde(default)]
    pub port: u16,
    #[serde(default)]
    pub params: String,
    #[serde(default)]
    pub connected: bool,
}

/// Recording state information (from status array component).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordingInfo {
    pub id: String,
    #[serde(default)]
    pub action: String,
    #[serde(default)]
    pub rec_duration_ns: u64,
    #[serde(default)]
    pub message: String,
}

/// POST /api/event request body.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventRequest {
    pub name: String,
    /// Optional timestamp in nanoseconds since Unix epoch.
    /// If omitted, the Companion App uses its own clock.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<i64>,
}

/// POST /api/event response body (inside envelope `result`).
/// Note: The Neon API does not currently return the event name in the response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventResponse {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub recording_id: Option<String>,
    pub timestamp: i64,
}

/// POST /api/recording:start response body (inside envelope `result`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordingStartResponse {
    pub id: String,
}

/// POST /api/recording:stop_and_save response body (inside envelope `result`).
#[derive(Debug, Clone, Deserialize)]
struct RecordingStopResponse {
    #[allow(dead_code)]
    id: String,
    #[serde(default)]
    #[allow(dead_code)]
    rec_duration_ns: u64,
}

/// POST /api/recording:cancel response body (inside envelope `result`).
#[derive(Debug, Clone, Deserialize)]
struct RecordingCancelResponse {
    #[allow(dead_code)]
    id: String,
}

// ---------------------------------------------------------------------------
// Command routing: JSON payloads sent via Device::send()
// ---------------------------------------------------------------------------

/// Internal command structure parsed from send() byte payload.
#[derive(Debug, Clone, Deserialize)]
struct PupilCommand {
    command: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    timestamp: Option<i64>,
    /// `recording_stop` / `recording_cancel` only: override the ownership guard
    /// and stop a recording this bridge did not start.
    #[serde(default)]
    force: bool,
}

// ---------------------------------------------------------------------------
// PupilDevice — Neon Companion REST API client
// ---------------------------------------------------------------------------

/// Pupil Labs Neon device controller using the Companion App REST API.
///
/// This module handles recording control, event sending, and status monitoring
/// via HTTP requests to the Neon Companion App (port 8080). Gaze data streaming
/// is handled separately via LSL through the `neon.rs` module.
#[derive(Debug)]
pub struct PupilDevice {
    http_client: reqwest::Client,
    base_url: String,
    device_ip: String,
    status: DeviceStatus,
    config: DeviceConfig,
    recording_id: Option<String>,
    /// True when this bridge issued `recording:start` for `recording_id`; false
    /// when the id was adopted from a recording already running on the phone.
    /// Only owned recordings may be stopped without `force`.
    recording_owned: bool,
    /// Hardware id this station is pinned to. When set, `connect()` refuses a
    /// phone whose `device_id` differs (`WrongDevice`) — the guard against
    /// `neon.local` resolving to another participant's phone.
    expected_device_id: Option<String>,
    neon_status: Option<NeonStatus>,
    connection_retry_count: u32,
    max_retries: u32,
}

impl PupilDevice {
    /// Create a new PupilDevice for the given host (e.g., "neon.local:8080" or "192.168.1.100").
    ///
    /// If no port is specified, defaults to 8080.
    pub fn new(host: String) -> Self {
        let (device_ip, base_url) = Self::parse_host(&host);

        let http_client = reqwest::Client::builder()
            .timeout(Duration::from_millis(DEFAULT_HTTP_TIMEOUT_MS))
            .build()
            .unwrap_or_default();

        Self {
            http_client,
            base_url,
            device_ip,
            status: DeviceStatus::Disconnected,
            config: DeviceConfig::default(),
            recording_id: None,
            recording_owned: false,
            expected_device_id: None,
            neon_status: None,
            connection_retry_count: 0,
            max_retries: 3,
        }
    }

    /// Restore ownership of `recording_id` after a reconnect or bridge
    /// restart, when the bridge remembers having started it on this phone.
    /// No-op unless that recording is the one currently active.
    pub fn claim_recording_ownership(&mut self, recording_id: &str) {
        if self.recording_id.as_deref() == Some(recording_id) {
            self.recording_owned = true;
        }
    }

    /// Pin this device to a phone hardware id (see `expected_device_id`).
    pub fn with_expected_device_id(mut self, device_id: Option<String>) -> Self {
        self.expected_device_id = device_id.filter(|s| !s.is_empty());
        self
    }

    /// Parse a host string into (ip, base_url).
    /// Accepts formats: "neon.local:8080", "192.168.1.100", "neon.local"
    fn parse_host(host: &str) -> (String, String) {
        // Strip any protocol prefix if accidentally included
        let cleaned = host
            .trim_start_matches("http://")
            .trim_start_matches("https://")
            .trim_start_matches("ws://")
            .trim_start_matches("wss://");

        let (ip_part, port) = if let Some((ip, port_str)) = cleaned.rsplit_once(':') {
            let port = port_str.parse::<u16>().unwrap_or(DEFAULT_API_PORT);
            (ip.to_string(), port)
        } else {
            (cleaned.to_string(), DEFAULT_API_PORT)
        };

        let base_url = format!("http://{}:{}/api", ip_part, port);
        (ip_part, base_url)
    }

    /// Resolve `.local` (mDNS) hostnames to IP addresses via the system resolver.
    ///
    /// HTTP clients like reqwest/hyper may not resolve mDNS names correctly on
    /// macOS, even though the system resolver (used by `ping`) handles them fine.
    /// This pre-resolves `.local` hostnames using `tokio::net::lookup_host` which
    /// delegates to `getaddrinfo` and the OS mDNS stack (mDNSResponder on macOS).
    async fn resolve_mdns_if_needed(&mut self) {
        if !self.device_ip.ends_with(".local") {
            return;
        }

        // Extract port from base_url
        let port = self
            .base_url
            .split("://")
            .nth(1)
            .and_then(|s| s.split(':').nth(1))
            .and_then(|s| s.split('/').next())
            .and_then(|s| s.parse::<u16>().ok())
            .unwrap_or(DEFAULT_API_PORT);

        let lookup_addr = format!("{}:{}", self.device_ip, port);
        let resolved = tokio::net::lookup_host(lookup_addr.as_str())
            .await
            .ok()
            .and_then(|mut addrs| addrs.next())
            .map(|addr| addr.ip().to_string());

        match resolved {
            Some(resolved_ip) => {
                info!(
                    device = "pupil",
                    hostname = %self.device_ip,
                    resolved_ip = %resolved_ip,
                    "Resolved mDNS hostname"
                );
                self.base_url = format!("http://{}:{}/api", resolved_ip, port);
            }
            None => {
                warn!(
                    device = "pupil",
                    hostname = %self.device_ip,
                    "Failed to resolve mDNS hostname, will try direct connection"
                );
            }
        }
    }

    // -----------------------------------------------------------------------
    // Public helper methods (convenience wrappers for common REST operations)
    // -----------------------------------------------------------------------

    /// Fetch current device status from `GET /api/status`.
    ///
    /// The Neon API returns a heterogeneous array of `{"model": "...", "data": {...}}`
    /// objects wrapped in a `{"message": "...", "result": [...]}` envelope.
    pub async fn get_neon_status(&mut self) -> Result<NeonStatus, DeviceError> {
        let url = format!("{}/status", self.base_url);
        let resp = self.http_client.get(&url).send().await.map_err(|e| {
            DeviceError::CommunicationError(format!("GET /api/status failed: {}", e))
        })?;

        if !resp.status().is_success() {
            return Err(DeviceError::CommunicationError(format!(
                "GET /api/status returned {}",
                resp.status()
            )));
        }

        let envelope: ApiResponse<Vec<StatusItem>> = resp.json().await.map_err(|e| {
            DeviceError::InvalidData(format!("Failed to parse status envelope: {}", e))
        })?;

        let neon_status = NeonStatus::from_status_items(envelope.result)?;
        self.neon_status = Some(neon_status.clone());
        Ok(neon_status)
    }

    /// True if a status `Recording` item describes a recording that is still
    /// running (the phone reports the last recording briefly with a terminal
    /// action after it ends).
    fn recording_is_active(rec: &RecordingInfo) -> bool {
        !matches!(
            rec.action.to_ascii_uppercase().as_str(),
            "STOP" | "SAVE" | "DISCARD" | "CANCEL" | "ERROR"
        )
    }

    /// Start a recording. Returns the recording UUID.
    ///
    /// Refuses (`PhoneBusy`) if the phone is already recording something this
    /// bridge did not start: on a shared lab network that means another station
    /// resolved the same phone, and piling both participants' markers into one
    /// recording is worse than failing loudly.
    pub async fn start_recording(&mut self) -> Result<String, DeviceError> {
        // Fail closed like stop/cancel: if we cannot see whether the phone is
        // already recording for someone else, we must not start on it.
        let status = self.get_neon_status().await.map_err(|e| {
            DeviceError::CommunicationError(format!(
                "could not check whether the phone is already recording ({}); refusing to start — retry when the phone is reachable",
                e
            ))
        })?;
        {
            if let Some(rec) = status.recording.filter(Self::recording_is_active) {
                let ours =
                    self.recording_owned && self.recording_id.as_deref() == Some(rec.id.as_str());
                if ours {
                    // Retry after a lost response — idempotent.
                    info!(device = "pupil", recording_id = %rec.id, "Recording already running (ours)");
                    return Ok(rec.id);
                }
                return Err(DeviceError::PhoneBusy(format!(
                    "phone is already recording {} which this bridge did not start — another station may be connected to this phone",
                    rec.id
                )));
            }
        }

        let url = format!("{}/recording:start", self.base_url);
        let resp = self.http_client.post(&url).send().await.map_err(|e| {
            DeviceError::CommunicationError(format!("POST recording:start failed: {}", e))
        })?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(DeviceError::CommunicationError(format!(
                "recording:start returned {} — {}",
                status, body
            )));
        }

        let envelope: ApiResponse<RecordingStartResponse> = resp.json().await.map_err(|e| {
            DeviceError::InvalidData(format!("Failed to parse recording response: {}", e))
        })?;

        let id = envelope.result.id;
        self.recording_id = Some(id.clone());
        self.recording_owned = true;
        info!(device = "pupil", recording_id = %id, "Recording started");

        self.wait_for_recording_active(&id).await;
        Ok(id)
    }

    /// Poll GET /api/status until the Companion app reports `id` as the active
    /// recording.
    ///
    /// `recording:start` returns before the phone has actually opened the
    /// recording. Events POSTed in that window (0.4–0.9 s in the field) come back
    /// attributed to the *previous* recording or to none at all — which is exactly
    /// where HyperStudy's `experiment_start` and `trigger` sync markers land.
    /// Best-effort: on timeout we warn and return so a slow phone can't stall the
    /// experiment start indefinitely.
    async fn wait_for_recording_active(&mut self, id: &str) {
        let started = Instant::now();
        let deadline = started + Duration::from_millis(RECORDING_ACTIVE_TIMEOUT_MS);

        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            // Bound each status request by what is left of the budget, so a
            // stalled request cannot hold the device lock past the cap.
            match timeout(remaining, self.get_neon_status()).await {
                Ok(Ok(status)) => {
                    let active = status
                        .recording
                        .as_ref()
                        .filter(|r| Self::recording_is_active(r))
                        .map(|r| r.id.as_str());
                    if active == Some(id) {
                        info!(
                            device = "pupil",
                            recording_id = %id,
                            elapsed_ms = started.elapsed().as_millis() as u64,
                            "Recording confirmed active"
                        );
                        return;
                    }
                    debug!(
                        device = "pupil",
                        reported = ?status.recording.as_ref().map(|r| (&r.id, &r.action)),
                        "Recording not yet reported active"
                    );
                }
                Ok(Err(e)) => {
                    debug!(device = "pupil", error = %e, "Status poll failed while waiting for recording");
                }
                Err(_) => break,
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            sleep(Duration::from_millis(RECORDING_ACTIVE_POLL_MS).min(remaining)).await;
        }

        warn!(
            device = "pupil",
            recording_id = %id,
            "Recording not reported active after {}ms; events sent now may not be attached to it",
            RECORDING_ACTIVE_TIMEOUT_MS
        );
    }

    /// Ownership guard shared by stop/cancel. Returns `Ok(false)` when there is
    /// nothing to stop, `Ok(true)` when the active recording is ours (or `force`),
    /// and `RecordingNotOwned` when it belongs to someone else.
    async fn check_recording_ownership(
        &mut self,
        verb: &str,
        force: bool,
    ) -> Result<bool, DeviceError> {
        if force {
            warn!(
                device = "pupil",
                "{} recording with force=true — skipping ownership check", verb
            );
            return Ok(true);
        }
        let status = match self.get_neon_status().await {
            Ok(s) => s,
            Err(e) => {
                // Fail closed: this guard exists precisely for flaky-network
                // moments, and "we couldn't check" must not become "go ahead and
                // stop whatever is running" — that is how a station ended the
                // other participant's recording. The caller can retry, or pass
                // force=true if they know the recording is theirs.
                return Err(DeviceError::CommunicationError(format!(
                    "could not verify who owns the active recording before {} ({}); refusing. Retry when the phone is reachable, or send the command with force=true",
                    verb, e
                )));
            }
        };
        match status.recording.filter(Self::recording_is_active) {
            None => {
                warn!(
                    device = "pupil",
                    ours = ?self.recording_id,
                    "No active recording on the phone — nothing to {} (stopped by another client?)", verb
                );
                self.recording_id = None;
                self.recording_owned = false;
                Ok(false)
            }
            Some(rec)
                if self.recording_owned
                    && self.recording_id.as_deref() == Some(rec.id.as_str()) =>
            {
                Ok(true)
            }
            Some(rec) => Err(DeviceError::RecordingNotOwned(format!(
                "refusing to {} recording {} — it was not started by this bridge (ours: {}). Another station may be using this phone; send the command with force=true to override",
                verb,
                rec.id,
                self.recording_id.as_deref().unwrap_or("none")
            ))),
        }
    }

    /// Stop the current recording and save it.
    ///
    /// Only stops a recording this bridge started unless `force` is set — an
    /// unconditional stop is exactly how one station ended the other
    /// participant's recording mid-experiment in the dyad sessions.
    pub async fn stop_recording(&mut self, force: bool) -> Result<(), DeviceError> {
        if !self.check_recording_ownership("stop", force).await? {
            return Ok(());
        }
        let url = format!("{}/recording:stop_and_save", self.base_url);
        let resp = self.http_client.post(&url).send().await.map_err(|e| {
            DeviceError::CommunicationError(format!("POST recording:stop_and_save failed: {}", e))
        })?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(DeviceError::CommunicationError(format!(
                "recording:stop_and_save returned {} — {}",
                status, body
            )));
        }

        // Read body as text first so we can log it on parse failure.
        // The HTTP 2xx already confirms the recording was stopped server-side,
        // so a response-shape mismatch should not be treated as an error.
        let body_text = resp.text().await.unwrap_or_default();
        match serde_json::from_str::<ApiResponse<RecordingStopResponse>>(&body_text) {
            Ok(_envelope) => {
                info!(device = "pupil", "Recording stopped and saved");
            }
            Err(e) => {
                warn!(
                    device = "pupil",
                    body = %body_text,
                    error = %e,
                    "Could not parse stop_and_save response (recording was stopped successfully)"
                );
            }
        }

        self.recording_id = None;
        self.recording_owned = false;
        Ok(())
    }

    /// Cancel the current recording without saving. Same ownership guard as
    /// [`stop_recording`](Self::stop_recording).
    pub async fn cancel_recording(&mut self, force: bool) -> Result<(), DeviceError> {
        if !self.check_recording_ownership("cancel", force).await? {
            return Ok(());
        }
        let url = format!("{}/recording:cancel", self.base_url);
        let resp = self.http_client.post(&url).send().await.map_err(|e| {
            DeviceError::CommunicationError(format!("POST recording:cancel failed: {}", e))
        })?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(DeviceError::CommunicationError(format!(
                "recording:cancel returned {} — {}",
                status, body
            )));
        }

        // Read body as text first — HTTP 2xx confirms cancellation succeeded.
        let body_text = resp.text().await.unwrap_or_default();
        match serde_json::from_str::<ApiResponse<RecordingCancelResponse>>(&body_text) {
            Ok(_envelope) => {
                info!(device = "pupil", "Recording cancelled");
            }
            Err(e) => {
                warn!(
                    device = "pupil",
                    body = %body_text,
                    error = %e,
                    "Could not parse cancel response (recording was cancelled successfully)"
                );
            }
        }

        self.recording_id = None;
        self.recording_owned = false;
        Ok(())
    }

    /// Send an event annotation to the Neon Companion App.
    ///
    /// Timestamps are in nanoseconds since Unix epoch. If `timestamp_ns` is None,
    /// the Companion App uses its own clock.
    pub async fn send_neon_event(
        &mut self,
        name: &str,
        timestamp_ns: Option<i64>,
    ) -> Result<EventResponse, DeviceError> {
        let url = format!("{}/event", self.base_url);
        let body = EventRequest {
            name: name.to_string(),
            timestamp: timestamp_ns,
        };

        let resp = self
            .http_client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                DeviceError::CommunicationError(format!("POST /api/event failed: {}", e))
            })?;

        if !resp.status().is_success() {
            let status = resp.status();
            let resp_body = resp.text().await.unwrap_or_default();
            return Err(DeviceError::CommunicationError(format!(
                "POST /api/event returned {} — {}",
                status, resp_body
            )));
        }

        let envelope: ApiResponse<EventResponse> = resp.json().await.map_err(|e| {
            DeviceError::InvalidData(format!("Failed to parse event response: {}", e))
        })?;

        // The Neon API does not return the event name — inject it manually
        let mut event_resp = envelope.result;
        event_resp.name = name.to_string();

        // Cross-check the recording the phone attached this event to against the
        // one this bridge started. A mismatch means something else is driving the
        // phone — e.g. a second bridge whose `neon.local` resolved to the same
        // device — or our recording was stopped out from under us. Either way the
        // event is not in the recording HyperStudy thinks it is, so shout.
        if let Some(expected) = self.recording_id.as_deref() {
            match event_resp.recording_id.as_deref() {
                Some(actual) if actual == expected => {}
                Some(actual) => warn!(
                    device = "pupil",
                    event = %name,
                    expected_recording_id = %expected,
                    actual_recording_id = %actual,
                    "Neon event attached to a different recording than the one this bridge started — is another client controlling this phone?"
                ),
                None => warn!(
                    device = "pupil",
                    event = %name,
                    expected_recording_id = %expected,
                    "Neon event not attached to any recording although this bridge started one — was the recording stopped externally?"
                ),
            }
        }

        info!(
            device = "pupil",
            event = %event_resp.name,
            recording_id = ?event_resp.recording_id,
            timestamp = event_resp.timestamp,
            "Neon event sent"
        );
        Ok(event_resp)
    }

    /// Route a JSON command to the appropriate REST endpoint.
    async fn route_command(&mut self, data: &[u8]) -> Result<serde_json::Value, DeviceError> {
        let text = String::from_utf8_lossy(data);

        let cmd: PupilCommand = serde_json::from_str(&text)
            .map_err(|e| DeviceError::InvalidData(format!("Invalid command JSON: {}", e)))?;

        match cmd.command.as_str() {
            "recording_start" => {
                let id = self.start_recording().await?;
                Ok(serde_json::json!({ "recording_id": id }))
            }
            "recording_stop" => {
                self.stop_recording(cmd.force).await?;
                Ok(serde_json::json!({ "success": true }))
            }
            "recording_cancel" => {
                self.cancel_recording(cmd.force).await?;
                Ok(serde_json::json!({ "success": true }))
            }
            "event" => {
                let name = cmd.name.unwrap_or_else(|| "unnamed".to_string());
                let resp = self.send_neon_event(&name, cmd.timestamp).await?;
                Ok(serde_json::json!({
                    "name": resp.name,
                    "recording_id": resp.recording_id,
                    "timestamp": resp.timestamp,
                }))
            }
            "status" => {
                let status = self.get_neon_status().await?;
                Ok(serde_json::to_value(&status)
                    .map_err(|e| DeviceError::InvalidData(e.to_string()))?)
            }
            other => Err(DeviceError::InvalidData(format!(
                "Unknown Pupil command: {}",
                other
            ))),
        }
    }
}

#[async_trait]
impl Device for PupilDevice {
    async fn connect(&mut self) -> Result<(), DeviceError> {
        // Resolve .local mDNS hostnames to IP before making HTTP requests,
        // since reqwest/hyper's DNS resolver may not handle mDNS correctly.
        self.resolve_mdns_if_needed().await;

        info!(
            device = "pupil",
            "Connecting to Neon Companion at {}", self.base_url
        );
        self.status = DeviceStatus::Connecting;
        self.connection_retry_count = 0;

        let connect_timeout = Duration::from_millis(self.config.timeout_ms);

        loop {
            match timeout(connect_timeout, self.get_neon_status()).await {
                Ok(Ok(status)) => {
                    if let Some(expected) = self.expected_device_id.as_deref() {
                        if status.phone.device_id != expected {
                            self.status = DeviceStatus::Error;
                            self.neon_status = None;
                            error!(
                                device = "pupil",
                                expected = %expected,
                                actual = %status.phone.device_id,
                                actual_name = %status.phone.device_name,
                                "A different Neon phone answered than this station is pinned to"
                            );
                            return Err(DeviceError::WrongDevice {
                                expected: expected.to_string(),
                                actual: status.phone.device_id.clone(),
                                actual_name: status.phone.device_name.clone(),
                            });
                        }
                    }

                    self.neon_status = Some(status.clone());
                    self.status = DeviceStatus::Connected;
                    self.connection_retry_count = 0;

                    info!(
                        device = "pupil",
                        device_name = %status.phone.device_name,
                        device_id = %status.phone.device_id,
                        battery = %status.phone.battery_level,
                        "Connected to Neon Companion"
                    );

                    // Adopt an already-running recording for event attribution, but
                    // remember we didn't start it: stop/cancel will refuse without
                    // force, and recording_start will report the phone as busy.
                    // If this bridge DID start it (before a reconnect), the
                    // WebSocket connect handler restores ownership afterwards via
                    // `reclaim_pupil_recording` from AppState::owned_recordings.
                    if let Some(rec) = status
                        .recording
                        .as_ref()
                        .filter(|r| Self::recording_is_active(r))
                    {
                        self.recording_id = Some(rec.id.clone());
                        self.recording_owned = false;
                        warn!(
                            device = "pupil",
                            recording_id = %rec.id,
                            "Phone already has an active recording that this bridge did not start"
                        );
                    }

                    return Ok(());
                }
                Ok(Err(e)) => {
                    self.connection_retry_count += 1;
                    error!(
                        device = "pupil",
                        "Failed to connect to Neon Companion: {}", e
                    );

                    if self.config.auto_reconnect && self.connection_retry_count < self.max_retries
                    {
                        warn!(
                            device = "pupil",
                            "Retrying connection ({}/{})",
                            self.connection_retry_count,
                            self.max_retries
                        );
                        tokio::time::sleep(Duration::from_millis(
                            self.config.reconnect_interval_ms,
                        ))
                        .await;
                        continue;
                    }

                    self.status = DeviceStatus::Error;
                    return Err(DeviceError::ConnectionFailed(format!(
                        "Cannot reach Neon Companion at {}: {}",
                        self.base_url, e
                    )));
                }
                Err(_) => {
                    self.connection_retry_count += 1;
                    error!(device = "pupil", "Connection timeout to Neon Companion");

                    if self.config.auto_reconnect && self.connection_retry_count < self.max_retries
                    {
                        warn!(
                            device = "pupil",
                            "Retrying after timeout ({}/{})",
                            self.connection_retry_count,
                            self.max_retries
                        );
                        tokio::time::sleep(Duration::from_millis(
                            self.config.reconnect_interval_ms,
                        ))
                        .await;
                        continue;
                    }

                    self.status = DeviceStatus::Error;
                    return Err(DeviceError::Timeout);
                }
            }
        }
    }

    async fn disconnect(&mut self) -> Result<(), DeviceError> {
        info!(device = "pupil", "Disconnecting from Neon Companion");

        self.status = DeviceStatus::Disconnected;
        self.neon_status = None;
        self.recording_id = None;
        self.recording_owned = false;
        self.connection_retry_count = 0;

        info!(device = "pupil", "Disconnected from Neon Companion");
        Ok(())
    }

    async fn send(&mut self, data: &[u8]) -> Result<(), DeviceError> {
        if self.status != DeviceStatus::Connected {
            return Err(DeviceError::NotConnected);
        }

        match self.route_command(data).await {
            Ok(result) => {
                debug!(device = "pupil", result = %result, "Command executed");
                Ok(())
            }
            Err(e) => {
                error!(device = "pupil", "Command failed: {}", e);
                Err(e)
            }
        }
    }

    fn get_info(&self) -> DeviceInfo {
        let mut metadata = serde_json::json!({
            "base_url": self.base_url,
            "device_ip": self.device_ip,
            "recording_id": self.recording_id,
            "recording_owned": self.recording_owned,
            "expected_device_id": self.expected_device_id,
        });

        if let Some(ref status) = self.neon_status {
            metadata["device_name"] = serde_json::json!(status.phone.device_name);
            metadata["device_id"] = serde_json::json!(status.phone.device_id);
            metadata["battery_level"] = serde_json::json!(status.phone.battery_level);
            metadata["battery_state"] = serde_json::json!(status.phone.battery_state);
            metadata["memory_state"] = serde_json::json!(status.phone.memory_state);
            metadata["sensor_count"] = serde_json::json!(status.sensors.len());
            // The port the phone's Time Echo service listens on. The recorder reaches it at the host of base_url,
            // the address Bridge itself connected to, never at an IP the phone reports.
            metadata["time_echo_port"] = serde_json::json!(status.phone.time_echo_port);
        }

        DeviceInfo {
            id: format!("pupil_{}", self.device_ip.replace('.', "_")),
            name: format!("Pupil Labs Neon ({})", self.device_ip),
            device_type: DeviceType::Pupil,
            status: self.status,
            metadata,
        }
    }

    fn get_status(&self) -> DeviceStatus {
        self.status
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn configure(&mut self, config: DeviceConfig) -> Result<(), DeviceError> {
        info!(device = "pupil", "Configuring Pupil device");
        self.config = config;

        if let Some(custom) = self.config.custom_settings.as_object() {
            if let Some(id) = custom.get("expected_device_id").and_then(|v| v.as_str()) {
                self.expected_device_id = Some(id.to_string()).filter(|s| !s.is_empty());
            }

            // Update device IP/URL if provided
            if let Some(ip) = custom.get("device_ip").and_then(|v| v.as_str()) {
                let (device_ip, base_url) = Self::parse_host(ip);
                self.device_ip = device_ip;
                self.base_url = base_url;
            }

            if let Some(url) = custom.get("device_url").and_then(|v| v.as_str()) {
                let (device_ip, base_url) = Self::parse_host(url);
                self.device_ip = device_ip;
                self.base_url = base_url;
            }

            // Update max retries if provided
            if let Some(retries) = custom.get("max_retries").and_then(|v| v.as_u64()) {
                self.max_retries = retries as u32;
            }
        }

        // Rebuild HTTP client with updated timeout
        self.http_client = reqwest::Client::builder()
            .timeout(Duration::from_millis(self.config.timeout_ms))
            .build()
            .unwrap_or_default();

        info!(device = "pupil", "Pupil device configuration updated");
        Ok(())
    }

    async fn test_connection(&mut self) -> Result<bool, DeviceError> {
        let test_url = format!("{}/status", self.base_url);
        let test_client = reqwest::Client::builder()
            .timeout(Duration::from_secs(3))
            .build()
            .unwrap_or_default();

        match test_client.get(&test_url).send().await {
            Ok(resp) => Ok(resp.status().is_success()),
            Err(_) => Ok(false),
        }
    }

    async fn send_event(
        &mut self,
        event: serde_json::Value,
    ) -> Result<serde_json::Value, DeviceError> {
        if self.status != DeviceStatus::Connected {
            return Err(DeviceError::NotConnected);
        }

        let name = event
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("unnamed");
        let timestamp = event.get("timestamp").and_then(|v| v.as_i64());

        let resp = self.send_neon_event(name, timestamp).await?;
        // Let the client see when the phone filed this event somewhere other than
        // the recording we started (or nowhere) — not just the log.
        let recording_mismatch =
            self.recording_id.is_some() && resp.recording_id != self.recording_id;
        Ok(serde_json::json!({
            "success": true,
            "name": resp.name,
            "recording_id": resp.recording_id,
            "expected_recording_id": self.recording_id,
            "recording_mismatch": recording_mismatch,
            "timestamp": resp.timestamp,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pupil_device_creation() {
        let device = PupilDevice::new("192.168.1.100".to_string());

        assert_eq!(device.get_status(), DeviceStatus::Disconnected);
        assert!(device.recording_id.is_none());
        assert_eq!(device.base_url, "http://192.168.1.100:8080/api");
        assert_eq!(device.device_ip, "192.168.1.100");
    }

    #[test]
    fn test_pupil_device_creation_with_port() {
        let device = PupilDevice::new("neon.local:8080".to_string());

        assert_eq!(device.base_url, "http://neon.local:8080/api");
        assert_eq!(device.device_ip, "neon.local");
        assert_eq!(device.get_status(), DeviceStatus::Disconnected);
    }

    #[test]
    fn test_pupil_device_creation_default_port() {
        let device = PupilDevice::new("neon.local".to_string());

        assert_eq!(device.base_url, "http://neon.local:8080/api");
        assert_eq!(device.device_ip, "neon.local");
    }

    #[test]
    fn test_pupil_device_strips_protocol() {
        let device = PupilDevice::new("http://192.168.1.100:8080".to_string());
        assert_eq!(device.base_url, "http://192.168.1.100:8080/api");

        let device2 = PupilDevice::new("ws://192.168.1.100:8080".to_string());
        assert_eq!(device2.base_url, "http://192.168.1.100:8080/api");
    }

    #[test]
    fn test_device_info() {
        let device = PupilDevice::new("192.168.1.100".to_string());
        let info = device.get_info();

        assert_eq!(info.device_type, DeviceType::Pupil);
        assert!(info.name.contains("192.168.1.100"));
        assert_eq!(info.id, "pupil_192_168_1_100");
    }

    #[test]
    fn test_device_configuration() {
        let mut device = PupilDevice::new("192.168.1.100".to_string());

        let mut config = DeviceConfig::default();
        config.timeout_ms = 10000;
        config.custom_settings = serde_json::json!({
            "device_ip": "192.168.1.101:8080",
            "max_retries": 5,
        });

        let result = device.configure(config);
        assert!(result.is_ok());
        assert_eq!(device.base_url, "http://192.168.1.101:8080/api");
        assert_eq!(device.max_retries, 5);
    }

    #[test]
    fn test_neon_status_from_api_response() {
        // Simulate the actual Neon API response format:
        // {"message": "...", "result": [{"model": "Phone", "data": {...}}, ...]}
        let api_json = serde_json::json!({
            "message": "success",
            "result": [
                {
                    "model": "Phone",
                    "data": {
                        "ip": "192.168.1.100",
                        "device_id": "abc123",
                        "device_name": "Neon Test",
                        "battery_level": 0.85,
                        "battery_state": "OK",
                        "memory": 4000000000_u64,
                        "memory_state": "OK"
                    }
                },
                {
                    "model": "Hardware",
                    "data": {
                        "version": "2.0",
                        "glasses_serial": "GL-001",
                        "world_camera_serial": "WC-001"
                    }
                },
                {
                    "model": "Sensor",
                    "data": {
                        "sensor": "world",
                        "conn_type": "DIRECT",
                        "protocol": "rtsp",
                        "ip": "192.168.1.100",
                        "port": 8086,
                        "connected": true
                    }
                },
                {
                    "model": "Sensor",
                    "data": {
                        "sensor": "gaze",
                        "conn_type": "WEBSOCKET",
                        "connected": true
                    }
                },
                {
                    "model": "Recording",
                    "data": {
                        "id": "550e8400-e29b-41d4-a716-446655440000",
                        "action": "START",
                        "rec_duration_ns": 5000000000_u64
                    }
                }
            ]
        });

        // Parse the envelope
        let envelope: ApiResponse<Vec<StatusItem>> = serde_json::from_value(api_json).unwrap();
        assert_eq!(envelope.message, "success");

        // Parse status from heterogeneous array
        let status = NeonStatus::from_status_items(envelope.result).unwrap();
        assert_eq!(status.phone.device_name, "Neon Test");
        assert_eq!(status.phone.battery_level, 0.85);
        assert!(status.hardware.is_some());
        assert_eq!(
            status.hardware.unwrap().glasses_serial.as_deref(),
            Some("GL-001")
        );
        assert_eq!(status.sensors.len(), 2);
        assert_eq!(status.sensors[0].sensor, "world");
        assert!(status.sensors[0].connected);
        assert!(status.recording.is_some());
        assert_eq!(status.recording.unwrap().action, "START");
    }

    #[test]
    fn test_neon_status_hardware_nulls_when_glasses_unplugged() {
        // The Companion app reports Hardware with explicit nulls when no glasses
        // or module is attached. This used to fail to parse and block connect.
        let items = vec![
            StatusItem {
                model: "Phone".to_string(),
                data: serde_json::json!({
                    "ip": "192.168.1.100",
                    "device_id": "abc123",
                    "device_name": "Neon Companion",
                    "battery_level": 0.9,
                    "battery_state": "OK",
                    "memory": 2000000000_u64,
                    "memory_state": "OK"
                }),
            },
            StatusItem {
                model: "Hardware".to_string(),
                data: serde_json::json!({
                    "version": null,
                    "glasses_serial": null,
                    "world_camera_serial": null,
                    "module_serial": null
                }),
            },
        ];

        let status = NeonStatus::from_status_items(items).unwrap();
        let hw = status
            .hardware
            .expect("hardware item should still be present");
        assert!(hw.glasses_serial.is_none());
        assert!(hw.module_serial.is_none());
    }

    #[test]
    fn test_pupil_command_force_flag() {
        let cmd: PupilCommand = serde_json::from_str(r#"{"command":"recording_stop"}"#).unwrap();
        assert!(!cmd.force);
        let cmd: PupilCommand =
            serde_json::from_str(r#"{"command":"recording_stop","force":true}"#).unwrap();
        assert!(cmd.force);
    }

    #[test]
    fn test_recording_is_active_by_action() {
        let mk = |action: &str| RecordingInfo {
            id: "r1".into(),
            action: action.into(),
            rec_duration_ns: 0,
            message: String::new(),
        };
        assert!(PupilDevice::recording_is_active(&mk("START")));
        assert!(PupilDevice::recording_is_active(&mk("")));
        assert!(!PupilDevice::recording_is_active(&mk("STOP")));
        assert!(!PupilDevice::recording_is_active(&mk("save")));
    }

    #[test]
    fn test_claim_recording_ownership_only_for_the_active_recording() {
        let mut device = PupilDevice::new("192.168.1.100".to_string());
        device.recording_id = Some("rec-1".to_string());
        device.recording_owned = false;
        device.claim_recording_ownership("rec-other");
        assert!(!device.recording_owned);
        device.claim_recording_ownership("rec-1");
        assert!(device.recording_owned);
    }

    #[test]
    fn test_device_error_codes() {
        assert_eq!(DeviceError::PhoneBusy("x".into()).code(), "phone_busy");
        assert_eq!(
            DeviceError::RecordingNotOwned("x".into()).code(),
            "recording_not_owned"
        );
        assert_eq!(DeviceError::NotConnected.code(), "not_connected");
    }

    #[test]
    fn test_neon_status_missing_phone() {
        // Status response without a Phone component should fail
        let items = vec![StatusItem {
            model: "Sensor".to_string(),
            data: serde_json::json!({"sensor": "world", "conn_type": "DIRECT"}),
        }];

        let result = NeonStatus::from_status_items(items);
        assert!(result.is_err());
    }

    #[test]
    fn test_neon_status_minimal() {
        // Minimal status with just Phone (no hardware, sensors, or recording)
        let items = vec![StatusItem {
            model: "Phone".to_string(),
            data: serde_json::json!({
                "ip": "192.168.1.100",
                "device_id": "abc123",
                "device_name": "Neon Minimal",
                "battery_level": 0.5,
                "battery_state": "OK",
                "memory": 2000000000_u64,
                "memory_state": "OK"
            }),
        }];

        let status = NeonStatus::from_status_items(items).unwrap();
        assert_eq!(status.phone.device_name, "Neon Minimal");
        assert!(status.hardware.is_none());
        assert!(status.sensors.is_empty());
        assert!(status.recording.is_none());
    }

    #[test]
    fn test_recording_start_envelope() {
        let json = serde_json::json!({
            "message": "Recording started",
            "result": {
                "id": "550e8400-e29b-41d4-a716-446655440000"
            }
        });

        let envelope: ApiResponse<RecordingStartResponse> = serde_json::from_value(json).unwrap();
        assert_eq!(envelope.result.id, "550e8400-e29b-41d4-a716-446655440000");
    }

    #[test]
    fn test_event_response_envelope() {
        // Note: The Neon API does not return the event name in the response
        let json = serde_json::json!({
            "message": "Event sent",
            "result": {
                "recording_id": "550e8400-e29b-41d4-a716-446655440000",
                "timestamp": 1700000000000000000_i64
            }
        });

        let envelope: ApiResponse<EventResponse> = serde_json::from_value(json).unwrap();
        assert_eq!(
            envelope.result.recording_id,
            Some("550e8400-e29b-41d4-a716-446655440000".to_string())
        );
        assert_eq!(envelope.result.timestamp, 1700000000000000000);
        assert_eq!(envelope.result.name, ""); // Not returned by API
    }

    #[test]
    fn test_event_response_envelope_no_recording() {
        let json = serde_json::json!({
            "message": "Event sent",
            "result": {
                "timestamp": 1700000000000000000_i64
            }
        });

        let envelope: ApiResponse<EventResponse> = serde_json::from_value(json).unwrap();
        assert_eq!(envelope.result.recording_id, None);
        assert_eq!(envelope.result.timestamp, 1700000000000000000);
    }

    #[test]
    fn test_event_request_serialization() {
        let event = EventRequest {
            name: "stimulus_onset".to_string(),
            timestamp: Some(1700000000_000_000_000),
        };
        let json = serde_json::to_value(&event).unwrap();

        assert_eq!(json["name"], "stimulus_onset");
        assert_eq!(json["timestamp"], 1700000000_000_000_000_i64);
    }

    #[test]
    fn test_event_request_without_timestamp() {
        let event = EventRequest {
            name: "marker".to_string(),
            timestamp: None,
        };
        let json = serde_json::to_value(&event).unwrap();

        assert_eq!(json["name"], "marker");
        assert!(json.get("timestamp").is_none());
    }

    #[test]
    fn test_command_routing_parse() {
        let cmd_json = r#"{"command": "recording_start"}"#;
        let cmd: PupilCommand = serde_json::from_str(cmd_json).unwrap();
        assert_eq!(cmd.command, "recording_start");

        let cmd_json = r#"{"command": "event", "name": "stim", "timestamp": 1700000000000000000}"#;
        let cmd: PupilCommand = serde_json::from_str(cmd_json).unwrap();
        assert_eq!(cmd.command, "event");
        assert_eq!(cmd.name.unwrap(), "stim");
        assert_eq!(cmd.timestamp.unwrap(), 1700000000_000_000_000);
    }

    /// Minimal HTTP server answering every request with `body` as JSON — enough
    /// for `PupilDevice::connect()`'s `GET /api/status`.
    async fn spawn_status_server(body: serde_json::Value) -> std::net::SocketAddr {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((mut sock, _)) = listener.accept().await {
                let body = body.to_string();
                tokio::spawn(async move {
                    let mut buf = [0u8; 4096];
                    let _ = sock.read(&mut buf).await;
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = sock.write_all(resp.as_bytes()).await;
                    let _ = sock.shutdown().await;
                });
            }
        });
        addr
    }

    fn status_json(device_id: &str, device_name: &str) -> serde_json::Value {
        serde_json::json!({
            "message": "success",
            "result": [
                {"model": "Phone", "data": {
                    "ip": "127.0.0.1", "device_id": device_id, "device_name": device_name,
                    "battery_level": 85, "battery_state": "OK",
                    "memory": 4000000000_u64, "memory_state": "OK"
                }},
                {"model": "Hardware", "data": {
                    "version": "2.0", "glasses_serial": "GL-001", "world_camera_serial": "WC-001"
                }}
            ]
        })
    }

    #[tokio::test]
    async fn test_connect_refuses_a_phone_other_than_the_pinned_one() {
        // The safety property behind pin-by-hardware-id: when a different phone
        // answers than the station is pinned to, connect() must refuse — fast,
        // without retries — so a station can never drive the other
        // participant's phone.
        let addr = spawn_status_server(status_json("other-phone", "Neon Companion")).await;
        let mut device = PupilDevice::new(addr.to_string())
            .with_expected_device_id(Some("pinned-phone".to_string()));
        let started = std::time::Instant::now();
        match device.connect().await {
            Err(DeviceError::WrongDevice {
                expected,
                actual,
                actual_name,
            }) => {
                assert_eq!(expected, "pinned-phone");
                assert_eq!(actual, "other-phone");
                assert_eq!(actual_name, "Neon Companion");
            }
            other => panic!("expected WrongDevice, got {:?}", other),
        }
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "WrongDevice must fail fast, not burn the retry loop"
        );
        assert_eq!(device.get_status(), DeviceStatus::Error);
        assert!(
            device.neon_status.is_none(),
            "no identity adopted from the wrong phone"
        );

        // The pinned phone itself connects normally.
        let addr = spawn_status_server(status_json("pinned-phone", "Neon Companion")).await;
        let mut device = PupilDevice::new(addr.to_string())
            .with_expected_device_id(Some("pinned-phone".to_string()));
        device.connect().await.expect("the pinned phone connects");
        assert_eq!(device.get_status(), DeviceStatus::Connected);
        assert_eq!(device.get_info().metadata["device_id"], "pinned-phone");

        // Unpinned stations accept whichever phone answers (legacy behaviour).
        let addr = spawn_status_server(status_json("any-phone", "Neon Companion")).await;
        let mut device = PupilDevice::new(addr.to_string());
        device
            .connect()
            .await
            .expect("unpinned connect accepts any phone");
        assert_eq!(device.get_info().metadata["device_id"], "any-phone");
        // The recorder reaches the phone's Time Echo service from these.
        let info = device.get_info().metadata;
        assert!(info["time_echo_port"].is_u64());
    }
}

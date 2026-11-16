use super::{Device, DeviceConfig, DeviceError, DeviceInfo, DeviceStatus, DeviceType};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use socket2::SockRef;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::{sleep, timeout};
use tracing::{debug, error, info, warn};

const DEFAULT_PORT: u16 = 6767;
const CONNECTION_HEALTH_CHECK_INTERVAL_MS: u64 = 10000; // Retained for config schema compatibility (unused since TCP keepalive replacement)
const MAX_RECONNECT_ATTEMPTS: u32 = 10;
const INITIAL_RECONNECT_DELAY_MS: u64 = 1000;
const MAX_RECONNECT_DELAY_MS: u64 = 30000;
const CONNECTION_TIMEOUT_MS: u64 = 5000;
const IO_TIMEOUT_MS: u64 = 5000; // Timeout for individual read/write operations
const READ_BUFFER_SIZE: usize = 8192;

/// Event structure conforming to the Kernel Tasks SDK protocol.
///
/// According to the Kernel Tasks SDK documentation, events must be sent as
/// length-prefixed JSON with this schema. The protocol uses:
/// - 4-byte big-endian u32 length prefix
/// - JSON-encoded event payload
///
/// ## Event Hierarchy
///
/// The Kernel Tasks SDK defines a required event hierarchy for proper data analysis:
/// `experiment > task > block > trial`
///
/// - **Required**: First event must be `start_experiment`, last must be `end_experiment`
/// - **Optional**: `task`, `block`, and `trial` levels can be used to structure your experiment
/// - **Metadata**: Events not prefixed with `start_`, `end_`, or `event_` are treated as metadata
///
/// ## Timestamps
///
/// Timestamps are sent as microseconds since Unix epoch. The Kernel acquisition
/// software "zeros" these to the beginning of the fNIRS data recording.
///
/// Reference: <https://docs.kernel.com/docs/kernel-tasks-sdk>
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KernelEvent {
    /// Unique identifier for the event
    pub id: i64,
    /// Timestamp in microseconds since Unix epoch (zeroed by Kernel to fNIRS start)
    pub timestamp: i64,
    /// Event type name (e.g., "start_experiment", "stimulus_onset", "response")
    pub event: String,
    /// Event value - can be a string or complex object (dict values become columns in SNIRF export)
    pub value: serde_json::Value,
}

impl KernelEvent {
    /// Create a new KernelEvent with the current timestamp
    pub fn new(id: i64, event: impl Into<String>, value: serde_json::Value) -> Self {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_micros() as i64)
            .unwrap_or(0);

        Self {
            id,
            timestamp,
            event: event.into(),
            value,
        }
    }

    /// Create a new KernelEvent with a specific timestamp
    pub fn with_timestamp(
        id: i64,
        timestamp: i64,
        event: impl Into<String>,
        value: serde_json::Value,
    ) -> Self {
        Self {
            id,
            timestamp,
            event: event.into(),
            value,
        }
    }

    /// Serialize the event to the Kernel Tasks SDK wire format.
    ///
    /// Returns bytes in the format: [4-byte big-endian length][JSON payload]
    pub fn to_wire_format(&self) -> Result<Vec<u8>, DeviceError> {
        let json_bytes = serde_json::to_vec(self).map_err(|e| {
            DeviceError::CommunicationError(format!("JSON serialization failed: {}", e))
        })?;

        let length = json_bytes.len() as u32;
        let length_bytes = length.to_be_bytes();

        let mut wire_data = Vec::with_capacity(4 + json_bytes.len());
        wire_data.extend_from_slice(&length_bytes);
        wire_data.extend_from_slice(&json_bytes);

        Ok(wire_data)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KernelConfig {
    pub ip_address: String,
    pub port: u16,
    pub connection_timeout_ms: u64,
    /// Formerly used for time-based staleness checks (now replaced by TCP keepalive).
    /// Retained for config schema compatibility; not used in health decisions.
    pub connection_health_check_interval_ms: u64,
    pub max_reconnect_attempts: u32,
    pub initial_reconnect_delay_ms: u64,
    pub max_reconnect_delay_ms: u64,
    pub buffer_size: usize,
}

impl Default for KernelConfig {
    fn default() -> Self {
        Self {
            ip_address: "127.0.0.1".to_string(),
            port: DEFAULT_PORT,
            connection_timeout_ms: CONNECTION_TIMEOUT_MS,
            connection_health_check_interval_ms: CONNECTION_HEALTH_CHECK_INTERVAL_MS,
            max_reconnect_attempts: MAX_RECONNECT_ATTEMPTS,
            initial_reconnect_delay_ms: INITIAL_RECONNECT_DELAY_MS,
            max_reconnect_delay_ms: MAX_RECONNECT_DELAY_MS,
            buffer_size: READ_BUFFER_SIZE,
        }
    }
}

pub struct KernelDevice {
    socket: Option<TcpStream>,
    status: DeviceStatus,
    config: KernelConfig,
    device_config: DeviceConfig,
    buffer: Vec<u8>,
    reconnect_attempts: u32,
    last_successful_operation: Option<Instant>,
    last_successful_connection: Option<Instant>,
    /// Counter for generating unique event IDs
    next_event_id: i64,
}

impl std::fmt::Debug for KernelDevice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KernelDevice")
            .field("ip_address", &self.config.ip_address)
            .field("port", &self.config.port)
            .field("status", &self.status)
            .field("config", &self.config)
            .field("device_config", &self.device_config)
            .field("reconnect_attempts", &self.reconnect_attempts)
            .field("last_successful_operation", &self.last_successful_operation)
            .field(
                "last_successful_connection",
                &self.last_successful_connection,
            )
            .field("next_event_id", &self.next_event_id)
            .finish()
    }
}

impl KernelDevice {
    pub fn new(ip_address: String) -> Self {
        Self {
            socket: None,
            status: DeviceStatus::Disconnected,
            config: KernelConfig {
                ip_address,
                ..Default::default()
            },
            device_config: DeviceConfig::default(),
            buffer: Vec::with_capacity(READ_BUFFER_SIZE),
            reconnect_attempts: 0,
            last_successful_operation: None,
            last_successful_connection: None,
            next_event_id: 1,
        }
    }

    /// Calculate exponential backoff delay
    fn calculate_backoff_delay(&self) -> Duration {
        let base_delay = self.config.initial_reconnect_delay_ms;
        let max_delay = self.config.max_reconnect_delay_ms;

        // Exponential backoff: base * 2^attempts, capped at max_delay
        let delay_ms = (base_delay * (2_u64.pow(self.reconnect_attempts.min(10)))).min(max_delay);

        Duration::from_millis(delay_ms)
    }

    /// Attempt to establish TCP connection with timeout
    async fn establish_connection(&mut self) -> Result<TcpStream, DeviceError> {
        let addr = format!("{}:{}", self.config.ip_address, self.config.port);
        let connection_timeout = Duration::from_millis(self.config.connection_timeout_ms);

        debug!(
            device = "kernel",
            "Attempting to connect to Kernel Flow2 at {}", addr
        );

        match timeout(connection_timeout, TcpStream::connect(&addr)).await {
            Ok(Ok(socket)) => {
                // Disable Nagle's algorithm for low-latency event delivery.
                // Kernel events are small, infrequent JSON payloads — Nagle would
                // buffer them for up to 40ms with no throughput benefit.
                if let Err(e) = socket.set_nodelay(true) {
                    warn!(device = "kernel", "Failed to set TCP_NODELAY: {}", e);
                }

                // Enable TCP keepalive for dead connection detection.
                // This replaces the time-based staleness heuristic: the OS sends
                // actual probe packets, so write_all() will fail with a real error
                // if the remote end is dead — no false positives on idle connections.
                let sock_ref = SockRef::from(&socket);
                let keepalive = socket2::TcpKeepalive::new()
                    .with_time(Duration::from_secs(10))
                    .with_interval(Duration::from_secs(2));
                if let Err(e) = sock_ref.set_tcp_keepalive(&keepalive) {
                    warn!(device = "kernel", "Failed to set TCP keepalive: {}", e);
                }

                info!(
                    device = "kernel",
                    "Successfully established connection to Kernel Flow2 at {}", addr
                );
                Ok(socket)
            }
            Ok(Err(e)) => {
                error!(
                    device = "kernel",
                    "TCP connection failed to {}: {}", addr, e
                );
                Err(DeviceError::ConnectionFailed(format!(
                    "TCP connection failed: {}",
                    e
                )))
            }
            Err(_) => {
                error!(
                    device = "kernel",
                    "Connection attempt to {} timed out after {:?}", addr, connection_timeout
                );
                Err(DeviceError::Timeout)
            }
        }
    }

    /// Reconnect with exponential backoff
    async fn attempt_reconnect(&mut self) -> Result<(), DeviceError> {
        if self.reconnect_attempts >= self.config.max_reconnect_attempts {
            error!(
                device = "kernel",
                "Maximum reconnection attempts ({}) reached for Kernel Flow2",
                self.config.max_reconnect_attempts
            );
            self.status = DeviceStatus::Error;
            return Err(DeviceError::ConnectionFailed(
                "Maximum reconnection attempts reached".to_string(),
            ));
        }

        self.reconnect_attempts += 1;
        // The first retry is immediate: the Kernel acquisition app is on
        // localhost and usually just dropped the socket, and this runs with the
        // device lock held — every second here is a second the caller's
        // marker (and anything queued behind it) waits.
        let backoff_delay = if self.reconnect_attempts == 1 {
            Duration::ZERO
        } else {
            self.calculate_backoff_delay()
        };

        warn!(
            device = "kernel",
            "Reconnection attempt {} of {} for Kernel Flow2, waiting {:?}",
            self.reconnect_attempts,
            self.config.max_reconnect_attempts,
            backoff_delay
        );

        if !backoff_delay.is_zero() {
            sleep(backoff_delay).await;
        }

        match self.establish_connection().await {
            Ok(socket) => {
                self.socket = Some(socket);
                self.status = DeviceStatus::Connected;
                self.last_successful_connection = Some(Instant::now());
                self.last_successful_operation = Some(Instant::now());
                self.reconnect_attempts = 0; // Reset on successful connection
                info!(device = "kernel", "Kernel Flow2 reconnection successful");
                Ok(())
            }
            Err(e) => {
                warn!(
                    device = "kernel",
                    "Reconnection attempt {} failed: {}", self.reconnect_attempts, e
                );
                Err(e)
            }
        }
    }

    /// Detect connection health based on socket presence and status.
    ///
    /// Dead-connection detection is handled by TCP keepalive at the OS level
    /// (configured in establish_connection). If the connection is actually dead,
    /// write_all() or flush() will return an error caught by the retry logic in send().
    fn is_connection_healthy(&self) -> bool {
        self.socket.is_some() && self.status == DeviceStatus::Connected
    }

    /// Check if an IO error indicates connection loss
    fn is_io_error_connection_lost(&self, error: &std::io::Error) -> bool {
        matches!(
            error.kind(),
            std::io::ErrorKind::ConnectionReset
                | std::io::ErrorKind::ConnectionAborted
                | std::io::ErrorKind::BrokenPipe
                | std::io::ErrorKind::UnexpectedEof
        )
    }

    /// Check if a DeviceError indicates connection loss (for retry decisions)
    fn is_connection_lost_error(&self, error: &DeviceError) -> bool {
        matches!(
            error,
            DeviceError::ConnectionFailed(_) | DeviceError::Timeout
        )
    }

    /// Attempt write_all + flush on the current socket.
    /// Returns Ok on success, or a DeviceError on failure.
    /// Does NOT modify self.socket on error — the caller decides whether to retry.
    async fn try_send(&mut self, data: &[u8]) -> Result<Duration, DeviceError> {
        let socket = self.socket.as_mut().ok_or(DeviceError::NotConnected)?;
        let io_timeout = Duration::from_millis(IO_TIMEOUT_MS);

        let start = Instant::now();
        let result = timeout(io_timeout, socket.write_all(data)).await;
        let latency = start.elapsed();

        match result {
            Ok(Ok(_)) => {
                // Flush with timeout
                match timeout(io_timeout, socket.flush()).await {
                    Ok(Ok(_)) => Ok(latency),
                    Ok(Err(flush_err)) => {
                        if self.is_io_error_connection_lost(&flush_err) {
                            Err(DeviceError::ConnectionFailed(format!(
                                "Connection lost during flush: {}",
                                flush_err
                            )))
                        } else {
                            Err(DeviceError::CommunicationError(format!(
                                "Flush failed: {}",
                                flush_err
                            )))
                        }
                    }
                    Err(_) => Err(DeviceError::Timeout),
                }
            }
            Ok(Err(send_err)) => {
                if self.is_io_error_connection_lost(&send_err) {
                    Err(DeviceError::ConnectionFailed(format!(
                        "Connection lost during send: {}",
                        send_err
                    )))
                } else {
                    Err(DeviceError::CommunicationError(format!(
                        "Send failed: {}",
                        send_err
                    )))
                }
            }
            Err(_) => Err(DeviceError::Timeout),
        }
    }

    /// Get connection uptime
    pub fn get_connection_uptime(&self) -> Option<Duration> {
        self.last_successful_connection.map(|t| t.elapsed())
    }

    /// Get current reconnection attempt count
    pub fn get_reconnect_attempts(&self) -> u32 {
        self.reconnect_attempts
    }

    /// Reset reconnection attempts counter
    pub fn reset_reconnect_attempts(&mut self) {
        self.reconnect_attempts = 0;
    }

    /// Check if device is currently connected
    pub fn is_connected(&self) -> bool {
        self.socket.is_some() && self.status == DeviceStatus::Connected
    }

    /// Get kernel-specific configuration
    pub fn get_kernel_config(&self) -> &KernelConfig {
        &self.config
    }

    /// Update kernel-specific configuration
    pub fn update_kernel_config(&mut self, config: KernelConfig) {
        self.config = config;
        // Resize buffer if size changed
        if self.buffer.capacity() < self.config.buffer_size {
            self.buffer
                .reserve(self.config.buffer_size - self.buffer.capacity());
        }
    }
}

#[async_trait]
impl Device for KernelDevice {
    async fn connect(&mut self) -> Result<(), DeviceError> {
        info!(
            device = "kernel",
            "Connecting to Kernel Flow2 at {}:{}", self.config.ip_address, self.config.port
        );
        self.status = DeviceStatus::Connecting;
        self.reconnect_attempts = 0; // Reset reconnection attempts on new connection

        match self.establish_connection().await {
            Ok(socket) => {
                self.socket = Some(socket);
                self.status = DeviceStatus::Connected;
                self.last_successful_connection = Some(Instant::now());
                self.last_successful_operation = Some(Instant::now());
                self.reconnect_attempts = 0;
                info!(device = "kernel", "Successfully connected to Kernel Flow2");
                Ok(())
            }
            Err(e) => {
                self.status = DeviceStatus::Error;
                error!(
                    device = "kernel",
                    "Failed to connect to Kernel Flow2: {}", e
                );
                Err(e)
            }
        }
    }

    async fn disconnect(&mut self) -> Result<(), DeviceError> {
        info!(device = "kernel", "Disconnecting from Kernel Flow2");

        if let Some(mut socket) = self.socket.take() {
            // Try to gracefully shutdown with timeout to prevent hanging
            let shutdown_timeout = Duration::from_secs(2);
            match timeout(shutdown_timeout, socket.shutdown()).await {
                Ok(Ok(_)) => debug!(device = "kernel", "Socket shutdown completed gracefully"),
                Ok(Err(e)) => warn!(device = "kernel", "Error during graceful shutdown: {}", e),
                Err(_) => warn!(
                    device = "kernel",
                    "Socket shutdown timed out after {:?}", shutdown_timeout
                ),
            }
        }

        self.status = DeviceStatus::Disconnected;
        self.buffer.clear();
        self.last_successful_operation = None;
        self.last_successful_connection = None;
        self.reconnect_attempts = 0;

        info!(device = "kernel", "Kernel Flow2 disconnected successfully");
        Ok(())
    }

    async fn send(&mut self, data: &[u8]) -> Result<(), DeviceError> {
        for attempt in 0..2u8 {
            // Ensure we have a connection (reconnect if socket is gone)
            if !self.is_connection_healthy() {
                if self.device_config.auto_reconnect {
                    warn!(
                        device = "kernel",
                        "Connection unhealthy (attempt {}), reconnecting before send", attempt
                    );
                    self.attempt_reconnect().await?;
                } else {
                    return Err(DeviceError::NotConnected);
                }
            }

            match self.try_send(data).await {
                Ok(latency) => {
                    // Record success
                    self.last_successful_operation = Some(Instant::now());

                    debug!(
                        device = "kernel",
                        "Kernel Flow2 data sent successfully: {} bytes with latency {:?}",
                        data.len(),
                        latency
                    );
                    return Ok(());
                }
                Err(ref e) if attempt == 0 && self.is_connection_lost_error(e) => {
                    warn!(
                        device = "kernel",
                        "Send failed on attempt 0, will reconnect and retry: {}", e
                    );
                    self.socket = None;
                    self.status = DeviceStatus::Connecting;
                    continue;
                }
                Err(e) => {
                    self.socket = None;
                    self.status = DeviceStatus::Error;
                    return Err(e);
                }
            }
        }
        unreachable!()
    }

    async fn receive(&mut self) -> Result<Vec<u8>, DeviceError> {
        // Check connection health before receiving
        if !self.is_connection_healthy() {
            warn!(
                device = "kernel",
                "Connection unhealthy, attempting reconnection before receive"
            );
            if self.device_config.auto_reconnect {
                self.attempt_reconnect().await?;
            } else {
                return Err(DeviceError::NotConnected);
            }
        }

        if let Some(ref mut socket) = self.socket {
            let io_timeout = Duration::from_millis(IO_TIMEOUT_MS);

            // Prepare buffer for reading
            self.buffer.clear();
            self.buffer.resize(self.config.buffer_size, 0);

            let read_result = timeout(io_timeout, socket.read(&mut self.buffer)).await;

            match read_result {
                Ok(Ok(0)) => {
                    // Connection closed by remote
                    warn!(
                        device = "kernel",
                        "Kernel Flow2 connection closed by remote"
                    );
                    self.status = DeviceStatus::Error;
                    self.socket = None;
                    Err(DeviceError::ConnectionFailed(
                        "Connection closed by remote".to_string(),
                    ))
                }
                Ok(Ok(n)) => {
                    self.buffer.truncate(n);
                    debug!(device = "kernel", "Kernel Flow2 received {} bytes", n);

                    // Use mem::take to avoid cloning - moves the data out and replaces with empty Vec
                    let data = std::mem::take(&mut self.buffer);

                    // Record success
                    if !data.is_empty() {
                        self.last_successful_operation = Some(Instant::now());
                    }

                    Ok(data)
                }
                Ok(Err(e)) if e.kind() == std::io::ErrorKind::TimedOut => {
                    // Timeout is acceptable, just return empty data
                    Ok(Vec::new())
                }
                Ok(Err(e)) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    // Non-blocking read, no data available
                    Ok(Vec::new())
                }
                Ok(Err(e)) => {
                    error!(device = "kernel", "Kernel Flow2 read error: {}", e);
                    if self.is_io_error_connection_lost(&e) {
                        self.socket = None;
                        self.status = DeviceStatus::Error;
                        Err(DeviceError::ConnectionFailed(format!(
                            "Connection lost during read: {}",
                            e
                        )))
                    } else {
                        Err(DeviceError::CommunicationError(format!(
                            "Read failed: {}",
                            e
                        )))
                    }
                }
                Err(_) => {
                    // Timeout elapsed - this is acceptable for receive, return empty data
                    Ok(Vec::new())
                }
            }
        } else {
            Err(DeviceError::NotConnected)
        }
    }

    fn get_info(&self) -> DeviceInfo {
        DeviceInfo {
            id: format!(
                "kernel_{}_{}",
                self.config.ip_address.replace('.', "_"),
                self.config.port
            ),
            name: format!(
                "Kernel Flow2 ({}:{})",
                self.config.ip_address, self.config.port
            ),
            device_type: DeviceType::Kernel,
            status: self.status,
            metadata: serde_json::json!({
                "ip_address": self.config.ip_address,
                "port": self.config.port,
                "connection_timeout_ms": self.config.connection_timeout_ms,
                "connection_health_check_interval_ms": self.config.connection_health_check_interval_ms,
                "max_reconnect_attempts": self.config.max_reconnect_attempts,
                "buffer_size": self.config.buffer_size,
                "reconnect_attempts": self.reconnect_attempts,
                "last_successful_operation": self.last_successful_operation.map(|t| t.elapsed().as_secs()),
                "last_successful_connection": self.last_successful_connection.map(|t| t.elapsed().as_secs()),
                "next_event_id": self.next_event_id,
            }),
        }
    }

    fn get_status(&self) -> DeviceStatus {
        self.status
    }

    fn configure(&mut self, config: DeviceConfig) -> Result<(), DeviceError> {
        self.device_config = config;

        if let Some(custom) = self.device_config.custom_settings.as_object() {
            if let Some(ip) = custom.get("ip_address").and_then(|v| v.as_str()) {
                self.config.ip_address = ip.to_string();
            }

            if let Some(port) = custom.get("port").and_then(|v| v.as_u64()) {
                self.config.port = port as u16;
            }

            if let Some(timeout) = custom.get("connection_timeout_ms").and_then(|v| v.as_u64()) {
                self.config.connection_timeout_ms = timeout;
            }

            if let Some(health_check_interval) = custom
                .get("connection_health_check_interval_ms")
                .and_then(|v| v.as_u64())
            {
                self.config.connection_health_check_interval_ms = health_check_interval;
            }

            if let Some(max_attempts) = custom
                .get("max_reconnect_attempts")
                .and_then(|v| v.as_u64())
            {
                self.config.max_reconnect_attempts = max_attempts as u32;
            }

            if let Some(buffer_size) = custom.get("buffer_size").and_then(|v| v.as_u64()) {
                self.config.buffer_size = buffer_size as usize;
                // Resize buffer to new size
                self.buffer.reserve(self.config.buffer_size);
            }
        }

        info!(
            device = "kernel",
            "Kernel Flow2 device configured: {}:{}", self.config.ip_address, self.config.port
        );
        Ok(())
    }

    async fn heartbeat(&mut self) -> Result<(), DeviceError> {
        // Check connection health (no data is sent to the device).
        // The Kernel Tasks SDK protocol does not define a heartbeat mechanism,
        // so we rely on tracking successful operations and TCP-level error detection.
        if self.socket.is_some() {
            // If auto-reconnect is enabled and connection is unhealthy, try to reconnect
            if !self.is_connection_healthy() && self.device_config.auto_reconnect {
                warn!(
                    device = "kernel",
                    "Connection health check failed, attempting automatic reconnection"
                );
                self.attempt_reconnect().await?;
            }

            Ok(())
        } else {
            Err(DeviceError::NotConnected)
        }
    }

    /// Test if the Kernel device can be reached without maintaining a connection
    async fn test_connection(&mut self) -> Result<bool, DeviceError> {
        info!(
            device = "kernel",
            "Testing connection to Kernel Flow2 at {}:{}", self.config.ip_address, self.config.port
        );

        match self.establish_connection().await {
            Ok(mut socket) => {
                // Successfully connected, now close the connection
                let _ = socket.shutdown().await;
                info!(device = "kernel", "Kernel Flow2 connection test successful");
                Ok(true)
            }
            Err(e) => {
                warn!(
                    device = "kernel",
                    "Kernel Flow2 connection test failed: {}", e
                );
                Ok(false)
            }
        }
    }

    /// Send a formatted event to the Kernel device using the Kernel Tasks SDK protocol.
    ///
    /// Extracts `timestamp`, `event`, and `value` from the incoming JSON and builds
    /// a proper [`KernelEvent`]. If the JSON is missing `id`, one is auto-assigned
    /// from [`next_event_id`] (with a warning log). If `id` is present, it is preserved.
    ///
    /// For the typed Rust API, prefer [`send_kernel_event()`] or [`send_event_simple()`].
    async fn send_event(
        &mut self,
        event: serde_json::Value,
    ) -> Result<serde_json::Value, DeviceError> {
        // Extract fields from the incoming JSON and build a proper KernelEvent.
        // The Kernel Tasks SDK requires {id, timestamp, event, value} — if the
        // caller omits `id` (e.g. the frontend), we auto-assign one from
        // next_event_id so events are not silently dropped by the Kernel device.

        let id = if let Some(id_val) = event.get("id") {
            id_val.as_i64().unwrap_or_else(|| {
                warn!(
                    device = "kernel",
                    "Event has non-integer 'id' field, auto-assigning id={}", self.next_event_id
                );
                let id = self.next_event_id;
                self.next_event_id += 1;
                id
            })
        } else {
            warn!(
                device = "kernel",
                "Event missing 'id' field, auto-assigning id={}", self.next_event_id
            );
            let id = self.next_event_id;
            self.next_event_id += 1;
            id
        };

        let timestamp = event
            .get("timestamp")
            .and_then(|v| v.as_i64())
            .unwrap_or_else(|| {
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_micros() as i64)
                    .unwrap_or(0)
            });

        let event_name = event
            .get("event")
            .and_then(|v| v.as_str())
            .unwrap_or("event_unknown")
            .to_string();

        let value = event
            .get("value")
            .cloned()
            .unwrap_or(serde_json::Value::String(String::new()));

        let kernel_event = KernelEvent::with_timestamp(id, timestamp, event_name.clone(), value);
        self.send_kernel_event(&kernel_event).await?;
        Ok(serde_json::json!({
            "success": true,
            "event_id": id,
            "event": event_name,
        }))
    }
}

impl KernelDevice {
    /// Send a typed KernelEvent to the device using the correct wire format.
    ///
    /// This is the preferred method for sending events as it ensures the
    /// correct schema is used.
    pub async fn send_kernel_event(&mut self, event: &KernelEvent) -> Result<(), DeviceError> {
        let wire_data = event.to_wire_format()?;

        debug!(
            device = "kernel",
            "Sending typed Kernel event (id={}, event='{}'): {} bytes total",
            event.id,
            event.event,
            wire_data.len()
        );

        self.send(&wire_data).await?;

        info!(
            device = "kernel",
            "Kernel event delivered: id={}, event='{}'", event.id, event.event
        );
        Ok(())
    }

    /// Create and send an event with auto-generated ID and current timestamp.
    ///
    /// This is a convenience method that handles ID generation and timestamping.
    pub async fn send_event_simple(
        &mut self,
        event_name: impl Into<String>,
        value: serde_json::Value,
    ) -> Result<i64, DeviceError> {
        let event_id = self.next_event_id;
        self.next_event_id += 1;

        let event = KernelEvent::new(event_id, event_name, value);
        self.send_kernel_event(&event).await?;

        Ok(event_id)
    }

    // =========================================================================
    // Experiment Lifecycle Methods
    // =========================================================================
    // Per Kernel Tasks SDK: First event MUST be start_experiment, last MUST be end_experiment
    // Reference: https://docs.kernel.com/docs/kernel-tasks-sdk

    /// Send the required `start_experiment` event.
    ///
    /// **This must be the first event sent** according to the Kernel Tasks SDK.
    /// Call this at the beginning of your experiment session.
    ///
    /// # Arguments
    /// * `experiment_name` - Optional name/identifier for the experiment
    ///
    /// # Returns
    /// The event ID assigned to this event
    pub async fn start_experiment(
        &mut self,
        experiment_name: Option<&str>,
    ) -> Result<i64, DeviceError> {
        let value = match experiment_name {
            Some(name) => serde_json::json!({ "name": name }),
            None => serde_json::json!(null),
        };
        self.send_event_simple("start_experiment", value).await
    }

    /// Send the required `end_experiment` event.
    ///
    /// **This must be the last event sent** according to the Kernel Tasks SDK.
    /// Call this at the end of your experiment session.
    ///
    /// # Returns
    /// The event ID assigned to this event
    pub async fn end_experiment(&mut self) -> Result<i64, DeviceError> {
        self.send_event_simple("end_experiment", serde_json::json!(null))
            .await
    }

    // =========================================================================
    // Event Hierarchy Helpers
    // =========================================================================
    // Hierarchy: experiment > task > block > trial
    // Task, block, and trial are optional but must respect the hierarchy.

    /// Send a `start_task` event to begin a task block.
    ///
    /// Tasks are the first level of optional hierarchy below experiment.
    /// Use tasks to group related blocks together.
    ///
    /// # Arguments
    /// * `task_name` - Identifier for this task
    /// * `metadata` - Optional additional metadata (becomes columns in SNIRF export)
    pub async fn start_task(
        &mut self,
        task_name: &str,
        metadata: Option<serde_json::Value>,
    ) -> Result<i64, DeviceError> {
        let value = match metadata {
            Some(mut meta) => {
                if let Some(obj) = meta.as_object_mut() {
                    obj.insert("task_name".to_string(), serde_json::json!(task_name));
                }
                meta
            }
            None => serde_json::json!({ "task_name": task_name }),
        };
        self.send_event_simple("start_task", value).await
    }

    /// Send an `end_task` event to end the current task.
    pub async fn end_task(&mut self, task_name: &str) -> Result<i64, DeviceError> {
        self.send_event_simple("end_task", serde_json::json!({ "task_name": task_name }))
            .await
    }

    /// Send a `start_block` event to begin a block within a task.
    ///
    /// Blocks are the second level of optional hierarchy (within tasks).
    /// Use blocks to group related trials together.
    ///
    /// # Arguments
    /// * `block_number` - Block identifier/number
    /// * `metadata` - Optional additional metadata
    pub async fn start_block(
        &mut self,
        block_number: i32,
        metadata: Option<serde_json::Value>,
    ) -> Result<i64, DeviceError> {
        let value = match metadata {
            Some(mut meta) => {
                if let Some(obj) = meta.as_object_mut() {
                    obj.insert("block_number".to_string(), serde_json::json!(block_number));
                }
                meta
            }
            None => serde_json::json!({ "block_number": block_number }),
        };
        self.send_event_simple("start_block", value).await
    }

    /// Send an `end_block` event to end the current block.
    pub async fn end_block(&mut self, block_number: i32) -> Result<i64, DeviceError> {
        self.send_event_simple(
            "end_block",
            serde_json::json!({ "block_number": block_number }),
        )
        .await
    }

    /// Send a `start_trial` event to begin a trial within a block.
    ///
    /// Trials are the third level of optional hierarchy (within blocks).
    /// Use trials for individual stimulus presentations or response periods.
    ///
    /// # Arguments
    /// * `trial_number` - Trial identifier/number
    /// * `metadata` - Optional additional metadata (e.g., condition, stimulus type)
    pub async fn start_trial(
        &mut self,
        trial_number: i32,
        metadata: Option<serde_json::Value>,
    ) -> Result<i64, DeviceError> {
        let value = match metadata {
            Some(mut meta) => {
                if let Some(obj) = meta.as_object_mut() {
                    obj.insert("trial_number".to_string(), serde_json::json!(trial_number));
                }
                meta
            }
            None => serde_json::json!({ "trial_number": trial_number }),
        };
        self.send_event_simple("start_trial", value).await
    }

    /// Send an `end_trial` event to end the current trial.
    pub async fn end_trial(&mut self, trial_number: i32) -> Result<i64, DeviceError> {
        self.send_event_simple(
            "end_trial",
            serde_json::json!({ "trial_number": trial_number }),
        )
        .await
    }

    // =========================================================================
    // Common Event Helpers
    // =========================================================================

    /// Send a stimulus onset event.
    ///
    /// Use this when presenting a stimulus to the participant.
    /// Send as close in time as possible to the actual screen flip or stimulus delivery.
    ///
    /// # Arguments
    /// * `stimulus_type` - Type of stimulus (e.g., "visual", "auditory", "image_name.png")
    /// * `metadata` - Optional additional metadata about the stimulus
    pub async fn stimulus_onset(
        &mut self,
        stimulus_type: &str,
        metadata: Option<serde_json::Value>,
    ) -> Result<i64, DeviceError> {
        let value = match metadata {
            Some(mut meta) => {
                if let Some(obj) = meta.as_object_mut() {
                    obj.insert(
                        "stimulus_type".to_string(),
                        serde_json::json!(stimulus_type),
                    );
                }
                meta
            }
            None => serde_json::json!({ "stimulus_type": stimulus_type }),
        };
        self.send_event_simple("event_stimulus_onset", value).await
    }

    /// Send a response event.
    ///
    /// Use this when the participant makes a response.
    ///
    /// # Arguments
    /// * `response` - The response made (e.g., "left", "right", "correct", key pressed)
    /// * `response_time_ms` - Optional response time in milliseconds
    /// * `correct` - Optional correctness flag
    pub async fn response(
        &mut self,
        response: &str,
        response_time_ms: Option<f64>,
        correct: Option<bool>,
    ) -> Result<i64, DeviceError> {
        let mut value = serde_json::json!({ "response": response });
        if let Some(rt) = response_time_ms {
            value["response_time_ms"] = serde_json::json!(rt);
        }
        if let Some(c) = correct {
            value["correct"] = serde_json::json!(c);
        }
        self.send_event_simple("event_response", value).await
    }

    /// Send a generic marker event.
    ///
    /// Use for custom events that don't fit the standard categories.
    /// Events prefixed with `event_` will have their timestamps included in SNIRF exports.
    ///
    /// # Arguments
    /// * `marker_name` - Name of the marker (will be prefixed with "event_")
    /// * `value` - Associated value or metadata
    pub async fn marker(
        &mut self,
        marker_name: &str,
        value: serde_json::Value,
    ) -> Result<i64, DeviceError> {
        self.send_event_simple(format!("event_{}", marker_name), value)
            .await
    }
}

#[cfg(test)]
#[path = "kernel_tests.rs"]
mod kernel_tests;

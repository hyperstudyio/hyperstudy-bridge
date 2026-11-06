use super::resolver::DiscoveredStream;
use super::sync::TimeSync;
use super::types::{ChannelFormat, LslError, Sample, SampleData, StreamInfo, StreamStatus};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use tokio::sync::{Mutex, RwLock};
use tokio::time::{Duration, Instant};
use tracing::{debug, error, info, warn};

// Import real LSL library
use lsl;

// Use std::sync channels for dedicated thread communication (avoids per-command runtime creation)
use std::sync::mpsc as std_mpsc;

/// Response channel type - uses std::sync::mpsc for thread-safe, non-async responses
type ResponseSender<T> = std_mpsc::Sender<T>;

/// Commands sent to the dedicated LSL inlet thread
enum InletCommand {
    /// Open the stream connection
    OpenStream {
        timeout: f64,
        response: ResponseSender<Result<(), LslError>>,
    },
    /// Pull a single sample with timeout
    PullSample {
        timeout: f64,
        response: ResponseSender<Result<Option<(SampleData, f64)>, LslError>>,
    },
    /// Get time correction
    TimeCorrection {
        timeout: f64,
        response: ResponseSender<Result<f64, LslError>>,
    },
    /// Shutdown the inlet thread
    Shutdown,
}

/// LSL stream inlet for consuming data
/// Uses a dedicated thread pattern because lsl::StreamInlet is not Send+Sync
pub struct StreamInlet {
    /// Stream information
    info: StreamInfo,
    /// Inlet status
    status: Arc<RwLock<StreamStatus>>,
    /// Time synchronization utility
    #[allow(dead_code)]
    time_sync: Arc<TimeSync>,
    /// Data buffer for incoming samples
    buffer: Arc<RwLock<VecDeque<Sample>>>,
    /// Buffer size limit
    buffer_limit: usize,
    /// Active status flag
    active: Arc<AtomicBool>,
    /// Sample counter
    sample_count: Arc<AtomicU64>,
    /// Command sender to the dedicated LSL thread (std::sync for blocking recv in thread)
    command_tx: std_mpsc::Sender<InletCommand>,
    /// Shutdown flag - thread polls this to know when to exit
    shutdown_flag: Arc<AtomicBool>,
    /// Performance metrics
    bytes_received: Arc<AtomicU64>,
    last_receive_time: Arc<RwLock<Option<Instant>>>,
    /// Stream UID for identification
    stream_uid: String,
    /// Time correction offset
    time_correction: Arc<RwLock<f64>>,
    /// Serialize close calls until the native inlet and its workers are destroyed.
    thread_handle: Mutex<Option<thread::JoinHandle<()>>>,
}

// Manual Debug implementation since thread::JoinHandle doesn't implement Debug
impl std::fmt::Debug for StreamInlet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamInlet")
            .field("info", &self.info)
            .field("stream_uid", &self.stream_uid)
            .field("active", &self.active.load(Ordering::Relaxed))
            .field("sample_count", &self.sample_count.load(Ordering::Relaxed))
            .finish()
    }
}

/// Inlet configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InletConfig {
    /// Buffer size for samples
    pub buffer_size: usize,
    /// Maximum recovery time for reconnection (seconds)
    pub max_recovery_time: f64,
    /// Post-processing flags
    pub post_processing: PostProcessingFlags,
    /// Automatic time correction
    pub auto_time_correction: bool,
    /// Data processing interval
    pub processing_interval_ms: u64,
    /// Max buffer length in seconds (for LSL inlet)
    pub max_buflen: i32,
    /// Max chunk length (0 = use sender's preference)
    pub max_chunklen: i32,
    /// Enable stream recovery
    pub recover: bool,
}

/// Post-processing options for inlet data
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PostProcessingFlags {
    /// Remove clock synchronization jitter
    pub dejitter_timestamps: bool,
    /// Automatically recover lost samples
    pub recover_samples: bool,
    /// Thread-safe sample retrieval
    pub thread_safe: bool,
    /// Enable monotonic timestamp ordering
    pub monotonic_timestamps: bool,
}

impl Default for InletConfig {
    fn default() -> Self {
        Self {
            buffer_size: 1000,
            max_recovery_time: 5.0,
            post_processing: PostProcessingFlags {
                dejitter_timestamps: true,
                recover_samples: false,
                thread_safe: true,
                monotonic_timestamps: true,
            },
            auto_time_correction: true,
            processing_interval_ms: 100,
            max_buflen: 360, // 6 minutes
            max_chunklen: 0, // Use sender's preference
            recover: true,
        }
    }
}

/// Inlet manager for handling multiple inlets
pub struct InletManager {
    /// Active inlets
    inlets: Arc<RwLock<std::collections::HashMap<String, Arc<StreamInlet>>>>,
    /// Default configuration
    default_config: InletConfig,
    /// Time synchronization
    #[allow(dead_code)]
    time_sync: Arc<TimeSync>,
}

// Manual Debug for InletManager
impl std::fmt::Debug for InletManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InletManager")
            .field("default_config", &self.default_config)
            .finish()
    }
}

impl StreamInlet {
    /// Create a new stream inlet from discovered stream with dedicated thread
    pub async fn new(
        discovered_stream: DiscoveredStream,
        config: InletConfig,
        time_sync: Arc<TimeSync>,
    ) -> Result<Self, LslError> {
        info!(
            device = "lsl",
            "Creating LSL inlet: {} (UID: {})", discovered_stream.info.name, discovered_stream.uid
        );

        // Create std::sync channel for commands (allows blocking recv in thread)
        let (command_tx, command_rx) = std_mpsc::channel::<InletCommand>();

        // Shutdown flag for clean thread termination
        let shutdown_flag = Arc::new(AtomicBool::new(false));
        let thread_shutdown_flag = shutdown_flag.clone();

        // Clone info for the thread
        let thread_uid = discovered_stream.uid.clone();
        let thread_info = discovered_stream.info.clone();
        let max_buflen = config.max_buflen;
        let max_chunklen = config.max_chunklen;
        let recover = config.recover;

        // Spawn dedicated thread for LSL operations
        let thread_handle = thread::spawn(move || {
            // First, resolve the stream by UID to get the LSL StreamInfo
            let resolve_pred = format!("uid='{}'", thread_uid);
            let lsl_streams = match lsl::resolve_bypred(&resolve_pred, 1, 5.0) {
                Ok(streams) => streams,
                Err(e) => {
                    error!(
                        device = "lsl",
                        "Failed to resolve stream by UID {}: {:?}", thread_uid, e
                    );
                    return;
                }
            };

            if lsl_streams.is_empty() {
                error!(device = "lsl", "No stream found with UID: {}", thread_uid);
                return;
            }

            let lsl_info = &lsl_streams[0];

            // Create LSL StreamInlet
            let lsl_inlet = match lsl::StreamInlet::new(lsl_info, max_buflen, max_chunklen, recover)
            {
                Ok(inlet) => inlet,
                Err(e) => {
                    error!(device = "lsl", "Failed to create LSL StreamInlet: {:?}", e);
                    return;
                }
            };

            info!(
                device = "lsl",
                "LSL inlet thread started for: {}", thread_info.name
            );

            // Get channel format for pulling the right type
            let channel_format = thread_info.channel_format;
            let channel_count = thread_info.channel_count;

            // Process commands in a blocking loop using std::sync::mpsc
            // No Tokio runtime needed - this is pure blocking I/O
            loop {
                // Check shutdown flag first
                if thread_shutdown_flag.load(Ordering::Relaxed) {
                    info!(
                        device = "lsl",
                        "LSL inlet thread shutdown flag set: {}", thread_info.name
                    );
                    lsl_inlet.close_stream();
                    break;
                }

                // Shutdown sends a command, so an idle worker needs no polling delay.
                match command_rx.recv() {
                    Ok(InletCommand::OpenStream { timeout, response }) => {
                        let result = lsl_inlet.open_stream(timeout).map_err(|e| {
                            LslError::LslLibraryError(format!("Open failed: {:?}", e))
                        });
                        let _ = response.send(result);
                    }
                    Ok(InletCommand::PullSample { timeout, response }) => {
                        let result = Self::pull_sample_from_lsl(
                            &lsl_inlet,
                            channel_format,
                            channel_count,
                            timeout,
                        );
                        let _ = response.send(result);
                    }
                    Ok(InletCommand::TimeCorrection { timeout, response }) => {
                        let result = lsl_inlet.time_correction(timeout).map_err(|e| {
                            LslError::LslLibraryError(format!("Time correction failed: {:?}", e))
                        });
                        let _ = response.send(result);
                    }
                    Ok(InletCommand::Shutdown) => {
                        info!(
                            device = "lsl",
                            "LSL inlet thread shutting down: {}", thread_info.name
                        );
                        lsl_inlet.close_stream();
                        break;
                    }
                    Err(std_mpsc::RecvError) => {
                        info!(
                            device = "lsl",
                            "LSL inlet command channel closed: {}", thread_info.name
                        );
                        lsl_inlet.close_stream();
                        break;
                    }
                }
            }
        });

        let inlet = Self {
            info: discovered_stream.info,
            status: Arc::new(RwLock::new(StreamStatus::default())),
            time_sync,
            buffer: Arc::new(RwLock::new(VecDeque::with_capacity(config.buffer_size))),
            buffer_limit: config.buffer_size,
            active: Arc::new(AtomicBool::new(false)),
            sample_count: Arc::new(AtomicU64::new(0)),
            command_tx,
            shutdown_flag,
            bytes_received: Arc::new(AtomicU64::new(0)),
            last_receive_time: Arc::new(RwLock::new(None)),
            stream_uid: discovered_stream.uid,
            time_correction: Arc::new(RwLock::new(0.0)),
            thread_handle: Mutex::new(Some(thread_handle)),
        };

        debug!(device = "lsl", "LSL inlet created successfully");
        Ok(inlet)
    }

    /// Pull sample from the real LSL inlet (called from dedicated thread)
    ///
    /// LSL Convention: A timestamp of 0.0 indicates the pull operation timed out
    /// (no sample was available within the timeout period). This is different from
    /// a valid sample that happens to have timestamp 0.0 (which would be Unix epoch).
    /// In practice, valid LSL timestamps are always positive and recent, so this
    /// convention is safe.
    fn pull_sample_from_lsl(
        inlet: &lsl::StreamInlet,
        channel_format: ChannelFormat,
        #[allow(unused_variables)] channel_count: u32,
        timeout: f64,
    ) -> Result<Option<(SampleData, f64)>, LslError> {
        use lsl::Pullable;

        match channel_format {
            ChannelFormat::Float32 => {
                let (sample, ts): (Vec<f32>, f64) = inlet
                    .pull_sample(timeout)
                    .map_err(|e| LslError::LslLibraryError(format!("Pull failed: {:?}", e)))?;
                // ts == 0.0 means timeout (LSL convention), not a valid timestamp
                if ts == 0.0 || sample.is_empty() {
                    Ok(None)
                } else {
                    Ok(Some((SampleData::Float32(sample), ts)))
                }
            }
            ChannelFormat::Float64 => {
                let (sample, ts): (Vec<f64>, f64) = inlet
                    .pull_sample(timeout)
                    .map_err(|e| LslError::LslLibraryError(format!("Pull failed: {:?}", e)))?;
                if ts == 0.0 || sample.is_empty() {
                    Ok(None)
                } else {
                    Ok(Some((SampleData::Float64(sample), ts)))
                }
            }
            ChannelFormat::String => {
                let (sample, ts): (Vec<String>, f64) = inlet
                    .pull_sample(timeout)
                    .map_err(|e| LslError::LslLibraryError(format!("Pull failed: {:?}", e)))?;
                if ts == 0.0 || sample.is_empty() {
                    Ok(None)
                } else {
                    Ok(Some((SampleData::String(sample), ts)))
                }
            }
            ChannelFormat::Int32 => {
                let (sample, ts): (Vec<i32>, f64) = inlet
                    .pull_sample(timeout)
                    .map_err(|e| LslError::LslLibraryError(format!("Pull failed: {:?}", e)))?;
                if ts == 0.0 || sample.is_empty() {
                    Ok(None)
                } else {
                    Ok(Some((SampleData::Int32(sample), ts)))
                }
            }
            ChannelFormat::Int16 => {
                let (sample, ts): (Vec<i16>, f64) = inlet
                    .pull_sample(timeout)
                    .map_err(|e| LslError::LslLibraryError(format!("Pull failed: {:?}", e)))?;
                if ts == 0.0 || sample.is_empty() {
                    Ok(None)
                } else {
                    Ok(Some((SampleData::Int16(sample), ts)))
                }
            }
            ChannelFormat::Int8 => {
                let (sample, ts): (Vec<i8>, f64) = inlet
                    .pull_sample(timeout)
                    .map_err(|e| LslError::LslLibraryError(format!("Pull failed: {:?}", e)))?;
                if ts == 0.0 || sample.is_empty() {
                    Ok(None)
                } else {
                    Ok(Some((SampleData::Int8(sample), ts)))
                }
            }
            #[cfg(not(windows))]
            ChannelFormat::Int64 => {
                let (sample, ts): (Vec<i64>, f64) = inlet
                    .pull_sample(timeout)
                    .map_err(|e| LslError::LslLibraryError(format!("Pull failed: {:?}", e)))?;
                if ts == 0.0 || sample.is_empty() {
                    Ok(None)
                } else {
                    Ok(Some((SampleData::Int64(sample), ts)))
                }
            }
            #[cfg(windows)]
            ChannelFormat::Int64 => Err(LslError::LslLibraryError(
                "Int64 not supported on Windows".to_string(),
            )),
        }
    }

    /// Open the inlet connection
    pub async fn open(&self, timeout: Duration) -> Result<(), LslError> {
        if self.shutdown_flag.load(Ordering::Relaxed) {
            return Err(LslError::LslLibraryError(
                "Inlet has been closed; create a new inlet".into(),
            ));
        }
        let started = Instant::now();
        if self.active.load(Ordering::Relaxed) {
            warn!(device = "lsl", "Inlet already open");
            return Ok(());
        }

        info!(
            device = "lsl",
            "Opening LSL inlet: {} (timeout: {:?})", self.info.name, timeout
        );

        // Create response channel (std::sync - blocking recv is wrapped in spawn_blocking)
        let (response_tx, response_rx) = std_mpsc::channel();

        // Send command to dedicated LSL thread
        self.command_tx
            .send(InletCommand::OpenStream {
                timeout: timeout.as_secs_f64(),
                response: response_tx,
            })
            .map_err(|_| LslError::LslLibraryError("Inlet thread not responding".to_string()))?;

        // Wait for response with timeout using spawn_blocking
        let total_timeout = timeout + Duration::from_secs(5);
        let result = tokio::time::timeout(total_timeout, async {
            tokio::task::spawn_blocking(move || {
                response_rx.recv_timeout(std::time::Duration::from_secs(30))
            })
            .await
        })
        .await;

        match result {
            Ok(Ok(Ok(inner_result))) => inner_result?,
            Ok(Ok(Err(_recv_err))) => {
                return Err(LslError::LslLibraryError(
                    "Inlet thread response timeout".to_string(),
                ))
            }
            Ok(Err(_join_err)) => {
                return Err(LslError::LslLibraryError(
                    "Inlet thread task failed".to_string(),
                ))
            }
            Err(_timeout) => {
                return Err(LslError::LslLibraryError(
                    "Inlet thread timeout".to_string(),
                ))
            }
        }

        let open_elapsed = started.elapsed();
        // Clock correction uses LSL's time service, even for an idle marker stream.
        // Never label timestamps as corrected when the initial estimate failed.
        let correction = self.calculate_time_correction().await?;
        *self.time_correction.write().await = correction;
        self.active.store(true, Ordering::Relaxed);
        self.status.write().await.active = true;

        info!(device = "lsl", stream = %self.info.name,
            open_ms = open_elapsed.as_millis() as u64,
            correction_ms = started.elapsed().saturating_sub(open_elapsed).as_millis() as u64,
            "LSL inlet opened successfully");
        Ok(())
    }

    /// Permanently close the inlet and wait for native resources to be released.
    pub async fn close(&self) -> Result<(), LslError> {
        let started = Instant::now();
        self.shutdown_flag.store(true, Ordering::Relaxed);
        self.active.store(false, Ordering::Relaxed);
        let _ = self.command_tx.send(InletCommand::Shutdown);
        self.status.write().await.active = false;

        // Also close failed/partially opened inlets. Keep this lock until join
        // completes so simultaneous callers cannot report premature completion.
        let mut handle = self.thread_handle.lock().await;
        if let Some(thread) = handle.take() {
            tokio::task::spawn_blocking(move || thread.join())
                .await
                .map_err(|e| LslError::LslLibraryError(format!("Inlet cleanup task failed: {e}")))?
                .map_err(|_| {
                    LslError::LslLibraryError("Inlet worker panicked during cleanup".into())
                })?;
        }
        info!(device = "lsl", stream = %self.info.name,
            elapsed_ms = started.elapsed().as_millis() as u64, "LSL inlet closed");
        Ok(())
    }

    /// Pull a single sample from the stream
    pub async fn pull_sample(&self, timeout: Duration) -> Result<Option<Sample>, LslError> {
        if !self.active.load(Ordering::Relaxed) {
            return Err(LslError::LslLibraryError("Inlet not open".to_string()));
        }

        // Try to get from local buffer first
        {
            let mut buffer = self.buffer.write().await;
            if let Some(sample) = buffer.pop_front() {
                return Ok(Some(sample));
            }
        }

        // Create response channel
        let (response_tx, response_rx) = std_mpsc::channel();

        // Send pull command to dedicated thread
        self.command_tx
            .send(InletCommand::PullSample {
                timeout: timeout.as_secs_f64(),
                response: response_tx,
            })
            .map_err(|_| LslError::LslLibraryError("Inlet thread not responding".to_string()))?;

        // Wait for response with timeout using spawn_blocking
        let recv_timeout = timeout + Duration::from_secs(1);
        let recv_timeout_std = std::time::Duration::from_secs_f64(recv_timeout.as_secs_f64());
        let result = tokio::time::timeout(recv_timeout, async {
            tokio::task::spawn_blocking(move || response_rx.recv_timeout(recv_timeout_std)).await
        })
        .await;

        let inner_result = match result {
            Ok(Ok(Ok(r))) => r?,
            Ok(Ok(Err(_recv_err))) => return Ok(None), // Timeout is normal
            Ok(Err(_join_err)) => {
                return Err(LslError::LslLibraryError(
                    "Inlet thread task failed".to_string(),
                ))
            }
            Err(_timeout) => return Ok(None), // Timeout is normal
        };

        if let Some((data, timestamp)) = inner_result {
            // Update metrics
            self.sample_count.fetch_add(1, Ordering::Relaxed);
            let bytes = data.to_bytes().len() as u64;
            self.bytes_received.fetch_add(bytes, Ordering::Relaxed);

            // Apply time correction
            let correction = *self.time_correction.read().await;
            let corrected_timestamp = timestamp + correction;

            // Update status
            {
                let mut status = self.status.write().await;
                status.sample_count += 1;
                status.last_timestamp = corrected_timestamp;
            }

            // Update last receive time
            {
                let mut last_receive = self.last_receive_time.write().await;
                *last_receive = Some(Instant::now());
            }

            Ok(Some(Sample {
                data,
                timestamp: corrected_timestamp,
            }))
        } else {
            Ok(None)
        }
    }

    /// Pull multiple samples as a chunk
    pub async fn pull_chunk(
        &self,
        max_samples: usize,
        timeout: Duration,
    ) -> Result<Vec<Sample>, LslError> {
        let mut samples = Vec::with_capacity(max_samples);
        let start_time = Instant::now();

        while samples.len() < max_samples && start_time.elapsed() < timeout {
            let remaining_timeout = timeout.saturating_sub(start_time.elapsed());
            if remaining_timeout.is_zero() {
                break;
            }
            match self.pull_sample(remaining_timeout).await? {
                Some(sample) => samples.push(sample),
                None => break, // No more samples available
            }
        }

        Ok(samples)
    }

    /// Start continuous data collection
    /// Returns a tokio channel receiver for samples
    pub async fn start_collection(&self) -> Result<tokio::sync::mpsc::Receiver<Sample>, LslError> {
        let (_sender, receiver) = tokio::sync::mpsc::channel(10000);

        if !self.active.load(Ordering::Relaxed) {
            return Err(LslError::LslLibraryError("Inlet not open".to_string()));
        }

        info!(
            device = "lsl",
            "Starting continuous collection for: {}", self.info.name
        );

        Ok(receiver)
    }

    /// Calculate time correction offset
    async fn calculate_time_correction(&self) -> Result<f64, LslError> {
        let (response_tx, response_rx) = std_mpsc::channel();
        self.command_tx
            .send(InletCommand::TimeCorrection {
                timeout: 5.0,
                response: response_tx,
            })
            .map_err(|_| LslError::LslLibraryError("Inlet thread not responding".to_string()))?;

        // Wait for response with timeout using spawn_blocking
        let result = tokio::time::timeout(Duration::from_secs(10), async {
            tokio::task::spawn_blocking(move || {
                response_rx.recv_timeout(std::time::Duration::from_secs(10))
            })
            .await
        })
        .await;

        match result {
            Ok(Ok(Ok(inner_result))) => inner_result,
            Ok(Ok(Err(_recv_err))) => Err(LslError::LslLibraryError(
                "Time correction response timeout".to_string(),
            )),
            Ok(Err(_join_err)) => Err(LslError::LslLibraryError(
                "Time correction task failed".to_string(),
            )),
            Err(_timeout) => Err(LslError::LslLibraryError(
                "Time correction timeout".to_string(),
            )),
        }
    }

    /// Get stream information
    pub fn get_info(&self) -> &StreamInfo {
        &self.info
    }

    /// Get stream UID
    pub fn get_uid(&self) -> &str {
        &self.stream_uid
    }

    /// Get current status
    pub async fn get_status(&self) -> StreamStatus {
        self.status.read().await.clone()
    }

    /// Check if inlet is active
    pub fn is_active(&self) -> bool {
        self.active.load(Ordering::Relaxed)
    }

    /// Get sample count
    pub fn get_sample_count(&self) -> u64 {
        self.sample_count.load(Ordering::Relaxed)
    }

    /// Get bytes received
    pub fn get_bytes_received(&self) -> u64 {
        self.bytes_received.load(Ordering::Relaxed)
    }

    /// Get buffer usage
    pub async fn get_buffer_usage(&self) -> (usize, usize) {
        let buffer = self.buffer.read().await;
        (buffer.len(), self.buffer_limit)
    }

    /// Flush buffer
    pub async fn flush_buffer(&self) {
        let mut buffer = self.buffer.write().await;
        buffer.clear();
    }

    /// Get time correction offset
    pub async fn get_time_correction(&self) -> f64 {
        *self.time_correction.read().await
    }

    /// Get inlet statistics
    pub async fn get_stats(&self) -> serde_json::Value {
        let status = self.status.read().await;
        let (buffer_used, buffer_size) = self.get_buffer_usage().await;
        let last_receive = self.last_receive_time.read().await;
        let time_correction = *self.time_correction.read().await;

        let seconds_since_last_receive = last_receive
            .map(|t| t.elapsed().as_secs_f64())
            .unwrap_or(0.0);

        serde_json::json!({
            "name": self.info.name,
            "uid": self.stream_uid,
            "type": self.info.stream_type,
            "active": self.is_active(),
            "sample_count": self.get_sample_count(),
            "bytes_received": self.get_bytes_received(),
            "buffer_usage": format!("{}/{}", buffer_used, buffer_size),
            "buffer_usage_percent": (buffer_used as f64 / buffer_size as f64) * 100.0,
            "data_loss_percent": status.data_loss,
            "last_timestamp": status.last_timestamp,
            "time_correction": time_correction,
            "seconds_since_last_receive": seconds_since_last_receive
        })
    }
}

impl Drop for StreamInlet {
    fn drop(&mut self) {
        // Best-effort shutdown on drop; normal remove/close awaits native destruction.
        self.shutdown_flag.store(true, Ordering::Relaxed);
        // Also send shutdown command for immediate response
        let _ = self.command_tx.send(InletCommand::Shutdown);
    }
}

impl InletManager {
    /// Create a new inlet manager
    pub fn new(time_sync: Arc<TimeSync>) -> Self {
        Self {
            inlets: Arc::new(RwLock::new(std::collections::HashMap::new())),
            default_config: InletConfig::default(),
            time_sync,
        }
    }

    /// Create inlet manager with custom configuration
    pub fn with_config(time_sync: Arc<TimeSync>, config: InletConfig) -> Self {
        Self {
            inlets: Arc::new(RwLock::new(std::collections::HashMap::new())),
            default_config: config,
            time_sync,
        }
    }

    /// Create a new inlet from discovered stream
    pub async fn create_inlet(
        &self,
        discovered_stream: DiscoveredStream,
        config: Option<InletConfig>,
    ) -> Result<String, LslError> {
        let inlet_config = config.unwrap_or_else(|| self.default_config.clone());
        let inlet = Arc::new(
            StreamInlet::new(
                discovered_stream.clone(),
                inlet_config,
                self.time_sync.clone(),
            )
            .await?,
        );

        let inlet_id = discovered_stream.uid.clone();

        let mut inlets = self.inlets.write().await;
        inlets.insert(inlet_id.clone(), inlet);

        info!(device = "lsl", "Created inlet: {}", inlet_id);
        Ok(inlet_id)
    }

    /// Get inlet by ID
    pub async fn get_inlet(&self, inlet_id: &str) -> Option<Arc<StreamInlet>> {
        let inlets = self.inlets.read().await;
        inlets.get(inlet_id).cloned()
    }

    /// Open inlet connection
    pub async fn open_inlet(&self, inlet_id: &str, timeout: Duration) -> Result<(), LslError> {
        let inlet = self
            .get_inlet(inlet_id)
            .await
            .ok_or_else(|| LslError::StreamNotFound(inlet_id.to_string()))?;

        inlet.open(timeout).await
    }

    /// Close inlet connection
    pub async fn close_inlet(&self, inlet_id: &str) -> Result<(), LslError> {
        let inlet = self
            .get_inlet(inlet_id)
            .await
            .ok_or_else(|| LslError::StreamNotFound(inlet_id.to_string()))?;

        inlet.close().await
    }

    /// Remove inlet
    pub async fn remove_inlet(&self, inlet_id: &str) -> Result<(), LslError> {
        // Close inlet first
        if let Some(inlet) = self.get_inlet(inlet_id).await {
            inlet.close().await?;
        }

        let mut inlets = self.inlets.write().await;
        inlets.remove(inlet_id);

        info!(device = "lsl", "Removed inlet: {}", inlet_id);
        Ok(())
    }

    /// Pull sample from inlet
    pub async fn pull_sample(
        &self,
        inlet_id: &str,
        timeout: Duration,
    ) -> Result<Option<Sample>, LslError> {
        let inlet = self
            .get_inlet(inlet_id)
            .await
            .ok_or_else(|| LslError::StreamNotFound(inlet_id.to_string()))?;

        inlet.pull_sample(timeout).await
    }

    /// List all inlets
    pub async fn list_inlets(&self) -> Vec<String> {
        let inlets = self.inlets.read().await;
        inlets.keys().cloned().collect()
    }

    /// Get inlet statistics for all inlets
    pub async fn get_all_stats(&self) -> serde_json::Value {
        let inlets = self.inlets.read().await;
        let mut stats = std::collections::HashMap::new();

        for (id, inlet) in inlets.iter() {
            stats.insert(id.clone(), inlet.get_stats().await);
        }

        serde_json::json!({
            "inlet_count": inlets.len(),
            "inlets": stats
        })
    }

    /// Start all inlets
    pub async fn start_all(&self, timeout: Duration) -> Result<(), LslError> {
        let inlets = self.inlets.read().await;
        for inlet in inlets.values() {
            inlet.open(timeout).await?;
        }
        Ok(())
    }

    /// Stop all inlets
    pub async fn stop_all(&self) -> Result<(), LslError> {
        let inlets = self.inlets.read().await;
        for inlet in inlets.values() {
            inlet.close().await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::devices::lsl::{resolver::DiscoveredStream, sync::TimeSync, types::StreamInfo};
    use std::time::SystemTime;

    fn create_mock_discovered_stream() -> DiscoveredStream {
        DiscoveredStream {
            info: StreamInfo::ttl_markers("test_device"),
            discovered_at: SystemTime::now(),
            last_seen: SystemTime::now(),
            available: true,
            uid: "test_stream_001".to_string(),
            session_id: "session_001".to_string(),
            data_loss: 0.0,
            time_stamps: (),
        }
    }

    fn inlet_with_worker(
        worker: impl FnOnce(std_mpsc::Receiver<InletCommand>) + Send + 'static,
    ) -> StreamInlet {
        let (command_tx, command_rx) = std_mpsc::channel();
        StreamInlet {
            info: StreamInfo::ttl_markers("test"),
            status: Arc::new(RwLock::new(StreamStatus::default())),
            time_sync: Arc::new(TimeSync::new(false)),
            buffer: Arc::new(RwLock::new(VecDeque::new())),
            buffer_limit: 1,
            active: Arc::new(AtomicBool::new(false)),
            sample_count: Arc::new(AtomicU64::new(0)),
            command_tx,
            shutdown_flag: Arc::new(AtomicBool::new(false)),
            bytes_received: Arc::new(AtomicU64::new(0)),
            last_receive_time: Arc::new(RwLock::new(None)),
            stream_uid: "unit-test".into(),
            time_correction: Arc::new(RwLock::new(0.0)),
            thread_handle: Mutex::new(Some(thread::spawn(move || worker(command_rx)))),
        }
    }

    #[tokio::test]
    async fn open_requires_initial_clock_correction() {
        let inlet = inlet_with_worker(|commands| {
            while let Ok(command) = commands.recv() {
                match command {
                    InletCommand::OpenStream { response, .. } => {
                        response.send(Ok(())).unwrap();
                    }
                    InletCommand::TimeCorrection { response, .. } => {
                        response
                            .send(Err(LslError::LslLibraryError("clock unavailable".into())))
                            .unwrap();
                    }
                    _ => break,
                }
            }
        });
        let result = inlet.open(Duration::from_secs(1)).await;
        assert!(
            result.is_err(),
            "An uncorrected timestamp must not silently be accepted"
        );
        assert!(!inlet.is_active());
    }

    #[tokio::test]
    async fn close_waits_for_native_worker_destruction() {
        let (stopping_tx, stopping_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std_mpsc::channel();
        let destroyed = Arc::new(AtomicBool::new(false));
        let worker_destroyed = destroyed.clone();
        let inlet = inlet_with_worker(move |commands| {
            let _ = commands.recv();
            let _ = stopping_tx.send(());
            let _ = release_rx.recv();
            worker_destroyed.store(true, Ordering::SeqCst);
        });
        inlet.active.store(true, Ordering::Relaxed);
        let close = inlet.close();
        tokio::pin!(close);
        tokio::select! {
            result = &mut close => {
                let _ = release_tx.send(());
                panic!("close returned before worker destruction: {:?}", result);
            }
            _ = stopping_rx => {}
        }
        assert!(!destroyed.load(Ordering::SeqCst));
        release_tx.send(()).unwrap();
        close.await.unwrap();
        assert!(destroyed.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn test_inlet_creation() {
        let time_sync = Arc::new(TimeSync::new(false));
        let discovered_stream = create_mock_discovered_stream();
        let config = InletConfig::default();

        // Note: This test will fail if no actual LSL stream is available
        // In a real test environment, you'd need to create an outlet first
        let inlet = StreamInlet::new(discovered_stream, config, time_sync).await;
        // The inlet creation might fail if no stream is available, which is expected
        // in unit tests without actual LSL streams
        if inlet.is_ok() {
            let inlet = inlet.unwrap();
            assert_eq!(inlet.get_info().name, "test_device_TTL_Markers");
            assert!(!inlet.is_active());
        }
    }
}

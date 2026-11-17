use super::sync::TimeSync;
use super::types::{ChannelFormat, LslError, Sample, SampleData, StreamInfo, StreamStatus};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use tokio::sync::RwLock;
use tokio::time::{Duration, Instant};
use tracing::{debug, error, info, warn};

// Import real LSL library
use lsl;

// Use std::sync channels for dedicated thread communication (avoids per-command runtime creation)
use std::sync::mpsc as std_mpsc;

/// Response channel type - uses std::sync::mpsc for thread-safe, non-async responses
type ResponseSender<T> = std_mpsc::Sender<T>;

/// Commands sent to the dedicated LSL outlet thread
enum OutletCommand {
    /// Push a sample with data and timestamp
    PushSample {
        data: SampleData,
        timestamp: f64,
        response: ResponseSender<Result<(), LslError>>,
    },
    /// Shutdown the outlet thread
    Shutdown,
}

/// LSL stream outlet for publishing data
/// Uses a dedicated thread pattern because lsl::StreamOutlet is not Send+Sync
pub struct StreamOutlet {
    /// Stream information
    info: StreamInfo,
    /// Outlet status
    status: Arc<RwLock<StreamStatus>>,
    /// Time synchronization utility
    time_sync: Arc<TimeSync>,
    /// Data buffer for outgoing samples (local cache)
    buffer: Arc<RwLock<VecDeque<Sample>>>,
    /// Buffer size limit
    buffer_limit: usize,
    /// Active status flag
    active: Arc<AtomicBool>,
    /// Sample counter
    sample_count: Arc<AtomicU64>,
    /// Command sender to the dedicated LSL thread (std::sync for blocking recv in thread)
    command_tx: std_mpsc::Sender<OutletCommand>,
    /// Shutdown flag - thread polls this to know when to exit
    shutdown_flag: Arc<AtomicBool>,
    /// Performance metrics
    bytes_sent: Arc<AtomicU64>,
    last_send_time: Arc<RwLock<Option<Instant>>>,
    /// Thread join handle (wrapped in Option for Drop)
    _thread_handle: Option<thread::JoinHandle<()>>,
}

// Manual Debug implementation since thread::JoinHandle doesn't implement Debug
impl std::fmt::Debug for StreamOutlet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamOutlet")
            .field("info", &self.info)
            .field("active", &self.active.load(Ordering::Relaxed))
            .field("sample_count", &self.sample_count.load(Ordering::Relaxed))
            .finish()
    }
}

/// Outlet configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutletConfig {
    /// Buffer size for samples
    pub buffer_size: usize,
    /// Maximum transmission rate (samples per second)
    pub max_rate: f64,
    /// Enable automatic time stamping
    pub auto_timestamp: bool,
    /// Compression level (0-9, 0=none)
    pub compression: u32,
    /// Enable data integrity checks
    pub enable_crc: bool,
    /// Chunk size for LSL transmission (0 = one chunk per push)
    pub chunk_size: i32,
    /// Max buffered seconds in LSL outlet
    pub max_buffered: i32,
}

impl Default for OutletConfig {
    fn default() -> Self {
        Self {
            buffer_size: 1000,
            max_rate: 0.0, // Unlimited
            auto_timestamp: true,
            compression: 0,
            enable_crc: false,
            chunk_size: 0,
            max_buffered: 360, // 6 minutes default
        }
    }
}

/// Outlet manager for handling multiple outlets
#[derive(Debug)]
pub struct OutletManager {
    /// Active outlets
    outlets: Arc<RwLock<std::collections::HashMap<String, Arc<StreamOutlet>>>>,
    /// Default configuration
    default_config: OutletConfig,
    /// Time synchronization
    time_sync: Arc<TimeSync>,
}

impl StreamOutlet {
    /// Create a new stream outlet with a dedicated thread for LSL operations
    pub async fn new(
        info: StreamInfo,
        config: OutletConfig,
        time_sync: Arc<TimeSync>,
    ) -> Result<Self, LslError> {
        info!(
            device = "lsl",
            "Creating LSL outlet: {} (type: {}, channels: {})",
            info.name,
            info.stream_type,
            info.channel_count
        );

        // Create std::sync channel for commands (allows blocking recv in thread)
        let (command_tx, command_rx) = std_mpsc::channel::<OutletCommand>();

        // Shutdown flag for clean thread termination
        let shutdown_flag = Arc::new(AtomicBool::new(false));
        let thread_shutdown_flag = shutdown_flag.clone();

        // Clone info for the thread
        let thread_info = info.clone();
        let chunk_size = config.chunk_size;
        let max_buffered = config.max_buffered;

        // Spawn dedicated thread for LSL operations
        // LSL objects use Rc internally and are not Send, so all LSL operations
        // must happen on the same thread
        let thread_handle = thread::spawn(move || {
            // Convert our StreamInfo to LSL format string for stream type
            let stream_type_str = match thread_info.stream_type {
                super::types::StreamType::Markers => "Markers",
                super::types::StreamType::FNIRS => "FNIRS",
                super::types::StreamType::Gaze => "Gaze",
                super::types::StreamType::Biosignals => "Biosignals",
                super::types::StreamType::Generic => "Generic",
            };

            // Convert our ChannelFormat to LSL ChannelFormat
            let lsl_format = match thread_info.channel_format {
                ChannelFormat::Float32 => lsl::ChannelFormat::Float32,
                ChannelFormat::Float64 => lsl::ChannelFormat::Double64,
                ChannelFormat::String => lsl::ChannelFormat::String,
                ChannelFormat::Int32 => lsl::ChannelFormat::Int32,
                ChannelFormat::Int16 => lsl::ChannelFormat::Int16,
                ChannelFormat::Int8 => lsl::ChannelFormat::Int8,
                ChannelFormat::Int64 => lsl::ChannelFormat::Int64,
            };

            // Create LSL StreamInfo
            let lsl_info = match lsl::StreamInfo::new(
                &thread_info.name,
                stream_type_str,
                thread_info.channel_count,
                thread_info.nominal_srate,
                lsl_format,
                &thread_info.source_id,
            ) {
                Ok(info) => info,
                Err(e) => {
                    error!(device = "lsl", "Failed to create LSL StreamInfo: {:?}", e);
                    return;
                }
            };

            // Create LSL StreamOutlet
            let lsl_outlet = match lsl::StreamOutlet::new(&lsl_info, chunk_size, max_buffered) {
                Ok(outlet) => outlet,
                Err(e) => {
                    error!(device = "lsl", "Failed to create LSL StreamOutlet: {:?}", e);
                    return;
                }
            };

            info!(
                device = "lsl",
                "LSL outlet thread started for: {}", thread_info.name
            );

            // Process commands in a blocking loop using std::sync::mpsc
            // No Tokio runtime needed - this is pure blocking I/O
            loop {
                // Check shutdown flag first
                if thread_shutdown_flag.load(Ordering::Relaxed) {
                    info!(
                        device = "lsl",
                        "LSL outlet thread shutdown flag set: {}", thread_info.name
                    );
                    break;
                }

                // Use recv_timeout to allow periodic shutdown flag checks
                match command_rx.recv_timeout(std::time::Duration::from_millis(100)) {
                    Ok(OutletCommand::PushSample {
                        data,
                        timestamp,
                        response,
                    }) => {
                        let result = Self::push_sample_to_lsl(&lsl_outlet, &data, timestamp);
                        let _ = response.send(result);
                    }
                    Ok(OutletCommand::Shutdown) => {
                        info!(
                            device = "lsl",
                            "LSL outlet thread shutting down: {}", thread_info.name
                        );
                        break;
                    }
                    Err(std_mpsc::RecvTimeoutError::Timeout) => {
                        // Normal timeout, continue loop to check shutdown flag
                        continue;
                    }
                    Err(std_mpsc::RecvTimeoutError::Disconnected) => {
                        info!(
                            device = "lsl",
                            "LSL outlet command channel closed: {}", thread_info.name
                        );
                        break;
                    }
                }
            }
        });

        let outlet = Self {
            info,
            status: Arc::new(RwLock::new(StreamStatus::default())),
            time_sync,
            buffer: Arc::new(RwLock::new(VecDeque::with_capacity(config.buffer_size))),
            buffer_limit: config.buffer_size,
            active: Arc::new(AtomicBool::new(false)),
            sample_count: Arc::new(AtomicU64::new(0)),
            command_tx,
            shutdown_flag,
            bytes_sent: Arc::new(AtomicU64::new(0)),
            last_send_time: Arc::new(RwLock::new(None)),
            _thread_handle: Some(thread_handle),
        };

        debug!(device = "lsl", "LSL outlet created successfully");
        Ok(outlet)
    }

    /// Push sample to the real LSL outlet (called from dedicated thread)
    /// Uses push_sample_ex() when timestamp > 0 to preserve user-provided timestamps
    fn push_sample_to_lsl(
        outlet: &lsl::StreamOutlet,
        data: &SampleData,
        timestamp: f64,
    ) -> Result<(), LslError> {
        use lsl::ExPushable;
        use lsl::Pushable;

        // Use push_sample_ex() for explicit timestamps, push_sample() for auto-timestamp
        // timestamp == 0.0 means "use current LSL time"
        // pushthrough = true means the sample should be sent immediately
        let use_explicit_timestamp = timestamp > 0.0;

        match data {
            SampleData::Float32(values) => {
                if use_explicit_timestamp {
                    outlet
                        .push_sample_ex(values, timestamp, true)
                        .map_err(|e| LslError::LslLibraryError(format!("Push failed: {:?}", e)))?;
                } else {
                    outlet
                        .push_sample(values)
                        .map_err(|e| LslError::LslLibraryError(format!("Push failed: {:?}", e)))?;
                }
            }
            SampleData::Float64(values) => {
                if use_explicit_timestamp {
                    outlet
                        .push_sample_ex(values, timestamp, true)
                        .map_err(|e| LslError::LslLibraryError(format!("Push failed: {:?}", e)))?;
                } else {
                    outlet
                        .push_sample(values)
                        .map_err(|e| LslError::LslLibraryError(format!("Push failed: {:?}", e)))?;
                }
            }
            SampleData::String(values) => {
                if use_explicit_timestamp {
                    outlet
                        .push_sample_ex(values, timestamp, true)
                        .map_err(|e| LslError::LslLibraryError(format!("Push failed: {:?}", e)))?;
                } else {
                    outlet
                        .push_sample(values)
                        .map_err(|e| LslError::LslLibraryError(format!("Push failed: {:?}", e)))?;
                }
            }
            SampleData::Int32(values) => {
                if use_explicit_timestamp {
                    outlet
                        .push_sample_ex(values, timestamp, true)
                        .map_err(|e| LslError::LslLibraryError(format!("Push failed: {:?}", e)))?;
                } else {
                    outlet
                        .push_sample(values)
                        .map_err(|e| LslError::LslLibraryError(format!("Push failed: {:?}", e)))?;
                }
            }
            SampleData::Int16(values) => {
                if use_explicit_timestamp {
                    outlet
                        .push_sample_ex(values, timestamp, true)
                        .map_err(|e| LslError::LslLibraryError(format!("Push failed: {:?}", e)))?;
                } else {
                    outlet
                        .push_sample(values)
                        .map_err(|e| LslError::LslLibraryError(format!("Push failed: {:?}", e)))?;
                }
            }
            SampleData::Int8(values) => {
                if use_explicit_timestamp {
                    outlet
                        .push_sample_ex(values, timestamp, true)
                        .map_err(|e| LslError::LslLibraryError(format!("Push failed: {:?}", e)))?;
                } else {
                    outlet
                        .push_sample(values)
                        .map_err(|e| LslError::LslLibraryError(format!("Push failed: {:?}", e)))?;
                }
            }
            SampleData::Int64(values) => {
                // LSL doesn't support i64 natively, convert to i32 (may truncate large values)
                let values_i32: Vec<i32> = values.iter().map(|&v| v as i32).collect();
                if use_explicit_timestamp {
                    outlet
                        .push_sample_ex(&values_i32, timestamp, true)
                        .map_err(|e| LslError::LslLibraryError(format!("Push failed: {:?}", e)))?;
                } else {
                    outlet
                        .push_sample(&values_i32)
                        .map_err(|e| LslError::LslLibraryError(format!("Push failed: {:?}", e)))?;
                }
            }
        }

        Ok(())
    }

    /// Start the outlet and begin publishing
    pub async fn start(&self) -> Result<(), LslError> {
        if self.active.load(Ordering::Relaxed) {
            warn!(device = "lsl", "Outlet already active");
            return Ok(());
        }

        info!(device = "lsl", "Starting LSL outlet: {}", self.info.name);

        self.active.store(true, Ordering::Relaxed);

        let mut status = self.status.write().await;
        status.active = true;

        info!(device = "lsl", "LSL outlet started successfully");
        Ok(())
    }

    /// Stop the outlet
    pub async fn stop(&self) -> Result<(), LslError> {
        if !self.active.load(Ordering::Relaxed) {
            return Ok(());
        }

        info!(device = "lsl", "Stopping LSL outlet: {}", self.info.name);

        self.active.store(false, Ordering::Relaxed);

        let mut status = self.status.write().await;
        status.active = false;

        // Set shutdown flag and send shutdown command to thread
        self.shutdown_flag.store(true, Ordering::Relaxed);
        let _ = self.command_tx.send(OutletCommand::Shutdown);

        info!(device = "lsl", "LSL outlet stopped");
        Ok(())
    }

    /// Send a sample through the outlet
    pub async fn send_sample(&self, mut sample: Sample) -> Result<(), LslError> {
        if !self.active.load(Ordering::Relaxed) {
            return Err(LslError::LslLibraryError("Outlet not active".to_string()));
        }

        // Auto-timestamp if enabled
        if sample.timestamp == 0.0 {
            sample.timestamp = self.time_sync.create_timestamp();
        }

        // Validate channel count
        if sample.data.channel_count() != self.info.channel_count as usize {
            return Err(LslError::DataFormatMismatch {
                expected: format!("{} channels", self.info.channel_count),
                actual: format!("{} channels", sample.data.channel_count()),
            });
        }

        // Add to local buffer for tracking
        {
            let mut buffer = self.buffer.write().await;

            // Check buffer overflow
            if buffer.len() >= self.buffer_limit {
                buffer.pop_front(); // Remove oldest sample
                warn!(
                    device = "lsl",
                    "Outlet buffer overflow, dropping oldest sample"
                );

                let mut status = self.status.write().await;
                status.data_loss = (status.data_loss + 0.1).min(100.0);
            }

            buffer.push_back(sample.clone());
        }

        // Update metrics
        self.sample_count.fetch_add(1, Ordering::Relaxed);
        let bytes = sample.data.to_bytes().len() as u64;
        self.bytes_sent.fetch_add(bytes, Ordering::Relaxed);

        // Update status
        {
            let mut status = self.status.write().await;
            status.sample_count += 1;
            status.last_timestamp = sample.timestamp;
        }

        // Update last send time
        {
            let mut last_send = self.last_send_time.write().await;
            *last_send = Some(Instant::now());
        }

        let timestamp = sample.timestamp;
        let channel_count = sample.data.channel_count();

        // Create response channel (std::sync - blocking recv is wrapped in spawn_blocking)
        let (response_tx, response_rx) = std_mpsc::channel();

        // Send command to dedicated LSL thread (std_mpsc::send is fast, OK to call from async)
        self.command_tx
            .send(OutletCommand::PushSample {
                data: sample.data,
                timestamp: sample.timestamp,
                response: response_tx,
            })
            .map_err(|_| LslError::LslLibraryError("Outlet thread not responding".to_string()))?;

        // Wait for response with timeout using spawn_blocking for the blocking recv
        let result = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::task::spawn_blocking(move || {
                response_rx.recv_timeout(std::time::Duration::from_secs(5))
            })
            .await
        })
        .await;

        match result {
            Ok(Ok(Ok(inner_result))) => inner_result?,
            Ok(Ok(Err(_recv_err))) => {
                return Err(LslError::LslLibraryError(
                    "Outlet thread response timeout".to_string(),
                ))
            }
            Ok(Err(_join_err)) => {
                return Err(LslError::LslLibraryError(
                    "Outlet thread task failed".to_string(),
                ))
            }
            Err(_timeout) => {
                return Err(LslError::LslLibraryError(
                    "Outlet thread timeout".to_string(),
                ))
            }
        }

        debug!(
            device = "lsl",
            "Sample sent: timestamp={:.3}, channels={}", timestamp, channel_count
        );

        Ok(())
    }

    /// Check if outlet is active
    pub fn is_active(&self) -> bool {
        self.active.load(Ordering::Relaxed)
    }

    /// Get sample count
    pub fn get_sample_count(&self) -> u64 {
        self.sample_count.load(Ordering::Relaxed)
    }

    /// Get bytes sent
    pub fn get_bytes_sent(&self) -> u64 {
        self.bytes_sent.load(Ordering::Relaxed)
    }

    /// Get buffer usage
    pub async fn get_buffer_usage(&self) -> (usize, usize) {
        let buffer = self.buffer.read().await;
        (buffer.len(), self.buffer_limit)
    }

    /// Get outlet statistics
    pub async fn get_stats(&self) -> serde_json::Value {
        let status = self.status.read().await;
        let (buffer_used, buffer_size) = self.get_buffer_usage().await;
        let last_send = self.last_send_time.read().await;

        let seconds_since_last_send = last_send.map(|t| t.elapsed().as_secs_f64()).unwrap_or(0.0);

        serde_json::json!({
            "name": self.info.name,
            "type": self.info.stream_type,
            "active": self.is_active(),
            "sample_count": self.get_sample_count(),
            "bytes_sent": self.get_bytes_sent(),
            "buffer_usage": format!("{}/{}", buffer_used, buffer_size),
            "buffer_usage_percent": (buffer_used as f64 / buffer_size as f64) * 100.0,
            "data_loss_percent": status.data_loss,
            "last_timestamp": status.last_timestamp,
            "seconds_since_last_send": seconds_since_last_send
        })
    }
}

impl Drop for StreamOutlet {
    fn drop(&mut self) {
        // Signal shutdown via flag (thread will see this within 100ms)
        self.shutdown_flag.store(true, Ordering::Relaxed);
        // Also send shutdown command for immediate response
        let _ = self.command_tx.send(OutletCommand::Shutdown);
    }
}

impl OutletManager {
    /// Create a new outlet manager
    pub fn new(time_sync: Arc<TimeSync>) -> Self {
        Self {
            outlets: Arc::new(RwLock::new(std::collections::HashMap::new())),
            default_config: OutletConfig::default(),
            time_sync,
        }
    }

    /// Create a new outlet
    pub async fn create_outlet(
        &self,
        info: StreamInfo,
        config: Option<OutletConfig>,
    ) -> Result<String, LslError> {
        let outlet_config = config.unwrap_or_else(|| self.default_config.clone());
        let outlet =
            Arc::new(StreamOutlet::new(info.clone(), outlet_config, self.time_sync.clone()).await?);

        let outlet_id = format!("{}_{}", info.stream_type, info.source_id);

        let mut outlets = self.outlets.write().await;
        outlets.insert(outlet_id.clone(), outlet);

        info!(device = "lsl", "Created outlet: {}", outlet_id);
        Ok(outlet_id)
    }

    /// Get outlet by ID
    pub async fn get_outlet(&self, outlet_id: &str) -> Option<Arc<StreamOutlet>> {
        let outlets = self.outlets.read().await;
        outlets.get(outlet_id).cloned()
    }

    /// Start outlet
    pub async fn start_outlet(&self, outlet_id: &str) -> Result<(), LslError> {
        let outlet = self
            .get_outlet(outlet_id)
            .await
            .ok_or_else(|| LslError::StreamNotFound(outlet_id.to_string()))?;

        outlet.start().await
    }

    /// Remove outlet
    pub async fn remove_outlet(&self, outlet_id: &str) -> Result<(), LslError> {
        // Stop outlet first
        if let Some(outlet) = self.get_outlet(outlet_id).await {
            outlet.stop().await?;
        }

        let mut outlets = self.outlets.write().await;
        outlets.remove(outlet_id);

        info!(device = "lsl", "Removed outlet: {}", outlet_id);
        Ok(())
    }

    /// Send sample to outlet
    pub async fn send_sample(&self, outlet_id: &str, sample: Sample) -> Result<(), LslError> {
        let outlet = self
            .get_outlet(outlet_id)
            .await
            .ok_or_else(|| LslError::StreamNotFound(outlet_id.to_string()))?;

        outlet.send_sample(sample).await
    }

    /// Get outlet statistics for all outlets
    pub async fn get_all_stats(&self) -> serde_json::Value {
        let outlets = self.outlets.read().await;
        let mut stats = std::collections::HashMap::new();

        for (id, outlet) in outlets.iter() {
            stats.insert(id.clone(), outlet.get_stats().await);
        }

        serde_json::json!({
            "outlet_count": outlets.len(),
            "outlets": stats
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::devices::lsl::sync::TimeSync;
    use crate::devices::lsl::types::SampleData;

    #[tokio::test]
    async fn test_outlet_creation() {
        let time_sync = Arc::new(TimeSync::new(false));
        let info = StreamInfo::ttl_markers("test_device");
        let config = OutletConfig::default();

        let outlet = StreamOutlet::new(info, config, time_sync).await;
        assert!(outlet.is_ok());

        let outlet = outlet.unwrap();
        assert_eq!(outlet.info.name, "test_device_TTL_Markers");
        assert!(!outlet.is_active());
    }

    #[tokio::test]
    async fn test_outlet_lifecycle() {
        let time_sync = Arc::new(TimeSync::new(false));
        let info = StreamInfo::ttl_markers("test_device");
        let config = OutletConfig::default();

        let outlet = StreamOutlet::new(info, config, time_sync).await.unwrap();

        // Start outlet
        assert!(outlet.start().await.is_ok());
        assert!(outlet.is_active());

        // Stop outlet
        assert!(outlet.stop().await.is_ok());
        assert!(!outlet.is_active());
    }

    #[tokio::test]
    async fn test_sample_sending() {
        let time_sync = Arc::new(TimeSync::new(false));
        let info = StreamInfo::ttl_markers("test_device");
        let config = OutletConfig::default();

        let outlet = StreamOutlet::new(info, config, time_sync).await.unwrap();
        outlet.start().await.unwrap();

        let sample = Sample {
            data: SampleData::ttl_marker("TEST_PULSE".to_string()),
            timestamp: 1000.0,
        };

        assert!(outlet.send_sample(sample).await.is_ok());
        assert_eq!(outlet.get_sample_count(), 1);
    }

    #[tokio::test]
    async fn test_outlet_manager() {
        let time_sync = Arc::new(TimeSync::new(false));
        let manager = OutletManager::new(time_sync);

        let outlet_id = manager
            .create_outlet(StreamInfo::ttl_markers("test_device"), None)
            .await;
        assert!(outlet_id.is_ok());

        let outlet_id = outlet_id.unwrap();
        assert!(manager.start_outlet(&outlet_id).await.is_ok());
        assert!(manager.get_outlet(&outlet_id).await.is_some());
    }

    #[tokio::test]
    async fn test_sample_with_explicit_timestamp() {
        // Test that explicit timestamps are preserved (push_sample_ex fix)
        let time_sync = Arc::new(TimeSync::new(false));
        let info = StreamInfo::ttl_markers("test_explicit_ts");
        let config = OutletConfig::default();

        let outlet = StreamOutlet::new(info, config, time_sync).await.unwrap();
        outlet.start().await.unwrap();

        // Send sample with explicit timestamp
        let explicit_timestamp = 12345.6789;
        let sample = Sample {
            data: SampleData::ttl_marker("EXPLICIT_TS".to_string()),
            timestamp: explicit_timestamp,
        };

        assert!(outlet.send_sample(sample).await.is_ok());
        assert_eq!(outlet.get_sample_count(), 1);

        // Verify the status records the explicit timestamp
        let status = outlet.status.read().await.clone();
        assert!((status.last_timestamp - explicit_timestamp).abs() < 0.001);
    }

    #[tokio::test]
    async fn test_sample_with_auto_timestamp() {
        // Test that timestamp == 0.0 triggers auto-timestamping
        let time_sync = Arc::new(TimeSync::new(false));
        let info = StreamInfo::ttl_markers("test_auto_ts");
        let config = OutletConfig::default();

        let outlet = StreamOutlet::new(info, config, time_sync).await.unwrap();
        outlet.start().await.unwrap();

        // Send sample without timestamp (0.0 = auto-timestamp)
        let sample = Sample {
            data: SampleData::ttl_marker("AUTO_TS".to_string()),
            timestamp: 0.0,
        };

        assert!(outlet.send_sample(sample).await.is_ok());

        // Verify a timestamp was assigned
        let status = outlet.status.read().await.clone();
        assert!(status.last_timestamp > 0.0);
    }

    #[tokio::test]
    async fn test_float32_sample_sending() {
        // Test Float32 data type (common for fNIRS, EEG)
        use crate::devices::lsl::types::{ChannelFormat, StreamType};

        let time_sync = Arc::new(TimeSync::new(false));
        let info = StreamInfo::new(
            "float32_test".to_string(),
            StreamType::Biosignals,
            4, // 4 channels
            100.0,
            ChannelFormat::Float32,
            "test_source".to_string(),
        );
        let config = OutletConfig::default();

        let outlet = StreamOutlet::new(info, config, time_sync).await.unwrap();
        outlet.start().await.unwrap();

        // Send Float32 sample
        let sample = Sample {
            data: SampleData::Float32(vec![1.0, 2.0, 3.0, 4.0]),
            timestamp: 5000.0,
        };

        assert!(outlet.send_sample(sample).await.is_ok());
        assert_eq!(outlet.get_sample_count(), 1);
    }

    #[tokio::test]
    async fn test_float64_sample_sending() {
        // Test Float64 data type (high precision)
        use crate::devices::lsl::types::{ChannelFormat, StreamType};

        let time_sync = Arc::new(TimeSync::new(false));
        let info = StreamInfo::new(
            "float64_test".to_string(),
            StreamType::Generic,
            2, // 2 channels
            50.0,
            ChannelFormat::Float64,
            "test_source".to_string(),
        );
        let config = OutletConfig::default();

        let outlet = StreamOutlet::new(info, config, time_sync).await.unwrap();
        outlet.start().await.unwrap();

        // Send Float64 sample
        let sample = Sample {
            data: SampleData::Float64(vec![1.23456789012345, 9.87654321098765]),
            timestamp: 6000.0,
        };

        assert!(outlet.send_sample(sample).await.is_ok());
        assert_eq!(outlet.get_sample_count(), 1);
    }

    #[tokio::test]
    async fn test_int32_sample_sending() {
        // Test Int32 data type (integer signals)
        use crate::devices::lsl::types::{ChannelFormat, StreamType};

        let time_sync = Arc::new(TimeSync::new(false));
        let info = StreamInfo::new(
            "int32_test".to_string(),
            StreamType::Generic,
            3,
            25.0,
            ChannelFormat::Int32,
            "test_source".to_string(),
        );
        let config = OutletConfig::default();

        let outlet = StreamOutlet::new(info, config, time_sync).await.unwrap();
        outlet.start().await.unwrap();

        // Send Int32 sample with edge cases
        let sample = Sample {
            data: SampleData::Int32(vec![-1000000, 0, 1000000]),
            timestamp: 7000.0,
        };

        assert!(outlet.send_sample(sample).await.is_ok());
        assert_eq!(outlet.get_sample_count(), 1);
    }

    #[tokio::test]
    async fn test_channel_count_validation() {
        // Test that channel count mismatch is caught
        let time_sync = Arc::new(TimeSync::new(false));
        let info = StreamInfo::ttl_markers("test_channel_validation"); // 1 channel
        let config = OutletConfig::default();

        let outlet = StreamOutlet::new(info, config, time_sync).await.unwrap();
        outlet.start().await.unwrap();

        // Try to send sample with wrong channel count
        let sample = Sample {
            data: SampleData::String(vec!["A".to_string(), "B".to_string()]), // 2 channels
            timestamp: 1000.0,
        };

        let result = outlet.send_sample(sample).await;
        assert!(result.is_err());

        // Verify it's a DataFormatMismatch error
        match result {
            Err(LslError::DataFormatMismatch { expected, actual }) => {
                assert!(expected.contains("1"));
                assert!(actual.contains("2"));
            }
            _ => panic!("Expected DataFormatMismatch error"),
        }
    }
}

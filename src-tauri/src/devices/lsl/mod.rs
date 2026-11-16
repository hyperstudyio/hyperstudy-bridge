pub mod frenz;
pub mod frenz_process;
pub mod inlet;
pub mod neon;
pub mod outlet;
pub mod resolver;
pub mod sync;
pub mod types;

use crate::devices::{Device, DeviceConfig, DeviceError, DeviceInfo, DeviceStatus, DeviceType};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, RwLock};
use tracing::{debug, info, warn};

// Re-export main types for convenience
pub use frenz::FrenzLslManager;
pub use frenz_process::FrenzProcessManager;
pub use inlet::{InletConfig, InletManager};
pub use neon::NeonLslManager;
pub use outlet::{OutletConfig, OutletManager};
pub use resolver::{DiscoveredStream, StreamFilter, StreamResolver};
pub use sync::TimeSync;
pub use types::*;

/// Type alias for performance callback
type PerformanceCallback = Box<dyn Fn(&str, Duration, u64, u64) + Send + Sync>;

/// Main LSL device implementation
pub struct LslDevice {
    /// Device configuration
    device_config: DeviceConfig,
    /// LSL-specific configuration
    lsl_config: LslConfig,
    /// Current device status
    status: DeviceStatus,
    /// Device identifier
    device_id: String,
    /// Time synchronization utility
    time_sync: Arc<TimeSync>,
    /// Stream resolver for discovery
    resolver: Arc<StreamResolver>,
    /// Inlet manager for consuming streams
    inlet_manager: Arc<InletManager>,
    /// Outlet manager for producing streams
    outlet_manager: Arc<OutletManager>,
    /// Performance monitoring callback
    performance_callback: Option<PerformanceCallback>,
    /// Command processing channel (bounded to prevent memory exhaustion)
    #[allow(dead_code)]
    command_sender: Option<mpsc::Sender<LslCommand>>,
    command_receiver: Option<mpsc::Receiver<LslCommand>>,
    /// Bridge device outlets (auto-created)
    bridge_outlets: Arc<RwLock<HashMap<String, String>>>, // device_type -> outlet_id
}

impl std::fmt::Debug for LslDevice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LslDevice")
            .field("device_config", &self.device_config)
            .field("lsl_config", &self.lsl_config)
            .field("status", &self.status)
            .field("device_id", &self.device_id)
            .field(
                "has_performance_callback",
                &self.performance_callback.is_some(),
            )
            .finish()
    }
}

/// LSL device commands
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum LslCommand {
    /// Discover streams
    DiscoverStreams {
        filters: Vec<StreamFilter>,
        timeout: f64,
    },
    /// Create inlet for stream
    CreateInlet {
        stream_uid: String,
        config: Option<InletConfig>,
    },
    /// Create outlet for device
    CreateOutlet {
        device_type: String,
        device_id: String,
        config: Option<OutletConfig>,
    },
    /// Send sample to outlet
    SendSample {
        outlet_id: String,
        data: Vec<u8>,
        timestamp: Option<f64>,
    },
    /// Pull sample from inlet
    PullSample { inlet_id: String, timeout: f64 },
    /// Start/stop inlet/outlet
    SetActive {
        stream_id: String,
        stream_type: StreamDirection,
        active: bool,
    },
    /// Get stream statistics
    GetStats { stream_id: Option<String> },
    /// Synchronize time
    SyncTime,
}

/// Stream direction for commands
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum StreamDirection {
    Inlet,
    Outlet,
}

/// LSL command response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LslResponse {
    pub success: bool,
    pub data: serde_json::Value,
    pub error: Option<String>,
}

impl LslDevice {
    /// Create a new LSL device
    pub fn new(device_id: String, lsl_config: Option<LslConfig>) -> Self {
        let lsl_config = lsl_config.unwrap_or_default();
        let time_sync = Arc::new(TimeSync::new(lsl_config.enable_time_sync));
        let resolver = Arc::new(StreamResolver::new(lsl_config.discovery_timeout));
        let inlet_manager = Arc::new(InletManager::new(time_sync.clone()));
        let outlet_manager = Arc::new(OutletManager::new(time_sync.clone()));

        // Use bounded channel to prevent memory exhaustion from command backlog
        let (command_sender, command_receiver) = mpsc::channel(1000);

        Self {
            device_config: DeviceConfig::default(),
            lsl_config,
            status: DeviceStatus::Disconnected,
            device_id: device_id.clone(),
            time_sync,
            resolver,
            inlet_manager,
            outlet_manager,
            performance_callback: None,
            command_sender: Some(command_sender),
            command_receiver: Some(command_receiver),
            bridge_outlets: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Set performance monitoring callback
    pub fn set_performance_callback<F>(&mut self, callback: F)
    where
        F: Fn(&str, Duration, u64, u64) + Send + Sync + 'static,
    {
        self.performance_callback = Some(Box::new(callback));
    }

    /// Create outlets for bridge devices automatically
    async fn create_bridge_outlets(&self) -> Result<(), DeviceError> {
        if !self.lsl_config.auto_create_outlets {
            return Ok(());
        }

        info!(
            device = "lsl",
            "Creating automatic outlets for bridge devices"
        );

        let device_types = ["ttl", "kernel", "pupil"];
        let mut outlets = self.bridge_outlets.write().await;

        for device_type in &device_types {
            let outlet_id = self
                .outlet_manager
                .create_device_outlet(&self.device_id, device_type, None)
                .await
                .map_err(|e| DeviceError::ConfigurationError(e.to_string()))?;

            outlets.insert(device_type.to_string(), outlet_id.clone());

            // Start the outlet
            self.outlet_manager
                .start_outlet(&outlet_id)
                .await
                .map_err(|e| DeviceError::ConfigurationError(e.to_string()))?;

            debug!(
                device = "lsl",
                "Created outlet for {}: {}", device_type, outlet_id
            );
        }

        info!(device = "lsl", "Created {} bridge outlets", outlets.len());
        Ok(())
    }

    /// Discover available LSL streams
    pub async fn discover_streams(
        &self,
        filters: Vec<StreamFilter>,
    ) -> Result<Vec<DiscoveredStream>, LslError> {
        info!(
            device = "lsl",
            "Discovering LSL streams with {} filters",
            filters.len()
        );

        // Create temporary resolver with filters
        let resolver = StreamResolver::with_filters(self.lsl_config.discovery_timeout, filters);

        resolver.discover_streams().await
    }

    /// Send data to a bridge device outlet
    pub async fn send_bridge_data(
        &self,
        device_type: &str,
        data: Vec<u8>,
        timestamp: Option<f64>,
    ) -> Result<(), DeviceError> {
        let data_len = data.len() as u64; // Capture length before moving data

        let outlets = self.bridge_outlets.read().await;
        let outlet_id = outlets.get(device_type).ok_or_else(|| {
            DeviceError::ConfigurationError(format!("No outlet for device type: {}", device_type))
        })?;

        // Convert data to appropriate sample format
        let sample_data = match device_type {
            "ttl" => {
                // TTL data should be string markers
                let marker =
                    String::from_utf8(data).map_err(|e| DeviceError::InvalidData(e.to_string()))?;
                SampleData::ttl_marker(marker)
            }
            "kernel" | "pupil" => {
                // Convert bytes to float32 array
                if data.len() % 4 != 0 {
                    return Err(DeviceError::InvalidData(
                        "Data length not multiple of 4".to_string(),
                    ));
                }

                let mut float_data = Vec::new();
                for chunk in data.chunks_exact(4) {
                    let bytes = [chunk[0], chunk[1], chunk[2], chunk[3]];
                    float_data.push(f32::from_le_bytes(bytes));
                }
                SampleData::float32(float_data)
            }
            _ => {
                return Err(DeviceError::InvalidData(format!(
                    "Unknown device type: {}",
                    device_type
                )))
            }
        };

        let sample = Sample {
            data: sample_data,
            timestamp: timestamp.unwrap_or_else(|| self.time_sync.create_timestamp()),
        };

        self.outlet_manager
            .send_sample(outlet_id, sample)
            .await
            .map_err(|e| DeviceError::CommunicationError(e.to_string()))?;

        // Record performance metrics
        if let Some(ref callback) = self.performance_callback {
            callback(&self.device_id, Duration::from_millis(1), data_len, 0);
        }

        Ok(())
    }

    /// Process LSL commands
    #[allow(dead_code)]
    async fn process_command(&self, command: LslCommand) -> LslResponse {
        match command {
            LslCommand::DiscoverStreams {
                filters,
                timeout: _,
            } => match self.discover_streams(filters).await {
                Ok(streams) => LslResponse {
                    success: true,
                    data: serde_json::json!({ "streams": streams }),
                    error: None,
                },
                Err(e) => LslResponse {
                    success: false,
                    data: serde_json::json!({}),
                    error: Some(e.to_string()),
                },
            },
            LslCommand::CreateOutlet {
                device_type,
                device_id,
                config,
            } => {
                match self
                    .outlet_manager
                    .create_device_outlet(&device_id, &device_type, config)
                    .await
                {
                    Ok(outlet_id) => LslResponse {
                        success: true,
                        data: serde_json::json!({ "outlet_id": outlet_id }),
                        error: None,
                    },
                    Err(e) => LslResponse {
                        success: false,
                        data: serde_json::json!({}),
                        error: Some(e.to_string()),
                    },
                }
            }
            LslCommand::SendSample {
                outlet_id,
                data,
                timestamp,
            } => {
                // Convert data based on outlet type (simplified)
                let sample_data = SampleData::float32(
                    data.chunks_exact(4)
                        .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
                        .collect(),
                );

                let sample = Sample {
                    data: sample_data,
                    timestamp: timestamp.unwrap_or_else(|| self.time_sync.create_timestamp()),
                };

                match self.outlet_manager.send_sample(&outlet_id, sample).await {
                    Ok(()) => LslResponse {
                        success: true,
                        data: serde_json::json!({}),
                        error: None,
                    },
                    Err(e) => LslResponse {
                        success: false,
                        data: serde_json::json!({}),
                        error: Some(e.to_string()),
                    },
                }
            }
            LslCommand::GetStats { stream_id } => {
                let stats = if let Some(_id) = stream_id {
                    // Get specific stream stats (not implemented in this simplified version)
                    serde_json::json!({})
                } else {
                    // Get all stats
                    let outlet_stats = self.outlet_manager.get_all_stats().await;
                    let inlet_stats = self.inlet_manager.get_all_stats().await;
                    let sync_stats = self.time_sync.get_sync_stats().await;

                    serde_json::json!({
                        "outlets": outlet_stats,
                        "inlets": inlet_stats,
                        "time_sync": sync_stats
                    })
                };

                LslResponse {
                    success: true,
                    data: stats,
                    error: None,
                }
            }
            LslCommand::SyncTime => match self.time_sync.synchronize().await {
                Ok(()) => LslResponse {
                    success: true,
                    data: serde_json::json!({}),
                    error: None,
                },
                Err(e) => LslResponse {
                    success: false,
                    data: serde_json::json!({}),
                    error: Some(e.to_string()),
                },
            },
            // Simplified handling for other commands
            _ => LslResponse {
                success: false,
                data: serde_json::json!({}),
                error: Some("Command not implemented".to_string()),
            },
        }
    }

    /// Start command processing loop
    async fn start_command_processing(&mut self) -> Result<(), DeviceError> {
        let mut receiver = self.command_receiver.take().ok_or_else(|| {
            DeviceError::ConfigurationError("Command receiver not available".to_string())
        })?;

        let device_id = self.device_id.clone();

        tokio::spawn(async move {
            info!(
                device = "lsl",
                "Starting LSL command processing for device: {}", device_id
            );

            while let Some(command) = receiver.recv().await {
                debug!(device = "lsl", "Processing LSL command: {:?}", command);

                // In a real implementation, we would process commands here
                // For now, we'll just log them
                debug!(device = "lsl", "Command processed: {:?}", command);
            }

            info!(
                device = "lsl",
                "LSL command processing stopped for device: {}", device_id
            );
        });

        Ok(())
    }

    /// Get comprehensive statistics
    pub async fn get_comprehensive_stats(&self) -> serde_json::Value {
        let outlet_stats = self.outlet_manager.get_all_stats().await;
        let inlet_stats = self.inlet_manager.get_all_stats().await;
        let resolver_stats = self.resolver.get_discovery_stats().await;
        let sync_stats = self.time_sync.get_sync_stats().await;
        let bridge_outlets = self.bridge_outlets.read().await;

        serde_json::json!({
            "device_id": self.device_id,
            "status": self.status,
            "config": {
                "auto_create_outlets": self.lsl_config.auto_create_outlets,
                "auto_discover_inlets": self.lsl_config.auto_discover_inlets,
                "max_inlets": self.lsl_config.max_inlets,
                "buffer_size": self.lsl_config.buffer_size,
                "enable_time_sync": self.lsl_config.enable_time_sync
            },
            "bridge_outlets": bridge_outlets.clone(),
            "outlets": outlet_stats,
            "inlets": inlet_stats,
            "resolver": resolver_stats,
            "time_sync": sync_stats
        })
    }
}

#[async_trait]
impl Device for LslDevice {
    async fn connect(&mut self) -> Result<(), DeviceError> {
        info!(device = "lsl", "Connecting LSL device: {}", self.device_id);

        self.status = DeviceStatus::Connecting;

        let start_time = std::time::Instant::now();

        // Perform time synchronization if enabled
        if self.lsl_config.enable_time_sync {
            self.time_sync.synchronize().await.map_err(|e| {
                if let Some(ref callback) = self.performance_callback {
                    callback(&self.device_id, start_time.elapsed(), 0, 0);
                }
                DeviceError::ConnectionFailed(e.to_string())
            })?;
        }

        // Create bridge outlets if configured
        self.create_bridge_outlets().await?;

        // Start command processing
        self.start_command_processing().await?;

        // Discover streams if auto-discovery is enabled
        if self.lsl_config.auto_discover_inlets {
            match self.discover_streams(vec![]).await {
                Ok(streams) => {
                    info!(device = "lsl", "Discovered {} LSL streams", streams.len());
                }
                Err(e) => {
                    warn!(
                        device = "lsl",
                        "Failed to discover streams during connection: {}", e
                    );
                    // Don't fail connection due to discovery issues
                }
            }
        }

        let connection_latency = start_time.elapsed();

        // Record successful connection
        if let Some(ref callback) = self.performance_callback {
            callback(&self.device_id, connection_latency, 0, 0);
        }

        self.status = DeviceStatus::Connected;
        info!(
            device = "lsl",
            "LSL device connected successfully in {:?}", connection_latency
        );

        Ok(())
    }

    async fn disconnect(&mut self) -> Result<(), DeviceError> {
        info!(
            device = "lsl",
            "Disconnecting LSL device: {}", self.device_id
        );

        // Stop all outlets and inlets
        self.outlet_manager
            .stop_all()
            .await
            .map_err(|e| DeviceError::CommunicationError(e.to_string()))?;

        self.inlet_manager
            .stop_all()
            .await
            .map_err(|e| DeviceError::CommunicationError(e.to_string()))?;

        self.status = DeviceStatus::Disconnected;
        info!(device = "lsl", "LSL device disconnected");

        Ok(())
    }

    async fn send(&mut self, data: &[u8]) -> Result<(), DeviceError> {
        // Parse the data to determine the target device type
        // For this implementation, we'll assume the first byte indicates device type
        if data.is_empty() {
            return Err(DeviceError::InvalidData("Empty data".to_string()));
        }

        let device_type = match data[0] {
            0 => "ttl",
            1 => "kernel",
            2 => "pupil",
            _ => return Err(DeviceError::InvalidData("Unknown device type".to_string())),
        };

        let payload = data[1..].to_vec();
        self.send_bridge_data(device_type, payload, None).await
    }

    async fn receive(&mut self) -> Result<Vec<u8>, DeviceError> {
        // This is a placeholder implementation
        // In a real implementation, this might pull from inlets

        // Try to get data from the first available inlet
        let inlet_ids = self.inlet_manager.list_inlets().await;

        if let Some(inlet_id) = inlet_ids.first() {
            match self
                .inlet_manager
                .pull_sample(inlet_id, Duration::from_millis(100))
                .await
            {
                Ok(Some(sample)) => {
                    // Convert sample to bytes
                    Ok(sample.data.to_bytes())
                }
                Ok(None) => {
                    // No data available
                    Ok(Vec::new())
                }
                Err(e) => Err(DeviceError::CommunicationError(e.to_string())),
            }
        } else {
            // No inlets available
            Ok(Vec::new())
        }
    }

    fn get_info(&self) -> DeviceInfo {
        DeviceInfo {
            id: format!("lsl_{}", self.device_id),
            name: format!("LSL Bridge ({})", self.device_id),
            device_type: DeviceType::LSL,
            status: self.status,
            metadata: serde_json::json!({
                "auto_create_outlets": self.lsl_config.auto_create_outlets,
                "auto_discover_inlets": self.lsl_config.auto_discover_inlets,
                "time_sync_enabled": self.lsl_config.enable_time_sync,
                "discovery_timeout": self.lsl_config.discovery_timeout,
                "buffer_size": self.lsl_config.buffer_size,
            }),
        }
    }

    fn get_status(&self) -> DeviceStatus {
        self.status
    }

    fn configure(&mut self, config: DeviceConfig) -> Result<(), DeviceError> {
        self.device_config = config;

        // Update LSL config from custom settings
        if let Some(custom) = self.device_config.custom_settings.as_object() {
            if let Some(auto_outlets) = custom.get("auto_create_outlets").and_then(|v| v.as_bool())
            {
                self.lsl_config.auto_create_outlets = auto_outlets;
            }

            if let Some(auto_inlets) = custom.get("auto_discover_inlets").and_then(|v| v.as_bool())
            {
                self.lsl_config.auto_discover_inlets = auto_inlets;
            }

            if let Some(max_inlets) = custom.get("max_inlets").and_then(|v| v.as_u64()) {
                self.lsl_config.max_inlets = max_inlets as usize;
            }

            if let Some(timeout) = custom.get("discovery_timeout").and_then(|v| v.as_f64()) {
                self.lsl_config.discovery_timeout = timeout;
            }

            if let Some(buffer_size) = custom.get("buffer_size").and_then(|v| v.as_u64()) {
                self.lsl_config.buffer_size = buffer_size as usize;
            }

            if let Some(time_sync) = custom.get("enable_time_sync").and_then(|v| v.as_bool()) {
                self.lsl_config.enable_time_sync = time_sync;
            }
        }

        Ok(())
    }

    async fn heartbeat(&mut self) -> Result<(), DeviceError> {
        if self.status != DeviceStatus::Connected {
            return Err(DeviceError::NotConnected);
        }

        // Check time sync if needed
        if self.time_sync.needs_sync().await {
            self.time_sync
                .synchronize()
                .await
                .map_err(|e| DeviceError::CommunicationError(e.to_string()))?;
        }

        // Check for clock drift
        self.time_sync
            .check_drift()
            .await
            .map_err(|e| DeviceError::CommunicationError(e.to_string()))?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_lsl_device_creation() {
        let device = LslDevice::new("test_device".to_string(), None);
        assert_eq!(device.device_id, "test_device");
        assert_eq!(device.status, DeviceStatus::Disconnected);
    }

    #[tokio::test]
    async fn test_device_info() {
        let device = LslDevice::new("test_device".to_string(), None);
        let info = device.get_info();

        assert_eq!(info.id, "lsl_test_device");
        assert_eq!(info.device_type, DeviceType::LSL);
        assert_eq!(info.status, DeviceStatus::Disconnected);
    }

    #[tokio::test]
    async fn test_configuration() {
        let mut device = LslDevice::new("test_device".to_string(), None);

        let config = DeviceConfig {
            custom_settings: serde_json::json!({
                "auto_create_outlets": false,
                "buffer_size": 2000
            }),
            ..Default::default()
        };

        assert!(device.configure(config).is_ok());
        assert!(!device.lsl_config.auto_create_outlets);
        assert_eq!(device.lsl_config.buffer_size, 2000);
    }

    #[tokio::test]
    async fn test_connect_disconnect() {
        let mut device = LslDevice::new("test_device".to_string(), None);

        // Test connection
        assert!(device.connect().await.is_ok());
        assert_eq!(device.get_status(), DeviceStatus::Connected);

        // Test disconnection
        assert!(device.disconnect().await.is_ok());
        assert_eq!(device.get_status(), DeviceStatus::Disconnected);
    }
}

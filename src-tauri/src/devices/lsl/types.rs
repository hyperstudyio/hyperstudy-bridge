use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;

/// LSL channel format types
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChannelFormat {
    Float32,
    Float64,
    String,
    Int32,
    Int16,
    Int8,
    Int64,
}

impl fmt::Display for ChannelFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ChannelFormat::Float32 => write!(f, "float32"),
            ChannelFormat::Float64 => write!(f, "float64"),
            ChannelFormat::String => write!(f, "string"),
            ChannelFormat::Int32 => write!(f, "int32"),
            ChannelFormat::Int16 => write!(f, "int16"),
            ChannelFormat::Int8 => write!(f, "int8"),
            ChannelFormat::Int64 => write!(f, "int64"),
        }
    }
}

/// Stream types for different device categories
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum StreamType {
    /// TTL pulse markers
    Markers,
    /// fNIRS optical density data
    FNIRS,
    /// Eye tracking gaze data
    Gaze,
    /// Physiological signals (EEG, EMG, ECG, etc.)
    Biosignals,
    /// Generic data stream
    Generic,
}

impl fmt::Display for StreamType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StreamType::Markers => write!(f, "Markers"),
            StreamType::FNIRS => write!(f, "fNIRS"),
            StreamType::Gaze => write!(f, "Gaze"),
            StreamType::Biosignals => write!(f, "Biosignals"),
            StreamType::Generic => write!(f, "Generic"),
        }
    }
}

/// Stream information structure
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamInfo {
    pub name: String,
    pub stream_type: StreamType,
    pub channel_count: u32,
    pub nominal_srate: f64,
    pub channel_format: ChannelFormat,
    pub source_id: String,
    pub hostname: String,
    pub metadata: HashMap<String, String>,
}

impl StreamInfo {
    pub fn new(
        name: String,
        stream_type: StreamType,
        channel_count: u32,
        nominal_srate: f64,
        channel_format: ChannelFormat,
        source_id: String,
    ) -> Self {
        Self {
            name,
            stream_type,
            channel_count,
            nominal_srate,
            channel_format,
            source_id,
            hostname: String::new(),
            metadata: HashMap::new(),
        }
    }

    /// Create stream info for TTL markers
    pub fn ttl_markers(device_id: &str) -> Self {
        Self::new(
            format!("{}_TTL_Markers", device_id),
            StreamType::Markers,
            1,
            0.0, // Irregular sampling rate
            ChannelFormat::String,
            device_id.to_string(),
        )
    }

    /// Create stream info for Kernel fNIRS data
    pub fn kernel_fnirs(device_id: &str, channel_count: u32) -> Self {
        let mut info = Self::new(
            format!("{}_fNIRS", device_id),
            StreamType::FNIRS,
            channel_count,
            10.0, // Typical fNIRS sampling rate
            ChannelFormat::Float32,
            device_id.to_string(),
        );
        info.metadata
            .insert("unit".to_string(), "optical_density".to_string());
        info
    }

    /// Create stream info for Pupil gaze data
    pub fn pupil_gaze(device_id: &str) -> Self {
        let mut info = Self::new(
            format!("{}_Gaze", device_id),
            StreamType::Gaze,
            3,     // x, y, confidence
            120.0, // Typical eye tracker sampling rate
            ChannelFormat::Float32,
            device_id.to_string(),
        );
        info.metadata
            .insert("unit".to_string(), "normalized".to_string());
        info
    }

    /// Create stream info for Pupil Labs Neon gaze data via LSL
    ///
    /// Neon streams at 200Hz with either 2 channels (basic: x, y) or
    /// 6 channels (with eye-state: x, y, pupil_diameter, eyeball_center xyz)
    pub fn neon_gaze(device_name: &str, with_eye_state: bool) -> Self {
        let channel_count = if with_eye_state { 6 } else { 2 };
        let mut info = Self::new(
            format!("{}_Neon Gaze", device_name),
            StreamType::Gaze,
            channel_count,
            200.0, // Neon streams at 200Hz
            ChannelFormat::Float32,
            device_name.to_string(),
        );
        info.metadata
            .insert("unit".to_string(), "normalized".to_string());
        info.metadata
            .insert("manufacturer".to_string(), "Pupil Labs".to_string());
        info.metadata
            .insert("device".to_string(), "Neon".to_string());
        info.metadata
            .insert("eye_state".to_string(), with_eye_state.to_string());
        info
    }

    /// Create stream info for Pupil Labs Neon event markers via LSL
    ///
    /// Events are string markers sent at irregular intervals
    pub fn neon_events(device_name: &str) -> Self {
        let mut info = Self::new(
            format!("{}_Neon Events", device_name),
            StreamType::Markers,
            1,   // Single string channel
            0.0, // Irregular sampling rate
            ChannelFormat::String,
            device_name.to_string(),
        );
        info.metadata
            .insert("manufacturer".to_string(), "Pupil Labs".to_string());
        info.metadata
            .insert("device".to_string(), "Neon".to_string());
        info
    }

    /// Add metadata to the stream info
    pub fn with_metadata(mut self, key: String, value: String) -> Self {
        self.metadata.insert(key, value);
        self
    }

    /// Set hostname
    pub fn with_hostname(mut self, hostname: String) -> Self {
        self.hostname = hostname;
        self
    }
}

/// LSL data sample with timestamp
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Sample {
    pub data: SampleData,
    pub timestamp: f64,
}

/// Sample data variants for different channel formats
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SampleData {
    Float32(Vec<f32>),
    Float64(Vec<f64>),
    String(Vec<String>),
    Int32(Vec<i32>),
    Int16(Vec<i16>),
    Int8(Vec<i8>),
    Int64(Vec<i64>),
}

impl SampleData {
    /// Get the number of channels in the sample
    pub fn channel_count(&self) -> usize {
        match self {
            SampleData::Float32(data) => data.len(),
            SampleData::Float64(data) => data.len(),
            SampleData::String(data) => data.len(),
            SampleData::Int32(data) => data.len(),
            SampleData::Int16(data) => data.len(),
            SampleData::Int8(data) => data.len(),
            SampleData::Int64(data) => data.len(),
        }
    }

    /// Create a TTL marker sample
    pub fn ttl_marker(marker: String) -> Self {
        SampleData::String(vec![marker])
    }

    /// Create a float32 sample (common for most numeric data)
    pub fn float32(data: Vec<f32>) -> Self {
        SampleData::Float32(data)
    }

    /// Convert to bytes for transmission
    pub fn to_bytes(&self) -> Vec<u8> {
        match self {
            SampleData::Float32(data) => {
                let mut bytes = Vec::with_capacity(data.len() * 4);
                for &value in data {
                    bytes.extend_from_slice(&value.to_le_bytes());
                }
                bytes
            }
            SampleData::Float64(data) => {
                let mut bytes = Vec::with_capacity(data.len() * 8);
                for &value in data {
                    bytes.extend_from_slice(&value.to_le_bytes());
                }
                bytes
            }
            SampleData::String(data) => serde_json::to_vec(data).unwrap_or_default(),
            SampleData::Int32(data) => {
                let mut bytes = Vec::with_capacity(data.len() * 4);
                for &value in data {
                    bytes.extend_from_slice(&value.to_le_bytes());
                }
                bytes
            }
            SampleData::Int16(data) => {
                let mut bytes = Vec::with_capacity(data.len() * 2);
                for &value in data {
                    bytes.extend_from_slice(&value.to_le_bytes());
                }
                bytes
            }
            SampleData::Int8(data) => data.iter().map(|&x| x as u8).collect(),
            SampleData::Int64(data) => {
                let mut bytes = Vec::with_capacity(data.len() * 8);
                for &value in data {
                    bytes.extend_from_slice(&value.to_le_bytes());
                }
                bytes
            }
        }
    }
}

/// Stream status information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamStatus {
    pub active: bool,
    pub sample_count: u64,
    pub last_timestamp: f64,
    pub data_loss: f64, // Percentage
    pub time_correction: f64,
}

impl Default for StreamStatus {
    fn default() -> Self {
        Self {
            active: false,
            sample_count: 0,
            last_timestamp: 0.0,
            data_loss: 0.0,
            time_correction: 0.0,
        }
    }
}

/// LSL configuration for device integration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LslConfig {
    /// Auto-create outlets for bridge devices
    pub auto_create_outlets: bool,
    /// Discover and connect to external streams
    pub auto_discover_inlets: bool,
    /// Maximum number of inlet connections
    pub max_inlets: usize,
    /// Stream discovery timeout in seconds
    pub discovery_timeout: f64,
    /// Buffer size for data buffering
    pub buffer_size: usize,
    /// Enable time synchronization
    pub enable_time_sync: bool,
    /// Stream name filters (regex patterns)
    pub stream_filters: Vec<String>,
    /// Metadata to add to created outlets
    pub outlet_metadata: HashMap<String, String>,
}

impl Default for LslConfig {
    fn default() -> Self {
        Self {
            auto_create_outlets: true,
            auto_discover_inlets: false,
            max_inlets: 10,
            discovery_timeout: 5.0,
            buffer_size: 1000,
            enable_time_sync: true,
            stream_filters: vec![],
            outlet_metadata: HashMap::new(),
        }
    }
}

// ============================================================================
// Earable FRENZ Brainband LSL Types
// ============================================================================

/// All known FRENZ LSL stream suffixes.
/// Streams are named `{DEVICE_ID}_{suffix}` by the Python LSL bridge script.
pub const FRENZ_STREAM_SUFFIXES: &[&str] = &[
    "_EEG_bluetooth",
    "_EEG_raw",
    "_EEG_filtered",
    "_EOG_filtered",
    "_EMG_filtered",
    "_PPG_raw",
    "_IMU_raw",
    "_HR_raw",
    "_SPO2_raw",
    "_EEG_firmware",
    "_PPG_firmware",
    "_IMU_firmware",
    "_HR_firmware",
    "_SPO2_firmware",
    "_POSTURE",
    "_poas",
    "_sleep_stage",
    "_focus",
    "_signal_quality",
    "_alpha",
    "_beta",
    "_theta",
    "_gamma",
    "_delta",
];

/// Category of a FRENZ stream for grouping in the UI
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FrenzStreamCategory {
    RawPhysiological,
    FilteredPhysiological,
    Cardiovascular,
    Motion,
    Metrics,
    Spectral,
}

impl fmt::Display for FrenzStreamCategory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FrenzStreamCategory::RawPhysiological => write!(f, "Raw Physiological"),
            FrenzStreamCategory::FilteredPhysiological => write!(f, "Filtered Physiological"),
            FrenzStreamCategory::Cardiovascular => write!(f, "Cardiovascular"),
            FrenzStreamCategory::Motion => write!(f, "Motion"),
            FrenzStreamCategory::Metrics => write!(f, "Metrics"),
            FrenzStreamCategory::Spectral => write!(f, "Spectral"),
        }
    }
}

/// Information about a specific FRENZ LSL stream
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FrenzStreamInfo {
    /// The full LSL stream name (e.g., "FRENZ_ABC123_EEG_raw")
    pub stream_name: String,
    /// The suffix identifying the stream type (e.g., "_EEG_raw")
    pub suffix: String,
    /// Number of channels
    pub channel_count: u32,
    /// Nominal sampling rate in Hz
    pub nominal_srate: f64,
    /// Channel data format
    pub channel_format: ChannelFormat,
    /// Category for UI grouping
    pub category: FrenzStreamCategory,
    /// Stream UID from LSL discovery
    pub stream_uid: String,
    /// Whether the stream is currently available on the network
    pub available: bool,
}

/// Information about a discovered FRENZ device streaming via LSL
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveredFrenzDevice {
    /// Device name/ID (extracted from stream name prefix)
    pub device_name: String,
    /// Available streams keyed by suffix (e.g., "_EEG_raw" -> FrenzStreamInfo)
    pub streams: HashMap<String, FrenzStreamInfo>,
    /// When this device was first discovered
    pub discovered_at: std::time::SystemTime,
}

/// Determine the category for a FRENZ stream based on its suffix
pub fn frenz_stream_category(suffix: &str) -> FrenzStreamCategory {
    match suffix.to_lowercase().as_str() {
        "_eeg_raw" | "_eeg_bluetooth" => FrenzStreamCategory::RawPhysiological,
        "_ppg_raw" => FrenzStreamCategory::Cardiovascular,
        "_imu_raw" => FrenzStreamCategory::Motion,
        "_eeg_filtered" | "_eog_filtered" | "_emg_filtered" => {
            FrenzStreamCategory::FilteredPhysiological
        }
        "_posture" | "_poas" | "_sleep_stage" | "_focus" | "_signal_quality" => {
            FrenzStreamCategory::Metrics
        }
        "_alpha" | "_beta" | "_theta" | "_gamma" | "_delta" => FrenzStreamCategory::Spectral,
        _ => FrenzStreamCategory::Metrics,
    }
}

/// Errors specific to LSL operations
#[derive(Debug, thiserror::Error)]
pub enum LslError {
    #[error("Failed to create stream outlet: {0}")]
    OutletCreationFailed(String),

    #[error("Failed to create stream inlet: {0}")]
    InletCreationFailed(String),

    #[error("Stream not found: {0}")]
    StreamNotFound(String),

    #[error("Data format mismatch: expected {expected}, got {actual}")]
    DataFormatMismatch { expected: String, actual: String },

    #[error("Buffer overflow: {0}")]
    BufferOverflow(String),

    #[error("Time synchronization failed: {0}")]
    TimeSyncFailed(String),

    #[error("Stream discovery timeout")]
    DiscoveryTimeout,

    #[error("Invalid sample data: {0}")]
    InvalidSampleData(String),

    #[error("LSL library error: {0}")]
    LslLibraryError(String),

    #[error("Neon device not found: {0}")]
    NeonDeviceNotFound(String),

    #[error("Neon stream not available: {0}")]
    NeonStreamNotAvailable(String),

    #[error("FRENZ device not found: {0}")]
    FrenzDeviceNotFound(String),

    #[error("FRENZ stream not available: {0}")]
    FrenzStreamNotAvailable(String),
}

// ============================================================================
// Pupil Labs Neon LSL Types
// ============================================================================

/// Gaze data from Pupil Labs Neon via LSL
///
/// Neon streams gaze at 200Hz when "Stream over LSL" is enabled in Companion App.
/// The stream can have 2 channels (basic) or 6 channels (with eye-state enabled):
/// - Basic: gaze_x, gaze_y (normalized 0-1 coordinates)
/// - With eye-state: gaze_x, gaze_y, pupil_diameter, eyeball_center_x, eyeball_center_y, eyeball_center_z
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct NeonGazeData {
    /// LSL timestamp (seconds since stream start, with time correction applied)
    pub timestamp: f64,
    /// Gaze X coordinate (normalized 0-1, where 0=left, 1=right)
    pub gaze_x: f32,
    /// Gaze Y coordinate (normalized 0-1, where 0=top, 1=bottom)
    pub gaze_y: f32,
    /// Pupil diameter in mm (only present if eye-state is enabled)
    pub pupil_diameter: Option<f32>,
    /// Eyeball center position in 3D space (only present if eye-state is enabled)
    /// Coordinates are in the scene camera coordinate system
    pub eyeball_center: Option<(f32, f32, f32)>,
}

impl NeonGazeData {
    /// Create gaze data from basic 2-channel LSL sample (x, y only)
    pub fn from_basic(timestamp: f64, gaze_x: f32, gaze_y: f32) -> Self {
        Self {
            timestamp,
            gaze_x,
            gaze_y,
            pupil_diameter: None,
            eyeball_center: None,
        }
    }

    /// Create gaze data from full 6-channel LSL sample (with eye-state)
    pub fn from_full(
        timestamp: f64,
        gaze_x: f32,
        gaze_y: f32,
        pupil_diameter: f32,
        eyeball_center_x: f32,
        eyeball_center_y: f32,
        eyeball_center_z: f32,
    ) -> Self {
        Self {
            timestamp,
            gaze_x,
            gaze_y,
            pupil_diameter: Some(pupil_diameter),
            eyeball_center: Some((eyeball_center_x, eyeball_center_y, eyeball_center_z)),
        }
    }

    /// Parse gaze data from LSL Float32 sample
    ///
    /// Handles both 2-channel (basic) and 6-channel (eye-state) formats
    pub fn from_lsl_sample(timestamp: f64, data: &[f32]) -> Result<Self, LslError> {
        match data.len() {
            2 => Ok(Self::from_basic(timestamp, data[0], data[1])),
            6 => Ok(Self::from_full(
                timestamp, data[0], data[1], data[2], data[3], data[4], data[5],
            )),
            n => Err(LslError::InvalidSampleData(format!(
                "Expected 2 or 6 channels for Neon gaze, got {}",
                n
            ))),
        }
    }
}

/// Event marker data from Pupil Labs Neon via LSL
///
/// Neon streams event markers as strings at irregular intervals.
/// These correspond to events triggered in the Companion App or via API.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct NeonEventData {
    /// LSL timestamp (seconds since stream start, with time correction applied)
    pub timestamp: f64,
    /// Event name/marker string
    pub event_name: String,
}

impl NeonEventData {
    pub fn new(timestamp: f64, event_name: String) -> Self {
        Self {
            timestamp,
            event_name,
        }
    }

    /// Parse event data from LSL String sample
    pub fn from_lsl_sample(timestamp: f64, data: &[String]) -> Result<Self, LslError> {
        if data.is_empty() {
            return Err(LslError::InvalidSampleData(
                "Empty string sample for Neon event".to_string(),
            ));
        }
        Ok(Self::new(timestamp, data[0].clone()))
    }
}

/// Information about a discovered Neon device streaming via LSL
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DiscoveredNeonDevice {
    /// Device name (extracted from stream name prefix, e.g., "MyNeon" from "MyNeon_Neon Gaze")
    pub device_name: String,
    /// Network hostname of the device (from LSL stream info), used for REST API connection
    pub hostname: Option<String>,
    /// Whether gaze stream is available
    pub has_gaze_stream: bool,
    /// Whether events stream is available
    pub has_events_stream: bool,
    /// Number of gaze channels; zero when gaze is absent or ambiguous.
    pub gaze_channel_count: u32,
    /// Gaze stream UID (if available)
    pub gaze_stream_uid: Option<String>,
    /// Events stream UID (if available)
    pub events_stream_uid: Option<String>,
    /// Multiple gaze publishers share this device identity; no UID is selected.
    #[serde(default)]
    pub gaze_stream_ambiguous: bool,
    /// Multiple event publishers share this device identity; no UID is selected.
    #[serde(default)]
    pub events_stream_ambiguous: bool,
    /// When this device was first discovered
    pub discovered_at: std::time::SystemTime,
}

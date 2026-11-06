use crate::devices::{DeviceStatus, DeviceType};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum BridgeCommand {
    #[serde(rename = "command")]
    Command {
        device: String,
        action: CommandAction,
        payload: Option<Value>,
        id: Option<String>,
    },
    #[serde(rename = "query")]
    Query {
        target: QueryTarget,
        id: Option<String>,
    },
    #[serde(rename = "subscribe")]
    Subscribe {
        device: Option<String>,
        events: Vec<String>,
    },
    #[serde(rename = "unsubscribe")]
    Unsubscribe {
        device: Option<String>,
        events: Vec<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandAction {
    StartStudyRecording,
    GetStudyRecording,
    StopStudyRecording,
    SendStudyMarker,
    PrepareRecordingUpload,
    UploadStudyRecording,
    ListStudyRecordings,
    DiscoverRecordingStreams,
    StartLslRecording,
    StopLslRecording,
    LslRecordingStatus,
    Connect,
    Disconnect,
    Send,
    SendEvent,      // For sending formatted events (Kernel, Pupil)
    SendPulse,      // For TTL pulse commands
    TestConnection, // For testing device connectivity without maintaining connection
    Configure,
    Status,
    Heartbeat,
    // Neon LSL specific actions
    DiscoverNeon,       // Discover Neon devices streaming via LSL
    DiscoverNeonPhones, // Discover Neon Companion phones via mDNS (name, hardware id, IP)
    ConnectNeonGaze,    // Connect to Neon gaze stream
    ConnectNeonEvents,  // Connect to Neon events stream
    ConnectNeonRest, // Connect Neon REST API by device_name (hostname resolved from discovery cache)
    DisconnectNeon,  // Disconnect from Neon streams
    NeonStatus,      // Get Neon LSL manager status
    // FRENZ LSL specific actions
    DiscoverFrenz,       // Discover FRENZ devices streaming via LSL
    ConnectFrenzStreams, // Connect to selected FRENZ streams
    DisconnectFrenz,     // Disconnect from FRENZ streams
    FrenzStatus,         // Get FRENZ LSL manager status
    SendFrenzMarker,     // Send event marker to FRENZ marker outlet
    // EyeLink specific actions
    ConnectEyeLink,        // Connect to EyeLink tracker
    DisconnectEyeLink,     // Disconnect from EyeLink tracker
    StartEyeLinkRecording, // Start EDF recording
    StopEyeLinkRecording,  // Stop EDF recording
    CalibrateEyeLink,      // Run calibration/validation loop
    SendEyeLinkMessage,    // Write marker to EDF file
    EyeLinkStatus,         // Get EyeLink status
    ConnectEyeLinkGaze,    // Start gaze data streaming
    DisconnectEyeLinkGaze, // Stop gaze data streaming
    CalibrateEyeLinkKey,   // Send key press to calibration (accept/cancel)
    Custom(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueryTarget {
    Devices,
    Device(String),
    Metrics,
    Connections,
    Status,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum BridgeResponse {
    #[serde(rename = "status")]
    Status {
        device: String,
        status: DeviceStatus,
        /// Device metadata (identity, recording state, battery…) so clients can
        /// verify *which* hardware answered, not just that something did.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        info: Option<Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        timestamp: u64,
    },
    #[serde(rename = "data")]
    Data {
        device: String,
        payload: Value,
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        timestamp: u64,
    },
    #[serde(rename = "error")]
    Error {
        device: Option<String>,
        message: String,
        code: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        timestamp: u64,
    },
    #[serde(rename = "event")]
    Event {
        device: Option<String>,
        event: String,
        payload: Value,
        timestamp: u64,
    },
    #[serde(rename = "query_result")]
    QueryResult {
        id: Option<String>,
        data: Value,
        timestamp: u64,
    },
}

impl BridgeResponse {
    pub fn error(message: String, id: Option<String>) -> Self {
        Self::Error {
            device: None,
            message,
            code: None,
            id,
            timestamp: Self::timestamp(),
        }
    }

    pub fn device_error(device: String, message: String, id: Option<String>) -> Self {
        Self::Error {
            device: Some(device),
            message,
            code: None,
            id,
            timestamp: Self::timestamp(),
        }
    }

    pub fn data(device: String, payload: Value, id: Option<String>) -> Self {
        Self::Data {
            device,
            payload,
            id,
            timestamp: Self::timestamp(),
        }
    }

    pub fn status(device: String, status: DeviceStatus, id: Option<String>) -> Self {
        Self::Status {
            device,
            status,
            info: None,
            id,
            timestamp: Self::timestamp(),
        }
    }

    /// Status response carrying the device's `get_info().metadata`.
    pub fn status_with_info(
        device: String,
        status: DeviceStatus,
        info: Value,
        id: Option<String>,
    ) -> Self {
        Self::Status {
            device,
            status,
            info: Some(info),
            id,
            timestamp: Self::timestamp(),
        }
    }

    /// Error response for a `DeviceError`, carrying its machine-readable code
    /// so clients can branch on `phone_busy` / `recording_not_owned` etc.
    /// instead of matching on message text.
    pub fn device_error_from(
        device: String,
        error: &crate::devices::DeviceError,
        id: Option<String>,
    ) -> Self {
        Self::Error {
            device: Some(device),
            message: error.to_string(),
            code: Some(error.code().to_string()),
            id,
            timestamp: Self::timestamp(),
        }
    }

    pub fn event(device: Option<String>, event: String, payload: Value) -> Self {
        Self::Event {
            device,
            event,
            payload,
            timestamp: Self::timestamp(),
        }
    }

    pub fn query_result(id: Option<String>, data: Value) -> Self {
        Self::QueryResult {
            id,
            data,
            timestamp: Self::timestamp(),
        }
    }

    fn timestamp() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64
    }
}

pub struct MessageHandler;

impl MessageHandler {
    pub fn parse_command(data: &str) -> Result<BridgeCommand, String> {
        serde_json::from_str(data).map_err(|e| format!("Failed to parse command: {}", e))
    }

    pub fn serialize_response(response: &BridgeResponse) -> Result<String, String> {
        serde_json::to_string(response).map_err(|e| format!("Failed to serialize response: {}", e))
    }

    pub fn validate_device_type(device: &str) -> Option<DeviceType> {
        match device.to_lowercase().as_str() {
            "ttl" => Some(DeviceType::TTL),
            "kernel" => Some(DeviceType::Kernel),
            "pupil" => Some(DeviceType::Pupil),
            "lsl" => Some(DeviceType::LSL),
            "neon_lsl" => Some(DeviceType::LSL),  // Neon via LSL
            "frenz" => Some(DeviceType::LSL),     // FRENZ unified connect
            "frenz_lsl" => Some(DeviceType::LSL), // FRENZ via LSL
            "eyelink" => Some(DeviceType::EyeLink),
            "mock" => Some(DeviceType::Mock),
            _ => None,
        }
    }
}

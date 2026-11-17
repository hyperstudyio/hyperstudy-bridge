use super::{Device, DeviceConfig, DeviceError, DeviceInfo, DeviceStatus, DeviceType};
use crate::usb_monitor::{TTL_USB_PID, TTL_USB_VID};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serialport::{self, SerialPort};
use std::io::{Read, Write};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::time::sleep;
use tracing::{error, info, warn};

const DEFAULT_PULSE_DURATION_MS: u64 = 10;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TtlConfig {
    pub port_name: String,
    pub baud_rate: u32,
    pub pulse_duration_ms: u64,
}

impl Default for TtlConfig {
    fn default() -> Self {
        Self {
            port_name: String::new(),
            baud_rate: 115200,
            pulse_duration_ms: DEFAULT_PULSE_DURATION_MS,
        }
    }
}

pub struct TtlDevice {
    /// Serial port wrapped in Arc<Mutex> to allow cloning into spawn_blocking tasks.
    /// Using std::sync::Mutex is correct here because serial I/O is inherently blocking
    /// and we use spawn_blocking to avoid blocking the async runtime.
    port: Option<Arc<Mutex<Box<dyn SerialPort>>>>,
    port_name: String,
    status: DeviceStatus,
    config: TtlConfig,
    device_config: DeviceConfig,
}

impl std::fmt::Debug for TtlDevice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TtlDevice")
            .field("port_name", &self.port_name)
            .field("status", &self.status)
            .field("config", &self.config)
            .field("device_config", &self.device_config)
            .finish()
    }
}

impl TtlDevice {
    pub fn new(port_name: String) -> Self {
        Self {
            port: None,
            port_name: port_name.clone(),
            status: DeviceStatus::Disconnected,
            config: TtlConfig {
                port_name,
                ..Default::default()
            },
            device_config: DeviceConfig::default(),
        }
    }

    /// Check if an error message indicates the device has been physically disconnected.
    ///
    /// This is used as a passive fallback to detect device disconnection when USB
    /// monitoring is not available or misses the event.
    fn is_connection_lost_error(error_msg: &str) -> bool {
        // Common error messages indicating physical disconnection across platforms
        error_msg.contains("Broken pipe")
            || error_msg.contains("Resource busy")
            || error_msg.contains("No such device")
            || error_msg.contains("No such file or directory")
            || error_msg.contains("Input/output error")
            || error_msg.contains("Device not configured")
            || error_msg.contains("Operation not supported")
            || error_msg.contains("Bad file descriptor")
            || error_msg.contains("Access is denied")
            || error_msg.contains("The device is not connected")
            || error_msg.contains("The system cannot find the file specified")
    }

    /// Mark the device as disconnected due to a communication error.
    ///
    /// This is called when we detect an I/O error that indicates the physical
    /// device is no longer present.
    fn mark_disconnected(&mut self, reason: &str) {
        warn!(
            device = "ttl",
            "TTL device disconnected: {} - marking as Error status", reason
        );
        self.port = None;
        self.status = DeviceStatus::Error;
    }

    /// Debug function to list ALL serial ports with detailed USB info
    pub fn list_all_ports_debug() -> Result<Vec<serde_json::Value>, DeviceError> {
        let ports =
            serialport::available_ports().map_err(|e| DeviceError::SerialError(e.to_string()))?;

        let mut all_ports = Vec::new();

        for port in ports {
            let port_info = match &port.port_type {
                serialport::SerialPortType::UsbPort(usb_info) => {
                    serde_json::json!({
                        "port": port.port_name,
                        "type": "USB",
                        "vid": format!("0x{:04X}", usb_info.vid),
                        "pid": format!("0x{:04X}", usb_info.pid),
                        "serial_number": usb_info.serial_number.as_ref().unwrap_or(&"None".to_string()),
                        "manufacturer": usb_info.manufacturer.as_ref().unwrap_or(&"None".to_string()),
                        "product": usb_info.product.as_ref().unwrap_or(&"None".to_string()),
                    })
                }
                serialport::SerialPortType::BluetoothPort => {
                    serde_json::json!({
                        "port": port.port_name,
                        "type": "Bluetooth",
                    })
                }
                serialport::SerialPortType::PciPort => {
                    serde_json::json!({
                        "port": port.port_name,
                        "type": "PCI",
                    })
                }
                serialport::SerialPortType::Unknown => {
                    serde_json::json!({
                        "port": port.port_name,
                        "type": "Unknown",
                    })
                }
            };
            all_ports.push(port_info);
        }

        info!(
            device = "ttl",
            "Found {} serial ports total",
            all_ports.len()
        );
        Ok(all_ports)
    }

    /// List TTL devices by filtering on VID/PID and return detailed info
    /// Returns a JSON object with 'devices' array and 'autoSelected' port if only one device found
    pub fn list_ttl_devices() -> Result<serde_json::Value, DeviceError> {
        let ports =
            serialport::available_ports().map_err(|e| DeviceError::SerialError(e.to_string()))?;

        let mut ttl_devices = Vec::new();

        for port in ports {
            if let serialport::SerialPortType::UsbPort(usb_info) = &port.port_type {
                // Check if this is an Adafruit RP2040 (our TTL device)
                // On macOS, skip /dev/tty.* ports (duplicates of /dev/cu.*)
                if usb_info.vid == TTL_USB_VID
                    && usb_info.pid == TTL_USB_PID
                    && !port.port_name.starts_with("/dev/tty.")
                {
                    let device_info = serde_json::json!({
                        "port": port.port_name,
                        "serial_number": usb_info.serial_number.as_ref().unwrap_or(&"Unknown".to_string()),
                        "manufacturer": usb_info.manufacturer.as_ref().unwrap_or(&"Unknown".to_string()),
                        "product": usb_info.product.as_ref().unwrap_or(&"Unknown".to_string()),
                        "vid": format!("0x{:04X}", usb_info.vid),
                        "pid": format!("0x{:04X}", usb_info.pid),
                    });
                    ttl_devices.push(device_info);
                    info!(
                        device = "ttl",
                        "Found TTL device: {} (S/N: {})",
                        port.port_name,
                        usb_info
                            .serial_number
                            .as_ref()
                            .unwrap_or(&"Unknown".to_string())
                    );
                }
            }
        }

        let result = if ttl_devices.is_empty() {
            info!(
                device = "ttl",
                "No TTL devices found (VID: 0x{:04X}, PID: 0x{:04X})", TTL_USB_VID, TTL_USB_PID
            );
            serde_json::json!({
                "devices": ttl_devices,
                "autoSelected": null,
                "count": 0
            })
        } else if ttl_devices.len() == 1 {
            let auto_port = ttl_devices[0]["port"].as_str().unwrap_or("");
            info!(
                device = "ttl",
                "Auto-selecting single TTL device: {}", auto_port
            );
            serde_json::json!({
                "devices": ttl_devices,
                "autoSelected": auto_port,
                "count": 1
            })
        } else {
            info!(
                device = "ttl",
                "Found {} TTL devices - manual selection required",
                ttl_devices.len()
            );
            serde_json::json!({
                "devices": ttl_devices,
                "autoSelected": null,
                "count": ttl_devices.len()
            })
        };

        Ok(result)
    }

    /// Send a TTL pulse using spawn_blocking to avoid blocking the async runtime.
    ///
    /// Serial I/O is inherently blocking, so we offload it to Tokio's blocking thread pool.
    /// The command includes the pulse duration so the firmware knows how long to hold
    /// GPIO HIGH (e.g., "PULSE 10\n" for a 10ms pulse).
    ///
    /// If `duration_override` is provided, it is used instead of the configured default.
    /// This allows callers (e.g. HyperStudy) to specify a per-pulse duration via
    /// `PULSE <ms>` without reconfiguring the device.
    async fn send_pulse(&mut self, duration_override: Option<u64>) -> Result<(), DeviceError> {
        if let Some(ref port_arc) = self.port {
            let port_clone = Arc::clone(port_arc);
            let pulse_duration = duration_override.unwrap_or(self.config.pulse_duration_ms);

            // Format command with duration (firmware parses duration after GPIO toggle)
            let pulse_command = format!("PULSE {}\n", pulse_duration);
            let command_bytes = pulse_command.into_bytes();

            // Measure latency around the blocking operation
            let start = Instant::now();

            // Run blocking serial I/O on the blocking thread pool with panic recovery
            let result = tokio::task::spawn_blocking(move || {
                // Wrap in catch_unwind to prevent mutex poisoning on panic
                let panic_result = catch_unwind(AssertUnwindSafe(|| {
                    let mut port = port_clone.lock().map_err(|e| {
                        error!(device = "ttl", "Mutex poisoned in send_pulse: {}", e);
                        DeviceError::CommunicationError(
                            "Mutex poisoned - device needs reset".to_string(),
                        )
                    })?;
                    port.write_all(&command_bytes)
                        .map_err(|e| DeviceError::CommunicationError(e.to_string()))?;
                    port.flush()
                        .map_err(|e| DeviceError::CommunicationError(e.to_string()))?;
                    Ok::<(), DeviceError>(())
                }));

                match panic_result {
                    Ok(result) => result,
                    Err(panic_info) => {
                        let msg = if let Some(s) = panic_info.downcast_ref::<&str>() {
                            s.to_string()
                        } else if let Some(s) = panic_info.downcast_ref::<String>() {
                            s.clone()
                        } else {
                            "Unknown panic in serial operation".to_string()
                        };
                        error!(device = "ttl", "Panic caught in send_pulse: {}", msg);
                        Err(DeviceError::CommunicationError(format!(
                            "Serial operation panicked: {}",
                            msg
                        )))
                    }
                }
            })
            .await
            .map_err(|e| DeviceError::CommunicationError(format!("Task join error: {}", e)))?;

            let latency = start.elapsed();

            info!(device = "ttl", "TTL pulse sent with latency: {:?}", latency);

            // Check for compliance with <1ms requirement
            if latency > Duration::from_millis(1) {
                warn!(
                    device = "ttl",
                    "TTL pulse latency exceeded 1ms: {:?} - Performance requirement not met!",
                    latency
                );
            } else if latency > Duration::from_micros(500) {
                warn!(
                    device = "ttl",
                    "TTL pulse latency approaching limit: {:?}", latency
                );
            }

            // Check if the error indicates a physical disconnection
            if let Err(ref e) = result {
                if let DeviceError::CommunicationError(msg) = e {
                    if Self::is_connection_lost_error(msg) {
                        self.mark_disconnected(msg);
                    }
                }
            }

            result?;

            sleep(Duration::from_millis(pulse_duration)).await;

            Ok(())
        } else {
            Err(DeviceError::NotConnected)
        }
    }
}

#[async_trait]
impl Device for TtlDevice {
    async fn connect(&mut self) -> Result<(), DeviceError> {
        info!(
            device = "ttl",
            "Connecting to TTL device on port: {}", self.port_name
        );

        self.status = DeviceStatus::Connecting;

        let port_name = self.port_name.clone();
        let baud_rate = self.config.baud_rate;

        // Run all blocking serial I/O on the blocking thread pool with panic recovery
        let connect_result = tokio::task::spawn_blocking(move || {
            // Wrap in catch_unwind to prevent panic propagation
            let panic_result = catch_unwind(AssertUnwindSafe(|| {
                let mut port = serialport::new(&port_name, baud_rate)
                    .timeout(Duration::from_millis(500))
                    .open()
                    .map_err(|e| {
                        DeviceError::ConnectionFailed(format!("Failed to open serial port: {}", e))
                    })?;

                // Drain any stale data in the serial buffer before validation
                // (e.g., unread PULSE responses from a previous session)
                let mut drain_buf = [0u8; 256];
                while port.read(&mut drain_buf).unwrap_or(0) > 0 {}

                // Validate connection by sending TEST command
                port.write_all(b"TEST\n").map_err(|e| {
                    DeviceError::ConnectionFailed(format!("Failed to send TEST command: {}", e))
                })?;

                port.flush()
                    .map_err(|e| DeviceError::ConnectionFailed(format!("Failed to flush: {}", e)))?;

                // Small delay for device to respond (blocking sleep is fine in spawn_blocking)
                std::thread::sleep(Duration::from_millis(100));

                // Read response
                let mut buffer = vec![0u8; 256];
                match port.read(&mut buffer) {
                    Ok(bytes_read) => {
                        buffer.truncate(bytes_read);
                        let response = String::from_utf8_lossy(&buffer).trim().to_string();
                        if response.is_empty() {
                            return Err(DeviceError::ConnectionFailed(
                                "Device did not respond to TEST command. Is this the correct device?"
                                    .to_string(),
                            ));
                        }
                        info!(device = "ttl", "TTL device validated. Response: {}", response);
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {
                        return Err(DeviceError::ConnectionFailed(
                            "Device timeout - no response to TEST command. Check device connection."
                                .to_string(),
                        ));
                    }
                    Err(e) => {
                        return Err(DeviceError::ConnectionFailed(format!(
                            "Failed to read validation response: {}",
                            e
                        )));
                    }
                }

                // Reset timeout to normal operation value
                port.set_timeout(Duration::from_millis(100)).map_err(|e| {
                    DeviceError::ConnectionFailed(format!("Failed to configure port: {}", e))
                })?;

                Ok(port)
            }));

            match panic_result {
                Ok(result) => result,
                Err(panic_info) => {
                    let msg = if let Some(s) = panic_info.downcast_ref::<&str>() {
                        s.to_string()
                    } else if let Some(s) = panic_info.downcast_ref::<String>() {
                        s.clone()
                    } else {
                        "Unknown panic during connection".to_string()
                    };
                    error!(device = "ttl", "Panic caught in connect: {}", msg);
                    Err(DeviceError::ConnectionFailed(format!("Connection panicked: {}", msg)))
                }
            }
        })
        .await
        .map_err(|e| DeviceError::ConnectionFailed(format!("Task join error: {}", e)))?;

        match connect_result {
            Ok(port) => {
                self.port = Some(Arc::new(Mutex::new(port)));
                self.status = DeviceStatus::Connected;
                info!(device = "ttl", "Successfully connected to TTL device");
                Ok(())
            }
            Err(e) => {
                self.status = DeviceStatus::Error;
                Err(e)
            }
        }
    }

    async fn disconnect(&mut self) -> Result<(), DeviceError> {
        info!(device = "ttl", "Disconnecting TTL device");

        if let Some(port_arc) = self.port.take() {
            // Run blocking flush on blocking thread pool
            tokio::task::spawn_blocking(move || {
                if let Ok(mut port) = port_arc.lock() {
                    let _ = port.flush(); // Best effort flush on disconnect
                }
            })
            .await
            .map_err(|e| DeviceError::CommunicationError(format!("Task join error: {}", e)))?;
        }

        self.status = DeviceStatus::Disconnected;
        Ok(())
    }

    async fn send(&mut self, data: &[u8]) -> Result<(), DeviceError> {
        // Route any PULSE command to send_pulse().
        // If the caller included an inline duration (e.g. "PULSE 50"), parse and
        // use it as a one-shot override; otherwise use the configured default.
        if data == b"PULSE" || data == b"PULSE\n" {
            self.send_pulse(None).await
        } else if data.starts_with(b"PULSE ") {
            let duration_override = std::str::from_utf8(data)
                .ok()
                .and_then(|s| s.trim().strip_prefix("PULSE "))
                .and_then(|d| d.parse::<u64>().ok());
            self.send_pulse(duration_override).await
        } else if let Some(ref port_arc) = self.port {
            let port_clone = Arc::clone(port_arc);
            let data_owned = data.to_vec();

            // Run blocking serial I/O on the blocking thread pool with panic recovery
            let result = tokio::task::spawn_blocking(move || {
                let panic_result = catch_unwind(AssertUnwindSafe(|| {
                    let mut port = port_clone.lock().map_err(|e| {
                        error!(device = "ttl", "Mutex poisoned in send: {}", e);
                        DeviceError::CommunicationError(
                            "Mutex poisoned - device needs reset".to_string(),
                        )
                    })?;
                    port.write_all(&data_owned)
                        .map_err(|e| DeviceError::CommunicationError(e.to_string()))?;
                    port.flush()
                        .map_err(|e| DeviceError::CommunicationError(e.to_string()))?;
                    Ok::<(), DeviceError>(())
                }));

                match panic_result {
                    Ok(result) => result,
                    Err(panic_info) => {
                        let msg = if let Some(s) = panic_info.downcast_ref::<&str>() {
                            s.to_string()
                        } else if let Some(s) = panic_info.downcast_ref::<String>() {
                            s.clone()
                        } else {
                            "Unknown panic in send operation".to_string()
                        };
                        error!(device = "ttl", "Panic caught in send: {}", msg);
                        Err(DeviceError::CommunicationError(format!(
                            "Send operation panicked: {}",
                            msg
                        )))
                    }
                }
            })
            .await
            .map_err(|e| DeviceError::CommunicationError(format!("Task join error: {}", e)))?;

            // Check if the error indicates a physical disconnection
            if let Err(ref e) = result {
                if let DeviceError::CommunicationError(msg) = e {
                    if Self::is_connection_lost_error(msg) {
                        self.mark_disconnected(msg);
                    }
                }
            }

            result
        } else {
            Err(DeviceError::NotConnected)
        }
    }

    fn get_info(&self) -> DeviceInfo {
        DeviceInfo {
            id: format!("ttl_{}", self.port_name.replace('/', "_")),
            name: format!("TTL Pulse Generator ({})", self.port_name),
            device_type: DeviceType::TTL,
            status: self.status,
            metadata: serde_json::json!({
                "port": self.port_name,
                "baud_rate": self.config.baud_rate,
                "pulse_duration_ms": self.config.pulse_duration_ms,
            }),
        }
    }

    fn get_status(&self) -> DeviceStatus {
        self.status
    }

    fn configure(&mut self, config: DeviceConfig) -> Result<(), DeviceError> {
        self.device_config = config;

        if let Some(custom) = self.device_config.custom_settings.as_object() {
            if let Some(port) = custom.get("port_name").and_then(|v| v.as_str()) {
                self.config.port_name = port.to_string();
                self.port_name = port.to_string();
            }

            if let Some(baud) = custom.get("baud_rate").and_then(|v| v.as_u64()) {
                self.config.baud_rate = baud as u32;
            }

            if let Some(duration) = custom.get("pulse_duration_ms").and_then(|v| v.as_u64()) {
                self.config.pulse_duration_ms = duration;
            }
        }

        Ok(())
    }

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}

#[cfg(test)]
#[path = "ttl_tests.rs"]
mod ttl_tests;

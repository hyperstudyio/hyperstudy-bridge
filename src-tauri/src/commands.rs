use crate::bridge::state::{DeviceStatusEvent, DisconnectTarget};
use crate::bridge::{AppState, BridgeServer};
use crate::devices::{kernel::KernelDevice, mock::MockDevice, pupil::PupilDevice, ttl::TtlDevice};
use crate::devices::{Device, DeviceInfo, DeviceStatus};
use crate::logging::{get_all_logs, LogEntry};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager, State};
use tracing::{error, info};

#[derive(Debug, Serialize, Deserialize)]
pub struct CommandResult<T> {
    pub success: bool,
    pub data: Option<T>,
    pub error: Option<String>,
}

impl<T> CommandResult<T> {
    pub fn success(data: T) -> Self {
        Self {
            success: true,
            data: Some(data),
            error: None,
        }
    }

    pub fn error(error: String) -> Self {
        Self {
            success: false,
            data: None,
            error: Some(error),
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct BridgeStatus {
    pub running: bool,
    pub port: u16,
    pub connected_clients: usize,
    pub device_count: usize,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SerialPortInfo {
    pub name: String,
    pub manufacturer: Option<String>,
    pub product: Option<String>,
}

// Bridge server management commands

#[tauri::command]
pub async fn start_bridge_server(
    app_handle: AppHandle,
    state: State<'_, Arc<AppState>>,
) -> Result<CommandResult<String>, ()> {
    info!("Starting bridge server...");

    let state_clone = state.inner().clone();
    let app_handle_clone = app_handle.clone();

    tokio::spawn(async move {
        let mut server = BridgeServer::new(state_clone, app_handle_clone);
        if let Err(e) = server.start().await {
            error!("Bridge server error: {}", e);
        }
    });

    Ok(CommandResult::success(
        "Bridge server started on port 9000".to_string(),
    ))
}

#[tauri::command]
pub async fn stop_bridge_server(
    state: State<'_, Arc<AppState>>,
) -> Result<CommandResult<String>, ()> {
    info!("Stopping bridge server...");

    // Collect device IDs first to avoid holding lock across await
    let device_ids: Vec<String> = {
        let devices = state.devices.read().await;
        devices.keys().cloned().collect()
    };

    // Disconnect each device individually (lock released between iterations)
    for device_id in device_ids {
        if let Some(device_lock) = state.get_device(&device_id).await {
            let mut device = device_lock.write().await;
            let _ = device.disconnect().await;
            drop(device); // Explicitly release lock before next iteration
        }
    }

    // Clear all connections
    state.connections.clear();

    Ok(CommandResult::success("Bridge server stopped".to_string()))
}

#[tauri::command]
pub async fn get_bridge_status(state: State<'_, Arc<AppState>>) -> Result<BridgeStatus, ()> {
    let devices = state.devices.read().await;

    Ok(BridgeStatus {
        running: true, // TODO: Track actual server state
        port: 9000,
        connected_clients: state.connections.len(),
        device_count: devices.len(),
    })
}

// Device management commands

#[tauri::command]
pub async fn connect_device(
    device_type: String,
    config: serde_json::Value,
    state: State<'_, Arc<AppState>>,
    app_handle: AppHandle,
) -> Result<CommandResult<DeviceInfo>, ()> {
    info!(
        "Connecting device: {} with config: {:?}",
        device_type, config
    );

    // Track if this is a TTL device so we can set up callbacks after connection
    let is_ttl_device = device_type == "ttl";

    let mut device: Box<dyn Device> = match device_type.as_str() {
        "ttl" => {
            let port = config
                .get("port")
                .and_then(|v| v.as_str())
                .unwrap_or("/dev/cu.usbmodem101");
            // Note: Performance callback is set up AFTER successful connection
            // to avoid race conditions where the callback fires before the device
            // is registered in state
            let mut ttl_device = TtlDevice::new(port.to_string());
            if let Some(duration) = config.get("pulse_duration_ms").and_then(|v| v.as_u64()) {
                let device_config = crate::devices::DeviceConfig {
                    custom_settings: json!({ "pulse_duration_ms": duration }),
                    ..Default::default()
                };
                let _ = ttl_device.configure(device_config);
            }
            Box::new(ttl_device)
        }
        "kernel" => {
            let ip = config
                .get("ip")
                .and_then(|v| v.as_str())
                .unwrap_or("192.168.1.100");
            Box::new(KernelDevice::new(ip.to_string()))
        }
        "pupil" => {
            let url = config
                .get("url")
                .and_then(|v| v.as_str())
                .unwrap_or("neon.local:8080");
            Box::new(PupilDevice::new(url.to_string()))
        }
        "mock" => Box::new(MockDevice::new(
            format!("mock_{}", uuid::Uuid::new_v4()),
            "Mock Device".to_string(),
        )),
        _ => {
            return Ok(CommandResult::error(format!(
                "Unknown device type: {}",
                device_type
            )));
        }
    };

    let device_id = device.get_info().id.clone();

    // Ticket at start so this native connect is ordered with WebSocket
    // connects: whichever started last is the one that gets registered.
    let ticket = state.begin_connect(&device_id).await;
    let mut attempt =
        crate::bridge::state::ConnectAttempt::new(state.inner().clone(), &device_id, ticket);

    match device.connect().await {
        Ok(_) => {
            let info = device.get_info();

            // Record successful connection attempt
            state.record_connection_attempt(&device_id, true).await;

            let never_closed = std::sync::atomic::AtomicBool::new(false);
            match state
                .add_device_if_latest(device_id.clone(), device, ticket, &never_closed)
                .await
            {
                Ok(Some(replaced)) => {
                    attempt.registered();
                    // Don't leave the replaced device's hardware connection open;
                    // done in the background so this response is not delayed.
                    tokio::spawn(async move {
                        let _ = replaced.write().await.disconnect().await;
                    });
                }
                Ok(None) => attempt.registered(),
                Err(mut orphan) => {
                    let _ = orphan.disconnect().await;
                    return Ok(CommandResult::error(format!(
                        "Connect for {} was superseded by a newer connect",
                        device_id
                    )));
                }
            }

            // Set up performance callback for TTL devices AFTER registration
            // This prevents race conditions where the callback fires before
            // the device is in state
            if is_ttl_device {
                if let Some(device_lock) = state.get_device(&device_id).await {
                    let mut device = device_lock.write().await;
                    // Downcast to TtlDevice to set callback
                    if let Some(any_ref) = device.as_any_mut() {
                        if let Some(ttl_device) = any_ref.downcast_mut::<TtlDevice>() {
                            let state_clone = state.inner().clone();
                            let device_id_clone = device_id.clone();
                            ttl_device.set_performance_callback(
                                move |_device_id, latency, bytes_sent, bytes_received| {
                                    let state_clone = state_clone.clone();
                                    let device_id = device_id_clone.clone();
                                    tokio::spawn(async move {
                                        state_clone
                                            .record_device_operation(
                                                &device_id,
                                                latency,
                                                bytes_sent,
                                                bytes_received,
                                            )
                                            .await;
                                    });
                                },
                            );
                        }
                    }
                }
            }

            // Emit status update
            app_handle
                .emit(
                    "device_status_changed",
                    json!({
                        "device": device_id,
                        "status": "Connected"
                    }),
                )
                .unwrap_or_else(|e| error!("Failed to emit event: {}", e));

            Ok(CommandResult::success(info))
        }
        Err(e) => {
            error!("Failed to connect device: {}", e);

            // Record failed connection attempt
            state.record_connection_attempt(&device_id, false).await;
            state.record_device_error(&device_id, &e.to_string()).await;

            Ok(CommandResult::error(e.to_string()))
        }
    }
}

#[tauri::command]
pub async fn disconnect_device(
    device_id: String,
    state: State<'_, Arc<AppState>>,
    app_handle: AppHandle,
) -> Result<CommandResult<String>, ()> {
    info!("Disconnecting device: {}", device_id);

    // Same discipline as the WebSocket Disconnect handler: select the device
    // under the registry lock and unregister only that exact device, so a
    // HyperStudy reconnect that registers a replacement while this disconnect
    // is in flight is never unregistered by it.
    let target = state.device_for_disconnect(&device_id, None).await;
    if let DisconnectTarget::Device(device_lock) = target {
        let mut device = device_lock.write().await;
        let device_type = device.get_info().device_type;
        match device.disconnect().await {
            Ok(_) => {
                drop(device);
                if state.remove_device_if_same(&device_id, &device_lock).await {
                    state.broadcast_device_status(DeviceStatusEvent::disconnected(
                        device_id.clone(),
                        device_type,
                        "disconnected from the Bridge app",
                    ));
                } else {
                    info!(
                        "{} was re-registered while disconnecting; leaving the newer device",
                        device_id
                    );
                }

                // Emit status update
                app_handle
                    .emit(
                        "device_status_changed",
                        json!({
                            "device": device_id,
                            "status": "Disconnected"
                        }),
                    )
                    .unwrap_or_else(|e| error!("Failed to emit event: {}", e));

                Ok(CommandResult::success(format!(
                    "Device {} disconnected",
                    device_id
                )))
            }
            Err(e) => {
                error!("Failed to disconnect device: {}", e);
                Ok(CommandResult::error(e.to_string()))
            }
        }
    } else {
        Ok(CommandResult::error(format!(
            "Device {} not found",
            device_id
        )))
    }
}

#[tauri::command]
pub async fn send_device_command(
    device_id: String,
    command: String,
    state: State<'_, Arc<AppState>>,
) -> Result<CommandResult<String>, ()> {
    info!("Sending command to device {}: {}", device_id, command);

    if let Some(device_lock) = state.get_device(&device_id).await {
        let mut device = device_lock.write().await;
        match device.send(command.as_bytes()).await {
            Ok(_) => Ok(CommandResult::success("Command sent".to_string())),
            Err(e) => {
                error!("Failed to send command: {}", e);

                // Record device error
                state.record_device_error(&device_id, &e.to_string()).await;

                Ok(CommandResult::error(e.to_string()))
            }
        }
    } else {
        Ok(CommandResult::error(format!(
            "Device {} not found",
            device_id
        )))
    }
}

// TTL-specific high-performance command
#[tauri::command]
pub async fn send_ttl_pulse(
    port: Option<String>,
    pulse_duration_ms: Option<u64>,
    state: State<'_, Arc<AppState>>,
    app_handle: AppHandle,
) -> Result<CommandResult<u64>, ()> {
    let start_time = std::time::Instant::now();

    // Try to find existing TTL device or use provided port
    let device_id = "ttl".to_string();

    if let Some(device_lock) = state.get_device(&device_id).await {
        let mut device = device_lock.write().await;
        // Apply pulse duration to the already-connected device if provided,
        // so the test pulse respects the current frontend config even if it
        // changed after the device was connected.
        if let Some(duration) = pulse_duration_ms {
            let device_config = crate::devices::DeviceConfig {
                custom_settings: json!({ "pulse_duration_ms": duration }),
                ..Default::default()
            };
            let _ = device.configure(device_config);
        }
        match device.send(b"PULSE\n").await {
            Ok(_) => {
                let latency_us = start_time.elapsed().as_micros() as u64;

                // Emit performance metric
                app_handle
                    .emit(
                        "performance_metrics",
                        json!({
                            "device": "ttl",
                            "operation": "pulse",
                            "latency_us": latency_us
                        }),
                    )
                    .unwrap_or_else(|e| error!("Failed to emit event: {}", e));

                Ok(CommandResult::success(latency_us))
            }
            Err(e) => {
                error!("Failed to send TTL pulse: {}", e);

                // Record device error
                state.record_device_error(&device_id, &e.to_string()).await;

                Ok(CommandResult::error(e.to_string()))
            }
        }
    } else if let Some(port) = port {
        // Quick connect and pulse for lowest latency
        info!(
            "Sending TTL pulse via temporary connection on port: {}",
            port
        );
        let mut device = TtlDevice::new(port.clone());
        if let Some(duration) = pulse_duration_ms {
            let device_config = crate::devices::DeviceConfig {
                custom_settings: json!({ "pulse_duration_ms": duration }),
                ..Default::default()
            };
            let _ = device.configure(device_config);
        }
        match device.connect().await {
            Ok(_) => match device.send(b"PULSE\n").await {
                Ok(_) => {
                    let latency_us = start_time.elapsed().as_micros() as u64;
                    let _ = device.disconnect().await;
                    info!(
                        "TTL pulse sent successfully via {} (latency: {}µs)",
                        port, latency_us
                    );
                    Ok(CommandResult::success(latency_us))
                }
                Err(e) => {
                    let _ = device.disconnect().await;
                    error!("Failed to send TTL pulse: {}", e);
                    Ok(CommandResult::error(e.to_string()))
                }
            },
            Err(e) => {
                error!("Failed to connect to TTL device on {}: {}", port, e);
                Ok(CommandResult::error(e.to_string()))
            }
        }
    } else {
        Ok(CommandResult::error(
            "TTL device not connected and no port specified".to_string(),
        ))
    }
}

#[tauri::command]
pub async fn list_serial_ports() -> Result<Vec<SerialPortInfo>, ()> {
    // Run blocking serial port enumeration on the blocking thread pool
    let result = tokio::task::spawn_blocking(|| {
        serialport::available_ports().map(|ports| {
            ports
                .into_iter()
                .map(|p| {
                    let (manufacturer, product) = match p.port_type {
                        serialport::SerialPortType::UsbPort(info) => {
                            (info.manufacturer, info.product)
                        }
                        _ => (None, None),
                    };

                    SerialPortInfo {
                        name: p.port_name,
                        manufacturer,
                        product,
                    }
                })
                .collect()
        })
    })
    .await;

    match result {
        Ok(Ok(ports)) => Ok(ports),
        Ok(Err(e)) => {
            error!("Failed to list serial ports: {}", e);
            Ok(vec![])
        }
        Err(e) => {
            error!("Task error listing serial ports: {}", e);
            Ok(vec![])
        }
    }
}

#[tauri::command]
pub async fn discover_devices() -> Result<Vec<DeviceInfo>, ()> {
    // Run blocking serial port enumeration on the blocking thread pool
    let result = tokio::task::spawn_blocking(|| {
        let mut discovered = Vec::new();

        // Check for serial devices (TTL)
        if let Ok(ports) = serialport::available_ports() {
            for port in ports {
                if let serialport::SerialPortType::UsbPort(info) = port.port_type {
                    if info.product.as_deref() == Some("RP2040")
                        || info.manufacturer.as_deref() == Some("Adafruit")
                    {
                        discovered.push(DeviceInfo {
                            id: "ttl".to_string(),
                            name: "TTL Pulse Generator".to_string(),
                            device_type: crate::devices::DeviceType::TTL,
                            status: DeviceStatus::Disconnected,
                            metadata: json!({
                                "port": port.port_name,
                                "manufacturer": info.manufacturer,
                                "product": info.product
                            }),
                        });
                    }
                }
            }
        }

        discovered
    })
    .await;

    // TODO: Implement network discovery for Kernel, Pupil devices

    match result {
        Ok(devices) => Ok(devices),
        Err(e) => {
            error!("Task error discovering devices: {}", e);
            Ok(vec![])
        }
    }
}

#[tauri::command]
pub async fn get_device_metrics(
    device_id: String,
    state: State<'_, Arc<AppState>>,
) -> Result<CommandResult<serde_json::Value>, ()> {
    if let Some(metrics) = state.get_device_metrics(&device_id).await {
        Ok(CommandResult::success(json!(metrics)))
    } else {
        Ok(CommandResult::error(format!(
            "No metrics available for device {}",
            device_id
        )))
    }
}

#[tauri::command]
pub async fn get_system_diagnostics(
    state: State<'_, Arc<AppState>>,
) -> Result<serde_json::Value, ()> {
    let devices = state.devices.read().await;
    let mut device_statuses: HashMap<String, String> = HashMap::new();

    // Use async read instead of blocking_read to avoid blocking the runtime
    for (id, device) in devices.iter() {
        let device = device.read().await;
        device_statuses.insert(id.clone(), format!("{:?}", device.get_status()));
    }

    Ok(json!({
        "devices": device_statuses,
        "connections": state.connections.len(),
        "uptime_seconds": state.get_uptime().as_secs(),
        "message_count": state.get_message_count().await,
        "last_error": state.get_last_error().await,
    }))
}

#[tauri::command]
pub async fn load_configuration() -> Result<serde_json::Value, ()> {
    // TODO: Load from config file
    Ok(json!({
        "auto_connect": false,
        "default_ttl_port": "/dev/cu.usbmodem101",
        "websocket_port": 9000,
        "log_level": "info"
    }))
}

#[tauri::command]
pub async fn save_configuration(config: serde_json::Value) -> Result<CommandResult<String>, ()> {
    // TODO: Save to config file
    info!("Saving configuration: {:?}", config);
    Ok(CommandResult::success("Configuration saved".to_string()))
}

// Performance monitoring commands

#[tauri::command]
pub async fn get_performance_metrics(
    state: State<'_, Arc<AppState>>,
) -> Result<crate::performance::PerformanceMetrics, ()> {
    Ok(state.get_performance_metrics().await)
}

#[tauri::command]
pub async fn get_device_performance_metrics(
    device_id: String,
    state: State<'_, Arc<AppState>>,
) -> Result<CommandResult<crate::performance::DevicePerformanceMetrics>, ()> {
    if let Some(metrics) = state.get_device_performance_metrics(&device_id).await {
        Ok(CommandResult::success(metrics))
    } else {
        Ok(CommandResult::error(format!(
            "No performance metrics available for device {}",
            device_id
        )))
    }
}

#[tauri::command]
pub async fn get_performance_summary(
    state: State<'_, Arc<AppState>>,
) -> Result<serde_json::Value, ()> {
    Ok(state.get_performance_summary().await)
}

#[tauri::command]
pub async fn check_ttl_latency_compliance(
    device_id: String,
    state: State<'_, Arc<AppState>>,
) -> Result<CommandResult<bool>, ()> {
    if let Some(is_compliant) = state.check_ttl_latency_compliance(&device_id).await {
        Ok(CommandResult::success(is_compliant))
    } else {
        Ok(CommandResult::error(format!(
            "No TTL latency data available for device {}",
            device_id
        )))
    }
}

#[tauri::command]
pub async fn list_all_serial_ports_debug() -> Result<CommandResult<Vec<serde_json::Value>>, ()> {
    // Run blocking serial port enumeration on the blocking thread pool
    let result = tokio::task::spawn_blocking(|| TtlDevice::list_all_ports_debug()).await;

    match result {
        Ok(Ok(ports)) => Ok(CommandResult::success(ports)),
        Ok(Err(e)) => Ok(CommandResult::error(format!(
            "Failed to list serial ports: {}",
            e
        ))),
        Err(e) => Ok(CommandResult::error(format!("Task error: {}", e))),
    }
}

#[tauri::command]
pub async fn list_ttl_devices() -> Result<CommandResult<serde_json::Value>, ()> {
    // Run blocking serial port enumeration on the blocking thread pool
    let result = tokio::task::spawn_blocking(|| TtlDevice::list_ttl_devices()).await;

    match result {
        Ok(Ok(devices)) => Ok(CommandResult::success(devices)),
        Ok(Err(e)) => Ok(CommandResult::error(format!(
            "Failed to list TTL devices: {}",
            e
        ))),
        Err(e) => Ok(CommandResult::error(format!("Task error: {}", e))),
    }
}

#[tauri::command]
pub async fn find_ttl_port_by_serial(
    serial_number: String,
) -> Result<CommandResult<Option<String>>, ()> {
    // Run blocking serial port search on the blocking thread pool
    let result =
        tokio::task::spawn_blocking(move || TtlDevice::find_port_by_serial(&serial_number)).await;

    match result {
        Ok(Ok(port)) => Ok(CommandResult::success(port)),
        Ok(Err(e)) => Ok(CommandResult::error(format!(
            "Failed to search for TTL device: {}",
            e
        ))),
        Err(e) => Ok(CommandResult::error(format!("Task error: {}", e))),
    }
}

#[tauri::command]
pub async fn reset_performance_metrics(
    device_id: Option<String>,
    state: State<'_, Arc<AppState>>,
) -> Result<CommandResult<String>, ()> {
    match device_id {
        Some(id) => {
            // Reset specific device metrics by removing and re-adding
            state.performance_monitor.remove_device(&id).await;
            state.performance_monitor.add_device(id.clone()).await;
            Ok(CommandResult::success(format!(
                "Performance metrics reset for device {}",
                id
            )))
        }
        None => {
            // Create new performance monitor to reset all metrics
            *state.performance_monitor.device_counters.write().await =
                std::collections::HashMap::new();
            Ok(CommandResult::success(
                "All performance metrics reset".to_string(),
            ))
        }
    }
}

// Logging commands
// Note: LogEntry is imported from crate::logging

/// Get logs from the in-memory buffer (for real-time display).
/// This returns the most recent logs quickly without database access.
#[tauri::command]
pub async fn get_logs(
    _state: State<'_, Arc<AppState>>,
) -> Result<CommandResult<Vec<LogEntry>>, ()> {
    // Use spawn_blocking to avoid blocking the Tokio runtime.
    // The log buffer uses std::sync::RwLock which can contend with
    // the tracing layer that writes logs on every event.
    let result = tokio::task::spawn_blocking(|| get_all_logs()).await;

    match result {
        Ok(logs) => Ok(CommandResult::success(logs)),
        Err(e) => {
            error!("Failed to get logs: {}", e);
            Ok(CommandResult::success(vec![]))
        }
    }
}

/// Query logs from the database with filtering and pagination.
/// Use this for historical log access with search, filtering, and pagination.
#[tauri::command]
pub async fn query_logs(
    limit: Option<i64>,
    offset: Option<i64>,
    level: Option<String>,
    device: Option<String>,
    search: Option<String>,
    from_timestamp: Option<String>,
    to_timestamp: Option<String>,
    session_id: Option<String>,
) -> Result<CommandResult<crate::storage::logs::LogQueryResult>, ()> {
    let storage = match crate::storage::get_storage() {
        Some(s) => s,
        None => {
            return Ok(CommandResult::error("Database not initialized".to_string()));
        }
    };

    let options = crate::storage::logs::LogQueryOptions {
        limit,
        offset,
        level,
        device,
        search,
        from_timestamp,
        to_timestamp,
        session_id,
    };

    match crate::storage::logs::query_logs(storage.pool(), options).await {
        Ok(result) => Ok(CommandResult::success(result)),
        Err(e) => Ok(CommandResult::error(format!("Query failed: {}", e))),
    }
}

/// Get log statistics (counts by level).
#[tauri::command]
pub async fn get_log_stats() -> Result<CommandResult<serde_json::Value>, ()> {
    let storage = match crate::storage::get_storage() {
        Some(s) => s,
        None => {
            return Ok(CommandResult::error("Database not initialized".to_string()));
        }
    };

    match crate::storage::logs::get_log_counts_by_level(storage.pool()).await {
        Ok(counts) => {
            let stats: serde_json::Map<String, serde_json::Value> = counts
                .into_iter()
                .map(|(level, count)| (level, serde_json::Value::Number(count.into())))
                .collect();
            Ok(CommandResult::success(serde_json::Value::Object(stats)))
        }
        Err(e) => Ok(CommandResult::error(format!("Query failed: {}", e))),
    }
}

/// Get storage statistics (database size, record counts).
#[tauri::command]
pub async fn get_storage_stats() -> Result<CommandResult<crate::storage::StorageStats>, ()> {
    let storage = match crate::storage::get_storage() {
        Some(s) => s,
        None => {
            return Ok(CommandResult::error("Database not initialized".to_string()));
        }
    };

    match storage.get_stats().await {
        Ok(stats) => Ok(CommandResult::success(stats)),
        Err(e) => Ok(CommandResult::error(format!("Query failed: {}", e))),
    }
}

/// Start a new recording session.
#[tauri::command]
pub async fn start_session(
    metadata: Option<serde_json::Value>,
) -> Result<CommandResult<String>, ()> {
    let storage = match crate::storage::get_storage() {
        Some(s) => s,
        None => {
            return Ok(CommandResult::error("Database not initialized".to_string()));
        }
    };

    match storage.start_session(metadata).await {
        Ok(session_id) => {
            info!("Started recording session: {}", session_id);
            Ok(CommandResult::success(session_id))
        }
        Err(e) => Ok(CommandResult::error(format!(
            "Failed to start session: {}",
            e
        ))),
    }
}

/// End the current recording session.
#[tauri::command]
pub async fn end_session() -> Result<CommandResult<String>, ()> {
    let storage = match crate::storage::get_storage() {
        Some(s) => s,
        None => {
            return Ok(CommandResult::error("Database not initialized".to_string()));
        }
    };

    // Flush logs before ending session
    crate::logging::flush_logs().await;

    match storage.end_session().await {
        Ok(_) => {
            info!("Ended recording session");
            Ok(CommandResult::success("Session ended".to_string()))
        }
        Err(e) => Ok(CommandResult::error(format!(
            "Failed to end session: {}",
            e
        ))),
    }
}

/// List all recording sessions.
#[tauri::command]
pub async fn list_sessions(
    limit: Option<i64>,
) -> Result<CommandResult<Vec<crate::storage::Session>>, ()> {
    let storage = match crate::storage::get_storage() {
        Some(s) => s,
        None => {
            return Ok(CommandResult::error("Database not initialized".to_string()));
        }
    };

    match storage.list_sessions(limit).await {
        Ok(sessions) => Ok(CommandResult::success(sessions)),
        Err(e) => Ok(CommandResult::error(format!("Query failed: {}", e))),
    }
}

/// Clean up old logs from the database.
///
/// Deletes logs older than the specified number of days.
/// The `older_than_days` parameter must be at least 1 to prevent accidental deletion.
#[tauri::command]
pub async fn cleanup_old_logs(older_than_days: i64) -> Result<CommandResult<u64>, ()> {
    // Validate input to prevent accidental deletion
    if older_than_days < 1 {
        return Ok(CommandResult::error(
            "older_than_days must be at least 1".to_string(),
        ));
    }

    // Cap at reasonable maximum to prevent overflow issues
    let days = older_than_days.min(36500); // ~100 years max

    let storage = match crate::storage::get_storage() {
        Some(s) => s,
        None => {
            return Ok(CommandResult::error("Database not initialized".to_string()));
        }
    };

    match storage.cleanup_old_logs(days).await {
        Ok(deleted) => {
            info!(
                "Deleted {} old log entries (older than {} days)",
                deleted, days
            );
            Ok(CommandResult::success(deleted))
        }
        Err(e) => Ok(CommandResult::error(format!("Cleanup failed: {}", e))),
    }
}

/// Clear ALL logs from the database.
///
/// This permanently deletes all log entries. Use with caution.
#[tauri::command]
pub async fn clear_all_logs() -> Result<CommandResult<u64>, ()> {
    let storage = match crate::storage::get_storage() {
        Some(s) => s,
        None => {
            return Ok(CommandResult::error("Database not initialized".to_string()));
        }
    };

    match storage.clear_all_logs().await {
        Ok(deleted) => {
            info!("Cleared all {} log entries from database", deleted);
            Ok(CommandResult::success(deleted))
        }
        Err(e) => Ok(CommandResult::error(format!("Clear failed: {}", e))),
    }
}

#[tauri::command]
pub async fn export_logs(
    logs_data: Vec<serde_json::Value>,
    file_path: Option<String>,
    app_handle: AppHandle,
) -> Result<CommandResult<serde_json::Value>, ()> {
    use std::fs::File;
    use std::io::Write;

    // Generate default filename with timestamp
    let timestamp = Utc::now().format("%Y%m%d_%H%M%S");
    let default_filename = format!("hyperstudy_bridge_logs_{}.json", timestamp);

    // Use provided file path or fall back to app data directory
    let final_path = if let Some(path) = file_path {
        std::path::PathBuf::from(path)
    } else {
        let app_data_dir = app_handle
            .path()
            .app_data_dir()
            .unwrap_or_else(|_| std::path::PathBuf::from("."));
        let logs_dir = app_data_dir.join("logs");

        // Create logs directory if it doesn't exist
        if let Err(e) = std::fs::create_dir_all(&logs_dir) {
            return Ok(CommandResult::error(format!(
                "Failed to create logs directory: {}",
                e
            )));
        }

        logs_dir.join(&default_filename)
    };

    // Get filename from path for response
    let filename = final_path
        .file_name()
        .map(|f| f.to_string_lossy().to_string())
        .unwrap_or(default_filename);

    match File::create(&final_path) {
        Ok(mut file) => {
            let json_data = serde_json::to_string_pretty(&logs_data)
                .map_err(|e| format!("Failed to serialize logs: {}", e));

            match json_data {
                Ok(json_str) => {
                    if let Err(e) = file.write_all(json_str.as_bytes()) {
                        Ok(CommandResult::error(format!(
                            "Failed to write log file: {}",
                            e
                        )))
                    } else {
                        Ok(CommandResult::success(json!({
                            "path": final_path.to_string_lossy(),
                            "filename": filename,
                            "count": logs_data.len()
                        })))
                    }
                }
                Err(e) => Ok(CommandResult::error(e)),
            }
        }
        Err(e) => Ok(CommandResult::error(format!(
            "Failed to create log file: {}",
            e
        ))),
    }
}

#[tauri::command]
pub async fn set_log_level(level: String) -> Result<CommandResult<String>, ()> {
    // TODO: Implement dynamic log level changes
    info!("Log level change requested: {}", level);
    Ok(CommandResult::success(format!(
        "Log level set to {}",
        level
    )))
}

/// Test a TTL device connection without keeping it open
/// Sends TEST command and returns device response (e.g., firmware version)
#[tauri::command]
pub async fn test_ttl_device(port: String) -> Result<CommandResult<String>, ()> {
    info!("Testing TTL device on port: {}", port);

    // Run all blocking serial I/O on the blocking thread pool
    let result = tokio::task::spawn_blocking(move || {
        let mut serial_port = serialport::new(&port, 115200)
            .timeout(std::time::Duration::from_millis(500))
            .open()
            .map_err(|e| format!("Failed to open port: {}", e))?;

        // Send TEST command
        serial_port
            .write_all(b"TEST\n")
            .map_err(|e| format!("Failed to send TEST command: {}", e))?;

        serial_port
            .flush()
            .map_err(|e| format!("Failed to flush: {}", e))?;

        // Small delay for device to respond
        std::thread::sleep(std::time::Duration::from_millis(100));

        // Read response
        let mut buffer = vec![0u8; 256];
        match serial_port.read(&mut buffer) {
            Ok(bytes_read) => {
                buffer.truncate(bytes_read);
                let response = String::from_utf8_lossy(&buffer).trim().to_string();
                Ok(response)
            }
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {
                Err("Device did not respond (timeout)".to_string())
            }
            Err(e) => Err(format!("Failed to read response: {}", e)),
        }
    })
    .await;

    match result {
        Ok(Ok(response)) => {
            info!("TTL device test response: {}", response);
            Ok(CommandResult::success(response))
        }
        Ok(Err(e)) => Ok(CommandResult::error(e)),
        Err(e) => Ok(CommandResult::error(format!("Task error: {}", e))),
    }
}

/// Reset a device - removes it from state and clears errors
/// Allows fresh connection attempt after an error
#[tauri::command]
pub async fn reset_device(
    device_id: String,
    state: State<'_, Arc<AppState>>,
    app_handle: AppHandle,
) -> Result<CommandResult<String>, ()> {
    info!("Resetting device: {}", device_id);

    // Remove the device from state if it exists — closing its hardware
    // connection first, and only the exact device we closed (see
    // disconnect_device).
    if let DisconnectTarget::Device(device_lock) =
        state.device_for_disconnect(&device_id, None).await
    {
        let device_type = {
            let mut device = device_lock.write().await;
            let device_type = device.get_info().device_type;
            let _ = device.disconnect().await;
            device_type
        };
        if state.remove_device_if_same(&device_id, &device_lock).await {
            state.broadcast_device_status(DeviceStatusEvent::disconnected(
                device_id.clone(),
                device_type,
                "reset from the Bridge app",
            ));
        }
    }

    // Clear last error
    state.set_last_error(None).await;

    // Emit status update
    app_handle
        .emit(
            "device_status_changed",
            json!({
                "device": device_id,
                "status": "Disconnected"
            }),
        )
        .unwrap_or_else(|e| error!("Failed to emit event: {}", e));

    Ok(CommandResult::success(format!(
        "Device {} reset successfully",
        device_id
    )))
}

// FRENZ Python bridge commands

/// Start the FRENZ Python bridge process (PyApp binary).
///
/// Credentials are passed from the frontend (read from Stronghold vault)
/// and forwarded to the child process via stdin.
#[tauri::command]
pub async fn start_frenz_bridge(
    device_id: String,
    product_key: String,
    state: State<'_, Arc<AppState>>,
    app_handle: AppHandle,
) -> Result<CommandResult<String>, ()> {
    info!(
        device = "frenz",
        "Starting FRENZ bridge for device: {}", device_id
    );

    match state
        .frenz_process
        .start(&device_id, &product_key, &app_handle)
        .await
    {
        Ok(()) => Ok(CommandResult::success("FRENZ bridge started".to_string())),
        Err(e) => {
            error!(device = "frenz", "Failed to start FRENZ bridge: {}", e);
            Ok(CommandResult::error(e))
        }
    }
}

/// Stop the FRENZ Python bridge process.
#[tauri::command]
pub async fn stop_frenz_bridge(
    state: State<'_, Arc<AppState>>,
) -> Result<CommandResult<String>, ()> {
    info!(device = "frenz", "Stopping FRENZ bridge");

    let (streams, process) = tokio::join!(
        state.frenz_manager.disconnect_all(),
        state.frenz_process.stop()
    );
    let errors: Vec<String> = [streams.err().map(|e| e.to_string()), process.err()]
        .into_iter()
        .flatten()
        .collect();
    if errors.is_empty() {
        state.broadcast_device_status(crate::bridge::DeviceStatusEvent::disconnected(
            "frenz".into(),
            crate::devices::DeviceType::LSL,
            "FRENZ disconnected",
        ));
        Ok(CommandResult::success("FRENZ bridge stopped".to_string()))
    } else {
        Ok(CommandResult::error(format!(
            "FRENZ cleanup failed: {}",
            errors.join("; ")
        )))
    }
}

/// Get the current status of the FRENZ Python bridge process.
#[tauri::command]
pub async fn get_frenz_bridge_status(
    state: State<'_, Arc<AppState>>,
) -> Result<crate::devices::lsl::frenz_process::FrenzBridgeStatus, ()> {
    Ok(state.frenz_process.get_status().await)
}

/// Check if the FRENZ bridge binary is available on this platform.
#[tauri::command]
pub async fn check_frenz_bridge_available(app_handle: AppHandle) -> Result<bool, ()> {
    Ok(crate::devices::lsl::frenz_process::FrenzProcessManager::check_available(&app_handle))
}

/// Application metadata from Cargo.toml
#[derive(Debug, Serialize, Deserialize)]
pub struct AppInfo {
    pub name: String,
    pub version: String,
    pub description: String,
    pub authors: Vec<String>,
    pub license: String,
    pub repository: String,
    pub homepage: String,
}

/// Get application information from Cargo.toml metadata.
/// This uses compile-time env! macros to embed Cargo.toml values.
#[tauri::command]
pub fn get_app_info() -> AppInfo {
    // Parse authors string (comma-separated in Cargo.toml)
    let authors_str = env!("CARGO_PKG_AUTHORS");
    let authors: Vec<String> = if authors_str.is_empty() {
        vec![]
    } else {
        authors_str
            .split(':')
            .map(|s| s.trim().to_string())
            .collect()
    };

    AppInfo {
        name: env!("CARGO_PKG_NAME").to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        description: env!("CARGO_PKG_DESCRIPTION").to_string(),
        authors,
        license: env!("CARGO_PKG_LICENSE").to_string(),
        repository: env!("CARGO_PKG_REPOSITORY").to_string(),
        homepage: env!("CARGO_PKG_HOMEPAGE").to_string(),
    }
}

// Recording operations run off the async executor; native LSL handles stay on
// the recording thread, independently of browser and display subscriptions.
#[tauri::command]
pub async fn discover_recording_streams() -> Result<Vec<crate::recording::StreamDescriptor>, String>
{
    tokio::task::spawn_blocking(crate::recording::discover)
        .await
        .map_err(|_| "Discovery worker failed".to_string())?
        .map_err(|e| e.to_string())
}
#[tauri::command]
pub async fn start_lsl_recording(
    request: crate::recording::RecordingRequest,
    state: State<'_, Arc<AppState>>,
) -> Result<crate::recording::RecordingStatus, String> {
    let manager = state.recording.clone();
    tokio::task::spawn_blocking(move || manager.start(request))
        .await
        .map_err(|_| "Recording worker failed".to_string())?
        .map_err(|e| e.to_string())
}
#[tauri::command]
pub async fn stop_lsl_recording(
    request: Option<crate::recording::RecordingStopRequest>,
    state: State<'_, Arc<AppState>>,
) -> Result<crate::recording::RecordingStatus, String> {
    let manager = state.recording.clone();
    tokio::task::spawn_blocking(move || manager.stop_with(request.unwrap_or_default()))
        .await
        .map_err(|_| "Recording worker failed".to_string())?
        .map_err(|e| e.to_string())
}
#[tauri::command]
pub fn lsl_recording_status(state: State<'_, Arc<AppState>>) -> crate::recording::RecordingStatus {
    state.recording.status()
}
#[tauri::command]
pub async fn recover_lsl_recording(
    path: std::path::PathBuf,
    state: State<'_, Arc<AppState>>,
) -> Result<std::path::PathBuf, String> {
    if matches!(
        state.recording.status().state.as_str(),
        "starting" | "recording" | "stopping"
    ) {
        return Err("Stop acquisition before recovering an interrupted recording".into());
    }
    tokio::task::spawn_blocking(move || crate::recording::recover(&path))
        .await
        .map_err(|_| "Recovery worker failed".to_string())?
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn scan_frenz_devices(
    app: tauri::AppHandle,
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<crate::devices::lsl::frenz_process::FrenzBluetoothDevice>, String> {
    state.frenz_process.scan(&app).await
}

#[tauri::command]
pub async fn list_study_recordings(
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<crate::recording::study::StudySession>, String> {
    let manager = state.recording.clone();
    tokio::task::spawn_blocking(move || manager.list_studies())
        .await
        .map_err(|_| "Study status worker failed".to_string())?
        .map_err(|e| e.to_string())
}

/// Shows a recording in Finder (Explorer / the file manager elsewhere). Takes an id, never a path: only files Bridge
/// recorded can be revealed.
#[tauri::command]
pub async fn reveal_recording(
    recording_id: Option<String>,
    state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    let manager = state.recording.clone();
    tokio::task::spawn_blocking(move || {
        let path = manager
            .recorded_file(recording_id.as_deref())
            .map_err(|e| e.to_string())?;
        let status = reveal_command(&path)
            .status()
            .map_err(|e| format!("Couldn't open the folder: {e}"))?;
        if status.success() {
            Ok(())
        } else {
            Err(format!("Couldn't open the folder ({status})"))
        }
    })
    .await
    .map_err(|_| "Reveal worker failed".to_string())?
}

fn reveal_command(path: &std::path::Path) -> std::process::Command {
    #[cfg(target_os = "macos")]
    {
        let mut command = std::process::Command::new("open");
        command.arg("-R").arg(path);
        command
    }
    #[cfg(target_os = "windows")]
    {
        let mut command = std::process::Command::new("explorer");
        command.arg(format!("/select,{}", path.display()));
        command
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let mut command = std::process::Command::new("xdg-open");
        command.arg(path.parent().unwrap_or(path));
        command
    }
}

/// The LSL streams Bridge receives and publishes, for the LSL panel.
#[tauri::command]
pub async fn lsl_bridge_streams(
    state: State<'_, Arc<AppState>>,
) -> Result<serde_json::Value, String> {
    Ok(state.lsl_bridge_streams().await)
}

/// The result of the latest update check (Bridge checks on every launch).
#[tauri::command]
pub async fn update_status(app: AppHandle) -> crate::updates::UpdateStatus {
    app.state::<crate::updates::UpdateState>().get().await
}

/// Checks for an update now.
#[tauri::command]
pub async fn check_for_updates(app: AppHandle) -> crate::updates::UpdateStatus {
    crate::updates::check(&app).await
}

/// Installs the available update and restarts Bridge; refused while a recording is running.
#[tauri::command]
pub async fn install_update(app: AppHandle, state: State<'_, Arc<AppState>>) -> Result<(), String> {
    let recorder_state = state.recording.status().state;
    crate::updates::install(&app, &recorder_state).await
}

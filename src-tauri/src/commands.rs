use crate::bridge::AppState;
use crate::devices::ttl::TtlDevice;
use crate::devices::Device;
use crate::logging::{get_all_logs, LogEntry};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Arc;
use tauri::{AppHandle, Manager, State};
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

// Bridge server management commands

// Device management commands

// TTL-specific high-performance command
#[tauri::command]
pub async fn send_ttl_pulse(
    port: Option<String>,
    pulse_duration_ms: Option<u64>,
    state: State<'_, Arc<AppState>>,
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

                Ok(CommandResult::success(latency_us))
            }
            Err(e) => {
                error!("Failed to send TTL pulse: {}", e);
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

// Performance monitoring commands

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
pub async fn stop_study_recording(
    recording_id: String,
    state: State<'_, Arc<AppState>>,
) -> Result<serde_json::Value, String> {
    let manager = state.recording.clone();
    let session = tokio::task::spawn_blocking(move || manager.stop_study_locally(&recording_id))
        .await
        .map_err(|_| "Recording worker failed".to_string())?
        .map_err(|e| e.to_string())?;
    serde_json::to_value(session).map_err(|e| e.to_string())
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
        if system_open_succeeded(status) {
            Ok(())
        } else {
            Err(format!("Couldn't open the folder ({status})"))
        }
    })
    .await
    .map_err(|_| "Reveal worker failed".to_string())?
}

/// Opens the folder where Bridge keeps study recordings, its log database and the band key (Show Data Folder in the
/// menu). It takes no path: the folder is always the app's own data directory.
pub async fn reveal_data_folder(app: AppHandle) -> Result<(), String> {
    let folder = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("Couldn't find the data folder: {e}"))?;
    tokio::task::spawn_blocking(move || {
        std::fs::create_dir_all(&folder)
            .map_err(|e| format!("Couldn't create the data folder: {e}"))?;
        let status = open_with_system(folder.as_os_str())
            .status()
            .map_err(|e| format!("Couldn't open the folder: {e}"))?;
        if system_open_succeeded(status) {
            Ok(())
        } else {
            Err(format!("Couldn't open the folder ({status})"))
        }
    })
    .await
    .map_err(|_| "Folder worker failed".to_string())?
}

/// The Bridge page on the Hyperstudy docs site (Help > Documentation).
pub const DOCS_URL: &str = "https://docs.hyperstudy.io/devices/hyperstudy-bridge";

/// Opens the documentation in the default browser. The URL is fixed; nothing from the page reaches it.
pub fn open_documentation() -> Result<(), String> {
    let status = open_with_system(std::ffi::OsStr::new(DOCS_URL))
        .status()
        .map_err(|e| format!("Couldn't open the documentation: {e}"))?;
    if system_open_succeeded(status) {
        Ok(())
    } else {
        Err(format!("Couldn't open the documentation ({status})"))
    }
}

/// Opens a folder or URL with the system's default handler.
fn open_with_system(target: &std::ffi::OsStr) -> std::process::Command {
    #[cfg(target_os = "macos")]
    let mut command = std::process::Command::new("open");
    #[cfg(target_os = "windows")]
    let mut command = std::process::Command::new("explorer");
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let mut command = std::process::Command::new("xdg-open");
    command.arg(target);
    command
}

/// Whether the system file browser or URL handler opened what we asked for. Windows Explorer exits with 1 even when
/// it worked, so its exit status says nothing there; elsewhere a failure exits non-zero.
fn system_open_succeeded(status: std::process::ExitStatus) -> bool {
    cfg!(target_os = "windows") || status.success()
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

/// Installs the available update and restarts Bridge; refused while a recording is running.
#[tauri::command]
pub async fn install_update(app: AppHandle, state: State<'_, Arc<AppState>>) -> Result<(), String> {
    let recording = state.recording.clone();
    crate::updates::install(&app, move || recording.status().state).await
}

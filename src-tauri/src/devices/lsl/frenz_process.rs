//! FRENZ Python Bridge Process Manager
//!
//! Manages the lifecycle of the PyApp-bundled Python process that bridges
//! frenztoolkit (BLE) data to Lab Streaming Layer streams.
//!
//! # Architecture
//!
//! ```text
//! Rust (FrenzProcessManager)
//!   |--- spawns ---> PyApp binary (embedded Python + frenztoolkit + pylsl)
//!   |--- stdin  ---> {"device_id":"...","product_key":"..."}
//!   |<-- stdout ---  {"status":"streaming","streams":[...],"sample_count":100}
//!   |--- stdin  ---> stop\n
//! ```

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tauri::Manager;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};

/// Current state of the FRENZ bridge process
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FrenzBridgeState {
    /// Binary not found or unsupported platform
    NotAvailable,
    /// Process not running, ready to start
    Stopped,
    /// First-run: PyApp installing packages
    Bootstrapping,
    /// BLE connection to FRENZ headband in progress
    Connecting,
    /// Actively pushing data to LSL
    Streaming,
    /// Error state
    Error,
}

/// Shown while the sidecar waits for macOS's Bluetooth prompt to be answered (frenz_permission.py).
pub const BLUETOOTH_PERMISSION_PROMPT: &str =
    "Allow Bluetooth access for HyperStudy Bridge in the macOS prompt to connect the band.";

/// Detailed status of the FRENZ bridge process
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FrenzBridgeStatus {
    pub state: FrenzBridgeState,
    /// Human-readable message (error detail, phase info, etc.)
    pub message: Option<String>,
    /// Bootstrap phase (e.g., "installing", "importing")
    pub phase: Option<String>,
    /// Active LSL stream suffixes when streaming
    pub streams: Vec<String>,
    /// Cumulative sample count from the Python process
    pub sample_count: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timing: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clock_pair: Option<serde_json::Value>,
}

impl Default for FrenzBridgeStatus {
    fn default() -> Self {
        Self {
            state: FrenzBridgeState::Stopped,
            message: None,
            phase: None,
            streams: vec![],
            sample_count: 0,
            timing: None,
            clock_pair: None,
        }
    }
}

impl FrenzBridgeStatus {
    fn apply_python_status(&mut self, parsed: PythonStatusLine) {
        // The sidecar reports cleanup separately after connection/acquisition
        // failures. Preserve the first failure and its phase until an explicit
        // new start resets status; teardown must not hide the cause.
        if self.state == FrenzBridgeState::Error {
            return;
        }
        match parsed.status.as_str() {
            "waiting_for_config" => {
                self.state = FrenzBridgeState::Bootstrapping;
                self.message = Some("Waiting for configuration...".to_string());
            }
            "bootstrapping" => {
                self.state = FrenzBridgeState::Bootstrapping;
                self.phase = parsed.phase.clone();
                if let Some(ref pkg) = parsed.package {
                    self.message = Some(if parsed.phase.as_deref() == Some("importing") {
                        format!("Loading {}...", pkg)
                    } else {
                        format!("Installing {}...", pkg)
                    });
                } else if let Some(ref phase) = parsed.phase {
                    self.message = Some(format!("{}...", phase));
                }
            }
            "connecting" => {
                self.state = FrenzBridgeState::Connecting;
                self.message =
                    if parsed.phase.as_deref() == Some("waiting_for_bluetooth_permission") {
                        Some(BLUETOOTH_PERMISSION_PROMPT.to_string())
                    } else {
                        parsed
                            .device_id
                            .as_ref()
                            .map(|id| format!("Connecting to {}...", id))
                    };
                if let Some(ref phase) = parsed.phase {
                    self.phase = Some(phase.clone());
                }
            }
            "streaming" => {
                self.state = FrenzBridgeState::Streaming;
                self.message = None;
                if let Some(streams) = parsed.streams {
                    self.streams = streams;
                }
                if let Some(count) = parsed.sample_count {
                    self.sample_count = count;
                }
                self.timing = parsed.timing;
                self.clock_pair = parsed.clock_pair;
            }
            "error" => {
                self.state = FrenzBridgeState::Error;
                self.message = parsed.message;
                self.phase = parsed.phase;
                self.streams.clear();
            }
            "stopped" => {
                self.state = FrenzBridgeState::Stopped;
                self.message = None;
                self.streams.clear();
            }
            other => {
                debug!(
                    device = "frenz",
                    "Unknown status from FRENZ bridge: {}", other
                );
            }
        }
    }

    fn mark_process_exit(&mut self, description: &str, requested: bool) {
        self.streams.clear();
        if requested {
            self.state = FrenzBridgeState::Stopped;
            self.message = None;
        } else if self.state != FrenzBridgeState::Error {
            self.state = FrenzBridgeState::Error;
            self.phase = Some("process_exit".into());
            self.message = Some(format!("FRENZ acquisition ended unexpectedly ({description}). Check the band and reconnect."));
        }
    }
}

/// JSON status line emitted by the Python bridge on stdout
#[derive(Debug, Deserialize)]
struct PythonStatusLine {
    status: String,
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    phase: Option<String>,
    #[serde(default)]
    streams: Option<Vec<String>>,
    #[serde(default)]
    sample_count: Option<u64>,
    #[serde(default)]
    device_id: Option<String>,
    #[serde(default)]
    package: Option<String>,
    #[serde(default)]
    progress: Option<u32>,
    #[serde(default)]
    timing: Option<serde_json::Value>,
    #[serde(default)]
    clock_pair: Option<serde_json::Value>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct FrenzBluetoothDevice {
    pub name: String,
    pub address: String,
    pub rssi: i32,
    pub selectable: bool,
}
#[derive(Deserialize)]
struct FrenzScanReply {
    status: String,
    #[serde(default)]
    devices: Vec<FrenzBluetoothDevice>,
    message: Option<String>,
}

/// Manages the FRENZ Python bridge process lifecycle
pub struct FrenzProcessManager {
    /// The running child process (if any)
    process: Arc<RwLock<Option<tokio::process::Child>>>,
    /// Current bridge status
    status: Arc<RwLock<FrenzBridgeStatus>>,
    /// Shutdown flag for the stdout reader task
    shutdown: Arc<AtomicBool>,
    /// Serialize scan/start so two processes cannot compete for Bluetooth.
    operation: tokio::sync::Mutex<()>,
    /// Handle for the stdout reader task
    reader_task: Arc<RwLock<Option<tokio::task::JoinHandle<()>>>>,
    /// Native status subscribers must also observe API-initiated shutdowns.
    app_handle: RwLock<Option<tauri::AppHandle>>,
    /// Set once Bridge starts exiting; a scan in progress is woken through `scan_cancel` and refuses to continue.
    exiting: AtomicBool,
    scan_cancel: tokio::sync::Notify,
}

impl FrenzProcessManager {
    pub fn new() -> Self {
        Self {
            operation: tokio::sync::Mutex::new(()),
            process: Arc::new(RwLock::new(None)),
            status: Arc::new(RwLock::new(FrenzBridgeStatus::default())),
            shutdown: Arc::new(AtomicBool::new(false)),
            reader_task: Arc::new(RwLock::new(None)),
            app_handle: RwLock::new(None),
            exiting: AtomicBool::new(false),
            scan_cancel: tokio::sync::Notify::new(),
        }
    }

    /// Locate the PyApp binary in the app's resource directory.
    ///
    /// Returns `None` if the binary doesn't exist or the platform is unsupported.
    pub fn find_binary(app_handle: &tauri::AppHandle) -> Option<PathBuf> {
        // frenztoolkit only supports macOS and Windows
        if cfg!(target_os = "linux") {
            info!(
                device = "frenz",
                "FRENZ bridge not available on Linux (frenztoolkit requires macOS/Windows)"
            );
            return None;
        }

        let binary_name = if cfg!(target_os = "windows") {
            "frenz-bridge.exe"
        } else {
            "frenz-bridge"
        };

        // Try resource directory (bundled app)
        if let Ok(resource_dir) = app_handle.path().resource_dir() {
            let path = resource_dir.join(binary_name);
            if path.exists() {
                info!(device = "frenz", "Found FRENZ bridge binary: {:?}", path);
                return Some(path);
            }
        }

        // Try alongside the executable (dev mode)
        if let Ok(exe_dir) = std::env::current_exe() {
            if let Some(dir) = exe_dir.parent() {
                let path = dir.join("resources").join(binary_name);
                if path.exists() {
                    info!(
                        device = "frenz",
                        "Found FRENZ bridge binary (dev): {:?}", path
                    );
                    return Some(path);
                }
            }
        }

        info!(
            device = "frenz",
            "FRENZ bridge binary not found ({})", binary_name
        );
        None
    }

    /// Check if the bridge process is available on this platform
    pub fn check_available(app_handle: &tauri::AppHandle) -> bool {
        Self::find_binary(app_handle).is_some()
    }

    /// Scan advertisements without credentials or initializing the vendor SDK.
    pub async fn scan(&self, app: &tauri::AppHandle) -> Result<Vec<FrenzBluetoothDevice>, String> {
        let binary = Self::find_binary(app).ok_or("FRENZ sidecar is unavailable")?;
        self.scan_with(binary).await
    }

    async fn scan_with(&self, binary: PathBuf) -> Result<Vec<FrenzBluetoothDevice>, String> {
        let _operation = self
            .operation
            .try_lock()
            .map_err(|_| "A FRENZ scan or connection is already starting")?;
        if self.process.read().await.is_some() {
            return Err("Disconnect FRENZ before scanning for another band".into());
        }
        // Registered before the exiting check, so an exit that starts after the check still wakes this scan.
        let cancelled = self.scan_cancel.notified();
        tokio::pin!(cancelled);
        cancelled.as_mut().enable();
        if self.exiting.load(Ordering::SeqCst) {
            return Err("Bridge is quitting".into());
        }
        let mut child = tokio::process::Command::new(binary)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|_| "Could not start Bluetooth scanner")?;
        let mut input = child.stdin.take().ok_or("Scanner input unavailable")?;
        input
            .write_all(b"{\"action\":\"scan\"}\n")
            .await
            .map_err(|_| "Could not request Bluetooth scan")?;
        drop(input);
        // The helper can wait minutes on the macOS Bluetooth prompt or a first-run install. Quitting must not wait
        // for that: dropping the child on cancellation kills it (kill_on_drop) and releases the operation lock.
        let output = tokio::select! {
            output = tokio::time::timeout(std::time::Duration::from_secs(180), child.wait_with_output()) => output,
            _ = &mut cancelled => return Err("Bridge is quitting".into()),
        }
        .map_err(|_| {
            "Bluetooth setup or scan timed out. Check network access and Bluetooth permission."
        })?
        .map_err(|_| "Bluetooth scanner stopped unexpectedly")?;
        let reply = String::from_utf8_lossy(&output.stdout);
        for line in reply.lines() {
            if let Ok(result) = serde_json::from_str::<FrenzScanReply>(line) {
                match result.status.as_str() {
                    "discovered" if output.status.success() => return Ok(result.devices),
                    "scan_error" => {
                        return Err(result
                            .message
                            .unwrap_or_else(|| "Bluetooth scan failed".into()))
                    }
                    _ => {}
                }
            }
        }
        Err("Bluetooth scanner could not initialize. Check the sidecar installation.".into())
    }

    /// Start the FRENZ bridge process
    ///
    /// Spawns the PyApp binary, sends credentials via stdin, and starts
    /// a background task to read status updates from stdout.
    pub async fn start(
        &self,
        device_id: &str,
        product_key: &str,
        app_handle: &tauri::AppHandle,
    ) -> Result<(), String> {
        let _operation = self
            .operation
            .try_lock()
            .map_err(|_| "Another FRENZ operation is in progress".to_string())?;
        self.prepare_start().await?;

        let binary_path = Self::find_binary(app_handle)
            .ok_or_else(|| "FRENZ bridge binary not found".to_string())?;

        info!(device = "frenz", "Starting FRENZ bridge: {:?}", binary_path);
        *self.app_handle.write().await = Some(app_handle.clone());

        // Reset state
        self.shutdown.store(false, Ordering::Relaxed);
        {
            let mut status = self.status.write().await;
            *status = FrenzBridgeStatus {
                state: FrenzBridgeState::Bootstrapping,
                message: Some("Starting Python bridge...".to_string()),
                phase: None,
                streams: vec![],
                sample_count: 0,
                timing: None,
                clock_pair: None,
            };
        }

        // Spawn the process
        let mut child = tokio::process::Command::new(&binary_path)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| format!("Failed to spawn FRENZ bridge: {}", e))?;

        // Send credentials via stdin
        let credentials = serde_json::json!({
            "device_id": device_id,
            "product_key": product_key,
        });
        let cred_line = format!("{}\n", credentials);

        if let Some(stdin) = child.stdin.as_mut() {
            stdin
                .write_all(cred_line.as_bytes())
                .await
                .map_err(|e| format!("Failed to write credentials to stdin: {}", e))?;
            stdin
                .flush()
                .await
                .map_err(|e| format!("Failed to flush stdin: {}", e))?;
        } else {
            return Err("Failed to get stdin handle".to_string());
        }

        // Take stdout for the reader task
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "Failed to get stdout handle".to_string())?;

        // Store the process
        {
            let mut proc = self.process.write().await;
            *proc = Some(child);
        }

        // Spawn stdout reader task
        let status_clone = self.status.clone();
        let shutdown_clone = self.shutdown.clone();
        let process_clone = self.process.clone();
        let app_handle_clone = app_handle.clone();

        let handle = tokio::spawn(async move {
            let reader = BufReader::new(stdout);
            let mut lines = reader.lines();

            // Wait for the first status line with a timeout.
            // If PyApp is doing a first-run install (pip install frenztoolkit + deps),
            // stdout will be silent for potentially minutes. Detect this and update
            // the UI so the user knows what's happening.
            let first_run_timeout = std::time::Duration::from_secs(5);
            let mut got_first_line = false;

            match tokio::time::timeout(first_run_timeout, lines.next_line()).await {
                Ok(Ok(Some(line))) => {
                    got_first_line = true;
                    Self::handle_status_line(&line, &status_clone, &app_handle_clone).await;
                }
                Ok(Ok(None)) => {
                    // EOF before any output — process exited immediately
                    Self::handle_process_exit(
                        &status_clone,
                        &process_clone,
                        &shutdown_clone,
                        &app_handle_clone,
                    )
                    .await;
                    return;
                }
                Ok(Err(e)) => {
                    warn!(device = "frenz", "Error reading FRENZ bridge stdout: {}", e);
                    return;
                }
                Err(_) => {
                    // Timeout — no output yet, likely PyApp first-run install
                    info!(
                        device = "frenz",
                        "No output from FRENZ bridge after {}s — likely first-run package install",
                        first_run_timeout.as_secs()
                    );
                    {
                        let mut status = status_clone.write().await;
                        status.state = FrenzBridgeState::Bootstrapping;
                        status.phase = Some("installing".to_string());
                        status.message = Some(
                            "First-run setup \u{2014} installing Python packages (this may take several minutes)..."
                                .to_string(),
                        );
                        Self::emit_status_event(&status, &app_handle_clone);
                    }
                }
            }

            // Main read loop
            loop {
                if shutdown_clone.load(Ordering::Relaxed) {
                    break;
                }

                match lines.next_line().await {
                    Ok(Some(line)) => {
                        if !got_first_line {
                            got_first_line = true;
                            info!(
                                device = "frenz",
                                "FRENZ bridge first-run setup complete, received first status"
                            );
                        }
                        Self::handle_status_line(&line, &status_clone, &app_handle_clone).await;
                    }
                    Ok(None) => {
                        Self::handle_process_exit(
                            &status_clone,
                            &process_clone,
                            &shutdown_clone,
                            &app_handle_clone,
                        )
                        .await;
                        break;
                    }
                    Err(e) => {
                        warn!(device = "frenz", "Error reading FRENZ bridge stdout: {}", e);
                        break;
                    }
                }
            }

            debug!(device = "frenz", "FRENZ bridge reader task exiting");
        });

        {
            let mut task = self.reader_task.write().await;
            *task = Some(handle);
        }

        info!(device = "frenz", "FRENZ bridge process started");
        Ok(())
    }

    /// Handle the case where the child process exits (stdout EOF)
    async fn handle_process_exit(
        status: &Arc<RwLock<FrenzBridgeStatus>>,
        process: &Arc<RwLock<Option<tokio::process::Child>>>,
        shutdown: &Arc<AtomicBool>,
        app_handle: &tauri::AppHandle,
    ) {
        // Hold the lifecycle guard through cleanup: a new start must not race
        // the release of this process's abandoned LSL inlets.
        let mut proc = process.write().await;
        let Some(mut child) = proc.take() else {
            return;
        };
        let description =
            match tokio::time::timeout(std::time::Duration::from_secs(5), child.wait()).await {
                Ok(Ok(exit)) => exit.to_string(),
                Ok(Err(_)) => "exit status unavailable".into(),
                Err(_) => {
                    let _ = child.kill().await;
                    "output closed before process exit; process terminated".into()
                }
            };
        info!(device = "frenz", "FRENZ sidecar exited: {}", description);
        let requested = shutdown.load(Ordering::Relaxed);
        {
            let mut s = status.write().await;
            s.mark_process_exit(&description, requested);
            Self::emit_status_event(&s, app_handle);
        }
        if let Some(state) = app_handle.try_state::<Arc<crate::bridge::AppState>>() {
            state.broadcast_device_status(crate::bridge::state::DeviceStatusEvent::disconnected(
                "frenz".into(),
                crate::devices::DeviceType::LSL,
                "FRENZ acquisition process exited",
            ));
            if let Err(error) = state.frenz_manager.disconnect_all().await {
                warn!(
                    device = "frenz",
                    "Failed to close abandoned FRENZ inlets: {}", error
                );
            }
        }
    }

    /// Parse and handle a JSON status line from the Python process
    async fn handle_status_line(
        line: &str,
        status: &Arc<RwLock<FrenzBridgeStatus>>,
        app_handle: &tauri::AppHandle,
    ) {
        let parsed: PythonStatusLine = match serde_json::from_str(line) {
            Ok(p) => p,
            Err(e) => {
                debug!(
                    device = "frenz",
                    "Ignoring non-protocol FRENZ output ({})", e
                );
                return;
            }
        };

        let mut s = status.write().await;

        if parsed.status != "streaming" || s.state != FrenzBridgeState::Streaming {
            // The sidecar's messages are fixed, credential-safe text (it already shows them in the UI); without the
            // message an error row in the log said only which phase failed, never why.
            info!(device = "frenz", status = %parsed.status, phase = ?parsed.phase, detail = ?parsed.message,
                "FRENZ sidecar phase changed");
        }

        s.apply_python_status(parsed);

        Self::emit_status_event(&s, app_handle);
    }

    /// Emit a Tauri event with the current bridge status
    fn emit_status_event(status: &FrenzBridgeStatus, app_handle: &tauri::AppHandle) {
        use tauri::Emitter;
        if let Err(e) = app_handle.emit("frenz_bridge_status", status) {
            warn!(
                device = "frenz",
                "Failed to emit frenz_bridge_status event: {}", e
            );
        }
    }

    /// Bridge is exiting: end any scan first (it holds the operation lock while it waits), then stop the sidecar.
    pub async fn stop_for_exit(&self) -> Result<(), String> {
        self.exiting.store(true, Ordering::SeqCst);
        self.scan_cancel.notify_waiters();
        self.stop().await
    }

    /// Stop the FRENZ bridge process gracefully
    pub async fn stop(&self) -> Result<(), String> {
        let _operation = self.operation.lock().await;
        self.stop_locked().await
    }

    /// Called only while start owns the operation lock. A user-initiated start
    /// may reap a failed child still exiting, but must never replace an active one.
    async fn prepare_start(&self) -> Result<(), String> {
        let failed = self.status.read().await.state == FrenzBridgeState::Error;
        if failed && self.process.read().await.is_some() {
            self.stop_locked().await?;
            if let Some(app) = self.app_handle.read().await.as_ref() {
                if let Some(state) = app.try_state::<Arc<crate::bridge::AppState>>() {
                    state
                        .frenz_manager
                        .disconnect_all()
                        .await
                        .map_err(|e| e.to_string())?;
                }
            }
        }
        if self.process.read().await.is_some() {
            return Err("FRENZ bridge is already running".into());
        }
        Ok(())
    }

    /// Caller owns the lifecycle operation lock (public stop or explicit start).
    async fn stop_locked(&self) -> Result<(), String> {
        self.shutdown.store(true, Ordering::Relaxed);

        let mut proc = self.process.write().await;
        if let Some(ref mut child) = *proc {
            info!(device = "frenz", "Stopping FRENZ bridge process...");

            // Try graceful shutdown via stdin "stop" command
            if let Some(stdin) = child.stdin.as_mut() {
                let _ = stdin.write_all(b"stop\n").await;
                let _ = stdin.flush().await;
            }

            // Wait up to 5 seconds for graceful exit
            match tokio::time::timeout(std::time::Duration::from_secs(5), child.wait()).await {
                Ok(Ok(exit_status)) => {
                    info!(
                        device = "frenz",
                        "FRENZ bridge exited with: {}", exit_status
                    );
                }
                Ok(Err(e)) => {
                    warn!(device = "frenz", "Error waiting for FRENZ bridge: {}", e);
                }
                Err(_) => {
                    // Timeout — force kill
                    warn!(
                        device = "frenz",
                        "FRENZ bridge did not exit gracefully, killing..."
                    );
                    let _ = child.kill().await;
                }
            }
        }

        *proc = None;
        drop(proc);

        // Wait for reader task to finish
        let task = {
            let mut t = self.reader_task.write().await;
            t.take()
        };
        if let Some(mut handle) = task {
            if tokio::time::timeout(std::time::Duration::from_secs(2), &mut handle)
                .await
                .is_err()
            {
                handle.abort();
                let _ = handle.await;
            }
        }

        // Update status
        {
            let mut status = self.status.write().await;
            *status = FrenzBridgeStatus {
                state: FrenzBridgeState::Stopped,
                message: None,
                phase: None,
                streams: vec![],
                sample_count: 0,
                timing: None,
                clock_pair: None,
            };
            if let Some(app_handle) = self.app_handle.read().await.as_ref() {
                Self::emit_status_event(&status, app_handle);
            }
        }

        info!(device = "frenz", "FRENZ bridge process stopped");
        Ok(())
    }

    /// Get the current bridge status
    pub async fn get_status(&self) -> FrenzBridgeStatus {
        self.status.read().await.clone()
    }
}

impl std::fmt::Debug for FrenzProcessManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FrenzProcessManager").finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Quitting while a scan waited on the macOS Bluetooth prompt (or a first-run install) blocked exit for up to
    /// the scan's 180 s timeout: the scan held the operation lock and the exit stop waited for it.
    #[cfg(unix)]
    #[tokio::test]
    async fn exit_ends_a_scan_that_is_still_waiting() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("frenz-scan-exit-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let helper = dir.join("never-answers");
        std::fs::write(&helper, "#!/bin/sh\nexec sleep 60\n").unwrap();
        std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o755)).unwrap();

        let manager = Arc::new(FrenzProcessManager::new());
        let scanning = manager.clone();
        let scan = tokio::spawn(async move { scanning.scan_with(helper).await });
        while manager.operation.try_lock().is_ok() {
            tokio::task::yield_now().await;
        }

        tokio::time::timeout(std::time::Duration::from_secs(5), manager.stop_for_exit())
            .await
            .expect("exit waited on the scan")
            .unwrap();
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), scan)
            .await
            .unwrap()
            .unwrap();
        assert!(result.is_err(), "a cancelled scan reports no devices");
        assert!(
            manager.scan_with(dir.join("never-answers")).await.is_err(),
            "no new scan once Bridge is exiting"
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn explicit_start_cleans_failed_child_but_preserves_active_child() {
        let manager = FrenzProcessManager::new();
        // A controlled child waits for stdin, like a sidecar still cleaning up.
        let child = tokio::process::Command::new("/bin/cat")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        *manager.process.write().await = Some(child);
        manager.status.write().await.state = FrenzBridgeState::Streaming;
        manager.prepare_start().await.unwrap_err();
        assert!(manager.process.read().await.is_some());
        manager.status.write().await.state = FrenzBridgeState::Error;
        // EOF makes the controlled child exit without a timeout or a sleep.
        manager.process.write().await.as_mut().unwrap().stdin.take();
        manager.prepare_start().await.unwrap();
        assert!(manager.process.read().await.is_none());
        assert_eq!(manager.get_status().await.state, FrenzBridgeState::Stopped);
    }

    #[test]
    fn cleanup_statuses_never_replace_the_first_sidecar_failure() {
        let mut status = FrenzBridgeStatus::default();
        status.sample_count = 123;
        for line in [
            r#"{"status":"connecting","phase":"sdk_start","device_id":"FRENZI85"}"#,
            r#"{"status":"error","phase":"connecting_bluetooth","message":"FRENZ failed during connecting_bluetooth (RuntimeError)."}"#,
            r#"{"status":"stopping"}"#,
            r#"{"status":"error","message":"FRENZ shutdown failed"}"#,
            r#"{"status":"stopped"}"#,
        ] {
            status.apply_python_status(serde_json::from_str(line).unwrap());
        }
        status.mark_process_exit("exit status: 1", false);
        assert_eq!(status.state, FrenzBridgeState::Error);
        assert_eq!(status.phase.as_deref(), Some("connecting_bluetooth"));
        assert_eq!(
            status.message.as_deref(),
            Some("FRENZ failed during connecting_bluetooth (RuntimeError).")
        );
        assert_eq!(status.sample_count, 123);
        // A shutdown failure is still the primary error when no earlier failure exists.
        let mut clean = FrenzBridgeStatus::default();
        clean.apply_python_status(
            serde_json::from_str(r#"{"status":"error","message":"FRENZ shutdown failed"}"#)
                .unwrap(),
        );
        assert_eq!(clean.message.as_deref(), Some("FRENZ shutdown failed"));
    }

    #[test]
    fn waiting_for_bluetooth_permission_says_what_to_do() {
        let mut status = FrenzBridgeStatus::default();
        status.apply_python_status(
            serde_json::from_str(
                r#"{"status":"connecting","phase":"waiting_for_bluetooth_permission","device_id":"FRENZI85"}"#,
            )
            .unwrap(),
        );
        assert_eq!(status.state, FrenzBridgeState::Connecting);
        assert_eq!(status.message.as_deref(), Some(BLUETOOTH_PERMISSION_PROMPT));
        status.apply_python_status(
            serde_json::from_str(
                r#"{"status":"connecting","phase":"sdk_start","device_id":"FRENZI85"}"#,
            )
            .unwrap(),
        );
        assert_eq!(status.message.as_deref(), Some("Connecting to FRENZI85..."));
    }

    #[test]
    fn unexpected_exit_clears_active_streams_and_reports_failure() {
        let mut status = FrenzBridgeStatus::default();
        status.state = FrenzBridgeState::Streaming;
        status.streams = vec!["_EEG_raw".into()];
        status.sample_count = 123;
        status.mark_process_exit("exit status: 0", false);
        assert_eq!(status.state, FrenzBridgeState::Error);
        assert!(status.streams.is_empty());
        assert_eq!(status.sample_count, 123);
        assert!(status.message.unwrap().contains("unexpectedly"));
    }

    #[test]
    fn process_exit_preserves_the_original_sdk_error() {
        let mut status = FrenzBridgeStatus::default();
        status.state = FrenzBridgeState::Error;
        status.message = Some("FRENZ failed during publishing_samples (TimeoutError).".into());
        let expected = status.message.clone();
        status.mark_process_exit("exit status: 1", false);
        assert_eq!(status.message, expected);
        assert_eq!(status.state, FrenzBridgeState::Error);
    }

    #[test]
    fn requested_shutdown_is_not_an_unexpected_failure() {
        let mut status = FrenzBridgeStatus::default();
        status.state = FrenzBridgeState::Streaming;
        status.mark_process_exit("exit status: 0", true);
        assert_eq!(status.state, FrenzBridgeState::Stopped);
        assert!(status.message.is_none());
    }

    #[test]
    fn test_default_status() {
        let status = FrenzBridgeStatus::default();
        assert_eq!(status.state, FrenzBridgeState::Stopped);
        assert!(status.message.is_none());
        assert!(status.streams.is_empty());
        assert_eq!(status.sample_count, 0);
    }

    #[test]
    fn test_status_serialization() {
        let status = FrenzBridgeStatus {
            state: FrenzBridgeState::Streaming,
            message: None,
            phase: None,
            streams: vec!["_EEG_raw".to_string(), "_PPG_raw".to_string()],
            sample_count: 42,
            timing: None,
            clock_pair: None,
        };
        let json = serde_json::to_string(&status).unwrap();
        assert!(json.contains("streaming"));
        assert!(json.contains("_EEG_raw"));
    }

    #[test]
    fn test_parse_python_status_line() {
        let line = r#"{"status":"streaming","streams":["_EEG_raw","_PPG_raw"],"sample_count":100}"#;
        let parsed: PythonStatusLine = serde_json::from_str(line).unwrap();
        assert_eq!(parsed.status, "streaming");
        assert_eq!(parsed.streams.unwrap().len(), 2);
        assert_eq!(parsed.sample_count.unwrap(), 100);
    }

    #[test]
    fn test_parse_error_status() {
        let line = r#"{"status":"error","message":"BLE connection failed"}"#;
        let parsed: PythonStatusLine = serde_json::from_str(line).unwrap();
        assert_eq!(parsed.status, "error");
        assert_eq!(parsed.message.unwrap(), "BLE connection failed");
    }

    #[test]
    fn test_parse_bootstrapping_status() {
        let line = r#"{"status":"bootstrapping","phase":"installing","package":"tensorflow","progress":45}"#;
        let parsed: PythonStatusLine = serde_json::from_str(line).unwrap();
        assert_eq!(parsed.status, "bootstrapping");
        assert_eq!(parsed.phase.unwrap(), "installing");
        assert_eq!(parsed.package.unwrap(), "tensorflow");
        assert_eq!(parsed.progress.unwrap(), 45);
    }

    #[test]
    fn test_manager_creation() {
        let manager = FrenzProcessManager::new();
        let _ = format!("{:?}", manager);
    }

    #[tokio::test]
    async fn test_initial_status() {
        let manager = FrenzProcessManager::new();
        let status = manager.get_status().await;
        assert_eq!(status.state, FrenzBridgeState::Stopped);
    }

    #[test]
    fn test_platform_check() {
        // On any platform, this should not panic
        if cfg!(target_os = "linux") {
            // Linux should be detected as unsupported
        } else {
            // macOS/Windows — binary may or may not exist, but check shouldn't panic
        }
    }
}

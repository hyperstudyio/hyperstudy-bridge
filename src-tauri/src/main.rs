#![cfg_attr(
    all(not(debug_assertions), target_os = "windows"),
    windows_subsystem = "windows"
)]

use std::sync::{
    atomic::{AtomicU8, Ordering},
    Arc,
};
use tauri::menu::{MenuBuilder, MenuItemBuilder, SubmenuBuilder};
use tauri::{Emitter, Manager};
use tracing::{error, info, warn};
use tracing_subscriber::prelude::*;
use tracing_subscriber::EnvFilter;

use hyperstudy_bridge::bridge::{AppState, BridgeServer};
use hyperstudy_bridge::commands::{self, *};
use hyperstudy_bridge::logging::{
    init_log_emitter, init_log_persister, set_app_handle, TauriLogLayer,
};
use hyperstudy_bridge::{open_files, secrets, storage, updates, usb_monitor};

/// Stops the FRENZ sidecar when Bridge exits, after the recording is finalized. Left running, it stayed connected
/// to the band over Bluetooth, so no later Bridge could connect to it. Runs on a fresh thread: the sidecar belongs to
/// Tauri's runtime, and the Exit callback is inside a tokio context where blocking on it would panic.
fn stop_frenz_sidecar(state: &Arc<AppState>) {
    let manager = state.frenz_process.clone();
    match std::thread::spawn(move || tauri::async_runtime::block_on(manager.stop_for_exit())).join()
    {
        Ok(Err(error)) => error!("FRENZ sidecar exit stop failed: {error}"),
        Err(_) => error!("FRENZ sidecar exit stop panicked"),
        Ok(Ok(())) => {}
    }
}

/// Disconnects a band held through openfrenz when Bridge exits, so the band stops streaming and can be connected
/// to again. On a fresh thread for the same reason as stop_frenz_sidecar.
fn disconnect_frenz_native_for_exit(state: &Arc<AppState>) {
    let native = state.frenz_native.clone();
    if std::thread::spawn(move || tauri::async_runtime::block_on(native.stop()))
        .join()
        .is_err()
    {
        error!("FRENZ openfrenz exit stop panicked");
    }
}

/// Saves the log entries still buffered (up to 5 s / 100 of them), including the exit-time recording and helper
/// lines. Runs on a fresh thread for the same reason as stop_frenz_sidecar.
fn flush_logs_for_exit() {
    let _ = std::thread::spawn(|| {
        tauri::async_runtime::block_on(hyperstudy_bridge::logging::flush_logs())
    })
    .join();
}

#[tokio::main]
async fn main() {
    // Create filters to prevent log explosion from dependencies.
    // Every event that reaches the Tauri layer is also written to the log database. A dependency that logs
    // its own work (sqlx, which the log database once used, logged every INSERT) would then feed back into
    // itself, filling the database and consuming all memory, so only this crate's events get there.
    //
    // Console filter: Show our logs at INFO+, dependencies at WARN+
    let console_filter =
        EnvFilter::new("warn").add_directive("hyperstudy_bridge=info".parse().unwrap());

    // Tauri layer filter: ONLY log our crate's messages to frontend/database
    // This completely prevents dependency logs from being stored
    let tauri_filter =
        EnvFilter::new("off").add_directive("hyperstudy_bridge=info".parse().unwrap());

    // Initialize tracing with filtering to prevent log explosion from dependencies
    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer().with_filter(console_filter))
        .with(TauriLogLayer::new().with_filter(tauri_filter))
        .init();

    info!("Starting Hyperstudy Device Bridge");
    // Before any LSL inlet or outlet exists: every FRENZ stream's inlet needs sockets (see open_files.rs).
    match open_files::raise_open_file_limit() {
        Ok(Some(limit)) => info!("Raised open-file soft limit to {limit}"),
        Ok(None) => {}
        Err(error) => warn!(
            "Could not raise the open-file limit; connecting many LSL streams may fail: {error}"
        ),
    }

    let app_state = Arc::new(AppState::new());
    let state_clone = app_state.clone();
    let exit_state = app_state.clone();
    let shutdown = Arc::new(AtomicU8::new(0));

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .setup(move |app| {
            let state = state_clone.clone();
            let app_handle = app.handle().clone();

            state.recording.configure_study_directory(app_handle.path().app_data_dir()?.join("recordings"))?;

            // Set the app handle for the logging layer to enable event emission
            set_app_handle(app_handle.clone());

            // Initialize log emitter for batched frontend events
            init_log_emitter();

            // Saved device credentials live in the OS credential store. Before any page can ask for them, start
            // moving the keys out of the old Stronghold vault (the migration holds the store until it is done).
            // TODO: Remove after 2027-03-01 (Stronghold vault migration)
            let secrets_state = app.state::<secrets::SecretsState>();
            secrets_state.migrate_stronghold_vault(app_handle.path().app_data_dir()?.join("vault.hold"));

            // Initialize database storage
            let app_handle_for_db = app_handle.clone();
            tokio::spawn(async move {
                // Get app data directory for database
                let db_path = match app_handle_for_db.path().app_data_dir() {
                    Ok(dir) => dir.join("hyperstudy-bridge.db"),
                    Err(e) => {
                        error!("Failed to get app data directory: {}", e);
                        return;
                    }
                };

                // Initialize storage
                match storage::init_storage(&db_path).await {
                    Ok(storage) => {
                        info!("Database initialized at {:?}", db_path);

                        // Start a default session for logging
                        if let Err(e) = storage.start_session(None).await {
                            warn!("Failed to start default session: {}", e);
                        }

                        // Initialize log persister after storage is ready
                        init_log_persister();
                        info!("Log persistence enabled");
                    }
                    Err(e) => {
                        error!("Failed to initialize database: {}", e);
                        // App continues without persistence
                    }
                }

                // Check for an update on every launch. Runs after the database step (whatever its outcome) so
                // the result is persisted with the rest of the log.
                updates::check(&app_handle_for_db).await;
            });

            // Start WebSocket server
            let app_handle_for_ws = app_handle.clone();
            let state_for_ws = state.clone();
            tokio::spawn(async move {
                let mut server = BridgeServer::new(state_for_ws, app_handle_for_ws.clone());
                if let Err(e) = server.start().await {
                    // Without its server this Bridge cannot work, and running on would leave its page talking to
                    // whichever app holds the port. Say so and quit. (This can fire before log persistence starts,
                    // so the dialog is the record.)
                    error!("Failed to start WebSocket server: {}", e);
                    use tauri_plugin_dialog::{DialogExt, MessageDialogKind};
                    let exit_handle = app_handle_for_ws.clone();
                    app_handle_for_ws
                        .dialog()
                        .message(e.to_string())
                        .title("Hyperstudy Device Bridge can't start")
                        .kind(MessageDialogKind::Error)
                        .show(move |_| exit_handle.exit(1));
                }
            });

            // While a study recording runs, save each connected Neon phone's clock against this computer's.
            tokio::spawn(hyperstudy_bridge::recording::clock::sample_neon_phones(state.clone()));

            // Start USB device monitoring for TTL disconnect detection
            let state_for_usb = state.clone();
            tokio::spawn(async move {
                let mut usb_rx = usb_monitor::start_usb_monitor();
                info!("USB device monitoring started");

                while let Some(event) = usb_rx.recv().await {
                    if event.is_ttl_device() && event.is_disconnect() {
                        let port = event.port_name();
                        let serial = event.serial_number();
                        info!(
                            "TTL USB disconnect detected on port {} (S/N: {:?}), updating device status",
                            port, serial
                        );

                        // Unregisters the device and tells WebSocket clients (the page among them).
                        state_for_usb.handle_ttl_usb_disconnect().await;
                    }
                }
            });

            // Native menu: About, Check for Updates and Show Data Folder (macOS app menu; Help elsewhere).
            let check_updates_item =
                MenuItemBuilder::with_id("check_for_updates", "Check for Updates...")
                    .build(app)?;
            let data_folder_item =
                MenuItemBuilder::with_id("show_data_folder", "Show Data Folder").build(app)?;
            let docs_item =
                MenuItemBuilder::with_id("open_documentation", "Hyperstudy Device Bridge Documentation")
                    .build(app)?;

            #[cfg(target_os = "macos")]
            {
                let quit_item = MenuItemBuilder::with_id("quit_after_recording", "Quit Hyperstudy Device Bridge")
                    .accelerator("CmdOrCtrl+Q")
                    .build(app)?;
                let app_submenu = SubmenuBuilder::new(app, "Hyperstudy Device Bridge")
                    .about(None)
                    .item(&check_updates_item)
                    .item(&data_folder_item)
                    .separator()
                    .services()
                    .separator()
                    .hide()
                    .hide_others()
                    .show_all()
                    .separator()
                    .item(&quit_item)
                    .build()?;

                let edit_submenu = SubmenuBuilder::new(app, "Edit")
                    .undo()
                    .redo()
                    .separator()
                    .cut()
                    .copy()
                    .paste()
                    .select_all()
                    .build()?;

                let window_submenu = SubmenuBuilder::new(app, "Window")
                    .minimize()
                    .close_window()
                    .build()?;

                let help_submenu = SubmenuBuilder::new(app, "Help").item(&docs_item).build()?;

                let menu = MenuBuilder::new(app)
                    .items(&[&app_submenu, &edit_submenu, &window_submenu, &help_submenu])
                    .build()?;

                app.set_menu(menu)?;
            }

            #[cfg(not(target_os = "macos"))]
            {
                let help_submenu = SubmenuBuilder::new(app, "Help")
                    .item(&docs_item)
                    .separator()
                    .item(&check_updates_item)
                    .item(&data_folder_item)
                    .separator()
                    .about(None)
                    .build()?;

                let menu = MenuBuilder::new(app).items(&[&help_submenu]).build()?;

                app.set_menu(menu)?;
            }

            // Handle menu events — forward to frontend via Tauri event
            app.on_menu_event(move |app_handle, event| {
                if event.id().as_ref() == "quit_after_recording" {
                    app_handle.exit(0);
                }
                if event.id().as_ref() == "open_documentation" {
                    tauri::async_runtime::spawn_blocking(|| {
                        if let Err(e) = commands::open_documentation() {
                            warn!("Open documentation failed: {e}");
                        }
                    });
                }
                if event.id().as_ref() == "show_data_folder" {
                    let app_handle = app_handle.clone();
                    tauri::async_runtime::spawn(async move {
                        if let Err(e) = commands::reveal_data_folder(app_handle).await {
                            warn!("Show Data Folder failed: {e}");
                        }
                    });
                }
                if event.id().as_ref() == "check_for_updates" {
                    // The page shows the result; announce that this check was asked for.
                    let _ = app_handle.emit("menu-check-for-updates", ());
                    let app_handle = app_handle.clone();
                    tauri::async_runtime::spawn(async move {
                        updates::check(&app_handle).await;
                    });
                }
            });

            Ok(())
        })
        .manage(app_state)
        .manage(secrets::SecretsState::new(secrets::SecretStore::platform()))
        .manage(updates::UpdateState::default())
        .invoke_handler(tauri::generate_handler![
            send_ttl_pulse,
            list_ttl_devices,
            get_logs,
            query_logs,
            get_log_stats,
            get_storage_stats,
            clear_all_logs,
            export_logs,
            get_app_info,
            discover_recording_streams,
            start_lsl_recording,
            stop_lsl_recording,
            stop_study_recording,
            lsl_recording_status,
            list_study_recordings,
            reveal_recording,
            lsl_bridge_streams,
            update_status,
            install_update,
            recover_lsl_recording,
            scan_frenz_devices,
            start_frenz_bridge,
            get_frenz_bridge_status,
            check_frenz_bridge_available,
            scan_frenz_native,
            start_frenz_native,
            stop_frenz_native,
            get_frenz_native_status,
            get_frenz_native_recent,
            check_pipeline,
            preview_pipeline,
            stop_pipeline_preview,
            list_pipelines,
            save_pipeline,
            delete_pipeline,
            export_pipeline,
            secrets::get_secret,
            secrets::set_secrets
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(move |app, event| {
            // Native OS termination (for example the macOS Dock Quit action)
            // can bypass ExitRequested. The final Exit callback is synchronous.
            if matches!(event, tauri::RunEvent::Exit) {
                match exit_state.recording.stop() {
                    Ok(status) if status.state == "failed" => error!("Recording exit error: {:?}", status.error),
                    Err(error) => error!("Recording exit flush failed: {error}"),
                    _ => {}
                }
                stop_frenz_sidecar(&exit_state);
                disconnect_frenz_native_for_exit(&exit_state);
                flush_logs_for_exit();
            }
            if let tauri::RunEvent::ExitRequested { api, code, .. } = event {
                // Tauri exits the process without guaranteeing manager destructors.
                // Stop on a worker, keep the event loop responsive, then permit exit.
                if shutdown.load(Ordering::Acquire) == 2 { return; }
                api.prevent_exit();
                if shutdown.compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire).is_ok() {
                    let state = exit_state.clone();
                    let handle = app.clone();
                    let finished = shutdown.clone();
                    tauri::async_runtime::spawn_blocking(move || {
                        match state.recording.stop() {
                            Ok(status) if status.state == "failed" => error!("Recording stopped with error: {:?}", status.error),
                            Err(error) => error!("Recording finalization failed: {error}"),
                            _ => {}
                        }
                        stop_frenz_sidecar(&state);
                        disconnect_frenz_native_for_exit(&state);
                        flush_logs_for_exit();
                        finished.store(2, Ordering::Release);
                        handle.exit(code.unwrap_or(0));
                    });
                }
            }
        });
}

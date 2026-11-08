use crate::bridge::device_queues::{
    CommandHandler, DeviceQueues, Dispatch, QueuedCommand, DEVICE_QUEUE_CAPACITY, MAX_DEVICE_QUEUES,
};
use crate::bridge::message::{CommandAction, QueryTarget};
use crate::bridge::state::{ConnectAttempt, DeviceStatusEvent, DisconnectTarget};
use crate::bridge::{AppState, BridgeCommand, BridgeResponse, MessageHandler};
use crate::devices::{kernel::KernelDevice, mock::MockDevice, pupil::PupilDevice, ttl::TtlDevice};
use crate::devices::{BoxedDevice, Device, DeviceError};
use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{AppHandle, Runtime};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, RwLock};
use tokio_tungstenite::{accept_async, tungstenite::Message};
use tracing::{debug, error, info, warn};
use uuid::Uuid;

const WS_PORT: u16 = 9000;
#[allow(dead_code)]
const MAX_MESSAGE_SIZE: usize = 10 * 1024 * 1024; // 10MB

#[cfg(test)]
mod hybrid_config_tests {
    use super::*;

    #[test]
    fn raw_eeg_selects_bluetooth_and_sdk_comparison_with_focus() {
        let suffixes = map_frenz_config_to_suffixes(&json!({"streamEegRaw": true, "streamFocus": true}));
        assert_eq!(suffixes, vec!["_EEG_raw", "_EEG_bluetooth", "_focus"]);
        assert!(crate::devices::lsl::types::FRENZ_STREAM_SUFFIXES.contains(&"_EEG_bluetooth"));
    }
}

/// Maps frontend FRENZ config boolean flags to LSL stream suffixes.
fn map_frenz_config_to_suffixes(config: &serde_json::Value) -> Vec<String> {
    let mapping: &[(&str, &str)] = &[
        ("streamEegRaw", "_EEG_raw"),
        ("streamEegRaw", "_EEG_bluetooth"),
        ("streamEegFiltered", "_EEG_filtered"),
        ("streamEogFiltered", "_EOG_filtered"),
        ("streamEmgFiltered", "_EMG_filtered"),
        ("streamPpgRaw", "_PPG_raw"),
        ("streamImuRaw", "_IMU_raw"),
        ("streamHrRaw", "_HR_raw"),
        ("streamSpo2Raw", "_SPO2_raw"),
        ("streamFirmware", "_EEG_firmware"),
        ("streamFirmware", "_PPG_firmware"),
        ("streamFirmware", "_IMU_firmware"),
        ("streamFirmware", "_HR_firmware"),
        ("streamFirmware", "_SPO2_firmware"),
        ("streamFocus", "_focus"),
        ("streamSleepStage", "_sleep_stage"),
        ("streamPoas", "_poas"),
        ("streamPosture", "_POSTURE"),
        ("streamSignalQuality", "_signal_quality"),
        ("streamAlpha", "_alpha"),
        ("streamBeta", "_beta"),
        ("streamTheta", "_theta"),
        ("streamGamma", "_gamma"),
        ("streamDelta", "_delta"),
    ];

    mapping
        .iter()
        .filter(|(key, _)| config.get(key).and_then(|v| v.as_bool()).unwrap_or(false))
        .map(|(_, suffix)| suffix.to_string())
        .collect()
}

/// Helper to send a response with error logging if send fails
async fn send_response(tx: &mpsc::Sender<BridgeResponse>, response: BridgeResponse) {
    if let Err(e) = tx.send(response).await {
        warn!("Failed to send WebSocket response: {}", e);
    }
}

pub struct BridgeServer<R: Runtime> {
    state: Arc<AppState>,
    app_handle: AppHandle<R>,
}

impl<R: Runtime> BridgeServer<R> {
    pub fn new(state: Arc<AppState>, app_handle: AppHandle<R>) -> Self {
        Self { state, app_handle }
    }

    pub async fn start(&mut self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let addr: SocketAddr = ([127, 0, 0, 1], WS_PORT).into();
        let listener = TcpListener::bind(&addr).await?;

        info!("WebSocket server listening on ws://{}", addr);

        while let Ok((stream, peer_addr)) = listener.accept().await {
            info!("New connection from: {}", peer_addr);

            let state = self.state.clone();
            let app_handle = self.app_handle.clone();

            tokio::spawn(async move {
                if let Err(e) = handle_connection(stream, peer_addr, state, app_handle).await {
                    error!("Error handling connection from {}: {}", peer_addr, e);
                }
            });
        }

        Ok(())
    }
}

async fn handle_connection<R: Runtime>(
    stream: TcpStream,
    peer_addr: SocketAddr,
    state: Arc<AppState>,
    app_handle: AppHandle<R>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let ws_stream = accept_async(stream).await?;
    let connection_id = Uuid::new_v4().to_string();

    state.add_connection(connection_id.clone(), peer_addr.to_string());

    let (mut ws_sender, mut ws_receiver) = ws_stream.split();
    let (tx, mut rx) = mpsc::channel::<BridgeResponse>(100);

    let state_clone = state.clone();
    let connection_id_clone = connection_id.clone();

    // Subscribe to device status broadcasts
    let mut status_rx = state.subscribe_device_status();
    let tx_for_status = tx.clone();

    // Task to forward device status broadcasts to this WebSocket client
    let status_task = tokio::spawn(async move {
        loop {
            match status_rx.recv().await {
                Ok(event) => {
                    // Convert DeviceStatusEvent to BridgeResponse. Device metadata
                    // (Neon device_id/IP, recording state…) rides along with the
                    // reason so every client can see which hardware answered.
                    let mut info =
                        json!({ "reason": event.reason, "device_type": event.device_type });
                    if let Some(extra) = event.info.as_ref().and_then(|v| v.as_object()) {
                        for (k, v) in extra {
                            info[k] = v.clone();
                        }
                    }
                    let response = BridgeResponse::status_with_info(
                        event.device_id,
                        event.status,
                        info,
                        None, // No request ID for broadcast events
                    );
                    if tx_for_status.send(response).await.is_err() {
                        // Channel closed, client disconnected
                        break;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    // Broadcast channel closed, exit
                    break;
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    // We missed some messages due to slow processing
                    warn!("WebSocket client lagged behind, missed {} status events", n);
                }
            }
        }
    });

    let send_task = tokio::spawn(async move {
        while let Some(response) = rx.recv().await {
            if let Ok(msg) = MessageHandler::serialize_response(&response) {
                if ws_sender.send(Message::Text(msg.into())).await.is_err() {
                    break;
                }
            }
        }
    });

    // Shared flag to signal the receive task to stop gracefully after
    // finishing its current message (avoids aborting mid-handle_command).
    let should_stop = Arc::new(AtomicBool::new(false));
    let should_stop_clone = should_stop.clone();

    // Set once this connection is gone. Device queue workers check it before
    // starting each queued command so a reconnect-and-retry can't double-send
    // markers that were still buffered when the socket closed.
    let connection_closed = Arc::new(AtomicBool::new(false));
    let connection_closed_for_workers = connection_closed.clone();

    let receive_task = tokio::spawn(async move {
        // Per-device FIFO queues (see bridge::device_queues): every device
        // command runs on the queue of the resource it mutates, in arrival
        // order; devices never wait on each other; queues are bounded; and
        // commands still queued when this connection closes are dropped.
        let handler: CommandHandler = {
            let state = state_clone.clone();
            let tx = tx.clone();
            let conn = Arc::new(ConnectionCtx::new(connection_closed_for_workers.clone()));
            Arc::new(move |cmd: QueuedCommand| {
                let state = state.clone();
                let tx = tx.clone();
                let conn = conn.clone();
                Box::pin(async move {
                    handle_device_command(
                        &state,
                        cmd.device,
                        cmd.action,
                        cmd.payload,
                        cmd.id,
                        &tx,
                        &conn,
                    )
                    .await
                })
            })
        };
        let mut device_queues = DeviceQueues::new(connection_closed_for_workers.clone(), handler);

        while let Some(msg) = ws_receiver.next().await {
            if should_stop_clone.load(Ordering::Relaxed) {
                debug!("Receive task stopping: send channel closed");
                break;
            }
            match msg {
                Ok(Message::Text(text)) => {
                    state_clone.update_connection_activity(&connection_id_clone);
                    state_clone.increment_message_count();
                    // Payloads can contain short-lived upload capabilities and device secrets.
                    info!(bytes = text.len(), "Received WebSocket message");

                    match MessageHandler::parse_command(&text) {
                        Ok(BridgeCommand::Command {
                            device,
                            action,
                            payload,
                            id,
                        }) => {
                            info!("Parsed command successfully");
                            match device_queues.dispatch(QueuedCommand {
                                device,
                                action,
                                payload,
                                id,
                            }) {
                                Dispatch::Queued => {}
                                Dispatch::Full(cmd) => {
                                    send_response(
                                        &tx,
                                        BridgeResponse::device_error(
                                            cmd.device,
                                            format!(
                                                "Command queue for this device is full ({} pending) — the device is not keeping up; check its connection in the Bridge",
                                                DEVICE_QUEUE_CAPACITY
                                            ),
                                            cmd.id,
                                        ),
                                    )
                                    .await;
                                }
                                Dispatch::TooManyQueues(cmd) => {
                                    warn!(
                                        "Too many distinct devices on one connection; rejecting {}",
                                        cmd.device
                                    );
                                    send_response(
                                        &tx,
                                        BridgeResponse::device_error(
                                            cmd.device,
                                            format!(
                                                "Too many distinct devices on this connection (limit {})",
                                                MAX_DEVICE_QUEUES
                                            ),
                                            cmd.id,
                                        ),
                                    )
                                    .await;
                                }
                                Dispatch::WorkerGone(cmd) => {
                                    warn!(
                                        "Device queue worker for {} gone; command rejected",
                                        cmd.device
                                    );
                                    send_response(
                                        &tx,
                                        BridgeResponse::device_error(
                                            cmd.device,
                                            "Bridge could not queue this command; retry"
                                                .to_string(),
                                            cmd.id,
                                        ),
                                    )
                                    .await;
                                }
                            }
                        }
                        Ok(command) => {
                            info!("Parsed command successfully");
                            handle_command(command, &state_clone, &tx, &app_handle).await;
                        }
                        Err(e) => {
                            warn!("Failed to parse command: {}", e);
                            send_response(&tx, BridgeResponse::error(e, None)).await;
                        }
                    }
                }
                Ok(Message::Binary(_bin)) => {
                    warn!("Binary messages not supported");
                    send_response(
                        &tx,
                        BridgeResponse::error("Binary messages not supported".to_string(), None),
                    )
                    .await;
                }
                Ok(Message::Ping(_data)) => {
                    debug!("Received ping, sending pong");
                }
                Ok(Message::Pong(_)) => {
                    debug!("Received pong");
                }
                Ok(Message::Close(_)) => {
                    info!("Client {} disconnected", peer_addr);
                    break;
                }
                Err(e) => {
                    error!("WebSocket error: {}", e);
                    break;
                }
                _ => {}
            }
        }
        // Set here, on every exit path of the reader (peer Close, error,
        // should_stop), not only in the outer select! after this task is
        // joined — otherwise a queued command could still start in between.
        connection_closed_for_workers.store(true, Ordering::Relaxed);
    });

    // Wait for either task to complete, then shut down the others.
    // When send_task exits (frontend disconnected), we signal receive_task
    // to stop gracefully so it can finish processing any in-flight command
    // (e.g., a Kernel event send) before shutting down.
    tokio::pin!(send_task);
    tokio::pin!(receive_task);
    tokio::pin!(status_task);

    tokio::select! {
        result = &mut send_task => {
            debug!("Send task completed: {:?}, draining receive task", result);
            // Nothing can reach this client any more: stop queued work first,
            // then let the command in flight finish.
            connection_closed.store(true, Ordering::Relaxed);
            should_stop.store(true, Ordering::Relaxed);
            // Give the reader up to 10 s to notice should_stop; if it is still
            // parked in ws_receiver.next() after that, abort it — a detached
            // reader would keep the queues and the response channel alive until
            // a frame happened to arrive. (A device command already running in
            // a queue worker is not cancelled: device I/O cannot be safely
            // interrupted mid-operation; it finishes on its own and its response
            // is dropped.)
            if tokio::time::timeout(Duration::from_secs(10), &mut receive_task)
                .await
                .is_err()
            {
                receive_task.abort();
            }
            status_task.abort();
        },
        result = &mut receive_task => {
            debug!("Receive task completed: {:?}", result);
            connection_closed.store(true, Ordering::Relaxed);
            send_task.abort();
            status_task.abort();
        },
        result = &mut status_task => {
            debug!("Status task completed: {:?}", result);
            connection_closed.store(true, Ordering::Relaxed);
            send_task.abort();
            receive_task.abort();
        },
    }

    connection_closed.store(true, Ordering::Relaxed);
    state.remove_connection(&connection_id);
    info!("Connection {} closed", connection_id);

    Ok(())
}

/// A bridge restart or reconnect loses the in-memory "this bridge started that
/// recording" flag, after which the bridge's own recording reads as busy and
/// cannot be stopped without force. Restore ownership when the phone we just
/// connected to is still running the recording AppState remembers starting.
fn reclaim_pupil_recording(state: &Arc<AppState>, device: &mut Box<dyn Device>) {
    let probe = device.get_info().metadata;
    let phone = probe
        .get("device_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let Some(active) = probe.get("recording_id").and_then(|v| v.as_str()) else {
        return;
    };
    let Some(remembered) = state.owned_recording(phone) else {
        return;
    };
    if remembered != active {
        return;
    }
    if let Some(pupil) = device
        .as_any_mut()
        .and_then(|any| any.downcast_mut::<PupilDevice>())
    {
        pupil.claim_recording_ownership(active);
        info!(
            "Restored ownership of Neon recording {} on phone {} after reconnect",
            active, phone
        );
    }
}

/// Re-create and connect a device from the payload that last connected it, so a
/// `send`/`send_event` to a device that dropped out of the registry (Kernel app
/// restarted, bridge UI disconnect, …) can succeed instead of being rejected.
///
/// Kernel only for now: it is `127.0.0.1` per station, so reconnecting cannot
/// pick up the wrong hardware. Neon deliberately stays manual — auto-connecting
/// a phone by remembered address is exactly the identity mistake the pinning
/// work exists to prevent.
/// A connect that replaced an already-registered device (e.g. one the
/// auto-reconnect path registered from remembered configuration) must not
/// leave the old hardware connection open. Done in the background so the
/// connect response is not delayed by the old device's teardown.
fn disconnect_replaced(device_id: &str, replaced: Option<Arc<RwLock<BoxedDevice>>>) {
    if let Some(old) = replaced {
        let device_id = device_id.to_string();
        tokio::spawn(async move {
            info!(
                "{} re-registered by a newer connect; disconnecting the replaced device",
                device_id
            );
            let _ = old.write().await.disconnect().await;
        });
    }
}

async fn try_auto_connect(state: &Arc<AppState>, device_id: &str) -> bool {
    if device_id != "kernel" {
        return false;
    }
    let Some(config) = state.last_connect_config(device_id) else {
        return false;
    };
    let Some(ip) = config
        .get("ip")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
    else {
        return false;
    };
    info!(
        "Auto-connecting {} from last known config ({})",
        device_id, ip
    );
    let mut device: Box<dyn Device> = Box::new(KernelDevice::new(ip.to_string()));
    match device.connect().await {
        Ok(_) => {
            state.record_connection_attempt(device_id, true).await;
            let info = device.get_info();
            // An explicit connect may have won the race (registered already, or
            // still in flight — started before or after us); never replace or
            // pre-empt it, and close the orphan's hardware connection cleanly.
            if let Err(mut orphan) = state
                .add_device_if_absent(device_id.to_string(), device)
                .await
            {
                info!(
                    "{} was connected explicitly while auto-connecting; using that",
                    device_id
                );
                let _ = orphan.disconnect().await;
                return true;
            }
            state.broadcast_device_status(DeviceStatusEvent::connected_with_info(
                device_id.to_string(),
                info.device_type,
                "auto-reconnected on send",
                info.metadata.clone(),
            ));
            true
        }
        Err(e) => {
            state.record_connection_attempt(device_id, false).await;
            warn!("Auto-connect of {} failed: {}", device_id, e);
            false
        }
    }
}

async fn handle_command<R: Runtime>(
    command: BridgeCommand,
    state: &Arc<AppState>,
    tx: &mpsc::Sender<BridgeResponse>,
    _app_handle: &AppHandle<R>,
) {
    match command {
        BridgeCommand::Command {
            device,
            action,
            payload,
            id,
        } => {
            // Commands normally arrive via the per-device queues in the receive
            // loop; this direct path has no owning connection (never closed,
            // no connect history).
            let conn = Arc::new(ConnectionCtx::new(Arc::new(AtomicBool::new(false))));
            handle_device_command(state, device, action, payload, id, tx, &conn).await;
        }
        BridgeCommand::Query { target, id } => {
            handle_query(state, target, id, tx).await;
        }
        BridgeCommand::Subscribe { device, events } => {
            send_response(
                tx,
                BridgeResponse::event(
                    device,
                    "subscribed".to_string(),
                    json!({ "events": events }),
                ),
            )
            .await;
        }
        BridgeCommand::Unsubscribe { device, events } => {
            send_response(
                tx,
                BridgeResponse::event(
                    device,
                    "unsubscribed".to_string(),
                    json!({ "events": events }),
                ),
            )
            .await;
        }
    }
}

/// What a device-command handler needs to know about the WebSocket connection
/// it is running for.
pub struct ConnectionCtx {
    /// Set once the connection is gone. A connect that was already running
    /// when its connection went away must not register its device afterwards —
    /// a newer connection may have selected a different phone by then.
    pub closed: Arc<AtomicBool>,
    /// The newest connect ticket this connection issued per device id (see
    /// `AppState::begin_connect`). A Disconnect from this connection is refused
    /// when a *newer* connect has registered the device since — that device is
    /// not ours to remove, even if this connection is not yet known to be dead.
    connect_tickets: Mutex<HashMap<String, u64>>,
}

impl ConnectionCtx {
    pub fn new(closed: Arc<AtomicBool>) -> Self {
        Self {
            closed,
            connect_tickets: Mutex::new(HashMap::new()),
        }
    }

    fn remember_connect_ticket(&self, device_id: &str, ticket: u64) {
        if let Ok(mut tickets) = self.connect_tickets.lock() {
            tickets.insert(device_id.to_string(), ticket);
        }
    }

    fn connect_ticket(&self, device_id: &str) -> Option<u64> {
        self.connect_tickets
            .lock()
            .ok()
            .and_then(|tickets| tickets.get(device_id).copied())
    }
}

async fn handle_device_command(
    state: &Arc<AppState>,
    device_id: String,
    action: CommandAction,
    payload: Option<serde_json::Value>,
    id: Option<String>,
    tx: &mpsc::Sender<BridgeResponse>,
    conn: &Arc<ConnectionCtx>,
) {
    let device_id = device_id.to_lowercase();
    info!(
        "Handling device command: device={}, action={:?}",
        device_id, action
    );
    match action {
        CommandAction::Connect => {
            info!("Processing connect for device: {}", device_id);
            let device_type = MessageHandler::validate_device_type(&device_id);

            if device_type.is_none() {
                warn!("Invalid device type: {}", device_id);
                send_response(
                    tx,
                    BridgeResponse::device_error(device_id, "Invalid device type".to_string(), id),
                )
                .await;
                return;
            }

            let config = if let Some(p) = payload { p } else { json!({}) };

            // Ticket once the attempt is known to be viable (valid device type)
            // and before any await: whichever connect STARTED last is the one
            // allowed to register (see AppState::add_device_if_latest). A newer
            // attempt that later fails still supersedes this one on purpose —
            // the operator has moved on; what it selected is what should be
            // registered, and its failure is reported to it.
            let connect_ticket = state.begin_connect(&device_id).await;
            conn.remember_connect_ticket(&device_id, connect_ticket);
            // Marks the ticket finished on every exit that does not register.
            let mut attempt = ConnectAttempt::new(state.clone(), &device_id, connect_ticket);

            // FRENZ: orchestrated connect via FrenzLslManager (not the Device trait)
            if device_id == "frenz" {
                let selected_streams = map_frenz_config_to_suffixes(&config);

                // Discover FRENZ devices on the LSL network
                let devices = match state.frenz_manager.discover_frenz_devices().await {
                    Ok(d) => d,
                    Err(e) => {
                        send_response(
                            tx,
                            BridgeResponse::device_error(
                                device_id,
                                format!("FRENZ discovery failed: {}", e),
                                id,
                            ),
                        )
                        .await;
                        return;
                    }
                };

                if devices.is_empty() {
                    send_response(
                        tx,
                        BridgeResponse::device_error(
                            device_id,
                            "No FRENZ devices found. Is the FRENZ bridge running?".to_string(),
                            id,
                        ),
                    )
                    .await;
                    return;
                }

                let requested = config.get("device_name").and_then(|v| v.as_str());
                let selected = match requested {
                    Some(name) => devices.iter().find(|d| d.device_name == name),
                    None if devices.len() == 1 => devices.first(),
                    _ => None,
                };
                let Some(selected) = selected else {
                    send_response(
                        tx,
                        BridgeResponse::device_error(
                            "frenz".into(),
                            "Select the intended FRENZ device; no unique matching device was found"
                                .into(),
                            id,
                        ),
                    )
                    .await;
                    return;
                };
                let device_name = &selected.device_name;
                info!("FRENZ connect: using device '{}'", device_name);

                // Create marker outlet if requested
                let enable_markers = config
                    .get("enableMarkerOutlet")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(true);

                if enable_markers {
                    if let Err(e) = state.frenz_manager.create_marker_outlet().await {
                        warn!("Failed to create FRENZ marker outlet: {}", e);
                    }
                }

                // Connect to selected streams
                match state
                    .frenz_manager
                    .connect_streams(device_name, &selected_streams)
                    .await
                {
                    Ok(receivers) => {
                        // Note: no dead-connection rollback here. FrenzLslManager is a
                        // process-wide singleton without per-consumer ownership, so a
                        // teardown issued by a connection that turned out to be dead
                        // could destroy streams a newer client has since opened. The
                        // streams stay until the next explicit FRENZ connect/disconnect.
                        let connected_count = receivers.len();
                        info!(
                            "FRENZ connected {} streams for '{}'",
                            connected_count, device_name
                        );

                        // Spawn forwarding tasks for each stream receiver. They stop
                        // when the client is gone (send failure, or the closed flag
                        // for a quiet stream); the streams themselves are released by
                        // the next explicit FRENZ connect/disconnect.
                        for (suffix, mut rx) in receivers {
                            let tx_clone = tx.clone();
                            let device_name_clone = device_name.to_string();
                            let suffix_clone = suffix.clone();
                            let closed = conn.closed.clone();

                            tokio::spawn(async move {
                                loop {
                                    tokio::select! {
                                        item = rx.recv() => match item {
                                            Some(sample) => {
                                                let response = BridgeResponse::data(
                                                    format!("frenz_{}", device_name_clone),
                                                    json!({
                                                        "type": "frenz_data",
                                                        "stream": suffix_clone,
                                                        "timestamp": sample.timestamp,
                                                        "data": sample.wire_data(),
                                                        "values": sample.values,
                                                        "string_value": sample.string_value,
                                                    }),
                                                    None,
                                                );
                                                if tx_clone.send(response).await.is_err() {
                                                    break;
                                                }
                                            }
                                            None => break,
                                        },
                                        _ = tokio::time::sleep(Duration::from_secs(1)) => {
                                            if closed.load(Ordering::Relaxed) {
                                                break;
                                            }
                                        }
                                    }
                                }
                            });
                        }

                        // Send status response so frontend resolves connectDevice() and updates status dot
                        send_response(
                            tx,
                            BridgeResponse::status(
                                device_id,
                                crate::devices::DeviceStatus::Connected,
                                id,
                            ),
                        )
                        .await;
                    }
                    Err(e) => {
                        send_response(
                            tx,
                            BridgeResponse::device_error(device_id, e.to_string(), id),
                        )
                        .await;
                    }
                }
                return;
            }

            let mut device: Box<dyn crate::devices::Device> = match device_id.as_str() {
                "ttl" => {
                    // Require explicit port configuration - no unsafe defaults
                    let port = match config.get("port").and_then(|v| v.as_str()) {
                        Some(p) if !p.is_empty() => p,
                        _ => {
                            send_response(
                                tx,
                                BridgeResponse::device_error(
                                    device_id.clone(),
                                    "TTL device requires 'port' in config (e.g., /dev/cu.usbmodem101)".to_string(),
                                    id,
                                ),
                            )
                            .await;
                            return;
                        }
                    };
                    let mut ttl_device = TtlDevice::new(port.to_string());
                    if let Some(duration) = config.get("pulse_duration_ms").and_then(|v| v.as_u64())
                    {
                        let device_config = crate::devices::DeviceConfig {
                            custom_settings: serde_json::json!({ "pulse_duration_ms": duration }),
                            ..Default::default()
                        };
                        let _ = ttl_device.configure(device_config);
                    }
                    Box::new(ttl_device)
                }
                "kernel" => {
                    // Require explicit IP configuration - no unsafe defaults
                    let ip = match config.get("ip").and_then(|v| v.as_str()) {
                        Some(i) if !i.is_empty() => i,
                        _ => {
                            send_response(
                                tx,
                                BridgeResponse::device_error(
                                    device_id.clone(),
                                    "Kernel device requires 'ip' in config (e.g., 192.168.1.100)"
                                        .to_string(),
                                    id,
                                ),
                            )
                            .await;
                            return;
                        }
                    };
                    Box::new(KernelDevice::new(ip.to_string()))
                }
                "pupil" => {
                    // Three ways in, most to least specific:
                    //   {device_id}             pinned phone, IP resolved over mDNS
                    //   {url, device_id}        given address, but the answering phone
                    //                           must have this hardware id
                    //   {url}                   legacy: trust whatever answers
                    let url = config
                        .get("url")
                        .and_then(|v| v.as_str())
                        .filter(|u| !u.is_empty())
                        .map(str::to_string);
                    let pinned_id = config
                        .get("device_id")
                        .or_else(|| config.get("expected_device_id"))
                        .and_then(|v| v.as_str())
                        .filter(|s| !s.is_empty())
                        .map(str::to_string);

                    let host = match (url, pinned_id.as_deref()) {
                        (Some(u), None) => u,
                        (Some(u), Some(pin)) => {
                            // A pinned phone is identified by its hardware id; the
                            // saved address is only a fallback (DHCP moves phones).
                            match crate::devices::neon_discovery::find_neon_phone(
                                pin,
                                Duration::from_secs(3),
                            )
                            .await
                            {
                                Ok(Some(phone)) => {
                                    info!(
                                        "Pinned Neon {} resolved via mDNS to {} (saved address {})",
                                        pin, phone.ip, u
                                    );
                                    phone.host()
                                }
                                _ => {
                                    info!(
                                        "Pinned Neon {} not seen via mDNS; trying saved address {}",
                                        pin, u
                                    );
                                    u
                                }
                            }
                        }
                        (None, Some(pin)) => {
                            info!("Resolving pinned Neon {} over mDNS", pin);
                            match crate::devices::neon_discovery::find_neon_phone(
                                pin,
                                Duration::from_secs(5),
                            )
                            .await
                            {
                                Ok(Some(phone)) => {
                                    info!(
                                        "Pinned Neon {} is '{}' at {}",
                                        pin, phone.device_name, phone.ip
                                    );
                                    phone.host()
                                }
                                Ok(None) => {
                                    let err = DeviceError::DeviceNotFound(format!(
                                        "Neon {} is not advertising on this network — is the Companion app open and the phone on the same Wi-Fi?",
                                        pin
                                    ));
                                    state
                                        .record_device_error(&device_id, &err.to_string())
                                        .await;
                                    send_response(
                                        tx,
                                        BridgeResponse::device_error_from(
                                            device_id.clone(),
                                            &err,
                                            id,
                                        ),
                                    )
                                    .await;
                                    return;
                                }
                                Err(e) => {
                                    send_response(
                                        tx,
                                        BridgeResponse::device_error_from(
                                            device_id.clone(),
                                            &e,
                                            id,
                                        ),
                                    )
                                    .await;
                                    return;
                                }
                            }
                        }
                        (None, None) => {
                            send_response(
                                tx,
                                BridgeResponse::device_error(
                                    device_id.clone(),
                                    "Pupil device requires 'url' (e.g., 192.168.1.101:8080) or 'device_id' (phone hardware id) in config"
                                        .to_string(),
                                    id,
                                ),
                            )
                            .await;
                            return;
                        }
                    };
                    Box::new(PupilDevice::new(host).with_expected_device_id(pinned_id))
                }
                "mock" => Box::new(MockDevice::new(
                    format!("mock_{}", Uuid::new_v4()),
                    "Mock Device".to_string(),
                )),
                _ => {
                    send_response(
                        tx,
                        BridgeResponse::device_error(
                            device_id,
                            "Unsupported device type".to_string(),
                            id,
                        ),
                    )
                    .await;
                    return;
                }
            };

            match device.connect().await {
                Ok(_) => {
                    if device_id == "pupil" {
                        reclaim_pupil_recording(state, &mut device);
                    }
                    let info = device.get_info();
                    // Register only while this connection is alive — decided under
                    // the registry lock so a stale connect can never overwrite the
                    // device a newer connection registered meanwhile.
                    let replaced = match state
                        .add_device_if_latest(
                            device_id.clone(),
                            device,
                            connect_ticket,
                            &conn.closed,
                        )
                        .await
                    {
                        Ok(replaced) => replaced,
                        Err(mut orphan) => {
                            info!(
                            "Connect for {} superseded (newer connect or connection closed); device not registered",
                            device_id
                        );
                            let _ = orphan.disconnect().await;
                            send_response(
                                tx,
                                BridgeResponse::device_error(
                                    device_id,
                                    "Connect superseded by a newer connect".to_string(),
                                    id,
                                ),
                            )
                            .await;
                            return;
                        }
                    };
                    attempt.registered();
                    disconnect_replaced(&device_id, replaced);
                    state.record_connection_attempt(&device_id, true).await;
                    state.remember_connect_config(&device_id, config.clone());

                    // Tell every client, not just the requester. HyperStudy tracks
                    // device state from these broadcasts and previously only heard
                    // about USB unplugs — a Kernel disconnected from the bridge UI
                    // stayed "connected" in the web app for the whole session.
                    state.broadcast_device_status(DeviceStatusEvent::connected_with_info(
                        device_id.clone(),
                        info.device_type,
                        "connected",
                        info.metadata.clone(),
                    ));
                    send_response(
                        tx,
                        BridgeResponse::status_with_info(
                            device_id.clone(),
                            info.status,
                            info.metadata,
                            id,
                        ),
                    )
                    .await;
                }
                Err(e) => {
                    // Record failed connection attempt
                    state.record_connection_attempt(&device_id, false).await;
                    state.record_device_error(&device_id, &e.to_string()).await;

                    // Keep the machine-readable code (wrong_device, device_not_found…)
                    // so the client can tell an identity refusal from a plain failure.
                    send_response(
                        tx,
                        BridgeResponse::device_error_from(device_id.clone(), &e, id),
                    )
                    .await;
                }
            }
        }
        CommandAction::Disconnect => {
            // FRENZ: disconnect all LSL streams and stop the bridge process
            if device_id == "frenz" {
                info!("FRENZ disconnect: disconnecting all streams and stopping bridge");
                let (streams, process) = tokio::join!(
                    state.frenz_manager.disconnect_all(),
                    state.frenz_process.stop()
                );
                let errors: Vec<String> = [streams.err().map(|e| e.to_string()), process.err()]
                    .into_iter()
                    .flatten()
                    .collect();
                let response = if errors.is_empty() {
                    state.broadcast_device_status(DeviceStatusEvent::disconnected(
                        "frenz".into(),
                        crate::devices::DeviceType::LSL,
                        "FRENZ disconnected",
                    ));
                    BridgeResponse::status(
                        device_id,
                        crate::devices::DeviceStatus::Disconnected,
                        id,
                    )
                } else {
                    BridgeResponse::device_error(
                        device_id,
                        format!("FRENZ cleanup failed: {}", errors.join("; ")),
                        id,
                    )
                };
                send_response(tx, response).await;
                return;
            }

            // A disconnect from a connection whose connect has since been
            // superseded (the page reloaded and reconnected before the server
            // observed the old socket close) must not remove the newer device.
            // The check and the device selection are one atomic step (see
            // AppState::device_for_disconnect).
            let target = state
                .device_for_disconnect(&device_id, conn.connect_ticket(&device_id))
                .await;
            let device_lock = match target {
                DisconnectTarget::Stale => {
                    info!(
                        "Stale disconnect for {}: a newer connect owns the device; ignoring",
                        device_id
                    );
                    send_response(
                        tx,
                        BridgeResponse::device_error(
                            device_id,
                            "Device was reconnected by a newer connection; disconnect ignored"
                                .to_string(),
                            id,
                        ),
                    )
                    .await;
                    return;
                }
                DisconnectTarget::Absent => None,
                DisconnectTarget::Device(lock) => Some(lock),
            };

            // Minimize lock duration: acquire lock, do operation, release lock, then send responses
            let disconnect_result = if let Some(device_lock) = device_lock {
                let mut device = device_lock.write().await;
                let device_type = device.get_info().device_type;
                let result = device.disconnect().await;
                drop(device); // Explicitly release lock before async response handling
                Some((device_type, result, device_lock))
            } else {
                None
            };

            match disconnect_result {
                Some((device_type, Ok(_), disconnected_lock)) => {
                    // Unregister only the device we actually disconnected; if a
                    // newer connection registered a replacement meanwhile, leave it.
                    if state
                        .remove_device_if_same(&device_id, &disconnected_lock)
                        .await
                    {
                        state.broadcast_device_status(DeviceStatusEvent::disconnected(
                            device_id.clone(),
                            device_type,
                            "disconnected",
                        ));
                    } else {
                        info!(
                            "{} was re-registered while disconnecting; leaving the newer device",
                            device_id
                        );
                    }

                    send_response(
                        tx,
                        BridgeResponse::status(
                            device_id,
                            crate::devices::DeviceStatus::Disconnected,
                            id,
                        ),
                    )
                    .await;
                }
                Some((_, Err(e), _)) => {
                    send_response(tx, BridgeResponse::device_error_from(device_id, &e, id)).await;
                }
                None => {
                    send_response(
                        tx,
                        BridgeResponse::device_error(device_id, "Device not found".to_string(), id),
                    )
                    .await;
                }
            }
        }
        CommandAction::Send => {
            // Prepare data outside the lock.
            // Pupil device expects the full JSON payload for command routing
            // (e.g., {"command": "recording_start", "name": "...", "timestamp": ...}).
            // Other devices (TTL, Kernel) expect raw command strings (e.g., "PULSE").
            let data = if let Some(p) = payload {
                if device_id == "pupil" {
                    p.to_string().as_bytes().to_vec()
                } else if let Some(cmd) = p.get("command").and_then(|v| v.as_str()) {
                    cmd.as_bytes().to_vec()
                } else if let Some(data) = p.get("data").and_then(|v| v.as_str()) {
                    data.as_bytes().to_vec()
                } else {
                    p.to_string().as_bytes().to_vec()
                }
            } else {
                Vec::new()
            };

            // Minimize lock duration: acquire lock, do send, release lock, then send responses
            let send_result = if let Some(device_lock) = state.get_device(&device_id).await {
                let mut device = device_lock.write().await;
                let result = device.send(&data).await;
                // Device::send() discards the command's result, so surface the
                // post-command state (e.g. Neon recording_id / device_id) here.
                let info = device.get_info().metadata;
                drop(device); // Explicitly release lock before async response handling
                if device_id == "pupil" {
                    // Remember which recording we own so a reconnect can reclaim it.
                    state.note_pupil_recording_state(&info);
                }
                Some((info, result))
            } else {
                None
            };

            match send_result {
                Some((info, Ok(_))) => {
                    send_response(
                        tx,
                        BridgeResponse::data(
                            device_id,
                            json!({ "success": true, "message": "Data sent", "info": info }),
                            id,
                        ),
                    )
                    .await;
                }
                Some((_, Err(e))) => {
                    send_response(tx, BridgeResponse::device_error_from(device_id, &e, id)).await;
                }
                None => {
                    send_response(
                        tx,
                        BridgeResponse::device_error(device_id, "Device not found".to_string(), id),
                    )
                    .await;
                }
            }
        }
        CommandAction::TestConnection => {
            info!("Testing connection for device: {}", device_id);

            // Create a temporary device instance for testing
            // Require explicit configuration - no unsafe defaults
            let mut test_device: Box<dyn crate::devices::Device> = match device_id.as_str() {
                "ttl" => {
                    let port = match payload
                        .as_ref()
                        .and_then(|p| p.get("port"))
                        .and_then(|v| v.as_str())
                    {
                        Some(p) if !p.is_empty() => p,
                        _ => {
                            send_response(
                                tx,
                                BridgeResponse::device_error(
                                    device_id,
                                    "TTL test requires 'port' in config".to_string(),
                                    id,
                                ),
                            )
                            .await;
                            return;
                        }
                    };
                    Box::new(TtlDevice::new(port.to_string()))
                }
                "kernel" => {
                    let ip = match payload
                        .as_ref()
                        .and_then(|p| p.get("ip"))
                        .and_then(|v| v.as_str())
                    {
                        Some(i) if !i.is_empty() => i,
                        _ => {
                            send_response(
                                tx,
                                BridgeResponse::device_error(
                                    device_id,
                                    "Kernel test requires 'ip' in config".to_string(),
                                    id,
                                ),
                            )
                            .await;
                            return;
                        }
                    };
                    Box::new(KernelDevice::new(ip.to_string()))
                }
                "pupil" => {
                    let url = match payload
                        .as_ref()
                        .and_then(|p| p.get("url"))
                        .and_then(|v| v.as_str())
                    {
                        Some(u) if !u.is_empty() => u,
                        _ => {
                            send_response(
                                tx,
                                BridgeResponse::device_error(
                                    device_id,
                                    "Pupil test requires 'url' in config".to_string(),
                                    id,
                                ),
                            )
                            .await;
                            return;
                        }
                    };
                    Box::new(PupilDevice::new(url.to_string()))
                }
                "mock" => Box::new(MockDevice::new(
                    format!("mock_test_{}", Uuid::new_v4()),
                    "Mock Test Device".to_string(),
                )),
                _ => {
                    send_response(
                        tx,
                        BridgeResponse::device_error(
                            device_id,
                            "Unsupported device type".to_string(),
                            id,
                        ),
                    )
                    .await;
                    return;
                }
            };

            // Test the connection
            match test_device.test_connection().await {
                Ok(reachable) => {
                    info!(
                        "Connection test for {} device: {}",
                        device_id,
                        if reachable { "SUCCESS" } else { "FAILED" }
                    );

                    send_response(
                        tx,
                        BridgeResponse::data(
                            device_id.clone(),
                            json!({
                                "success": reachable,
                                "message": if reachable {
                                    format!("{} device is reachable", device_id)
                                } else {
                                    format!("{} device is not reachable", device_id)
                                }
                            }),
                            id,
                        ),
                    )
                    .await;
                }
                Err(e) => {
                    warn!("Connection test failed for {}: {}", device_id, e);

                    send_response(
                        tx,
                        BridgeResponse::device_error(device_id, format!("Test failed: {}", e), id),
                    )
                    .await;
                }
            }
        }
        CommandAction::SendEvent => {
            // Prepare event data outside lock
            let event = payload.unwrap_or_else(|| json!({}));

            // Minimize lock duration: acquire lock, do send_event, release lock, then send responses
            let mut device_lock = state.get_device(&device_id).await;
            if device_lock.is_none() && try_auto_connect(state, &device_id).await {
                device_lock = state.get_device(&device_id).await;
            }
            let send_result = if let Some(device_lock) = device_lock {
                let mut device = device_lock.write().await;
                let result = device.send_event(event).await;
                drop(device); // Explicitly release lock before async response handling
                Some(result)
            } else {
                None
            };

            match send_result {
                Some(Ok(response_data)) => {
                    send_response(tx, BridgeResponse::data(device_id, response_data, id)).await;
                }
                Some(Err(e)) => {
                    send_response(tx, BridgeResponse::device_error_from(device_id, &e, id)).await;
                }
                None => {
                    warn!("Device {} not connected, cannot send event", device_id);
                    send_response(
                        tx,
                        BridgeResponse::device_error(
                            device_id.clone(),
                            "Device not connected. Please connect first.".to_string(),
                            id,
                        ),
                    )
                    .await;
                }
            }
        }
        CommandAction::Status => {
            // FRENZ: return LSL manager stats
            if device_id == "frenz" {
                let stats = state.frenz_manager.get_stats().await;

                send_response(
                    tx,
                    BridgeResponse::data(
                        device_id,
                        json!({
                            "type": "status",
                            "stats": stats
                        }),
                        id,
                    ),
                )
                .await;
                return;
            }

            if device_id == "all" {
                // Return status for all registered devices
                let devices = state.list_devices().await;
                let statuses: Vec<serde_json::Value> = devices
                    .iter()
                    .map(|d| {
                        json!({
                            "device": d.id,
                            "device_type": d.device_type,
                            "status": d.status,
                            "name": d.name
                        })
                    })
                    .collect();

                send_response(
                    tx,
                    BridgeResponse::data("all".to_string(), json!({ "devices": statuses }), id),
                )
                .await;
            } else if let Some(device_lock) = state.get_device(&device_id).await {
                // Exact device ID match — include identity metadata (Neon device_id
                // and IP, recording_id, battery…) so the client can check *which*
                // hardware it is talking to, not just that something is connected.
                let info = device_lock.read().await.get_info();
                send_response(
                    tx,
                    BridgeResponse::status_with_info(device_id, info.status, info.metadata, id),
                )
                .await;
            } else {
                // Try matching by device type (e.g., "ttl" matches any TTL device)
                let devices = state.list_devices().await;
                let device_type_upper = device_id.to_uppercase();
                let matching_devices: Vec<_> = devices
                    .iter()
                    .filter(|d| format!("{:?}", d.device_type).to_uppercase() == device_type_upper)
                    .collect();

                if !matching_devices.is_empty() {
                    // Return status for all devices of this type
                    let statuses: Vec<serde_json::Value> = matching_devices
                        .iter()
                        .map(|d| {
                            json!({
                                "device": d.id,
                                "device_type": d.device_type,
                                "status": d.status,
                                "name": d.name
                            })
                        })
                        .collect();

                    send_response(
                        tx,
                        BridgeResponse::data(device_id, json!({ "devices": statuses }), id),
                    )
                    .await;
                } else {
                    send_response(
                        tx,
                        BridgeResponse::device_error(device_id, "Device not found".to_string(), id),
                    )
                    .await;
                }
            }
        }
        CommandAction::DiscoverNeonPhones => {
            // mDNS browse: every Companion phone on the LAN with its hardware id,
            // independent of LSL. Each is then probed for battery/recording so the
            // picker can show which phone is which — and whether it is already busy.
            let timeout_ms = payload
                .as_ref()
                .and_then(|p| p.get("timeout_ms"))
                .and_then(|v| v.as_u64())
                .unwrap_or(3000)
                .clamp(500, 15_000);
            info!("Discovering Neon phones via mDNS ({} ms)", timeout_ms);

            match crate::devices::neon_discovery::discover_neon_phones(Duration::from_millis(
                timeout_ms,
            ))
            .await
            {
                Ok(phones) => {
                    let probes = phones.iter().map(describe_neon_phone);
                    let described = futures_util::future::join_all(probes).await;
                    send_response(
                        tx,
                        BridgeResponse::data(
                            "pupil".to_string(),
                            json!({
                                "type": "phone_discovery",
                                "phones": described,
                                "count": phones.len(),
                            }),
                            id,
                        ),
                    )
                    .await;
                }
                Err(e) => {
                    warn!("Neon phone discovery failed: {}", e);
                    send_response(
                        tx,
                        BridgeResponse::device_error_from("pupil".to_string(), &e, id),
                    )
                    .await;
                }
            }
        }
        CommandAction::DiscoverNeon => {
            info!("Discovering Neon devices via LSL");

            match state.neon_manager.discover_neon_devices().await {
                Ok(devices) => {
                    let device_list: Vec<serde_json::Value> = devices
                        .iter()
                        .map(|d| {
                            json!({
                                "device_name": d.device_name,
                                "hostname": d.hostname,
                                "has_gaze_stream": d.has_gaze_stream,
                                "has_events_stream": d.has_events_stream,
                                "gaze_channel_count": d.gaze_channel_count,
                                "gaze_stream_uid": d.gaze_stream_uid,
                                "events_stream_uid": d.events_stream_uid,
                                "gaze_stream_ambiguous": d.gaze_stream_ambiguous,
                                "events_stream_ambiguous": d.events_stream_ambiguous,
                            })
                        })
                        .collect();

                    send_response(
                        tx,
                        BridgeResponse::data(
                            "neon_lsl".to_string(),
                            json!({
                                "type": "discovery",
                                "devices": device_list,
                                "count": devices.len()
                            }),
                            id,
                        ),
                    )
                    .await;
                }
                Err(e) => {
                    warn!("Neon discovery failed: {}", e);
                    send_response(
                        tx,
                        BridgeResponse::device_error("neon_lsl".to_string(), e.to_string(), id),
                    )
                    .await;
                }
            }
        }
        CommandAction::ConnectNeonGaze => {
            let device_name = payload
                .as_ref()
                .and_then(|p| p.get("device_name"))
                .and_then(|v| v.as_str())
                .unwrap_or("");

            if device_name.is_empty() {
                send_response(
                    tx,
                    BridgeResponse::device_error(
                        "neon_lsl".to_string(),
                        "Missing 'device_name' in payload".to_string(),
                        id,
                    ),
                )
                .await;
                return;
            }

            info!("Connecting to Neon gaze stream: {}", device_name);

            match state.neon_manager.connect_gaze_stream(device_name).await {
                Ok((mut gaze_rx, task_id)) => {
                    if conn.closed.load(Ordering::Relaxed) {
                        // Nobody is listening any more; don't leave a live inlet and
                        // task registered under this label for the next client to
                        // trip over as "already connected".
                        let _ = state
                            .neon_manager
                            .disconnect_gaze_stream_owned(device_name, task_id)
                            .await;
                        return;
                    }
                    // Spawn a task to forward gaze data to WebSocket. It also watches
                    // the connection's closed flag (a quiet stream would otherwise
                    // never notice the client is gone) and releases the stream when
                    // the client disappears, so the next client is not refused as
                    // "already connected".
                    let tx_clone = tx.clone();
                    let device_name_clone = device_name.to_string();
                    let closed = conn.closed.clone();
                    let teardown_state = state.clone();

                    tokio::spawn(async move {
                        let mut client_gone = false;
                        loop {
                            tokio::select! {
                                item = gaze_rx.recv() => match item {
                                    Some(gaze) => {
                                        // Streaming data doesn't need request ID (it's unsolicited)
                                        let response = BridgeResponse::data(
                                            format!("neon_{}", device_name_clone),
                                            json!({
                                                "type": "gaze",
                                                "timestamp": gaze.timestamp,
                                                "gaze_x": gaze.gaze_x,
                                                "gaze_y": gaze.gaze_y,
                                                "pupil_diameter": gaze.pupil_diameter,
                                                "eyeball_center": gaze.eyeball_center,
                                            }),
                                            None,
                                        );
                                        if tx_clone.send(response).await.is_err() {
                                            client_gone = true;
                                            break;
                                        }
                                    }
                                    None => break, // stream disconnected explicitly
                                },
                                _ = tokio::time::sleep(Duration::from_secs(1)) => {
                                    if closed.load(Ordering::Relaxed) {
                                        client_gone = true;
                                        break;
                                    }
                                }
                            }
                        }
                        if client_gone {
                            // Release only OUR stream: if a newer client has
                            // since opened this label, that stream is theirs.
                            let _ = teardown_state
                                .neon_manager
                                .disconnect_gaze_stream_owned(&device_name_clone, task_id)
                                .await;
                        }
                    });

                    send_response(
                        tx,
                        BridgeResponse::status(
                            format!("neon_{}", device_name),
                            crate::devices::DeviceStatus::Connected,
                            id,
                        ),
                    )
                    .await;
                }
                Err(e) => {
                    warn!("Failed to connect Neon gaze: {}", e);
                    send_response(
                        tx,
                        BridgeResponse::device_error("neon_lsl".to_string(), e.to_string(), id),
                    )
                    .await;
                }
            }
        }
        CommandAction::ConnectNeonEvents => {
            let device_name = payload
                .as_ref()
                .and_then(|p| p.get("device_name"))
                .and_then(|v| v.as_str())
                .unwrap_or("");

            if device_name.is_empty() {
                send_response(
                    tx,
                    BridgeResponse::device_error(
                        "neon_lsl".to_string(),
                        "Missing 'device_name' in payload".to_string(),
                        id,
                    ),
                )
                .await;
                return;
            }

            info!("Connecting to Neon events stream: {}", device_name);

            match state.neon_manager.connect_events_stream(device_name).await {
                Ok((mut events_rx, task_id)) => {
                    if conn.closed.load(Ordering::Relaxed) {
                        let _ = state
                            .neon_manager
                            .disconnect_events_stream_owned(device_name, task_id)
                            .await;
                        return;
                    }
                    // Spawn a task to forward event data to WebSocket (closed-aware,
                    // see the gaze forwarder above).
                    let tx_clone = tx.clone();
                    let device_name_clone = device_name.to_string();
                    let closed = conn.closed.clone();
                    let teardown_state = state.clone();

                    tokio::spawn(async move {
                        let mut client_gone = false;
                        loop {
                            tokio::select! {
                                item = events_rx.recv() => match item {
                                    Some(event) => {
                                        // Streaming data doesn't need request ID (it's unsolicited)
                                        let response = BridgeResponse::data(
                                            format!("neon_{}", device_name_clone),
                                            json!({
                                                "type": "event",
                                                "timestamp": event.timestamp,
                                                "event_name": event.event_name,
                                            }),
                                            None,
                                        );
                                        if tx_clone.send(response).await.is_err() {
                                            client_gone = true;
                                            break;
                                        }
                                    }
                                    None => break,
                                },
                                _ = tokio::time::sleep(Duration::from_secs(1)) => {
                                    if closed.load(Ordering::Relaxed) {
                                        client_gone = true;
                                        break;
                                    }
                                }
                            }
                        }
                        if client_gone {
                            let _ = teardown_state
                                .neon_manager
                                .disconnect_events_stream_owned(&device_name_clone, task_id)
                                .await;
                        }
                    });

                    // Send status response (consistent with ConnectNeonGaze)
                    send_response(
                        tx,
                        BridgeResponse::status(
                            format!("neon_{}", device_name),
                            crate::devices::DeviceStatus::Connected,
                            id,
                        ),
                    )
                    .await;
                }
                Err(e) => {
                    warn!("Failed to connect Neon events: {}", e);
                    send_response(
                        tx,
                        BridgeResponse::device_error("neon_lsl".to_string(), e.to_string(), id),
                    )
                    .await;
                }
            }
        }
        CommandAction::ConnectNeonRest => {
            // Connect the Neon REST API (pupil device) using hostname from discovery cache
            let device_name = payload
                .as_ref()
                .and_then(|p| p.get("device_name"))
                .and_then(|v| v.as_str())
                .unwrap_or("");

            if device_name.is_empty() {
                send_response(
                    tx,
                    BridgeResponse::device_error(
                        "neon_lsl".to_string(),
                        "Missing 'device_name' in payload".to_string(),
                        id,
                    ),
                )
                .await;
                return;
            }

            let connect_ticket = state.begin_connect("pupil").await;
            conn.remember_connect_ticket("pupil", connect_ticket);
            let mut attempt = ConnectAttempt::new(state.clone(), "pupil", connect_ticket);
            info!(
                "ConnectNeonRest: resolving hostname for device '{}'",
                device_name
            );

            // Look up device in discovery cache
            let discovered = state.neon_manager.get_device(device_name).await;
            let hostname = match discovered {
                Some(ref dev) => match &dev.hostname {
                    Some(h) if !h.is_empty() => h.clone(),
                    _ => {
                        send_response(
                            tx,
                            BridgeResponse::device_error(
                                "neon_lsl".to_string(),
                                "No hostname available for device. Use Manual IP.".to_string(),
                                id,
                            ),
                        )
                        .await;
                        return;
                    }
                },
                None => {
                    send_response(
                        tx,
                        BridgeResponse::device_error(
                            "neon_lsl".to_string(),
                            "Device not found. Run discovery first.".to_string(),
                            id,
                        ),
                    )
                    .await;
                    return;
                }
            };

            info!(
                "ConnectNeonRest: connecting to Neon REST API at '{}'",
                hostname
            );

            let mut device: Box<dyn crate::devices::Device> = Box::new(PupilDevice::new(hostname));

            match device.connect().await {
                Ok(_) => {
                    reclaim_pupil_recording(state, &mut device);
                    let info = device.get_info();
                    let replaced = match state
                        .add_device_if_latest(
                            "pupil".to_string(),
                            device,
                            connect_ticket,
                            &conn.closed,
                        )
                        .await
                    {
                        Ok(replaced) => replaced,
                        Err(mut orphan) => {
                            info!("Pupil connect via discovery superseded (newer connect or connection closed); not registered");
                            let _ = orphan.disconnect().await;
                            send_response(
                                tx,
                                BridgeResponse::device_error(
                                    "pupil".to_string(),
                                    "Connect superseded by a newer connect".to_string(),
                                    id,
                                ),
                            )
                            .await;
                            return;
                        }
                    };
                    attempt.registered();
                    disconnect_replaced("pupil", replaced);
                    state.record_connection_attempt("pupil", true).await;

                    state.broadcast_device_status(DeviceStatusEvent::connected_with_info(
                        "pupil".to_string(),
                        info.device_type,
                        "connected",
                        info.metadata.clone(),
                    ));
                    send_response(
                        tx,
                        BridgeResponse::status_with_info(
                            "pupil".to_string(),
                            info.status,
                            info.metadata,
                            id,
                        ),
                    )
                    .await;
                }
                Err(e) => {
                    state.record_connection_attempt("pupil", false).await;
                    state.record_device_error("pupil", &e.to_string()).await;

                    send_response(
                        tx,
                        BridgeResponse::device_error_from("pupil".to_string(), &e, id),
                    )
                    .await;
                }
            }
        }
        CommandAction::DisconnectNeon => {
            let device_name = payload
                .as_ref()
                .and_then(|p| p.get("device_name"))
                .and_then(|v| v.as_str())
                .unwrap_or("");

            if device_name.is_empty() {
                // Disconnect all if no device specified
                info!("Disconnecting all Neon streams");
                let _ = state.neon_manager.disconnect_all().await;

                send_response(
                    tx,
                    BridgeResponse::data(
                        "neon_lsl".to_string(),
                        json!({ "success": true, "message": "Disconnected all Neon streams" }),
                        id,
                    ),
                )
                .await;
            } else {
                info!("Disconnecting Neon device: {}", device_name);
                let _ = state.neon_manager.disconnect(device_name).await;

                send_response(
                    tx,
                    BridgeResponse::status(
                        format!("neon_{}", device_name),
                        crate::devices::DeviceStatus::Disconnected,
                        id,
                    ),
                )
                .await;
            }
        }
        CommandAction::NeonStatus => {
            let stats = state.neon_manager.get_stats().await;

            send_response(
                tx,
                BridgeResponse::data(
                    "neon_lsl".to_string(),
                    json!({
                        "type": "status",
                        "stats": stats
                    }),
                    id,
                ),
            )
            .await;
        }
        // ====================================================================
        // FRENZ Brainband LSL handlers
        // ====================================================================
        CommandAction::UploadStudyRecording => {
            let result = async {
                let value = payload.ok_or_else(|| anyhow::anyhow!("Missing upload request"))?;
                let recording_id = value.get("recording_id").and_then(|v|v.as_str()).ok_or_else(|| anyhow::anyhow!("Missing recording ID"))?.to_string();
                let base = value.get("api_base_url").and_then(|v|v.as_str()).ok_or_else(|| anyhow::anyhow!("Missing API base URL"))?.to_string();
                let grant = serde_json::from_value(value.get("grant").cloned().ok_or_else(|| anyhow::anyhow!("Missing upload grant"))?)
                    .map_err(|_|anyhow::anyhow!("Invalid upload grant"))?;
                crate::recording::upload::enqueue(state.recording.clone(),recording_id,base,grant).await
            }.await;
            let response = match result {
                Ok(value) => BridgeResponse::data("recording".into(),value,id),
                Err(error) => BridgeResponse::device_error("recording".into(),error.to_string(),id),
            };
            send_response(tx,response).await;
        }
        CommandAction::StartStudyRecording
        | CommandAction::GetStudyRecording
        | CommandAction::StopStudyRecording
        | CommandAction::SendStudyMarker
        | CommandAction::PrepareRecordingUpload
        | CommandAction::ListStudyRecordings => {
            let manager = state.recording.clone();
            let result = tokio::task::spawn_blocking(move || -> anyhow::Result<serde_json::Value> {
                let value = payload.unwrap_or_else(||json!({}));
                let field = |name: &str| value.get(name).cloned().ok_or_else(||anyhow::anyhow!("Missing {}",name));
                match action {
                    CommandAction::StartStudyRecording => Ok(serde_json::to_value(manager.start_study(serde_json::from_value(value)?)?)?),
                    CommandAction::GetStudyRecording => Ok(serde_json::to_value(manager.get_study(&serde_json::from_value(field("context")?)?)?)?),
                    CommandAction::StopStudyRecording => Ok(serde_json::to_value(manager.stop_study(serde_json::from_value(value)?)?)?),
                    CommandAction::SendStudyMarker => {
                        let id: String = serde_json::from_value(field("recording_id")?)?;
                        manager.study_marker(&id,&serde_json::from_value(field("context")?)?,field("marker")?)
                    }
                    CommandAction::PrepareRecordingUpload => {
                        let id: String = serde_json::from_value(field("recording_id")?)?;
                        crate::recording::upload::manifest(&manager.study_path(&id)?,&id)
                    }
                    _ => Ok(serde_json::to_value(manager.list_studies()?)?),
                }
            }).await;
            let response = match result {
                Ok(Ok(value)) => BridgeResponse::data("recording".into(),value,id),
                Ok(Err(error)) => BridgeResponse::device_error("recording".into(),error.to_string(),id),
                Err(_) => BridgeResponse::device_error("recording".into(),"Study recording command failed".into(),id),
            };
            send_response(tx,response).await;
        }
        CommandAction::DiscoverRecordingStreams
        | CommandAction::DescribeRecordingStream
        | CommandAction::StartLslRecording
        | CommandAction::StopLslRecording
        | CommandAction::LslRecordingStatus => {
            let manager = state.recording.clone();
            let result =
                tokio::task::spawn_blocking(move || -> anyhow::Result<serde_json::Value> {
                    match action {
                        CommandAction::DiscoverRecordingStreams => {
                            Ok(serde_json::to_value(crate::recording::discover()?)?)
                        }
                        CommandAction::DescribeRecordingStream => {
                            let uid = payload
                                .as_ref()
                                .and_then(|value| value.get("uid"))
                                .and_then(|value| value.as_str())
                                .ok_or_else(|| anyhow::anyhow!("Missing stream UID"))?;
                            Ok(serde_json::to_value(crate::recording::describe(uid)?)?)
                        }
                        CommandAction::StartLslRecording => {
                            let request =
                                serde_json::from_value(payload.ok_or_else(|| {
                                    anyhow::anyhow!("Missing recording request")
                                })?)?;
                            Ok(serde_json::to_value(manager.start(request)?)?)
                        }
                        CommandAction::StopLslRecording => {
                            let request = match payload {
                                Some(value) if !value.is_null() => serde_json::from_value(value)?,
                                _ => crate::recording::RecordingStopRequest::default(),
                            };
                            Ok(serde_json::to_value(manager.stop_unmanaged(request)?)?)
                        }
                        _ => Ok(serde_json::to_value(manager.status())?),
                    }
                })
                .await;
            let response = match result {
                Ok(Ok(value)) => BridgeResponse::data("recording".into(), value, id),
                Ok(Err(error)) => {
                    BridgeResponse::device_error("recording".into(), error.to_string(), id)
                }
                Err(_) => BridgeResponse::device_error(
                    "recording".into(),
                    "Recording command failed".into(),
                    id,
                ),
            };
            send_response(tx, response).await;
        }
        CommandAction::DiscoverFrenz => {
            info!("Discovering FRENZ devices via LSL");

            match state.frenz_manager.discover_frenz_devices().await {
                Ok(devices) => {
                    let device_list: Vec<serde_json::Value> = devices
                        .iter()
                        .map(|d| {
                            let streams: Vec<serde_json::Value> = d
                                .streams
                                .values()
                                .map(|s| {
                                    json!({
                                        "suffix": s.suffix,
                                        "channel_count": s.channel_count,
                                        "nominal_srate": s.nominal_srate,
                                        "category": s.category.to_string(),
                                    })
                                })
                                .collect();
                            json!({
                                "device_name": d.device_name,
                                "stream_count": d.streams.len(),
                                "streams": streams,
                            })
                        })
                        .collect();

                    send_response(
                        tx,
                        BridgeResponse::data(
                            "frenz_lsl".to_string(),
                            json!({
                                "type": "discovery",
                                "devices": device_list,
                                "count": devices.len()
                            }),
                            id,
                        ),
                    )
                    .await;
                }
                Err(e) => {
                    warn!("FRENZ discovery failed: {}", e);
                    send_response(
                        tx,
                        BridgeResponse::device_error("frenz_lsl".to_string(), e.to_string(), id),
                    )
                    .await;
                }
            }
        }
        CommandAction::ConnectFrenzStreams => {
            let device_name = payload
                .as_ref()
                .and_then(|p| p.get("device_name"))
                .and_then(|v| v.as_str())
                .unwrap_or("");

            if device_name.is_empty() {
                send_response(
                    tx,
                    BridgeResponse::device_error(
                        "frenz_lsl".to_string(),
                        "Missing 'device_name' in payload".to_string(),
                        id,
                    ),
                )
                .await;
                return;
            }

            // Extract selected stream suffixes (empty = connect all)
            let selected_streams: Vec<String> = payload
                .as_ref()
                .and_then(|p| p.get("streams"))
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();

            // Check if marker outlet should be created
            let enable_markers = payload
                .as_ref()
                .and_then(|p| p.get("enable_markers"))
                .and_then(|v| v.as_bool())
                .unwrap_or(true);

            info!(
                "Connecting FRENZ streams for {}: {:?}",
                device_name,
                if selected_streams.is_empty() {
                    "all".to_string()
                } else {
                    format!("{} selected", selected_streams.len())
                }
            );

            // Create marker outlet if requested
            if enable_markers {
                if let Err(e) = state.frenz_manager.create_marker_outlet().await {
                    warn!("Failed to create FRENZ marker outlet: {}", e);
                }
            }

            match state
                .frenz_manager
                .connect_streams(device_name, &selected_streams)
                .await
            {
                Ok(receivers) => {
                    let connected_suffixes: Vec<String> = receivers.keys().cloned().collect();
                    let connected_count = receivers.len();

                    // Spawn forwarding tasks for each stream
                    for (suffix, mut rx) in receivers {
                        let tx_clone = tx.clone();
                        let device_name_clone = device_name.to_string();
                        let suffix_clone = suffix.clone();

                        tokio::spawn(async move {
                            while let Some(sample) = rx.recv().await {
                                let response = BridgeResponse::data(
                                    format!("frenz_{}", device_name_clone),
                                    json!({
                                        "type": "frenz_data",
                                        "stream": suffix_clone,
                                        "timestamp": sample.timestamp,
                                        "data": sample.wire_data(),
                                                        "values": sample.values,
                                        "string_value": sample.string_value,
                                    }),
                                    None,
                                );
                                if tx_clone.send(response).await.is_err() {
                                    break;
                                }
                            }
                        });
                    }

                    send_response(
                        tx,
                        BridgeResponse::data(
                            "frenz_lsl".to_string(),
                            json!({
                                "type": "connected",
                                "device_name": device_name,
                                "connected_streams": connected_suffixes,
                                "count": connected_count,
                            }),
                            id,
                        ),
                    )
                    .await;
                }
                Err(e) => {
                    warn!("Failed to connect FRENZ streams: {}", e);
                    send_response(
                        tx,
                        BridgeResponse::device_error("frenz_lsl".to_string(), e.to_string(), id),
                    )
                    .await;
                }
            }
        }
        CommandAction::DisconnectFrenz => {
            let device_name = payload
                .as_ref()
                .and_then(|p| p.get("device_name"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let suffix = payload
                .as_ref()
                .and_then(|p| p.get("stream"))
                .and_then(|v| v.as_str());
            let result = if device_name.is_empty() {
                state.frenz_manager.disconnect_all().await
            } else if let Some(suffix) = suffix {
                state
                    .frenz_manager
                    .disconnect_stream(device_name, suffix)
                    .await
            } else {
                state.frenz_manager.disconnect_device(device_name).await
            };
            let response = match result {
                Err(error) => {
                    BridgeResponse::device_error("frenz_lsl".into(), error.to_string(), id)
                }
                Ok(()) if device_name.is_empty() => BridgeResponse::data(
                    "frenz_lsl".into(),
                    json!({"success":true,"message":"Disconnected all FRENZ streams"}),
                    id,
                ),
                Ok(()) if suffix.is_some() => BridgeResponse::data(
                    "frenz_lsl".into(),
                    json!({"success":true,"device_name":device_name,"disconnected_stream":suffix}),
                    id,
                ),
                Ok(()) => BridgeResponse::status(
                    format!("frenz_{}", device_name),
                    crate::devices::DeviceStatus::Disconnected,
                    id,
                ),
            };
            send_response(tx, response).await;
        }
        CommandAction::FrenzStatus => {
            let stats = state.frenz_manager.get_stats().await;

            send_response(
                tx,
                BridgeResponse::data(
                    "frenz_lsl".to_string(),
                    json!({
                        "type": "status",
                        "stats": stats
                    }),
                    id,
                ),
            )
            .await;
        }
        CommandAction::SendFrenzMarker => {
            let marker = payload
                .as_ref()
                .and_then(|p| p.get("marker"))
                .and_then(|v| v.as_str())
                .unwrap_or("");

            if marker.is_empty() {
                send_response(
                    tx,
                    BridgeResponse::device_error(
                        "frenz_lsl".to_string(),
                        "Missing 'marker' in payload".to_string(),
                        id,
                    ),
                )
                .await;
                return;
            }

            let timestamp = payload
                .as_ref()
                .and_then(|p| p.get("timestamp"))
                .and_then(|v| v.as_f64());

            match state.frenz_manager.send_marker(marker, timestamp).await {
                Ok(()) => {
                    send_response(
                        tx,
                        BridgeResponse::data(
                            "frenz_lsl".to_string(),
                            json!({ "success": true, "marker": marker }),
                            id,
                        ),
                    )
                    .await;
                }
                Err(e) => {
                    send_response(
                        tx,
                        BridgeResponse::device_error("frenz_lsl".to_string(), e.to_string(), id),
                    )
                    .await;
                }
            }
        }
        // ====================================================================
        // EyeLink commands (SDK loaded at runtime via libloading)
        // ====================================================================
        CommandAction::ConnectEyeLink => {
            let ip = payload
                .as_ref()
                .and_then(|p| p.get("ip"))
                .and_then(|v| v.as_str())
                .unwrap_or("100.1.1.1");
            let sample_rate = payload
                .as_ref()
                .and_then(|p| p.get("sample_rate"))
                .and_then(|v| v.as_u64())
                .map(|v| v as u32);
            let display_width = payload
                .as_ref()
                .and_then(|p| p.get("display_width"))
                .and_then(|v| v.as_u64())
                .map(|v| v as u32);
            let display_height = payload
                .as_ref()
                .and_then(|p| p.get("display_height"))
                .and_then(|v| v.as_u64())
                .map(|v| v as u32);

            match state
                .eyelink_manager
                .connect(ip, sample_rate, display_width, display_height)
                .await
            {
                Ok(status) => {
                    // No dead-connection rollback: EyeLinkManager is a singleton
                    // without per-consumer ownership (see the FRENZ note above).
                    send_response(
                        tx,
                        BridgeResponse::data(
                            "eyelink".to_string(),
                            serde_json::to_value(&status).unwrap_or_default(),
                            id,
                        ),
                    )
                    .await;
                }
                Err(e) => {
                    send_response(
                        tx,
                        BridgeResponse::device_error("eyelink".to_string(), e, id),
                    )
                    .await;
                }
            }
        }
        CommandAction::DisconnectEyeLink => match state.eyelink_manager.disconnect().await {
            Ok(()) => {
                send_response(
                    tx,
                    BridgeResponse::status(
                        "eyelink".to_string(),
                        crate::devices::DeviceStatus::Disconnected,
                        id,
                    ),
                )
                .await;
            }
            Err(e) => {
                send_response(
                    tx,
                    BridgeResponse::device_error("eyelink".to_string(), e, id),
                )
                .await;
            }
        },
        CommandAction::StartEyeLinkRecording => {
            match state.eyelink_manager.start_recording().await {
                Ok(()) => {
                    send_response(
                        tx,
                        BridgeResponse::data(
                            "eyelink".to_string(),
                            json!({ "recording": true }),
                            id,
                        ),
                    )
                    .await;
                }
                Err(e) => {
                    send_response(
                        tx,
                        BridgeResponse::device_error("eyelink".to_string(), e, id),
                    )
                    .await;
                }
            }
        }
        CommandAction::StopEyeLinkRecording => match state.eyelink_manager.stop_recording().await {
            Ok(()) => {
                send_response(
                    tx,
                    BridgeResponse::data("eyelink".to_string(), json!({ "recording": false }), id),
                )
                .await;
            }
            Err(e) => {
                send_response(
                    tx,
                    BridgeResponse::device_error("eyelink".to_string(), e, id),
                )
                .await;
            }
        },
        CommandAction::SendEyeLinkMessage => {
            let message = payload
                .as_ref()
                .and_then(|p| p.get("message"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            if message.is_empty() {
                send_response(
                    tx,
                    BridgeResponse::device_error(
                        "eyelink".to_string(),
                        "Missing 'message' in payload".to_string(),
                        id,
                    ),
                )
                .await;
                return;
            }

            match state.eyelink_manager.send_message(message.clone()).await {
                Ok(()) => {
                    send_response(
                        tx,
                        BridgeResponse::data(
                            "eyelink".to_string(),
                            json!({ "success": true, "message": message }),
                            id,
                        ),
                    )
                    .await;
                }
                Err(e) => {
                    send_response(
                        tx,
                        BridgeResponse::device_error("eyelink".to_string(), e, id),
                    )
                    .await;
                }
            }
        }
        CommandAction::CalibrateEyeLink => {
            info!("Starting EyeLink calibration");

            match state.eyelink_manager.start_calibration().await {
                Ok((mut cal_rx, cal_handle)) => {
                    // Spawn a task to forward calibration events to WebSocket.
                    // Exits when: broadcast channel closes (calibration ends) or WebSocket closes.
                    let tx_clone = tx.clone();
                    tokio::spawn(async move {
                        while let Ok(event) = cal_rx.recv().await {
                            let response = BridgeResponse::data(
                                "eyelink".to_string(),
                                json!({
                                    "type": "calibration",
                                    "event": serde_json::to_value(&event).unwrap_or_default(),
                                }),
                                None,
                            );
                            if tx_clone.send(response).await.is_err() {
                                // WebSocket closed — send cancel key so calibration loop exits
                                tracing::debug!(
                                    "WebSocket closed during calibration, sending cancel"
                                );
                                crate::devices::eyelink::calibration::cancel_calibration();
                                break;
                            }
                        }
                    });

                    // Spawn a task to wait for calibration completion and send the result.
                    // Exits when: calibration finishes (cal_handle resolves).
                    let tx_clone = tx.clone();
                    tokio::spawn(async move {
                        match cal_handle.await {
                            Ok(Ok(result)) => {
                                let _ = tx_clone
                                    .send(BridgeResponse::data(
                                        "eyelink".to_string(),
                                        json!({
                                            "type": "calibration_complete",
                                            "result_code": result,
                                        }),
                                        None,
                                    ))
                                    .await;
                            }
                            Ok(Err(e)) => {
                                let _ = tx_clone
                                    .send(BridgeResponse::device_error(
                                        "eyelink".to_string(),
                                        e,
                                        None,
                                    ))
                                    .await;
                            }
                            Err(e) => {
                                let _ = tx_clone
                                    .send(BridgeResponse::device_error(
                                        "eyelink".to_string(),
                                        format!("Calibration task failed: {}", e),
                                        None,
                                    ))
                                    .await;
                            }
                        }
                    });

                    send_response(
                        tx,
                        BridgeResponse::data(
                            "eyelink".to_string(),
                            json!({ "calibrating": true }),
                            id,
                        ),
                    )
                    .await;
                }
                Err(e) => {
                    send_response(
                        tx,
                        BridgeResponse::device_error("eyelink".to_string(), e, id),
                    )
                    .await;
                }
            }
        }
        CommandAction::CalibrateEyeLinkKey => {
            let key = payload
                .as_ref()
                .and_then(|p| p.get("key"))
                .and_then(|v| v.as_str())
                .unwrap_or("");

            match key {
                "accept" | "enter" => {
                    crate::devices::eyelink::calibration::accept_calibration();
                    send_response(
                        tx,
                        BridgeResponse::data(
                            "eyelink".to_string(),
                            json!({ "key_sent": "accept" }),
                            id,
                        ),
                    )
                    .await;
                }
                "cancel" | "escape" => {
                    crate::devices::eyelink::calibration::cancel_calibration();
                    send_response(
                        tx,
                        BridgeResponse::data(
                            "eyelink".to_string(),
                            json!({ "key_sent": "cancel" }),
                            id,
                        ),
                    )
                    .await;
                }
                _ => {
                    send_response(
                        tx,
                        BridgeResponse::device_error(
                            "eyelink".to_string(),
                            format!("Unknown key: '{}'. Use 'accept' or 'cancel'", key),
                            id,
                        ),
                    )
                    .await;
                }
            }
        }
        CommandAction::EyeLinkStatus => {
            let status = state.eyelink_manager.get_status().await;
            send_response(
                tx,
                BridgeResponse::data(
                    "eyelink".to_string(),
                    serde_json::to_value(&status).unwrap_or_default(),
                    id,
                ),
            )
            .await;
        }
        CommandAction::ConnectEyeLinkGaze => {
            match state.eyelink_manager.start_gaze_stream().await {
                Ok(mut gaze_rx) => {
                    // Spawn a task to forward gaze data to WebSocket.
                    // When the WebSocket closes (tx_clone.send fails), we proactively
                    // stop the gaze stream to avoid wasting CPU on polling with no
                    // consumer (pre-existing behaviour). A quiet stream only stops
                    // forwarding on the closed flag: the manager is a singleton with
                    // no per-consumer ownership, so an unowned stop could hit a
                    // newer client's stream.
                    let tx_clone = tx.clone();
                    let eyelink_mgr = state.eyelink_manager.clone();
                    let closed = conn.closed.clone();
                    tokio::spawn(async move {
                        loop {
                            let gaze = tokio::select! {
                                item = gaze_rx.recv() => match item {
                                    Some(gaze) => gaze,
                                    None => break,
                                },
                                _ = tokio::time::sleep(Duration::from_secs(1)) => {
                                    if closed.load(Ordering::Relaxed) {
                                        tracing::debug!("WebSocket closed (quiet stream), stopping forwarder");
                                        break;
                                    }
                                    continue;
                                }
                            };
                            let response = BridgeResponse::data(
                                "eyelink".to_string(),
                                json!({
                                    "type": "gaze",
                                    "timestamp": gaze.timestamp,
                                    "left_gaze_x": gaze.left_gaze_x,
                                    "left_gaze_y": gaze.left_gaze_y,
                                    "right_gaze_x": gaze.right_gaze_x,
                                    "right_gaze_y": gaze.right_gaze_y,
                                    "left_pupil": gaze.left_pupil,
                                    "right_pupil": gaze.right_pupil,
                                    "ppd_x": gaze.ppd_x,
                                    "ppd_y": gaze.ppd_y,
                                    "status": gaze.status,
                                    "display_width": gaze.display_width,
                                    "display_height": gaze.display_height,
                                }),
                                None,
                            );
                            if tx_clone.send(response).await.is_err() {
                                // WebSocket closed — stop the gaze polling thread
                                tracing::debug!("WebSocket closed, stopping gaze stream");
                                let _ = eyelink_mgr.stop_gaze_stream().await;
                                break;
                            }
                        }
                    });

                    send_response(
                        tx,
                        BridgeResponse::data(
                            "eyelink".to_string(),
                            json!({ "gaze_streaming": true }),
                            id,
                        ),
                    )
                    .await;
                }
                Err(e) => {
                    send_response(
                        tx,
                        BridgeResponse::device_error("eyelink".to_string(), e, id),
                    )
                    .await;
                }
            }
        }
        CommandAction::DisconnectEyeLinkGaze => {
            match state.eyelink_manager.stop_gaze_stream().await {
                Ok(()) => {
                    send_response(
                        tx,
                        BridgeResponse::data(
                            "eyelink".to_string(),
                            json!({ "gaze_streaming": false }),
                            id,
                        ),
                    )
                    .await;
                }
                Err(e) => {
                    send_response(
                        tx,
                        BridgeResponse::device_error("eyelink".to_string(), e, id),
                    )
                    .await;
                }
            }
        }
        _ => {
            send_response(
                tx,
                BridgeResponse::error("Unsupported action".to_string(), id),
            )
            .await;
        }
    }
}

/// Probe a discovered phone's REST API (2 s cap) so the picker can show battery
/// and whether it is already recording. Unreachable phones are still listed.
async fn describe_neon_phone(
    phone: &crate::devices::neon_discovery::NeonPhone,
) -> serde_json::Value {
    let mut value = serde_json::to_value(phone).unwrap_or_else(|_| json!({}));
    let mut device = PupilDevice::new(phone.host());
    match tokio::time::timeout(Duration::from_secs(2), device.get_neon_status()).await {
        Ok(Ok(status)) => {
            value["reachable"] = json!(true);
            value["battery_level"] = json!(status.phone.battery_level);
            value["battery_state"] = json!(status.phone.battery_state);
            value["recording_id"] = json!(status.recording.as_ref().map(|r| r.id.clone()));
            value["glasses_connected"] = json!(status
                .hardware
                .as_ref()
                .map(|h| h.glasses_serial.is_some())
                .unwrap_or(false));
        }
        _ => {
            value["reachable"] = json!(false);
        }
    }
    value
}

async fn handle_query(
    state: &Arc<AppState>,
    target: QueryTarget,
    id: Option<String>,
    tx: &mpsc::Sender<BridgeResponse>,
) {
    let data = match target {
        QueryTarget::Devices => {
            let devices = state.list_devices().await;
            json!(devices)
        }
        QueryTarget::Device(device_id) => {
            let device_id = device_id.to_lowercase();
            if let Some(device_lock) = state.get_device(&device_id).await {
                let device = device_lock.read().await;
                json!(device.get_info())
            } else {
                json!({ "error": "Device not found" })
            }
        }
        QueryTarget::Metrics => {
            let metrics = state.get_metrics().await;
            json!(metrics)
        }
        QueryTarget::Connections => {
            let connections: Vec<_> = state
                .connections
                .iter()
                .map(|entry| entry.value().clone())
                .collect();
            json!(connections)
        }
        QueryTarget::Status => {
            let devices = state.devices.read().await;
            json!({
                "server": "running",
                "port": WS_PORT,
                "devices": devices.len(),
                "connections": state.connections.len(),
            })
        }
    };

    send_response(tx, BridgeResponse::query_result(id, data)).await;
}

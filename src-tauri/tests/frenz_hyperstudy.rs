//! HyperStudy's view of a band held through openfrenz, over the WebSocket it uses: the connection check it runs
//! before a study, the stream discovery its FRENZ setup screen groups into bands, and a study recording of those
//! streams with markers. A replayed synthetic recording stands in for the band.

use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use hyperstudy_bridge::bridge::{AppState, BridgeServer};
use serde_json::{json, Value};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;

const BAND: &str = "FRENZT01";

/// A Bridge with a replayed band streaming through openfrenz, serving its WebSocket on `port` (0 for a free one).
/// `FRENZ_REPLAY` names a recording to play in place of the synthetic one.
async fn bridge_with_a_band(
    port: u16,
    seconds: u32,
) -> (Arc<AppState>, String, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("frenz-hyperstudy-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let recording = match std::env::var("FRENZ_REPLAY") {
        Ok(path) => std::path::PathBuf::from(path),
        Err(_) => {
            let path = dir.join("synthetic.sqlite");
            frenz::synth::write_recording(&path, seconds).unwrap();
            path
        }
    };
    let state = Arc::new(AppState::new());
    state
        .recording
        .configure_study_directory(dir.join("recordings"))
        .unwrap();
    // `FRENZ_BAND` names a real band to connect to over Bluetooth in place of any recording.
    match std::env::var("FRENZ_BAND") {
        Ok(band) => state
            .frenz_native
            .start(&band, &[], &[], None, None)
            .await
            .unwrap(),
        Err(_) => state
            .frenz_native
            .start_replay(BAND, &recording, &[], &[], None, None)
            .await
            .unwrap(),
    }

    let app = tauri::test::mock_app();
    let mut server = BridgeServer::new(state.clone(), app.handle().clone());
    let listener = TcpListener::bind(("127.0.0.1", port)).await.unwrap();
    let url = format!("ws://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        let _app = app;
        let _ = server.serve(listener).await;
    });
    (state, url, dir)
}

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// Sends one command the way HyperStudy's bridgeService does and returns the response carrying its id.
async fn command(socket: &mut Socket, device: &str, action: &str, payload: Value) -> Value {
    let id = format!("msg_{}", uuid::Uuid::new_v4());
    let message = json!({"type": "command", "device": device, "action": action, "payload": payload, "id": id});
    socket
        .send(Message::Text(message.to_string().into()))
        .await
        .unwrap();
    let answer = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let Some(Ok(Message::Text(text))) = socket.next().await else {
                continue;
            };
            let value: Value = serde_json::from_str(&text).unwrap();
            if value["id"] == id {
                return value;
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("no answer to {device}/{action}"));
    assert_ne!(answer["type"], "error", "{device}/{action}: {answer}");
    answer
}

/// HyperStudy's grouping (frontend/src/lib/services/frenzRecordingStreams.js): streams whose source id is
/// `frenz-{band}-{session uuid}_{stream}` and whose name is `{band}_{stream}`, by band and session.
fn hyperstudy_bands(streams: &[Value]) -> std::collections::BTreeMap<String, Vec<Value>> {
    let mut bands = std::collections::BTreeMap::<String, Vec<Value>>::new();
    for stream in streams {
        let (Some(source), Some(name)) = (stream["source_id"].as_str(), stream["name"].as_str())
        else {
            continue;
        };
        let Some(rest) = source.strip_prefix("frenz-") else {
            continue;
        };
        // {band}-{36 character uuid}_{stream}
        let Some(mark) = rest.find('_') else { continue };
        let (head, suffix) = rest.split_at(mark);
        if head.len() < 38 || !head.is_char_boundary(head.len() - 36) {
            continue;
        }
        let (band, session) = head.split_at(head.len() - 37);
        if uuid::Uuid::parse_str(&session[1..]).is_err() || name != format!("{band}{suffix}") {
            continue;
        }
        bands
            .entry(format!("{band}:{}", &session[1..]))
            .or_default()
            .push(stream.clone());
    }
    bands
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hyperstudy_checks_finds_and_records_a_band_held_through_openfrenz() {
    let (state, url, dir) = bridge_with_a_band(0, 60).await;
    let (mut socket, _) = tokio_tungstenite::connect_async(&url).await.unwrap();

    // The pre-flight check (bridgeService.getFrenzDeviceInfo): connected means streams are active.
    let status = command(&mut socket, "frenz_lsl", "frenz_status", json!({})).await;
    let active = status["payload"]["stats"]["active_stream_count"].as_u64();
    assert!(
        active.is_some_and(|count| count > 0),
        "HyperStudy would report FRENZ as not connected: {status}"
    );

    // The setup screen: one band, with every stream Bridge publishes for it.
    let published = state.frenz_native.status().streams;
    let mut bands = std::collections::BTreeMap::new();
    for _ in 0..10 {
        let found = command(
            &mut socket,
            "recording",
            "discover_recording_streams",
            json!({}),
        )
        .await;
        bands = hyperstudy_bands(found["payload"].as_array().unwrap());
        if bands
            .values()
            .any(|streams| streams.len() == published.len())
        {
            break;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    assert_eq!(bands.len(), 1, "bands HyperStudy would list: {bands:?}");
    let (key, streams) = bands.into_iter().next().unwrap();
    assert!(key.starts_with(&format!("{BAND}:")));
    let mut names: Vec<_> = streams
        .iter()
        .map(|stream| stream["name"].as_str().unwrap().to_owned())
        .collect();
    names.sort();
    let mut expected = published.clone();
    expected.sort();
    assert_eq!(names, expected);

    // A study recording of those streams, with a marker, as the experiment runner makes it.
    let context = json!({"experiment_id": "exp", "room_id": "room", "participant_id": "p1",
        "session_id": "localhost:room:p1"});
    let uids: Vec<_> = streams.iter().map(|stream| stream["uid"].clone()).collect();
    let started = command(
        &mut socket,
        "recording",
        "start_study_recording",
        json!({"context": context, "stream_uids": uids}),
    )
    .await;
    assert_eq!(started["payload"]["state"], "recording", "{started}");
    let recording_id = started["payload"]["recording_id"].clone();
    tokio::time::sleep(Duration::from_secs(4)).await;
    command(
        &mut socket,
        "recording",
        "send_study_marker",
        json!({"recording_id": recording_id, "context": context,
            "marker": {"eventType": "component.start", "experimentId": "exp", "roomId": "room",
                "participantId": "p1", "timestamp": 1}}),
    )
    .await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    let stopped = command(
        &mut socket,
        "recording",
        "stop_study_recording",
        json!({"recording_id": recording_id, "context": context,
            "end_marker": {"eventType": "experiment.end", "experimentId": "exp", "roomId": "room",
                "participantId": "p1"}}),
    )
    .await;
    let recording = &stopped["payload"];
    assert_eq!(recording["state"], "completed", "{stopped}");
    // The band's streams and the recording's own marker stream.
    assert_eq!(
        recording["streams"],
        json!(published.len() + 1),
        "{stopped}"
    );
    // Six seconds of 125 Hz EEG alone is 750 samples.
    assert!(
        recording["committed_samples"].as_u64().unwrap() > 750,
        "{stopped}"
    );

    state.frenz_native.stop().await;
    let _ = std::fs::remove_dir_all(dir);
}

/// Serves a replayed band on `FRENZ_E2E_PORT` for `FRENZ_E2E_SECONDS`, for driving with HyperStudy's own client code.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore]
async fn serve_a_replayed_band() {
    let port = std::env::var("FRENZ_E2E_PORT")
        .ok()
        .and_then(|port| port.parse().ok())
        .unwrap_or(9017);
    let seconds = std::env::var("FRENZ_E2E_SECONDS")
        .ok()
        .and_then(|seconds| seconds.parse().ok())
        .unwrap_or(120);
    let (state, url, dir) = bridge_with_a_band(port, seconds + 30).await;
    println!("serving {url}; study recordings in {}", dir.display());
    tokio::time::sleep(Duration::from_secs(u64::from(seconds))).await;
    state.frenz_native.stop().await;
}

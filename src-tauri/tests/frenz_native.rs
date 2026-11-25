//! The openfrenz FRENZ source end to end, fed by a replayed synthetic recording in place of a band: the LSL
//! streams it publishes carry data and are named the way HyperStudy recognises FRENZ streams, and the window
//! can read recent samples.

use std::time::Duration;

use hyperstudy_bridge::devices::frenz_native::{FrenzNative, NativeState, MODELS, STREAMS};
use lsl::Pullable;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_replayed_band_is_published_and_readable() {
    let dir = std::env::temp_dir().join(format!("frenz-native-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let recording = dir.join("synthetic.sqlite");
    frenz::synth::write_recording(&recording, 30).unwrap();

    let native = FrenzNative::new();
    assert_eq!(native.status().state, NativeState::Stopped);
    assert!(native
        .start_replay("bad name!", &recording, None)
        .await
        .is_err());

    native
        .start_replay("FRENZT01", &recording, None)
        .await
        .unwrap();
    let status = native.status();
    assert_eq!(status.state, NativeState::Streaming);
    assert_eq!(status.streams.len(), STREAMS.len() + MODELS.len());
    assert!(status.streams.contains(&"FRENZT01_EEG".to_owned()));
    assert!(status
        .streams
        .contains(&"FRENZT01_EOG_openfrenz".to_owned()));
    // One band, one connection.
    assert!(native
        .start_replay("FRENZT01", &recording, None)
        .await
        .is_err());

    // The EEG outlet is visible on the network, identified as HyperStudy expects, and carries timed samples.
    let (found, stamps, width) = tokio::task::spawn_blocking(|| {
        let found = lsl::resolve_bypred("name='FRENZT01_EEG'", 1, 10.0).unwrap();
        let info = found
            .into_iter()
            .next()
            .expect("the EEG stream was not found");
        let inlet = lsl::StreamInlet::new(&info, 360, 0, true).unwrap();
        let full = inlet.info(5.0).unwrap();
        let mut stamps = Vec::new();
        let mut width = 0;
        let until = std::time::Instant::now() + Duration::from_secs(6);
        while stamps.len() < 250 && std::time::Instant::now() < until {
            let (sample, stamp): (Vec<f64>, f64) = inlet.pull_sample(0.5).unwrap();
            if stamp != 0.0 {
                width = sample.len();
                stamps.push(stamp);
            }
        }
        let now = lsl::local_clock();
        (
            (full.source_id(), full.to_xml().unwrap(), now),
            stamps,
            width,
        )
    })
    .await
    .unwrap();
    let (source_id, xml, now) = found;
    let rest = source_id
        .strip_prefix("frenz-FRENZT01-")
        .expect("source id prefix");
    let (session, suffix) = rest.split_at(36);
    assert!(
        uuid::Uuid::parse_str(session).is_ok(),
        "session id {session:?}"
    );
    assert_eq!(suffix, "_EEG");
    assert!(
        xml.contains("<label>forehead_left</label>")
            && xml.contains("<timestamp_policy>source_unix</timestamp_policy>")
    );
    assert_eq!(width, 4);
    assert!(
        stamps.len() >= 250,
        "only {} EEG samples arrived",
        stamps.len()
    );
    assert!(
        stamps.windows(2).all(|w| w[1] >= w[0]),
        "timestamps go backwards"
    );
    // On the LSL clock, and recent: openfrenz's sample times are Unix seconds, moved over by the bridge.
    let last = *stamps.last().unwrap();
    assert!(
        (now - last).abs() < 5.0,
        "last sample is {} s from now on the LSL clock",
        now - last
    );
    let spacing = (last - stamps[0]) / (stamps.len() - 1) as f64;
    assert!(
        (spacing - 0.008).abs() < 0.002,
        "samples are {spacing} s apart"
    );

    // The window's view: the band's streams, and model outputs once they have warmed up.
    tokio::time::sleep(Duration::from_secs(4)).await;
    let traces = native.recent(5.0).await;
    let eeg = traces
        .iter()
        .find(|t| t.name == "eeg")
        .expect("no eeg trace");
    assert_eq!(
        eeg.channels,
        ["forehead_right", "comb_left", "forehead_left", "comb_right"]
    );
    assert!(eeg.t.len() > 400 && eeg.values.iter().all(|c| c.len() == eeg.t.len()));
    let brow = traces
        .iter()
        .find(|t| t.name == "brow-emg")
        .expect("no brow-emg output");
    assert_eq!(brow.channels, ["brow:log_sd"]);
    assert!(brow.t.len() > 5, "brow-emg gave {} steps", brow.t.len());
    let stats = native.stats().expect("no stats while streaming");
    assert_eq!(stats["active_stream_count"], 12);
    assert_eq!(stats["devices"][0]["device_name"], "FRENZT01");
    assert_eq!(stats["devices"][0]["streams"][0]["suffix"], "_EEG");
    assert_eq!(stats["devices"][0]["streams"][0]["channel_count"], 4);
    let status = native.status();
    assert!(status.samples["FRENZT01_EEG"] > 500);
    assert!(status.samples["FRENZT01_BROW_EMG_openfrenz"] > 5);

    native.stop().await;
    assert_eq!(native.status().state, NativeState::Stopped);
    assert!(native.stats().is_none());
    assert!(native.recent(5.0).await.is_empty());
    native.stop().await;
    let _ = std::fs::remove_dir_all(&dir);
}

/// Against a real band: `FRENZ_BAND=FRENZI85 cargo test --test frenz_native real_band -- --ignored --nocapture`.
/// Needs the band on the head, not connected to anything else, and Bluetooth permission for the terminal.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore]
async fn real_band() {
    let name = std::env::var("FRENZ_BAND").expect("set FRENZ_BAND to the band's name");
    let native = FrenzNative::new();
    let bands = native.scan().await.unwrap();
    println!("scan: {bands:?}");
    let started = std::time::Instant::now();
    native.start(&name, None).await.unwrap();
    println!("connected in {:.1} s", started.elapsed().as_secs_f64());
    for _ in 0..8 {
        tokio::time::sleep(Duration::from_secs(5)).await;
        let status = native.status();
        println!("{:?} {:?}", status.state, status.samples);
    }
    let lsl_names = tokio::task::spawn_blocking(|| {
        lsl::resolve_streams(2.0)
            .unwrap()
            .iter()
            .map(|s| format!("{} [{}]", s.stream_name(), s.source_id()))
            .collect::<Vec<_>>()
    })
    .await
    .unwrap();
    println!("on the LSL network: {lsl_names:#?}");
    for trace in native.recent(30.0).await {
        let last: Vec<String> = trace
            .values
            .iter()
            .map(|c| c.last().map_or("-".into(), |v| format!("{v:.2}")))
            .collect();
        println!(
            "{:>10}: {:>5} samples, last {:?} = {:?}",
            trace.name,
            trace.t.len(),
            trace.channels,
            last
        );
    }
    let status = native.status();
    assert_eq!(status.state, NativeState::Streaming);
    assert!(
        status.samples[&format!("{name}_EEG")] > 4000,
        "too few EEG samples"
    );
    native.stop().await;
}

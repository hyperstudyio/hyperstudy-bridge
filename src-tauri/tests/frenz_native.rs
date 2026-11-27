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
        .start_replay("bad name!", &recording, &[], &[], None, None)
        .await
        .is_err());

    // One extra model from a folder (as a Hugging Face model would be once fetched), and one that is not a model.
    let extra = dir.join("rms-model");
    std::fs::create_dir_all(&extra).unwrap();
    std::fs::write(
        extra.join("config.json"),
        r#"{"openfrenz_model": "pipeline", "name": "my level", "version": "1",
            "step": {"stream": "eeg", "samples": 125},
            "nodes": [{"name": "s", "op": "summary", "in": "eeg", "stats": ["sd"]}],
            "outputs": ["s"]}"#,
    )
    .unwrap();
    let models = vec![
        extra.to_string_lossy().to_string(),
        "./not/a model".to_owned(),
    ];
    native
        .start_replay("FRENZT01", &recording, &models, &[], None, None)
        .await
        .unwrap();
    let status = native.status();
    assert_eq!(status.state, NativeState::Streaming);
    assert_eq!(status.streams.len(), STREAMS.len() + MODELS.len() + 1);
    assert!(status
        .streams
        .contains(&"FRENZT01_MY_LEVEL_openfrenz".to_owned()));
    assert_eq!(status.model_errors.len(), 1, "{:?}", status.model_errors);
    assert!(status.model_errors[0].starts_with("./not/a model:"));
    assert!(status.streams.contains(&"FRENZT01_EEG".to_owned()));
    assert!(status
        .streams
        .contains(&"FRENZT01_EOG_openfrenz".to_owned()));
    // One band, one connection.
    assert!(native
        .start_replay("FRENZT01", &recording, &[], &[], None, None)
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
    assert_eq!(stats["active_stream_count"], 13);
    assert_eq!(stats["devices"][0]["device_name"], "FRENZT01");
    assert_eq!(stats["devices"][0]["streams"][0]["suffix"], "_EEG");
    assert_eq!(stats["devices"][0]["streams"][0]["channel_count"], 4);
    let mine = traces
        .iter()
        .find(|t| t.name == "my-level")
        .expect("no output of the extra model");
    assert_eq!(mine.channels.len(), 4);
    assert!(mine.t.len() > 2);
    let status = native.status();
    assert!(status.samples["FRENZT01_EEG"] > 500);
    assert!(status.samples["FRENZT01_BROW_EMG_openfrenz"] > 5);

    // A pipeline from the editor runs on the band, is drawn as `preview`, and is not published.
    let streams_before = native.status().streams.len();
    let pipeline = frenz_model::Pipeline::from_config(&serde_json::json!({
        "step": { "stream": "eeg", "samples": 25 },
        "nodes": [{ "name": "s", "op": "summary", "in": "eeg", "stats": ["mean"] }],
        "outputs": ["s"]
    }))
    .unwrap();
    let names = native.preview(pipeline).await.unwrap();
    assert_eq!(names.len(), 4);
    tokio::time::sleep(Duration::from_secs(2)).await;
    let preview = native
        .recent(5.0)
        .await
        .into_iter()
        .find(|t| t.name == "preview")
        .expect("no preview trace");
    assert!(
        preview.t.len() > 3,
        "the preview gave {} steps",
        preview.t.len()
    );
    assert_eq!(native.status().streams.len(), streams_before);
    native.stop_preview().await;
    assert!(native.recent(5.0).await.iter().all(|t| t.name != "preview"));

    // The editor opens a running model from the folder it was loaded from.
    assert_eq!(
        native.model_dir("my-level").as_deref(),
        Some(extra.as_path())
    );
    assert!(native
        .model_dir("eog")
        .unwrap()
        .join("config.json")
        .is_file());

    // A pipeline started while connected is published and drawn like the others.
    let second = dir.join("second");
    std::fs::create_dir_all(&second).unwrap();
    std::fs::write(
        second.join("config.json"),
        r#"{"openfrenz_model": "pipeline", "name": "second", "version": "1",
            "step": {"stream": "eeg", "samples": 25},
            "nodes": [{"name": "s", "op": "summary", "in": "eeg", "stats": ["mean"]}],
            "outputs": ["s"]}"#,
    )
    .unwrap();
    assert_eq!(
        native.run_model(second.clone(), None).await.unwrap(),
        "second"
    );
    let streams_now = native.status().streams.len();
    assert_eq!(streams_now, streams_before + 1);
    assert!(native
        .status()
        .streams
        .contains(&"FRENZT01_SECOND_openfrenz".to_owned()));
    tokio::time::sleep(Duration::from_secs(2)).await;
    let drawn = native.recent(5.0).await;
    let added = drawn
        .iter()
        .find(|t| t.name == "second")
        .expect("no output of the model started while connected");
    assert!(added.t.len() > 3, "it gave {} steps", added.t.len());

    // Changed and run again, a model replaces its running copy: same name, same stream.
    std::fs::write(
        extra.join("config.json"),
        r#"{"openfrenz_model": "pipeline", "name": "my level", "version": "2",
            "step": {"stream": "eeg", "samples": 25},
            "nodes": [{"name": "s", "op": "summary", "in": "eeg", "stats": ["mean"]}],
            "outputs": ["s"]}"#,
    )
    .unwrap();
    assert_eq!(
        native.run_model(extra.clone(), None).await.unwrap(),
        "my-level"
    );
    assert_eq!(native.status().streams.len(), streams_now);
    tokio::time::sleep(Duration::from_secs(2)).await;
    let replaced = native.recent(5.0).await;
    let mine = replaced
        .iter()
        .find(|t| t.name == "my-level")
        .expect("the replaced model is not drawn");
    assert!(
        mine.channels.iter().all(|c| c.ends_with(":mean")),
        "{:?}",
        mine.channels
    );
    assert!(mine.t.len() > 3);
    assert!(native.run_model(dir.join("missing"), None).await.is_err());

    // An edited copy of a built-in runs in its place: same name, same stream, the copy's outputs.
    let copy = dir.join("brow-emg-edited");
    std::fs::create_dir_all(&copy).unwrap();
    std::fs::write(
        copy.join("config.json"),
        r#"{"openfrenz_model": "pipeline", "name": "brow-emg-edited", "version": "1",
            "step": {"stream": "eeg", "samples": 25},
            "nodes": [{"name": "pairs", "op": "mix", "in": "eeg",
                       "channels": [{"name": "brow", "weights": {"forehead_left": 0.5, "forehead_right": -0.5}}]},
                      {"name": "level", "op": "summary", "in": "pairs", "stats": ["sd"]}],
            "outputs": ["level"]}"#,
    )
    .unwrap();
    // An LSL reader on the built-in's stream, as a study recording would hold.
    let brow_stream = native
        .status()
        .streams
        .into_iter()
        .find(|s| s.ends_with("_BROW_EMG_openfrenz"))
        .unwrap();
    let found = lsl::resolve_byprop("name", &brow_stream, 1, 5.0).unwrap();
    let inlet = lsl::StreamInlet::new(&found[0], 360, 0, true).unwrap();
    inlet.open_stream(5.0).unwrap();
    let streams_then = native.status().streams.len();
    assert!(native
        .run_model(copy.clone(), Some("nothing-running"))
        .await
        .is_err());
    assert_eq!(
        native
            .run_model(copy.clone(), Some("brow-emg"))
            .await
            .unwrap(),
        "brow-emg"
    );
    assert_eq!(native.status().streams.len(), streams_then, "no new stream");
    assert_eq!(native.status().stand_ins["brow-emg"], "brow-emg-edited");
    assert_eq!(
        native.model_dir("brow-emg").as_deref(),
        Some(copy.as_path())
    );
    tokio::time::sleep(Duration::from_secs(2)).await;
    let brow = native
        .recent(5.0)
        .await
        .into_iter()
        .find(|t| t.name == "brow-emg")
        .expect("brow-emg is not drawn");
    assert_eq!(
        brow.channels,
        ["brow:sd"],
        "the copy's outputs, under the built-in's name"
    );
    assert!(brow.t.len() > 3);
    // The reader that was open on the built-in's stream gets the copy's samples.
    let mut sample = vec![0.0_f64; 1];
    let mut got = 0;
    for _ in 0..50 {
        if inlet.pull_sample_buf(&mut sample, 0.2).unwrap_or(0.0) > 0.0 {
            got += 1;
        }
    }
    assert!(got > 3, "the open reader got {got} samples from the copy");
    // Changed again, it keeps standing in.
    assert_eq!(
        native.run_model(copy.clone(), None).await.unwrap(),
        "brow-emg"
    );
    assert!(native.stands_in("brow-emg"));

    // Reverted, the built-in is back on the same stream, and the copy is forgotten.
    assert!(
        native.revert_model("my-level").await.is_err(),
        "only a copy can be reverted"
    );
    native.revert_model("brow-emg").await.unwrap();
    assert!(!native.stands_in("brow-emg"));
    assert_eq!(native.status().streams.len(), streams_then);
    assert_ne!(
        native.model_dir("brow-emg").as_deref(),
        Some(copy.as_path())
    );
    tokio::time::sleep(Duration::from_secs(2)).await;
    let back = native
        .recent(5.0)
        .await
        .into_iter()
        .find(|t| t.name == "brow-emg")
        .expect("brow-emg is not drawn after reverting");
    assert_eq!(
        back.channels,
        ["brow:log_sd"],
        "the built-in's outputs again"
    );
    assert!(back.t.len() > 3);
    assert!(
        native.revert_model("brow-emg").await.is_err(),
        "nothing left to revert"
    );

    // A copy kept in the configuration runs in the built-in's place from the start, and reverts the same way.
    native.stop().await;
    let instead = [
        ("brow-emg".to_owned(), copy.to_string_lossy().to_string()),
        ("not-a-model".to_owned(), copy.to_string_lossy().to_string()),
    ];
    native
        .start_replay("FRENZT01", &recording, &[], &instead, None, None)
        .await
        .unwrap();
    let status = native.status();
    assert_eq!(status.stand_ins["brow-emg"], "brow-emg-edited");
    assert_eq!(status.model_errors.len(), 1, "{:?}", status.model_errors);
    assert!(status.model_errors[0].contains("not-a-model is not running"));
    assert!(status
        .streams
        .contains(&"FRENZT01_BROW_EMG_openfrenz".to_owned()));
    tokio::time::sleep(Duration::from_secs(2)).await;
    let brow = native
        .recent(5.0)
        .await
        .into_iter()
        .find(|t| t.name == "brow-emg")
        .expect("brow-emg is not drawn");
    assert_eq!(brow.channels, ["brow:sd"], "the copy runs from the start");
    native.revert_model("brow-emg").await.unwrap();
    assert!(native.status().stand_ins.is_empty());
    assert!(
        copy.join("config.json").is_file(),
        "reverting never deletes the copy"
    );

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
    native.start(&name, &[], &[], None, None).await.unwrap();
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

/// A private Hugging Face model on a replayed recording:
/// `FRENZ_REPLAY=rec.sqlite FRENZ_MODELS=owner/name HF_TOKEN=... cargo test --test frenz_native hub_model -- --ignored --nocapture`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore]
async fn hub_model_on_a_replay() {
    let recording = std::env::var("FRENZ_REPLAY").expect("set FRENZ_REPLAY to a .sqlite recording");
    let models: Vec<String> = std::env::var("FRENZ_MODELS")
        .expect("set FRENZ_MODELS")
        .split(',')
        .map(str::to_owned)
        .collect();
    let token = std::env::var("HF_TOKEN").ok();
    let native = FrenzNative::new();
    // Without the token: the model is reported, and the rest runs.
    native
        .start_replay(
            "FRENZT02",
            std::path::Path::new(&recording),
            &models,
            &[],
            Some("hf_not_valid".into()),
            None,
        )
        .await
        .unwrap();
    println!("with a bad token: {:?}", native.status().model_errors);
    assert_eq!(native.status().model_errors.len(), models.len());
    native.stop().await;
    native
        .start_replay(
            "FRENZT02",
            std::path::Path::new(&recording),
            &models,
            &[],
            token,
            None,
        )
        .await
        .unwrap();
    assert!(
        native.status().model_errors.is_empty(),
        "{:?}",
        native.status().model_errors
    );
    tokio::time::sleep(Duration::from_secs(16)).await;
    for trace in native.recent(30.0).await {
        if ![
            "eeg",
            "accel",
            "gyro",
            "ppg",
            "heart_rate",
            "spo2",
            "contact",
            "battery",
        ]
        .contains(&trace.name.as_str())
        {
            println!(
                "{:>10}: {} steps, last {:?}",
                trace.name,
                trace.t.len(),
                trace
                    .values
                    .iter()
                    .map(|c| c.last().copied())
                    .collect::<Vec<_>>()
            );
        }
    }
    println!(
        "published: {:?}",
        native
            .status()
            .streams
            .iter()
            .filter(|s| s.contains("openfrenz"))
            .collect::<Vec<_>>()
    );
    native.stop().await;
}

/// A model named like a built-in but for its case would share that model's LSL suffix; two outlets under one
/// suffix used to end publishing for the whole connection.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_model_named_like_a_built_in_gets_its_own_stream() {
    let dir = std::env::temp_dir().join(format!("frenz-native-test-{}", uuid::Uuid::new_v4()));
    let extra = dir.join("EOG");
    std::fs::create_dir_all(&extra).unwrap();
    let recording = dir.join("synthetic.sqlite");
    frenz::synth::write_recording(&recording, 30).unwrap();
    std::fs::write(
        extra.join("config.json"),
        r#"{"openfrenz_model": "pipeline", "name": "EOG", "version": "1",
            "step": {"stream": "eeg", "samples": 125},
            "nodes": [{"name": "s", "op": "summary", "in": "eeg", "stats": ["sd"]}],
            "outputs": ["s"]}"#,
    )
    .unwrap();
    let native = FrenzNative::new();
    native
        .start_replay(
            "FRENZT02",
            &recording,
            &[extra.to_string_lossy().to_string()],
            &[],
            None,
            None,
        )
        .await
        .unwrap();
    let streams = native.status().streams;
    let mut distinct = streams.clone();
    distinct.sort();
    distinct.dedup();
    assert_eq!(distinct.len(), streams.len(), "{streams:?}");
    assert!(
        streams.contains(&"FRENZT02_EOG_2_openfrenz".to_owned()),
        "{streams:?}"
    );

    // Both are on the network with their own widths, and the built-in still carries data.
    let (built_in, copy, got) = tokio::task::spawn_blocking(|| {
        let width = |name: &str| {
            lsl::resolve_bypred(&format!("name='{name}'"), 1, 10.0)
                .unwrap()
                .first()
                .map(|info| info.channel_count())
        };
        let info = lsl::resolve_bypred("name='FRENZT02_EOG_openfrenz'", 1, 10.0).unwrap();
        let inlet = lsl::StreamInlet::new(&info[0], 360, 0, true).unwrap();
        let (sample, stamp): (Vec<f64>, f64) = inlet.pull_sample(8.0).unwrap();
        (
            width("FRENZT02_EOG_openfrenz"),
            width("FRENZT02_EOG_2_openfrenz"),
            stamp != 0.0 && !sample.is_empty(),
        )
    })
    .await
    .unwrap();
    assert_eq!(built_in, Some(5));
    assert_eq!(copy, Some(4));
    assert!(got, "the built-in eog stream carried no data");
    native.stop().await;
    let _ = std::fs::remove_dir_all(dir);
}

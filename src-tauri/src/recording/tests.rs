use super::*;
use crate::devices::lsl::types::SampleData;
fn path() -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "hyperstudy-recording-test-{}.sqlite",
        uuid::Uuid::new_v4()
    ))
}
fn descriptor() -> StreamDescriptor {
    StreamDescriptor {
        uid: "u".into(),
        name: "test".into(),
        source_id: "s".into(),
        channels: 2,
        format: "int64".into(),
        nominal_rate: 125.,
        xml: "<info/>".into(),
    }
}
#[test]
fn exact_values_and_acknowledged_counts_survive_reopen() {
    let p = path();
    let mut store = Store::create(&p, &RecordingContext::default(), &[descriptor()]).unwrap();
    let data = SampleData::Int64(vec![i64::MAX, i64::MIN]);
    let sample = RecordedSample {
        stream: 0,
        timestamp: 1.,
        received: 2.,
        data: data.clone(),
    };
    assert_eq!(store.commit(&[sample.clone(), sample]).unwrap(), 2);
    drop(store);
    let db = rusqlite::Connection::open(&p).unwrap();
    let n: i64 = db
        .query_row("SELECT committed_samples FROM recording", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 2);
    let bytes: Vec<u8> = db
        .query_row("SELECT data FROM blocks", [], |r| r.get(0))
        .unwrap();
    assert_eq!(&bytes[4..20], data.to_bytes());
    let ts: Vec<u8> = db
        .query_row("SELECT timestamps FROM blocks", [], |r| r.get(0))
        .unwrap();
    assert_eq!(ts, [1_f64.to_le_bytes(), 1_f64.to_le_bytes()].concat());
    assert!(Store::create(&p, &RecordingContext::default(), &[descriptor()]).is_err());
    drop(db);
    std::fs::remove_file(p).unwrap();
}
#[test]
fn invalid_sample_does_not_advance_durable_count() {
    let p = path();
    let mut store = Store::create(&p, &RecordingContext::default(), &[descriptor()]).unwrap();
    let bad = RecordedSample {
        stream: 0,
        timestamp: 1.,
        received: 2.,
        data: SampleData::Int64(vec![1]),
    };
    assert!(store.commit(&[bad]).is_err());
    assert_eq!(store.committed, 0);
    let n: i64 = store
        .db
        .query_row("SELECT count(*) FROM blocks", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 0);
}
#[test]
fn recovery_preserves_original_and_labels_interruption() {
    let p = path();
    let mut store = Store::create(&p, &RecordingContext::default(), &[descriptor()]).unwrap();
    store
        .commit(&[RecordedSample {
            stream: 0,
            timestamp: 1.,
            received: 2.,
            data: SampleData::Int64(vec![1, 2]),
        }])
        .unwrap();
    drop(store);
    let recovered = recover(&p).unwrap();
    let original = rusqlite::Connection::open(&p).unwrap();
    let state: String = original
        .query_row("SELECT state FROM recording", [], |r| r.get(0))
        .unwrap();
    assert_eq!(state, "recording");
    let db = rusqlite::Connection::open(&recovered).unwrap();
    let row: (String, i64) = db
        .query_row("SELECT state,committed_samples FROM recording", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    assert_eq!(row, ("interrupted".into(), 1));
}

#[cfg(not(windows))]
#[test]
fn real_lsl_to_sqlite_preserves_int64_and_repeated_timestamps() {
    use lsl::ExPushable;
    use std::time::{Duration, Instant};
    let name = format!("HyperStudy-test-{}", uuid::Uuid::new_v4());
    let info =
        lsl::StreamInfo::new(&name, "Test", 2, 0., lsl::ChannelFormat::Int64, &name).unwrap();
    let outlet = lsl::StreamOutlet::new(&info, 0, 60).unwrap();
    let uid = outlet.info().unwrap().uid();
    let manager = RecordingManager::default();
    let dir = std::env::temp_dir().join(format!("hyperstudy-lsl-test-{}", uuid::Uuid::new_v4()));
    manager
        .start(RecordingRequest {
            stream_uids: vec![uid],
            directory: dir.clone(),
            context: RecordingContext::default(),
        })
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    while manager.status().state == "starting" && Instant::now() < deadline {
        std::thread::yield_now();
    }
    assert_eq!(
        manager.status().state,
        "recording",
        "{:?}",
        manager.status()
    );
    assert!(outlet.wait_for_consumers(5.));
    let ts = lsl::local_clock();
    for i in 0..100_i64 {
        outlet
            .push_sample_ex(&vec![i64::MAX - i, i64::MIN + i], ts, true)
            .unwrap();
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    while manager.status().committed_samples < 100 && Instant::now() < deadline {
        std::thread::yield_now();
    }
    let stopped = manager.stop().unwrap();
    assert_eq!(stopped.state, "completed", "{:?}", stopped);
    assert_eq!(stopped.committed_samples, 100);
    let db = rusqlite::Connection::open(stopped.path.unwrap()).unwrap();
    let mut query = db
        .prepare("SELECT timestamps,data FROM blocks ORDER BY first_sequence")
        .unwrap();
    let rows = query
        .query_map([], |r| {
            Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, Vec<u8>>(1)?))
        })
        .unwrap();
    let mut expected = 0_i64;
    for row in rows {
        let (timestamps, bytes) = row.unwrap();
        assert!(timestamps.chunks_exact(8).all(|t| t == ts.to_le_bytes()));
        for frame in bytes.chunks_exact(20) {
            assert_eq!(u32::from_le_bytes(frame[..4].try_into().unwrap()), 16);
            assert_eq!(
                i64::from_le_bytes(frame[4..12].try_into().unwrap()),
                i64::MAX - expected
            );
            assert_eq!(
                i64::from_le_bytes(frame[12..20].try_into().unwrap()),
                i64::MIN + expected
            );
            expected += 1;
        }
    }
    assert_eq!(expected, 100);
}

#[test]
fn database_full_preserves_previously_acknowledged_transaction() {
    let p = path();
    let mut store = Store::create(&p, &RecordingContext::default(), &[descriptor()]).unwrap();
    let sample = RecordedSample {
        stream: 0,
        timestamp: 1.,
        received: 2.,
        data: SampleData::Int64(vec![1, 2]),
    };
    store.commit(&[sample.clone()]).unwrap();
    let pages: i64 = store
        .db
        .pragma_query_value(None, "page_count", |r| r.get(0))
        .unwrap();
    store
        .db
        .pragma_update(None, "max_page_count", pages)
        .unwrap();
    assert!(store.commit(&vec![sample; 10000]).is_err());
    assert_eq!(store.committed, 1);
    let count: i64 = store
        .db
        .query_row("SELECT committed_samples FROM recording", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1);
}

#[test]
fn failed_checkpoint_does_not_label_recording_completed() {
    let p = path();
    let mut store = Store::create(&p, &RecordingContext::default(), &[descriptor()]).unwrap();
    let reader = rusqlite::Connection::open(&p).unwrap();
    reader
        .execute_batch("BEGIN; SELECT * FROM recording;")
        .unwrap();
    store
        .commit(&[RecordedSample {
            stream: 0,
            timestamp: 1.,
            received: 2.,
            data: SampleData::Int64(vec![1, 2]),
        }])
        .unwrap();
    assert!(store.finish(&p, "completed").is_err());
    reader.execute_batch("ROLLBACK").unwrap();
    let state: String = reader
        .query_row("SELECT state FROM recording", [], |r| r.get(0))
        .unwrap();
    assert_ne!(state, "completed");
    assert!(recover(&p).is_ok());
}

#[cfg(not(windows))]
#[test]
fn stopping_drains_the_native_inlet_backlog() {
    use lsl::ExPushable;
    use std::time::{Duration, Instant};
    let name = format!("HyperStudy-backlog-{}", uuid::Uuid::new_v4());
    let info =
        lsl::StreamInfo::new(&name, "Test", 2, 0., lsl::ChannelFormat::Int64, &name).unwrap();
    let outlet = lsl::StreamOutlet::new(&info, 0, 60).unwrap();
    let resolved = lsl::resolve_byprop("source_id", &name, 1, 5.).unwrap();
    let inlet = lsl::StreamInlet::new(&resolved[0], 60, 0, false).unwrap();
    inlet.open_stream(5.).unwrap();
    assert!(outlet.wait_for_consumers(5.));
    for i in 0..100_i64 {
        outlet
            .push_sample_ex(&vec![i, i], lsl::local_clock(), true)
            .unwrap();
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    while inlet.samples_available() < 100 && Instant::now() < deadline {
        std::thread::yield_now();
    }
    assert_eq!(inlet.samples_available(), 100);
    let p = path();
    let mut store = Store::create(&p, &RecordingContext::default(), &[descriptor()]).unwrap();
    let status = Arc::new(Mutex::new(RecordingStatus::default()));
    worker::drain_at_stop(
        &[inlet],
        &[descriptor()],
        &mut store,
        &mut vec![],
        &status,
        &mut [boundary::StreamEvidence::new(descriptor())],
    )
    .unwrap();
    assert_eq!(store.committed, 100);
    assert_eq!(status.lock().unwrap().committed_samples, 100);
}

#[test]
fn source_regression_cannot_report_a_boundary_as_observed() {
    let mut evidence = boundary::StreamEvidence::new(descriptor());
    evidence.observe(10.0);
    evidence.observe(11.0);
    evidence.observe(10.5);
    assert_eq!(evidence.outcome(10.75, false), "unreliable_timestamps");
}

#[test]
fn boundary_deadline_reports_missing_tail_without_inventing_samples() {
    let mut evidence = boundary::StreamEvidence::new(descriptor());
    evidence.observe(10.0);
    assert_eq!(evidence.outcome(11.0, false), "pending");
    assert_eq!(evidence.outcome(11.0, true), "timed_out");
    assert_eq!(evidence.last_timestamp, Some(10.0));
}

#[test]
fn crossing_a_boundary_never_claims_verified_acquisition_coverage() {
    let mut evidence = boundary::StreamEvidence::new(descriptor());
    evidence.observe(10.0);
    evidence.observe(10.008);
    assert_eq!(evidence.outcome(10.004, false), "observed_unverified");
}

#[test]
fn forward_clock_jump_cannot_report_a_boundary_as_observed() {
    let mut evidence = boundary::StreamEvidence::new(descriptor());
    evidence.observe(10.0);
    evidence.observe(11.0);
    assert_eq!(evidence.outcome(10.5, false), "unreliable_timestamps");
}

/// Manual performance characterization: no hardware or running Bridge required.
#[test]
#[ignore]
fn characterize_multi_inlet_teardown() {
    use std::time::Instant;
    for close_first in [false, true] {
        let mut outlets = Vec::new();
        let mut inlets = Vec::new();
        for _ in 0..24 {
            let name = format!("HyperStudy-teardown-{}", uuid::Uuid::new_v4());
            let info =
                lsl::StreamInfo::new(&name, "Test", 1, 1., lsl::ChannelFormat::Float32, &name)
                    .unwrap();
            let outlet = lsl::StreamOutlet::new(&info, 0, 60).unwrap();
            let resolved = lsl::resolve_byprop("source_id", &name, 1, 5.).unwrap();
            assert!(!resolved.is_empty(), "Synthetic outlet was not discovered");
            let inlet = lsl::StreamInlet::new(&resolved[0], 60, 0, false).unwrap();
            inlet.open_stream(5.).unwrap();
            inlet.info(5.).unwrap();
            let _ = inlet.time_correction_ex(0.);
            outlets.push(outlet);
            inlets.push(inlet);
        }
        let started = Instant::now();
        if close_first {
            for inlet in &inlets {
                inlet.close_stream();
            }
        }
        drop(inlets);
        eprintln!(
            "24 inlet teardown close_first={close_first}: {:?}",
            started.elapsed()
        );
        drop(outlets);
    }
}

#[test]
fn repeated_timestamps_do_not_certify_continuity_for_regular_streams() {
    let mut evidence = boundary::StreamEvidence::new(descriptor());
    evidence.observe(10.0);
    evidence.observe(10.0);
    assert_eq!(evidence.outcome(10.0, false), "unreliable_timestamps");
}

#[test]
fn stop_boundary_validation_rejects_unknown_duplicate_and_nonfinite_targets() {
    let valid = RecordingStopRequest {
        boundaries: vec![SourceBoundary {
            stream_uid: "u".into(),
            source_timestamp: 10.,
        }],
        timeout_ms: 100,
    };
    assert!(valid.validate(&["u".into()]).is_ok());
    assert!(valid.validate(&["different".into()]).is_err());
    let mut duplicate = valid.clone();
    duplicate.boundaries.push(valid.boundaries[0].clone());
    assert!(duplicate.validate(&["u".into(), "second".into()]).is_err());
    let mut nonfinite = valid;
    nonfinite.boundaries[0].source_timestamp = f64::NAN;
    assert!(nonfinite.validate(&["u".into()]).is_err());
    assert!(RecordingStopRequest::default()
        .validate(&["u".into()])
        .is_ok());
    assert!(serde_json::from_str::<RecordingStopRequest>(r#"{"unexpected":1}"#).is_err());
}

#[test]
fn completed_file_retains_timed_out_source_boundary_evidence() {
    let p = path();
    let store = Store::create(&p, &RecordingContext::default(), &[descriptor()]).unwrap();
    let report = RecordingEndReport {
        acquisition_coverage: "unverified".into(),
        requested_lsl: 100.,
        drain_finished_lsl: 101.,
        drain_elapsed_ms: 1000,
        unmapped_streams: vec![],
        streams: vec![StreamEndReport {
            stream_uid: "u".into(),
            source_timestamp: 99.,
            outcome: "timed_out".into(),
            last_timestamp: Some(98.),
            samples_observed: 12,
            backward_steps: 0,
            repeated_timestamps: 0,
            large_forward_steps: 0,
        }],
    };
    store.save_end_report(&report).unwrap();
    store.finish(&p, "completed").unwrap();
    let db = rusqlite::Connection::open(&p).unwrap();
    let (state, json): (String, String) = db
        .query_row("SELECT state,end_report_json FROM recording", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    assert_eq!(state, "completed");
    let saved: RecordingEndReport = serde_json::from_str(&json).unwrap();
    assert_eq!(saved.acquisition_coverage, "unverified");
    assert_eq!(saved.streams[0].outcome, "timed_out");
    assert_eq!(saved.streams[0].last_timestamp, Some(98.));
}

#[test]
fn unreliable_clock_waits_until_the_explicit_deadline() {
    use std::time::Duration;
    let p = path();
    let mut store = Store::create(&p, &RecordingContext::default(), &[descriptor()]).unwrap();
    let status = Arc::new(Mutex::new(RecordingStatus::default()));
    let mut evidence = [boundary::StreamEvidence::new(descriptor())];
    evidence[0].observe(10.);
    evidence[0].observe(11.); // A forward jump must not stop draining early.
    let signal = StopSignal::new(RecordingStopRequest {
        boundaries: vec![SourceBoundary {
            stream_uid: "u".into(),
            source_timestamp: 10.5,
        }],
        timeout_ms: 10,
    });
    let started = signal.started;
    worker::drain_to_boundaries(
        &[],
        &[],
        &mut store,
        &mut vec![],
        &status,
        &mut evidence,
        &signal,
    )
    .unwrap();
    assert!(started.elapsed() >= Duration::from_millis(10));
    assert!(started.elapsed() < Duration::from_secs(2));
    let reports = boundary::reports(&evidence, &signal.request, true);
    assert_eq!(reports[0].outcome, "unreliable_timestamps");
    assert_eq!(store.committed, 0);
}

#[test]
fn study_context_requires_complete_identity_and_ignores_label_for_ownership() {
    let mut context = RecordingContext::default();
    assert!(super::study::validate_context(&context).is_err());
    context.experiment_id = Some("experiment".into());
    context.room_id = Some("room".into());
    context.participant_id = Some("participant".into());
    context.session_id = Some("session".into());
    assert!(super::study::validate_context(&context).is_ok());
    let mut other = context.clone();
    other.label = Some("new label".into());
    assert!(super::study::same_context(&context, &other));
    other.participant_id = Some("different".into());
    assert!(!super::study::same_context(&context, &other));
}
#[test]
fn study_manifest_rejects_unfinalized_and_hashes_exact_file() {
    let p = path();
    let context = RecordingContext {
        experiment_id: Some("experiment".into()),
        room_id: Some("room".into()),
        participant_id: Some("participant".into()),
        session_id: Some("session".into()),
        label: None,
    };
    let store = Store::create(&p, &context, &[descriptor()]).unwrap();
    assert!(super::upload::manifest(&p, "recording").is_err());
    store.finish(&p, "completed").unwrap();
    let manifest = super::upload::manifest(&p, "recording").unwrap();
    assert_eq!(manifest["fileSize"], std::fs::metadata(&p).unwrap().len());
    assert_eq!(manifest["participantId"], "participant");
    assert_eq!(manifest["sampleCount"], 0);
    assert_eq!(manifest["sha256"].as_str().unwrap().len(), 64);
}
#[test]
fn upload_targets_are_exact_trusted_hosts() {
    use super::upload::{validate_api_base, validate_upload_url};
    assert!(validate_api_base("https://dev.hyperstudy.io").is_ok());
    assert!(validate_api_base("https://dev.hyperstudy.io.evil.test").is_err());
    assert!(validate_api_base("https://user:pass@dev.hyperstudy.io").is_err());
    assert!(validate_api_base("https://dev.hyperstudy.io/path").is_err());
    assert!(
        validate_upload_url("https://storage.googleapis.com/bucket/object?signature=value").is_ok()
    );
    assert!(validate_upload_url("https://storage.googleapis.com.evil.test/bucket").is_err());
    assert!(validate_upload_url("http://storage.googleapis.com/bucket").is_err());
}

#[test]
fn study_ready_attach_markers_scoped_stop_and_final_manifest() {
    let name = format!("HyperStudy-study-test-{}", uuid::Uuid::new_v4());
    let info =
        lsl::StreamInfo::new(&name, "Test", 1, 0., lsl::ChannelFormat::String, &name).unwrap();
    let outlet = lsl::StreamOutlet::new(&info, 0, 60).unwrap();
    let uid = outlet.info().unwrap().uid();
    let manager = RecordingManager::default();
    let dir = std::env::temp_dir().join(&name);
    manager.configure_study_directory(dir).unwrap();
    let context = RecordingContext {
        experiment_id: Some("e".into()),
        room_id: Some("r".into()),
        participant_id: Some("p".into()),
        session_id: Some("s".into()),
        label: None,
    };
    let started = manager
        .start_study(super::study::StudyStartRequest {
            stream_uids: vec![uid.clone()],
            context: context.clone(),
        })
        .unwrap();
    assert_eq!(started.recording.state, "recording");
    assert_eq!(started.recording.streams, 2);
    assert!(manager
        .stop_unmanaged(RecordingStopRequest::default())
        .is_err());
    let attached = manager
        .start_study(super::study::StudyStartRequest {
            stream_uids: vec![uid.clone()],
            context: context.clone(),
        })
        .unwrap();
    assert_eq!(attached.recording_id, started.recording_id);
    let mut wrong = context.clone();
    wrong.room_id = Some("another-room".into());
    assert!(manager
        .start_study(super::study::StudyStartRequest {
            stream_uids: vec![uid],
            context: wrong.clone()
        })
        .is_err());
    assert!(manager
        .stop_study(super::study::StudyScope {
            recording_id: started.recording_id.clone(),
            context: wrong,
            end_marker: None
        })
        .is_err());
    let ack = manager
        .study_marker(
            &started.recording_id,
            &context,
            serde_json::json!({"eventType":"experiment.start"}),
        )
        .unwrap();
    assert!(ack["marker_lsl_timestamp"].as_f64().unwrap().is_finite());
    let stopped = manager
        .stop_study(super::study::StudyScope {
            recording_id: started.recording_id.clone(),
            context: context.clone(),
            end_marker: Some(serde_json::json!({"eventType":"experiment.end"})),
        })
        .unwrap();
    assert_eq!(stopped.recording.state, "completed");
    assert_eq!(stopped.recording.committed_samples, 2);
    assert_eq!(
        stopped
            .recording
            .end_report
            .as_ref()
            .unwrap()
            .acquisition_coverage,
        "unverified"
    );
    let duplicate = manager
        .stop_study(super::study::StudyScope {
            recording_id: started.recording_id.clone(),
            context,
            end_marker: Some(serde_json::json!({"eventType":"experiment.end"})),
        })
        .unwrap();
    assert_eq!(duplicate.recording.committed_samples, 2);
    let manifest = super::upload::manifest(
        &manager.study_path(&started.recording_id).unwrap(),
        &started.recording_id,
    )
    .unwrap();
    assert_eq!(manifest["sampleCount"], 2);
    assert_eq!(
        manifest["streams"][1]["source_id"],
        "hyperstudy-bridge-study"
    );
}

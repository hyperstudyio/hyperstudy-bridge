use super::boundary::{observations_satisfied, reports, StreamEvidence};
use super::*;
use anyhow::{bail, Context};
use lsl::Pullable;
use rusqlite::OptionalExtension;
use std::{
    path::Path,
    time::{Duration, Instant},
};

fn descriptor(info: &lsl::StreamInfo) -> Result<StreamDescriptor> {
    let format = match info.channel_format() {
        lsl::ChannelFormat::Float32 => "float32",
        lsl::ChannelFormat::Double64 => "float64",
        lsl::ChannelFormat::String => "string",
        lsl::ChannelFormat::Int64 => "int64",
        lsl::ChannelFormat::Int32 => "int32",
        lsl::ChannelFormat::Int16 => "int16",
        lsl::ChannelFormat::Int8 => "int8",
        _ => bail!("Unsupported LSL format"),
    };
    Ok(StreamDescriptor {
        uid: info.uid(),
        name: info.stream_name(),
        source_id: info.source_id(),
        channels: info.channel_count() as u32,
        nominal_rate: info.nominal_srate(),
        format: format.into(),
        xml: info.to_xml()?,
    })
}
pub fn discover() -> Result<Vec<StreamDescriptor>> {
    lsl::resolve_streams(1.)?.iter().map(descriptor).collect()
}
pub fn describe(uid: &str) -> Result<StreamDescriptor> {
    ensure!(
        !uid.trim().is_empty() && !uid.contains('\0'),
        "A valid stream UID is required"
    );
    let available = lsl::resolve_byprop("uid", uid, 1, 3.)?;
    let info = available
        .iter()
        .find(|info| info.uid() == uid)
        .context("Selected LSL stream is no longer available")?;
    // Fetch full XML without subscribing to samples, timestamp postprocessing,
    // or source recovery. Discovery alone only supplies abbreviated metadata.
    let inlet = lsl::StreamInlet::new(info, 1, 0, false)?;
    let full = inlet.info(3.)?;
    ensure!(full.uid() == uid, "Selected LSL stream identity changed");
    descriptor(&full)
}
fn pull(inlet: &lsl::StreamInlet, format: &str) -> Result<Option<(SampleData, f64)>> {
    macro_rules! sample {
        ($t:ty,$v:ident) => {{
            let (v, t): (Vec<$t>, f64) = inlet.pull_sample(0.)?;
            if t == 0. {
                Ok(None)
            } else {
                Ok(Some((SampleData::$v(v), t)))
            }
        }};
    }
    match format {
        "float32" => sample!(f32, Float32),
        "float64" => sample!(f64, Float64),
        "string" => sample!(String, String),
        "int32" => sample!(i32, Int32),
        "int16" => sample!(i16, Int16),
        "int8" => sample!(i8, Int8),
        #[cfg(not(windows))]
        "int64" => sample!(i64, Int64),
        _ => bail!("LSL format {format} is not supported by this platform's binding"),
    }
}
fn commit(
    store: &mut Store,
    pending: &mut Vec<RecordedSample>,
    status: &Arc<Mutex<RecordingStatus>>,
) -> Result<()> {
    let count = store.commit(pending)?;
    pending.clear(); // Never discard unacknowledged data on an insert failure.
    lock(&status).committed_samples = count;
    Ok(())
}
pub fn record(
    request: RecordingRequest,
    path: &Path,
    stop: mpsc::Receiver<WorkerCommand>,
    status: Arc<Mutex<RecordingStatus>>,
    study: bool,
    ready: mpsc::Sender<()>,
) -> Result<()> {
    let available = lsl::resolve_streams(1.)?;
    let mut inlets = Inlets(Vec::new());
    let mut specs = Vec::new();
    for uid in &request.stream_uids {
        if stop.try_recv().is_ok() {
            lock(&status).state = "cancelled".into();
            return Ok(());
        }
        let info = available
            .iter()
            .find(|s| s.uid() == *uid)
            .context("Selected LSL stream is no longer available")?;
        #[cfg(windows)]
        ensure!(
            info.channel_format() != lsl::ChannelFormat::Int64,
            "Int64 recording is unsupported on Windows by this LSL binding"
        );
        // No timestamp postprocessing and no silent source recovery.
        let inlet = lsl::StreamInlet::new(info, 360, 0, false)?;
        inlet.open_stream(5.)?;
        specs.push(descriptor(&inlet.info(5.)?)?);
        inlets.push(inlet);
    }
    let marker_index = if study {
        let id = specs.len();
        specs.push(StreamDescriptor {
            uid: format!(
                "study-markers-{}",
                path.file_stem().unwrap().to_string_lossy()
            ),
            name: "HyperStudy Study Markers".into(),
            source_id: "hyperstudy-bridge-study".into(),
            channels: 1,
            format: "string".into(),
            nominal_rate: 0.,
            xml: "<info><type>Markers</type><clock>bridge_local_lsl</clock></info>".into(),
        });
        Some(id)
    } else {
        None
    };
    let mut store = Store::create(path, &request.context, &specs)?;
    lock(&status).state = "recording".into();
    lock(&status).streams = specs.len();
    let _ = ready.send(());
    let mut evidence: Vec<_> = specs.iter().cloned().map(StreamEvidence::new).collect();
    let mut pending = Vec::with_capacity(4096);
    let mut flush = Instant::now() + Duration::from_millis(100);
    let mut clocks = Instant::now();
    let mut signal = loop {
        if let Ok(command) = stop.try_recv() {
            match command {
                WorkerCommand::Stop(signal) => break signal,
                WorkerCommand::Marker { marker, reply } => {
                    let result =
                        write_marker(&mut store, &mut pending, &status, marker_index, marker)
                            .map_err(|e| e.to_string());
                    let _ = reply.send(result);
                }
            }
        }
        for (id, inlet) in inlets.iter().enumerate() {
            // Bound per-stream work so busy streams cannot starve peers or stop.
            for _ in 0..512 {
                match pull(inlet, &specs[id].format) {
                    Ok(Some((data, timestamp))) => {
                        evidence[id].observe(timestamp);
                        pending.push(RecordedSample {
                            stream: id,
                            data,
                            timestamp,
                            received: lsl::local_clock(),
                        });
                    }
                    Ok(None) => break,
                    Err(error) => {
                        commit(&mut store, &mut pending, &status)?;
                        store.db.execute(
                            "UPDATE recording SET state='failed',error=? WHERE id=1",
                            [error.to_string()],
                        )?;
                        return Err(error.context("LSL source failed; recording stopped"));
                    }
                }
                if pending.len() >= 4096 {
                    commit(&mut store, &mut pending, &status)?;
                }
            }
        }
        if Instant::now() >= flush {
            commit(&mut store, &mut pending, &status)?;
            flush = Instant::now() + Duration::from_millis(100);
        }
        if Instant::now() >= clocks {
            for (id, inlet) in inlets.iter().enumerate() {
                // Zero timeout: do not stall acquisition waiting for a clock estimate.
                match inlet.time_correction_ex(0.) {
                    Ok((correction, remote, uncertainty)) => {
                        store.db.execute(
                            "INSERT INTO clock_observations VALUES(?,?,?,?,?,NULL)",
                            rusqlite::params![
                                id as i64,
                                lsl::local_clock(),
                                remote,
                                correction,
                                uncertainty
                            ],
                        )?;
                    }
                    Err(error) => {
                        store.db.execute(
                            "INSERT INTO clock_observations VALUES(?,?,NULL,NULL,NULL,?)",
                            rusqlite::params![id as i64, lsl::local_clock(), format!("{error:?}")],
                        )?;
                    }
                }
            }
            clocks = Instant::now() + Duration::from_secs(5);
        }
        // Interruptible acquisition polling cadence; no synchronization sleeps.
        match stop.recv_timeout(Duration::from_millis(5)) {
            Ok(WorkerCommand::Stop(signal)) => break signal,
            Ok(WorkerCommand::Marker { marker, reply }) => {
                let result = write_marker(&mut store, &mut pending, &status, marker_index, marker)
                    .map_err(|e| e.to_string());
                let _ = reply.send(result);
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                break StopSignal::new(RecordingStopRequest::default())
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
    };
    lock(&status).state = "stopping".into();
    if let Some(marker) = signal.end_marker.take() {
        signal.requested_lsl =
            write_marker(&mut store, &mut pending, &status, marker_index, marker)?;
    }
    let mut unmapped_streams = Vec::new();
    if signal.map_local_boundary {
        // LSL correction maps remote -> local: source target = local - correction.
        // Retain uncertainty in clock_observations; no acquisition precision claim.
        for (id, spec) in specs.iter().enumerate().take(inlets.len()) {
            let measured: Option<(f64, f64, f64)> = store.db.query_row(
                "SELECT observed_lsl,correction,uncertainty FROM clock_observations WHERE stream_id=? AND correction IS NOT NULL ORDER BY rowid DESC LIMIT 1",
                [id as i64], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
            if let Some((_, correction, _)) = measured.filter(|(observed, c, u)| {
                c.is_finite()
                    && u.is_finite()
                    && *u >= 0.
                    && signal.requested_lsl - *observed <= 10.
                    && signal.requested_lsl >= *observed
            }) {
                signal.request.boundaries.push(SourceBoundary {
                    stream_uid: spec.uid.clone(),
                    source_timestamp: signal.requested_lsl - correction,
                });
            } else {
                unmapped_streams.push(spec.uid.clone());
            }
        }
    }
    drain_to_boundaries(
        &inlets,
        &specs,
        &mut store,
        &mut pending,
        &status,
        &mut evidence,
        &signal,
    )?;
    if signal.map_local_boundary {
        drain_at_stop_until(
            &inlets,
            &specs,
            &mut store,
            &mut pending,
            &status,
            &mut evidence,
            Some(signal.started + Duration::from_millis(signal.request.timeout_ms)),
        )?;
    } else {
        drain_at_stop(
            &inlets,
            &specs,
            &mut store,
            &mut pending,
            &status,
            &mut evidence,
        )?;
    }
    let report = RecordingEndReport {
        acquisition_coverage: if !signal.map_local_boundary && signal.request.boundaries.is_empty()
        {
            "not_requested"
        } else {
            "unverified"
        }
        .into(),
        requested_lsl: signal.requested_lsl,
        drain_finished_lsl: lsl::local_clock(),
        drain_elapsed_ms: signal.started.elapsed().as_millis() as u64,
        streams: reports(&evidence, &signal.request, true),
        unmapped_streams,
    };
    store.save_end_report(&report)?;
    lock(&status).end_report = Some(report);
    let finalization_started = Instant::now();
    store.finish(path, "completed")?;
    let finalization_ms = finalization_started.elapsed().as_millis();
    let teardown_started = Instant::now();
    // All native reads are finished before cancellation. Cancel every receiver
    // before dropping any handle, so teardown is not serialized per stream.
    drop(inlets);
    tracing::info!(
        finalization_ms,
        inlet_teardown_ms = teardown_started.elapsed().as_millis(),
        "Recording file finalized and native inlets closed"
    );
    lock(&status).state = "completed".into();
    Ok(())
}

/// Freeze the queued count per inlet, then drain only that prefix. Producers
/// can keep running; stop never waits for them to become idle.
pub(super) fn drain_at_stop(
    inlets: &[lsl::StreamInlet],
    specs: &[StreamDescriptor],
    store: &mut Store,
    pending: &mut Vec<RecordedSample>,
    status: &Arc<Mutex<RecordingStatus>>,
    evidence: &mut [StreamEvidence],
) -> Result<()> {
    drain_at_stop_until(inlets, specs, store, pending, status, evidence, None)
}
fn drain_at_stop_until(
    inlets: &[lsl::StreamInlet],
    specs: &[StreamDescriptor],
    store: &mut Store,
    pending: &mut Vec<RecordedSample>,
    status: &Arc<Mutex<RecordingStatus>>,
    evidence: &mut [StreamEvidence],
    deadline: Option<Instant>,
) -> Result<()> {
    let counts: Vec<_> = inlets.iter().map(|i| i.samples_available()).collect();
    for (id, count) in counts.into_iter().enumerate() {
        for _ in 0..count {
            if deadline.is_some_and(|d| Instant::now() >= d) {
                break;
            }
            let (data, timestamp) = pull(&inlets[id], &specs[id].format)?
                .context("Inlet backlog disappeared during stop")?;
            evidence[id].observe(timestamp);
            pending.push(RecordedSample {
                stream: id,
                data,
                timestamp,
                received: lsl::local_clock(),
            });
            if pending.len() >= 4096 {
                commit(store, pending, status)?;
            }
        }
    }
    commit(store, pending, status)
}

// Cancellation is nonblocking in liblsl. Keep the native handles on their owner
// thread; StreamInlet deliberately does not implement Send or Sync.
struct Inlets(Vec<lsl::StreamInlet>);
impl std::ops::Deref for Inlets {
    type Target = Vec<lsl::StreamInlet>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl std::ops::DerefMut for Inlets {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}
impl Drop for Inlets {
    fn drop(&mut self) {
        for inlet in &self.0 {
            inlet.close_stream();
        }
    }
}

pub(super) fn drain_to_boundaries(
    inlets: &[lsl::StreamInlet],
    specs: &[StreamDescriptor],
    store: &mut Store,
    pending: &mut Vec<RecordedSample>,
    status: &Arc<Mutex<RecordingStatus>>,
    evidence: &mut [StreamEvidence],
    signal: &StopSignal,
) -> Result<()> {
    if signal.request.boundaries.is_empty() {
        return Ok(());
    }
    let deadline = signal.started + Duration::from_millis(signal.request.timeout_ms);
    loop {
        if Instant::now() >= deadline
            || observations_satisfied(&reports(evidence, &signal.request, false))
        {
            break;
        }
        drain_at_stop_until(
            inlets,
            specs,
            store,
            pending,
            status,
            evidence,
            Some(deadline),
        )?;
        // The same bounded acquisition cadence used during recording. This is
        // polling until explicit observations/deadline, not a guessed tail delay.
        let remaining = deadline.saturating_duration_since(Instant::now());
        std::thread::park_timeout(remaining.min(Duration::from_millis(5)));
    }
    Ok(())
}

fn write_marker(
    store: &mut Store,
    pending: &mut Vec<RecordedSample>,
    status: &Arc<Mutex<RecordingStatus>>,
    marker_index: Option<usize>,
    marker: serde_json::Value,
) -> Result<f64> {
    let stream = marker_index.context("This recording has no study marker stream")?;
    let value = serde_json::to_string(&marker)?;
    ensure!(value.len() <= 64 * 1024, "Marker exceeds 64 KiB");
    let timestamp = lsl::local_clock();
    pending.push(RecordedSample {
        stream,
        timestamp,
        received: timestamp,
        data: SampleData::String(vec![value]),
    });
    commit(store, pending, status)?;
    Ok(timestamp)
}

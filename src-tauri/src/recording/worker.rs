use super::*;
use anyhow::{bail, Context};
use lsl::Pullable;
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
    status.lock().unwrap().committed_samples = count;
    Ok(())
}
pub fn record(
    request: RecordingRequest,
    path: &Path,
    stop: mpsc::Receiver<()>,
    status: Arc<Mutex<RecordingStatus>>,
) -> Result<()> {
    let available = lsl::resolve_streams(1.)?;
    let mut inlets = Vec::new();
    let mut specs = Vec::new();
    for uid in &request.stream_uids {
        if stop.try_recv().is_ok() {
            status.lock().unwrap().state = "cancelled".into();
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
    let mut store = Store::create(path, &request.context, &specs)?;
    status.lock().unwrap().state = "recording".into();
    let mut pending = Vec::with_capacity(4096);
    let mut flush = Instant::now() + Duration::from_millis(100);
    let mut clocks = Instant::now();
    loop {
        if stop.try_recv().is_ok() {
            break;
        }
        for (id, inlet) in inlets.iter().enumerate() {
            // Bound per-stream work so busy streams cannot starve peers or stop.
            for _ in 0..512 {
                match pull(inlet, &specs[id].format) {
                    Ok(Some((data, timestamp))) => pending.push(RecordedSample {
                        stream: id,
                        data,
                        timestamp,
                        received: lsl::local_clock(),
                    }),
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
            Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
    }
    status.lock().unwrap().state = "stopping".into();
    drain_at_stop(&inlets, &specs, &mut store, &mut pending, &status)?;
    store.finish(path, "completed")?;
    status.lock().unwrap().state = "completed".into();
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
) -> Result<()> {
    let counts: Vec<_> = inlets.iter().map(|i| i.samples_available()).collect();
    for (id, count) in counts.into_iter().enumerate() {
        for _ in 0..count {
            let (data, timestamp) = pull(&inlets[id], &specs[id].format)?
                .context("Inlet backlog disappeared during stop")?;
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

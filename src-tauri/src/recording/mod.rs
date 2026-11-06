//! Independent LSL acquisition with per-recording SQLite FULL WAL persistence.
mod boundary;
mod store;
mod worker;
use crate::devices::lsl::types::SampleData;
use anyhow::{ensure, Result};
pub use boundary::{RecordingEndReport, StreamEndReport};
use serde::{Deserialize, Serialize};
use std::{
    path::PathBuf,
    sync::{mpsc, Arc, Mutex},
    thread::JoinHandle,
};
pub use store::recover;
use store::Store;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordingContext {
    pub experiment_id: Option<String>,
    pub room_id: Option<String>,
    pub participant_id: Option<String>,
    pub session_id: Option<String>,
    pub label: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordingRequest {
    pub stream_uids: Vec<String>,
    pub directory: PathBuf,
    #[serde(default)]
    pub context: RecordingContext,
}
/// Targets use each stream's ORIGINAL source LSL timestamp domain. Callers
/// must not substitute local wall-clock time or assume acquisition alignment.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceBoundary {
    pub stream_uid: String,
    pub source_timestamp: f64,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordingStopRequest {
    #[serde(default)]
    pub boundaries: Vec<SourceBoundary>,
    #[serde(default)]
    pub timeout_ms: u64,
}
impl RecordingStopRequest {
    fn validate(&self, selected: &[String]) -> Result<()> {
        ensure!(
            self.timeout_ms <= 120_000,
            "Stop deadline must not exceed 120 seconds"
        );
        ensure!(
            self.boundaries.len() <= selected.len(),
            "Too many stop boundaries"
        );
        let mut seen = std::collections::HashSet::new();
        for target in &self.boundaries {
            ensure!(
                selected.contains(&target.stream_uid),
                "Boundary stream is not being recorded"
            );
            ensure!(
                seen.insert(&target.stream_uid),
                "Duplicate boundary stream UID"
            );
            ensure!(
                target.source_timestamp.is_finite(),
                "Invalid boundary timestamp"
            );
        }
        ensure!(
            !self.boundaries.is_empty() || self.timeout_ms == 0,
            "A stop timeout requires explicit source boundaries"
        );
        Ok(())
    }
}
struct StopSignal {
    request: RecordingStopRequest,
    requested_lsl: f64,
    started: std::time::Instant,
}
impl StopSignal {
    fn new(request: RecordingStopRequest) -> Self {
        Self {
            request,
            requested_lsl: lsl::local_clock(),
            started: std::time::Instant::now(),
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamDescriptor {
    pub uid: String,
    pub name: String,
    pub source_id: String,
    pub channels: u32,
    pub format: String,
    pub nominal_rate: f64,
    pub xml: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordingStatus {
    pub state: String,
    pub path: Option<PathBuf>,
    pub committed_samples: u64,
    pub error: Option<String>,
    pub streams: usize,
    pub end_report: Option<RecordingEndReport>,
}
impl Default for RecordingStatus {
    fn default() -> Self {
        Self {
            state: "idle".into(),
            path: None,
            committed_samples: 0,
            error: None,
            streams: 0,
            end_report: None,
        }
    }
}
#[derive(Clone)]
pub(super) struct RecordedSample {
    pub stream: usize,
    pub timestamp: f64,
    pub received: f64,
    pub data: SampleData,
}
impl RecordedSample {
    fn format(&self) -> &'static str {
        match &self.data {
            SampleData::Float32(_) => "float32",
            SampleData::Float64(_) => "float64",
            SampleData::Int64(_) => "int64",
            SampleData::Int32(_) => "int32",
            SampleData::Int16(_) => "int16",
            SampleData::Int8(_) => "int8",
            SampleData::String(_) => "string",
        }
    }
}
struct Active {
    stop: mpsc::Sender<StopSignal>,
    stream_uids: Vec<String>,
    handle: JoinHandle<()>,
}
#[derive(Default)]
pub struct RecordingManager {
    active: Mutex<Option<Active>>,
    status: Arc<Mutex<RecordingStatus>>,
}
impl RecordingManager {
    pub fn status(&self) -> RecordingStatus {
        self.status.lock().unwrap().clone()
    }
    pub fn start(&self, request: RecordingRequest) -> Result<RecordingStatus> {
        ensure!(
            !request.stream_uids.is_empty() && request.stream_uids.len() <= 64,
            "Select 1–64 streams"
        );
        ensure!(
            request.stream_uids.iter().all(|u| !u.is_empty()),
            "Empty stream UID"
        );
        ensure!(
            request
                .stream_uids
                .iter()
                .collect::<std::collections::HashSet<_>>()
                .len()
                == request.stream_uids.len(),
            "Duplicate stream UID"
        );
        let mut active = self.active.lock().unwrap();
        if active.as_ref().is_some_and(|a| a.handle.is_finished()) {
            if let Some(old) = active.take() {
                let _ = old.handle.join();
            }
        }
        ensure!(active.is_none(), "A recording is already active");
        std::fs::create_dir_all(&request.directory)?;
        let path = request
            .directory
            .join(format!("{}.sqlite", uuid::Uuid::new_v4()));
        *self.status.lock().unwrap() = RecordingStatus {
            state: "starting".into(),
            path: Some(path.clone()),
            committed_samples: 0,
            error: None,
            streams: request.stream_uids.len(),
            end_report: None,
        };
        let stream_uids = request.stream_uids.clone();
        let (stop, receiver) = mpsc::channel();
        let status = self.status.clone();
        let handle = std::thread::spawn(move || {
            if let Err(error) = worker::record(request, &path, receiver, status.clone()) {
                let mut s = status.lock().unwrap();
                s.state = "failed".into();
                s.error = Some(error.to_string());
            }
        });
        *active = Some(Active {
            stop,
            handle,
            stream_uids,
        });
        Ok(self.status())
    }
    pub fn stop(&self) -> Result<RecordingStatus> {
        self.stop_with(RecordingStopRequest::default())
    }
    pub fn stop_with(&self, request: RecordingStopRequest) -> Result<RecordingStatus> {
        // Hold lifecycle lock through join so a new start cannot race finalization.
        let mut active = self.active.lock().unwrap();
        if let Some(running) = active.as_ref() {
            request.validate(&running.stream_uids)?;
        }
        if let Some(running) = active.take() {
            let _ = running.stop.send(StopSignal::new(request));
            running
                .handle
                .join()
                .map_err(|_| anyhow::anyhow!("Recorder worker panicked"))?;
        }
        Ok(self.status())
    }
}
impl Drop for RecordingManager {
    fn drop(&mut self) {
        if let Ok(active) = self.active.get_mut() {
            if let Some(r) = active.take() {
                let _ = r
                    .stop
                    .send(StopSignal::new(RecordingStopRequest::default()));
                let _ = r.handle.join();
            }
        }
    }
}
pub fn discover() -> Result<Vec<StreamDescriptor>> {
    worker::discover()
}
#[cfg(test)]
mod tests;

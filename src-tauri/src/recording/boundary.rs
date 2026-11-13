//! Source-timestamp observations are evidence, never proof of acquisition coverage.
use super::{RecordingStopRequest, StreamDescriptor};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamEndReport {
    pub stream_uid: String,
    pub source_timestamp: f64,
    pub outcome: String,
    pub last_timestamp: Option<f64>,
    pub samples_observed: u64,
    pub backward_steps: u64,
    pub repeated_timestamps: u64,
    pub large_forward_steps: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordingEndReport {
    /// No outcome in this report certifies true device acquisition coverage.
    pub acquisition_coverage: String,
    pub requested_lsl: f64,
    pub drain_finished_lsl: f64,
    pub drain_elapsed_ms: u64,
    pub streams: Vec<StreamEndReport>,
    #[serde(default)]
    pub unmapped_streams: Vec<String>,
}

pub(super) struct StreamEvidence {
    uid: String,
    nominal_rate: f64,
    pub last_timestamp: Option<f64>,
    samples: u64,
    backwards: u64,
    repeated: u64,
    forward_gaps: u64,
}
impl StreamEvidence {
    pub fn new(spec: StreamDescriptor) -> Self {
        Self {
            uid: spec.uid,
            nominal_rate: spec.nominal_rate,
            last_timestamp: None,
            samples: 0,
            backwards: 0,
            repeated: 0,
            forward_gaps: 0,
        }
    }
    pub fn observe(&mut self, timestamp: f64) {
        if let Some(previous) = self.last_timestamp {
            let delta = timestamp - previous;
            if delta == 0. && self.nominal_rate > 0. {
                self.repeated += 1;
            }
            if delta < 0. {
                self.backwards += 1;
            }
            // A gap of more than 1.5 nominal periods is not continuous evidence.
            // This can be dropped data, batch timestamps, or a clock jump: do not
            // guess which, and do not silently repair the original timestamps.
            if self.nominal_rate > 0. && delta > 1.5 / self.nominal_rate {
                self.forward_gaps += 1;
            }
        }
        self.last_timestamp = Some(timestamp);
        self.samples += 1;
    }
    pub fn outcome(&self, target: f64, expired: bool) -> &'static str {
        if self.backwards > 0 || self.forward_gaps > 0 || self.repeated > 0 {
            "unreliable_timestamps"
        } else if self.last_timestamp.is_some_and(|last| last >= target) {
            "observed_unverified"
        } else if expired {
            "timed_out"
        } else {
            "pending"
        }
    }
    fn report(&self, target: f64, expired: bool) -> StreamEndReport {
        StreamEndReport {
            stream_uid: self.uid.clone(),
            source_timestamp: target,
            outcome: self.outcome(target, expired).into(),
            last_timestamp: self.last_timestamp,
            samples_observed: self.samples,
            backward_steps: self.backwards,
            repeated_timestamps: self.repeated,
            large_forward_steps: self.forward_gaps,
        }
    }
}

pub(super) fn reports(
    evidence: &[StreamEvidence],
    request: &RecordingStopRequest,
    expired: bool,
) -> Vec<StreamEndReport> {
    request
        .boundaries
        .iter()
        .map(|target| {
            // Manager validates requested UIDs against the active selection.
            evidence
                .iter()
                .find(|e| e.uid == target.stream_uid)
                .unwrap()
                .report(target.source_timestamp, expired)
        })
        .collect()
}

/// Unreliable clocks must continue acquiring until the explicit deadline; a
/// forward jump cannot cause an early stop and trim a later backwards batch.
pub(super) fn observations_satisfied(reports: &[StreamEndReport]) -> bool {
    reports.iter().all(|r| r.outcome == "observed_unverified")
}

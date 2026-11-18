//! The live estimate of each Neon phone's clock against this computer's, and the correction it implies for the
//! event timestamps Bridge sends to the phone.
//!
//! Why: the Companion app reads an event's Unix timestamp on the phone's own clock. A phone whose clock is off (one
//! lab phone ran 0.5 s fast) files every HyperStudy marker that far off in its recording and in Pupil Cloud, data
//! that never comes back through HyperStudy and so can only be fixed as the event is sent.
//!
//! How: Time Echo exchanges (see `recording::clock`) feed a per-phone window of recent exchanges. The estimate is the
//! offset of the exchange with the shortest round trip in the last `WINDOW`, the NTP clock-filter rule: delay only
//! ever adds error, and an exchange's error is bounded by half its round trip. Drift over the window is far below
//! that bound; the after-session fit, from the raw exchanges saved with recordings, handles drift properly.
//!
//! Sign convention (Pupil Labs `time_echo.py`): `offset = phone - this computer`, so
//! `phone_time = this_computer_time + offset`.
use crate::recording::clock::ClockSample;
use serde::Serialize;
use std::collections::{HashMap, VecDeque};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

/// How far back exchanges count. A burst runs every 10 s while a phone is connected, so this holds six bursts.
pub const WINDOW: Duration = Duration::from_secs(60);
/// A correction larger than this is refused as a broken measurement, not applied.
const MAX_PLAUSIBLE_OFFSET_MS: f64 = 24. * 3600. * 1000.;

#[derive(Debug, Clone, Copy)]
struct Exchange {
    at: Instant,
    offset_ms: f64,
    round_trip_ms: f64,
}

/// The current estimate for one phone.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Estimate {
    /// Phone clock minus this computer's, in milliseconds.
    pub offset_ms: f64,
    /// The round trip of the exchange the estimate comes from; the error is at most half of it.
    pub round_trip_ms: f64,
    /// How long ago that exchange was made.
    pub age_ms: f64,
    /// Exchanges in the window.
    pub exchanges: usize,
}

/// What happened to one event timestamp, reported with the event so it can be audited.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Correction {
    pub applied: bool,
    /// The timestamp as HyperStudy sent it (this computer's clock), Unix nanoseconds.
    pub original_ns: i64,
    /// The timestamp sent to the phone, Unix nanoseconds.
    pub sent_ns: i64,
    pub estimate: Option<Estimate>,
    /// Why the timestamp went uncorrected, when it did.
    pub reason: Option<String>,
}

#[derive(Default)]
pub struct NeonClocks {
    phones: Mutex<HashMap<String, VecDeque<Exchange>>>,
}

static GLOBAL: LazyLock<NeonClocks> = LazyLock::new(NeonClocks::default);

/// The process-wide estimates: the background sampler and every Neon device share them.
pub fn global() -> &'static NeonClocks {
    &GLOBAL
}

impl NeonClocks {
    /// Add Time Echo exchanges for a phone (identified by its hardware id). Failed or incomplete exchanges and ones
    /// with an impossible (negative) round trip are skipped.
    pub fn record(&self, phone_id: &str, samples: &[ClockSample]) {
        self.record_at(phone_id, samples, Instant::now());
    }

    fn record_at(&self, phone_id: &str, samples: &[ClockSample], now: Instant) {
        let mut phones = self.phones.lock().unwrap();
        let window = phones.entry(phone_id.to_string()).or_default();
        for s in samples {
            let (Some(t1), Some(t2), Some(t3), Some(t4)) = (s.t1, s.t2, s.t3, s.t4) else {
                continue;
            };
            if s.error.is_some() {
                continue;
            }
            let round_trip_ms = ((t4 - t1) - (t3 - t2)) * 1000.;
            let offset_ms = ((t2 - t1) + (t3 - t4)) / 2. * 1000.;
            if round_trip_ms < 0. || !round_trip_ms.is_finite() || !offset_ms.is_finite() {
                continue;
            }
            window.push_back(Exchange {
                at: now,
                offset_ms,
                round_trip_ms,
            });
        }
        while window
            .front()
            .is_some_and(|e| now.duration_since(e.at) > WINDOW)
        {
            window.pop_front();
        }
    }

    /// The phone's offset from the fastest exchange in the last `WINDOW`, or None without one.
    pub fn estimate(&self, phone_id: &str) -> Option<Estimate> {
        self.estimate_at(phone_id, Instant::now())
    }

    fn estimate_at(&self, phone_id: &str, now: Instant) -> Option<Estimate> {
        let phones = self.phones.lock().unwrap();
        let recent: Vec<&Exchange> = phones
            .get(phone_id)?
            .iter()
            .filter(|e| now.duration_since(e.at) <= WINDOW)
            .collect();
        let best = recent
            .iter()
            .min_by(|a, b| a.round_trip_ms.total_cmp(&b.round_trip_ms))?;
        Some(Estimate {
            offset_ms: best.offset_ms,
            round_trip_ms: best.round_trip_ms,
            age_ms: now.duration_since(best.at).as_secs_f64() * 1000.,
            exchanges: recent.len(),
        })
    }

    /// Map an event timestamp from this computer's clock to the phone's. Without a recent estimate, or with an
    /// implausible one, the timestamp goes out unchanged and the correction says why.
    pub fn correct(&self, phone_id: Option<&str>, timestamp_ns: i64) -> Correction {
        self.correct_with(
            phone_id.and_then(|id| self.estimate(id)),
            phone_id.is_some(),
            timestamp_ns,
        )
    }

    fn correct_with(
        &self,
        estimate: Option<Estimate>,
        known_phone: bool,
        timestamp_ns: i64,
    ) -> Correction {
        let unchanged = |reason: &str, estimate| Correction {
            applied: false,
            original_ns: timestamp_ns,
            sent_ns: timestamp_ns,
            estimate,
            reason: Some(reason.to_string()),
        };
        if !known_phone {
            return unchanged("The phone's hardware id is not known yet", None);
        }
        let Some(estimate) = estimate else {
            return unchanged(
                "No Time Echo exchange with this phone in the last minute",
                None,
            );
        };
        if estimate.offset_ms.abs() > MAX_PLAUSIBLE_OFFSET_MS {
            return unchanged("The measured offset is implausibly large", Some(estimate));
        }
        let shift_ns = (estimate.offset_ms * 1e6).round() as i64;
        match timestamp_ns.checked_add(shift_ns) {
            Some(sent_ns) => Correction {
                applied: true,
                original_ns: timestamp_ns,
                sent_ns,
                estimate: Some(estimate),
                reason: None,
            },
            None => unchanged("The corrected timestamp is out of range", Some(estimate)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One exchange in seconds: this computer sends at `t`, the phone (`offset_ms` ahead) answers halfway through a
    /// `round_trip_ms` round trip.
    fn exchange(t: f64, offset_ms: f64, round_trip_ms: f64) -> ClockSample {
        let phone = t + round_trip_ms / 2000. + offset_ms / 1000.;
        ClockSample {
            link: crate::recording::clock::NEON_TIME_ECHO.into(),
            peer: Some("phone".into()),
            local_clock: "unix".into(),
            t1: Some(t),
            t2: Some(phone),
            t3: Some(phone),
            t4: Some(t + round_trip_ms / 1000.),
            error: None,
        }
    }

    // Unix seconds near 1.8e9 carry about 0.2 µs of float error, so offsets agree to a microsecond, not exactly.
    const US: f64 = 1e-3;

    #[test]
    fn the_estimate_is_the_fastest_exchange_in_the_window() {
        let clocks = NeonClocks::default();
        let now = Instant::now();
        let t = 1_794_959_000.;
        // A slow exchange skewed by asymmetric delay, and a fast one near the truth (+512 ms).
        let mut slow = exchange(t, 512., 80.);
        slow.t2 = slow.t2.map(|v| v + 0.030);
        slow.t3 = slow.t3.map(|v| v + 0.030);
        clocks.record_at("phone-a", &[slow, exchange(t + 1., 512.4, 8.)], now);
        let estimate = clocks.estimate_at("phone-a", now).unwrap();
        assert!((estimate.offset_ms - 512.4).abs() < US, "{estimate:?}");
        assert!((estimate.round_trip_ms - 8.).abs() < US);
        assert_eq!(estimate.exchanges, 2);
    }

    #[test]
    fn exchanges_older_than_the_window_stop_counting() {
        let clocks = NeonClocks::default();
        let start = Instant::now();
        clocks.record_at("phone-a", &[exchange(1_794_959_000., 512., 5.)], start);
        let later = start + WINDOW + Duration::from_secs(1);
        assert!(clocks.estimate_at("phone-a", later).is_none());
        clocks.record_at("phone-a", &[exchange(1_794_959_061., 530., 9.)], later);
        let estimate = clocks.estimate_at("phone-a", later).unwrap();
        assert!((estimate.offset_ms - 530.).abs() < US);
        assert_eq!(estimate.exchanges, 1);
    }

    #[test]
    fn failed_and_impossible_exchanges_are_ignored() {
        let clocks = NeonClocks::default();
        let mut failed = exchange(1_794_959_000., 512., 5.);
        failed.error = Some("Time Echo timed out".into());
        let mut backwards = exchange(1_794_959_000., 512., 5.);
        backwards.t4 = Some(1_794_958_999.); // the clock stepped back mid-exchange
        clocks.record("phone-a", &[failed, backwards]);
        assert!(clocks.estimate("phone-a").is_none());
    }

    #[test]
    fn a_marker_moves_onto_the_phone_clock() {
        let clocks = NeonClocks::default();
        clocks.record("phone-a", &[exchange(1_794_959_000., 512., 8.)]);
        let sent = 1_794_959_010_000_000_000_i64;
        let c = clocks.correct(Some("phone-a"), sent);
        assert!(c.applied);
        assert_eq!(c.original_ns, sent);
        // The phone runs 512 ms ahead, so the same instant reads 512 ms later on it.
        assert!((c.sent_ns - sent - 512_000_000).abs() < 1_000, "{c:?}");
        assert!(c.reason.is_none());
    }

    #[test]
    fn without_a_recent_measurement_the_marker_goes_out_unchanged_and_says_why() {
        let clocks = NeonClocks::default();
        let sent = 1_794_959_010_000_000_000_i64;
        let c = clocks.correct(Some("phone-never-measured"), sent);
        assert!(!c.applied);
        assert_eq!(c.sent_ns, sent);
        assert!(c.reason.unwrap().contains("Time Echo"));
        let c = clocks.correct(None, sent);
        assert!(!c.applied);
        assert!(c.reason.unwrap().contains("hardware id"));
    }

    #[test]
    fn an_implausible_offset_is_refused() {
        let clocks = NeonClocks::default();
        clocks.record(
            "phone-a",
            &[exchange(1_794_959_000., 3. * 24. * 3600. * 1000., 8.)],
        );
        let c = clocks.correct(Some("phone-a"), 1_794_959_010_000_000_000);
        assert!(!c.applied);
        assert!(c.estimate.is_some());
    }
}

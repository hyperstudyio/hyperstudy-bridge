//! Raw clock samples saved with a study recording, so the offsets between the recording computer and every other
//! clock involved can be fitted (and refitted) after the session.
//!
//! Every link is stored in one shape, an NTP-style exchange of four times in seconds:
//!
//! ```text
//! t1  local clock when the request left        t2  remote clock when it arrived
//! t4  local clock when the reply arrived        t3  remote clock when the reply left
//! offset (remote - local) = ((t2 - t1) + (t3 - t4)) / 2      round trip = (t4 - t1) - (t3 - t2)
//! ```
//!
//! A peer that reports a single time (Neon's time echo) has `t2 == t3`. Nothing here is corrected or filtered:
//! slow and failed exchanges are kept, because choosing what to trust is the fit's job, not the recorder's.
//!
//! Links, by `link`:
//! - `host_wall_lsl`: this computer's wall clock (remote, Unix seconds) read between two reads of its LSL clock
//!   (local). Maps the LSL timeline of the recording to wall time; the FRENZ helper and the browser stamp in wall
//!   time.
//! - `neon_time_echo`: a Neon Companion phone's clock (remote, Unix seconds) against this computer's wall clock
//!   (local, Unix seconds), from the phone's Time Echo service. `peer` is the phone's hardware id.
//! - `hyperstudy_server`: the HyperStudy server's clock (remote) against the browser's clock (local, Unix seconds),
//!   from the experiment's own clock-sync pings, forwarded by the browser. `peer` names the server.
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Links the recorder accepts. The browser may only add `hyperstudy_server` samples.
pub const HOST_WALL_LSL: &str = "host_wall_lsl";
pub const NEON_TIME_ECHO: &str = "neon_time_echo";
pub const HYPERSTUDY_SERVER: &str = "hyperstudy_server";

/// The most samples one `record_clock_samples` command may carry.
pub const MAX_SAMPLES_PER_BATCH: usize = 256;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClockSample {
    pub link: String,
    pub peer: Option<String>,
    /// The clock `t1` and `t4` are read from: `lsl` or `unix`.
    pub local_clock: String,
    pub t1: Option<f64>,
    pub t2: Option<f64>,
    pub t3: Option<f64>,
    pub t4: Option<f64>,
    /// Set when the exchange failed; the times it did collect are kept.
    pub error: Option<String>,
}

/// One clock-sync exchange as the browser reports it, in its own units (milliseconds, the browser's clock for
/// t1/t4 and the server's for t2/t3).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrowserClockSample {
    pub t1_ms: f64,
    pub t2_ms: f64,
    pub t3_ms: f64,
    pub t4_ms: f64,
}

/// Validate and convert the browser's server-clock samples. `peer` names the server (its host), up to 256 bytes.
pub fn from_browser(peer: &str, samples: &[BrowserClockSample]) -> Result<Vec<ClockSample>> {
    ensure!(
        !samples.is_empty() && samples.len() <= MAX_SAMPLES_PER_BATCH,
        "Send 1–{MAX_SAMPLES_PER_BATCH} clock samples at a time"
    );
    ensure!(
        !peer.trim().is_empty() && peer.len() <= 256 && !peer.contains('\0'),
        "A server name is required"
    );
    samples
        .iter()
        .map(|s| {
            for value in [s.t1_ms, s.t2_ms, s.t3_ms, s.t4_ms] {
                // Unix milliseconds between 2001 and 2286: rejects seconds, zeros and garbage, nothing plausible.
                ensure!(
                    value.is_finite() && (1e12..1e13).contains(&value),
                    "Clock samples must be Unix times in milliseconds"
                );
            }
            Ok(ClockSample {
                link: HYPERSTUDY_SERVER.into(),
                peer: Some(peer.into()),
                local_clock: "unix".into(),
                t1: Some(s.t1_ms / 1000.),
                t2: Some(s.t2_ms / 1000.),
                t3: Some(s.t3_ms / 1000.),
                t4: Some(s.t4_ms / 1000.),
                error: None,
            })
        })
        .collect()
}

pub fn unix_seconds() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.)
}

/// This computer's wall clock bracketed by two reads of its LSL clock.
pub fn host_wall_lsl() -> ClockSample {
    let before = lsl::local_clock();
    let wall = unix_seconds();
    let after = lsl::local_clock();
    ClockSample {
        link: HOST_WALL_LSL.into(),
        peer: None,
        local_clock: "lsl".into(),
        t1: Some(before),
        t2: Some(wall),
        t3: Some(wall),
        t4: Some(after),
        error: None,
    }
}

/// How long one Time Echo exchange may take before the burst is abandoned.
const ECHO_TIMEOUT: Duration = Duration::from_secs(1);

/// A burst of `count` Time Echo exchanges with a Neon Companion phone over one connection.
///
/// The protocol (Pupil Labs realtime API, `time_echo.py`): send this computer's Unix milliseconds as a big-endian
/// u64; the phone answers with that value echoed and its own Unix milliseconds, two big-endian u64s. A failure
/// ends the burst with one sample carrying the error, after the exchanges that succeeded.
pub async fn neon_time_echo(
    host: &str,
    port: u16,
    phone_id: &str,
    count: usize,
) -> Vec<ClockSample> {
    let failed = |error: String| ClockSample {
        link: NEON_TIME_ECHO.into(),
        peer: Some(phone_id.into()),
        local_clock: "unix".into(),
        t1: None,
        t2: None,
        t3: None,
        t4: None,
        error: Some(error),
    };
    let mut samples = Vec::with_capacity(count);
    let mut stream = match tokio::time::timeout(
        ECHO_TIMEOUT,
        tokio::net::TcpStream::connect((host, port)),
    )
    .await
    {
        Ok(Ok(stream)) => stream,
        Ok(Err(error)) => return vec![failed(format!("Time Echo connection failed: {error}"))],
        Err(_) => return vec![failed("Time Echo connection timed out".into())],
    };
    let _ = stream.set_nodelay(true);
    for _ in 0..count {
        match tokio::time::timeout(ECHO_TIMEOUT, echo_once(&mut stream)).await {
            Ok(Ok((sent_ms, phone_ms, received))) => samples.push(ClockSample {
                link: NEON_TIME_ECHO.into(),
                peer: Some(phone_id.into()),
                local_clock: "unix".into(),
                t1: Some(sent_ms as f64 / 1000.),
                t2: Some(phone_ms as f64 / 1000.),
                t3: Some(phone_ms as f64 / 1000.),
                t4: Some(received),
                error: None,
            }),
            Ok(Err(error)) => {
                samples.push(failed(format!("Time Echo failed: {error:#}")));
                break;
            }
            Err(_) => {
                samples.push(failed("Time Echo timed out".into()));
                break;
            }
        }
    }
    samples
}

async fn echo_once(stream: &mut tokio::net::TcpStream) -> Result<(u64, u64, f64)> {
    // The phone echoes whole milliseconds, so t1 is the value sent, not a finer reading of the same instant.
    let sent_ms = (unix_seconds() * 1000.) as u64;
    stream.write_all(&sent_ms.to_be_bytes()).await?;
    let mut reply = [0u8; 16];
    stream.read_exact(&mut reply).await?;
    let received = unix_seconds();
    let echoed = u64::from_be_bytes(reply[..8].try_into()?);
    let phone_ms = u64::from_be_bytes(reply[8..].try_into()?);
    ensure!(
        echoed == sent_ms,
        "The phone echoed {echoed}, not {sent_ms}"
    );
    Ok((sent_ms, phone_ms, received))
}

/// The Neon phones connected to this Bridge, as `(host, time echo port, hardware id)`.
pub async fn connected_neon_phones(state: &crate::bridge::AppState) -> Vec<(String, u16, String)> {
    let devices: Vec<_> = state.devices.read().await.values().cloned().collect();
    let mut phones = Vec::new();
    for device in devices {
        let info = device.read().await.get_info();
        if info.device_type != crate::devices::DeviceType::Pupil
            || info.status != crate::devices::DeviceStatus::Connected
        {
            continue;
        }
        let meta = &info.metadata;
        let host = meta["phone_ip"].as_str().or(meta["device_ip"].as_str());
        let port = meta["time_echo_port"]
            .as_u64()
            .and_then(|p| u16::try_from(p).ok());
        let id = meta["device_id"].as_str();
        if let (Some(host), Some(port), Some(id)) = (host, port, id) {
            if port != 0 && !host.is_empty() {
                phones.push((host.to_string(), port, id.to_string()));
            }
        }
    }
    phones
}

/// Exchanges per burst, and how often a burst runs while a study recording is active.
const ECHO_BURST: usize = 8;
const ECHO_INTERVAL: Duration = Duration::from_secs(10);

/// While a study recording runs, measure every connected Neon phone's clock against this computer's and save the
/// raw exchanges with the recording. Runs for the life of the app; does nothing between recordings.
pub async fn sample_neon_phones(state: std::sync::Arc<crate::bridge::AppState>) {
    let mut tick = tokio::time::interval(ECHO_INTERVAL);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        if state.recording.status().state != "recording" {
            continue;
        }
        for (host, port, id) in connected_neon_phones(&state).await {
            let samples = neon_time_echo(&host, port, &id, ECHO_BURST).await;
            let manager = state.recording.clone();
            // The recorder's channel is synchronous; a recording that just ended refuses the samples, which is fine.
            let result =
                tokio::task::spawn_blocking(move || manager.record_clock_samples(samples)).await;
            if let Ok(Err(error)) = result {
                tracing::debug!("Neon clock samples not saved: {error:#}");
            }
        }
    }
}

/// Insert samples into a recording's `clock_samples` table. `observed_lsl` is the recorder's LSL clock when the
/// row was written, which places browser and phone samples on the recording's own timeline.
pub(super) fn insert(db: &rusqlite::Connection, samples: &[ClockSample]) -> Result<()> {
    let observed = lsl::local_clock();
    let mut statement = db
        .prepare_cached("INSERT INTO clock_samples(link,peer,local_clock,t1,t2,t3,t4,observed_lsl,error) VALUES(?,?,?,?,?,?,?,?,?)")
        .context("Recording has no clock_samples table")?;
    for s in samples {
        statement.execute(rusqlite::params![
            s.link,
            s.peer,
            s.local_clock,
            s.t1,
            s.t2,
            s.t3,
            s.t4,
            observed,
            s.error
        ])?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn browser(t1: f64, t2: f64, t3: f64, t4: f64) -> BrowserClockSample {
        BrowserClockSample {
            t1_ms: t1,
            t2_ms: t2,
            t3_ms: t3,
            t4_ms: t4,
        }
    }

    #[test]
    fn browser_samples_become_seconds_on_the_server_link() {
        let t = 1_794_959_000_000.;
        let samples = from_browser(
            "api.hyperstudy.io",
            &[browser(t, t + 20., t + 21., t + 40.)],
        )
        .unwrap();
        assert_eq!(samples.len(), 1);
        let s = &samples[0];
        assert_eq!(s.link, HYPERSTUDY_SERVER);
        assert_eq!(s.peer.as_deref(), Some("api.hyperstudy.io"));
        assert_eq!(s.local_clock, "unix");
        assert_eq!(s.t1, Some(t / 1000.));
        assert_eq!(s.t4, Some((t + 40.) / 1000.));
    }

    #[test]
    fn browser_samples_reject_seconds_garbage_and_oversized_batches() {
        let t = 1_794_959_000_000.;
        assert!(from_browser("server", &[browser(t / 1000., t, t, t)]).is_err());
        assert!(from_browser("server", &[browser(f64::NAN, t, t, t)]).is_err());
        assert!(from_browser("server", &[]).is_err());
        assert!(from_browser("", &[browser(t, t, t, t)]).is_err());
        let many: Vec<_> = (0..=MAX_SAMPLES_PER_BATCH)
            .map(|_| browser(t, t, t, t))
            .collect();
        assert!(from_browser("server", &many).is_err());
    }

    #[test]
    fn host_wall_lsl_brackets_one_wall_read() {
        let s = host_wall_lsl();
        assert_eq!(s.link, HOST_WALL_LSL);
        assert!(s.t1.unwrap() <= s.t4.unwrap());
        assert_eq!(s.t2, s.t3);
        assert!(s.t2.unwrap() > 1.7e9);
    }

    /// A stand-in for the phone's Time Echo service: echoes the request and answers with a clock `skew_ms` ahead.
    async fn fake_phone(skew_ms: i64, replies: usize) -> u16 {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            for _ in 0..replies {
                let mut request = [0u8; 8];
                if socket.read_exact(&mut request).await.is_err() {
                    return;
                }
                let phone = (unix_seconds() * 1000.) as i64 + skew_ms;
                let mut reply = request.to_vec();
                reply.extend_from_slice(&(phone as u64).to_be_bytes());
                socket.write_all(&reply).await.unwrap();
            }
        });
        port
    }

    #[tokio::test]
    async fn time_echo_records_the_phone_clock_with_the_pupil_labs_sign() {
        let port = fake_phone(512, 8).await;
        let samples = neon_time_echo("127.0.0.1", port, "phone-a", 8).await;
        assert_eq!(samples.len(), 8);
        for s in &samples {
            assert_eq!(s.link, NEON_TIME_ECHO);
            assert_eq!(s.peer.as_deref(), Some("phone-a"));
            assert!(s.error.is_none());
            let offset = ((s.t2.unwrap() - s.t1.unwrap()) + (s.t3.unwrap() - s.t4.unwrap())) / 2.;
            // The phone runs 512 ms ahead; whole-millisecond rounding and loopback leave a few ms either way.
            assert!((offset - 0.512).abs() < 0.005, "offset {offset}");
        }
    }

    #[tokio::test]
    async fn time_echo_keeps_the_exchanges_before_a_failure() {
        let port = fake_phone(0, 3).await;
        let samples = neon_time_echo("127.0.0.1", port, "phone-a", 8).await;
        assert_eq!(samples.len(), 4);
        assert!(samples[..3].iter().all(|s| s.error.is_none()));
        assert!(samples[3].error.is_some());
    }

    #[tokio::test]
    async fn time_echo_reports_an_unreachable_phone() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let samples = neon_time_echo("127.0.0.1", port, "phone-a", 8).await;
        assert_eq!(samples.len(), 1);
        assert!(samples[0].error.as_deref().unwrap().contains("connection"));
    }

    /// Against a real Neon Companion phone on this network. Run with
    /// `NEON_HOST=<phone ip> cargo test --lib real_neon_time_echo -- --ignored --nocapture`.
    #[tokio::test]
    #[ignore = "needs a Neon Companion phone on the network (NEON_HOST)"]
    async fn real_neon_time_echo() {
        let host = std::env::var("NEON_HOST").expect("set NEON_HOST to the phone's IP");
        let port = std::env::var("NEON_TIME_ECHO_PORT")
            .ok()
            .and_then(|p| p.parse().ok())
            .unwrap_or(12321);
        // The recorder's own path: connect the way Bridge does, then read where Time Echo listens.
        use crate::devices::Device;
        let mut device = crate::devices::pupil::PupilDevice::new(format!("{host}:8080"));
        device.connect().await.expect("phone connects");
        let meta = device.get_info().metadata;
        assert_eq!(meta["time_echo_port"].as_u64(), Some(u64::from(port)));
        println!(
            "phone_ip {} device_id {}",
            meta["phone_ip"], meta["device_id"]
        );
        let samples = neon_time_echo(&host, port, "real-phone", 40).await;
        let mut exchanges: Vec<(f64, f64)> = samples
            .iter()
            .filter(|s| s.error.is_none())
            .map(|s| {
                let (t1, t2, t3, t4) = (s.t1.unwrap(), s.t2.unwrap(), s.t3.unwrap(), s.t4.unwrap());
                (
                    ((t4 - t1) - (t3 - t2)) * 1000.,
                    ((t2 - t1) + (t3 - t4)) / 2. * 1000.,
                )
            })
            .collect();
        assert_eq!(exchanges.len(), 40, "{:?}", samples.last());
        exchanges.sort_by(|a, b| a.0.total_cmp(&b.0));
        let (fastest_rtt, fastest_offset) = exchanges[0];
        let median = |mut v: Vec<f64>| {
            v.sort_by(f64::total_cmp);
            v[v.len() / 2]
        };
        println!(
            "phone - this computer: fastest exchange {fastest_offset:+.1} ms (round trip {fastest_rtt:.1} ms); \
             median of all {:+.1} ms; median round trip {:.1} ms",
            median(exchanges.iter().map(|e| e.1).collect()),
            median(exchanges.iter().map(|e| e.0).collect())
        );
    }
}

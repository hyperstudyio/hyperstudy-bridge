//! Real-time gaze data streaming from the EyeLink tracker.
//!
//! Spawns a dedicated blocking thread that polls `eyelink_newest_float_sample()`
//! and forwards gaze data to an mpsc channel. The WebSocket handler subscribes
//! to this channel and forwards data to the frontend.
//!
//! The EyeLink provides binocular gaze data in pixel coordinates at the
//! configured sample rate (typically 250–2000 Hz).

use super::ffi;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::debug;

/// A single gaze sample from the EyeLink, ready for the frontend.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GazeSample {
    /// EyeLink tracker timestamp (ms)
    pub timestamp: u32,
    /// Left eye gaze X (pixels)
    pub left_gaze_x: f32,
    /// Left eye gaze Y (pixels)
    pub left_gaze_y: f32,
    /// Right eye gaze X (pixels)
    pub right_gaze_x: f32,
    /// Right eye gaze Y (pixels)
    pub right_gaze_y: f32,
    /// Left pupil area
    pub left_pupil: f32,
    /// Right pupil area
    pub right_pupil: f32,
    /// Angular resolution: pixels per degree X
    pub ppd_x: f32,
    /// Angular resolution: pixels per degree Y
    pub ppd_y: f32,
    /// Tracker status flags
    pub status: u16,
    /// Display width used for normalization
    pub display_width: u32,
    /// Display height used for normalization
    pub display_height: u32,
}

/// Spawn the gaze polling task.
///
/// Uses a single `spawn_blocking` thread with an internal polling loop
/// (much more efficient than spawning a new blocking task per sample).
/// The thread polls at 2x the sample rate and sends samples through
/// the mpsc channel. It exits when the shutdown signal is set or the
/// channel is closed.
pub fn spawn_gaze_polling(
    gaze_tx: mpsc::Sender<GazeSample>,
    mut shutdown_rx: mpsc::Receiver<()>,
    sample_rate: u32,
    display_width: u32,
    display_height: u32,
) -> tokio::task::JoinHandle<()> {
    // Poll interval: slightly faster than sample rate to avoid missing samples
    let poll_interval = Duration::from_micros(if sample_rate > 0 {
        (1_000_000 / (sample_rate as u64 * 2)).max(100) // Poll at 2x sample rate, min 100µs
    } else {
        2000 // Default 500Hz equivalent
    });

    // Use an atomic flag for shutdown signaling across threads
    let shutdown = Arc::new(AtomicBool::new(false));
    let shutdown_clone = shutdown.clone();

    // Spawn a listener for the shutdown channel
    let shutdown_listener = tokio::spawn(async move {
        let _ = shutdown_rx.recv().await;
        shutdown_clone.store(true, Ordering::Relaxed);
    });

    let task = tokio::task::spawn_blocking(move || {
        debug!(
            device = "eyelink",
            "Gaze polling thread started (interval: {:?})", poll_interval
        );

        while !shutdown.load(Ordering::Relaxed) {
            if let Some(sample) = ffi::get_newest_sample() {
                let gaze = GazeSample {
                    timestamp: sample.time,
                    left_gaze_x: sample.gx[ffi::LEFT_EYE],
                    left_gaze_y: sample.gy[ffi::LEFT_EYE],
                    right_gaze_x: sample.gx[ffi::RIGHT_EYE],
                    right_gaze_y: sample.gy[ffi::RIGHT_EYE],
                    left_pupil: sample.pa[ffi::LEFT_EYE],
                    right_pupil: sample.pa[ffi::RIGHT_EYE],
                    ppd_x: sample.rx,
                    ppd_y: sample.ry,
                    status: sample.flags,
                    display_width,
                    display_height,
                };

                // Use try_send to avoid blocking the polling thread
                if gaze_tx.try_send(gaze).is_err() {
                    // Channel full or closed — if closed, exit
                    if gaze_tx.is_closed() {
                        debug!(device = "eyelink", "Gaze channel closed, stopping poll");
                        break;
                    }
                    // Channel full — drop this sample (acceptable at high rates)
                }
            }

            std::thread::sleep(poll_interval);
        }

        debug!(device = "eyelink", "Gaze polling thread exiting");
    });

    // Wrap both tasks into a single JoinHandle
    tokio::spawn(async move {
        let _ = task.await;
        shutdown_listener.abort();
    })
}

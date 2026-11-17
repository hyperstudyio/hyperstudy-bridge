//! Per-device FIFO command queues for one WebSocket connection.
//!
//! Every device command a client sends is parked on the queue of the resource
//! it mutates and executed by that queue's worker task, in arrival order.
//! Devices therefore never wait on each other (a Kernel stuck in a reconnect
//! cannot delay Neon markers), while for a single device a `disconnect` can
//! never overtake the `send_event` that preceded it.
//!
//! Extracted from the WebSocket handler so the guarantees that matter for
//! marker integrity — per-device ordering, cross-device independence, dropping
//! still-queued commands once the connection is gone, and bounded buffering —
//! are unit-tested rather than asserted in comments.

use crate::bridge::message::CommandAction;
use futures_util::FutureExt;
use serde_json::Value;
use std::collections::HashMap;
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{debug, error, warn};

/// Pending commands allowed per device before new ones are rejected instead of
/// buffered without bound (and replayed as stale markers minutes later).
pub const DEVICE_QUEUE_CAPACITY: usize = 256;

/// Distinct queues (device keys) one connection may create. Keys come from the
/// client's `device` string; without a cap a misbehaving client could spawn a
/// worker task per arbitrary string.
pub const MAX_DEVICE_QUEUES: usize = 32;

/// A device command parked on its device's FIFO queue.
#[derive(Debug)]
pub struct QueuedCommand {
    pub device: String,
    pub action: CommandAction,
    pub payload: Option<Value>,
    pub id: Option<String>,
}

/// Executes one command. Boxed so the queue does not depend on the WebSocket
/// handler's concrete future type (and so tests can supply their own).
pub type CommandHandler =
    Arc<dyn Fn(QueuedCommand) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;

/// Outcome of [`DeviceQueues::dispatch`].
#[derive(Debug)]
pub enum Dispatch {
    /// Accepted; will run after everything already queued for that device.
    Queued,
    /// That device's queue is full — the command is handed back so the caller
    /// can answer the client with its request id.
    Full(QueuedCommand),
    /// The connection already has [`MAX_DEVICE_QUEUES`] distinct devices.
    TooManyQueues(QueuedCommand),
    /// The worker for that device has exited (should not happen while the
    /// connection is alive).
    WorkerGone(QueuedCommand),
}

fn warn_if_replacing(existed: bool, key: &str) {
    if existed {
        warn!(
            "Device queue worker for {} was gone; starting a new one",
            key
        );
    }
}

pub struct DeviceQueues {
    queues: HashMap<String, mpsc::Sender<QueuedCommand>>,
    closed: Arc<AtomicBool>,
    handler: CommandHandler,
    capacity: usize,
    max_queues: usize,
}

impl DeviceQueues {
    /// `closed` is the owning connection's flag: once it is set, workers drop
    /// every command still queued (the one in flight finishes) so a client
    /// that reconnects and retries cannot double-send markers.
    pub fn new(closed: Arc<AtomicBool>, handler: CommandHandler) -> Self {
        Self::with_limits(closed, handler, DEVICE_QUEUE_CAPACITY, MAX_DEVICE_QUEUES)
    }

    pub fn with_limits(
        closed: Arc<AtomicBool>,
        handler: CommandHandler,
        capacity: usize,
        max_queues: usize,
    ) -> Self {
        Self {
            queues: HashMap::new(),
            closed,
            handler,
            capacity: capacity.max(1),
            max_queues: max_queues.max(1),
        }
    }

    /// The queue a command belongs to: the resource the action mutates, which
    /// is not always the `device` string the client used. `connect_neon_rest`
    /// arrives as `neon_lsl` but replaces the `pupil` device, so it must be
    /// ordered with pupil markers; phone discovery touches no device at all and
    /// must not hold pupil markers for its browse window.
    pub fn key_for(device: &str, action: &CommandAction) -> String {
        match action {
            CommandAction::ConnectNeonRest => "pupil".to_string(),
            CommandAction::DiscoverNeonPhones => "neon_discovery".to_string(),
            _ => device.to_lowercase(),
        }
    }

    pub fn dispatch(&mut self, cmd: QueuedCommand) -> Dispatch {
        let key = Self::key_for(&cmd.device, &cmd.action);
        if !self.queues.contains_key(&key) && self.queues.len() >= self.max_queues {
            return Dispatch::TooManyQueues(cmd);
        }
        // A worker whose task died (it should not — panics are caught below —
        // but a dead sender must never wedge a device for the rest of the
        // connection) is replaced rather than dispatched into.
        let sender = match self.queues.get(&key) {
            Some(existing) if !existing.is_closed() => existing.clone(),
            _ => {
                warn_if_replacing(self.queues.contains_key(&key), &key);
                let fresh = self.spawn_worker(key.clone());
                self.queues.insert(key.clone(), fresh.clone());
                fresh
            }
        };
        match sender.try_send(cmd) {
            Ok(()) => Dispatch::Queued,
            Err(mpsc::error::TrySendError::Full(cmd)) => {
                warn!(
                    "Command queue for {} is full ({} pending); rejecting {:?}",
                    cmd.device, self.capacity, cmd.action
                );
                Dispatch::Full(cmd)
            }
            Err(mpsc::error::TrySendError::Closed(cmd)) => Dispatch::WorkerGone(cmd),
        }
    }

    fn spawn_worker(&self, key: String) -> mpsc::Sender<QueuedCommand> {
        let (qtx, mut qrx) = mpsc::channel::<QueuedCommand>(self.capacity);
        let closed = self.closed.clone();
        let handler = self.handler.clone();
        tokio::spawn(async move {
            while let Some(cmd) = qrx.recv().await {
                if closed.load(Ordering::Relaxed) {
                    debug!(
                        "Connection closed; dropping queued {:?} for {}",
                        cmd.action, cmd.device
                    );
                    continue;
                }
                let (device, action, id) = (cmd.device.clone(), cmd.action.clone(), cmd.id.clone());
                // A panicking device handler must not take the worker down with
                // it: that would silently drop every later command for this
                // device (marker loss with no error to the client). The
                // panicking command itself gets no response; the client's own
                // timeout reports it.
                if AssertUnwindSafe((handler)(cmd))
                    .catch_unwind()
                    .await
                    .is_err()
                {
                    error!(
                        "Device command handler panicked: device={} action={:?} id={:?}; worker continues",
                        device, action, id
                    );
                }
            }
            debug!("Device queue worker for {} finished", key);
        });
        qtx
    }

    /// Number of distinct device queues created so far.
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.queues.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::sync::Notify;
    use tokio::time::timeout;

    fn cmd(device: &str, action: CommandAction, id: &str) -> QueuedCommand {
        QueuedCommand {
            device: device.to_string(),
            action,
            payload: None,
            id: Some(id.to_string()),
        }
    }

    /// Handler that reports each completed command id on a channel (so tests
    /// wait for completions instead of sleeping) and can block commands for one
    /// device behind a gate to control interleaving.
    fn reporting_handler(
        done: mpsc::UnboundedSender<String>,
        gate: Option<(String, Arc<Notify>)>,
    ) -> CommandHandler {
        Arc::new(move |c: QueuedCommand| {
            let done = done.clone();
            let gate = gate.clone();
            Box::pin(async move {
                if let Some((device, notify)) = gate {
                    if c.device == device {
                        notify.notified().await;
                    }
                }
                let _ = done.send(c.id.unwrap_or_default());
            })
        })
    }

    async fn next_done(rx: &mut mpsc::UnboundedReceiver<String>) -> String {
        timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("command did not complete in time")
            .expect("channel closed")
    }

    /// Negative check: nothing completes within a short window.
    async fn assert_nothing_done(rx: &mut mpsc::UnboundedReceiver<String>) {
        assert!(
            timeout(Duration::from_millis(100), rx.recv())
                .await
                .is_err(),
            "a command completed that should still be blocked"
        );
    }

    #[test]
    fn key_follows_the_mutated_resource_not_the_client_string() {
        assert_eq!(
            DeviceQueues::key_for("Kernel", &CommandAction::SendEvent),
            "kernel"
        );
        assert_eq!(
            DeviceQueues::key_for("neon_lsl", &CommandAction::ConnectNeonRest),
            "pupil"
        );
        assert_eq!(
            DeviceQueues::key_for("pupil", &CommandAction::DiscoverNeonPhones),
            "neon_discovery"
        );
        assert_eq!(
            DeviceQueues::key_for("neon_lsl", &CommandAction::ConnectNeonGaze),
            "neon_lsl"
        );
    }

    #[tokio::test]
    async fn commands_for_one_device_run_in_arrival_order() {
        let (done, mut rx) = mpsc::unbounded_channel();
        let mut q = DeviceQueues::new(
            Arc::new(AtomicBool::new(false)),
            reporting_handler(done, None),
        );
        for i in 0..5 {
            assert!(matches!(
                q.dispatch(cmd("kernel", CommandAction::SendEvent, &format!("k{i}"))),
                Dispatch::Queued
            ));
        }
        // A disconnect queued after the markers must run after them.
        assert!(matches!(
            q.dispatch(cmd("kernel", CommandAction::Disconnect, "k-disc")),
            Dispatch::Queued
        ));
        let mut seen = Vec::new();
        for _ in 0..6 {
            seen.push(next_done(&mut rx).await);
        }
        assert_eq!(seen, vec!["k0", "k1", "k2", "k3", "k4", "k-disc"]);
    }

    #[tokio::test]
    async fn a_stalled_device_does_not_delay_another() {
        let (done, mut rx) = mpsc::unbounded_channel();
        let gate = Arc::new(Notify::new());
        let mut q = DeviceQueues::new(
            Arc::new(AtomicBool::new(false)),
            reporting_handler(done, Some(("kernel".to_string(), gate.clone()))),
        );
        q.dispatch(cmd("kernel", CommandAction::SendEvent, "k0"));
        q.dispatch(cmd("pupil", CommandAction::SendEvent, "p0"));
        q.dispatch(cmd("pupil", CommandAction::SendEvent, "p1"));
        // Kernel is blocked on its gate; pupil markers go through regardless.
        assert_eq!(next_done(&mut rx).await, "p0");
        assert_eq!(next_done(&mut rx).await, "p1");
        assert_nothing_done(&mut rx).await;
        gate.notify_one();
        assert_eq!(next_done(&mut rx).await, "k0");
    }

    #[tokio::test]
    async fn queued_commands_are_dropped_once_the_connection_closes() {
        let (done, mut rx) = mpsc::unbounded_channel();
        let gate = Arc::new(Notify::new());
        let closed = Arc::new(AtomicBool::new(false));
        let mut q = DeviceQueues::new(
            closed.clone(),
            reporting_handler(done, Some(("pupil".to_string(), gate.clone()))),
        );
        q.dispatch(cmd("pupil", CommandAction::SendEvent, "in-flight"));
        q.dispatch(cmd("pupil", CommandAction::SendEvent, "queued-1"));
        q.dispatch(cmd("pupil", CommandAction::SendEvent, "queued-2"));
        assert_nothing_done(&mut rx).await; // first is parked on the gate
                                            // The socket closes while the first command is in flight.
        closed.store(true, Ordering::Relaxed);
        gate.notify_one();
        assert_eq!(next_done(&mut rx).await, "in-flight");
        // The commands still queued must be dropped, never executed — a
        // reconnecting client that retries them cannot produce duplicates.
        gate.notify_one();
        gate.notify_one();
        assert_nothing_done(&mut rx).await;
    }

    #[tokio::test]
    async fn a_full_queue_rejects_the_new_command_and_keeps_order_of_accepted_ones() {
        let (done, mut rx) = mpsc::unbounded_channel();
        let gate = Arc::new(Notify::new());
        let mut q = DeviceQueues::with_limits(
            Arc::new(AtomicBool::new(false)),
            reporting_handler(done, Some(("kernel".to_string(), gate.clone()))),
            2,
            MAX_DEVICE_QUEUES,
        );
        q.dispatch(cmd("kernel", CommandAction::SendEvent, "k0"));
        assert_nothing_done(&mut rx).await; // k0 taken by the worker and parked
        assert!(matches!(
            q.dispatch(cmd("kernel", CommandAction::SendEvent, "k1")),
            Dispatch::Queued
        ));
        assert!(matches!(
            q.dispatch(cmd("kernel", CommandAction::SendEvent, "k2")),
            Dispatch::Queued
        ));
        match q.dispatch(cmd("kernel", CommandAction::SendEvent, "k3")) {
            Dispatch::Full(rejected) => assert_eq!(rejected.id.as_deref(), Some("k3")),
            other => panic!("expected Full, got {other:?}"),
        }
        // Release the worker one command at a time: accepted work runs in order.
        for expected in ["k0", "k1", "k2"] {
            gate.notify_one();
            assert_eq!(next_done(&mut rx).await, expected);
        }
        assert_nothing_done(&mut rx).await;
    }

    #[tokio::test]
    async fn a_panicking_command_does_not_wedge_the_device_queue() {
        let (done, mut rx) = mpsc::unbounded_channel::<String>();
        let handler: CommandHandler = Arc::new(move |c: QueuedCommand| {
            let done = done.clone();
            Box::pin(async move {
                let id = c.id.unwrap_or_default();
                if id == "boom" {
                    panic!("device driver bug");
                }
                let _ = done.send(id);
            })
        });
        let mut q = DeviceQueues::new(Arc::new(AtomicBool::new(false)), handler);
        q.dispatch(cmd("kernel", CommandAction::SendEvent, "k0"));
        q.dispatch(cmd("kernel", CommandAction::SendEvent, "boom"));
        q.dispatch(cmd("kernel", CommandAction::SendEvent, "k1"));
        assert_eq!(next_done(&mut rx).await, "k0");
        // The panic is contained; the next marker still goes through, in order.
        assert_eq!(next_done(&mut rx).await, "k1");
        assert!(matches!(
            q.dispatch(cmd("kernel", CommandAction::SendEvent, "k2")),
            Dispatch::Queued
        ));
        assert_eq!(next_done(&mut rx).await, "k2");
        assert_eq!(q.len(), 1);
    }

    #[tokio::test]
    async fn the_number_of_queues_is_bounded() {
        let (done, _rx) = mpsc::unbounded_channel();
        let mut q = DeviceQueues::with_limits(
            Arc::new(AtomicBool::new(false)),
            reporting_handler(done, None),
            DEVICE_QUEUE_CAPACITY,
            2,
        );
        assert!(matches!(
            q.dispatch(cmd("a", CommandAction::Send, "a0")),
            Dispatch::Queued
        ));
        assert!(matches!(
            q.dispatch(cmd("b", CommandAction::Send, "b0")),
            Dispatch::Queued
        ));
        assert!(matches!(
            q.dispatch(cmd("c", CommandAction::Send, "c0")),
            Dispatch::TooManyQueues(_)
        ));
        // Existing keys still accept.
        assert!(matches!(
            q.dispatch(cmd("a", CommandAction::Send, "a1")),
            Dispatch::Queued
        ));
        assert_eq!(q.len(), 2);
    }
}

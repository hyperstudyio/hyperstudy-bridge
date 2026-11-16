//! Custom tracing layer for capturing logs and emitting them to the Tauri frontend.
//!
//! This module provides:
//! - `LogBuffer`: A thread-safe circular buffer for storing recent log entries
//! - `TauriLogLayer`: A custom tracing Layer that captures log events and emits them to the frontend
//! - `LogEntry`: The serializable log entry structure shared with the frontend
//! - `LogPersister`: Batches logs and flushes them to the database

use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::sync::{Arc, OnceLock, RwLock};
use std::time::Duration;
use tauri::{AppHandle, Emitter};
use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::layer::Context;
use tracing_subscriber::Layer;

/// Global app handle for emitting events. Set during Tauri setup.
static APP_HANDLE: OnceLock<AppHandle> = OnceLock::new();

/// Global log buffer for storing logs before and after AppHandle is available.
static LOG_BUFFER: OnceLock<Arc<RwLock<LogBuffer>>> = OnceLock::new();

/// Global log persister for database writes.
static LOG_PERSISTER: OnceLock<Arc<LogPersister>> = OnceLock::new();

/// Global log emitter for batched frontend events.
static LOG_EMITTER: OnceLock<Arc<LogEmitter>> = OnceLock::new();

/// Default capacity for the log buffer (in-memory, for real-time display)
const DEFAULT_BUFFER_CAPACITY: usize = 500;

/// Default batch size for database writes
const DEFAULT_PERSIST_BATCH_SIZE: usize = 100;

/// Default flush interval for database writes
const DEFAULT_PERSIST_FLUSH_INTERVAL: Duration = Duration::from_secs(5);

/// Flush interval for frontend event batching (50ms for responsive UI)
const DEFAULT_EMIT_FLUSH_INTERVAL: Duration = Duration::from_millis(50);

/// Maximum batch size before immediate flush
const DEFAULT_EMIT_BATCH_SIZE: usize = 50;

/// A log entry that can be serialized and sent to the frontend.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEntry {
    pub timestamp: String,
    pub level: String,
    pub message: String,
    pub device: Option<String>,
    pub source: String,
}

impl LogEntry {
    /// Create a new log entry with the current timestamp.
    pub fn new(level: &str, message: String, device: Option<String>, source: String) -> Self {
        Self {
            timestamp: Utc::now().to_rfc3339(),
            level: level.to_string(),
            message,
            device,
            source,
        }
    }
}

/// A circular buffer for storing log entries.
#[derive(Debug)]
pub struct LogBuffer {
    entries: VecDeque<LogEntry>,
    capacity: usize,
}

impl LogBuffer {
    /// Create a new log buffer with the specified capacity.
    pub fn new(capacity: usize) -> Self {
        Self {
            entries: VecDeque::with_capacity(capacity),
            capacity,
        }
    }

    /// Add a log entry to the buffer, removing the oldest if at capacity.
    pub fn push(&mut self, entry: LogEntry) {
        if self.entries.len() >= self.capacity {
            self.entries.pop_front();
        }
        self.entries.push_back(entry);
    }

    /// Get all log entries as a vector (newest last).
    pub fn get_all(&self) -> Vec<LogEntry> {
        self.entries.iter().cloned().collect()
    }
}

impl Default for LogBuffer {
    fn default() -> Self {
        Self::new(DEFAULT_BUFFER_CAPACITY)
    }
}

/// Persists logs to the database in batches.
///
/// Uses a bounded buffer with a std::sync::Mutex for lock-free sync access.
/// When the buffer is full, oldest entries are dropped (bounded buffer behavior).
/// Entries are periodically flushed to the database.
pub struct LogPersister {
    /// Buffer for pending log entries. Uses std::sync::Mutex for sync access.
    buffer: std::sync::Mutex<Vec<LogEntry>>,
    /// Maximum buffer size before triggering flush or dropping entries.
    max_buffer_size: usize,
    /// Number of entries to trigger an immediate flush.
    batch_size: usize,
    /// Interval between periodic flushes.
    flush_interval: Duration,
    /// Track dropped entries for diagnostics.
    dropped_count: std::sync::atomic::AtomicU64,
    /// Wakes the flush task when a batch fills. `add` runs on whatever thread logged (including plain OS threads such
    /// as LSL inlets, outlets and the recorder), so it must never touch the Tokio runtime itself: release builds abort
    /// on panic, and `tokio::spawn` panics outside the runtime.
    flush_wanted: tokio::sync::Notify,
}

impl LogPersister {
    /// Create a new log persister with default settings.
    pub fn new() -> Self {
        Self {
            buffer: std::sync::Mutex::new(Vec::with_capacity(DEFAULT_PERSIST_BATCH_SIZE)),
            max_buffer_size: DEFAULT_PERSIST_BATCH_SIZE * 10, // Allow 10x batch size before dropping
            batch_size: DEFAULT_PERSIST_BATCH_SIZE,
            flush_interval: DEFAULT_PERSIST_FLUSH_INTERVAL,
            dropped_count: std::sync::atomic::AtomicU64::new(0),
            flush_wanted: tokio::sync::Notify::new(),
        }
    }

    /// Create a new log persister with custom settings.
    #[cfg(test)]
    pub fn with_config(batch_size: usize, flush_interval: Duration) -> Self {
        Self {
            buffer: std::sync::Mutex::new(Vec::with_capacity(batch_size)),
            max_buffer_size: batch_size * 10,
            batch_size,
            flush_interval,
            dropped_count: std::sync::atomic::AtomicU64::new(0),
            flush_wanted: tokio::sync::Notify::new(),
        }
    }

    /// Add a log entry to the persist buffer.
    ///
    /// This is called from a sync context (tracing layer). Uses std::sync::Mutex
    /// which can be briefly held without issue. If the buffer is full, the entry
    /// is dropped rather than blocking.
    pub fn add(&self, entry: LogEntry) {
        // Use std::sync::Mutex - brief lock is acceptable in sync context
        let should_flush = match self.buffer.lock() {
            Ok(mut buffer) => {
                // Check if buffer is at max capacity
                if buffer.len() >= self.max_buffer_size {
                    // Drop oldest entries to make room (bounded buffer)
                    let to_remove = buffer.len() - self.max_buffer_size + 1;
                    buffer.drain(0..to_remove);
                    self.dropped_count
                        .fetch_add(to_remove as u64, std::sync::atomic::Ordering::Relaxed);
                }

                buffer.push(entry);
                buffer.len() >= self.batch_size
            }
            Err(poisoned) => {
                // Mutex was poisoned (panic occurred while held)
                // Recover by taking the lock anyway
                let mut buffer = poisoned.into_inner();
                buffer.push(entry);
                buffer.len() >= self.batch_size
            }
        };

        if should_flush {
            self.flush_wanted.notify_one();
        }
    }

    /// Flush buffered entries to the database.
    async fn flush_entries(
        entries: Vec<LogEntry>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        if entries.is_empty() {
            return Ok(());
        }

        if let Some(storage) = crate::storage::get_storage() {
            let session_id = storage.current_session_id().await;
            crate::storage::logs::insert_logs_batch(
                storage.pool(),
                &entries,
                session_id.as_deref(),
            )
            .await?;
        }

        Ok(())
    }

    /// Force flush all buffered logs (called on shutdown).
    pub async fn flush(&self) {
        let entries = match self.buffer.lock() {
            Ok(mut buffer) => std::mem::take(&mut *buffer),
            Err(poisoned) => std::mem::take(&mut *poisoned.into_inner()),
        };

        if let Err(e) = Self::flush_entries(entries).await {
            eprintln!("Failed to flush logs on shutdown: {}", e);
        }
    }

    /// Get the number of entries currently buffered.
    #[cfg(test)]
    pub fn buffered_count(&self) -> usize {
        match self.buffer.lock() {
            Ok(buffer) => buffer.len(),
            Err(poisoned) => poisoned.into_inner().len(),
        }
    }

    /// Get the count of dropped log entries (for diagnostics).
    #[cfg(test)]
    pub fn dropped_count(&self) -> u64 {
        self.dropped_count
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Start the background task that flushes logs: every flush_interval, and as soon as a batch fills.
    pub fn start_periodic_flush(self: Arc<Self>) {
        let persister = self;
        tokio::spawn(async move {
            let start = tokio::time::Instant::now() + persister.flush_interval;
            let mut interval = tokio::time::interval_at(start, persister.flush_interval);
            loop {
                tokio::select! {
                    _ = interval.tick() => {}
                    _ = persister.flush_wanted.notified() => {}
                }
                persister.flush().await;
            }
        });
    }
}

impl Default for LogPersister {
    fn default() -> Self {
        Self::new()
    }
}

/// Batches log entries and emits them to the frontend periodically.
///
/// Instead of emitting each log individually (which floods the frontend),
/// this collects logs and flushes them every 50ms or when the batch reaches
/// 50 entries. This dramatically improves UI performance.
pub struct LogEmitter {
    /// Buffer for pending log entries.
    buffer: std::sync::Mutex<Vec<LogEntry>>,
    /// Maximum batch size before immediate flush.
    batch_size: usize,
    /// Maximum buffer size before dropping old entries.
    max_buffer_size: usize,
    /// Flush interval.
    flush_interval: Duration,
}

impl LogEmitter {
    /// Create a new log emitter with default settings.
    pub fn new() -> Self {
        Self {
            buffer: std::sync::Mutex::new(Vec::with_capacity(DEFAULT_EMIT_BATCH_SIZE)),
            batch_size: DEFAULT_EMIT_BATCH_SIZE,
            max_buffer_size: DEFAULT_EMIT_BATCH_SIZE * 4, // Allow 4x batch size before dropping
            flush_interval: DEFAULT_EMIT_FLUSH_INTERVAL,
        }
    }

    /// Add a log entry to the emit buffer.
    ///
    /// If the buffer reaches batch_size, triggers an immediate flush.
    /// If the buffer exceeds max_buffer_size, oldest entries are dropped.
    pub fn add(&self, entry: LogEntry) {
        let should_flush = match self.buffer.lock() {
            Ok(mut buffer) => {
                // Bound the buffer to prevent unbounded growth under high log pressure
                if buffer.len() >= self.max_buffer_size {
                    // Drop oldest entries to make room
                    let to_remove = buffer.len() - self.max_buffer_size + 1;
                    buffer.drain(0..to_remove);
                }
                buffer.push(entry);
                buffer.len() >= self.batch_size
            }
            Err(poisoned) => {
                let mut buffer = poisoned.into_inner();
                if buffer.len() >= self.max_buffer_size {
                    let to_remove = buffer.len() - self.max_buffer_size + 1;
                    buffer.drain(0..to_remove);
                }
                buffer.push(entry);
                buffer.len() >= self.batch_size
            }
        };

        if should_flush {
            self.flush_now();
        }
    }

    /// Flush buffered logs to the frontend immediately.
    fn flush_now(&self) {
        let entries = match self.buffer.lock() {
            Ok(mut buffer) => {
                if buffer.is_empty() {
                    return;
                }
                std::mem::take(&mut *buffer)
            }
            Err(poisoned) => std::mem::take(&mut *poisoned.into_inner()),
        };

        if !entries.is_empty() {
            if let Some(handle) = APP_HANDLE.get() {
                // Emit batch event to frontend
                let _ = handle.emit("log_batch", &entries);
            }
        }
    }

    /// Start the periodic flush timer.
    ///
    /// This spawns a background task that flushes the buffer every flush_interval.
    pub fn start_periodic_flush(self: Arc<Self>) {
        let emitter = self;
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(emitter.flush_interval);
            loop {
                interval.tick().await;
                emitter.flush_now();
            }
        });
    }
}

impl Default for LogEmitter {
    fn default() -> Self {
        Self::new()
    }
}

/// Initialize the global log persister.
///
/// Should be called after storage is initialized. Safe to call multiple times;
/// only the first call will initialize the persister.
pub fn init_log_persister() -> Arc<LogPersister> {
    // Check if already initialized
    if let Some(existing) = LOG_PERSISTER.get() {
        return existing.clone();
    }

    let persister = Arc::new(LogPersister::new());
    match LOG_PERSISTER.set(persister.clone()) {
        Ok(_) => {
            persister.clone().start_periodic_flush();
        }
        Err(_) => {
            // Another thread beat us to it, use theirs
            // Use unwrap_or to handle the edge case where get() returns None
            return LOG_PERSISTER.get().cloned().unwrap_or(persister);
        }
    }
    persister
}

/// Get the global log persister, if initialized.
pub fn get_log_persister() -> Option<Arc<LogPersister>> {
    LOG_PERSISTER.get().cloned()
}

/// Saves the log entries still buffered. Called on exit (main.rs `flush_logs_for_exit`, both exit paths) and before
/// the Windows updater quits Bridge (updates.rs), so the last seconds of logs are not lost.
pub async fn flush_logs() {
    if let Some(persister) = get_log_persister() {
        persister.flush().await;
    }
}

/// Initialize the global app handle for event emission.
/// This must be called during Tauri's setup phase.
pub fn set_app_handle(handle: AppHandle) {
    let _ = APP_HANDLE.set(handle);
}

/// Initialize the global log emitter for batched frontend events.
///
/// Should be called after the app handle is set. Safe to call multiple times;
/// only the first call will initialize the emitter.
pub fn init_log_emitter() -> Arc<LogEmitter> {
    // Check if already initialized
    if let Some(existing) = LOG_EMITTER.get() {
        return existing.clone();
    }

    let emitter = Arc::new(LogEmitter::new());
    match LOG_EMITTER.set(emitter.clone()) {
        Ok(_) => {
            emitter.clone().start_periodic_flush();
        }
        Err(_) => {
            // Another thread beat us to it, use theirs
            return LOG_EMITTER.get().cloned().unwrap_or(emitter);
        }
    }
    emitter
}

/// Get the global log emitter, if initialized.
pub fn get_log_emitter() -> Option<Arc<LogEmitter>> {
    LOG_EMITTER.get().cloned()
}

/// Get a reference to the global log buffer.
/// Initializes the buffer on first access.
pub fn get_log_buffer() -> Arc<RwLock<LogBuffer>> {
    LOG_BUFFER
        .get_or_init(|| Arc::new(RwLock::new(LogBuffer::default())))
        .clone()
}

/// Get all logs from the global buffer.
///
/// Returns an empty vector if the buffer lock is poisoned (should never happen
/// in normal operation, but prevents panic on mutex poisoning).
pub fn get_all_logs() -> Vec<LogEntry> {
    let buffer = get_log_buffer();
    let result = match buffer.read() {
        Ok(guard) => guard.get_all(),
        Err(poisoned) => {
            // Log buffer was poisoned - this indicates a serious bug but we
            // shouldn't panic. Return what we can from the poisoned lock.
            eprintln!("WARNING: Log buffer lock was poisoned, recovering...");
            poisoned.into_inner().get_all()
        }
    };
    result
}

/// A visitor that extracts message and device fields from tracing events.
struct LogVisitor {
    message: String,
    device: Option<String>,
}

impl LogVisitor {
    fn new() -> Self {
        Self {
            message: String::new(),
            device: None,
        }
    }
}

impl Visit for LogVisitor {
    fn record_str(&mut self, field: &Field, value: &str) {
        match field.name() {
            "message" => self.message = value.to_string(),
            "device" => self.device = Some(value.to_string()),
            _ => {
                // Append other string fields to message
                if !self.message.is_empty() {
                    self.message.push_str("; ");
                }
                self.message
                    .push_str(&format!("{}={}", field.name(), value));
            }
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        match field.name() {
            "message" => self.message = format!("{:?}", value),
            "device" => self.device = Some(format!("{:?}", value)),
            _ => {
                // Append other debug fields to message
                if !self.message.is_empty() {
                    self.message.push_str("; ");
                }
                self.message
                    .push_str(&format!("{}={:?}", field.name(), value));
            }
        }
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        if !self.message.is_empty() {
            self.message.push_str("; ");
        }
        self.message
            .push_str(&format!("{}={}", field.name(), value));
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        if !self.message.is_empty() {
            self.message.push_str("; ");
        }
        self.message
            .push_str(&format!("{}={}", field.name(), value));
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        if !self.message.is_empty() {
            self.message.push_str("; ");
        }
        self.message
            .push_str(&format!("{}={}", field.name(), value));
    }

    fn record_f64(&mut self, field: &Field, value: f64) {
        if !self.message.is_empty() {
            self.message.push_str("; ");
        }
        self.message
            .push_str(&format!("{}={}", field.name(), value));
    }
}

/// A custom tracing Layer that captures log events and emits them to the Tauri frontend.
///
/// This layer:
/// 1. Extracts the message, level, and optional device field from log events
/// 2. Stores them in a circular buffer for historical access
/// 3. Emits them to the frontend via Tauri's event system (once AppHandle is available)
pub struct TauriLogLayer;

impl TauriLogLayer {
    pub fn new() -> Self {
        // Ensure the buffer is initialized
        let _ = get_log_buffer();
        Self
    }
}

impl Default for TauriLogLayer {
    fn default() -> Self {
        Self::new()
    }
}

impl<S> Layer<S> for TauriLogLayer
where
    S: Subscriber,
{
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        // Extract fields from the event
        let mut visitor = LogVisitor::new();
        event.record(&mut visitor);

        // Get the level as a string
        let level = match *event.metadata().level() {
            Level::ERROR => "error",
            Level::WARN => "warn",
            Level::INFO => "info",
            Level::DEBUG => "debug",
            Level::TRACE => "debug", // Map TRACE to debug for frontend
        };

        // Get the source from the target (module path)
        let target = event.metadata().target();
        let source = if target.starts_with("hyperstudy_bridge::") {
            // Extract the module name after the crate name
            target
                .strip_prefix("hyperstudy_bridge::")
                .unwrap_or(target)
                .split("::")
                .next()
                .unwrap_or("bridge")
        } else if target.starts_with("hyperstudy_bridge") {
            "bridge"
        } else {
            // External crate or unknown
            target.split("::").next().unwrap_or("system")
        };

        // Create the log entry
        let entry = LogEntry::new(
            level,
            visitor.message.clone(),
            visitor.device.clone(),
            source.to_string(),
        );

        // Store in buffer (for real-time frontend display)
        if let Ok(mut buffer) = get_log_buffer().write() {
            buffer.push(entry.clone());
        }

        // Persist to database (batched)
        if let Some(persister) = get_log_persister() {
            persister.add(entry.clone());
        }

        // Emit to frontend via batched emitter (preferred) or directly
        if let Some(emitter) = get_log_emitter() {
            emitter.add(entry);
        } else if let Some(handle) = APP_HANDLE.get() {
            // Fallback to direct emit if emitter not initialized
            let _ = handle.emit("log_event", &entry);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_log_buffer_push_and_get() {
        let mut buffer = LogBuffer::new(3);

        buffer.push(LogEntry::new(
            "info",
            "msg1".to_string(),
            None,
            "test".to_string(),
        ));
        buffer.push(LogEntry::new(
            "info",
            "msg2".to_string(),
            None,
            "test".to_string(),
        ));
        buffer.push(LogEntry::new(
            "info",
            "msg3".to_string(),
            None,
            "test".to_string(),
        ));

        let logs = buffer.get_all();
        assert_eq!(logs.len(), 3);
        assert_eq!(logs[0].message, "msg1");
        assert_eq!(logs[2].message, "msg3");
    }

    #[test]
    fn test_log_buffer_circular() {
        let mut buffer = LogBuffer::new(2);

        buffer.push(LogEntry::new(
            "info",
            "msg1".to_string(),
            None,
            "test".to_string(),
        ));
        buffer.push(LogEntry::new(
            "info",
            "msg2".to_string(),
            None,
            "test".to_string(),
        ));
        buffer.push(LogEntry::new(
            "info",
            "msg3".to_string(),
            None,
            "test".to_string(),
        ));

        let logs = buffer.get_all();
        assert_eq!(logs.len(), 2);
        assert_eq!(logs[0].message, "msg2"); // msg1 was evicted
        assert_eq!(logs[1].message, "msg3");
    }

    #[test]
    fn test_log_entry_new() {
        let entry = LogEntry::new(
            "error",
            "test message".to_string(),
            Some("ttl".to_string()),
            "bridge".to_string(),
        );

        assert_eq!(entry.level, "error");
        assert_eq!(entry.message, "test message");
        assert_eq!(entry.device, Some("ttl".to_string()));
        assert_eq!(entry.source, "bridge");
        assert!(!entry.timestamp.is_empty());
    }

    #[test]
    fn test_log_persister_buffering() {
        let persister = LogPersister::with_config(10, Duration::from_secs(60));

        // Add entries below batch size
        for i in 0..5 {
            persister.add(LogEntry::new(
                "info",
                format!("message {}", i),
                None,
                "test".to_string(),
            ));
        }

        assert_eq!(persister.buffered_count(), 5);
    }

    #[tokio::test]
    async fn test_log_persister_bounded_buffer() {
        // Create persister with small max buffer (batch_size * 10 = 20)
        let persister = LogPersister::with_config(2, Duration::from_secs(60));
        // max_buffer_size = 2 * 10 = 20

        // Add more entries than max buffer size
        // No flush task is running, so the buffer must bound itself by dropping the oldest entries
        for i in 0..25 {
            persister.add(LogEntry::new(
                "info",
                format!("message {}", i),
                None,
                "test".to_string(),
            ));
        }

        // Buffer should be bounded or flushed, entries may have been dropped or flushed
        assert!(
            persister.buffered_count() <= 20,
            "Buffer exceeded max size: {}",
            persister.buffered_count()
        );

        // Verify dropped count was tracked (some entries may have been flushed instead of dropped)
        let dropped = persister
            .dropped_count
            .load(std::sync::atomic::Ordering::Relaxed);
        // At least some entries should have been dropped or we should have a bounded buffer
        assert!(
            dropped > 0 || persister.buffered_count() <= 20,
            "Expected bounded behavior"
        );
    }

    /// Release builds abort on any panic. A plain OS thread (an LSL inlet or outlet, the recorder) whose log line
    /// filled the buffer called tokio::spawn there and ended the whole app (installed 0.8.28, 2026-11-16).
    #[tokio::test]
    async fn a_full_buffer_reached_from_a_plain_thread_is_flushed_without_panicking() {
        let persister = Arc::new(LogPersister::with_config(3, Duration::from_secs(3600)));
        persister.clone().start_periodic_flush();
        let writer = persister.clone();
        let joined = std::thread::spawn(move || {
            for i in 0..3 {
                writer.add(LogEntry::new("info", format!("m{i}"), None, "test".into()));
            }
        })
        .join();
        assert!(joined.is_ok(), "logging from a plain thread panicked");
        // A full batch is flushed now, not at the next periodic flush an hour away.
        tokio::time::timeout(Duration::from_secs(5), async {
            while persister.buffered_count() > 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the full batch was not flushed");
    }

    #[test]
    fn test_log_persister_default() {
        let persister = LogPersister::new();

        // Verify default config
        assert_eq!(persister.batch_size, DEFAULT_PERSIST_BATCH_SIZE);
        assert_eq!(persister.flush_interval, DEFAULT_PERSIST_FLUSH_INTERVAL);
        assert_eq!(persister.buffered_count(), 0);
    }
}

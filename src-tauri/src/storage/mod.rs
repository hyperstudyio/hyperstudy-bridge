//! Database storage layer for HyperStudy Bridge.
//!
//! This module provides persistent storage for:
//! - Application logs (replaces in-memory circular buffer for history)
//! - Session metadata (recording sessions with timestamps)
//!
//! Study recordings are not stored here: each one is its own SQLite file (see `crate::recording`).
//!
//! Uses SQLite (rusqlite) in WAL mode. Every query runs on one dedicated database thread; see [`database`].

mod database;
pub mod logs;
pub mod schema;

use crate::logging::LogEntry;
use database::{Database, Location};
use rusqlite::{params, OptionalExtension};
use std::path::Path;
use std::sync::Arc;
use thiserror::Error;
use tokio::sync::RwLock;
use tracing::{info, warn};

/// Errors that can occur during storage operations.
#[derive(Error, Debug)]
pub enum StorageError {
    #[error("Database error: {0}")]
    Database(#[from] rusqlite::Error),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("The log database has stopped")]
    Closed,
}

pub type StorageResult<T> = Result<T, StorageError>;

/// Log entries older than this are deleted when Bridge starts.
pub const LOG_RETENTION_DAYS: i64 = 30;

/// The log database and the current logging session.
pub struct Storage {
    db: Database,
    current_session_id: RwLock<Option<String>>,
}

impl Storage {
    /// Opens the log database for the app and drops entries older than [`LOG_RETENTION_DAYS`], so it never grows
    /// without bound. Runs once per launch.
    pub async fn open(db_path: &Path) -> StorageResult<Self> {
        let storage = Self::new(db_path).await?;
        // Pruning is housekeeping: if it fails, keep the database and keep saving logs.
        if let Err(error) = storage.cleanup_old_logs(LOG_RETENTION_DAYS).await {
            warn!("Couldn't delete log entries older than {LOG_RETENTION_DAYS} days: {error}");
        }
        Ok(storage)
    }

    /// Open (creating it if missing) the database file and bring its schema up to date.
    pub async fn new(db_path: &Path) -> StorageResult<Self> {
        info!("Opening database at: {}", db_path.display());
        let db = Database::open(Location::File(db_path.to_path_buf())).await?;
        info!("Database initialized successfully");
        Ok(Self::with_database(db))
    }

    /// Create an in-memory database (primarily for testing).
    pub async fn new_in_memory() -> StorageResult<Self> {
        Ok(Self::with_database(Database::open(Location::Memory).await?))
    }

    fn with_database(db: Database) -> Self {
        Self {
            db,
            current_session_id: RwLock::new(None),
        }
    }

    /// Start a new recording session.
    ///
    /// Returns the session ID for use in subsequent operations.
    pub async fn start_session(
        &self,
        metadata: Option<serde_json::Value>,
    ) -> StorageResult<String> {
        let session_id = uuid::Uuid::new_v4().to_string();
        let started_at = chrono::Utc::now().to_rfc3339();
        let metadata_json = metadata.map(|m| m.to_string());

        let id = session_id.clone();
        self.db
            .run(move |conn| {
                conn.execute(
                    "INSERT INTO sessions (id, started_at, metadata) VALUES (?, ?, ?)",
                    params![id, started_at, metadata_json],
                )?;
                Ok(())
            })
            .await?;

        *self.current_session_id.write().await = Some(session_id.clone());

        info!("Started new session: {}", session_id);
        Ok(session_id)
    }

    /// End the current recording session.
    pub async fn end_session(&self) -> StorageResult<()> {
        let session_id = self.current_session_id.write().await.take();

        if let Some(id) = session_id {
            let ended_at = chrono::Utc::now().to_rfc3339();
            let session = id.clone();
            self.db
                .run(move |conn| {
                    conn.execute(
                        "UPDATE sessions SET ended_at = ? WHERE id = ?",
                        params![ended_at, session],
                    )?;
                    Ok(())
                })
                .await?;

            info!("Ended session: {}", id);
        }

        Ok(())
    }

    /// Get the current session ID, if any.
    pub async fn current_session_id(&self) -> Option<String> {
        self.current_session_id.read().await.clone()
    }

    /// Get session metadata by ID.
    pub async fn get_session(&self, session_id: &str) -> StorageResult<Option<Session>> {
        let session_id = session_id.to_string();
        self.db
            .run(move |conn| {
                Ok(conn
                    .query_row(
                        "SELECT id, started_at, ended_at, metadata FROM sessions WHERE id = ?",
                        [session_id],
                        Session::from_row,
                    )
                    .optional()?)
            })
            .await
    }

    /// List all sessions, ordered by start time (most recent first).
    pub async fn list_sessions(&self, limit: Option<i64>) -> StorageResult<Vec<Session>> {
        let limit = limit.unwrap_or(100);
        self.db
            .run(move |conn| {
                let mut statement = conn.prepare(
                    "SELECT id, started_at, ended_at, metadata FROM sessions
                     ORDER BY started_at DESC LIMIT ?",
                )?;
                let sessions = statement
                    .query_map([limit], Session::from_row)?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(sessions)
            })
            .await
    }

    /// Get database statistics for diagnostics.
    pub async fn get_stats(&self) -> StorageResult<StorageStats> {
        self.db
            .run(|conn| {
                let count = |sql: &str| conn.query_row(sql, [], |row| row.get::<_, i64>(0));
                let log_count = count("SELECT COUNT(*) FROM logs")?;
                let session_count = count("SELECT COUNT(*) FROM sessions")?;
                // Database file size (approximate via page_count * page_size)
                let (page_count, page_size) = conn
                    .query_row(
                        "SELECT page_count, page_size FROM pragma_page_count(), pragma_page_size()",
                        [],
                        |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
                    )
                    .unwrap_or((0, 0));
                Ok(StorageStats {
                    log_count,
                    session_count,
                    database_size_bytes: page_count * page_size,
                })
            })
            .await
    }

    /// Write log entries in one transaction, tagged with `session_id`.
    pub async fn insert_logs_batch(
        &self,
        entries: Vec<LogEntry>,
        session_id: Option<String>,
    ) -> StorageResult<()> {
        if entries.is_empty() {
            return Ok(());
        }
        self.db
            .run(move |conn| logs::insert_logs_batch(conn, &entries, session_id.as_deref()))
            .await
    }

    /// Query logs with optional filtering and pagination (see [`logs::query_logs`]).
    pub async fn query_logs(
        &self,
        options: logs::LogQueryOptions,
    ) -> StorageResult<logs::LogQueryResult> {
        self.db
            .run(move |conn| logs::query_logs(conn, &options))
            .await
    }

    /// Count logs by level (for dashboard statistics).
    pub async fn log_counts_by_level(&self) -> StorageResult<Vec<(String, i64)>> {
        self.db
            .run(|conn| logs::get_log_counts_by_level(conn))
            .await
    }

    /// Delete ALL logs from the database.
    pub async fn clear_all_logs(&self) -> StorageResult<u64> {
        let deleted = self
            .db
            .run(|conn| Ok(conn.execute("DELETE FROM logs", [])? as u64))
            .await?;
        info!("Cleared {} log entries from database", deleted);
        Ok(deleted)
    }

    /// Delete logs older than the specified duration.
    pub async fn cleanup_old_logs(&self, older_than_days: i64) -> StorageResult<u64> {
        let cutoff = chrono::Utc::now() - chrono::Duration::days(older_than_days);
        let cutoff_str = cutoff.to_rfc3339();

        let deleted = self
            .db
            .run(move |conn| {
                Ok(conn.execute("DELETE FROM logs WHERE timestamp < ?", [cutoff_str])? as u64)
            })
            .await?;
        info!("Deleted {} old log entries", deleted);
        Ok(deleted)
    }
}

/// Session metadata stored in the database.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Session {
    pub id: String,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub metadata: Option<String>,
}

impl Session {
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            started_at: row.get(1)?,
            ended_at: row.get(2)?,
            metadata: row.get(3)?,
        })
    }
}

/// Storage statistics for diagnostics.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct StorageStats {
    pub log_count: i64,
    pub session_count: i64,
    pub database_size_bytes: i64,
}

/// Global storage instance, initialized during app startup.
static STORAGE: std::sync::OnceLock<Arc<Storage>> = std::sync::OnceLock::new();

/// Initialize the global storage instance.
///
/// Should be called once during application startup.
pub async fn init_storage(db_path: &Path) -> StorageResult<Arc<Storage>> {
    let storage = Arc::new(Storage::open(db_path).await?);

    // Store globally for access from logging layer
    let _ = STORAGE.set(storage.clone());

    Ok(storage)
}

/// Get the global storage instance.
///
/// Returns None if storage hasn't been initialized yet.
pub fn get_storage() -> Option<Arc<Storage>> {
    STORAGE.get().cloned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn temp_db_path(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("bridge-{tag}-{}.db", uuid::Uuid::new_v4()))
    }

    fn remove_db(path: &Path) {
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
        }
    }

    fn entry(age_days: i64, message: &str) -> LogEntry {
        LogEntry {
            timestamp: (chrono::Utc::now() - chrono::Duration::days(age_days)).to_rfc3339(),
            level: "info".into(),
            message: message.into(),
            device: None,
            source: "test".into(),
        }
    }

    /// The log database was never pruned (9,390 entries after a few weeks of use). Opening it now drops entries
    /// older than the retention period.
    #[tokio::test]
    async fn opening_the_log_database_drops_entries_past_retention() {
        let path = temp_db_path("retention");
        {
            let storage = Storage::new(&path).await.unwrap();
            storage
                .insert_logs_batch(
                    vec![entry(LOG_RETENTION_DAYS + 1, "old"), entry(0, "today")],
                    None,
                )
                .await
                .unwrap();
        }
        let storage = Storage::open(&path).await.unwrap();
        assert_eq!(storage.get_stats().await.unwrap().log_count, 1);
        drop(storage);
        remove_db(&path);
    }

    /// The schema exactly as the sqlx-based storage layer created it (schema.rs before the move to rusqlite),
    /// including the LSL time-series tables that have since been removed.
    const SQLX_ERA_SCHEMA: &str = "
        CREATE TABLE IF NOT EXISTS schema_version (
            version INTEGER PRIMARY KEY,
            applied_at TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS sessions (
            id TEXT PRIMARY KEY,
            started_at TEXT NOT NULL,
            ended_at TEXT,
            metadata TEXT
        );
        CREATE TABLE IF NOT EXISTS logs (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            session_id TEXT,
            timestamp TEXT NOT NULL,
            level TEXT NOT NULL,
            message TEXT NOT NULL,
            device TEXT,
            source TEXT NOT NULL,
            FOREIGN KEY (session_id) REFERENCES sessions(id)
        );
        CREATE INDEX IF NOT EXISTS idx_logs_timestamp ON logs(timestamp);
        CREATE INDEX IF NOT EXISTS idx_logs_level ON logs(level);
        CREATE INDEX IF NOT EXISTS idx_logs_device ON logs(device);
        CREATE INDEX IF NOT EXISTS idx_logs_session ON logs(session_id);
        CREATE TABLE IF NOT EXISTS lsl_streams (
            uid TEXT PRIMARY KEY,
            session_id TEXT NOT NULL,
            name TEXT NOT NULL,
            stream_type TEXT NOT NULL,
            channel_count INTEGER NOT NULL,
            sample_rate REAL NOT NULL,
            channel_format TEXT NOT NULL,
            source_id TEXT,
            hostname TEXT,
            metadata TEXT,
            created_at TEXT NOT NULL,
            FOREIGN KEY (session_id) REFERENCES sessions(id)
        );
        CREATE INDEX IF NOT EXISTS idx_lsl_streams_session ON lsl_streams(session_id);
        CREATE TABLE IF NOT EXISTS lsl_samples (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            session_id TEXT NOT NULL,
            stream_uid TEXT NOT NULL,
            timestamp REAL NOT NULL,
            channel_data BLOB NOT NULL,
            FOREIGN KEY (session_id) REFERENCES sessions(id),
            FOREIGN KEY (stream_uid) REFERENCES lsl_streams(uid)
        );
        CREATE INDEX IF NOT EXISTS idx_lsl_samples_stream_time
         ON lsl_samples(session_id, stream_uid, timestamp);
    ";

    /// Write a database the way a sqlx-era Bridge left it: WAL, schema version 1, an ended and a still-open
    /// session, logs (one past retention), and rows in the removed LSL tables. The writer is still open when the
    /// files are copied, so the copy holds its commits only in the WAL, as after a crash or force quit.
    fn write_sqlx_era_database(dir: &Path) -> std::path::PathBuf {
        let writing = dir.join("writing.db");
        let conn = Connection::open(&writing).unwrap();
        conn.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0; PRAGMA foreign_keys=ON;",
        )
        .unwrap();
        conn.execute_batch(SQLX_ERA_SCHEMA).unwrap();
        let ago = |days: i64| (chrono::Utc::now() - chrono::Duration::days(days)).to_rfc3339();
        conn.execute(
            "INSERT INTO schema_version (version, applied_at) VALUES (1, ?)",
            [ago(90)],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO sessions VALUES ('s-ended', ?, ?, '{\"k\":\"v\"}')",
            params![ago(45), ago(44)],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO sessions (id, started_at) VALUES ('s-open', ?)",
            [ago(1)],
        )
        .unwrap();
        let log =
            |session: Option<&str>, age: i64, level: &str, message: &str, device: Option<&str>| {
                conn.execute(
                    "INSERT INTO logs (session_id, timestamp, level, message, device, source)
                 VALUES (?, ?, ?, ?, ?, 'old-bridge')",
                    params![session, ago(age), level, message, device],
                )
                .unwrap();
            };
        log(Some("s-ended"), 44, "info", "past retention", None);
        log(
            Some("s-open"),
            3,
            "info",
            "Connection established",
            Some("ttl"),
        );
        log(Some("s-open"), 2, "warn", "100% buffer", Some("kernel"));
        log(Some("s-open"), 1, "error", "Connection failed", Some("ttl"));
        log(None, 0, "info", "before any session", None);
        conn.execute(
            "INSERT INTO lsl_streams VALUES ('uid-1', 's-ended', 'EEG', 'EEG', 2, 250.0, 'float32',
             NULL, NULL, NULL, ?)",
            [ago(45)],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO lsl_samples (session_id, stream_uid, timestamp, channel_data)
             VALUES ('s-ended', 'uid-1', 1.5, x'0000803F00000040')",
            [],
        )
        .unwrap();

        let crashed = dir.join("hyperstudy-bridge.db");
        for suffix in ["", "-wal", "-shm"] {
            let from = format!("{}{suffix}", writing.display());
            if Path::new(&from).exists() {
                std::fs::copy(&from, format!("{}{suffix}", crashed.display())).unwrap();
            }
        }
        assert!(
            Path::new(&format!("{}-wal", crashed.display())).exists(),
            "the copy should hold uncheckpointed commits in its WAL"
        );
        drop(conn);
        crashed
    }

    /// A user's existing database, written by the sqlx version, opens with the rusqlite version: its logs and
    /// sessions read back, retention still applies, new logs can join an old session, the removed LSL tables and
    /// their rows are left alone, and the file stays in WAL at schema version 1.
    #[tokio::test]
    async fn a_database_written_by_the_sqlx_version_opens_and_keeps_working() {
        let dir = std::env::temp_dir().join(format!("bridge-sqlx-era-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = write_sqlx_era_database(&dir);

        let storage = Storage::open(&path).await.unwrap();
        let stats = storage.get_stats().await.unwrap();
        assert_eq!(
            (stats.log_count, stats.session_count),
            (4, 2),
            "only the 44-day-old log is pruned"
        );
        assert!(stats.database_size_bytes > 0);

        let all = storage.query_logs(Default::default()).await.unwrap();
        let messages: Vec<&str> = all.logs.iter().map(|l| l.message.as_str()).collect();
        assert_eq!(
            messages,
            [
                "before any session",
                "Connection failed",
                "100% buffer",
                "Connection established"
            ]
        );
        assert_eq!(all.logs[1].session_id.as_deref(), Some("s-open"));
        assert_eq!(all.logs[1].device.as_deref(), Some("ttl"));
        assert_eq!(all.logs[1].source, "old-bridge");
        assert_eq!(all.logs[0].session_id, None);

        let ttl = storage
            .query_logs(logs::LogQueryOptions {
                device: Some("ttl".into()),
                search: Some("connection".into()),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(ttl.total_count, 2);

        let sessions = storage.list_sessions(None).await.unwrap();
        assert_eq!(
            sessions.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
            ["s-open", "s-ended"]
        );
        assert_eq!(sessions[1].metadata.as_deref(), Some("{\"k\":\"v\"}"));
        assert!(sessions[1].ended_at.is_some());

        // New entries join an existing session (the foreign key to sessions is still enforced).
        storage
            .insert_logs_batch(vec![entry(0, "after upgrade")], Some("s-open".into()))
            .await
            .unwrap();
        assert!(storage
            .insert_logs_batch(vec![entry(0, "orphan")], Some("no-such-session".into()))
            .await
            .is_err());
        let counts: std::collections::HashMap<_, _> = storage
            .log_counts_by_level()
            .await
            .unwrap()
            .into_iter()
            .collect();
        assert_eq!(counts.get("info"), Some(&3));
        drop(storage);

        let conn = Connection::open(&path).unwrap();
        let one = |sql: &str| conn.query_row(sql, [], |row| row.get::<_, i64>(0)).unwrap();
        assert_eq!(one("SELECT COUNT(*) FROM lsl_streams"), 1);
        assert_eq!(one("SELECT COUNT(*) FROM lsl_samples"), 1);
        assert_eq!(one("SELECT COUNT(*) FROM schema_version"), 1);
        assert_eq!(one("SELECT MAX(version) FROM schema_version"), 1);
        assert_eq!(
            one("SELECT COUNT(*) FROM logs WHERE message = 'after upgrade'"),
            1
        );
        let journal: String = conn
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();
        assert_eq!(journal, "wal");
        drop(conn);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A database created now has only the live tables: the LSL tables are no longer created.
    #[tokio::test]
    async fn a_new_database_has_no_lsl_tables() {
        let path = temp_db_path("fresh");
        drop(Storage::new(&path).await.unwrap());
        let conn = Connection::open(&path).unwrap();
        let lsl_tables: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name LIKE 'lsl_%'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(lsl_tables, 0);
        let journal: String = conn
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();
        assert_eq!(journal, "wal");
        drop(conn);
        remove_db(&path);
    }

    /// Drive a future to completion on the current thread with no async runtime at all.
    fn block_on_without_runtime<F: std::future::Future>(future: F) -> F::Output {
        struct ThreadWaker(std::thread::Thread);
        impl std::task::Wake for ThreadWaker {
            fn wake(self: Arc<Self>) {
                self.0.unpark();
            }
        }
        let waker = std::task::Waker::from(Arc::new(ThreadWaker(std::thread::current())));
        let mut context = std::task::Context::from_waker(&waker);
        let mut future = std::pin::pin!(future);
        loop {
            if let std::task::Poll::Ready(output) = future.as_mut().poll(&mut context) {
                return output;
            }
            std::thread::park();
        }
    }

    /// The exit-time log flush runs on a plain thread. Nothing on the storage path may need a Tokio runtime:
    /// `spawn_blocking` or `tokio::spawn` there would panic, and release builds abort on panic.
    #[test]
    fn storage_calls_need_no_tokio_runtime() {
        let storage = block_on_without_runtime(Storage::new_in_memory()).unwrap();
        block_on_without_runtime(storage.insert_logs_batch(vec![entry(0, "x")], None)).unwrap();
        let stats = block_on_without_runtime(storage.get_stats()).unwrap();
        assert_eq!(stats.log_count, 1);
    }

    #[tokio::test]
    async fn test_storage_creation() {
        let storage = Storage::new_in_memory().await.unwrap();
        let stats = storage.get_stats().await.unwrap();
        assert_eq!(stats.log_count, 0);
        assert_eq!(stats.session_count, 0);
    }

    #[tokio::test]
    async fn test_session_lifecycle() {
        let storage = Storage::new_in_memory().await.unwrap();

        // Start session
        let session_id = storage.start_session(None).await.unwrap();
        assert!(!session_id.is_empty());

        // Check current session
        assert_eq!(storage.current_session_id().await, Some(session_id.clone()));

        // End session
        storage.end_session().await.unwrap();
        assert_eq!(storage.current_session_id().await, None);

        // Verify session was updated
        let session = storage.get_session(&session_id).await.unwrap().unwrap();
        assert!(session.ended_at.is_some());
        assert!(storage.get_session("missing").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_list_sessions() {
        let storage = Storage::new_in_memory().await.unwrap();

        // Create multiple sessions
        let id1 = storage.start_session(None).await.unwrap();
        storage.end_session().await.unwrap();

        let id2 = storage.start_session(None).await.unwrap();
        storage.end_session().await.unwrap();

        // List sessions
        let sessions = storage.list_sessions(Some(10)).await.unwrap();
        assert_eq!(sessions.len(), 2);

        // Most recent first
        assert_eq!(sessions[0].id, id2);
        assert_eq!(sessions[1].id, id1);
    }

    #[tokio::test]
    async fn test_cleanup_old_logs() {
        let storage = Storage::new_in_memory().await.unwrap();

        // Insert some logs with old timestamps
        storage
            .insert_logs_batch(vec![entry(10, "Old log"), entry(0, "Recent log")], None)
            .await
            .unwrap();

        // Verify we have 2 logs
        let stats = storage.get_stats().await.unwrap();
        assert_eq!(stats.log_count, 2);

        // Clean up logs older than 5 days
        let deleted = storage.cleanup_old_logs(5).await.unwrap();
        assert_eq!(deleted, 1);

        // Verify only recent log remains
        let stats = storage.get_stats().await.unwrap();
        assert_eq!(stats.log_count, 1);
    }

    #[tokio::test]
    async fn test_cleanup_with_no_old_logs() {
        let storage = Storage::new_in_memory().await.unwrap();

        // Insert only recent logs
        storage
            .insert_logs_batch(vec![entry(0, "Recent log")], None)
            .await
            .unwrap();

        // Clean up should delete nothing
        let deleted = storage.cleanup_old_logs(1).await.unwrap();
        assert_eq!(deleted, 0);

        // Verify log still exists
        let stats = storage.get_stats().await.unwrap();
        assert_eq!(stats.log_count, 1);
    }

    #[tokio::test]
    async fn test_clear_all_logs() {
        let storage = Storage::new_in_memory().await.unwrap();
        storage
            .insert_logs_batch(vec![entry(0, "a"), entry(0, "b")], None)
            .await
            .unwrap();
        assert_eq!(storage.clear_all_logs().await.unwrap(), 2);
        assert_eq!(storage.get_stats().await.unwrap().log_count, 0);
    }

    #[tokio::test]
    async fn test_storage_stats() {
        let storage = Storage::new_in_memory().await.unwrap();

        // Initially empty
        let stats = storage.get_stats().await.unwrap();
        assert_eq!(stats.log_count, 0);
        assert_eq!(stats.session_count, 0);

        // Add a session
        storage.start_session(None).await.unwrap();
        let stats = storage.get_stats().await.unwrap();
        assert_eq!(stats.session_count, 1);

        // Add a log
        storage
            .insert_logs_batch(vec![entry(0, "Test log")], None)
            .await
            .unwrap();

        let stats = storage.get_stats().await.unwrap();
        assert_eq!(stats.log_count, 1);
    }
}

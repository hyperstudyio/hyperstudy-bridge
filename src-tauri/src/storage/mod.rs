//! Database storage layer for HyperStudy Bridge.
//!
//! This module provides persistent storage for:
//! - Application logs (replaces in-memory circular buffer for history)
//! - LSL time-series data (samples from connected streams)
//! - Session metadata (recording sessions with timestamps)
//!
//! Uses SQLite with WAL mode for high-performance concurrent access.

pub mod logs;
pub mod schema;
pub mod timeseries;

use sqlx::sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions};
use std::path::Path;
use std::str::FromStr;
use std::sync::Arc;
use thiserror::Error;
use tokio::sync::RwLock;
use tracing::{error, info};

/// Errors that can occur during storage operations.
#[derive(Error, Debug)]
pub enum StorageError {
    #[error("Database error: {0}")]
    Database(#[from] sqlx::Error),

    #[error("Migration error: {0}")]
    Migration(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("Session not found: {0}")]
    SessionNotFound(String),

    #[error("Storage not initialized")]
    NotInitialized,
}

pub type StorageResult<T> = Result<T, StorageError>;

/// Log entries older than this are deleted when Bridge starts.
pub const LOG_RETENTION_DAYS: i64 = 30;

/// Database connection pool and session management.
pub struct Storage {
    pool: SqlitePool,
    current_session_id: RwLock<Option<String>>,
}

impl Storage {
    /// Create a new storage instance and initialize the database.
    ///
    /// Creates the database file if it doesn't exist and runs migrations.
    /// Opens the log database for the app and drops entries older than [`LOG_RETENTION_DAYS`], so it never grows
    /// without bound. Runs once per launch.
    pub async fn open(db_path: &Path) -> StorageResult<Self> {
        let storage = Self::new(db_path).await?;
        storage.cleanup_old_logs(LOG_RETENTION_DAYS).await?;
        Ok(storage)
    }

    pub async fn new(db_path: &Path) -> StorageResult<Self> {
        // Ensure parent directory exists (using async I/O to avoid blocking)
        if let Some(parent) = db_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        let db_url = format!("sqlite:{}?mode=rwc", db_path.display());
        info!("Opening database at: {}", db_path.display());

        // Configure SQLite connection options for optimal performance
        let connect_options = SqliteConnectOptions::from_str(&db_url)?
            .create_if_missing(true)
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
            .synchronous(sqlx::sqlite::SqliteSynchronous::Normal)
            .busy_timeout(std::time::Duration::from_secs(30));

        // Create connection pool
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .min_connections(1)
            .connect_with(connect_options)
            .await?;

        // Set additional pragmas for performance
        sqlx::query("PRAGMA cache_size = 10000")
            .execute(&pool)
            .await?;
        sqlx::query("PRAGMA temp_store = MEMORY")
            .execute(&pool)
            .await?;

        // Run schema migrations
        schema::run_migrations(&pool).await?;

        info!("Database initialized successfully");

        Ok(Self {
            pool,
            current_session_id: RwLock::new(None),
        })
    }

    /// Create an in-memory database (primarily for testing).
    pub async fn new_in_memory() -> StorageResult<Self> {
        let connect_options = SqliteConnectOptions::from_str("sqlite::memory:")?
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
            .synchronous(sqlx::sqlite::SqliteSynchronous::Normal);

        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(connect_options)
            .await?;

        schema::run_migrations(&pool).await?;

        Ok(Self {
            pool,
            current_session_id: RwLock::new(None),
        })
    }

    /// Get a reference to the connection pool.
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
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

        sqlx::query("INSERT INTO sessions (id, started_at, metadata) VALUES (?, ?, ?)")
            .bind(&session_id)
            .bind(&started_at)
            .bind(&metadata_json)
            .execute(&self.pool)
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

            sqlx::query("UPDATE sessions SET ended_at = ? WHERE id = ?")
                .bind(&ended_at)
                .bind(&id)
                .execute(&self.pool)
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
        let session = sqlx::query_as::<_, Session>(
            "SELECT id, started_at, ended_at, metadata FROM sessions WHERE id = ?",
        )
        .bind(session_id)
        .fetch_optional(&self.pool)
        .await?;

        Ok(session)
    }

    /// List all sessions, ordered by start time (most recent first).
    pub async fn list_sessions(&self, limit: Option<i64>) -> StorageResult<Vec<Session>> {
        let limit = limit.unwrap_or(100);

        let sessions = sqlx::query_as::<_, Session>(
            "SELECT id, started_at, ended_at, metadata FROM sessions
             ORDER BY started_at DESC LIMIT ?",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;

        Ok(sessions)
    }

    /// Get database statistics for diagnostics.
    pub async fn get_stats(&self) -> StorageResult<StorageStats> {
        let log_count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM logs")
            .fetch_one(&self.pool)
            .await?;

        let sample_count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM lsl_samples")
            .fetch_one(&self.pool)
            .await?;

        let stream_count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM lsl_streams")
            .fetch_one(&self.pool)
            .await?;

        let session_count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM sessions")
            .fetch_one(&self.pool)
            .await?;

        // Get database file size (approximate via page_count * page_size)
        let page_info: (i64, i64) = sqlx::query_as(
            "SELECT page_count, page_size FROM pragma_page_count(), pragma_page_size()",
        )
        .fetch_one(&self.pool)
        .await
        .unwrap_or((0, 0));

        Ok(StorageStats {
            log_count: log_count.0,
            sample_count: sample_count.0,
            stream_count: stream_count.0,
            session_count: session_count.0,
            database_size_bytes: page_info.0 * page_info.1,
        })
    }

    /// Vacuum the database to reclaim space.
    pub async fn vacuum(&self) -> StorageResult<()> {
        sqlx::query("VACUUM").execute(&self.pool).await?;
        info!("Database vacuumed");
        Ok(())
    }

    /// Delete ALL logs from the database.
    pub async fn clear_all_logs(&self) -> StorageResult<u64> {
        let result = sqlx::query("DELETE FROM logs").execute(&self.pool).await?;

        let deleted = result.rows_affected();
        info!("Cleared {} log entries from database", deleted);
        Ok(deleted)
    }

    /// Delete logs older than the specified duration.
    pub async fn cleanup_old_logs(&self, older_than_days: i64) -> StorageResult<u64> {
        let cutoff = chrono::Utc::now() - chrono::Duration::days(older_than_days);
        let cutoff_str = cutoff.to_rfc3339();

        let result = sqlx::query("DELETE FROM logs WHERE timestamp < ?")
            .bind(&cutoff_str)
            .execute(&self.pool)
            .await?;

        let deleted = result.rows_affected();
        info!("Deleted {} old log entries", deleted);
        Ok(deleted)
    }

    /// Delete LSL samples older than the specified duration.
    pub async fn cleanup_old_samples(&self, older_than_days: i64) -> StorageResult<u64> {
        // LSL timestamps are in seconds since some epoch, so we need to handle them differently
        // For simplicity, we'll use session-based cleanup instead
        let cutoff = chrono::Utc::now() - chrono::Duration::days(older_than_days);
        let cutoff_str = cutoff.to_rfc3339();

        // Delete samples from sessions that ended before the cutoff
        let result = sqlx::query(
            "DELETE FROM lsl_samples WHERE session_id IN
             (SELECT id FROM sessions WHERE ended_at IS NOT NULL AND ended_at < ?)",
        )
        .bind(&cutoff_str)
        .execute(&self.pool)
        .await?;

        let deleted = result.rows_affected();
        info!("Deleted {} old LSL samples", deleted);
        Ok(deleted)
    }
}

/// Session metadata stored in the database.
#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize)]
pub struct Session {
    pub id: String,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub metadata: Option<String>,
}

/// Storage statistics for diagnostics.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct StorageStats {
    pub log_count: i64,
    pub sample_count: i64,
    pub stream_count: i64,
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

    /// The log database was never pruned (9,390 entries after a few weeks of use). Opening it now drops entries
    /// older than the retention period.
    #[tokio::test]
    async fn opening_the_log_database_drops_entries_past_retention() {
        let path =
            std::env::temp_dir().join(format!("bridge-retention-{}.db", uuid::Uuid::new_v4()));
        let entry = |age_days: i64, message: &str| crate::logging::LogEntry {
            timestamp: (chrono::Utc::now() - chrono::Duration::days(age_days)).to_rfc3339(),
            level: "info".into(),
            message: message.into(),
            device: None,
            source: "test".into(),
        };
        {
            let storage = Storage::new(&path).await.unwrap();
            logs::insert_logs_batch(
                storage.pool(),
                &[entry(LOG_RETENTION_DAYS + 1, "old"), entry(0, "today")],
                None,
            )
            .await
            .unwrap();
            storage.pool().close().await;
        }
        let storage = Storage::open(&path).await.unwrap();
        assert_eq!(storage.get_stats().await.unwrap().log_count, 1);
        storage.pool().close().await;
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
        }
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
        let old_timestamp = (chrono::Utc::now() - chrono::Duration::days(10)).to_rfc3339();
        let recent_timestamp = chrono::Utc::now().to_rfc3339();

        sqlx::query("INSERT INTO logs (timestamp, level, message, source) VALUES (?, ?, ?, ?)")
            .bind(&old_timestamp)
            .bind("info")
            .bind("Old log")
            .bind("test")
            .execute(storage.pool())
            .await
            .unwrap();

        sqlx::query("INSERT INTO logs (timestamp, level, message, source) VALUES (?, ?, ?, ?)")
            .bind(&recent_timestamp)
            .bind("info")
            .bind("Recent log")
            .bind("test")
            .execute(storage.pool())
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
        let recent_timestamp = chrono::Utc::now().to_rfc3339();

        sqlx::query("INSERT INTO logs (timestamp, level, message, source) VALUES (?, ?, ?, ?)")
            .bind(&recent_timestamp)
            .bind("info")
            .bind("Recent log")
            .bind("test")
            .execute(storage.pool())
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
    async fn test_storage_stats() {
        let storage = Storage::new_in_memory().await.unwrap();

        // Initially empty
        let stats = storage.get_stats().await.unwrap();
        assert_eq!(stats.log_count, 0);
        assert_eq!(stats.session_count, 0);
        assert_eq!(stats.stream_count, 0);
        assert_eq!(stats.sample_count, 0);

        // Add a session
        storage.start_session(None).await.unwrap();
        let stats = storage.get_stats().await.unwrap();
        assert_eq!(stats.session_count, 1);

        // Add a log
        sqlx::query("INSERT INTO logs (timestamp, level, message, source) VALUES (?, ?, ?, ?)")
            .bind(chrono::Utc::now().to_rfc3339())
            .bind("info")
            .bind("Test log")
            .bind("test")
            .execute(storage.pool())
            .await
            .unwrap();

        let stats = storage.get_stats().await.unwrap();
        assert_eq!(stats.log_count, 1);
    }
}

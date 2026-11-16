//! Database schema definitions and migrations.
//!
//! This module handles:
//! - Schema creation for all tables
//! - Index creation for query optimization
//! - Schema migrations for upgrades

use sqlx::SqlitePool;
use tracing::info;

use super::StorageResult;

/// Current schema version.
/// Increment this when making schema changes.
const SCHEMA_VERSION: i32 = 1;

/// Run all necessary migrations to bring the database up to date.
pub async fn run_migrations(pool: &SqlitePool) -> StorageResult<()> {
    // Create schema version table if it doesn't exist
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS schema_version (
            version INTEGER PRIMARY KEY,
            applied_at TEXT NOT NULL
        )",
    )
    .execute(pool)
    .await?;

    // Get current version
    // Note: MAX() returns NULL when table is empty, so we use Option<i32>
    let current_version: (Option<i32>,) = sqlx::query_as("SELECT MAX(version) FROM schema_version")
        .fetch_one(pool)
        .await?;

    let current = current_version.0.unwrap_or(0);

    if current < SCHEMA_VERSION {
        info!(
            "Running database migrations from version {} to {}",
            current, SCHEMA_VERSION
        );

        // Run migrations in order
        if current < 1 {
            migrate_v1(pool).await?;
        }

        info!("Database migrations complete");
    }

    Ok(())
}

/// Migration to version 1: Initial schema.
///
/// Databases created before the LSL time-series store was removed also hold `lsl_streams` and `lsl_samples`
/// (and their indexes). Nothing reads or writes them any more. They are left in place rather than dropped, so
/// opening an existing database never deletes data; new databases simply don't get them.
async fn migrate_v1(pool: &SqlitePool) -> StorageResult<()> {
    info!("Applying migration v1: Initial schema");

    // Sessions table - tracks recording sessions
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS sessions (
            id TEXT PRIMARY KEY,
            started_at TEXT NOT NULL,
            ended_at TEXT,
            metadata TEXT
        )",
    )
    .execute(pool)
    .await?;

    // Logs table - stores application log entries
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS logs (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            session_id TEXT,
            timestamp TEXT NOT NULL,
            level TEXT NOT NULL,
            message TEXT NOT NULL,
            device TEXT,
            source TEXT NOT NULL,
            FOREIGN KEY (session_id) REFERENCES sessions(id)
        )",
    )
    .execute(pool)
    .await?;

    // Log indexes for efficient querying
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_logs_timestamp ON logs(timestamp)")
        .execute(pool)
        .await?;
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_logs_level ON logs(level)")
        .execute(pool)
        .await?;
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_logs_device ON logs(device)")
        .execute(pool)
        .await?;
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_logs_session ON logs(session_id)")
        .execute(pool)
        .await?;

    // Record migration
    let applied_at = chrono::Utc::now().to_rfc3339();
    sqlx::query("INSERT INTO schema_version (version, applied_at) VALUES (?, ?)")
        .bind(1)
        .bind(&applied_at)
        .execute(pool)
        .await?;

    info!("Migration v1 complete");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;

    async fn create_test_pool() -> SqlitePool {
        SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn test_migrations_run_successfully() {
        let pool = create_test_pool().await;
        run_migrations(&pool).await.unwrap();

        // Verify tables exist
        let tables: Vec<(String,)> =
            sqlx::query_as("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
                .fetch_all(&pool)
                .await
                .unwrap();

        let table_names: Vec<&str> = tables.iter().map(|t| t.0.as_str()).collect();
        assert!(table_names.contains(&"sessions"));
        assert!(table_names.contains(&"logs"));
        assert!(table_names.contains(&"schema_version"));
    }

    #[tokio::test]
    async fn test_migrations_are_idempotent() {
        let pool = create_test_pool().await;

        // Run migrations twice
        run_migrations(&pool).await.unwrap();
        run_migrations(&pool).await.unwrap();

        // Should still work
        let version: (i32,) = sqlx::query_as("SELECT MAX(version) FROM schema_version")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(version.0, SCHEMA_VERSION);
    }

    #[tokio::test]
    async fn test_indexes_created() {
        let pool = create_test_pool().await;
        run_migrations(&pool).await.unwrap();

        let indexes: Vec<(String,)> = sqlx::query_as(
            "SELECT name FROM sqlite_master WHERE type='index' AND name LIKE 'idx_%'",
        )
        .fetch_all(&pool)
        .await
        .unwrap();

        let index_names: Vec<&str> = indexes.iter().map(|i| i.0.as_str()).collect();
        assert!(index_names.contains(&"idx_logs_timestamp"));
        assert!(index_names.contains(&"idx_logs_level"));
        assert!(index_names.contains(&"idx_logs_device"));
    }
}

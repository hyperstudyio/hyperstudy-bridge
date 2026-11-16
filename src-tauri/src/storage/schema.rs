//! Database schema definitions and migrations.
//!
//! This module handles:
//! - Schema creation for all tables
//! - Index creation for query optimization
//! - Schema migrations for upgrades

use rusqlite::{params, Connection};
use tracing::info;

use super::StorageResult;

/// Current schema version.
/// Increment this when making schema changes.
const SCHEMA_VERSION: i32 = 1;

/// Run all necessary migrations to bring the database up to date.
pub fn run_migrations(conn: &mut Connection) -> StorageResult<()> {
    // Create schema version table if it doesn't exist
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_version (
            version INTEGER PRIMARY KEY,
            applied_at TEXT NOT NULL
        )",
    )?;

    // MAX() returns NULL when the table is empty.
    let current: Option<i32> =
        conn.query_row("SELECT MAX(version) FROM schema_version", [], |row| {
            row.get(0)
        })?;
    let current = current.unwrap_or(0);

    if current < SCHEMA_VERSION {
        info!(
            "Running database migrations from version {} to {}",
            current, SCHEMA_VERSION
        );

        // Run migrations in order, each in its own transaction so a failure leaves no partial schema.
        if current < 1 {
            let tx = conn.transaction()?;
            migrate_v1(&tx)?;
            tx.commit()?;
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
fn migrate_v1(conn: &Connection) -> StorageResult<()> {
    info!("Applying migration v1: Initial schema");

    conn.execute_batch(
        "-- Sessions table - tracks recording sessions
        CREATE TABLE IF NOT EXISTS sessions (
            id TEXT PRIMARY KEY,
            started_at TEXT NOT NULL,
            ended_at TEXT,
            metadata TEXT
        );

        -- Logs table - stores application log entries
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

        -- Log indexes for efficient querying
        CREATE INDEX IF NOT EXISTS idx_logs_timestamp ON logs(timestamp);
        CREATE INDEX IF NOT EXISTS idx_logs_level ON logs(level);
        CREATE INDEX IF NOT EXISTS idx_logs_device ON logs(device);
        CREATE INDEX IF NOT EXISTS idx_logs_session ON logs(session_id);",
    )?;

    // Record migration
    let applied_at = chrono::Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO schema_version (version, applied_at) VALUES (?, ?)",
        params![1, applied_at],
    )?;

    info!("Migration v1 complete");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn migrated() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        run_migrations(&mut conn).unwrap();
        conn
    }

    fn names(conn: &Connection, kind: &str) -> Vec<String> {
        let mut statement = conn
            .prepare("SELECT name FROM sqlite_master WHERE type = ? ORDER BY name")
            .unwrap();
        let names = statement
            .query_map([kind], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        names
    }

    #[test]
    fn test_migrations_run_successfully() {
        let conn = migrated();
        let tables = names(&conn, "table");
        for table in ["sessions", "logs", "schema_version"] {
            assert!(
                tables.iter().any(|t| t == table),
                "missing {table}: {tables:?}"
            );
        }
    }

    #[test]
    fn test_migrations_are_idempotent() {
        let mut conn = migrated();
        run_migrations(&mut conn).unwrap();

        let (version, rows): (i32, i64) = conn
            .query_row(
                "SELECT MAX(version), COUNT(*) FROM schema_version",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!((version, rows), (SCHEMA_VERSION, 1));
    }

    #[test]
    fn test_indexes_created() {
        let conn = migrated();
        let indexes = names(&conn, "index");
        for index in [
            "idx_logs_timestamp",
            "idx_logs_level",
            "idx_logs_device",
            "idx_logs_session",
        ] {
            assert!(
                indexes.iter().any(|i| i == index),
                "missing {index}: {indexes:?}"
            );
        }
    }
}

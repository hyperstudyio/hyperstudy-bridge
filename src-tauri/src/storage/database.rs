//! The log database's single connection and the thread that owns it.
//!
//! rusqlite is blocking, and every caller of the storage layer is async. All database work therefore runs on
//! one dedicated OS thread that owns the only connection: callers send it a closure and await the reply on a
//! oneshot channel. This keeps blocking I/O off tokio's workers, runs statements in the order they were sent
//! (SQLite allows one writer at a time anyway), and needs no runtime at all: sending is a plain
//! `std::sync::mpsc` send and the reply is awaited on a runtime-agnostic oneshot, so the exit-time flush works
//! from any thread, and no log call path ever reaches `tokio::spawn` or `spawn_blocking`.
//!
//! The thread stops, closing the connection, once the last [`Database`] handle is dropped and the queued work
//! has run.

use super::{schema, StorageError, StorageResult};
use rusqlite::Connection;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;
use tokio::sync::oneshot;

type Job = Box<dyn FnOnce(&mut Connection) + Send>;

/// Where the database lives.
pub(super) enum Location {
    File(PathBuf),
    Memory,
}

/// A handle to the database thread.
pub(super) struct Database {
    jobs: mpsc::Sender<Job>,
}

impl Database {
    /// Start the database thread, open the connection on it and bring the schema up to date.
    pub(super) async fn open(location: Location) -> StorageResult<Self> {
        let (jobs, inbox) = mpsc::channel::<Job>();
        let (opened_tx, opened_rx) = oneshot::channel();
        std::thread::Builder::new()
            .name("bridge-log-db".into())
            .spawn(move || {
                let mut conn = match open_connection(&location) {
                    Ok(conn) => {
                        let _ = opened_tx.send(Ok(()));
                        conn
                    }
                    Err(error) => {
                        let _ = opened_tx.send(Err(error));
                        return;
                    }
                };
                for job in inbox {
                    job(&mut conn);
                }
            })?;
        opened_rx.await.map_err(|_| StorageError::Closed)??;
        Ok(Self { jobs })
    }

    /// Run `work` on the database thread and return its result.
    pub(super) async fn run<T, F>(&self, work: F) -> StorageResult<T>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> StorageResult<T> + Send + 'static,
    {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.jobs
            .send(Box::new(move |conn| {
                let _ = reply_tx.send(work(conn));
            }))
            .map_err(|_| StorageError::Closed)?;
        reply_rx.await.map_err(|_| StorageError::Closed)?
    }
}

/// Open the connection with the settings the log database has always used: WAL, synchronous=NORMAL, a 30 s
/// busy timeout and foreign keys enforced (sqlx, which this replaced, turned them on by default).
fn open_connection(location: &Location) -> StorageResult<Connection> {
    let mut conn = match location {
        Location::File(path) => {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            Connection::open(path)?
        }
        Location::Memory => Connection::open_in_memory()?,
    };
    conn.busy_timeout(Duration::from_secs(30))?;
    let journal_mode: String = conn.query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))?;
    if matches!(location, Location::File(_)) && !journal_mode.eq_ignore_ascii_case("wal") {
        tracing::warn!("Log database is using journal mode {journal_mode}, not WAL");
    }
    conn.execute_batch(
        "PRAGMA synchronous=NORMAL;
         PRAGMA foreign_keys=ON;
         PRAGMA cache_size=10000;
         PRAGMA temp_store=MEMORY;",
    )?;
    schema::run_migrations(&mut conn)?;
    Ok(conn)
}

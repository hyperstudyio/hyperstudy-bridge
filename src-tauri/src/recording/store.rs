use super::{RecordedSample, RecordingContext, StreamDescriptor};
use anyhow::{ensure, Context, Result};
use rusqlite::{params, Connection, OpenFlags};
use std::{
    collections::BTreeMap,
    fs::{File, OpenOptions},
    path::{Path, PathBuf},
};

pub(super) const APPLICATION_ID: i32 = 0x48535231;

pub(super) fn sync_file(path: &Path) -> Result<()> {
    let file = File::open(path)?;
    file.sync_all()?;
    #[cfg(target_os = "macos")]
    {
        use std::os::fd::AsRawFd;
        ensure!(
            unsafe { libc::fcntl(file.as_raw_fd(), libc::F_FULLFSYNC) } != -1,
            "Device cache synchronization failed"
        );
    }
    Ok(())
}

pub(super) struct Store {
    pub db: Connection,
    pub committed: u64,
    streams: Vec<StreamDescriptor>,
    sequences: Vec<u64>,
}

impl Store {
    pub fn create(
        path: &Path,
        context: &RecordingContext,
        streams: &[StreamDescriptor],
    ) -> Result<Self> {
        ensure!(!streams.is_empty(), "Select at least one stream");
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .context("Refusing to overwrite an existing recording")?
            .sync_all()?;
        let db = Connection::open(path)?;
        db.busy_timeout(std::time::Duration::ZERO)?;
        db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;
            PRAGMA fullfsync=ON; PRAGMA checkpoint_fullfsync=ON; PRAGMA foreign_keys=ON;
            PRAGMA user_version=1;
            CREATE TABLE recording (id INTEGER PRIMARY KEY CHECK(id=1), state TEXT NOT NULL,
                context_json TEXT NOT NULL, committed_samples INTEGER NOT NULL DEFAULT 0,
                started_utc TEXT NOT NULL, ended_utc TEXT, error TEXT);
            CREATE TABLE streams (id INTEGER PRIMARY KEY, metadata_json TEXT NOT NULL);
            CREATE TABLE blocks (id INTEGER PRIMARY KEY, stream_id INTEGER NOT NULL REFERENCES streams(id),
                first_sequence INTEGER NOT NULL, sample_count INTEGER NOT NULL,
                timestamps BLOB NOT NULL, received BLOB NOT NULL, data BLOB NOT NULL,
                UNIQUE(stream_id, first_sequence));
            CREATE TABLE clock_observations (stream_id INTEGER NOT NULL REFERENCES streams(id),
                observed_lsl REAL NOT NULL, remote_lsl REAL, correction REAL, uncertainty REAL, error TEXT);")?;
        db.pragma_update(None, "application_id", APPLICATION_ID)?;
        db.execute(
            "INSERT INTO recording(id,state,context_json,started_utc) VALUES(1,'recording',?,?)",
            params![
                serde_json::to_string(context)?,
                chrono::Utc::now().to_rfc3339()
            ],
        )?;
        for (id, stream) in streams.iter().enumerate() {
            db.execute(
                "INSERT INTO streams VALUES(?,?)",
                params![id as i64, serde_json::to_string(stream)?],
            )?;
        }
        #[cfg(unix)]
        if let Some(parent) = path.parent() {
            File::open(parent)?.sync_all()?;
        }
        Ok(Self {
            db,
            committed: 0,
            streams: streams.to_vec(),
            sequences: vec![0; streams.len()],
        })
    }

    pub fn commit(&mut self, samples: &[RecordedSample]) -> Result<u64> {
        if samples.is_empty() {
            return Ok(self.committed);
        }
        let mut grouped: BTreeMap<usize, Vec<&RecordedSample>> = BTreeMap::new();
        for sample in samples {
            let spec = self
                .streams
                .get(sample.stream)
                .context("Unknown stream index")?;
            ensure!(
                sample.data.channel_count() == spec.channels as usize,
                "Wrong channel count"
            );
            ensure!(
                sample.format() == spec.format,
                "Sample type does not match stream metadata"
            );
            ensure!(
                sample.timestamp.is_finite() && sample.received.is_finite(),
                "Invalid LSL timestamp"
            );
            grouped.entry(sample.stream).or_default().push(sample);
        }
        let tx = self.db.transaction()?;
        let mut next = self.sequences.clone();
        for (id, rows) in grouped {
            let mut timestamps = Vec::new();
            let mut received = Vec::new();
            let mut data = Vec::new();
            for row in &rows {
                timestamps.extend_from_slice(&row.timestamp.to_le_bytes());
                received.extend_from_slice(&row.received.to_le_bytes());
                let bytes = match &row.data {
                    crate::devices::lsl::types::SampleData::String(v) => serde_json::to_vec(v)?,
                    _ => row.data.to_bytes(),
                };
                let len =
                    u32::try_from(bytes.len()).context("Sample exceeds supported frame size")?;
                data.extend_from_slice(&len.to_le_bytes());
                data.extend(bytes);
            }
            tx.execute("INSERT INTO blocks(stream_id,first_sequence,sample_count,timestamps,received,data) VALUES(?,?,?,?,?,?)",
                params![id as i64,next[id] as i64,rows.len() as i64,timestamps,received,data])?;
            next[id] += rows.len() as u64;
        }
        let committed = self.committed + samples.len() as u64;
        tx.execute(
            "UPDATE recording SET committed_samples=? WHERE id=1",
            [committed as i64],
        )?;
        tx.commit()?; // Counters advance only after the durable transaction succeeds.
        self.committed = committed;
        self.sequences = next;
        Ok(committed)
    }

    pub fn finish(self, path: &Path, state: &str) -> Result<()> {
        self.db
            .execute("UPDATE recording SET state='finalizing' WHERE id=1", [])?;
        let busy: i64 = self
            .db
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| r.get(0))?;
        ensure!(
            busy == 0,
            "Recording checkpoint is busy; retain database and WAL together"
        );
        sync_file(path)?;
        // The final state is a FULL/fullfsync rollback-journal transaction in
        // the standalone file, after WAL consolidation. Failed consolidation
        // therefore cannot leave a completed marker behind.
        let mode: String = self
            .db
            .query_row("PRAGMA journal_mode=DELETE", [], |r| r.get(0))?;
        ensure!(mode == "delete", "Could not finalize standalone recording");
        self.db.execute(
            "UPDATE recording SET state=?,ended_utc=? WHERE id=1",
            params![state, chrono::Utc::now().to_rfc3339()],
        )?;
        drop(self);
        Ok(())
    }
}

/// Recover into a new database, retaining the original file and its WAL untouched.
pub fn recover(path: &Path) -> Result<PathBuf> {
    let source = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let app: i32 = source.pragma_query_value(None, "application_id", |r| r.get(0))?;
    let version: i32 = source.pragma_query_value(None, "user_version", |r| r.get(0))?;
    ensure!(
        app == APPLICATION_ID && version == 1,
        "Not a supported HyperStudy recording"
    );
    let integrity: String = source.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
    ensure!(integrity == "ok", "Recording integrity check failed");
    let target = path.with_file_name(format!("recovered-{}.sqlite", uuid::Uuid::new_v4()));
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&target)?;
    let mut dest = Connection::open(&target)?;
    dest.execute_batch("PRAGMA synchronous=FULL; PRAGMA fullfsync=ON;")?;
    {
        let backup = rusqlite::backup::Backup::new(&source, &mut dest)?;
        ensure!(
            backup.step(-1)? == rusqlite::backup::StepResult::Done,
            "Recovery source is busy"
        );
    }
    dest.execute(
        "UPDATE recording SET state='interrupted',ended_utc=? WHERE id=1",
        [chrono::Utc::now().to_rfc3339()],
    )?;
    drop(dest);
    sync_file(&target)?;
    #[cfg(unix)]
    if let Some(parent) = target.parent() {
        File::open(parent)?.sync_all()?;
    }
    Ok(target)
}

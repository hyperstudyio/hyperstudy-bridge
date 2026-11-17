//! Log storage operations: batch inserts and queries with filtering, pagination and search.
//!
//! These functions run on the database thread with its connection; async callers reach them through the
//! methods on [`super::Storage`]. Batching itself happens before this point, in `crate::logging::LogPersister`.

use super::StorageResult;
use crate::logging::LogEntry;
use rusqlite::{params, Connection};

/// Options for querying logs.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct LogQueryOptions {
    /// Maximum number of logs to return.
    pub limit: Option<i64>,
    /// Number of logs to skip (for pagination).
    pub offset: Option<i64>,
    /// Filter by log level (exact match).
    pub level: Option<String>,
    /// Filter by device (exact match).
    pub device: Option<String>,
    /// Search term to match against message (case-insensitive LIKE).
    pub search: Option<String>,
    /// Filter logs after this timestamp (ISO 8601).
    pub from_timestamp: Option<String>,
    /// Filter logs before this timestamp (ISO 8601).
    pub to_timestamp: Option<String>,
    /// Filter by session ID.
    pub session_id: Option<String>,
}

/// Result of a paginated log query.
#[derive(Debug, Clone, serde::Serialize)]
pub struct LogQueryResult {
    /// The log entries.
    pub logs: Vec<StoredLogEntry>,
    /// Total count of matching logs (before pagination).
    pub total_count: i64,
    /// Whether there are more logs available.
    pub has_more: bool,
}

/// A log entry as stored in the database.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct StoredLogEntry {
    pub id: i64,
    pub session_id: Option<String>,
    pub timestamp: String,
    pub level: String,
    pub message: String,
    pub device: Option<String>,
    pub source: String,
}

impl StoredLogEntry {
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            session_id: row.get(1)?,
            timestamp: row.get(2)?,
            level: row.get(3)?,
            message: row.get(4)?,
            device: row.get(5)?,
            source: row.get(6)?,
        })
    }
}

/// The filters shared by the count and the page query. Each `(? IS NULL OR column ...)` pair takes the same
/// value twice, so an absent filter matches everything. All values are bound parameters.
const FILTERS: &str = r#"
    WHERE (?1 IS NULL OR level = ?1)
      AND (?2 IS NULL OR device = ?2)
      AND (?3 IS NULL OR message LIKE ?3 ESCAPE '\')
      AND (?4 IS NULL OR timestamp >= ?4)
      AND (?5 IS NULL OR timestamp <= ?5)
      AND (?6 IS NULL OR session_id = ?6)
"#;

/// Query logs from the database with optional filtering and pagination, newest first.
///
/// Uses parameterized queries to prevent SQL injection attacks.
/// All filter parameters are properly bound as query parameters.
pub fn query_logs(conn: &Connection, options: &LogQueryOptions) -> StorageResult<LogQueryResult> {
    let limit = options.limit.unwrap_or(100).clamp(1, 10000);
    let offset = options.offset.unwrap_or(0).max(0);

    // Prepare search pattern if provided (escape SQL LIKE wildcards)
    let search_pattern = options.search.as_ref().map(|s| {
        let escaped = s
            .replace('\\', "\\\\") // Escape backslash first
            .replace('%', "\\%") // Escape percent
            .replace('_', "\\_"); // Escape underscore
        format!("%{}%", escaped)
    });

    let filters = params![
        options.level,
        options.device,
        search_pattern,
        options.from_timestamp,
        options.to_timestamp,
        options.session_id,
    ];

    let total_count: i64 = conn.query_row(
        &format!("SELECT COUNT(*) FROM logs {FILTERS}"),
        filters,
        |row| row.get(0),
    )?;

    let mut statement = conn.prepare(&format!(
        "SELECT id, session_id, timestamp, level, message, device, source
         FROM logs {FILTERS}
         ORDER BY timestamp DESC
         LIMIT ?7 OFFSET ?8"
    ))?;
    let page = params![
        options.level,
        options.device,
        search_pattern,
        options.from_timestamp,
        options.to_timestamp,
        options.session_id,
        limit,
        offset,
    ];
    let logs = statement
        .query_map(page, StoredLogEntry::from_row)?
        .collect::<Result<Vec<_>, _>>()?;

    let has_more = offset + (logs.len() as i64) < total_count;

    Ok(LogQueryResult {
        logs,
        total_count,
        has_more,
    })
}

/// Get log count by level (for dashboard statistics).
pub fn get_log_counts_by_level(conn: &Connection) -> StorageResult<Vec<(String, i64)>> {
    let mut statement = conn.prepare("SELECT level, COUNT(*) as count FROM logs GROUP BY level")?;
    let counts = statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(counts)
}

/// Insert log entries in one transaction.
pub fn insert_logs_batch(
    conn: &mut Connection,
    entries: &[LogEntry],
    session_id: Option<&str>,
) -> StorageResult<()> {
    if entries.is_empty() {
        return Ok(());
    }

    let tx = conn.transaction()?;
    {
        let mut insert = tx.prepare_cached(
            "INSERT INTO logs (session_id, timestamp, level, message, device, source)
             VALUES (?, ?, ?, ?, ?, ?)",
        )?;
        for entry in entries {
            insert.execute(params![
                session_id,
                entry.timestamp,
                entry.level,
                entry.message,
                entry.device,
                entry.source,
            ])?;
        }
    }
    tx.commit()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::Storage;

    async fn storage() -> Storage {
        Storage::new_in_memory().await.unwrap()
    }

    fn create_test_entry(level: &str, message: &str) -> LogEntry {
        LogEntry {
            timestamp: chrono::Utc::now().to_rfc3339(),
            level: level.to_string(),
            message: message.to_string(),
            device: None,
            source: "test".to_string(),
        }
    }

    async fn count_matching(storage: &Storage, options: LogQueryOptions) -> i64 {
        storage.query_logs(options).await.unwrap().total_count
    }

    #[tokio::test]
    async fn test_insert_and_query_logs() {
        let storage = storage().await;

        // Insert some logs
        let entries = vec![
            create_test_entry("info", "Test message 1"),
            create_test_entry("warn", "Test warning"),
            create_test_entry("error", "Test error"),
        ];

        storage.insert_logs_batch(entries, None).await.unwrap();

        // Query all logs
        let result = storage
            .query_logs(LogQueryOptions::default())
            .await
            .unwrap();
        assert_eq!(result.total_count, 3);
        assert_eq!(result.logs.len(), 3);
    }

    #[tokio::test]
    async fn test_filter_by_level() {
        let storage = storage().await;

        let entries = vec![
            create_test_entry("info", "Info message"),
            create_test_entry("warn", "Warning message"),
            create_test_entry("error", "Error message"),
        ];

        storage.insert_logs_batch(entries, None).await.unwrap();

        // Filter by level
        let result = storage
            .query_logs(LogQueryOptions {
                level: Some("error".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();

        assert_eq!(result.total_count, 1);
        assert_eq!(result.logs[0].level, "error");
    }

    #[tokio::test]
    async fn test_search_logs() {
        let storage = storage().await;

        let entries = vec![
            create_test_entry("info", "Connection established"),
            create_test_entry("info", "Data received"),
            create_test_entry("error", "Connection failed"),
        ];

        storage.insert_logs_batch(entries, None).await.unwrap();

        // Search for "Connection"
        let matching = count_matching(
            &storage,
            LogQueryOptions {
                search: Some("Connection".to_string()),
                ..Default::default()
            },
        )
        .await;
        assert_eq!(matching, 2);
    }

    #[tokio::test]
    async fn test_pagination() {
        let storage = storage().await;

        // Insert 10 logs
        let entries: Vec<LogEntry> = (0..10)
            .map(|i| create_test_entry("info", &format!("Message {}", i)))
            .collect();

        storage.insert_logs_batch(entries, None).await.unwrap();

        // Query with limit and offset
        let result = storage
            .query_logs(LogQueryOptions {
                limit: Some(3),
                offset: Some(2),
                ..Default::default()
            })
            .await
            .unwrap();

        assert_eq!(result.total_count, 10);
        assert_eq!(result.logs.len(), 3);
        assert!(result.has_more);
    }

    #[tokio::test]
    async fn test_log_counts_by_level() {
        let storage = storage().await;

        let entries = vec![
            create_test_entry("info", "Info 1"),
            create_test_entry("info", "Info 2"),
            create_test_entry("warn", "Warning"),
            create_test_entry("error", "Error"),
        ];

        storage.insert_logs_batch(entries, None).await.unwrap();

        let counts = storage.log_counts_by_level().await.unwrap();
        let counts_map: std::collections::HashMap<_, _> = counts.into_iter().collect();

        assert_eq!(counts_map.get("info"), Some(&2));
        assert_eq!(counts_map.get("warn"), Some(&1));
        assert_eq!(counts_map.get("error"), Some(&1));
    }

    #[tokio::test]
    async fn test_sql_injection_prevention() {
        let storage = storage().await;

        // Insert a test entry
        let entries = vec![create_test_entry("info", "Normal message")];
        storage.insert_logs_batch(entries, None).await.unwrap();

        // Attempt SQL injection through various parameters
        // These should NOT cause errors or return unexpected results

        // SQL injection attempt in level filter
        let matching = count_matching(
            &storage,
            LogQueryOptions {
                level: Some("info' OR '1'='1".to_string()),
                ..Default::default()
            },
        )
        .await;
        assert_eq!(matching, 0); // Should find nothing, not all entries

        // SQL injection attempt in search
        let matching = count_matching(
            &storage,
            LogQueryOptions {
                search: Some("'; DROP TABLE logs; --".to_string()),
                ..Default::default()
            },
        )
        .await;
        assert_eq!(matching, 0); // Should find nothing

        // Verify table still exists
        assert_eq!(
            count_matching(&storage, LogQueryOptions::default()).await,
            1
        ); // Original entry still exists

        // SQL injection attempt with wildcards in search
        let matching = count_matching(
            &storage,
            LogQueryOptions {
                search: Some("%".to_string()),
                ..Default::default()
            },
        )
        .await;
        // Should treat % literally, not as wildcard (escaped)
        assert_eq!(matching, 0);

        // Verify normal search still works
        let matching = count_matching(
            &storage,
            LogQueryOptions {
                search: Some("Normal".to_string()),
                ..Default::default()
            },
        )
        .await;
        assert_eq!(matching, 1);
    }

    #[tokio::test]
    async fn test_search_with_special_characters() {
        let storage = storage().await;

        // Insert entries with special characters
        let entries = vec![
            create_test_entry("info", "Message with 100% success"),
            create_test_entry("info", "Query like SELECT * FROM table"),
            create_test_entry("info", "Path: C:\\Users\\test"),
        ];
        storage.insert_logs_batch(entries, None).await.unwrap();

        // Search for literal %
        let matching = count_matching(
            &storage,
            LogQueryOptions {
                search: Some("100%".to_string()),
                ..Default::default()
            },
        )
        .await;
        assert_eq!(matching, 1);

        // Search for backslash
        let matching = count_matching(
            &storage,
            LogQueryOptions {
                search: Some("C:\\".to_string()),
                ..Default::default()
            },
        )
        .await;
        assert_eq!(matching, 1);
    }

    /// Twelve entries a minute apart (index 0 newest), across two sessions, three devices and three levels.
    fn filter_fixture() -> (Vec<LogEntry>, Vec<LogEntry>) {
        let now = chrono::Utc::now();
        let entry = |i: i64| LogEntry {
            timestamp: (now - chrono::Duration::minutes(i)).to_rfc3339(),
            level: ["info", "warn", "error"][(i % 3) as usize].to_string(),
            message: format!(
                "entry {i:02} {}",
                if i % 4 == 0 { "Connection lost" } else { "ok" }
            ),
            device: [Some("ttl"), Some("kernel"), None][(i % 3) as usize].map(String::from),
            source: "test".to_string(),
        };
        let first: Vec<LogEntry> = (0..12).filter(|i| i % 2 == 0).map(entry).collect();
        let second: Vec<LogEntry> = (0..12).filter(|i| i % 2 == 1).map(entry).collect();
        (first, second)
    }

    fn messages(result: &LogQueryResult) -> Vec<String> {
        result
            .logs
            .iter()
            .map(|l| l.message[..8].to_string())
            .collect()
    }

    struct QueryFixture {
        storage: Storage,
        second_session: String,
    }

    impl QueryFixture {
        async fn new(first: Vec<LogEntry>, second: Vec<LogEntry>) -> Self {
            let storage = storage().await;
            let first_session = storage.start_session(None).await.unwrap();
            storage
                .insert_logs_batch(first, Some(first_session))
                .await
                .unwrap();
            let second_session = storage.start_session(None).await.unwrap();
            storage
                .insert_logs_batch(second, Some(second_session.clone()))
                .await
                .unwrap();
            Self {
                storage,
                second_session,
            }
        }

        async fn query(&self, options: LogQueryOptions) -> LogQueryResult {
            self.storage.query_logs(options).await.unwrap()
        }
    }

    /// query_logs returns newest first, pages with limit/offset, reports total_count before paging and has_more,
    /// and combines the level/device/search/time/session filters with AND. Written against the sqlx version
    /// first (it passed there unchanged apart from the call syntax), so it pins the behaviour across the port.
    #[tokio::test]
    async fn query_logs_filters_and_pages_newest_first() {
        let (first, second) = filter_fixture();
        let fx = QueryFixture::new(first.clone(), second.clone()).await;

        let page = fx
            .query(LogQueryOptions {
                limit: Some(5),
                offset: Some(0),
                ..Default::default()
            })
            .await;
        assert_eq!(
            messages(&page),
            ["entry 00", "entry 01", "entry 02", "entry 03", "entry 04"]
        );
        assert_eq!((page.total_count, page.has_more), (12, true));
        let page = fx
            .query(LogQueryOptions {
                limit: Some(5),
                offset: Some(10),
                ..Default::default()
            })
            .await;
        assert_eq!(messages(&page), ["entry 10", "entry 11"]);
        assert_eq!((page.total_count, page.has_more), (12, false));
        let page = fx
            .query(LogQueryOptions {
                limit: Some(5),
                offset: Some(12),
                ..Default::default()
            })
            .await;
        assert!(page.logs.is_empty());
        assert_eq!((page.total_count, page.has_more), (12, false));

        // Out-of-range paging is clamped: limit to 1..=10000, offset to >= 0.
        let page = fx
            .query(LogQueryOptions {
                limit: Some(0),
                offset: Some(-3),
                ..Default::default()
            })
            .await;
        assert_eq!(messages(&page), ["entry 00"]);
        assert!(page.has_more);

        let page = fx
            .query(LogQueryOptions {
                level: Some("warn".into()),
                ..Default::default()
            })
            .await;
        assert_eq!(
            messages(&page),
            ["entry 01", "entry 04", "entry 07", "entry 10"]
        );
        let page = fx
            .query(LogQueryOptions {
                device: Some("ttl".into()),
                ..Default::default()
            })
            .await;
        assert_eq!(
            messages(&page),
            ["entry 00", "entry 03", "entry 06", "entry 09"]
        );
        let page = fx
            .query(LogQueryOptions {
                search: Some("connection LOST".into()),
                ..Default::default()
            })
            .await;
        assert_eq!(
            messages(&page),
            ["entry 00", "entry 04", "entry 08"],
            "search is case-insensitive"
        );
        let page = fx
            .query(LogQueryOptions {
                level: Some("info".into()),
                device: Some("ttl".into()),
                search: Some("Connection".into()),
                ..Default::default()
            })
            .await;
        assert_eq!(messages(&page), ["entry 00"]);
        assert_eq!((page.total_count, page.has_more), (1, false));

        let page = fx
            .query(LogQueryOptions {
                from_timestamp: Some(first[3].timestamp.clone()), // entry 06
                to_timestamp: Some(second[1].timestamp.clone()),  // entry 03
                ..Default::default()
            })
            .await;
        assert_eq!(
            messages(&page),
            ["entry 03", "entry 04", "entry 05", "entry 06"],
            "time bounds are inclusive"
        );

        let page = fx
            .query(LogQueryOptions {
                session_id: Some(fx.second_session.clone()),
                limit: Some(2),
                ..Default::default()
            })
            .await;
        assert_eq!(messages(&page), ["entry 01", "entry 03"]);
        assert_eq!((page.total_count, page.has_more), (6, true));
        assert!(page
            .logs
            .iter()
            .all(|l| l.session_id.as_deref() == Some(fx.second_session.as_str())));
        let stored = &page.logs[0];
        assert_eq!(
            (
                stored.level.as_str(),
                stored.device.as_deref(),
                stored.source.as_str()
            ),
            ("warn", Some("kernel"), "test")
        );
        assert_eq!(stored.timestamp, second[0].timestamp);

        let page = fx
            .query(LogQueryOptions {
                level: Some("nope".into()),
                ..Default::default()
            })
            .await;
        assert!(page.logs.is_empty());
        assert_eq!((page.total_count, page.has_more), (0, false));
    }
}

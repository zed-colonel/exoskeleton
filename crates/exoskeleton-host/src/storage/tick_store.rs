//! SQLite-backed tick record store.

use std::path::Path;
use std::sync::Mutex;

use exoskeleton_core::{ExoError, TickId, TickRecord, TickStore};
use rusqlite::Connection;

/// SQLite-backed tick record store.
///
/// Each completed tick produces a `TickRecord`. Keyed by `tick_id` (UUID PK)
/// with a unique constraint on `tick_number`.
///
/// The full `TickRecord` is serialized as JSON bytes in a BLOB column, same
/// pattern as the Snapshot Store.
///
/// SQLite pragmas: WAL mode, `synchronous = FULL`, `busy_timeout = 5000`.
pub struct SqliteTickStore {
    conn: Mutex<Connection>,
}

impl SqliteTickStore {
    /// Open or create a tick store at the given path.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, ExoError> {
        let conn = Connection::open(path.as_ref())
            .map_err(|e| ExoError::Storage(format!("tick store open: {e}")))?;
        Self::initialize(conn)
    }

    /// Create an in-memory tick store (for testing).
    pub fn in_memory() -> Result<Self, ExoError> {
        let conn = Connection::open_in_memory()
            .map_err(|e| ExoError::Storage(format!("tick store in-memory: {e}")))?;
        Self::initialize(conn)
    }

    fn initialize(conn: Connection) -> Result<Self, ExoError> {
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA busy_timeout = 5000;
             PRAGMA synchronous = FULL;",
        )
        .map_err(|e| ExoError::Storage(format!("tick store pragmas: {e}")))?;

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS ticks (
                tick_id      TEXT NOT NULL PRIMARY KEY,
                tick_number  INTEGER NOT NULL UNIQUE,
                data         BLOB NOT NULL,
                started_at   TEXT NOT NULL,
                completed_at TEXT
            ) STRICT;",
        )
        .map_err(|e| ExoError::Storage(format!("tick store schema: {e}")))?;

        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Deserialize a tick record from a BLOB column.
    fn deserialize(data: &[u8]) -> Result<TickRecord, ExoError> {
        serde_json::from_slice(data)
            .map_err(|e| ExoError::Storage(format!("tick record deserialization: {e}")))
    }
}

impl TickStore for SqliteTickStore {
    fn save(&self, record: &TickRecord) -> Result<(), ExoError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| ExoError::Storage(format!("lock poisoned: {e}")))?;
        let data = serde_json::to_vec(record)
            .map_err(|e| ExoError::Storage(format!("tick record serialization: {e}")))?;

        match conn.execute(
            "INSERT INTO ticks (tick_id, tick_number, data, started_at, completed_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                record.tick_id.to_string(),
                record.tick_number as i64,
                data,
                record.started_at.to_rfc3339(),
                record.completed_at.map(|t| t.to_rfc3339()),
            ],
        ) {
            Ok(_) => Ok(()),
            Err(rusqlite::Error::SqliteFailure(err, _))
                if err.code == rusqlite::ErrorCode::ConstraintViolation =>
            {
                Err(ExoError::Storage(format!(
                    "tick record already exists (tick_id={}, tick_number={})",
                    record.tick_id, record.tick_number
                )))
            }
            Err(e) => Err(ExoError::Storage(format!("tick save: {e}"))),
        }
    }

    fn latest(&self) -> Result<Option<TickRecord>, ExoError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| ExoError::Storage(format!("lock poisoned: {e}")))?;
        let mut stmt = conn
            .prepare("SELECT data FROM ticks ORDER BY tick_number DESC LIMIT 1")
            .map_err(|e| ExoError::Storage(format!("prepare: {e}")))?;

        match stmt.query_row([], |row| {
            let data: Vec<u8> = row.get(0)?;
            Ok(data)
        }) {
            Ok(data) => Self::deserialize(&data).map(Some),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(ExoError::Storage(format!("tick latest: {e}"))),
        }
    }

    fn get(&self, tick_id: TickId) -> Result<Option<TickRecord>, ExoError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| ExoError::Storage(format!("lock poisoned: {e}")))?;
        let mut stmt = conn
            .prepare("SELECT data FROM ticks WHERE tick_id = ?1")
            .map_err(|e| ExoError::Storage(format!("prepare: {e}")))?;

        match stmt.query_row(rusqlite::params![tick_id.to_string()], |row| {
            let data: Vec<u8> = row.get(0)?;
            Ok(data)
        }) {
            Ok(data) => Self::deserialize(&data).map(Some),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(ExoError::Storage(format!("tick get: {e}"))),
        }
    }

    fn range(&self, from_tick: u64, to_tick: u64) -> Result<Vec<TickRecord>, ExoError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| ExoError::Storage(format!("lock poisoned: {e}")))?;
        let mut stmt = conn
            .prepare(
                "SELECT data FROM ticks WHERE tick_number >= ?1 AND tick_number <= ?2
                 ORDER BY tick_number ASC",
            )
            .map_err(|e| ExoError::Storage(format!("prepare: {e}")))?;

        let rows = stmt
            .query_map(rusqlite::params![from_tick as i64, to_tick as i64], |row| {
                let data: Vec<u8> = row.get(0)?;
                Ok(data)
            })
            .map_err(|e| ExoError::Storage(format!("range query: {e}")))?;

        let mut records = Vec::new();
        for row in rows {
            let data = row.map_err(|e| ExoError::Storage(format!("range row: {e}")))?;
            records.push(Self::deserialize(&data)?);
        }
        Ok(records)
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use exoskeleton_core::{
        ActionOutcome, ActionRecord, ArtifactId, LlmCallRecord, ThreadContribution, ThreadId,
        TickPhase,
    };

    use super::*;

    fn make_tick(tick_number: u64) -> TickRecord {
        TickRecord {
            tick_id: TickId::new(),
            tick_number,
            phase: TickPhase::Amend,
            started_at: Utc::now(),
            completed_at: Some(Utc::now()),
            snapshot_before: ArtifactId::from_content(format!("before-{tick_number}").as_bytes()),
            snapshot_after: Some(ArtifactId::from_content(
                format!("after-{tick_number}").as_bytes(),
            )),
            thread_contributions: Vec::new(),
            actions_taken: Vec::new(),
            llm_calls: Vec::new(),
            decision_rationale: None,
            context_breakdown_ref: None,
        }
    }

    // ── T-4: Tick Store ──

    #[test]
    fn save_and_get() {
        let store = SqliteTickStore::in_memory().unwrap();
        let record = make_tick(0);
        let tick_id = record.tick_id;

        store.save(&record).unwrap();
        let retrieved = store.get(tick_id).unwrap().unwrap();
        assert_eq!(retrieved.tick_id, tick_id);
        assert_eq!(retrieved.tick_number, 0);
    }

    #[test]
    fn get_returns_none_for_missing() {
        let store = SqliteTickStore::in_memory().unwrap();
        assert!(store.get(TickId::new()).unwrap().is_none());
    }

    #[test]
    fn latest_returns_most_recent() {
        let store = SqliteTickStore::in_memory().unwrap();
        for t in 0..3 {
            store.save(&make_tick(t)).unwrap();
        }
        let latest = store.latest().unwrap().unwrap();
        assert_eq!(latest.tick_number, 2);
    }

    #[test]
    fn latest_returns_none_when_empty() {
        let store = SqliteTickStore::in_memory().unwrap();
        assert!(store.latest().unwrap().is_none());
    }

    #[test]
    fn save_rejects_duplicate_tick_id() {
        let store = SqliteTickStore::in_memory().unwrap();
        let record = make_tick(0);
        store.save(&record).unwrap();

        // Same tick_id, different tick_number
        let dup = TickRecord {
            tick_number: 99,
            ..record.clone()
        };
        let err = store.save(&dup).unwrap_err();
        assert!(
            matches!(err, ExoError::Storage(ref msg) if msg.contains("already exists")),
            "Expected duplicate rejection, got: {err}"
        );
    }

    #[test]
    fn save_rejects_duplicate_tick_number() {
        let store = SqliteTickStore::in_memory().unwrap();
        store.save(&make_tick(0)).unwrap();

        // Different tick_id, same tick_number
        let dup = make_tick(0); // new tick_id, same tick_number=0
        let err = store.save(&dup).unwrap_err();
        assert!(
            matches!(err, ExoError::Storage(ref msg) if msg.contains("already exists")),
            "Expected duplicate rejection, got: {err}"
        );
    }

    #[test]
    fn range_returns_inclusive() {
        let store = SqliteTickStore::in_memory().unwrap();
        for t in 0..5 {
            store.save(&make_tick(t)).unwrap();
        }
        let range = store.range(1, 3).unwrap();
        assert_eq!(range.len(), 3);
        assert_eq!(range[0].tick_number, 1);
        assert_eq!(range[1].tick_number, 2);
        assert_eq!(range[2].tick_number, 3);
    }

    #[test]
    fn range_returns_oldest_first() {
        let store = SqliteTickStore::in_memory().unwrap();
        for t in 0..5 {
            store.save(&make_tick(t)).unwrap();
        }
        let range = store.range(0, 4).unwrap();
        assert_eq!(range.len(), 5);
        for (i, rec) in range.iter().enumerate() {
            assert_eq!(rec.tick_number, i as u64);
        }
    }

    #[test]
    fn range_empty_for_missing_range() {
        let store = SqliteTickStore::in_memory().unwrap();
        store.save(&make_tick(0)).unwrap();
        let range = store.range(5, 10).unwrap();
        assert!(range.is_empty());
    }

    #[test]
    fn tick_record_complex_roundtrip() {
        let store = SqliteTickStore::in_memory().unwrap();
        let record = TickRecord {
            tick_id: TickId::new(),
            tick_number: 42,
            phase: TickPhase::Amend,
            started_at: Utc::now(),
            completed_at: Some(Utc::now()),
            snapshot_before: ArtifactId::from_content(b"before"),
            snapshot_after: Some(ArtifactId::from_content(b"after")),
            thread_contributions: vec![ThreadContribution {
                thread_id: ThreadId::new(),
                artifact_id: ArtifactId::from_content(b"thread out"),
                summary: "Analysis complete".into(),
            }],
            actions_taken: vec![ActionRecord {
                action_type: "fs.write".into(),
                target: "/tmp/output.txt".into(),
                receipt_ref: Some(ArtifactId::from_content(b"receipt")),
                outcome: ActionOutcome::Success,
            }],
            llm_calls: vec![LlmCallRecord {
                model: "local-7b".into(),
                tokens_in: 1000,
                tokens_out: 500,
                cost_cents: 0.0,
                latency_ms: 250,
                response_artifact_ref: Some(ArtifactId::from_content(b"llm response")),
            }],
            decision_rationale: Some("Decided to write output file".into()),
            context_breakdown_ref: None,
        };

        store.save(&record).unwrap();
        let retrieved = store.get(record.tick_id).unwrap().unwrap();
        assert_eq!(record, retrieved);
    }
}

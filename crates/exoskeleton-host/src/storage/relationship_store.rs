//! SQLite-backed relationship ledger.
//!
//! Append-only store for relationship records (I8: durable and explicit).
//! Records are never modified or deleted (IBP §4.1).

use std::path::Path;
use std::sync::Mutex;

use chrono::{DateTime, Utc};
use exoskeleton_core::{
    ArtifactId, ExoError, LedgerEntryId, PrincipalId, RelationalSignalType, RelationshipRecord,
    TickId,
};
use exoskeleton_relationship::RelationshipLedger;
use rusqlite::Connection;

/// SQLite-backed append-only relationship ledger.
///
/// Each `append()` is a single INSERT wrapped in a transaction (atomic, IBP §4.1).
/// No UPDATE or DELETE operations exist.
///
/// SQLite pragmas: WAL mode, `synchronous = FULL`, `busy_timeout = 5000`.
pub struct SqliteRelationshipLedger {
    conn: Mutex<Connection>,
}

impl SqliteRelationshipLedger {
    /// Open or create a relationship ledger at the given path.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, ExoError> {
        let conn = Connection::open(path.as_ref())
            .map_err(|e| ExoError::Storage(format!("relationship ledger open: {e}")))?;
        Self::initialize(conn)
    }

    /// Create an in-memory relationship ledger (for testing).
    pub fn in_memory() -> Result<Self, ExoError> {
        let conn = Connection::open_in_memory()
            .map_err(|e| ExoError::Storage(format!("relationship ledger in-memory: {e}")))?;
        Self::initialize(conn)
    }

    fn initialize(conn: Connection) -> Result<Self, ExoError> {
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA busy_timeout = 5000;
             PRAGMA synchronous = FULL;",
        )
        .map_err(|e| ExoError::Storage(format!("relationship ledger pragmas: {e}")))?;

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS relationship_ledger (
                id           TEXT NOT NULL PRIMARY KEY,
                principal_id TEXT NOT NULL,
                signal_type  TEXT NOT NULL,
                content_ref  TEXT NOT NULL,
                tick_id      TEXT NOT NULL,
                timestamp    TEXT NOT NULL,
                metadata     TEXT NOT NULL DEFAULT '{}'
            ) STRICT;

            CREATE INDEX IF NOT EXISTS idx_rl_principal
                ON relationship_ledger(principal_id);
            CREATE INDEX IF NOT EXISTS idx_rl_tick
                ON relationship_ledger(tick_id);
            CREATE INDEX IF NOT EXISTS idx_rl_timestamp
                ON relationship_ledger(timestamp DESC);",
        )
        .map_err(|e| ExoError::Storage(format!("relationship ledger schema: {e}")))?;

        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Parse a single row into a RelationshipRecord.
    fn row_to_record(row: &rusqlite::Row<'_>) -> Result<RelationshipRecord, rusqlite::Error> {
        let id_str: String = row.get(0)?;
        let principal_id_str: String = row.get(1)?;
        let signal_type_str: String = row.get(2)?;
        let content_ref_str: String = row.get(3)?;
        let tick_id_str: String = row.get(4)?;
        let timestamp_str: String = row.get(5)?;
        let metadata_str: String = row.get(6)?;

        let id: LedgerEntryId = id_str.parse().map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
        })?;
        let principal_id: PrincipalId = principal_id_str.parse().map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(1, rusqlite::types::Type::Text, Box::new(e))
        })?;
        let signal_type: RelationalSignalType =
            serde_json::from_str(&signal_type_str).map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    2,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })?;
        let content_ref: ArtifactId = content_ref_str.parse().map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(3, rusqlite::types::Type::Text, Box::new(e))
        })?;
        let tick_id: TickId = tick_id_str.parse().map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(4, rusqlite::types::Type::Text, Box::new(e))
        })?;
        let timestamp: DateTime<Utc> = DateTime::parse_from_rfc3339(&timestamp_str)
            .map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    5,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })?
            .with_timezone(&Utc);

        let metadata: std::collections::HashMap<String, String> =
            serde_json::from_str(&metadata_str).unwrap_or_default();

        Ok(RelationshipRecord {
            id,
            principal_id,
            signal_type,
            content_ref,
            tick_id,
            timestamp,
            metadata,
        })
    }
}

impl RelationshipLedger for SqliteRelationshipLedger {
    fn append(&self, record: &RelationshipRecord) -> Result<LedgerEntryId, ExoError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| ExoError::Storage(format!("lock poisoned: {e}")))?;

        let signal_type_str = serde_json::to_string(&record.signal_type)
            .map_err(|e| ExoError::Storage(format!("signal_type serialization: {e}")))?;

        let metadata_str = serde_json::to_string(&record.metadata)
            .map_err(|e| ExoError::Storage(format!("metadata serialization: {e}")))?;

        match conn.execute(
            "INSERT INTO relationship_ledger (id, principal_id, signal_type, content_ref, tick_id, timestamp, metadata)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                record.id.to_string(),
                record.principal_id.to_string(),
                signal_type_str,
                record.content_ref.as_str(),
                record.tick_id.to_string(),
                record.timestamp.to_rfc3339(),
                metadata_str,
            ],
        ) {
            Ok(_) => Ok(record.id),
            Err(rusqlite::Error::SqliteFailure(err, _))
                if err.code == rusqlite::ErrorCode::ConstraintViolation =>
            {
                Err(ExoError::Storage(format!(
                    "relationship record already exists with id {}",
                    record.id
                )))
            }
            Err(e) => Err(ExoError::Storage(format!("relationship append: {e}"))),
        }
    }

    fn for_principal(
        &self,
        principal_id: PrincipalId,
        limit: usize,
    ) -> Result<Vec<RelationshipRecord>, ExoError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| ExoError::Storage(format!("lock poisoned: {e}")))?;

        let mut stmt = conn
            .prepare(
                "SELECT id, principal_id, signal_type, content_ref, tick_id, timestamp, metadata
                 FROM relationship_ledger
                 WHERE principal_id = ?1
                 ORDER BY timestamp DESC LIMIT ?2",
            )
            .map_err(|e| ExoError::Storage(format!("prepare: {e}")))?;

        // Cap limit to i32::MAX to avoid SQLite out-of-range errors when
        // callers pass usize::MAX (e.g., compile_relationship_snapshot).
        let limit_capped = std::cmp::min(limit, i32::MAX as usize) as i32;
        let rows = stmt
            .query_map(
                rusqlite::params![principal_id.to_string(), limit_capped],
                Self::row_to_record,
            )
            .map_err(|e| ExoError::Storage(format!("for_principal query: {e}")))?;

        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| ExoError::Storage(format!("for_principal row: {e}")))
    }

    fn recent(&self, limit: usize) -> Result<Vec<RelationshipRecord>, ExoError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| ExoError::Storage(format!("lock poisoned: {e}")))?;

        let mut stmt = conn
            .prepare(
                "SELECT id, principal_id, signal_type, content_ref, tick_id, timestamp, metadata
                 FROM relationship_ledger
                 ORDER BY timestamp DESC LIMIT ?1",
            )
            .map_err(|e| ExoError::Storage(format!("prepare: {e}")))?;

        let limit_capped = std::cmp::min(limit, i32::MAX as usize) as i32;
        let rows = stmt
            .query_map(rusqlite::params![limit_capped], Self::row_to_record)
            .map_err(|e| ExoError::Storage(format!("recent query: {e}")))?;

        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| ExoError::Storage(format!("recent row: {e}")))
    }

    fn since_tick(&self, tick_id: TickId) -> Result<Vec<RelationshipRecord>, ExoError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| ExoError::Storage(format!("lock poisoned: {e}")))?;

        // Find the earliest timestamp for the given tick_id, then return all
        // records from that point onward (oldest first).
        let mut stmt = conn
            .prepare(
                "SELECT id, principal_id, signal_type, content_ref, tick_id, timestamp, metadata
                 FROM relationship_ledger
                 WHERE timestamp >= (
                     SELECT MIN(timestamp) FROM relationship_ledger WHERE tick_id = ?1
                 )
                 ORDER BY timestamp ASC",
            )
            .map_err(|e| ExoError::Storage(format!("prepare: {e}")))?;

        let rows = stmt
            .query_map(rusqlite::params![tick_id.to_string()], Self::row_to_record)
            .map_err(|e| ExoError::Storage(format!("since_tick query: {e}")))?;

        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| ExoError::Storage(format!("since_tick row: {e}")))
    }

    fn distinct_principals(&self) -> Result<Vec<PrincipalId>, ExoError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| ExoError::Storage(format!("lock poisoned: {e}")))?;

        let mut stmt = conn
            .prepare("SELECT DISTINCT principal_id FROM relationship_ledger")
            .map_err(|e| ExoError::Storage(format!("prepare: {e}")))?;

        let rows = stmt
            .query_map([], |row| {
                let id_str: String = row.get(0)?;
                id_str.parse::<PrincipalId>().map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        0,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })
            })
            .map_err(|e| ExoError::Storage(format!("distinct_principals query: {e}")))?;

        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| ExoError::Storage(format!("distinct_principals row: {e}")))
    }

    fn count(&self) -> Result<u64, ExoError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| ExoError::Storage(format!("lock poisoned: {e}")))?;

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM relationship_ledger", [], |row| {
                row.get(0)
            })
            .map_err(|e| ExoError::Storage(format!("count query: {e}")))?;

        Ok(count as u64)
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use exoskeleton_core::{ArtifactId, RelationalSignalType};

    use super::*;

    fn make_record(
        principal_id: PrincipalId,
        signal_type: RelationalSignalType,
    ) -> RelationshipRecord {
        RelationshipRecord {
            id: LedgerEntryId::new(),
            principal_id,
            signal_type,
            content_ref: ArtifactId::from_content(b"test-signal"),
            tick_id: TickId::new(),
            timestamp: Utc::now(),
            metadata: Default::default(),
        }
    }

    // ── T-5: SqliteRelationshipLedger ──

    #[test]
    fn open_creates_database_and_table() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("relationships.db");
        let _ledger = SqliteRelationshipLedger::open(&path).unwrap();
        assert!(path.exists());
    }

    #[test]
    fn append_and_retrieve_single() {
        let ledger = SqliteRelationshipLedger::in_memory().unwrap();
        let p = PrincipalId::new();
        let record = make_record(p, RelationalSignalType::TrustUpdate);
        let id = ledger.append(&record).unwrap();
        assert_eq!(id, record.id);

        let recent = ledger.recent(1).unwrap();
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].id, record.id);
        assert_eq!(recent[0].principal_id, p);
        assert_eq!(recent[0].signal_type, RelationalSignalType::TrustUpdate);
    }

    #[test]
    fn for_principal_filters_correctly() {
        let ledger = SqliteRelationshipLedger::in_memory().unwrap();
        let p1 = PrincipalId::new();
        let p2 = PrincipalId::new();

        ledger
            .append(&make_record(p1, RelationalSignalType::TrustUpdate))
            .unwrap();
        ledger
            .append(&make_record(p2, RelationalSignalType::FeedbackReceived))
            .unwrap();
        ledger
            .append(&make_record(p1, RelationalSignalType::CommitmentMade))
            .unwrap();

        let p1_records = ledger.for_principal(p1, 10).unwrap();
        assert_eq!(p1_records.len(), 2);
        for r in &p1_records {
            assert_eq!(r.principal_id, p1);
        }
    }

    #[test]
    fn recent_returns_newest_first() {
        let ledger = SqliteRelationshipLedger::in_memory().unwrap();
        let p = PrincipalId::new();
        let ts_base = Utc::now();

        for i in 0..5 {
            let mut record = make_record(p, RelationalSignalType::TrustUpdate);
            record.timestamp = ts_base + chrono::Duration::seconds(i);
            ledger.append(&record).unwrap();
        }

        let recent = ledger.recent(3).unwrap();
        assert_eq!(recent.len(), 3);
        assert!(recent[0].timestamp >= recent[1].timestamp);
        assert!(recent[1].timestamp >= recent[2].timestamp);
    }

    #[test]
    fn since_tick_returns_records_from_tick_onward() {
        let ledger = SqliteRelationshipLedger::in_memory().unwrap();
        let p = PrincipalId::new();
        let tick1 = TickId::new();
        let tick2 = TickId::new();
        let tick3 = TickId::new();
        let ts_base = Utc::now();

        let mut r1 = make_record(p, RelationalSignalType::TrustUpdate);
        r1.tick_id = tick1;
        r1.timestamp = ts_base;
        ledger.append(&r1).unwrap();

        let mut r2 = make_record(p, RelationalSignalType::CommitmentMade);
        r2.tick_id = tick2;
        r2.timestamp = ts_base + chrono::Duration::seconds(10);
        ledger.append(&r2).unwrap();

        let mut r3 = make_record(p, RelationalSignalType::FeedbackReceived);
        r3.tick_id = tick3;
        r3.timestamp = ts_base + chrono::Duration::seconds(20);
        ledger.append(&r3).unwrap();

        let since = ledger.since_tick(tick2).unwrap();
        assert_eq!(since.len(), 2);
        assert_eq!(since[0].tick_id, tick2);
        assert_eq!(since[1].tick_id, tick3);
    }

    #[test]
    fn distinct_principals_returns_unique() {
        let ledger = SqliteRelationshipLedger::in_memory().unwrap();
        let p1 = PrincipalId::new();
        let p2 = PrincipalId::new();

        ledger
            .append(&make_record(p1, RelationalSignalType::TrustUpdate))
            .unwrap();
        ledger
            .append(&make_record(p1, RelationalSignalType::CommitmentMade))
            .unwrap();
        ledger
            .append(&make_record(p2, RelationalSignalType::FeedbackReceived))
            .unwrap();

        let principals = ledger.distinct_principals().unwrap();
        assert_eq!(principals.len(), 2);
        assert!(principals.contains(&p1));
        assert!(principals.contains(&p2));
    }

    #[test]
    fn count_reflects_total() {
        let ledger = SqliteRelationshipLedger::in_memory().unwrap();
        assert_eq!(ledger.count().unwrap(), 0);

        let p = PrincipalId::new();
        ledger
            .append(&make_record(p, RelationalSignalType::TrustUpdate))
            .unwrap();
        assert_eq!(ledger.count().unwrap(), 1);

        ledger
            .append(&make_record(p, RelationalSignalType::CommitmentMade))
            .unwrap();
        assert_eq!(ledger.count().unwrap(), 2);
    }

    #[test]
    fn duplicate_id_returns_error() {
        let ledger = SqliteRelationshipLedger::in_memory().unwrap();
        let record = make_record(PrincipalId::new(), RelationalSignalType::TrustUpdate);
        ledger.append(&record).unwrap();

        let err = ledger.append(&record).unwrap_err();
        assert!(
            matches!(err, ExoError::Storage(ref msg) if msg.contains("already exists")),
            "Expected duplicate rejection, got: {err}"
        );
    }

    #[test]
    fn reopen_preserves_data() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("relationships.db");
        let p = PrincipalId::new();

        // Write records
        {
            let ledger = SqliteRelationshipLedger::open(&path).unwrap();
            ledger
                .append(&make_record(p, RelationalSignalType::TrustUpdate))
                .unwrap();
            ledger
                .append(&make_record(p, RelationalSignalType::CommitmentMade))
                .unwrap();
        }
        // Drop — connection closed

        // Reopen and verify
        let ledger = SqliteRelationshipLedger::open(&path).unwrap();
        assert_eq!(ledger.count().unwrap(), 2);
        let records = ledger.for_principal(p, 10).unwrap();
        assert_eq!(records.len(), 2);
    }

    #[test]
    fn empty_queries_return_empty() {
        let ledger = SqliteRelationshipLedger::in_memory().unwrap();
        assert!(ledger.recent(10).unwrap().is_empty());
        assert!(ledger
            .for_principal(PrincipalId::new(), 10)
            .unwrap()
            .is_empty());
        assert!(ledger.since_tick(TickId::new()).unwrap().is_empty());
        assert!(ledger.distinct_principals().unwrap().is_empty());
        assert_eq!(ledger.count().unwrap(), 0);
    }
}

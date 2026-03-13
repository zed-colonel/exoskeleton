//! SQLite-backed State Snapshot store.

use std::path::Path;
use std::sync::Mutex;

use exoskeleton_core::{ExoError, SnapshotStore, StateSnapshot};
use rusqlite::Connection;

/// SQLite-backed State Snapshot store.
///
/// Each tick produces a new StateSnapshot. The store retains all snapshots
/// for history and debugging. Keyed by `tick_number` — duplicates rejected.
///
/// The full `StateSnapshot` is serialized as JSON bytes in a BLOB column.
/// This avoids schema migration issues when fields are added in later sprints.
///
/// SQLite pragmas: WAL mode, `synchronous = FULL`, `busy_timeout = 5000`.
pub struct SqliteSnapshotStore {
    conn: Mutex<Connection>,
}

impl SqliteSnapshotStore {
    /// Open or create a snapshot store at the given path.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, ExoError> {
        let conn = Connection::open(path.as_ref())
            .map_err(|e| ExoError::Storage(format!("snapshot store open: {e}")))?;
        Self::initialize(conn)
    }

    /// Create an in-memory snapshot store (for testing).
    pub fn in_memory() -> Result<Self, ExoError> {
        let conn = Connection::open_in_memory()
            .map_err(|e| ExoError::Storage(format!("snapshot store in-memory: {e}")))?;
        Self::initialize(conn)
    }

    fn initialize(conn: Connection) -> Result<Self, ExoError> {
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA busy_timeout = 5000;
             PRAGMA synchronous = FULL;",
        )
        .map_err(|e| ExoError::Storage(format!("snapshot store pragmas: {e}")))?;

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS snapshots (
                tick_number  INTEGER NOT NULL PRIMARY KEY,
                vessel_id    TEXT NOT NULL,
                data         BLOB NOT NULL,
                created_at   TEXT NOT NULL
            ) STRICT;",
        )
        .map_err(|e| ExoError::Storage(format!("snapshot store schema: {e}")))?;

        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Deserialize a snapshot from a BLOB column.
    fn deserialize(data: &[u8]) -> Result<StateSnapshot, ExoError> {
        serde_json::from_slice(data)
            .map_err(|e| ExoError::Storage(format!("snapshot deserialization: {e}")))
    }
}

impl SnapshotStore for SqliteSnapshotStore {
    fn save(&self, snapshot: &StateSnapshot) -> Result<(), ExoError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| ExoError::Storage(format!("lock poisoned: {e}")))?;
        let data = serde_json::to_vec(snapshot)
            .map_err(|e| ExoError::Storage(format!("snapshot serialization: {e}")))?;

        match conn.execute(
            "INSERT INTO snapshots (tick_number, vessel_id, data, created_at)
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![
                snapshot.tick_number as i64,
                snapshot.vessel_id.to_string(),
                data,
                snapshot.updated_at.to_rfc3339(),
            ],
        ) {
            Ok(_) => Ok(()),
            Err(rusqlite::Error::SqliteFailure(err, _))
                if err.code == rusqlite::ErrorCode::ConstraintViolation =>
            {
                Err(ExoError::Storage(format!(
                    "snapshot already exists for tick_number {}",
                    snapshot.tick_number
                )))
            }
            Err(e) => Err(ExoError::Storage(format!("snapshot save: {e}"))),
        }
    }

    fn latest(&self) -> Result<Option<StateSnapshot>, ExoError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| ExoError::Storage(format!("lock poisoned: {e}")))?;
        let mut stmt = conn
            .prepare("SELECT data FROM snapshots ORDER BY tick_number DESC LIMIT 1")
            .map_err(|e| ExoError::Storage(format!("prepare: {e}")))?;

        match stmt.query_row([], |row| {
            let data: Vec<u8> = row.get(0)?;
            Ok(data)
        }) {
            Ok(data) => Self::deserialize(&data).map(Some),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(ExoError::Storage(format!("snapshot latest: {e}"))),
        }
    }

    fn at_tick(&self, tick_number: u64) -> Result<Option<StateSnapshot>, ExoError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| ExoError::Storage(format!("lock poisoned: {e}")))?;
        let mut stmt = conn
            .prepare("SELECT data FROM snapshots WHERE tick_number = ?1")
            .map_err(|e| ExoError::Storage(format!("prepare: {e}")))?;

        match stmt.query_row(rusqlite::params![tick_number as i64], |row| {
            let data: Vec<u8> = row.get(0)?;
            Ok(data)
        }) {
            Ok(data) => Self::deserialize(&data).map(Some),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(ExoError::Storage(format!("snapshot at_tick: {e}"))),
        }
    }

    fn history(&self, limit: usize) -> Result<Vec<StateSnapshot>, ExoError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| ExoError::Storage(format!("lock poisoned: {e}")))?;
        let mut stmt = conn
            .prepare("SELECT data FROM snapshots ORDER BY tick_number DESC LIMIT ?1")
            .map_err(|e| ExoError::Storage(format!("prepare: {e}")))?;

        let rows = stmt
            .query_map(rusqlite::params![limit], |row| {
                let data: Vec<u8> = row.get(0)?;
                Ok(data)
            })
            .map_err(|e| ExoError::Storage(format!("history query: {e}")))?;

        let mut snapshots = Vec::new();
        for row in rows {
            let data = row.map_err(|e| ExoError::Storage(format!("history row: {e}")))?;
            snapshots.push(Self::deserialize(&data)?);
        }
        Ok(snapshots)
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use exoskeleton_core::thread::ThreadStatus;
    use exoskeleton_core::{ArtifactId, BudgetStatus, ThreadSummary, VesselId, VesselStatus};

    use super::*;

    fn make_snapshot(vessel_id: VesselId, tick_number: u64) -> StateSnapshot {
        StateSnapshot {
            vessel_id,
            tick_number,
            mission: "test mission".into(),
            plan: None,
            status: VesselStatus::Idle,
            working_context: String::new(),
            thread_summaries: Vec::new(),
            relationship_snapshot_ref: None,
            budget_status: BudgetStatus::unlimited(),
            last_action_summary: None,
            updated_at: Utc::now(),
        }
    }

    // ── T-2: Snapshot Store ──

    #[test]
    fn save_and_latest() {
        let store = SqliteSnapshotStore::in_memory().unwrap();
        let vid = VesselId::new();
        let snap = make_snapshot(vid, 0);

        store.save(&snap).unwrap();
        let latest = store.latest().unwrap().unwrap();
        assert_eq!(latest.tick_number, 0);
        assert_eq!(latest.vessel_id, vid);
    }

    #[test]
    fn latest_returns_highest_tick() {
        let store = SqliteSnapshotStore::in_memory().unwrap();
        let vid = VesselId::new();

        for t in 0..3 {
            store.save(&make_snapshot(vid, t)).unwrap();
        }

        let latest = store.latest().unwrap().unwrap();
        assert_eq!(latest.tick_number, 2);
    }

    #[test]
    fn latest_returns_none_when_empty() {
        let store = SqliteSnapshotStore::in_memory().unwrap();
        assert!(store.latest().unwrap().is_none());
    }

    #[test]
    fn at_tick_returns_correct() {
        let store = SqliteSnapshotStore::in_memory().unwrap();
        let vid = VesselId::new();

        for t in 0..3 {
            store.save(&make_snapshot(vid, t)).unwrap();
        }

        let snap = store.at_tick(1).unwrap().unwrap();
        assert_eq!(snap.tick_number, 1);
    }

    #[test]
    fn at_tick_returns_none_for_missing() {
        let store = SqliteSnapshotStore::in_memory().unwrap();
        let vid = VesselId::new();
        store.save(&make_snapshot(vid, 0)).unwrap();
        assert!(store.at_tick(99).unwrap().is_none());
    }

    #[test]
    fn save_rejects_duplicate_tick_number() {
        let store = SqliteSnapshotStore::in_memory().unwrap();
        let vid = VesselId::new();
        let snap = make_snapshot(vid, 0);

        store.save(&snap).unwrap();
        let err = store.save(&snap).unwrap_err();
        assert!(
            matches!(err, ExoError::Storage(ref msg) if msg.contains("already exists")),
            "Expected duplicate rejection, got: {err}"
        );
    }

    #[test]
    fn history_returns_newest_first() {
        let store = SqliteSnapshotStore::in_memory().unwrap();
        let vid = VesselId::new();

        for t in 0..3 {
            store.save(&make_snapshot(vid, t)).unwrap();
        }

        let history = store.history(3).unwrap();
        assert_eq!(history.len(), 3);
        assert_eq!(history[0].tick_number, 2);
        assert_eq!(history[1].tick_number, 1);
        assert_eq!(history[2].tick_number, 0);
    }

    #[test]
    fn history_respects_limit() {
        let store = SqliteSnapshotStore::in_memory().unwrap();
        let vid = VesselId::new();

        for t in 0..5 {
            store.save(&make_snapshot(vid, t)).unwrap();
        }

        let history = store.history(2).unwrap();
        assert_eq!(history.len(), 2);
    }

    #[test]
    fn snapshot_json_roundtrip_through_store() {
        let store = SqliteSnapshotStore::in_memory().unwrap();
        let vid = VesselId::new();

        let snap = StateSnapshot {
            vessel_id: vid,
            tick_number: 42,
            mission: "Complex mission".into(),
            plan: Some("Execute plan B".into()),
            status: VesselStatus::Thinking,
            working_context: "Evaluating threats".into(),
            thread_summaries: vec![ThreadSummary {
                thread_id: exoskeleton_core::ThreadId::new(),
                name: "Threat Monitor".into(),
                status: ThreadStatus::Active,
                last_output_summary: Some("All clear".into()),
                token_budget_remaining: 5000,
            }],
            relationship_snapshot_ref: Some(ArtifactId::from_content(b"rel")),
            budget_status: BudgetStatus {
                local_tokens_remaining: 80_000,
                frontier_tokens_remaining: 20_000,
                frontier_cost_cents_remaining: 500,
                time_secs_remaining: 3600,
                thrash_level: exoskeleton_core::budget::ThrashLevel::None,
                tool_invocations_remaining: 950,
            },
            last_action_summary: Some("Created file".into()),
            updated_at: Utc::now(),
        };

        store.save(&snap).unwrap();
        let retrieved = store.latest().unwrap().unwrap();
        assert_eq!(snap, retrieved);
    }
}

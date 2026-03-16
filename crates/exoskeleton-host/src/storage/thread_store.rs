//! SQLite-backed thread store for thread specifications and operational state.

use std::path::Path;
use std::str::FromStr;
use std::sync::Mutex;

use exoskeleton_core::{
    ArtifactId, ExoError, ThreadId, ThreadOutput, ThreadSpec, ThreadStatus, TickId,
};
use exoskeleton_threads::ThreadStore;
use rusqlite::Connection;

/// SQLite-backed thread store.
///
/// Database location: `{data_dir}/exo/threads.db`
///
/// SQLite pragmas: WAL mode, `synchronous = FULL`, `busy_timeout = 5000`.
pub struct SqliteThreadStore {
    conn: Mutex<Connection>,
}

impl SqliteThreadStore {
    /// Open or create a thread store at the given path.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, ExoError> {
        let conn = Connection::open(path.as_ref())
            .map_err(|e| ExoError::Storage(format!("thread store open: {e}")))?;
        Self::initialize(conn)
    }

    /// Create an in-memory thread store (for testing).
    pub fn in_memory() -> Result<Self, ExoError> {
        let conn = Connection::open_in_memory()
            .map_err(|e| ExoError::Storage(format!("thread store in-memory: {e}")))?;
        Self::initialize(conn)
    }

    fn initialize(conn: Connection) -> Result<Self, ExoError> {
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA busy_timeout = 5000;
             PRAGMA synchronous = FULL;",
        )
        .map_err(|e| ExoError::Storage(format!("thread store pragmas: {e}")))?;

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS thread_specs (
                thread_id   TEXT NOT NULL PRIMARY KEY,
                spec_json   TEXT NOT NULL,
                status      TEXT NOT NULL DEFAULT 'active',
                last_run_tick INTEGER,
                created_at  TEXT NOT NULL,
                updated_at  TEXT NOT NULL
            ) STRICT;

            CREATE TABLE IF NOT EXISTS thread_outputs (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                thread_id   TEXT NOT NULL,
                tick_id     TEXT NOT NULL,
                artifact_id TEXT NOT NULL,
                summary     TEXT NOT NULL,
                recommendations_json TEXT NOT NULL,
                created_at  TEXT NOT NULL
            ) STRICT;

            CREATE INDEX IF NOT EXISTS idx_thread_outputs_thread_id
                ON thread_outputs(thread_id, id DESC);",
        )
        .map_err(|e| ExoError::Storage(format!("thread store schema: {e}")))?;

        Ok(Self {
            conn: Mutex::new(conn),
        })
    }
}

impl ThreadStore for SqliteThreadStore {
    fn save(&self, spec: &ThreadSpec, status: ThreadStatus) -> Result<(), ExoError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| ExoError::Storage(format!("lock poisoned: {e}")))?;

        let spec_json = serde_json::to_string(spec)
            .map_err(|e| ExoError::Storage(format!("spec serialization: {e}")))?;

        let status_str = serde_json::to_string(&status)
            .unwrap()
            .trim_matches('"')
            .to_string();

        let now = chrono::Utc::now().to_rfc3339();

        conn.execute(
            "INSERT OR REPLACE INTO thread_specs
             (thread_id, spec_json, status, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![spec.thread_id.to_string(), spec_json, status_str, now, now,],
        )
        .map_err(|e| ExoError::Storage(format!("save thread: {e}")))?;

        Ok(())
    }

    fn get(&self, thread_id: ThreadId) -> Result<Option<(ThreadSpec, ThreadStatus)>, ExoError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| ExoError::Storage(format!("lock poisoned: {e}")))?;

        let mut stmt = conn
            .prepare(
                "SELECT spec_json, status FROM thread_specs
                 WHERE thread_id = ?1",
            )
            .map_err(|e| ExoError::Storage(format!("prepare: {e}")))?;

        let result = stmt.query_row(rusqlite::params![thread_id.to_string()], |row| {
            Ok(ThreadRow {
                spec_json: row.get(0)?,
                status: row.get(1)?,
            })
        });

        match result {
            Ok(row) => {
                let spec: ThreadSpec = serde_json::from_str(&row.spec_json)
                    .map_err(|e| ExoError::Storage(format!("spec deserialization: {e}")))?;
                let status: ThreadStatus = serde_json::from_str(&format!("\"{}\"", row.status))
                    .map_err(|e| ExoError::Storage(format!("invalid status: {e}")))?;
                Ok(Some((spec, status)))
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(ExoError::Storage(format!("get thread: {e}"))),
        }
    }

    fn list(&self) -> Result<Vec<(ThreadSpec, ThreadStatus)>, ExoError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| ExoError::Storage(format!("lock poisoned: {e}")))?;

        let mut stmt = conn
            .prepare("SELECT spec_json, status FROM thread_specs")
            .map_err(|e| ExoError::Storage(format!("prepare: {e}")))?;

        let rows = stmt
            .query_map([], |row| {
                Ok(ThreadRow {
                    spec_json: row.get(0)?,
                    status: row.get(1)?,
                })
            })
            .map_err(|e| ExoError::Storage(format!("list threads: {e}")))?;

        let mut results = Vec::new();
        for row in rows {
            let row = row.map_err(|e| ExoError::Storage(format!("row: {e}")))?;
            let spec: ThreadSpec = serde_json::from_str(&row.spec_json)
                .map_err(|e| ExoError::Storage(format!("spec deserialization: {e}")))?;
            let status: ThreadStatus = serde_json::from_str(&format!("\"{}\"", row.status))
                .map_err(|e| ExoError::Storage(format!("invalid status: {e}")))?;
            results.push((spec, status));
        }
        Ok(results)
    }

    fn remove(&self, thread_id: ThreadId) -> Result<(), ExoError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| ExoError::Storage(format!("lock poisoned: {e}")))?;

        conn.execute(
            "DELETE FROM thread_outputs WHERE thread_id = ?1",
            rusqlite::params![thread_id.to_string()],
        )
        .map_err(|e| ExoError::Storage(format!("remove outputs: {e}")))?;

        conn.execute(
            "DELETE FROM thread_specs WHERE thread_id = ?1",
            rusqlite::params![thread_id.to_string()],
        )
        .map_err(|e| ExoError::Storage(format!("remove thread: {e}")))?;

        Ok(())
    }

    fn update_status(&self, thread_id: ThreadId, status: ThreadStatus) -> Result<(), ExoError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| ExoError::Storage(format!("lock poisoned: {e}")))?;

        let status_str = serde_json::to_string(&status)
            .unwrap()
            .trim_matches('"')
            .to_string();

        let now = chrono::Utc::now().to_rfc3339();

        let rows = conn
            .execute(
                "UPDATE thread_specs
                 SET status = ?1, updated_at = ?2
                 WHERE thread_id = ?3",
                rusqlite::params![status_str, now, thread_id.to_string(),],
            )
            .map_err(|e| ExoError::Storage(format!("update_status: {e}")))?;

        if rows == 0 {
            return Err(ExoError::Storage(format!("thread not found: {thread_id}")));
        }

        Ok(())
    }

    fn save_last_run(&self, thread_id: ThreadId, tick_number: u64) -> Result<(), ExoError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| ExoError::Storage(format!("lock poisoned: {e}")))?;

        let now = chrono::Utc::now().to_rfc3339();

        conn.execute(
            "UPDATE thread_specs
             SET last_run_tick = ?1, updated_at = ?2
             WHERE thread_id = ?3",
            rusqlite::params![tick_number as i64, now, thread_id.to_string(),],
        )
        .map_err(|e| ExoError::Storage(format!("save_last_run: {e}")))?;

        Ok(())
    }

    fn get_last_run(&self, thread_id: ThreadId) -> Result<Option<u64>, ExoError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| ExoError::Storage(format!("lock poisoned: {e}")))?;

        let result = conn.query_row(
            "SELECT last_run_tick FROM thread_specs
             WHERE thread_id = ?1",
            rusqlite::params![thread_id.to_string()],
            |row| row.get::<_, Option<i64>>(0),
        );

        match result {
            Ok(Some(tick)) => Ok(Some(tick as u64)),
            Ok(None) => Ok(None),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(ExoError::Storage(format!("get_last_run: {e}"))),
        }
    }

    fn save_output(&self, output: &ThreadOutput) -> Result<(), ExoError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| ExoError::Storage(format!("lock poisoned: {e}")))?;

        let recommendations_json = serde_json::to_string(&output.recommendations)
            .map_err(|e| ExoError::Storage(format!("recommendations serialization: {e}")))?;

        let now = chrono::Utc::now().to_rfc3339();

        conn.execute(
            "INSERT INTO thread_outputs
             (thread_id, tick_id, artifact_id, summary,
              recommendations_json, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                output.thread_id.to_string(),
                output.tick_id.to_string(),
                output.artifact_id.to_string(),
                output.summary,
                recommendations_json,
                now,
            ],
        )
        .map_err(|e| ExoError::Storage(format!("save_output: {e}")))?;

        Ok(())
    }

    fn update_charter(&self, thread_id: ThreadId, charter: String) -> Result<(), ExoError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| ExoError::Storage(format!("lock poisoned: {e}")))?;

        // Read current spec, update charter, re-serialize
        let row: String = conn
            .query_row(
                "SELECT spec_json FROM thread_specs WHERE thread_id = ?1",
                rusqlite::params![thread_id.to_string()],
                |row| row.get(0),
            )
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => {
                    ExoError::Storage(format!("thread not found: {thread_id}"))
                }
                other => ExoError::Storage(format!("update_charter query: {other}")),
            })?;

        let mut spec: ThreadSpec = serde_json::from_str(&row)
            .map_err(|e| ExoError::Storage(format!("spec deserialization: {e}")))?;
        spec.charter = charter;
        let new_json = serde_json::to_string(&spec)
            .map_err(|e| ExoError::Storage(format!("spec serialization: {e}")))?;

        let now = chrono::Utc::now().to_rfc3339();
        conn.execute(
            "UPDATE thread_specs SET spec_json = ?1, updated_at = ?2 WHERE thread_id = ?3",
            rusqlite::params![new_json, now, thread_id.to_string()],
        )
        .map_err(|e| ExoError::Storage(format!("update_charter: {e}")))?;

        Ok(())
    }

    fn recent_outputs(
        &self,
        thread_id: ThreadId,
        limit: usize,
    ) -> Result<Vec<ThreadOutput>, ExoError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| ExoError::Storage(format!("lock poisoned: {e}")))?;

        let mut stmt = conn
            .prepare(
                "SELECT thread_id, tick_id, artifact_id, summary,
                        recommendations_json
                 FROM thread_outputs
                 WHERE thread_id = ?1
                 ORDER BY id DESC
                 LIMIT ?2",
            )
            .map_err(|e| ExoError::Storage(format!("prepare: {e}")))?;

        let rows = stmt
            .query_map(
                rusqlite::params![thread_id.to_string(), limit as i64,],
                |row| {
                    Ok(OutputRow {
                        thread_id: row.get(0)?,
                        tick_id: row.get(1)?,
                        artifact_id: row.get(2)?,
                        summary: row.get(3)?,
                        recommendations_json: row.get(4)?,
                    })
                },
            )
            .map_err(|e| ExoError::Storage(format!("recent_outputs query: {e}")))?;

        let mut results = Vec::new();
        for row in rows {
            let row = row.map_err(|e| ExoError::Storage(format!("row: {e}")))?;
            results.push(row.into_thread_output()?);
        }
        Ok(results)
    }
}

/// Intermediate row type for thread spec deserialization.
struct ThreadRow {
    spec_json: String,
    status: String,
}

/// Intermediate row type for thread output deserialization.
struct OutputRow {
    thread_id: String,
    tick_id: String,
    artifact_id: String,
    summary: String,
    recommendations_json: String,
}

impl OutputRow {
    fn into_thread_output(self) -> Result<ThreadOutput, ExoError> {
        let thread_id = ThreadId::from_str(&self.thread_id)
            .map_err(|e| ExoError::Storage(format!("invalid thread_id: {e}")))?;
        let tick_id = TickId::from_str(&self.tick_id)
            .map_err(|e| ExoError::Storage(format!("invalid tick_id: {e}")))?;
        let artifact_id = ArtifactId::from_str(&self.artifact_id)
            .map_err(|e| ExoError::Storage(format!("invalid artifact_id: {e}")))?;
        let recommendations: Vec<String> = serde_json::from_str(&self.recommendations_json)
            .map_err(|e| ExoError::Storage(format!("recommendations deserialization: {e}")))?;

        Ok(ThreadOutput {
            thread_id,
            tick_id,
            artifact_id,
            summary: self.summary,
            recommendations,
        })
    }
}

#[cfg(test)]
mod tests {
    use exoskeleton_core::{
        ArtifactId, ThreadId, ThreadPriority, ThreadSchedule, ThreadSpec, ThreadStatus, TickId,
    };

    use super::*;

    /// Helper: create a `ThreadSpec` with sensible defaults for testing.
    fn make_spec(name: &str) -> ThreadSpec {
        ThreadSpec {
            thread_id: ThreadId::new(),
            name: name.into(),
            charter: format!("Charter for {name}"),
            priority: ThreadPriority::Normal,
            token_budget: 4096,
            schedule: ThreadSchedule::EveryTick,
        }
    }

    /// Helper: create a `ThreadOutput` for a given thread.
    fn make_output(thread_id: ThreadId, summary: &str) -> ThreadOutput {
        ThreadOutput {
            thread_id,
            tick_id: TickId::new(),
            artifact_id: ArtifactId::from_content(summary.as_bytes()),
            summary: summary.into(),
            recommendations: vec![format!("rec from {summary}")],
        }
    }

    #[test]
    fn sqlite_save_and_get_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("threads.db");

        let spec = make_spec("Alpha");
        let thread_id = spec.thread_id;

        {
            let store = SqliteThreadStore::open(&path).unwrap();
            store.save(&spec, ThreadStatus::Active).unwrap();
        } // drop → connection closed

        let store2 = SqliteThreadStore::open(&path).unwrap();
        let result = store2.get(thread_id).unwrap();
        assert!(result.is_some());
        let (got_spec, got_status) = result.unwrap();
        assert_eq!(got_spec, spec);
        assert_eq!(got_status, ThreadStatus::Active);
    }

    #[test]
    fn sqlite_list_survives_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("threads.db");

        let specs: Vec<ThreadSpec> = vec![make_spec("One"), make_spec("Two"), make_spec("Three")];

        {
            let store = SqliteThreadStore::open(&path).unwrap();
            for spec in &specs {
                store.save(spec, ThreadStatus::Active).unwrap();
            }
        }

        let store2 = SqliteThreadStore::open(&path).unwrap();
        let list = store2.list().unwrap();
        assert_eq!(list.len(), 3);

        for spec in &specs {
            assert!(
                list.iter().any(|(s, _)| s.thread_id == spec.thread_id),
                "Missing thread {}",
                spec.name
            );
        }
    }

    #[test]
    fn sqlite_status_update_persists() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("threads.db");

        let spec = make_spec("Updatable");
        let thread_id = spec.thread_id;

        {
            let store = SqliteThreadStore::open(&path).unwrap();
            store.save(&spec, ThreadStatus::Active).unwrap();
            store
                .update_status(thread_id, ThreadStatus::Suspended)
                .unwrap();
        }

        let store2 = SqliteThreadStore::open(&path).unwrap();
        let (_, status) = store2.get(thread_id).unwrap().unwrap();
        assert_eq!(status, ThreadStatus::Suspended);
    }

    #[test]
    fn sqlite_last_run_persists() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("threads.db");

        let spec = make_spec("Runner");
        let thread_id = spec.thread_id;

        {
            let store = SqliteThreadStore::open(&path).unwrap();
            store.save(&spec, ThreadStatus::Active).unwrap();
            store.save_last_run(thread_id, 42).unwrap();
        }

        let store2 = SqliteThreadStore::open(&path).unwrap();
        let last_run = store2.get_last_run(thread_id).unwrap();
        assert_eq!(last_run, Some(42));
    }

    #[test]
    fn sqlite_recent_outputs_persists() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("threads.db");

        let spec = make_spec("OutputProducer");
        let thread_id = spec.thread_id;

        let output1 = make_output(thread_id, "output-1");
        let output2 = make_output(thread_id, "output-2");
        let output3 = make_output(thread_id, "output-3");

        {
            let store = SqliteThreadStore::open(&path).unwrap();
            store.save(&spec, ThreadStatus::Active).unwrap();
            store.save_output(&output1).unwrap();
            store.save_output(&output2).unwrap();
            store.save_output(&output3).unwrap();
        }

        let store2 = SqliteThreadStore::open(&path).unwrap();
        let recent = store2.recent_outputs(thread_id, 2).unwrap();
        assert_eq!(recent.len(), 2);
        // Newest first
        assert_eq!(recent[0].summary, "output-3");
        assert_eq!(recent[1].summary, "output-2");
    }

    #[test]
    fn sqlite_remove_cascades() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("threads.db");

        let spec = make_spec("Removable");
        let thread_id = spec.thread_id;

        {
            let store = SqliteThreadStore::open(&path).unwrap();
            store.save(&spec, ThreadStatus::Active).unwrap();
            store.save_last_run(thread_id, 10).unwrap();
            store.save_output(&make_output(thread_id, "out-1")).unwrap();
            store.save_output(&make_output(thread_id, "out-2")).unwrap();

            store.remove(thread_id).unwrap();
        }

        let store2 = SqliteThreadStore::open(&path).unwrap();
        assert!(store2.get(thread_id).unwrap().is_none());
        assert!(store2.get_last_run(thread_id).unwrap().is_none());
        assert!(store2.recent_outputs(thread_id, 10).unwrap().is_empty());
    }

    #[test]
    fn sqlite_update_charter_persists() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("threads.db");

        let spec = make_spec("CharterUpdate");
        let thread_id = spec.thread_id;

        {
            let store = SqliteThreadStore::open(&path).unwrap();
            store.save(&spec, ThreadStatus::Active).unwrap();
            store
                .update_charter(thread_id, "New charter from test".into())
                .unwrap();
        }

        let store2 = SqliteThreadStore::open(&path).unwrap();
        let (got_spec, _) = store2.get(thread_id).unwrap().unwrap();
        assert_eq!(got_spec.charter, "New charter from test");
        // Name should be unchanged
        assert_eq!(got_spec.name, "CharterUpdate");
    }
}

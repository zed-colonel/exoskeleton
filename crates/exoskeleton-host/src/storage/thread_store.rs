//! SQLite-backed unified thread store.

use std::path::Path;
use std::sync::Mutex;

use exoskeleton_core::{
    ExecThreadLocalState, ExoError, ThreadExecutionResult, ThreadId, ThreadSpec,
};
use exoskeleton_threads::{RegisteredThreadStatus, ThreadStore};
use rusqlite::Connection;

pub struct SqliteThreadStore {
    conn: Mutex<Connection>,
}

impl SqliteThreadStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, ExoError> {
        let conn = Connection::open(path.as_ref())
            .map_err(|e| ExoError::Storage(format!("thread store open: {e}")))?;
        Self::initialize(conn)
    }

    pub fn in_memory() -> Result<Self, ExoError> {
        let conn = Connection::open_in_memory()
            .map_err(|e| ExoError::Storage(format!("thread store in-memory: {e}")))?;
        Self::initialize(conn)
    }

    fn initialize(conn: Connection) -> Result<Self, ExoError> {
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA busy_timeout = 5000;
             PRAGMA synchronous = FULL;

             CREATE TABLE IF NOT EXISTS thread_specs (
                thread_id TEXT NOT NULL PRIMARY KEY,
                spec_json TEXT NOT NULL,
                status_json TEXT NOT NULL,
                last_run_tick INTEGER,
                local_state_json TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
             ) STRICT;

             CREATE TABLE IF NOT EXISTS thread_outputs (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                thread_id TEXT NOT NULL,
                result_json TEXT NOT NULL,
                created_at TEXT NOT NULL
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
    fn save(&self, spec: &ThreadSpec, status: RegisteredThreadStatus) -> Result<(), ExoError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| ExoError::Storage(format!("lock poisoned: {e}")))?;
        let spec_json = serde_json::to_string(spec)
            .map_err(|e| ExoError::Storage(format!("spec serialization: {e}")))?;
        let status_json = serde_json::to_string(&status)
            .map_err(|e| ExoError::Storage(format!("status serialization: {e}")))?;
        let now = chrono::Utc::now().to_rfc3339();
        conn.execute(
            "INSERT OR REPLACE INTO thread_specs
             (thread_id, spec_json, status_json, last_run_tick, local_state_json, created_at, updated_at)
             VALUES (
                ?1,
                ?2,
                ?3,
                COALESCE((SELECT last_run_tick FROM thread_specs WHERE thread_id = ?1), NULL),
                COALESCE((SELECT local_state_json FROM thread_specs WHERE thread_id = ?1), NULL),
                COALESCE((SELECT created_at FROM thread_specs WHERE thread_id = ?1), ?4),
                ?5
             )",
            rusqlite::params![spec.thread_id.to_string(), spec_json, status_json, now, now],
        )
        .map_err(|e| ExoError::Storage(format!("save thread: {e}")))?;
        Ok(())
    }

    fn get(
        &self,
        thread_id: ThreadId,
    ) -> Result<Option<(ThreadSpec, RegisteredThreadStatus)>, ExoError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| ExoError::Storage(format!("lock poisoned: {e}")))?;
        let result = conn.query_row(
            "SELECT spec_json, status_json FROM thread_specs WHERE thread_id = ?1",
            rusqlite::params![thread_id.to_string()],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        );

        match result {
            Ok((spec_json, status_json)) => Ok(Some((
                serde_json::from_str(&spec_json)
                    .map_err(|e| ExoError::Storage(format!("spec deserialization: {e}")))?,
                serde_json::from_str(&status_json)
                    .map_err(|e| ExoError::Storage(format!("status deserialization: {e}")))?,
            ))),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(ExoError::Storage(format!("get thread: {e}"))),
        }
    }

    fn list(&self) -> Result<Vec<(ThreadSpec, RegisteredThreadStatus)>, ExoError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| ExoError::Storage(format!("lock poisoned: {e}")))?;
        let mut stmt = conn
            .prepare("SELECT spec_json, status_json FROM thread_specs")
            .map_err(|e| ExoError::Storage(format!("prepare: {e}")))?;
        let rows = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|e| ExoError::Storage(format!("list threads: {e}")))?;
        let mut out = Vec::new();
        for row in rows {
            let (spec_json, status_json) =
                row.map_err(|e| ExoError::Storage(format!("row: {e}")))?;
            out.push((
                serde_json::from_str(&spec_json)
                    .map_err(|e| ExoError::Storage(format!("spec deserialization: {e}")))?,
                serde_json::from_str(&status_json)
                    .map_err(|e| ExoError::Storage(format!("status deserialization: {e}")))?,
            ));
        }
        Ok(out)
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

    fn update_status(
        &self,
        thread_id: ThreadId,
        status: RegisteredThreadStatus,
    ) -> Result<(), ExoError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| ExoError::Storage(format!("lock poisoned: {e}")))?;
        let status_json = serde_json::to_string(&status)
            .map_err(|e| ExoError::Storage(format!("status serialization: {e}")))?;
        let now = chrono::Utc::now().to_rfc3339();
        let rows = conn
            .execute(
                "UPDATE thread_specs SET status_json = ?1, updated_at = ?2 WHERE thread_id = ?3",
                rusqlite::params![status_json, now, thread_id.to_string()],
            )
            .map_err(|e| ExoError::Storage(format!("update thread status: {e}")))?;
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
            "UPDATE thread_specs SET last_run_tick = ?1, updated_at = ?2 WHERE thread_id = ?3",
            rusqlite::params![tick_number as i64, now, thread_id.to_string()],
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
            "SELECT last_run_tick FROM thread_specs WHERE thread_id = ?1",
            rusqlite::params![thread_id.to_string()],
            |row| row.get::<_, Option<i64>>(0),
        );
        match result {
            Ok(Some(tick)) => Ok(Some(tick as u64)),
            Ok(None) | Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(ExoError::Storage(format!("get_last_run: {e}"))),
        }
    }

    fn recent_execution_results(
        &self,
        thread_id: ThreadId,
        limit: usize,
    ) -> Result<Vec<ThreadExecutionResult>, ExoError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| ExoError::Storage(format!("lock poisoned: {e}")))?;
        let mut stmt = conn
            .prepare(
                "SELECT result_json FROM thread_outputs
                 WHERE thread_id = ?1
                 ORDER BY id DESC
                 LIMIT ?2",
            )
            .map_err(|e| ExoError::Storage(format!("prepare: {e}")))?;
        let rows = stmt
            .query_map(
                rusqlite::params![thread_id.to_string(), limit as i64],
                |row| row.get::<_, String>(0),
            )
            .map_err(|e| ExoError::Storage(format!("recent thread outputs: {e}")))?;
        let mut out = Vec::new();
        for row in rows {
            let json = row.map_err(|e| ExoError::Storage(format!("row: {e}")))?;
            out.push(
                serde_json::from_str(&json)
                    .map_err(|e| ExoError::Storage(format!("result deserialization: {e}")))?,
            );
        }
        Ok(out)
    }

    fn save_execution_result(&self, output: &ThreadExecutionResult) -> Result<(), ExoError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| ExoError::Storage(format!("lock poisoned: {e}")))?;
        let json = serde_json::to_string(output)
            .map_err(|e| ExoError::Storage(format!("result serialization: {e}")))?;
        let now = chrono::Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO thread_outputs (thread_id, result_json, created_at) VALUES (?1, ?2, ?3)",
            rusqlite::params![output.thread_id.to_string(), json, now],
        )
        .map_err(|e| ExoError::Storage(format!("save execution result: {e}")))?;
        Ok(())
    }

    fn get_local_state(
        &self,
        thread_id: ThreadId,
    ) -> Result<Option<ExecThreadLocalState>, ExoError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| ExoError::Storage(format!("lock poisoned: {e}")))?;
        let result = conn.query_row(
            "SELECT local_state_json FROM thread_specs WHERE thread_id = ?1",
            rusqlite::params![thread_id.to_string()],
            |row| row.get::<_, Option<String>>(0),
        );
        match result {
            Ok(Some(json)) => Ok(Some(serde_json::from_str(&json).map_err(|e| {
                ExoError::Storage(format!("local state deserialization: {e}"))
            })?)),
            Ok(None) | Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(ExoError::Storage(format!("get local state: {e}"))),
        }
    }

    fn save_local_state(
        &self,
        thread_id: ThreadId,
        local_state: &ExecThreadLocalState,
    ) -> Result<(), ExoError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| ExoError::Storage(format!("lock poisoned: {e}")))?;
        let json = serde_json::to_string(local_state)
            .map_err(|e| ExoError::Storage(format!("local state serialization: {e}")))?;
        let now = chrono::Utc::now().to_rfc3339();
        conn.execute(
            "UPDATE thread_specs SET local_state_json = ?1, updated_at = ?2 WHERE thread_id = ?3",
            rusqlite::params![json, now, thread_id.to_string()],
        )
        .map_err(|e| ExoError::Storage(format!("save local state: {e}")))?;
        Ok(())
    }

    fn update_charter(&self, thread_id: ThreadId, charter: String) -> Result<(), ExoError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| ExoError::Storage(format!("lock poisoned: {e}")))?;
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
}

#[cfg(test)]
mod tests {
    use exoskeleton_core::{
        ArtifactId, ExecThreadKind, ExecThreadLocalState, ExecThreadOutput, ExecThreadStatus,
        ThreadFlavor, ThreadId, ThreadPriority, ThreadRole, ThreadSchedule, ThreadSpec,
        ThreadStatus, TickId,
    };
    use exoskeleton_threads::RegisteredThreadStatus;

    use super::*;

    fn make_spec(name: &str) -> ThreadSpec {
        ThreadSpec {
            thread_id: ThreadId::new(),
            role: ThreadRole::Other,
            flavor: ThreadFlavor::Cognitive,
            name: name.into(),
            charter: format!("Charter for {name}"),
            priority: ThreadPriority::Normal,
            token_budget: 4096,
            schedule: ThreadSchedule::EveryTick,
            workspace_root: None,
        }
    }

    fn make_output(thread_id: ThreadId, summary: &str) -> exoskeleton_core::ThreadOutput {
        exoskeleton_core::ThreadOutput {
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
            store
                .save(&spec, RegisteredThreadStatus::cognitive_active())
                .unwrap();
        }

        let store2 = SqliteThreadStore::open(&path).unwrap();
        let (got_spec, got_status) = store2.get(thread_id).unwrap().unwrap();
        assert_eq!(got_spec, spec);
        assert_eq!(got_status, RegisteredThreadStatus::cognitive_active());
    }

    #[test]
    fn sqlite_status_update_persists() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("threads.db");
        let spec = make_spec("Updatable");
        let thread_id = spec.thread_id;

        {
            let store = SqliteThreadStore::open(&path).unwrap();
            store
                .save(&spec, RegisteredThreadStatus::cognitive_active())
                .unwrap();
            store
                .update_status(
                    thread_id,
                    RegisteredThreadStatus::Cognitive(ThreadStatus::Suspended),
                )
                .unwrap();
        }

        let store2 = SqliteThreadStore::open(&path).unwrap();
        let (_, status) = store2.get(thread_id).unwrap().unwrap();
        assert_eq!(
            status,
            RegisteredThreadStatus::Cognitive(ThreadStatus::Suspended)
        );
    }

    #[test]
    fn sqlite_recent_outputs_persist_unified_results() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("threads.db");
        let spec = make_spec("OutputProducer");
        let thread_id = spec.thread_id;
        let output1 = make_output(thread_id, "output-1");
        let output2 = make_output(thread_id, "output-2");

        {
            let store = SqliteThreadStore::open(&path).unwrap();
            store
                .save(&spec, RegisteredThreadStatus::cognitive_active())
                .unwrap();
            store.save_output(&output1).unwrap();
            store.save_output(&output2).unwrap();
        }

        let store2 = SqliteThreadStore::open(&path).unwrap();
        let recent = store2.recent_outputs(thread_id, 2).unwrap();
        assert_eq!(recent[0].summary, "output-2");
        assert_eq!(recent[1].summary, "output-1");
        assert_eq!(
            store2.recent_execution_results(thread_id, 2).unwrap().len(),
            2
        );
    }

    #[test]
    fn sqlite_exec_local_state_and_outputs_persist() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("threads.db");
        let spec = ThreadSpec {
            role: ThreadRole::Coding,
            flavor: ThreadFlavor::Executable,
            ..make_spec("Coding")
        };
        let thread_id = spec.thread_id;
        let state = ExecThreadLocalState {
            current_focus: Some("focus".into()),
            ..ExecThreadLocalState::default()
        };
        let output = ExecThreadOutput {
            thread_id,
            tick_id: TickId::new(),
            artifact_id: ArtifactId::from_content(b"exec"),
            kind: ExecThreadKind::Coding,
            summary: "exec".into(),
            status: ExecThreadStatus::Idle,
            evidence_complete: true,
            proposal_confidence: None,
            proposed_action: None,
            local_state: state.clone(),
        };

        {
            let store = SqliteThreadStore::open(&path).unwrap();
            store
                .save(
                    &spec,
                    RegisteredThreadStatus::Executable(ExecThreadStatus::Idle),
                )
                .unwrap();
            store.save_local_state(thread_id, &state).unwrap();
            store.save_exec_output(&output).unwrap();
        }

        let store2 = SqliteThreadStore::open(&path).unwrap();
        assert_eq!(store2.get_local_state(thread_id).unwrap(), Some(state));
        assert_eq!(
            store2.recent_exec_outputs(thread_id, 1).unwrap(),
            vec![output]
        );
    }
}

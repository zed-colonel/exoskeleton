//! SQLite-backed executable thread store.

use std::path::Path;
use std::sync::Mutex;

use exoskeleton_core::{
    ExecThreadLocalState, ExecThreadOutput, ExecThreadSpec, ExecThreadStatus, ExoError, ThreadId,
};
use rusqlite::Connection;

use crate::exec_threads::ExecThreadStore;

pub struct SqliteExecThreadStore {
    conn: Mutex<Connection>,
}

impl SqliteExecThreadStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, ExoError> {
        let conn = Connection::open(path.as_ref())
            .map_err(|e| ExoError::Storage(format!("exec thread store open: {e}")))?;
        Self::initialize(conn)
    }

    fn initialize(conn: Connection) -> Result<Self, ExoError> {
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA busy_timeout = 5000;
             PRAGMA synchronous = FULL;
             CREATE TABLE IF NOT EXISTS exec_thread_specs (
                thread_id TEXT NOT NULL PRIMARY KEY,
                spec_json TEXT NOT NULL,
                status TEXT NOT NULL,
                local_state_json TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
             ) STRICT;
             CREATE TABLE IF NOT EXISTS exec_thread_outputs (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                thread_id TEXT NOT NULL,
                output_json TEXT NOT NULL,
                created_at TEXT NOT NULL
             ) STRICT;
             CREATE INDEX IF NOT EXISTS idx_exec_thread_outputs_thread_id
                ON exec_thread_outputs(thread_id, id DESC);",
        )
        .map_err(|e| ExoError::Storage(format!("exec thread store schema: {e}")))?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }
}

impl ExecThreadStore for SqliteExecThreadStore {
    fn save(&self, spec: &ExecThreadSpec, status: ExecThreadStatus) -> Result<(), ExoError> {
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
            "INSERT OR REPLACE INTO exec_thread_specs
             (thread_id, spec_json, status, created_at, updated_at)
             VALUES (?1, ?2, ?3, COALESCE((SELECT created_at FROM exec_thread_specs WHERE thread_id = ?1), ?4), ?5)",
            rusqlite::params![spec.thread_id.to_string(), spec_json, status_json, now, now],
        )
        .map_err(|e| ExoError::Storage(format!("save exec thread: {e}")))?;
        Ok(())
    }

    fn get(
        &self,
        thread_id: ThreadId,
    ) -> Result<Option<(ExecThreadSpec, ExecThreadStatus)>, ExoError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| ExoError::Storage(format!("lock poisoned: {e}")))?;
        let result = conn.query_row(
            "SELECT spec_json, status FROM exec_thread_specs WHERE thread_id = ?1",
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
            Err(e) => Err(ExoError::Storage(format!("get exec thread: {e}"))),
        }
    }

    fn list(&self) -> Result<Vec<(ExecThreadSpec, ExecThreadStatus)>, ExoError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| ExoError::Storage(format!("lock poisoned: {e}")))?;
        let mut stmt = conn
            .prepare("SELECT spec_json, status FROM exec_thread_specs")
            .map_err(|e| ExoError::Storage(format!("prepare: {e}")))?;
        let rows = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|e| ExoError::Storage(format!("list exec threads: {e}")))?;
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

    fn update_status(&self, thread_id: ThreadId, status: ExecThreadStatus) -> Result<(), ExoError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| ExoError::Storage(format!("lock poisoned: {e}")))?;
        let status_json = serde_json::to_string(&status)
            .map_err(|e| ExoError::Storage(format!("status serialization: {e}")))?;
        let now = chrono::Utc::now().to_rfc3339();
        conn.execute(
            "UPDATE exec_thread_specs SET status = ?1, updated_at = ?2 WHERE thread_id = ?3",
            rusqlite::params![status_json, now, thread_id.to_string()],
        )
        .map_err(|e| ExoError::Storage(format!("update exec thread status: {e}")))?;
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
            "SELECT local_state_json FROM exec_thread_specs WHERE thread_id = ?1",
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
            "UPDATE exec_thread_specs SET local_state_json = ?1, updated_at = ?2 WHERE thread_id = ?3",
            rusqlite::params![json, now, thread_id.to_string()],
        )
        .map_err(|e| ExoError::Storage(format!("save local state: {e}")))?;
        Ok(())
    }

    fn recent_outputs(
        &self,
        thread_id: ThreadId,
        limit: usize,
    ) -> Result<Vec<ExecThreadOutput>, ExoError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| ExoError::Storage(format!("lock poisoned: {e}")))?;
        let mut stmt = conn
            .prepare(
                "SELECT output_json FROM exec_thread_outputs
                 WHERE thread_id = ?1 ORDER BY id DESC LIMIT ?2",
            )
            .map_err(|e| ExoError::Storage(format!("prepare: {e}")))?;
        let rows = stmt
            .query_map(
                rusqlite::params![thread_id.to_string(), limit as i64],
                |row| row.get::<_, String>(0),
            )
            .map_err(|e| ExoError::Storage(format!("recent exec thread outputs: {e}")))?;
        let mut out = Vec::new();
        for row in rows {
            let json = row.map_err(|e| ExoError::Storage(format!("row: {e}")))?;
            out.push(
                serde_json::from_str(&json)
                    .map_err(|e| ExoError::Storage(format!("output deserialization: {e}")))?,
            );
        }
        Ok(out)
    }

    fn save_output(&self, output: &ExecThreadOutput) -> Result<(), ExoError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| ExoError::Storage(format!("lock poisoned: {e}")))?;
        let json = serde_json::to_string(output)
            .map_err(|e| ExoError::Storage(format!("output serialization: {e}")))?;
        let now = chrono::Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO exec_thread_outputs (thread_id, output_json, created_at)
             VALUES (?1, ?2, ?3)",
            rusqlite::params![output.thread_id.to_string(), json, now],
        )
        .map_err(|e| ExoError::Storage(format!("save exec thread output: {e}")))?;
        Ok(())
    }
}

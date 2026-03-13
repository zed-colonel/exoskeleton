//! SQLite-backed budget state persistence.

use std::path::Path;
use std::sync::Mutex;

use exoskeleton_core::budget::{BudgetStore, PersistedBudgetState};
use exoskeleton_core::ExoError;
use rusqlite::Connection;

/// SQLite-backed budget state persistence.
///
/// Stores a single JSON-serialized `PersistedBudgetState` under the key "current".
/// Updated atomically on each `persist()` call and on window reset.
///
/// Added as the 8th store in Sprint 9. Database file: `{data_dir}/exo/budget.db`.
pub struct SqliteBudgetStore {
    conn: Mutex<Connection>,
}

impl SqliteBudgetStore {
    /// Open or create a budget store at the given path.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, ExoError> {
        let conn = Connection::open(path.as_ref())
            .map_err(|e| ExoError::Storage(format!("budget store open: {e}")))?;
        Self::initialize(conn)
    }

    /// Create an in-memory budget store (for testing).
    pub fn in_memory() -> Result<Self, ExoError> {
        let conn = Connection::open_in_memory()
            .map_err(|e| ExoError::Storage(format!("budget store in-memory: {e}")))?;
        Self::initialize(conn)
    }

    fn initialize(conn: Connection) -> Result<Self, ExoError> {
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA busy_timeout = 5000;
             PRAGMA synchronous = FULL;",
        )
        .map_err(|e| ExoError::Storage(format!("budget store pragmas: {e}")))?;

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS budget_state (
                key        TEXT PRIMARY KEY,
                value      TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );",
        )
        .map_err(|e| ExoError::Storage(format!("budget store schema: {e}")))?;

        Ok(Self {
            conn: Mutex::new(conn),
        })
    }
}

impl BudgetStore for SqliteBudgetStore {
    fn save(&self, state: &PersistedBudgetState) -> Result<(), ExoError> {
        let json = serde_json::to_string(state)
            .map_err(|e| ExoError::Storage(format!("budget state serialize: {e}")))?;
        let now = chrono::Utc::now().to_rfc3339();

        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO budget_state (key, value, updated_at) VALUES (?1, ?2, ?3)",
            rusqlite::params!["current", json, now],
        )
        .map_err(|e| ExoError::Storage(format!("budget state save: {e}")))?;
        Ok(())
    }

    fn load(&self) -> Result<Option<PersistedBudgetState>, ExoError> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare("SELECT value FROM budget_state WHERE key = 'current'")
            .map_err(|e| ExoError::Storage(format!("budget state load prepare: {e}")))?;

        let result: Option<String> = stmt.query_row([], |row| row.get(0)).ok();

        match result {
            Some(json) => {
                let state: PersistedBudgetState = serde_json::from_str(&json)
                    .map_err(|e| ExoError::Storage(format!("budget state deserialize: {e}")))?;
                Ok(Some(state))
            }
            None => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_and_load_roundtrip() {
        let store = SqliteBudgetStore::in_memory().unwrap();
        let state = PersistedBudgetState {
            window_start: chrono::Utc::now(),
            local_tokens_consumed: 5000,
            frontier_tokens_consumed: 1000,
            frontier_cost_consumed: 50,
            frontier_calls: 3,
            per_thread_consumed: {
                let mut m = std::collections::HashMap::new();
                m.insert("thread-1".into(), 2000);
                m
            },
            consecutive_failures: 1,
            tool_invocations: 10,
        };

        store.save(&state).unwrap();
        let loaded = store.load().unwrap().unwrap();
        assert_eq!(loaded.local_tokens_consumed, 5000);
        assert_eq!(loaded.frontier_tokens_consumed, 1000);
        assert_eq!(loaded.frontier_calls, 3);
        assert_eq!(loaded.consecutive_failures, 1);
        assert_eq!(loaded.per_thread_consumed.get("thread-1"), Some(&2000));
    }

    #[test]
    fn load_empty_returns_none() {
        let store = SqliteBudgetStore::in_memory().unwrap();
        assert!(store.load().unwrap().is_none());
    }

    #[test]
    fn save_overwrites_previous() {
        let store = SqliteBudgetStore::in_memory().unwrap();
        let state1 = PersistedBudgetState {
            window_start: chrono::Utc::now(),
            local_tokens_consumed: 1000,
            frontier_tokens_consumed: 0,
            frontier_cost_consumed: 0,
            frontier_calls: 0,
            per_thread_consumed: Default::default(),
            consecutive_failures: 0,
            tool_invocations: 0,
        };
        store.save(&state1).unwrap();

        let state2 = PersistedBudgetState {
            local_tokens_consumed: 5000,
            ..state1.clone()
        };
        store.save(&state2).unwrap();

        let loaded = store.load().unwrap().unwrap();
        assert_eq!(loaded.local_tokens_consumed, 5000);
    }
}

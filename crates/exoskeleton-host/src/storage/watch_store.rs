//! SQLite-backed WatchStore implementation.

use std::path::Path;
use std::sync::Mutex;

use exoskeleton_core::watch::{WatchDefinition, WatchSchedule, WatchStatus, WatchStore};
use exoskeleton_core::ExoError;
use exoskeleton_core::WatchId;
use rusqlite::Connection;

pub struct SqliteWatchStore {
    conn: Mutex<Connection>,
}

impl SqliteWatchStore {
    pub fn open(db_path: impl AsRef<Path>) -> Result<Self, ExoError> {
        let conn = Connection::open(db_path.as_ref())
            .map_err(|e| ExoError::Storage(format!("open watches.db: {e}")))?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS watches (
                id TEXT PRIMARY KEY,
                data TEXT NOT NULL
            )",
        )
        .map_err(|e| ExoError::Storage(format!("create watches table: {e}")))?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }
}

impl WatchStore for SqliteWatchStore {
    fn save(&self, watch: &WatchDefinition) -> Result<(), ExoError> {
        let conn = self.conn.lock().unwrap();
        let data = serde_json::to_string(watch)
            .map_err(|e| ExoError::Storage(format!("serialize watch: {e}")))?;
        conn.execute(
            "INSERT OR REPLACE INTO watches (id, data) VALUES (?1, ?2)",
            rusqlite::params![watch.id.to_string(), data],
        )
        .map_err(|e| ExoError::Storage(format!("save watch: {e}")))?;
        Ok(())
    }

    fn get(&self, id: WatchId) -> Result<Option<WatchDefinition>, ExoError> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare("SELECT data FROM watches WHERE id = ?1")
            .map_err(|e| ExoError::Storage(format!("prepare get watch: {e}")))?;
        let result = stmt
            .query_row(rusqlite::params![id.to_string()], |row| {
                let data: String = row.get(0)?;
                Ok(data)
            })
            .optional()
            .map_err(|e| ExoError::Storage(format!("get watch: {e}")))?;
        match result {
            Some(data) => {
                let watch: WatchDefinition = serde_json::from_str(&data)
                    .map_err(|e| ExoError::Storage(format!("deserialize watch: {e}")))?;
                Ok(Some(watch))
            }
            None => Ok(None),
        }
    }

    fn list(&self) -> Result<Vec<WatchDefinition>, ExoError> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare("SELECT data FROM watches")
            .map_err(|e| ExoError::Storage(format!("prepare list watches: {e}")))?;
        let watches = stmt
            .query_map([], |row| {
                let data: String = row.get(0)?;
                Ok(data)
            })
            .map_err(|e| ExoError::Storage(format!("list watches: {e}")))?
            .filter_map(|r| r.ok())
            .filter_map(|data| serde_json::from_str(&data).ok())
            .collect();
        Ok(watches)
    }

    fn due_watches(&self, tick_number: u64) -> Result<Vec<WatchDefinition>, ExoError> {
        let all = self.list()?;
        let due = all
            .into_iter()
            .filter(|w| {
                if w.status != WatchStatus::Active {
                    return false;
                }
                match w.schedule {
                    WatchSchedule::Once => w.trigger_count == 0,
                    WatchSchedule::EveryNTicks { n } => {
                        let last = w.last_checked_tick.unwrap_or(w.created_at_tick);
                        tick_number.saturating_sub(last) >= n as u64
                    }
                }
            })
            .collect();
        Ok(due)
    }

    fn update_status(&self, id: WatchId, status: WatchStatus) -> Result<(), ExoError> {
        let mut watch = self
            .get(id)?
            .ok_or_else(|| ExoError::Storage(format!("watch {id} not found")))?;
        watch.status = status;
        self.save(&watch)
    }

    fn record_trigger(&self, id: WatchId, tick_number: u64) -> Result<(), ExoError> {
        let mut watch = self
            .get(id)?
            .ok_or_else(|| ExoError::Storage(format!("watch {id} not found")))?;
        watch.trigger_count += 1;
        watch.last_checked_tick = Some(tick_number);
        if watch.schedule == WatchSchedule::Once {
            watch.status = WatchStatus::Completed;
        }
        self.save(&watch)
    }

    fn record_check(&self, id: WatchId, tick_number: u64) -> Result<(), ExoError> {
        let mut watch = self
            .get(id)?
            .ok_or_else(|| ExoError::Storage(format!("watch {id} not found")))?;
        watch.last_checked_tick = Some(tick_number);
        self.save(&watch)
    }

    fn delete(&self, id: WatchId) -> Result<(), ExoError> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "DELETE FROM watches WHERE id = ?1",
            rusqlite::params![id.to_string()],
        )
        .map_err(|e| ExoError::Storage(format!("delete watch: {e}")))?;
        Ok(())
    }

    fn active_count(&self) -> Result<u32, ExoError> {
        let all = self.list()?;
        Ok(all
            .iter()
            .filter(|w| w.status == WatchStatus::Active)
            .count() as u32)
    }
}

/// Import for `query_row().optional()`.
use rusqlite::OptionalExtension;

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use exoskeleton_core::watch::{MetricKind, WatchCondition, WatchType};

    use super::*;

    fn make_threshold_watch(name: &str) -> WatchDefinition {
        WatchDefinition {
            id: WatchId::new(),
            name: name.into(),
            description: format!("Test watch: {name}"),
            watch_type: WatchType::Threshold {
                metric: MetricKind::ConsecutiveFailures,
                condition: WatchCondition::Above { value: 3.0 },
            },
            schedule: WatchSchedule::EveryNTicks { n: 1 },
            status: WatchStatus::Active,
            created_at_tick: 0,
            last_checked_tick: None,
            trigger_count: 0,
            created_at: Utc::now(),
        }
    }

    // ── E5S2-T2: watch_store_crud ──

    #[test]
    fn watch_store_crud() {
        let dir = tempfile::tempdir().unwrap();
        let store = SqliteWatchStore::open(dir.path().join("watches.db")).unwrap();

        let watch = make_threshold_watch("crud-test");

        // Save
        store.save(&watch).unwrap();

        // Get
        let retrieved = store.get(watch.id).unwrap().unwrap();
        assert_eq!(retrieved.name, "crud-test");

        // List
        assert_eq!(store.list().unwrap().len(), 1);

        // Active count
        assert_eq!(store.active_count().unwrap(), 1);

        // Update status
        store.update_status(watch.id, WatchStatus::Paused).unwrap();
        assert_eq!(
            store.get(watch.id).unwrap().unwrap().status,
            WatchStatus::Paused
        );
        assert_eq!(store.active_count().unwrap(), 0);

        // Delete
        store.delete(watch.id).unwrap();
        assert!(store.get(watch.id).unwrap().is_none());
        assert_eq!(store.list().unwrap().len(), 0);
    }

    // ── E5S2-T3: watch_store_due_watches ──

    #[test]
    fn watch_store_due_watches() {
        let dir = tempfile::tempdir().unwrap();
        let store = SqliteWatchStore::open(dir.path().join("watches.db")).unwrap();

        // EveryNTicks(5), created at tick 0
        let mut w1 = make_threshold_watch("every-5");
        w1.schedule = WatchSchedule::EveryNTicks { n: 5 };

        // Once watch
        let mut w2 = make_threshold_watch("once-watch");
        w2.schedule = WatchSchedule::Once;

        store.save(&w1).unwrap();
        store.save(&w2).unwrap();

        // Tick 3: EveryNTicks(5) not due (3 < 5), Once is due
        let due = store.due_watches(3).unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].name, "once-watch");

        // Tick 5: both due
        let due = store.due_watches(5).unwrap();
        assert_eq!(due.len(), 2);

        // Record trigger on Once → should complete
        store.record_trigger(w2.id, 5).unwrap();
        let w2_after = store.get(w2.id).unwrap().unwrap();
        assert_eq!(w2_after.status, WatchStatus::Completed);
        assert_eq!(w2_after.trigger_count, 1);

        // Once no longer due
        let due = store.due_watches(10).unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].name, "every-5");

        // Record check at tick 5
        store.record_check(w1.id, 5).unwrap();
        // Tick 9: not due (9-5=4 < 5)
        assert_eq!(store.due_watches(9).unwrap().len(), 0);
        // Tick 10: due (10-5=5 >= 5)
        assert_eq!(store.due_watches(10).unwrap().len(), 1);
    }
}

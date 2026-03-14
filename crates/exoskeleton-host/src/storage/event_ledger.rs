//! SQLite-backed append-only event ledger.

use std::path::Path;
use std::sync::Mutex;

use chrono::{DateTime, Utc};
use exoskeleton_core::{
    ArtifactId, EventEntry, EventLedger, EventType, ExoError, LedgerEntryId, TickId,
};
use rusqlite::Connection;

/// SQLite-backed append-only event ledger.
///
/// Events are written as they occur — once written, they are never modified or
/// deleted. No UPDATE or DELETE methods are exposed.
///
/// SQLite pragmas: WAL mode, `synchronous = FULL`, `busy_timeout = 5000`.
pub struct SqliteEventLedger {
    conn: Mutex<Connection>,
}

impl SqliteEventLedger {
    /// Open or create an event ledger at the given path.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, ExoError> {
        let conn = Connection::open(path.as_ref())
            .map_err(|e| ExoError::Storage(format!("event ledger open: {e}")))?;
        Self::initialize(conn)
    }

    /// Create an in-memory event ledger (for testing).
    pub fn in_memory() -> Result<Self, ExoError> {
        let conn = Connection::open_in_memory()
            .map_err(|e| ExoError::Storage(format!("event ledger in-memory: {e}")))?;
        Self::initialize(conn)
    }

    fn initialize(conn: Connection) -> Result<Self, ExoError> {
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA busy_timeout = 5000;
             PRAGMA synchronous = FULL;",
        )
        .map_err(|e| ExoError::Storage(format!("event ledger pragmas: {e}")))?;

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS events (
                id           TEXT NOT NULL PRIMARY KEY,
                tick_id      TEXT,
                event_type   TEXT NOT NULL,
                payload_ref  TEXT,
                summary      TEXT NOT NULL,
                timestamp    TEXT NOT NULL
            ) STRICT;",
        )
        .map_err(|e| ExoError::Storage(format!("event ledger schema: {e}")))?;

        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Parse a single row into an EventEntry.
    fn row_to_event_entry(row: &rusqlite::Row<'_>) -> Result<EventEntry, rusqlite::Error> {
        let id_str: String = row.get(0)?;
        let tick_id_str: Option<String> = row.get(1)?;
        let event_type_str: String = row.get(2)?;
        let payload_ref_str: Option<String> = row.get(3)?;
        let summary: String = row.get(4)?;
        let timestamp_str: String = row.get(5)?;

        // These conversions can fail, but we need to return rusqlite::Error.
        // Use InvalidColumnType for parse failures from the DB layer.
        let id: LedgerEntryId = id_str.parse().map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
        })?;

        let tick_id: Option<TickId> = tick_id_str
            .map(|s| {
                s.parse::<TickId>().map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        1,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })
            })
            .transpose()?;

        let event_type: EventType = serde_json::from_str(&event_type_str).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(2, rusqlite::types::Type::Text, Box::new(e))
        })?;

        let payload_ref: Option<ArtifactId> = payload_ref_str
            .map(|s| {
                s.parse::<ArtifactId>().map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        3,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })
            })
            .transpose()?;

        let timestamp: DateTime<Utc> = DateTime::parse_from_rfc3339(&timestamp_str)
            .map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    5,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })?
            .with_timezone(&Utc);

        Ok(EventEntry {
            id,
            tick_id,
            event_type,
            payload_ref,
            summary,
            timestamp,
        })
    }
}

impl EventLedger for SqliteEventLedger {
    fn append(&self, entry: &EventEntry) -> Result<LedgerEntryId, ExoError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| ExoError::Storage(format!("lock poisoned: {e}")))?;
        let event_type_str = serde_json::to_string(&entry.event_type)
            .map_err(|e| ExoError::Storage(format!("event_type serialization: {e}")))?;

        match conn.execute(
            "INSERT INTO events (id, tick_id, event_type, payload_ref, summary, timestamp)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                entry.id.to_string(),
                entry.tick_id.map(|t| t.to_string()),
                event_type_str,
                entry.payload_ref.as_ref().map(|r| r.as_str().to_string()),
                entry.summary,
                entry.timestamp.to_rfc3339(),
            ],
        ) {
            Ok(_) => Ok(entry.id),
            Err(rusqlite::Error::SqliteFailure(err, _))
                if err.code == rusqlite::ErrorCode::ConstraintViolation =>
            {
                Err(ExoError::Storage(format!(
                    "event entry already exists with id {}",
                    entry.id
                )))
            }
            Err(e) => Err(ExoError::Storage(format!("event append: {e}"))),
        }
    }

    fn recent(&self, limit: usize) -> Result<Vec<EventEntry>, ExoError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| ExoError::Storage(format!("lock poisoned: {e}")))?;
        let mut stmt = conn
            .prepare(
                "SELECT id, tick_id, event_type, payload_ref, summary, timestamp
                 FROM events ORDER BY timestamp DESC LIMIT ?1",
            )
            .map_err(|e| ExoError::Storage(format!("prepare: {e}")))?;

        let rows = stmt
            .query_map(rusqlite::params![limit], Self::row_to_event_entry)
            .map_err(|e| ExoError::Storage(format!("recent query: {e}")))?;

        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| ExoError::Storage(format!("recent row: {e}")))
    }

    fn for_tick(&self, tick_id: TickId) -> Result<Vec<EventEntry>, ExoError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| ExoError::Storage(format!("lock poisoned: {e}")))?;
        let mut stmt = conn
            .prepare(
                "SELECT id, tick_id, event_type, payload_ref, summary, timestamp
                 FROM events WHERE tick_id = ?1 ORDER BY timestamp ASC",
            )
            .map_err(|e| ExoError::Storage(format!("prepare: {e}")))?;

        let rows = stmt
            .query_map(
                rusqlite::params![tick_id.to_string()],
                Self::row_to_event_entry,
            )
            .map_err(|e| ExoError::Storage(format!("for_tick query: {e}")))?;

        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| ExoError::Storage(format!("for_tick row: {e}")))
    }

    fn by_type(&self, event_type: EventType, limit: usize) -> Result<Vec<EventEntry>, ExoError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| ExoError::Storage(format!("lock poisoned: {e}")))?;
        let event_type_str = serde_json::to_string(&event_type)
            .map_err(|e| ExoError::Storage(format!("event_type serialization: {e}")))?;

        let mut stmt = conn
            .prepare(
                "SELECT id, tick_id, event_type, payload_ref, summary, timestamp
                 FROM events WHERE event_type = ?1 ORDER BY timestamp DESC LIMIT ?2",
            )
            .map_err(|e| ExoError::Storage(format!("prepare: {e}")))?;

        let rows = stmt
            .query_map(
                rusqlite::params![event_type_str, limit],
                Self::row_to_event_entry,
            )
            .map_err(|e| ExoError::Storage(format!("by_type query: {e}")))?;

        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| ExoError::Storage(format!("by_type row: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_event(event_type: EventType, tick_id: Option<TickId>) -> EventEntry {
        EventEntry {
            id: LedgerEntryId::new(),
            tick_id,
            event_type,
            payload_ref: None,
            summary: format!("{event_type:?} event"),
            timestamp: Utc::now(),
        }
    }

    // ── T-3: Event Ledger ──

    #[test]
    fn append_and_recent() {
        let ledger = SqliteEventLedger::in_memory().unwrap();
        let entry = make_event(EventType::VesselStarted, None);
        let id = ledger.append(&entry).unwrap();
        assert_eq!(id, entry.id);

        let recent = ledger.recent(1).unwrap();
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].id, entry.id);
        assert_eq!(recent[0].event_type, EventType::VesselStarted);
    }

    #[test]
    fn recent_returns_newest_first() {
        let ledger = SqliteEventLedger::in_memory().unwrap();

        let ts_base = Utc::now();
        for i in 0..3 {
            let mut entry = make_event(EventType::TickStarted, Some(TickId::new()));
            entry.timestamp = ts_base + chrono::Duration::seconds(i);
            ledger.append(&entry).unwrap();
        }

        let recent = ledger.recent(3).unwrap();
        assert_eq!(recent.len(), 3);
        assert!(recent[0].timestamp >= recent[1].timestamp);
        assert!(recent[1].timestamp >= recent[2].timestamp);
    }

    #[test]
    fn recent_respects_limit() {
        let ledger = SqliteEventLedger::in_memory().unwrap();
        for _ in 0..5 {
            ledger
                .append(&make_event(EventType::TickCompleted, Some(TickId::new())))
                .unwrap();
        }
        let recent = ledger.recent(2).unwrap();
        assert_eq!(recent.len(), 2);
    }

    #[test]
    fn for_tick_returns_matching() {
        let ledger = SqliteEventLedger::in_memory().unwrap();
        let tick_a = TickId::new();
        let tick_b = TickId::new();

        ledger
            .append(&make_event(EventType::TickStarted, Some(tick_a)))
            .unwrap();
        ledger
            .append(&make_event(EventType::ActionExecuted, Some(tick_a)))
            .unwrap();
        ledger
            .append(&make_event(EventType::TickStarted, Some(tick_b)))
            .unwrap();

        let events = ledger.for_tick(tick_a).unwrap();
        assert_eq!(events.len(), 2);
        for e in &events {
            assert_eq!(e.tick_id, Some(tick_a));
        }
    }

    #[test]
    fn for_tick_empty_for_unknown() {
        let ledger = SqliteEventLedger::in_memory().unwrap();
        ledger
            .append(&make_event(EventType::VesselStarted, None))
            .unwrap();
        let events = ledger.for_tick(TickId::new()).unwrap();
        assert!(events.is_empty());
    }

    #[test]
    fn by_type_returns_matching() {
        let ledger = SqliteEventLedger::in_memory().unwrap();
        ledger
            .append(&make_event(EventType::TickStarted, Some(TickId::new())))
            .unwrap();
        ledger
            .append(&make_event(EventType::TickCompleted, Some(TickId::new())))
            .unwrap();
        ledger
            .append(&make_event(EventType::TickStarted, Some(TickId::new())))
            .unwrap();

        let starts = ledger.by_type(EventType::TickStarted, 10).unwrap();
        assert_eq!(starts.len(), 2);
        for e in &starts {
            assert_eq!(e.event_type, EventType::TickStarted);
        }
    }

    #[test]
    fn by_type_respects_limit() {
        let ledger = SqliteEventLedger::in_memory().unwrap();
        for _ in 0..5 {
            ledger
                .append(&make_event(EventType::TickStarted, Some(TickId::new())))
                .unwrap();
        }
        let starts = ledger.by_type(EventType::TickStarted, 2).unwrap();
        assert_eq!(starts.len(), 2);
    }

    // ── T-7: by_type returns empty for no matches ──

    #[test]
    fn by_type_returns_empty_for_no_matches() {
        let ledger = SqliteEventLedger::in_memory().unwrap();
        // Insert events of different types
        ledger
            .append(&make_event(EventType::TickStarted, Some(TickId::new())))
            .unwrap();
        ledger
            .append(&make_event(EventType::TickCompleted, Some(TickId::new())))
            .unwrap();
        // Query a type that has no entries
        let results = ledger.by_type(EventType::MessageReceived, 10).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn append_rejects_duplicate_id() {
        let ledger = SqliteEventLedger::in_memory().unwrap();
        let entry = make_event(EventType::VesselStarted, None);
        ledger.append(&entry).unwrap();
        let err = ledger.append(&entry).unwrap_err();
        assert!(
            matches!(err, ExoError::Storage(ref msg) if msg.contains("already exists")),
            "Expected duplicate rejection, got: {err}"
        );
    }

    #[test]
    fn nullable_tick_id() {
        let ledger = SqliteEventLedger::in_memory().unwrap();
        let entry = make_event(EventType::VesselStarted, None);
        assert!(entry.tick_id.is_none());

        ledger.append(&entry).unwrap();
        let recent = ledger.recent(1).unwrap();
        assert_eq!(recent[0].tick_id, None);
    }

    #[test]
    fn nullable_payload_ref() {
        let ledger = SqliteEventLedger::in_memory().unwrap();
        let entry = EventEntry {
            id: LedgerEntryId::new(),
            tick_id: Some(TickId::new()),
            event_type: EventType::ActionExecuted,
            payload_ref: None,
            summary: "Action without receipt".into(),
            timestamp: Utc::now(),
        };

        ledger.append(&entry).unwrap();
        let recent = ledger.recent(1).unwrap();
        assert!(recent[0].payload_ref.is_none());
    }

    #[test]
    fn all_event_types_roundtrip() {
        let ledger = SqliteEventLedger::in_memory().unwrap();
        let types = [
            EventType::VesselStarted,
            EventType::VesselStopped,
            EventType::TickStarted,
            EventType::TickCompleted,
            EventType::ActionExecuted,
            EventType::LlmCalled,
            EventType::ThreadRan,
            EventType::RelationshipUpdated,
            EventType::BudgetConsumed,
            EventType::Error,
        ];

        for et in &types {
            let entry = make_event(*et, Some(TickId::new()));
            ledger.append(&entry).unwrap();
        }

        let recent = ledger.recent(10).unwrap();
        assert_eq!(recent.len(), 10);

        // All types should be present
        let stored_types: Vec<EventType> = recent.iter().map(|e| e.event_type).collect();
        for et in &types {
            assert!(stored_types.contains(et), "Missing event type: {et:?}");
        }
    }
}

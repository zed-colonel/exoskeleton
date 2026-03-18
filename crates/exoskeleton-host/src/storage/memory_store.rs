//! SQLite-backed memory store for episodic summaries and long-term notes.

use std::path::Path;
use std::sync::Mutex;

use exoskeleton_core::{ArtifactId, EpisodicSummary, ExoError, LongTermNote};
use exoskeleton_memory::MemoryStore;
use rusqlite::Connection;

/// SQLite-backed memory store for episodic summaries and long-term notes.
///
/// Database location: `{data_dir}/exo/memory.db`
///
/// Both tables are in the same database file (unlike Sprint 2's one-file-per-store
/// pattern) because episodic and long-term memory are rarely written concurrently
/// and don't benefit from separate WALs. The single file simplifies management.
///
/// SQLite pragmas: WAL mode, `synchronous = FULL`, `busy_timeout = 5000`.
pub struct SqliteMemoryStore {
    conn: Mutex<Connection>,
}

impl SqliteMemoryStore {
    /// Open or create a memory store at the given path.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, ExoError> {
        let conn = Connection::open(path.as_ref())
            .map_err(|e| ExoError::Storage(format!("memory store open: {e}")))?;
        Self::initialize(conn)
    }

    /// Create an in-memory memory store (for testing).
    pub fn in_memory() -> Result<Self, ExoError> {
        let conn = Connection::open_in_memory()
            .map_err(|e| ExoError::Storage(format!("memory store in-memory: {e}")))?;
        Self::initialize(conn)
    }

    fn initialize(conn: Connection) -> Result<Self, ExoError> {
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA busy_timeout = 5000;
             PRAGMA synchronous = FULL;",
        )
        .map_err(|e| ExoError::Storage(format!("memory store pragmas: {e}")))?;

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS episodic_summaries (
                id           TEXT NOT NULL PRIMARY KEY,
                start_tick   INTEGER NOT NULL,
                end_tick     INTEGER NOT NULL,
                summary      TEXT NOT NULL,
                key_events   TEXT NOT NULL DEFAULT '[]',
                token_count  INTEGER NOT NULL,
                created_at   TEXT NOT NULL
            ) STRICT;

            CREATE TABLE IF NOT EXISTS long_term_notes (
                id           TEXT NOT NULL PRIMARY KEY,
                topic        TEXT NOT NULL,
                content      TEXT NOT NULL,
                tags         TEXT NOT NULL DEFAULT '[]',
                token_count  INTEGER NOT NULL,
                created_at   TEXT NOT NULL
            ) STRICT;",
        )
        .map_err(|e| ExoError::Storage(format!("memory store schema: {e}")))?;

        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Escape SQL LIKE wildcards in a search query (H-5).
    fn escape_like(query: &str) -> String {
        query
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_")
    }
}

impl MemoryStore for SqliteMemoryStore {
    fn write_episodic(&self, summary: &EpisodicSummary) -> Result<ArtifactId, ExoError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| ExoError::Storage(format!("lock poisoned: {e}")))?;

        let key_events_json = serde_json::to_string(&summary.key_events)
            .map_err(|e| ExoError::Storage(format!("key_events serialization: {e}")))?;

        conn.execute(
            "INSERT OR IGNORE INTO episodic_summaries
             (id, start_tick, end_tick, summary, key_events, token_count, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                summary.id.as_str(),
                summary.start_tick as i64,
                summary.end_tick as i64,
                summary.summary,
                key_events_json,
                summary.token_count as i64,
                summary.created_at.to_rfc3339(),
            ],
        )
        .map_err(|e| ExoError::Storage(format!("write_episodic: {e}")))?;

        Ok(summary.id.clone())
    }

    fn recent_episodic(&self, limit: usize) -> Result<Vec<EpisodicSummary>, ExoError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| ExoError::Storage(format!("lock poisoned: {e}")))?;

        let mut stmt = conn
            .prepare(
                "SELECT id, start_tick, end_tick, summary, key_events, token_count, created_at
                 FROM episodic_summaries
                 ORDER BY end_tick DESC
                 LIMIT ?1",
            )
            .map_err(|e| ExoError::Storage(format!("prepare: {e}")))?;

        let rows = stmt
            .query_map(rusqlite::params![limit as i64], |row| {
                Ok(EpisodicRow {
                    id: row.get(0)?,
                    start_tick: row.get(1)?,
                    end_tick: row.get(2)?,
                    summary: row.get(3)?,
                    key_events: row.get(4)?,
                    token_count: row.get(5)?,
                    created_at: row.get(6)?,
                })
            })
            .map_err(|e| ExoError::Storage(format!("recent_episodic query: {e}")))?;

        let mut results = Vec::new();
        for row in rows {
            let row = row.map_err(|e| ExoError::Storage(format!("row: {e}")))?;
            results.push(row.into_episodic_summary()?);
        }
        Ok(results)
    }

    fn write_long_term(&self, note: &LongTermNote) -> Result<ArtifactId, ExoError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| ExoError::Storage(format!("lock poisoned: {e}")))?;

        let tags_json = serde_json::to_string(&note.tags)
            .map_err(|e| ExoError::Storage(format!("tags serialization: {e}")))?;

        conn.execute(
            "INSERT OR IGNORE INTO long_term_notes
             (id, topic, content, tags, token_count, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                note.id.as_str(),
                note.topic,
                note.content,
                tags_json,
                note.token_count as i64,
                note.created_at.to_rfc3339(),
            ],
        )
        .map_err(|e| ExoError::Storage(format!("write_long_term: {e}")))?;

        Ok(note.id.clone())
    }

    fn search_long_term(&self, query: &str, limit: usize) -> Result<Vec<LongTermNote>, ExoError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| ExoError::Storage(format!("lock poisoned: {e}")))?;

        let escaped = Self::escape_like(query);

        let mut stmt = conn
            .prepare(
                "SELECT id, topic, content, tags, token_count, created_at
                 FROM long_term_notes
                 WHERE topic LIKE '%' || ?1 || '%' ESCAPE '\\'
                    OR content LIKE '%' || ?1 || '%' ESCAPE '\\'
                 ORDER BY created_at DESC
                 LIMIT ?2",
            )
            .map_err(|e| ExoError::Storage(format!("prepare: {e}")))?;

        let rows = stmt
            .query_map(rusqlite::params![escaped, limit as i64], |row| {
                Ok(LongTermRow {
                    id: row.get(0)?,
                    topic: row.get(1)?,
                    content: row.get(2)?,
                    tags: row.get(3)?,
                    token_count: row.get(4)?,
                    created_at: row.get(5)?,
                })
            })
            .map_err(|e| ExoError::Storage(format!("search_long_term query: {e}")))?;

        let mut results = Vec::new();
        for row in rows {
            let row = row.map_err(|e| ExoError::Storage(format!("row: {e}")))?;
            results.push(row.into_long_term_note()?);
        }
        Ok(results)
    }

    fn all_long_term(&self, limit: usize) -> Result<Vec<LongTermNote>, ExoError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| ExoError::Storage(format!("lock poisoned: {e}")))?;

        let mut stmt = conn
            .prepare(
                "SELECT id, topic, content, tags, token_count, created_at
                 FROM long_term_notes
                 ORDER BY created_at DESC
                 LIMIT ?1",
            )
            .map_err(|e| ExoError::Storage(format!("prepare: {e}")))?;

        let rows = stmt
            .query_map(rusqlite::params![limit as i64], |row| {
                Ok(LongTermRow {
                    id: row.get(0)?,
                    topic: row.get(1)?,
                    content: row.get(2)?,
                    tags: row.get(3)?,
                    token_count: row.get(4)?,
                    created_at: row.get(5)?,
                })
            })
            .map_err(|e| ExoError::Storage(format!("all_long_term query: {e}")))?;

        let mut results = Vec::new();
        for row in rows {
            let row = row.map_err(|e| ExoError::Storage(format!("row: {e}")))?;
            results.push(row.into_long_term_note()?);
        }
        Ok(results)
    }

    fn count_episodic(&self) -> Result<u64, ExoError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| ExoError::Storage(format!("lock poisoned: {e}")))?;
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM episodic_summaries", [], |row| {
                row.get(0)
            })
            .map_err(|e| ExoError::Storage(format!("count_episodic: {e}")))?;
        Ok(count as u64)
    }

    fn count_long_term(&self) -> Result<u64, ExoError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| ExoError::Storage(format!("lock poisoned: {e}")))?;
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM long_term_notes", [], |row| row.get(0))
            .map_err(|e| ExoError::Storage(format!("count_long_term: {e}")))?;
        Ok(count as u64)
    }

    fn evict_episodic_beyond(&self, capacity: u64) -> Result<u64, ExoError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| ExoError::Storage(format!("lock poisoned: {e}")))?;

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM episodic_summaries", [], |row| {
                row.get(0)
            })
            .map_err(|e| ExoError::Storage(format!("count_episodic: {e}")))?;

        let capacity = capacity as i64;
        if count <= capacity {
            return Ok(0);
        }

        let to_evict = count - capacity;

        conn.execute(
            "DELETE FROM episodic_summaries WHERE id IN (
                SELECT id FROM episodic_summaries ORDER BY end_tick ASC LIMIT ?1
            )",
            rusqlite::params![to_evict],
        )
        .map_err(|e| ExoError::Storage(format!("evict_episodic: {e}")))?;

        Ok(to_evict as u64)
    }
}

/// Intermediate row type for episodic summary deserialization.
struct EpisodicRow {
    id: String,
    start_tick: i64,
    end_tick: i64,
    summary: String,
    key_events: String,
    token_count: i64,
    created_at: String,
}

impl EpisodicRow {
    fn into_episodic_summary(self) -> Result<EpisodicSummary, ExoError> {
        let id: ArtifactId = self
            .id
            .parse()
            .map_err(|e| ExoError::Storage(format!("invalid artifact id: {e}")))?;
        let key_events: Vec<String> = serde_json::from_str(&self.key_events)
            .map_err(|e| ExoError::Storage(format!("key_events deserialization: {e}")))?;
        let created_at = chrono::DateTime::parse_from_rfc3339(&self.created_at)
            .map_err(|e| ExoError::Storage(format!("created_at parse: {e}")))?
            .with_timezone(&chrono::Utc);

        Ok(EpisodicSummary {
            id,
            start_tick: self.start_tick as u64,
            end_tick: self.end_tick as u64,
            summary: self.summary,
            key_events,
            token_count: self.token_count as u64,
            created_at,
        })
    }
}

/// Intermediate row type for long-term note deserialization.
struct LongTermRow {
    id: String,
    topic: String,
    content: String,
    tags: String,
    token_count: i64,
    created_at: String,
}

impl LongTermRow {
    fn into_long_term_note(self) -> Result<LongTermNote, ExoError> {
        let id: ArtifactId = self
            .id
            .parse()
            .map_err(|e| ExoError::Storage(format!("invalid artifact id: {e}")))?;
        let tags: Vec<String> = serde_json::from_str(&self.tags)
            .map_err(|e| ExoError::Storage(format!("tags deserialization: {e}")))?;
        let created_at = chrono::DateTime::parse_from_rfc3339(&self.created_at)
            .map_err(|e| ExoError::Storage(format!("created_at parse: {e}")))?
            .with_timezone(&chrono::Utc);

        Ok(LongTermNote {
            id,
            topic: self.topic,
            content: self.content,
            tags,
            token_count: self.token_count as u64,
            created_at,
        })
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use exoskeleton_core::ArtifactId;

    use super::*;

    fn make_episodic(start: u64, end: u64, summary: &str) -> EpisodicSummary {
        EpisodicSummary {
            id: ArtifactId::from_content(format!("ep-{start}-{end}").as_bytes()),
            start_tick: start,
            end_tick: end,
            summary: summary.into(),
            key_events: vec!["event1".into()],
            token_count: 20,
            created_at: Utc::now(),
        }
    }

    fn make_note(topic: &str, content: &str) -> LongTermNote {
        LongTermNote {
            id: ArtifactId::from_content(format!("{topic}-{content}").as_bytes()),
            topic: topic.into(),
            content: content.into(),
            tags: Vec::new(),
            token_count: 10,
            created_at: Utc::now(),
        }
    }

    // ── T-3: Memory Store ──

    #[test]
    fn write_and_read_episodic() {
        let store = SqliteMemoryStore::in_memory().unwrap();
        let summary = make_episodic(0, 5, "Initial phase");
        store.write_episodic(&summary).unwrap();

        let results = store.recent_episodic(1).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, summary.id);
        assert_eq!(results[0].start_tick, 0);
        assert_eq!(results[0].end_tick, 5);
        assert_eq!(results[0].summary, "Initial phase");
    }

    #[test]
    fn recent_episodic_newest_first() {
        let store = SqliteMemoryStore::in_memory().unwrap();
        store
            .write_episodic(&make_episodic(0, 2, "span 1"))
            .unwrap();
        store
            .write_episodic(&make_episodic(3, 5, "span 2"))
            .unwrap();
        store
            .write_episodic(&make_episodic(6, 8, "span 3"))
            .unwrap();

        let results = store.recent_episodic(3).unwrap();
        assert_eq!(results.len(), 3);
        assert_eq!(results[0].end_tick, 8); // newest first
        assert_eq!(results[1].end_tick, 5);
        assert_eq!(results[2].end_tick, 2);
    }

    #[test]
    fn recent_episodic_respects_limit() {
        let store = SqliteMemoryStore::in_memory().unwrap();
        for i in 0..5 {
            store
                .write_episodic(&make_episodic(i * 3, i * 3 + 2, &format!("span {i}")))
                .unwrap();
        }
        let results = store.recent_episodic(2).unwrap();
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn write_episodic_deduplicates() {
        let store = SqliteMemoryStore::in_memory().unwrap();
        let summary = make_episodic(0, 5, "dedup test");
        store.write_episodic(&summary).unwrap();
        store.write_episodic(&summary).unwrap(); // same id → no-op
        assert_eq!(store.count_episodic().unwrap(), 1);
    }

    #[test]
    fn write_and_read_long_term() {
        let store = SqliteMemoryStore::in_memory().unwrap();
        let note = make_note("architecture", "Dual AQ design");
        store.write_long_term(&note).unwrap();

        let results = store.all_long_term(10).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, note.id);
        assert_eq!(results[0].topic, "architecture");
        assert_eq!(results[0].content, "Dual AQ design");
    }

    #[test]
    fn all_long_term_newest_first() {
        let store = SqliteMemoryStore::in_memory().unwrap();
        // Use slightly different timestamps by varying content (same timestamps would be ambiguous)
        let note1 = LongTermNote {
            id: ArtifactId::from_content(b"lt1"),
            topic: "topic1".into(),
            content: "First note".into(),
            tags: Vec::new(),
            token_count: 10,
            created_at: chrono::DateTime::parse_from_rfc3339("2025-01-01T00:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
        };
        let note2 = LongTermNote {
            id: ArtifactId::from_content(b"lt2"),
            topic: "topic2".into(),
            content: "Second note".into(),
            tags: Vec::new(),
            token_count: 10,
            created_at: chrono::DateTime::parse_from_rfc3339("2025-01-02T00:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
        };
        let note3 = LongTermNote {
            id: ArtifactId::from_content(b"lt3"),
            topic: "topic3".into(),
            content: "Third note".into(),
            tags: Vec::new(),
            token_count: 10,
            created_at: chrono::DateTime::parse_from_rfc3339("2025-01-03T00:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
        };
        store.write_long_term(&note1).unwrap();
        store.write_long_term(&note2).unwrap();
        store.write_long_term(&note3).unwrap();

        let results = store.all_long_term(3).unwrap();
        assert_eq!(results.len(), 3);
        assert_eq!(results[0].topic, "topic3"); // newest first
        assert_eq!(results[1].topic, "topic2");
        assert_eq!(results[2].topic, "topic1");
    }

    #[test]
    fn all_long_term_respects_limit() {
        let store = SqliteMemoryStore::in_memory().unwrap();
        for i in 0..5 {
            store
                .write_long_term(&make_note(&format!("topic_{i}"), &format!("content {i}")))
                .unwrap();
        }
        let results = store.all_long_term(2).unwrap();
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn write_long_term_deduplicates() {
        let store = SqliteMemoryStore::in_memory().unwrap();
        let note = make_note("dedup", "test");
        store.write_long_term(&note).unwrap();
        store.write_long_term(&note).unwrap(); // same id → no-op
        assert_eq!(store.count_long_term().unwrap(), 1);
    }

    #[test]
    fn search_long_term_by_topic() {
        let store = SqliteMemoryStore::in_memory().unwrap();
        store
            .write_long_term(&make_note("architecture", "Design note"))
            .unwrap();
        store
            .write_long_term(&make_note("preferences", "User pref"))
            .unwrap();

        let results = store.search_long_term("arch", 10).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].topic, "architecture");
    }

    #[test]
    fn search_long_term_by_content() {
        let store = SqliteMemoryStore::in_memory().unwrap();
        store
            .write_long_term(&make_note("topic1", "Found a pattern in the data"))
            .unwrap();
        store
            .write_long_term(&make_note("topic2", "No patterns here"))
            .unwrap();

        let results = store.search_long_term("pattern", 10).unwrap();
        assert_eq!(results.len(), 2); // both match "pattern"
    }

    #[test]
    fn search_long_term_case_insensitive() {
        let store = SqliteMemoryStore::in_memory().unwrap();
        store
            .write_long_term(&make_note("Architecture", "Design doc"))
            .unwrap();

        let results = store.search_long_term("architecture", 10).unwrap();
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn search_long_term_no_results() {
        let store = SqliteMemoryStore::in_memory().unwrap();
        store
            .write_long_term(&make_note("topic", "content"))
            .unwrap();

        let results = store.search_long_term("nonexistent", 10).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn search_long_term_respects_limit() {
        let store = SqliteMemoryStore::in_memory().unwrap();
        for i in 0..5 {
            store
                .write_long_term(&make_note("same_topic", &format!("content {i}")))
                .unwrap();
        }
        let results = store.search_long_term("same_topic", 2).unwrap();
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn count_episodic_empty() {
        let store = SqliteMemoryStore::in_memory().unwrap();
        assert_eq!(store.count_episodic().unwrap(), 0);
    }

    #[test]
    fn count_long_term_empty() {
        let store = SqliteMemoryStore::in_memory().unwrap();
        assert_eq!(store.count_long_term().unwrap(), 0);
    }

    #[test]
    fn count_after_writes() {
        let store = SqliteMemoryStore::in_memory().unwrap();
        for i in 0..3 {
            store
                .write_episodic(&make_episodic(i * 3, i * 3 + 2, &format!("span {i}")))
                .unwrap();
        }
        for i in 0..2 {
            store
                .write_long_term(&make_note(&format!("topic_{i}"), &format!("content {i}")))
                .unwrap();
        }
        assert_eq!(store.count_episodic().unwrap(), 3);
        assert_eq!(store.count_long_term().unwrap(), 2);
    }

    #[test]
    fn episodic_complex_roundtrip() {
        let store = SqliteMemoryStore::in_memory().unwrap();
        let summary = EpisodicSummary {
            id: ArtifactId::from_content(b"complex-ep"),
            start_tick: 100,
            end_tick: 200,
            summary: "Complex summary with special chars: <>&\"'".into(),
            key_events: vec![
                "Event A occurred".into(),
                "Event B with special chars: <>&".into(),
                "Event C: final".into(),
            ],
            token_count: 50,
            created_at: Utc::now(),
        };
        store.write_episodic(&summary).unwrap();

        let results = store.recent_episodic(1).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, summary.id);
        assert_eq!(results[0].key_events.len(), 3);
        assert_eq!(results[0].summary, summary.summary);
    }

    #[test]
    fn long_term_complex_roundtrip() {
        let store = SqliteMemoryStore::in_memory().unwrap();
        let note = LongTermNote {
            id: ArtifactId::from_content(b"complex-lt"),
            topic: "complex/topic".into(),
            content: "Content with unicode: \u{4F60}\u{597D} and special chars: <>&".into(),
            tags: vec!["tag-1".into(), "tag_2".into(), "tag.3".into()],
            token_count: 30,
            created_at: Utc::now(),
        };
        store.write_long_term(&note).unwrap();

        let results = store.all_long_term(1).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, note.id);
        assert_eq!(results[0].tags.len(), 3);
        assert_eq!(results[0].content, note.content);
    }

    // ── T-8: Crash Simulation (File-Backed) ──

    #[test]
    fn episodic_survives_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("memory.db");

        let summary = make_episodic(0, 5, "survives reopen");
        {
            let store = SqliteMemoryStore::open(&path).unwrap();
            store.write_episodic(&summary).unwrap();
        } // drop → connection closed

        let store2 = SqliteMemoryStore::open(&path).unwrap();
        let results = store2.recent_episodic(1).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].summary, "survives reopen");
    }

    #[test]
    fn long_term_survives_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("memory.db");

        let note = make_note("persistence", "survives reopen");
        {
            let store = SqliteMemoryStore::open(&path).unwrap();
            store.write_long_term(&note).unwrap();
        }

        let store2 = SqliteMemoryStore::open(&path).unwrap();
        let results = store2.all_long_term(1).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].content, "survives reopen");
    }

    #[test]
    fn memory_store_sqlite_pragmas() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("memory.db");
        let store = SqliteMemoryStore::open(&path).unwrap();

        let conn = store.conn.lock().unwrap();

        let journal_mode: String = conn
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();
        assert_eq!(journal_mode, "wal");

        let synchronous: i64 = conn
            .query_row("PRAGMA synchronous", [], |row| row.get(0))
            .unwrap();
        assert_eq!(synchronous, 2); // FULL = 2

        let busy_timeout: i64 = conn
            .query_row("PRAGMA busy_timeout", [], |row| row.get(0))
            .unwrap();
        assert_eq!(busy_timeout, 5000);
    }

    // ── T-9: Edge Cases ──

    #[test]
    fn episodic_summary_unicode() {
        let store = SqliteMemoryStore::in_memory().unwrap();
        let summary = EpisodicSummary {
            id: ArtifactId::from_content(b"unicode-ep"),
            start_tick: 0,
            end_tick: 1,
            summary: "\u{4F60}\u{597D}\u{4E16}\u{754C} \u{1F600} Emoji test".into(),
            key_events: vec!["\u{2764} Heart event".into()],
            token_count: 10,
            created_at: Utc::now(),
        };
        store.write_episodic(&summary).unwrap();

        let results = store.recent_episodic(1).unwrap();
        assert_eq!(results[0].summary, summary.summary);
        assert_eq!(results[0].key_events[0], "\u{2764} Heart event");
    }

    #[test]
    fn long_term_note_large_content() {
        let store = SqliteMemoryStore::in_memory().unwrap();
        let large_content = "x".repeat(10_000);
        let note = LongTermNote {
            id: ArtifactId::from_content(large_content.as_bytes()),
            topic: "large".into(),
            content: large_content.clone(),
            tags: Vec::new(),
            token_count: 2500,
            created_at: Utc::now(),
        };
        store.write_long_term(&note).unwrap();

        let results = store.all_long_term(1).unwrap();
        assert_eq!(results[0].content.len(), 10_000);
    }

    #[test]
    fn long_term_note_many_tags() {
        let store = SqliteMemoryStore::in_memory().unwrap();
        let tags: Vec<String> = (0..50).map(|i| format!("tag_{i}")).collect();
        let note = LongTermNote {
            id: ArtifactId::from_content(b"many-tags"),
            topic: "test".into(),
            content: "Many tags test".into(),
            tags: tags.clone(),
            token_count: 10,
            created_at: Utc::now(),
        };
        store.write_long_term(&note).unwrap();

        let results = store.all_long_term(1).unwrap();
        assert_eq!(results[0].tags.len(), 50);
        assert_eq!(results[0].tags, tags);
    }

    #[test]
    fn search_special_characters() {
        let store = SqliteMemoryStore::in_memory().unwrap();
        store
            .write_long_term(&make_note("100% complete", "Value with _underscore"))
            .unwrap();
        store
            .write_long_term(&make_note("normal", "Normal content"))
            .unwrap();

        // Searching for "%" should not match everything (H-5 escape)
        let results = store.search_long_term("%", 10).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].topic, "100% complete");

        // Searching for "_" should not match single characters
        let results = store.search_long_term("_underscore", 10).unwrap();
        assert_eq!(results.len(), 1);
    }

    // ── T-10: Property-Based Tests ──

    proptest::proptest! {
        #[test]
        fn any_episodic_roundtrips(
            start_tick in 0u64..10_000,
            end_offset in 1u64..100,
            ref summary in "\\PC{1,200}",
            key_event_count in 0usize..5,
            token_count in 1u64..10_000,
        ) {
            let end_tick = start_tick + end_offset;
            let key_events: Vec<String> = (0..key_event_count)
                .map(|i| format!("event_{i}"))
                .collect();
            let content = serde_json::to_vec(&(start_tick, end_tick, summary, &key_events))
                .unwrap();
            let episodic = EpisodicSummary {
                id: ArtifactId::from_content(&content),
                start_tick,
                end_tick,
                summary: summary.clone(),
                key_events,
                token_count,
                created_at: Utc::now(),
            };
            let store = SqliteMemoryStore::in_memory().unwrap();
            store.write_episodic(&episodic).unwrap();
            let results = store.recent_episodic(1).unwrap();
            proptest::prop_assert_eq!(results.len(), 1);
            proptest::prop_assert_eq!(&results[0].id, &episodic.id);
            proptest::prop_assert_eq!(results[0].start_tick, episodic.start_tick);
            proptest::prop_assert_eq!(results[0].end_tick, episodic.end_tick);
            proptest::prop_assert_eq!(&results[0].summary, &episodic.summary);
            proptest::prop_assert_eq!(&results[0].key_events, &episodic.key_events);
            proptest::prop_assert_eq!(results[0].token_count, episodic.token_count);
        }

        #[test]
        fn any_long_term_roundtrips(
            ref topic in "[a-zA-Z0-9_\\-/]{1,50}",
            ref content in "\\PC{1,200}",
            tag_count in 0usize..5,
            token_count in 1u64..10_000,
        ) {
            let tags: Vec<String> = (0..tag_count)
                .map(|i| format!("tag_{i}"))
                .collect();
            let id_content = serde_json::to_vec(&(topic, content, &tags)).unwrap();
            let note = LongTermNote {
                id: ArtifactId::from_content(&id_content),
                topic: topic.clone(),
                content: content.clone(),
                tags,
                token_count,
                created_at: Utc::now(),
            };
            let store = SqliteMemoryStore::in_memory().unwrap();
            store.write_long_term(&note).unwrap();
            let results = store.all_long_term(1).unwrap();
            proptest::prop_assert_eq!(results.len(), 1);
            proptest::prop_assert_eq!(&results[0].id, &note.id);
            proptest::prop_assert_eq!(&results[0].topic, &note.topic);
            proptest::prop_assert_eq!(&results[0].content, &note.content);
            proptest::prop_assert_eq!(&results[0].tags, &note.tags);
            proptest::prop_assert_eq!(results[0].token_count, note.token_count);
        }
    }

    // ── E1-T77–E1-T80: Episodic Eviction Tests ──

    #[test]
    fn evict_episodic_beyond_capacity() {
        // E1-T77: 150 entries, capacity 100 → 50 evicted, 100 remain
        let store = SqliteMemoryStore::in_memory().unwrap();
        for i in 0..150u64 {
            let ep = EpisodicSummary {
                id: ArtifactId::from_content(format!("evict-ep-{i}").as_bytes()),
                start_tick: i * 2,
                end_tick: i * 2 + 1,
                summary: format!("Episode {i}"),
                key_events: vec![],
                token_count: 10,
                created_at: Utc::now(),
            };
            store.write_episodic(&ep).unwrap();
        }
        assert_eq!(store.count_episodic().unwrap(), 150);

        let evicted = store.evict_episodic_beyond(100).unwrap();
        assert_eq!(evicted, 50);
        assert_eq!(store.count_episodic().unwrap(), 100);
    }

    #[test]
    fn evict_episodic_below_capacity_noop() {
        // E1-T78: 80 entries, capacity 100 → no eviction
        let store = SqliteMemoryStore::in_memory().unwrap();
        for i in 0..80u64 {
            let ep = EpisodicSummary {
                id: ArtifactId::from_content(format!("noop-ep-{i}").as_bytes()),
                start_tick: i,
                end_tick: i + 1,
                summary: format!("Episode {i}"),
                key_events: vec![],
                token_count: 10,
                created_at: Utc::now(),
            };
            store.write_episodic(&ep).unwrap();
        }

        let evicted = store.evict_episodic_beyond(100).unwrap();
        assert_eq!(evicted, 0);
        assert_eq!(store.count_episodic().unwrap(), 80);
    }

    #[test]
    fn evict_episodic_oldest_entries_first() {
        // E1-T79: Oldest entries (lowest end_tick) evicted first
        let store = SqliteMemoryStore::in_memory().unwrap();
        for i in 0..10u64 {
            let ep = EpisodicSummary {
                id: ArtifactId::from_content(format!("order-ep-{i}").as_bytes()),
                start_tick: i * 10,
                end_tick: i * 10 + 9,
                summary: format!("Episode {i}"),
                key_events: vec![],
                token_count: 10,
                created_at: Utc::now(),
            };
            store.write_episodic(&ep).unwrap();
        }

        // Evict down to 5 → oldest 5 removed (end_ticks 9, 19, 29, 39, 49)
        let evicted = store.evict_episodic_beyond(5).unwrap();
        assert_eq!(evicted, 5);

        let remaining = store.recent_episodic(10).unwrap();
        assert_eq!(remaining.len(), 5);
        // Remaining should be the newest 5 (end_ticks 59, 69, 79, 89, 99)
        assert_eq!(remaining[0].end_tick, 99);
        assert_eq!(remaining[4].end_tick, 59);
    }

    #[test]
    fn evict_episodic_count_reflects_post_eviction() {
        // E1-T80: count_episodic() reflects post-eviction count
        let store = SqliteMemoryStore::in_memory().unwrap();
        for i in 0..20u64 {
            let ep = EpisodicSummary {
                id: ArtifactId::from_content(format!("count-ep-{i}").as_bytes()),
                start_tick: i,
                end_tick: i + 1,
                summary: format!("Episode {i}"),
                key_events: vec![],
                token_count: 10,
                created_at: Utc::now(),
            };
            store.write_episodic(&ep).unwrap();
        }

        assert_eq!(store.count_episodic().unwrap(), 20);
        store.evict_episodic_beyond(10).unwrap();
        assert_eq!(store.count_episodic().unwrap(), 10);
    }

    #[test]
    fn episodic_overlapping_tick_ranges() {
        let store = SqliteMemoryStore::in_memory().unwrap();
        let s1 = EpisodicSummary {
            id: ArtifactId::from_content(b"overlap-1"),
            start_tick: 0,
            end_tick: 10,
            summary: "First span".into(),
            key_events: Vec::new(),
            token_count: 10,
            created_at: Utc::now(),
        };
        let s2 = EpisodicSummary {
            id: ArtifactId::from_content(b"overlap-2"),
            start_tick: 5,
            end_tick: 15,
            summary: "Overlapping span".into(),
            key_events: Vec::new(),
            token_count: 10,
            created_at: Utc::now(),
        };
        store.write_episodic(&s1).unwrap();
        store.write_episodic(&s2).unwrap();

        let results = store.recent_episodic(10).unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].end_tick, 15); // newest first
        assert_eq!(results[1].end_tick, 10);
    }
}

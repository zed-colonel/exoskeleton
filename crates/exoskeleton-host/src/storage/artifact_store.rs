//! SQLite-backed content-addressed artifact store.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

use chrono::{DateTime, Utc};
use exoskeleton_core::{Artifact, ArtifactId, ArtifactKind, ArtifactRef, ArtifactStore, ExoError};
use rusqlite::Connection;

/// SQLite-backed content-addressed artifact store.
///
/// Content-addressing provides natural deduplication: identical content produces
/// identical IDs, and `put` uses `INSERT OR IGNORE` to silently skip duplicates.
///
/// SQLite pragmas: WAL mode, `synchronous = FULL`, `busy_timeout = 5000`.
pub struct SqliteArtifactStore {
    conn: Mutex<Connection>,
}

impl SqliteArtifactStore {
    /// Open or create an artifact store at the given path.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, ExoError> {
        let conn = Connection::open(path.as_ref())
            .map_err(|e| ExoError::Storage(format!("artifact store open: {e}")))?;
        Self::initialize(conn)
    }

    /// Create an in-memory artifact store (for testing).
    pub fn in_memory() -> Result<Self, ExoError> {
        let conn = Connection::open_in_memory()
            .map_err(|e| ExoError::Storage(format!("artifact store in-memory: {e}")))?;
        Self::initialize(conn)
    }

    fn initialize(conn: Connection) -> Result<Self, ExoError> {
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA busy_timeout = 5000;
             PRAGMA synchronous = FULL;",
        )
        .map_err(|e| ExoError::Storage(format!("artifact store pragmas: {e}")))?;

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS artifacts (
                id           TEXT NOT NULL PRIMARY KEY,
                kind         TEXT NOT NULL,
                content      BLOB NOT NULL,
                content_type TEXT NOT NULL,
                created_at   TEXT NOT NULL,
                metadata     TEXT NOT NULL DEFAULT '{}'
            ) STRICT;",
        )
        .map_err(|e| ExoError::Storage(format!("artifact store schema: {e}")))?;

        Ok(Self {
            conn: Mutex::new(conn),
        })
    }
}

impl ArtifactStore for SqliteArtifactStore {
    fn put(&self, artifact: &Artifact) -> Result<ArtifactId, ExoError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| ExoError::Storage(format!("lock poisoned: {e}")))?;
        let metadata_json = serde_json::to_string(&artifact.metadata)
            .map_err(|e| ExoError::Storage(format!("metadata serialization: {e}")))?;
        let kind_str = serde_json::to_string(&artifact.kind)
            .map_err(|e| ExoError::Storage(format!("kind serialization: {e}")))?;

        conn.execute(
            "INSERT OR IGNORE INTO artifacts (id, kind, content, content_type, created_at, metadata)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                artifact.id.as_str(),
                kind_str,
                artifact.content,
                artifact.content_type,
                artifact.created_at.to_rfc3339(),
                metadata_json,
            ],
        )
        .map_err(|e| ExoError::Storage(format!("artifact put: {e}")))?;

        Ok(artifact.id.clone())
    }

    fn get(&self, id: &ArtifactId) -> Result<Option<Artifact>, ExoError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| ExoError::Storage(format!("lock poisoned: {e}")))?;
        let mut stmt = conn
            .prepare(
                "SELECT id, kind, content, content_type, created_at, metadata
                 FROM artifacts WHERE id = ?1",
            )
            .map_err(|e| ExoError::Storage(format!("prepare: {e}")))?;

        let result = stmt.query_row(rusqlite::params![id.as_str()], |row| {
            let id_str: String = row.get(0)?;
            let kind_str: String = row.get(1)?;
            let content: Vec<u8> = row.get(2)?;
            let content_type: String = row.get(3)?;
            let created_at_str: String = row.get(4)?;
            let metadata_str: String = row.get(5)?;
            Ok((
                id_str,
                kind_str,
                content,
                content_type,
                created_at_str,
                metadata_str,
            ))
        });

        match result {
            Ok((id_str, kind_str, content, content_type, created_at_str, metadata_str)) => {
                let id = id_str
                    .parse::<ArtifactId>()
                    .map_err(|e| ExoError::Storage(format!("invalid artifact ID: {e}")))?;
                let kind: ArtifactKind = serde_json::from_str(&kind_str)
                    .map_err(|e| ExoError::Storage(format!("invalid kind: {e}")))?;
                let created_at = DateTime::parse_from_rfc3339(&created_at_str)
                    .map_err(|e| ExoError::Storage(format!("invalid timestamp: {e}")))?
                    .with_timezone(&Utc);
                let metadata: HashMap<String, String> = serde_json::from_str(&metadata_str)
                    .map_err(|e| ExoError::Storage(format!("invalid metadata: {e}")))?;

                Ok(Some(Artifact {
                    id,
                    kind,
                    content,
                    content_type,
                    created_at,
                    metadata,
                }))
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(ExoError::Storage(format!("artifact get: {e}"))),
        }
    }

    fn exists(&self, id: &ArtifactId) -> Result<bool, ExoError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| ExoError::Storage(format!("lock poisoned: {e}")))?;
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM artifacts WHERE id = ?1",
                rusqlite::params![id.as_str()],
                |row| row.get(0),
            )
            .map_err(|e| ExoError::Storage(format!("artifact exists: {e}")))?;
        Ok(count > 0)
    }

    fn list_by_kind(&self, kind: ArtifactKind, limit: usize) -> Result<Vec<ArtifactRef>, ExoError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| ExoError::Storage(format!("lock poisoned: {e}")))?;
        let kind_str = serde_json::to_string(&kind)
            .map_err(|e| ExoError::Storage(format!("kind serialization: {e}")))?;

        let mut stmt = conn
            .prepare("SELECT id FROM artifacts WHERE kind = ?1 ORDER BY created_at DESC LIMIT ?2")
            .map_err(|e| ExoError::Storage(format!("prepare: {e}")))?;

        let refs = stmt
            .query_map(rusqlite::params![kind_str, limit], |row| {
                let id_str: String = row.get(0)?;
                Ok(id_str)
            })
            .map_err(|e| ExoError::Storage(format!("list_by_kind query: {e}")))?
            .collect::<Result<Vec<String>, _>>()
            .map_err(|e| ExoError::Storage(format!("list_by_kind row: {e}")))?;

        refs.into_iter()
            .map(|id_str| {
                let id = id_str
                    .parse::<ArtifactId>()
                    .map_err(|e| ExoError::Storage(format!("invalid artifact ID: {e}")))?;
                Ok(ArtifactRef::new(id, kind))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── T-1: Artifact Store — Content Addressing ──

    #[test]
    fn put_and_get_roundtrip() {
        let store = SqliteArtifactStore::in_memory().unwrap();
        let artifact = Artifact::new(
            ArtifactKind::Snapshot,
            b"hello world".to_vec(),
            "text/plain".into(),
        );

        let id = store.put(&artifact).unwrap();
        assert_eq!(id, artifact.id);

        let retrieved = store.get(&id).unwrap().unwrap();
        assert_eq!(retrieved.id, artifact.id);
        assert_eq!(retrieved.kind, artifact.kind);
        assert_eq!(retrieved.content, artifact.content);
        assert_eq!(retrieved.content_type, artifact.content_type);
        assert_eq!(retrieved.created_at, artifact.created_at);
        assert_eq!(retrieved.metadata, artifact.metadata);
    }

    #[test]
    fn put_is_idempotent() {
        let store = SqliteArtifactStore::in_memory().unwrap();
        let artifact = Artifact::new(
            ArtifactKind::Plan,
            b"same content".to_vec(),
            "text/plain".into(),
        );

        let id1 = store.put(&artifact).unwrap();
        let id2 = store.put(&artifact).unwrap();
        assert_eq!(id1, id2);
    }

    #[test]
    fn put_deduplicates() {
        let store = SqliteArtifactStore::in_memory().unwrap();
        let a = Artifact::new(
            ArtifactKind::Receipt,
            b"dedup content".to_vec(),
            "text/plain".into(),
        );
        let b = Artifact::new(
            ArtifactKind::Receipt,
            b"dedup content".to_vec(),
            "text/plain".into(),
        );
        assert_eq!(a.id, b.id);

        store.put(&a).unwrap();
        store.put(&b).unwrap();

        // Only one row in DB
        let conn = store.conn.lock().unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM artifacts", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn get_nonexistent_returns_none() {
        let store = SqliteArtifactStore::in_memory().unwrap();
        let id = ArtifactId::from_content(b"nonexistent");
        let result = store.get(&id).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn exists_true_after_put() {
        let store = SqliteArtifactStore::in_memory().unwrap();
        let artifact = Artifact::new(
            ArtifactKind::Memory,
            b"exists test".to_vec(),
            "text/plain".into(),
        );
        store.put(&artifact).unwrap();
        assert!(store.exists(&artifact.id).unwrap());
    }

    #[test]
    fn exists_false_for_nonexistent() {
        let store = SqliteArtifactStore::in_memory().unwrap();
        let id = ArtifactId::from_content(b"nope");
        assert!(!store.exists(&id).unwrap());
    }

    #[test]
    fn list_by_kind_returns_matching() {
        let store = SqliteArtifactStore::in_memory().unwrap();

        for i in 0..3 {
            let a = Artifact::new(
                ArtifactKind::Snapshot,
                format!("snapshot-{i}").into_bytes(),
                "text/plain".into(),
            );
            store.put(&a).unwrap();
        }
        for i in 0..2 {
            let a = Artifact::new(
                ArtifactKind::Receipt,
                format!("receipt-{i}").into_bytes(),
                "text/plain".into(),
            );
            store.put(&a).unwrap();
        }

        let snapshots = store.list_by_kind(ArtifactKind::Snapshot, 100).unwrap();
        assert_eq!(snapshots.len(), 3);
        for r in &snapshots {
            assert_eq!(r.kind, ArtifactKind::Snapshot);
        }
    }

    #[test]
    fn list_by_kind_respects_limit() {
        let store = SqliteArtifactStore::in_memory().unwrap();
        for i in 0..5 {
            let a = Artifact::new(
                ArtifactKind::Plan,
                format!("plan-{i}").into_bytes(),
                "text/plain".into(),
            );
            store.put(&a).unwrap();
        }
        let plans = store.list_by_kind(ArtifactKind::Plan, 2).unwrap();
        assert_eq!(plans.len(), 2);
    }

    #[test]
    fn list_by_kind_newest_first() {
        let store = SqliteArtifactStore::in_memory().unwrap();

        let ts_a = Utc::now() - chrono::Duration::seconds(10);
        let ts_b = Utc::now();

        let a = Artifact::new_with_timestamp(
            ArtifactKind::Decision,
            b"older".to_vec(),
            "text/plain".into(),
            ts_a,
        );
        let b = Artifact::new_with_timestamp(
            ArtifactKind::Decision,
            b"newer".to_vec(),
            "text/plain".into(),
            ts_b,
        );

        store.put(&a).unwrap();
        store.put(&b).unwrap();

        let list = store.list_by_kind(ArtifactKind::Decision, 10).unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].id, b.id);
        assert_eq!(list[1].id, a.id);
    }

    #[test]
    fn list_by_kind_empty_for_unused_kind() {
        let store = SqliteArtifactStore::in_memory().unwrap();
        let a = Artifact::new(
            ArtifactKind::Snapshot,
            b"only snapshots".to_vec(),
            "text/plain".into(),
        );
        store.put(&a).unwrap();
        let receipts = store.list_by_kind(ArtifactKind::Receipt, 100).unwrap();
        assert!(receipts.is_empty());
    }

    #[test]
    fn put_preserves_metadata() {
        let store = SqliteArtifactStore::in_memory().unwrap();
        let artifact = Artifact::new(
            ArtifactKind::Memory,
            b"with metadata".to_vec(),
            "text/plain".into(),
        )
        .with_metadata("source", "test")
        .with_metadata("version", "1");

        store.put(&artifact).unwrap();
        let retrieved = store.get(&artifact.id).unwrap().unwrap();
        assert_eq!(retrieved.metadata.get("source"), Some(&"test".to_string()));
        assert_eq!(retrieved.metadata.get("version"), Some(&"1".to_string()));
        assert_eq!(retrieved.metadata.len(), 2);
    }

    #[test]
    fn put_preserves_empty_metadata() {
        let store = SqliteArtifactStore::in_memory().unwrap();
        let artifact = Artifact::new(
            ArtifactKind::Envelope,
            b"no metadata".to_vec(),
            "text/plain".into(),
        );
        assert!(artifact.metadata.is_empty());

        store.put(&artifact).unwrap();
        let retrieved = store.get(&artifact.id).unwrap().unwrap();
        assert!(retrieved.metadata.is_empty());
    }
}

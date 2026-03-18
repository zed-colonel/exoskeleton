//! SQLite-backed conversation store (E1-S2).
//!
//! Three-table schema for efficient query paths:
//! - `conversations` — main table with JSON data column
//! - `conversation_envelopes` — O(1) envelope→conversation lookups (Perceive hot path)
//! - `conversation_participants` — principal→conversation JOINs (W-27 forward compat)

use std::path::Path;
use std::sync::Mutex;

use chrono::{DateTime, Utc};
use exoskeleton_core::conversation::{
    Conversation, ConversationMessage, ConversationState, ConversationStore,
};
use exoskeleton_core::{ConversationId, EnvelopeId, ExoError, PrincipalId};
use rusqlite::Connection;

/// SQLite-backed conversation store.
///
/// Uses upsert semantics: `save()` creates or updates a conversation in a
/// single transaction. Three tables provide efficient query paths for the
/// Perceive grouping algorithm and future search endpoints.
pub struct SqliteConversationStore {
    conn: Mutex<Connection>,
}

impl SqliteConversationStore {
    /// Open or create a conversation store at the given path.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, ExoError> {
        let conn = Connection::open(path.as_ref())
            .map_err(|e| ExoError::Storage(format!("conversation store open: {e}")))?;
        Self::initialize(conn)
    }

    /// Create an in-memory conversation store (for testing).
    pub fn in_memory() -> Result<Self, ExoError> {
        let conn = Connection::open_in_memory()
            .map_err(|e| ExoError::Storage(format!("conversation store in-memory: {e}")))?;
        Self::initialize(conn)
    }

    fn initialize(conn: Connection) -> Result<Self, ExoError> {
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA busy_timeout = 5000;
             PRAGMA synchronous = FULL;",
        )
        .map_err(|e| ExoError::Storage(format!("conversation store pragmas: {e}")))?;

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS conversations (
                id         TEXT NOT NULL PRIMARY KEY,
                state      TEXT NOT NULL,
                topic      TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                data       TEXT NOT NULL
            ) STRICT;

            CREATE INDEX IF NOT EXISTS idx_conv_state
                ON conversations(state);
            CREATE INDEX IF NOT EXISTS idx_conv_updated_at
                ON conversations(updated_at DESC);

            CREATE TABLE IF NOT EXISTS conversation_envelopes (
                envelope_id     TEXT NOT NULL PRIMARY KEY,
                conversation_id TEXT NOT NULL
            ) STRICT;

            CREATE INDEX IF NOT EXISTS idx_ce_conversation
                ON conversation_envelopes(conversation_id);

            CREATE TABLE IF NOT EXISTS conversation_participants (
                conversation_id TEXT NOT NULL,
                principal_id    TEXT NOT NULL,
                PRIMARY KEY (conversation_id, principal_id)
            ) STRICT;

            CREATE INDEX IF NOT EXISTS idx_cp_principal
                ON conversation_participants(principal_id);",
        )
        .map_err(|e| ExoError::Storage(format!("conversation store schema: {e}")))?;

        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Parse a row from the conversations table into a Conversation.
    fn row_to_conversation(row: &rusqlite::Row<'_>) -> Result<Conversation, rusqlite::Error> {
        let id_str: String = row.get(0)?;
        let state_str: String = row.get(1)?;
        let topic: Option<String> = row.get(2)?;
        let created_at_str: String = row.get(3)?;
        let updated_at_str: String = row.get(4)?;
        let data_str: String = row.get(5)?;

        let id: ConversationId = id_str.parse().map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
        })?;

        let state: ConversationState =
            serde_json::from_str(&format!("\"{state_str}\"")).map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    1,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })?;

        let created_at: DateTime<Utc> = DateTime::parse_from_rfc3339(&created_at_str)
            .map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    3,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })?
            .with_timezone(&Utc);

        let updated_at: DateTime<Utc> = DateTime::parse_from_rfc3339(&updated_at_str)
            .map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    4,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })?
            .with_timezone(&Utc);

        #[derive(serde::Deserialize)]
        struct ConversationData {
            participants: Vec<PrincipalId>,
            message_refs: Vec<ConversationMessage>,
        }

        let data: ConversationData = serde_json::from_str(&data_str).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(5, rusqlite::types::Type::Text, Box::new(e))
        })?;

        Ok(Conversation {
            id,
            participants: data.participants,
            message_refs: data.message_refs,
            state,
            topic,
            created_at,
            updated_at,
        })
    }
}

fn state_to_str(state: ConversationState) -> &'static str {
    match state {
        ConversationState::Active => "active",
        ConversationState::Stale => "stale",
        ConversationState::Closed => "closed",
    }
}

impl ConversationStore for SqliteConversationStore {
    fn save(&self, conversation: &Conversation) -> Result<(), ExoError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| ExoError::Storage(format!("lock poisoned: {e}")))?;

        #[derive(serde::Serialize)]
        struct ConversationData<'a> {
            participants: &'a [PrincipalId],
            message_refs: &'a [ConversationMessage],
        }

        let data = ConversationData {
            participants: &conversation.participants,
            message_refs: &conversation.message_refs,
        };
        let data_json = serde_json::to_string(&data)
            .map_err(|e| ExoError::Storage(format!("conversation data serialization: {e}")))?;

        let tx = conn
            .unchecked_transaction()
            .map_err(|e| ExoError::Storage(format!("begin transaction: {e}")))?;

        // Upsert main conversation row
        tx.execute(
            "INSERT OR REPLACE INTO conversations (id, state, topic, created_at, updated_at, data)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                conversation.id.to_string(),
                state_to_str(conversation.state),
                conversation.topic.as_deref(),
                conversation.created_at.to_rfc3339(),
                conversation.updated_at.to_rfc3339(),
                data_json,
            ],
        )
        .map_err(|e| ExoError::Storage(format!("conversation upsert: {e}")))?;

        // Insert envelope mappings (IGNORE for idempotency on re-save)
        for msg in &conversation.message_refs {
            tx.execute(
                "INSERT OR IGNORE INTO conversation_envelopes (envelope_id, conversation_id)
                 VALUES (?1, ?2)",
                rusqlite::params![msg.envelope_id.to_string(), conversation.id.to_string(),],
            )
            .map_err(|e| ExoError::Storage(format!("envelope mapping insert: {e}")))?;
        }

        // Reconcile participant mappings (delete + reinsert — participant lists are small)
        tx.execute(
            "DELETE FROM conversation_participants WHERE conversation_id = ?1",
            rusqlite::params![conversation.id.to_string()],
        )
        .map_err(|e| ExoError::Storage(format!("participant cleanup: {e}")))?;

        for p in &conversation.participants {
            tx.execute(
                "INSERT INTO conversation_participants (conversation_id, principal_id)
                 VALUES (?1, ?2)",
                rusqlite::params![conversation.id.to_string(), p.to_string()],
            )
            .map_err(|e| ExoError::Storage(format!("participant insert: {e}")))?;
        }

        tx.commit()
            .map_err(|e| ExoError::Storage(format!("commit transaction: {e}")))?;

        Ok(())
    }

    fn get(&self, id: ConversationId) -> Result<Option<Conversation>, ExoError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| ExoError::Storage(format!("lock poisoned: {e}")))?;

        let mut stmt = conn
            .prepare(
                "SELECT id, state, topic, created_at, updated_at, data
                 FROM conversations WHERE id = ?1",
            )
            .map_err(|e| ExoError::Storage(format!("prepare: {e}")))?;

        let mut rows = stmt
            .query_map(rusqlite::params![id.to_string()], Self::row_to_conversation)
            .map_err(|e| ExoError::Storage(format!("get query: {e}")))?;

        match rows.next() {
            Some(Ok(conv)) => Ok(Some(conv)),
            Some(Err(e)) => Err(ExoError::Storage(format!("get row: {e}"))),
            None => Ok(None),
        }
    }

    fn find_by_envelope(
        &self,
        envelope_id: EnvelopeId,
    ) -> Result<Option<ConversationId>, ExoError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| ExoError::Storage(format!("lock poisoned: {e}")))?;

        let mut stmt = conn
            .prepare("SELECT conversation_id FROM conversation_envelopes WHERE envelope_id = ?1")
            .map_err(|e| ExoError::Storage(format!("prepare: {e}")))?;

        let mut rows = stmt
            .query_map(rusqlite::params![envelope_id.to_string()], |row| {
                let id_str: String = row.get(0)?;
                id_str.parse::<ConversationId>().map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        0,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })
            })
            .map_err(|e| ExoError::Storage(format!("find_by_envelope query: {e}")))?;

        match rows.next() {
            Some(Ok(id)) => Ok(Some(id)),
            Some(Err(e)) => Err(ExoError::Storage(format!("find_by_envelope row: {e}"))),
            None => Ok(None),
        }
    }

    fn find_by_principal(
        &self,
        principal_id: PrincipalId,
        limit: usize,
    ) -> Result<Vec<Conversation>, ExoError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| ExoError::Storage(format!("lock poisoned: {e}")))?;

        let mut stmt = conn
            .prepare(
                "SELECT c.id, c.state, c.topic, c.created_at, c.updated_at, c.data
                 FROM conversations c
                 JOIN conversation_participants cp ON c.id = cp.conversation_id
                 WHERE cp.principal_id = ?1
                 ORDER BY c.updated_at DESC LIMIT ?2",
            )
            .map_err(|e| ExoError::Storage(format!("prepare: {e}")))?;

        let limit_capped = std::cmp::min(limit, i32::MAX as usize) as i32;
        let rows = stmt
            .query_map(
                rusqlite::params![principal_id.to_string(), limit_capped],
                Self::row_to_conversation,
            )
            .map_err(|e| ExoError::Storage(format!("find_by_principal query: {e}")))?;

        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| ExoError::Storage(format!("find_by_principal row: {e}")))
    }

    fn active_conversations(&self, limit: usize) -> Result<Vec<Conversation>, ExoError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| ExoError::Storage(format!("lock poisoned: {e}")))?;

        let mut stmt = conn
            .prepare(
                "SELECT id, state, topic, created_at, updated_at, data
                 FROM conversations
                 WHERE state = 'active'
                 ORDER BY updated_at DESC LIMIT ?1",
            )
            .map_err(|e| ExoError::Storage(format!("prepare: {e}")))?;

        let limit_capped = std::cmp::min(limit, i32::MAX as usize) as i32;
        let rows = stmt
            .query_map(rusqlite::params![limit_capped], Self::row_to_conversation)
            .map_err(|e| ExoError::Storage(format!("active_conversations query: {e}")))?;

        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| ExoError::Storage(format!("active_conversations row: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use exoskeleton_core::conversation::{Conversation, ConversationState};
    use exoskeleton_core::{ArtifactId, EnvelopeId, PrincipalId};

    use super::*;

    // ── E1-T40: SQLite save and get roundtrip ──

    #[test]
    fn sqlite_save_and_get_roundtrip() {
        let store = SqliteConversationStore::in_memory().unwrap();
        let p = PrincipalId::new();
        let now = Utc::now();

        let mut conv = Conversation::from_first_message(
            p,
            EnvelopeId::new(),
            ArtifactId::from_content(b"m1"),
            now,
        );
        conv.topic = Some("Test topic".into());

        store.save(&conv).unwrap();
        let retrieved = store.get(conv.id).unwrap().unwrap();

        assert_eq!(retrieved.id, conv.id);
        assert_eq!(retrieved.participants, conv.participants);
        assert_eq!(retrieved.message_refs.len(), 1);
        assert_eq!(retrieved.state, ConversationState::Active);
        assert_eq!(retrieved.topic.as_deref(), Some("Test topic"));
        assert_eq!(
            retrieved.created_at.timestamp_millis(),
            conv.created_at.timestamp_millis()
        );

        // Non-existent returns None
        assert!(store.get(ConversationId::new()).unwrap().is_none());
    }

    // ── E1-T41: SQLite find_by_envelope ──

    #[test]
    fn sqlite_find_by_envelope() {
        let store = SqliteConversationStore::in_memory().unwrap();
        let p = PrincipalId::new();
        let env_id = EnvelopeId::new();

        let conv = Conversation::from_first_message(
            p,
            env_id,
            ArtifactId::from_content(b"payload"),
            Utc::now(),
        );
        store.save(&conv).unwrap();

        assert_eq!(store.find_by_envelope(env_id).unwrap(), Some(conv.id));
        assert!(store.find_by_envelope(EnvelopeId::new()).unwrap().is_none());
    }

    // ── E1-T42: SQLite find_by_principal ──

    #[test]
    fn sqlite_find_by_principal() {
        let store = SqliteConversationStore::in_memory().unwrap();
        let p1 = PrincipalId::new();
        let p2 = PrincipalId::new();

        let conv1 = Conversation::from_first_message(
            p1,
            EnvelopeId::new(),
            ArtifactId::from_content(b"c1"),
            Utc::now(),
        );
        let conv2 = Conversation::from_first_message(
            p2,
            EnvelopeId::new(),
            ArtifactId::from_content(b"c2"),
            Utc::now(),
        );
        store.save(&conv1).unwrap();
        store.save(&conv2).unwrap();

        let p1_convs = store.find_by_principal(p1, 10).unwrap();
        assert_eq!(p1_convs.len(), 1);
        assert_eq!(p1_convs[0].id, conv1.id);

        assert!(store
            .find_by_principal(PrincipalId::new(), 10)
            .unwrap()
            .is_empty());
    }

    // ── E1-T43: SQLite active_conversations ──

    #[test]
    fn sqlite_active_conversations_filter_and_order() {
        let store = SqliteConversationStore::in_memory().unwrap();
        let p = PrincipalId::new();
        let t0 = Utc::now();

        let conv1 = Conversation::from_first_message(
            p,
            EnvelopeId::new(),
            ArtifactId::from_content(b"c1"),
            t0,
        );
        let conv2 = Conversation::from_first_message(
            p,
            EnvelopeId::new(),
            ArtifactId::from_content(b"c2"),
            t0 + chrono::Duration::seconds(5),
        );
        let mut conv3 = Conversation::from_first_message(
            p,
            EnvelopeId::new(),
            ArtifactId::from_content(b"c3"),
            t0 + chrono::Duration::seconds(10),
        );
        conv3.state = ConversationState::Closed;

        store.save(&conv1).unwrap();
        store.save(&conv2).unwrap();
        store.save(&conv3).unwrap();

        let active = store.active_conversations(10).unwrap();
        assert_eq!(active.len(), 2);
        assert_eq!(active[0].id, conv2.id);
        assert_eq!(active[1].id, conv1.id);

        let limited = store.active_conversations(1).unwrap();
        assert_eq!(limited.len(), 1);
    }

    // ── E1-T44: SQLite upsert adds new messages ──

    #[test]
    fn sqlite_save_upsert_adds_messages() {
        let store = SqliteConversationStore::in_memory().unwrap();
        let p1 = PrincipalId::new();
        let p2 = PrincipalId::new();
        let t0 = Utc::now();

        let mut conv = Conversation::from_first_message(
            p1,
            EnvelopeId::new(),
            ArtifactId::from_content(b"m1"),
            t0,
        );
        store.save(&conv).unwrap();

        // Add a second message and re-save
        conv.add_message(
            p2,
            EnvelopeId::new(),
            ArtifactId::from_content(b"m2"),
            t0 + chrono::Duration::seconds(5),
        );
        store.save(&conv).unwrap();

        let retrieved = store.get(conv.id).unwrap().unwrap();
        assert_eq!(retrieved.message_refs.len(), 2);
        assert_eq!(retrieved.participants.len(), 2);
    }

    // ── E1-T45: SQLite reopen preserves data ──

    #[test]
    fn sqlite_reopen_preserves_data() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("conversations.db");
        let p = PrincipalId::new();
        let env_id = EnvelopeId::new();

        // Write
        {
            let store = SqliteConversationStore::open(&path).unwrap();
            let conv = Conversation::from_first_message(
                p,
                env_id,
                ArtifactId::from_content(b"durable"),
                Utc::now(),
            );
            store.save(&conv).unwrap();
        }

        // Reopen and verify
        let store = SqliteConversationStore::open(&path).unwrap();
        let found = store.find_by_envelope(env_id).unwrap();
        assert!(found.is_some());

        let active = store.active_conversations(10).unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].participants, vec![p]);
    }
}

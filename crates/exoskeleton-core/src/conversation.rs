//! First-class conversation abstraction for multi-turn interactions (W-13).
//!
//! A Conversation groups related MessageEnvelopes by threading chain
//! and principal affinity. Grouping happens in Perceive (where messages
//! arrive) rather than Orient (where context is compiled). This keeps
//! Orient focused on token-budgeted assembly (I5) and puts classification
//! logic at the ingestion point.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::id::{ArtifactId, ConversationId, EnvelopeId, PrincipalId};
use crate::ExoError;

/// A conversation grouping related message envelopes.
///
/// Participants can be any mix of human, agent, and vessel principals
/// (forward-compatible with W-34 vessel-to-vessel messaging).
/// Message history is stored as lightweight references — actual content
/// lives in the artifact store (I3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
pub struct Conversation {
    /// Unique identity of this conversation.
    pub id: ConversationId,
    /// All principals who have participated (any type: human, agent, vessel).
    pub participants: Vec<PrincipalId>,
    /// Ordered message history (oldest first).
    pub message_refs: Vec<ConversationMessage>,
    /// Current conversation state.
    pub state: ConversationState,
    /// Optional topic (derived or set explicitly).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub topic: Option<String>,
    /// When this conversation was created.
    pub created_at: DateTime<Utc>,
    /// When the last message was added.
    pub updated_at: DateTime<Utc>,
}

/// A lightweight reference to a message within a conversation.
///
/// Does NOT contain the message content — that lives in the artifact store
/// referenced by `payload_ref`. This keeps Conversation lightweight for
/// serialization and storage.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
pub struct ConversationMessage {
    /// The original envelope ID.
    pub envelope_id: EnvelopeId,
    /// Who sent this message.
    pub source: PrincipalId,
    /// Reference to the full payload in the artifact store.
    pub payload_ref: ArtifactId,
    /// When this message was sent.
    pub timestamp: DateTime<Utc>,
}

/// State of a conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum ConversationState {
    /// Expecting further messages.
    Active,
    /// No activity for configurable timeout. Can be reactivated.
    Stale,
    /// Concluded (manually or by the agent).
    Closed,
}

impl Conversation {
    /// Create a new conversation from a first message.
    pub fn from_first_message(
        source: PrincipalId,
        envelope_id: EnvelopeId,
        payload_ref: ArtifactId,
        timestamp: DateTime<Utc>,
    ) -> Self {
        Self {
            id: ConversationId::new(),
            participants: vec![source],
            message_refs: vec![ConversationMessage {
                envelope_id,
                source,
                payload_ref,
                timestamp,
            }],
            state: ConversationState::Active,
            topic: None,
            created_at: timestamp,
            updated_at: timestamp,
        }
    }

    /// Add a message to this conversation.
    ///
    /// Also adds the source to participants if not already present.
    pub fn add_message(
        &mut self,
        source: PrincipalId,
        envelope_id: EnvelopeId,
        payload_ref: ArtifactId,
        timestamp: DateTime<Utc>,
    ) {
        if !self.participants.contains(&source) {
            self.participants.push(source);
        }
        self.message_refs.push(ConversationMessage {
            envelope_id,
            source,
            payload_ref,
            timestamp,
        });
        self.updated_at = timestamp;
        // Reactivate if stale
        if self.state == ConversationState::Stale {
            self.state = ConversationState::Active;
        }
    }

    /// Number of messages in this conversation.
    pub fn message_count(&self) -> usize {
        self.message_refs.len()
    }
}

/// Persistent store for conversation state.
///
/// Follows the same pattern as ArtifactStore, SnapshotStore, TickStore,
/// BudgetStore — all defined in exoskeleton-core with `Arc<dyn` Store>
/// wiring in KernelContext.
pub trait ConversationStore: Send + Sync {
    /// Save or update a conversation (upsert semantics).
    fn save(&self, conversation: &Conversation) -> Result<(), ExoError>;

    /// Get a conversation by ID.
    fn get(&self, id: ConversationId) -> Result<Option<Conversation>, ExoError>;

    /// Find which conversation an envelope belongs to.
    fn find_by_envelope(&self, envelope_id: EnvelopeId)
        -> Result<Option<ConversationId>, ExoError>;

    /// Find conversations involving a specific principal.
    fn find_by_principal(
        &self,
        principal_id: PrincipalId,
        limit: usize,
    ) -> Result<Vec<Conversation>, ExoError>;

    /// Get all active conversations, ordered by updated_at descending.
    fn active_conversations(&self, limit: usize) -> Result<Vec<Conversation>, ExoError>;
}

/// In-memory implementation for unit testing.
pub struct InMemoryConversationStore {
    conversations: std::sync::Mutex<Vec<Conversation>>,
}

impl InMemoryConversationStore {
    pub fn new() -> Self {
        Self {
            conversations: std::sync::Mutex::new(Vec::new()),
        }
    }
}

impl Default for InMemoryConversationStore {
    fn default() -> Self {
        Self::new()
    }
}

impl ConversationStore for InMemoryConversationStore {
    fn save(&self, conversation: &Conversation) -> Result<(), ExoError> {
        let mut convs = self
            .conversations
            .lock()
            .map_err(|e| ExoError::Storage(format!("lock poisoned: {e}")))?;
        if let Some(existing) = convs.iter_mut().find(|c| c.id == conversation.id) {
            *existing = conversation.clone();
        } else {
            convs.push(conversation.clone());
        }
        Ok(())
    }

    fn get(&self, id: ConversationId) -> Result<Option<Conversation>, ExoError> {
        let convs = self
            .conversations
            .lock()
            .map_err(|e| ExoError::Storage(format!("lock poisoned: {e}")))?;
        Ok(convs.iter().find(|c| c.id == id).cloned())
    }

    fn find_by_envelope(
        &self,
        envelope_id: EnvelopeId,
    ) -> Result<Option<ConversationId>, ExoError> {
        let convs = self
            .conversations
            .lock()
            .map_err(|e| ExoError::Storage(format!("lock poisoned: {e}")))?;
        Ok(convs
            .iter()
            .find(|c| c.message_refs.iter().any(|m| m.envelope_id == envelope_id))
            .map(|c| c.id))
    }

    fn find_by_principal(
        &self,
        principal_id: PrincipalId,
        limit: usize,
    ) -> Result<Vec<Conversation>, ExoError> {
        let convs = self
            .conversations
            .lock()
            .map_err(|e| ExoError::Storage(format!("lock poisoned: {e}")))?;
        Ok(convs
            .iter()
            .filter(|c| c.participants.contains(&principal_id))
            .take(limit)
            .cloned()
            .collect())
    }

    fn active_conversations(&self, limit: usize) -> Result<Vec<Conversation>, ExoError> {
        let convs = self
            .conversations
            .lock()
            .map_err(|e| ExoError::Storage(format!("lock poisoned: {e}")))?;
        let mut active: Vec<_> = convs
            .iter()
            .filter(|c| c.state == ConversationState::Active)
            .cloned()
            .collect();
        active.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        active.truncate(limit);
        Ok(active)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::{ArtifactId, EnvelopeId, PrincipalId};

    fn make_msg() -> (PrincipalId, EnvelopeId, ArtifactId, DateTime<Utc>) {
        (
            PrincipalId::new(),
            EnvelopeId::new(),
            ArtifactId::from_content(format!("msg-{}", EnvelopeId::new()).as_bytes()),
            Utc::now(),
        )
    }

    // ── E1-T31: Conversation JSON roundtrip ──

    #[test]
    fn conversation_json_roundtrip() {
        let p1 = PrincipalId::new();
        let p2 = PrincipalId::new();
        let now = Utc::now();

        let mut conv = Conversation::from_first_message(
            p1,
            EnvelopeId::new(),
            ArtifactId::from_content(b"msg-1"),
            now,
        );
        conv.add_message(
            p2,
            EnvelopeId::new(),
            ArtifactId::from_content(b"msg-2"),
            now + chrono::Duration::seconds(5),
        );
        conv.topic = Some("Test topic".into());

        let json = serde_json::to_string(&conv).unwrap();
        let parsed: Conversation = serde_json::from_str(&json).unwrap();
        assert_eq!(conv, parsed);
        assert_eq!(parsed.participants.len(), 2);
        assert_eq!(parsed.message_refs.len(), 2);
        assert_eq!(parsed.topic.as_deref(), Some("Test topic"));
    }

    // ── E1-T32: ConversationState all 3 variants roundtrip ──

    #[test]
    fn conversation_state_roundtrip() {
        for (state, expected_json) in [
            (ConversationState::Active, "\"active\""),
            (ConversationState::Stale, "\"stale\""),
            (ConversationState::Closed, "\"closed\""),
        ] {
            let json = serde_json::to_string(&state).unwrap();
            assert_eq!(json, expected_json, "state: {state:?}");
            let parsed: ConversationState = serde_json::from_str(&json).unwrap();
            assert_eq!(state, parsed);
        }
    }

    // ── E1-T33: from_first_message ──

    #[test]
    fn from_first_message_creates_single_participant_and_message() {
        let p = PrincipalId::new();
        let env_id = EnvelopeId::new();
        let payload = ArtifactId::from_content(b"first");
        let now = Utc::now();

        let conv = Conversation::from_first_message(p, env_id, payload.clone(), now);
        assert_eq!(conv.participants, vec![p]);
        assert_eq!(conv.message_refs.len(), 1);
        assert_eq!(conv.message_refs[0].envelope_id, env_id);
        assert_eq!(conv.message_refs[0].source, p);
        assert_eq!(conv.message_refs[0].payload_ref, payload);
        assert_eq!(conv.state, ConversationState::Active);
        assert!(conv.topic.is_none());
        assert_eq!(conv.created_at, now);
        assert_eq!(conv.updated_at, now);
    }

    // ── E1-T34: add_message mutation logic ──

    #[test]
    fn add_message_updates_participants_timestamp_and_reactivates() {
        let p1 = PrincipalId::new();
        let p2 = PrincipalId::new();
        let t0 = Utc::now();
        let t1 = t0 + chrono::Duration::seconds(10);
        let t2 = t1 + chrono::Duration::seconds(10);

        let mut conv = Conversation::from_first_message(
            p1,
            EnvelopeId::new(),
            ArtifactId::from_content(b"m1"),
            t0,
        );

        // Add message from new participant
        conv.add_message(p2, EnvelopeId::new(), ArtifactId::from_content(b"m2"), t1);
        assert_eq!(conv.participants.len(), 2);
        assert!(conv.participants.contains(&p2));
        assert_eq!(conv.updated_at, t1);
        assert_eq!(conv.message_count(), 2);

        // Add message from existing participant (no duplicate)
        conv.add_message(p1, EnvelopeId::new(), ArtifactId::from_content(b"m3"), t2);
        assert_eq!(conv.participants.len(), 2);
        assert_eq!(conv.updated_at, t2);
        assert_eq!(conv.message_count(), 3);

        // Test stale reactivation
        conv.state = ConversationState::Stale;
        conv.add_message(
            p1,
            EnvelopeId::new(),
            ArtifactId::from_content(b"m4"),
            t2 + chrono::Duration::seconds(10),
        );
        assert_eq!(conv.state, ConversationState::Active);
    }

    // ── E1-T35: InMemoryConversationStore save and get ──

    #[test]
    fn inmemory_store_save_and_get() {
        let store = InMemoryConversationStore::new();
        let (p, env_id, payload, ts) = make_msg();

        let conv = Conversation::from_first_message(p, env_id, payload, ts);
        store.save(&conv).unwrap();

        let retrieved = store.get(conv.id).unwrap().unwrap();
        assert_eq!(retrieved, conv);

        // Non-existent returns None
        assert!(store.get(ConversationId::new()).unwrap().is_none());
    }

    // ── E1-T36: InMemoryConversationStore find_by_envelope ──

    #[test]
    fn inmemory_store_find_by_envelope() {
        let store = InMemoryConversationStore::new();
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

    // ── E1-T37: InMemoryConversationStore find_by_principal ──

    #[test]
    fn inmemory_store_find_by_principal() {
        let store = InMemoryConversationStore::new();
        let p1 = PrincipalId::new();
        let p2 = PrincipalId::new();
        let p3 = PrincipalId::new();

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

        // p3 has no conversations
        assert!(store.find_by_principal(p3, 10).unwrap().is_empty());
    }

    // ── E1-T38: InMemoryConversationStore active_conversations ──

    #[test]
    fn inmemory_store_active_conversations() {
        let store = InMemoryConversationStore::new();
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
        assert_eq!(active.len(), 2); // conv3 is Closed
                                     // Newest first
        assert_eq!(active[0].id, conv2.id);
        assert_eq!(active[1].id, conv1.id);

        // Limit works
        let limited = store.active_conversations(1).unwrap();
        assert_eq!(limited.len(), 1);
        assert_eq!(limited[0].id, conv2.id);
    }

    // ── E1-T39: ts-rs generates valid TypeScript ──

    #[test]
    fn ts_conversation_types_generate() {
        use ts_rs::TS;
        let cfg = ts_rs::Config::default();

        let conv_decl = Conversation::decl(&cfg);
        assert!(conv_decl.contains("Conversation"), "decl: {conv_decl}");
        assert!(conv_decl.contains("participants"), "decl: {conv_decl}");

        let msg_decl = ConversationMessage::decl(&cfg);
        assert!(msg_decl.contains("ConversationMessage"), "decl: {msg_decl}");
        assert!(msg_decl.contains("envelope_id"), "decl: {msg_decl}");

        let state_decl = ConversationState::decl(&cfg);
        assert!(
            state_decl.contains("ConversationState"),
            "decl: {state_decl}"
        );
        assert!(state_decl.contains("active"), "decl: {state_decl}");
        assert!(state_decl.contains("stale"), "decl: {state_decl}");
        assert!(state_decl.contains("closed"), "decl: {state_decl}");
    }
}

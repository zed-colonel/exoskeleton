//! Content-addressed artifact model.
//!
//! All meaningful state in Exoskeleton is stored as artifacts: snapshots,
//! plans, receipts, thread outputs, relationship entries, memory, decisions,
//! and envelopes.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::id::ArtifactId;
use crate::ExoError;

/// Content-addressed immutable record.
///
/// All meaningful state in Exoskeleton is stored as artifacts: snapshots,
/// plans, receipts, thread outputs, relationship entries, memory, decisions,
/// and envelopes. Content-addressing via `ArtifactId` (SHA-256 of `content`)
/// provides deduplication and tamper detection (I3: replayable).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
pub struct Artifact {
    /// Content-addressed ID (SHA-256 of `content`).
    pub id: ArtifactId,
    /// What kind of artifact this is.
    pub kind: ArtifactKind,
    /// Serialized payload bytes.
    pub content: Vec<u8>,
    /// MIME type of the content (e.g., "application/json").
    pub content_type: String,
    /// When this artifact was created.
    pub created_at: DateTime<Utc>,
    /// Extensible key-value metadata tags.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub metadata: HashMap<String, String>,
}

impl Artifact {
    /// Create a new artifact. Computes the ArtifactId from content.
    pub fn new(kind: ArtifactKind, content: Vec<u8>, content_type: String) -> Self {
        let id = ArtifactId::from_content(&content);
        Self {
            id,
            kind,
            content,
            content_type,
            created_at: Utc::now(),
            metadata: HashMap::new(),
        }
    }

    /// Create a new artifact with a specific timestamp (for tests, replay).
    pub fn new_with_timestamp(
        kind: ArtifactKind,
        content: Vec<u8>,
        content_type: String,
        created_at: DateTime<Utc>,
    ) -> Self {
        let id = ArtifactId::from_content(&content);
        Self {
            id,
            kind,
            content,
            content_type,
            created_at,
            metadata: HashMap::new(),
        }
    }

    /// Add a metadata entry. Returns self for chaining.
    pub fn with_metadata(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.metadata.insert(key.into(), value.into());
        self
    }

    /// Create an artifact from a JSON-serializable value.
    /// Serializes the value, computes ArtifactId from the serialized bytes.
    pub fn from_json<T: Serialize>(
        kind: ArtifactKind,
        value: &T,
    ) -> Result<Self, serde_json::Error> {
        let content = serde_json::to_vec(value)?;
        Ok(Self::new(kind, content, "application/json".to_string()))
    }
}

/// Classification of artifact content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    /// A State Snapshot (the single authoritative view of vessel state).
    Snapshot,
    /// A plan or strategy document.
    Plan,
    /// A receipt from a tool invocation (produced by the Tool AQ via WI Host).
    Receipt,
    /// An output artifact from a cognitive thread (produced on Cognitive AQ).
    ThreadOutput,
    /// An entry in the Relationship Ledger.
    RelationshipEntry,
    /// A memory entry (episodic summary or long-term note).
    Memory,
    /// A decision record from the Decide step (includes LLM response artifact).
    Decision,
    /// A message envelope (inbound or outbound communication).
    Envelope,
    /// An LLM response artifact (Sprint 4). Stored for replay (I3).
    /// Contains a serialized `LlmResponse` as JSON.
    LlmResponse,
    /// A TickRecord artifact (Sprint 5). Contains a serialized TickRecord as JSON.
    Tick,
    /// A ContextBreakdown artifact (E3-S2). Contains a serialized CompiledContext as JSON.
    /// Shows per-section token allocation from the Orient step.
    ContextBreakdown,
    /// An event payload artifact (E4-S4). Contains serialized event-specific data as JSON.
    /// Used for CapabilityRequest payloads and other structured event details.
    Event,
    /// Agent-proposed plan draft during planning mode.
    PlanDraft,
    /// Operator approval linking to a PlanDraft artifact.
    PlanApproved,
    /// Structured diff output from a mutating code tool, or a per-tick diff summary.
    CodeDiff,
}

/// Lightweight reference to an artifact (id + kind) for embedding in other structures.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
pub struct ArtifactRef {
    /// Content-addressed ID.
    pub id: ArtifactId,
    /// What kind of artifact this references.
    pub kind: ArtifactKind,
}

impl ArtifactRef {
    /// Create a new artifact reference.
    pub fn new(id: ArtifactId, kind: ArtifactKind) -> Self {
        Self { id, kind }
    }
}

/// Durable store for content-addressed artifacts.
///
/// All meaningful state in Exoskeleton is stored as artifacts (I3). Both
/// cognitive work (LLM responses, decisions, snapshots) and tool execution
/// (receipts) produce artifacts. The store is unified — there is one artifact
/// store shared across both domains.
///
/// Content-addressed: `put` computes the ArtifactId from content. If an
/// artifact with the same content already exists, `put` is a no-op (returns
/// the existing ArtifactId). This provides natural deduplication.
pub trait ArtifactStore: Send + Sync {
    /// Store an artifact. Returns the content-addressed ArtifactId.
    ///
    /// If an artifact with the same content (same ArtifactId) already exists,
    /// this is a no-op and returns the existing ID. This is intentional:
    /// content-addressed storage is naturally idempotent.
    fn put(&self, artifact: &Artifact) -> Result<ArtifactId, ExoError>;

    /// Retrieve an artifact by its content-addressed ID.
    ///
    /// Returns `None` if no artifact with this ID exists.
    fn get(&self, id: &ArtifactId) -> Result<Option<Artifact>, ExoError>;

    /// Check whether an artifact exists without loading its content.
    fn exists(&self, id: &ArtifactId) -> Result<bool, ExoError>;

    /// List artifact references of a specific kind, most recent first.
    ///
    /// Returns lightweight `ArtifactRef` entries (id + kind), not full artifacts.
    /// Use `get()` to load the full content when needed.
    fn list_by_kind(&self, kind: ArtifactKind, limit: usize) -> Result<Vec<ArtifactRef>, ExoError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── T-5: Artifact Model ──

    #[test]
    fn artifact_content_addressing() {
        let content = b"same content".to_vec();
        let a = Artifact::new(ArtifactKind::Snapshot, content.clone(), "text/plain".into());
        let b = Artifact::new(ArtifactKind::Snapshot, content, "text/plain".into());
        assert_eq!(a.id, b.id);
    }

    #[test]
    fn artifact_different_content() {
        let a = Artifact::new(ArtifactKind::Plan, b"alpha".to_vec(), "text/plain".into());
        let b = Artifact::new(ArtifactKind::Plan, b"bravo".to_vec(), "text/plain".into());
        assert_ne!(a.id, b.id);
    }

    #[test]
    fn artifact_json_roundtrip() {
        let artifact = Artifact::new(
            ArtifactKind::Decision,
            b"test payload".to_vec(),
            "text/plain".into(),
        )
        .with_metadata("source", "test");

        let json = serde_json::to_string(&artifact).unwrap();
        let parsed: Artifact = serde_json::from_str(&json).unwrap();
        assert_eq!(artifact, parsed);
    }

    #[test]
    fn artifact_from_json() {
        let value = serde_json::json!({"key": "value"});
        let artifact = Artifact::from_json(ArtifactKind::Snapshot, &value).unwrap();
        assert_eq!(artifact.content_type, "application/json");
        // Content should be valid JSON
        let parsed: serde_json::Value = serde_json::from_slice(&artifact.content).unwrap();
        assert_eq!(parsed, value);
    }

    #[test]
    fn artifact_with_metadata() {
        let artifact = Artifact::new(ArtifactKind::Memory, b"data".to_vec(), "text/plain".into())
            .with_metadata("key", "val");
        assert_eq!(artifact.metadata.get("key"), Some(&"val".to_string()));
    }

    #[test]
    fn artifact_kind_all_variants_roundtrip() {
        let variants = [
            ArtifactKind::Snapshot,
            ArtifactKind::Plan,
            ArtifactKind::Receipt,
            ArtifactKind::ThreadOutput,
            ArtifactKind::RelationshipEntry,
            ArtifactKind::Memory,
            ArtifactKind::Decision,
            ArtifactKind::Envelope,
            ArtifactKind::LlmResponse,
            ArtifactKind::Tick,
            ArtifactKind::ContextBreakdown,
            ArtifactKind::Event,
            ArtifactKind::PlanDraft,
            ArtifactKind::PlanApproved,
            ArtifactKind::CodeDiff,
        ];
        for kind in &variants {
            let json = serde_json::to_string(kind).unwrap();
            let parsed: ArtifactKind = serde_json::from_str(&json).unwrap();
            assert_eq!(*kind, parsed);
        }
    }

    #[test]
    fn artifact_kind_plan_variants() {
        assert_eq!(
            serde_json::to_string(&ArtifactKind::PlanDraft).unwrap(),
            "\"plan_draft\""
        );
        assert_eq!(
            serde_json::to_string(&ArtifactKind::PlanApproved).unwrap(),
            "\"plan_approved\""
        );
        assert_eq!(
            serde_json::to_string(&ArtifactKind::CodeDiff).unwrap(),
            "\"code_diff\""
        );
    }

    #[test]
    fn artifact_ref_roundtrip() {
        let r = ArtifactRef::new(ArtifactId::from_content(b"ref test"), ArtifactKind::Receipt);
        let json = serde_json::to_string(&r).unwrap();
        let parsed: ArtifactRef = serde_json::from_str(&json).unwrap();
        assert_eq!(r, parsed);
    }

    #[test]
    fn artifact_new_with_timestamp() {
        let ts = Utc::now();
        let artifact = Artifact::new_with_timestamp(
            ArtifactKind::Plan,
            b"timed".to_vec(),
            "text/plain".into(),
            ts,
        );
        assert_eq!(artifact.created_at, ts);
    }
}

//! Memory domain types — vocabulary for the three-tier memory hierarchy.
//!
//! These types are pure domain model with no I/O or compilation logic.
//! They belong in `exoskeleton-core` as part of the universal vocabulary.
//!
//! The three tiers:
//! - **Working** (tier 0): ephemeral per-tick context, not persisted
//! - **Episodic** (tier 1): compressed summaries of tick spans, medium retention
//! - **LongTerm** (tier 2): persistent knowledge and insights, indefinite retention

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::id::ArtifactId;

/// Classification of memory tiers in the memory hierarchy.
///
/// The three tiers correspond to different retention and access patterns:
/// - Working: ephemeral per-tick context, not persisted
/// - Episodic: compressed summaries of tick spans, medium retention
/// - LongTerm: persistent knowledge and insights, indefinite retention
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryTier {
    /// Current-tick ephemeral context. Not persisted across ticks.
    /// Represented by the ContextSources struct in the compiler, not stored in DB.
    Working,
    /// Compressed summary of a span of ticks. Written by the Memory
    /// Consolidation thread (Sprint 7) or the master loop's Amend step (Sprint 5).
    Episodic,
    /// Persistent knowledge, insights, and learned patterns. Written by the
    /// Memory Consolidation thread or explicitly by the master loop.
    LongTerm,
}

/// Compressed summary of a span of cognitive ticks.
///
/// Episodic summaries are produced by the Memory Consolidation thread (Sprint 7)
/// by summarizing a range of TickRecords. They compress the detailed tick history
/// into a more compact form suitable for inclusion in the context window.
///
/// Each episodic summary is also stored as an Artifact (ArtifactKind::Memory)
/// for content-addressing and replay (I3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
pub struct EpisodicSummary {
    /// Content-addressed ID of this summary when stored as an artifact.
    pub id: ArtifactId,
    /// First tick number covered by this summary (inclusive).
    pub start_tick: u64,
    /// Last tick number covered by this summary (inclusive).
    pub end_tick: u64,
    /// Natural-language summary of what happened during this span.
    pub summary: String,
    /// Notable events or decisions during this span.
    pub key_events: Vec<String>,
    /// Pre-computed token count of the summary text (for budget allocation).
    pub token_count: u64,
    /// When this summary was created.
    pub created_at: DateTime<Utc>,
}

/// Persistent knowledge entry in long-term memory.
///
/// Long-term notes capture durable insights, learned patterns, and important
/// facts that should persist indefinitely. They are written by the Memory
/// Consolidation thread or explicitly by the master loop when significant
/// insights emerge.
///
/// Each note is also stored as an Artifact (ArtifactKind::Memory) for
/// content-addressing and replay (I3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
pub struct LongTermNote {
    /// Content-addressed ID of this note when stored as an artifact.
    pub id: ArtifactId,
    /// Topic category for this note (e.g., "architecture", "user-preferences").
    pub topic: String,
    /// Natural-language content of the note.
    pub content: String,
    /// Searchable tags for retrieval.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// Pre-computed token count of the content (for budget allocation).
    pub token_count: u64,
    /// When this note was created.
    pub created_at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── T-1: Memory Domain Types ──

    #[test]
    fn memory_tier_all_variants_roundtrip() {
        let variants = [
            MemoryTier::Working,
            MemoryTier::Episodic,
            MemoryTier::LongTerm,
        ];
        for tier in &variants {
            let json = serde_json::to_string(tier).unwrap();
            let parsed: MemoryTier = serde_json::from_str(&json).unwrap();
            assert_eq!(*tier, parsed);
        }
    }

    #[test]
    fn memory_tier_snake_case() {
        assert_eq!(
            serde_json::to_string(&MemoryTier::Working).unwrap(),
            "\"working\""
        );
        assert_eq!(
            serde_json::to_string(&MemoryTier::Episodic).unwrap(),
            "\"episodic\""
        );
        assert_eq!(
            serde_json::to_string(&MemoryTier::LongTerm).unwrap(),
            "\"long_term\""
        );
    }

    #[test]
    fn episodic_summary_roundtrip_full() {
        let summary = EpisodicSummary {
            id: ArtifactId::from_content(b"episodic test"),
            start_tick: 10,
            end_tick: 20,
            summary: "Processed batch of analysis tasks".into(),
            key_events: vec![
                "Completed initial scan".into(),
                "Identified 3 anomalies".into(),
            ],
            token_count: 42,
            created_at: Utc::now(),
        };
        let json = serde_json::to_string(&summary).unwrap();
        let parsed: EpisodicSummary = serde_json::from_str(&json).unwrap();
        assert_eq!(summary, parsed);
    }

    #[test]
    fn episodic_summary_empty_key_events() {
        let summary = EpisodicSummary {
            id: ArtifactId::from_content(b"empty events"),
            start_tick: 0,
            end_tick: 5,
            summary: "Quiet period".into(),
            key_events: Vec::new(),
            token_count: 10,
            created_at: Utc::now(),
        };
        let json = serde_json::to_string(&summary).unwrap();
        let parsed: EpisodicSummary = serde_json::from_str(&json).unwrap();
        assert_eq!(summary, parsed);
        // key_events should be present as [] (no skip_serializing_if on this field)
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(value.get("key_events").is_some());
    }

    #[test]
    fn long_term_note_roundtrip_full() {
        let note = LongTermNote {
            id: ArtifactId::from_content(b"lt note"),
            topic: "architecture".into(),
            content: "The system uses dual ActionQueue engines".into(),
            tags: vec!["design".into(), "invariant".into()],
            token_count: 25,
            created_at: Utc::now(),
        };
        let json = serde_json::to_string(&note).unwrap();
        let parsed: LongTermNote = serde_json::from_str(&json).unwrap();
        assert_eq!(note, parsed);
    }

    #[test]
    fn long_term_note_empty_tags_omitted() {
        let note = LongTermNote {
            id: ArtifactId::from_content(b"no tags"),
            topic: "test".into(),
            content: "A note without tags".into(),
            tags: Vec::new(),
            token_count: 10,
            created_at: Utc::now(),
        };
        let value = serde_json::to_value(&note).unwrap();
        let obj = value.as_object().unwrap();
        assert!(!obj.contains_key("tags"), "Empty tags should be omitted");
    }

    #[test]
    fn long_term_note_roundtrip_minimal() {
        let note = LongTermNote {
            id: ArtifactId::from_content(b"minimal"),
            topic: "test".into(),
            content: "Minimal note".into(),
            tags: Vec::new(),
            token_count: 5,
            created_at: Utc::now(),
        };
        let json = serde_json::to_string(&note).unwrap();
        let parsed: LongTermNote = serde_json::from_str(&json).unwrap();
        assert_eq!(note, parsed);
    }
}

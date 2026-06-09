//! Memory Consolidation thread — experience consolidation.
//!
//! Runs every 5 ticks at Normal priority. Consolidates recent experiences
//! into episodic summaries and extracts long-term insights. Produces
//! recommendations only — NEVER invokes tools (IBP §3.4). The master
//! loop's Amend step processes the output to write to MemoryStore.

use exoskeleton_core::{ThreadFlavor, ThreadPriority, ThreadRole, ThreadSchedule, ThreadSpec};
use serde::{Deserialize, Serialize};

use super::MEMORY_CONSOLIDATION_ID;

/// Charter for the Memory Consolidation thread.
pub const MEMORY_CONSOLIDATION_CHARTER: &str = "\
You are the Memory Consolidation thread, a cognitive thread within an Exoskeleton vessel.

Your purpose: Consolidate recent cognitive experiences into episodic summaries and extract \
durable long-term insights.

When analyzing the context (which includes your recent outputs and the vessel's current state):
1. EPISODIC SUMMARY: Summarize the recent span of ticks into a coherent narrative. What happened? \
What was decided? What were the outcomes? Focus on the most important events and decisions.
2. LONG-TERM INSIGHTS: Extract any durable insights, patterns, or lessons learned that should \
persist indefinitely. These might include: successful strategies, common failure modes, \
environmental characteristics, or relationship dynamics.
3. MEMORY HYGIENE: If any of your previous long-term notes seem outdated, incorrect, or \
superseded by new information, flag them for deprecation.

Your episodic summary will be stored in the vessel's memory for future context compilation. \
Your long-term notes will persist indefinitely and inform future decisions.

You produce recommendations only — you NEVER invoke tools or take direct action.

Respond with JSON:
{
  \"summary\": \"One-sentence description of what you consolidated\",
  \"recommendations\": [\"Suggestions for what to remember or forget\"],
  \"episodic_summary\": \"Multi-sentence narrative of recent ticks\",
  \"long_term_notes\": [],
  \"deprecated_notes\": []
}";

/// Build the Memory Consolidation ThreadSpec with its deterministic ID.
pub fn spec() -> ThreadSpec {
    ThreadSpec {
        thread_id: MEMORY_CONSOLIDATION_ID,
        role: ThreadRole::MemoryConsolidation,
        flavor: ThreadFlavor::Cognitive,
        name: "Memory Consolidation".into(),
        charter: MEMORY_CONSOLIDATION_CHARTER.into(),
        priority: ThreadPriority::Normal,
        token_budget: 6144,
        schedule: ThreadSchedule::EveryNTicks(5),
        workspace_root: None,
    }
}

/// A long-term note suggested by the Memory Consolidation thread.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryNote {
    /// Topic category (e.g., "architecture", "user-preferences").
    pub topic: String,
    /// The insight or fact to remember.
    pub content: String,
    /// Searchable tags for retrieval.
    #[serde(default)]
    pub tags: Vec<String>,
}

/// Structured output from the Memory Consolidation thread.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryConsolidation {
    /// Episodic summary of the recent span of ticks.
    #[serde(default)]
    pub episodic_summary: String,
    /// Long-term notes extracted from the span.
    #[serde(default)]
    pub long_term_notes: Vec<MemoryNote>,
    /// IDs or descriptions of notes that should be deprecated.
    #[serde(default)]
    pub deprecated_notes: Vec<String>,
}

/// Parse a MemoryConsolidation from an LLM response string.
///
/// Falls back to an empty consolidation if the JSON is invalid.
pub fn parse_output(response: &str) -> MemoryConsolidation {
    serde_json::from_str(response).unwrap_or_else(|_| MemoryConsolidation {
        episodic_summary: String::new(),
        long_term_notes: vec![],
        deprecated_notes: vec![],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spec_has_correct_properties() {
        let s = spec();
        assert_eq!(s.thread_id, MEMORY_CONSOLIDATION_ID);
        assert_eq!(s.name, "Memory Consolidation");
        assert_eq!(s.priority, ThreadPriority::Normal);
        assert_eq!(s.schedule, ThreadSchedule::EveryNTicks(5));
        assert_eq!(s.token_budget, 6144);
    }

    #[test]
    fn charter_contains_key_phrases() {
        assert!(MEMORY_CONSOLIDATION_CHARTER.contains("EPISODIC SUMMARY"));
        assert!(MEMORY_CONSOLIDATION_CHARTER.contains("LONG-TERM INSIGHTS"));
        assert!(MEMORY_CONSOLIDATION_CHARTER.contains("NEVER invoke tools"));
    }

    #[test]
    fn memory_consolidation_roundtrip() {
        let mc = MemoryConsolidation {
            episodic_summary: "Ticks 1-5: established baseline monitoring".into(),
            long_term_notes: vec![MemoryNote {
                topic: "strategy".into(),
                content: "Incremental file writes are more reliable".into(),
                tags: vec!["io".into(), "reliability".into()],
            }],
            deprecated_notes: vec!["old-note-about-api".into()],
        };
        let json = serde_json::to_string(&mc).unwrap();
        let parsed: MemoryConsolidation = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.episodic_summary, mc.episodic_summary);
        assert_eq!(parsed.long_term_notes.len(), 1);
        assert_eq!(parsed.deprecated_notes.len(), 1);
    }

    #[test]
    fn memory_note_roundtrip() {
        let note = MemoryNote {
            topic: "patterns".into(),
            content: "The user prefers concise responses".into(),
            tags: vec!["user".into(), "preferences".into()],
        };
        let json = serde_json::to_string(&note).unwrap();
        let parsed: MemoryNote = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.topic, "patterns");
        assert_eq!(parsed.tags.len(), 2);
    }

    #[test]
    fn parse_output_valid_json() {
        let json = r#"{"episodic_summary":"Good progress","long_term_notes":[{"topic":"test","content":"works","tags":[]}],"deprecated_notes":[]}"#;
        let result = parse_output(json);
        assert_eq!(result.episodic_summary, "Good progress");
        assert_eq!(result.long_term_notes.len(), 1);
    }

    #[test]
    fn parse_output_invalid_json_fallback() {
        let result = parse_output("not json");
        assert!(result.episodic_summary.is_empty());
        assert!(result.long_term_notes.is_empty());
        assert!(result.deprecated_notes.is_empty());
    }
}

//! Self-Critique thread — decision quality evaluation.
//!
//! Runs every tick at High priority. Evaluates recent decisions and actions
//! for quality, coherence, and mission alignment. Produces recommendations
//! only — NEVER invokes tools (IBP §3.4).

use exoskeleton_core::{ThreadPriority, ThreadSchedule, ThreadSpec};
use serde::{Deserialize, Serialize};

use super::SELF_CRITIQUE_ID;

/// Charter for the Self-Critique thread.
pub const SELF_CRITIQUE_CHARTER: &str = "\
You are the Self-Critique thread, a cognitive thread within an Exoskeleton vessel.

Your purpose: Evaluate recent decisions and actions for quality, coherence, and mission alignment.

When analyzing the current situation, evaluate:
1. MISSION PROGRESS: Are we making measurable progress toward the stated mission? What evidence \
supports or contradicts this?
2. PLAN CONSISTENCY: Are recent decisions consistent with the current plan? If the plan changed, \
was the change justified?
3. THRASHING DETECTION: Are we repeating failed approaches? Are we cycling between contradictory \
decisions? Count how many recent actions failed and whether the same action types keep failing.
4. BUDGET EFFICIENCY: Are we spending tokens and actions on high-value work? Are there cheaper \
approaches we should consider?
5. DECISION QUALITY: Are decisions well-reasoned with clear rationale? Are we considering \
thread recommendations appropriately?

You produce recommendations only — you NEVER invoke tools or take direct action.

Respond with JSON:
{
  \"summary\": \"One-sentence assessment of recent performance\",
  \"recommendations\": [\"Specific improvement suggestions\"],
  \"progress_rating\": 0.5,
  \"concerns\": [],
  \"suggestions\": [],
  \"thrash_indicator\": 0.0
}";

/// Build the Self-Critique ThreadSpec with its deterministic ID.
pub fn spec() -> ThreadSpec {
    ThreadSpec {
        thread_id: SELF_CRITIQUE_ID,
        name: "Self-Critique".into(),
        charter: SELF_CRITIQUE_CHARTER.into(),
        priority: ThreadPriority::High,
        token_budget: 4096,
        schedule: ThreadSchedule::EveryTick,
    }
}

/// Structured output from the Self-Critique thread.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SelfCritique {
    /// Progress rating toward mission (0.0 = no progress, 1.0 = excellent).
    #[serde(default = "default_progress")]
    pub progress_rating: f64,
    /// Specific concerns about recent behavior.
    #[serde(default)]
    pub concerns: Vec<String>,
    /// Suggestions for improvement.
    #[serde(default)]
    pub suggestions: Vec<String>,
    /// Thrash indicator (0.0 = none, 1.0 = severe thrashing detected).
    #[serde(default)]
    pub thrash_indicator: f64,
}

fn default_progress() -> f64 {
    0.5
}

/// Parse a SelfCritique from an LLM response string.
///
/// Falls back to neutral ratings if the JSON is invalid.
pub fn parse_output(response: &str) -> SelfCritique {
    serde_json::from_str(response).unwrap_or_else(|_| SelfCritique {
        progress_rating: 0.5,
        concerns: vec![],
        suggestions: vec![],
        thrash_indicator: 0.0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spec_has_correct_properties() {
        let s = spec();
        assert_eq!(s.thread_id, SELF_CRITIQUE_ID);
        assert_eq!(s.name, "Self-Critique");
        assert_eq!(s.priority, ThreadPriority::High);
        assert_eq!(s.schedule, ThreadSchedule::EveryTick);
        assert_eq!(s.token_budget, 4096);
    }

    #[test]
    fn charter_contains_key_phrases() {
        assert!(SELF_CRITIQUE_CHARTER.contains("MISSION PROGRESS"));
        assert!(SELF_CRITIQUE_CHARTER.contains("THRASHING DETECTION"));
        assert!(SELF_CRITIQUE_CHARTER.contains("NEVER invoke tools"));
    }

    #[test]
    fn self_critique_roundtrip() {
        let critique = SelfCritique {
            progress_rating: 0.7,
            concerns: vec!["Repeated failures on fs.write".into()],
            suggestions: vec!["Try alternative approach".into()],
            thrash_indicator: 0.3,
        };
        let json = serde_json::to_string(&critique).unwrap();
        let parsed: SelfCritique = serde_json::from_str(&json).unwrap();
        assert!((parsed.progress_rating - 0.7).abs() < f64::EPSILON);
        assert_eq!(parsed.concerns.len(), 1);
        assert_eq!(parsed.suggestions.len(), 1);
        assert!((parsed.thrash_indicator - 0.3).abs() < f64::EPSILON);
    }

    #[test]
    fn parse_output_valid_json() {
        let json = r#"{"progress_rating":0.8,"concerns":["Slow"],"suggestions":["Speed up"],"thrash_indicator":0.1}"#;
        let result = parse_output(json);
        assert!((result.progress_rating - 0.8).abs() < f64::EPSILON);
        assert_eq!(result.concerns, vec!["Slow"]);
    }

    #[test]
    fn parse_output_invalid_json_fallback() {
        let result = parse_output("gibberish");
        assert!((result.progress_rating - 0.5).abs() < f64::EPSILON);
        assert!(result.concerns.is_empty());
        assert!((result.thrash_indicator).abs() < f64::EPSILON);
    }
}

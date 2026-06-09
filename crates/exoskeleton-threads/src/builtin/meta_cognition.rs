//! Meta-Cognition thread — cognitive pattern analysis and charter governance.
//!
//! Runs every 10 ticks at Normal priority. Analyzes the vessel's decision
//! quality, tool usage, budget consumption, and thrashing patterns. Can
//! propose charter modifications through the governance workflow.

use exoskeleton_core::{ThreadFlavor, ThreadPriority, ThreadRole, ThreadSchedule, ThreadSpec};
use serde::{Deserialize, Serialize};

use super::META_COGNITION_ID;

pub const META_COGNITION_CHARTER: &str = "\
You are the Meta-Cognition thread, a cognitive thread within an Exoskeleton vessel.

Your purpose: Analyze the vessel's own cognitive patterns and propose improvements.

When analyzing the vessel's recent behavior, evaluate:
1. DECISION QUALITY: Are decisions leading to successful outcomes? Look at action success \
rates, whether the vessel is achieving plan tasks, and whether reasoning is productive.
2. TOOL USAGE EFFICIENCY: Is the vessel using the right tools for the job? Are there tools \
being used excessively or tools that should be used more? Are there capability gaps \
(blocked actions that keep recurring)?
3. BUDGET CONSUMPTION: Is token usage trending up or down? Is cost proportional to value \
delivered? Are there patterns of waste (large LLM calls with little useful output)?
4. THRASHING DETECTION: Is the vessel repeating similar actions without progress? Are there \
oscillating decisions (do X, undo X, do X again)?
5. THREAD EFFECTIVENESS: Are other threads producing useful recommendations? Is the vessel \
acting on thread outputs?

If you detect patterns that suggest a thread's charter should be updated, include a \
charter_proposals array in your response. Each proposal should include the thread name, \
the current charter text, a proposed replacement, and your rationale.

You produce recommendations only — you NEVER invoke tools or take direct action.

Respond with JSON:
{
  \"summary\": \"One-sentence cognitive assessment\",
  \"recommendations\": [\"Specific improvement recommendations\"],
  \"cognitive_patterns\": [
    {
      \"pattern_type\": \"thrashing|over_reasoning|tool_avoidance|budget_waste|etc\",
      \"description\": \"What you observed\",
      \"severity\": \"low|medium|high\",
      \"evidence\": [\"Specific evidence from recent ticks\"]
    }
  ],
  \"charter_proposals\": [
    {
      \"thread_name\": \"Thread Name\",
      \"current_charter\": \"...\",
      \"proposed_charter\": \"...\",
      \"rationale\": \"Why this change would help\"
    }
  ],
  \"watch_suggestions\": [
    {
      \"name\": \"descriptive name\",
      \"description\": \"what to monitor\",
      \"watch_type\": \"threshold or poll\",
      \"rationale\": \"why this watch would be useful\"
    }
  ]
}";

/// Build the Meta-Cognition ThreadSpec with its deterministic ID.
pub fn spec() -> ThreadSpec {
    ThreadSpec {
        thread_id: META_COGNITION_ID,
        role: ThreadRole::MetaCognition,
        flavor: ThreadFlavor::Cognitive,
        name: "Meta-Cognition".into(),
        charter: META_COGNITION_CHARTER.into(),
        priority: ThreadPriority::Normal,
        token_budget: 8192,
        schedule: ThreadSchedule::EveryNTicks(10),
        workspace_root: None,
    }
}

/// Severity of a detected cognitive pattern.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PatternSeverity {
    Low,
    Medium,
    High,
}

/// A cognitive pattern detected by the Meta-Cognition thread.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CognitivePattern {
    pub pattern_type: String,
    pub description: String,
    pub severity: PatternSeverity,
    #[serde(default)]
    pub evidence: Vec<String>,
}

/// A draft charter proposal from the Meta-Cognition thread.
///
/// This is the raw output from the thread. It gets enriched with thread_id
/// and tick context to create a full CharterProposal artifact.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CharterProposalDraft {
    pub thread_name: String,
    pub current_charter: String,
    pub proposed_charter: String,
    pub rationale: String,
}

/// A watch suggestion from the Meta-Cognition thread.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WatchSuggestion {
    pub name: String,
    pub description: String,
    pub watch_type: String,
    pub rationale: String,
}

/// Structured output from the Meta-Cognition thread.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetaCognitionAnalysis {
    pub summary: String,
    #[serde(default)]
    pub recommendations: Vec<String>,
    #[serde(default)]
    pub cognitive_patterns: Vec<CognitivePattern>,
    #[serde(default)]
    pub charter_proposals: Vec<CharterProposalDraft>,
    #[serde(default)]
    pub watch_suggestions: Vec<WatchSuggestion>,
}

/// Parse a MetaCognitionAnalysis from an LLM response string.
///
/// Falls back to an empty analysis if the JSON is invalid.
pub fn parse_output(response: &str) -> MetaCognitionAnalysis {
    serde_json::from_str(response).unwrap_or_else(|_| MetaCognitionAnalysis {
        summary: "Unable to parse meta-cognition output".into(),
        recommendations: vec![],
        cognitive_patterns: vec![],
        charter_proposals: vec![],
        watch_suggestions: vec![],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── E5S2-T12: meta_cognition_thread_registered ──

    #[test]
    fn meta_cognition_spec_has_correct_defaults() {
        let s = spec();
        assert_eq!(s.thread_id, META_COGNITION_ID);
        assert_eq!(s.name, "Meta-Cognition");
        assert_eq!(s.priority, ThreadPriority::Normal);
        assert_eq!(s.schedule, ThreadSchedule::EveryNTicks(10));
        assert_eq!(s.token_budget, 8192);
    }

    // ── E5S2-T13: meta_cognition_thread_executes (parse_output) ──

    #[test]
    fn parse_output_valid_json() {
        let json = r#"{
            "summary": "Vessel is performing well",
            "recommendations": ["Continue current approach"],
            "cognitive_patterns": [
                {
                    "pattern_type": "efficient",
                    "description": "Good tool usage",
                    "severity": "low",
                    "evidence": ["Action success rate: 95%"]
                }
            ],
            "charter_proposals": [],
            "watch_suggestions": []
        }"#;
        let analysis = parse_output(json);
        assert_eq!(analysis.summary, "Vessel is performing well");
        assert_eq!(analysis.recommendations.len(), 1);
        assert_eq!(analysis.cognitive_patterns.len(), 1);
        assert_eq!(
            analysis.cognitive_patterns[0].severity,
            PatternSeverity::Low
        );
    }

    // ── E5S2-T14: meta_cognition_detects_pattern ──

    #[test]
    fn parse_output_with_thrashing_pattern() {
        let json = r#"{
            "summary": "Detected thrashing behavior",
            "recommendations": ["Reduce action frequency"],
            "cognitive_patterns": [
                {
                    "pattern_type": "thrashing",
                    "description": "Oscillating between two actions",
                    "severity": "high",
                    "evidence": ["Tick 10: wrote file", "Tick 11: deleted file", "Tick 12: wrote file"]
                }
            ],
            "charter_proposals": [
                {
                    "thread_name": "Self-Critique",
                    "current_charter": "Original charter",
                    "proposed_charter": "Enhanced charter with anti-thrashing",
                    "rationale": "Need better oscillation detection"
                }
            ],
            "watch_suggestions": []
        }"#;
        let analysis = parse_output(json);
        assert_eq!(analysis.cognitive_patterns.len(), 1);
        assert_eq!(
            analysis.cognitive_patterns[0].severity,
            PatternSeverity::High
        );
        assert_eq!(analysis.cognitive_patterns[0].pattern_type, "thrashing");
        assert_eq!(analysis.charter_proposals.len(), 1);
        assert_eq!(analysis.charter_proposals[0].thread_name, "Self-Critique");
    }

    #[test]
    fn parse_output_invalid_json_fallback() {
        let analysis = parse_output("not valid json");
        assert_eq!(analysis.summary, "Unable to parse meta-cognition output");
        assert!(analysis.cognitive_patterns.is_empty());
        assert!(analysis.charter_proposals.is_empty());
    }
}

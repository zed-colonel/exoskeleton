//! Creative Synthesis thread — cross-domain pattern discovery and hypothesis generation.
//!
//! Runs every 15 ticks at Background priority. Looks across accumulated
//! experience to find unexpected connections between domains, generate
//! testable hypotheses, and suggest experiments.

use exoskeleton_core::{ThreadFlavor, ThreadPriority, ThreadRole, ThreadSchedule, ThreadSpec};
use serde::{Deserialize, Serialize};

use super::CREATIVE_SYNTHESIS_ID;

pub const CREATIVE_SYNTHESIS_CHARTER: &str = "\
You are the Creative Synthesis thread, a cognitive thread within an Exoskeleton vessel.

Your purpose: Generate novel connections from accumulated experience.

You see the vessel's long-term memory notes, episodic summaries, current working memory, and \
outputs from other cognitive threads. Your job is NOT to evaluate, criticize, or optimize — \
the other threads handle that. Your job is to create.

Look for:
1. CROSS-DOMAIN CONNECTIONS: Patterns that span different domains of the vessel's experience. \
If trust patterns resemble budget consumption patterns, that's interesting. If tool usage \
frequency correlates with decision quality, note it.
2. TESTABLE HYPOTHESES: Formulate specific, falsifiable statements about the vessel's \
environment or behavior. Include evidence for and against. Flag whether the hypothesis is \
testable with current capabilities.
3. SUGGESTED EXPERIMENTS: Propose concrete actions the vessel could take to test hypotheses \
or explore promising connections. These are recommendations only — the Decide step chooses \
whether to act on them.
4. ANALOGIES AND METAPHORS: If a situation resembles something from a different domain, \
describe the analogy. These can help the Decide step reason about novel situations.

You produce recommendations only — you NEVER invoke tools or take direct action.

Respond with JSON:
{
  \"summary\": \"One-sentence creative synthesis\",
  \"novel_connections\": [
    {
      \"domains\": [\"domain_a\", \"domain_b\"],
      \"observation\": \"What you noticed\",
      \"potential_value\": \"Why this connection might matter\"
    }
  ],
  \"hypotheses\": [
    {
      \"statement\": \"Falsifiable statement\",
      \"evidence_for\": [\"Supporting evidence\"],
      \"evidence_against\": [\"Contradicting evidence\"],
      \"testable\": true
    }
  ],
  \"suggested_experiments\": [\"Concrete experiment descriptions\"]
}";

pub fn spec() -> ThreadSpec {
    ThreadSpec {
        thread_id: CREATIVE_SYNTHESIS_ID,
        role: ThreadRole::CreativeSynthesis,
        flavor: ThreadFlavor::Cognitive,
        name: "Creative-Synthesis".into(),
        charter: CREATIVE_SYNTHESIS_CHARTER.into(),
        priority: ThreadPriority::Background,
        token_budget: 8192,
        schedule: ThreadSchedule::EveryNTicks(15),
        workspace_root: None,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NovelConnection {
    pub domains: Vec<String>,
    pub observation: String,
    pub potential_value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hypothesis {
    pub statement: String,
    #[serde(default)]
    pub evidence_for: Vec<String>,
    #[serde(default)]
    pub evidence_against: Vec<String>,
    #[serde(default)]
    pub testable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreativeSynthesis {
    pub summary: String,
    #[serde(default)]
    pub novel_connections: Vec<NovelConnection>,
    #[serde(default)]
    pub hypotheses: Vec<Hypothesis>,
    #[serde(default)]
    pub suggested_experiments: Vec<String>,
}

pub fn parse_output(response: &str) -> CreativeSynthesis {
    serde_json::from_str(response).unwrap_or_else(|_| CreativeSynthesis {
        summary: "Unable to parse creative synthesis output".into(),
        novel_connections: vec![],
        hypotheses: vec![],
        suggested_experiments: vec![],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // E5S3-T18
    #[test]
    fn creative_synthesis_thread_registered() {
        let s = spec();
        assert_eq!(s.thread_id, CREATIVE_SYNTHESIS_ID);
        assert_eq!(s.name, "Creative-Synthesis");
        assert_eq!(s.priority, ThreadPriority::Background);
        assert_eq!(s.schedule, ThreadSchedule::EveryNTicks(15));
        assert_eq!(s.token_budget, 8192);
    }

    // E5S3-T19
    #[test]
    fn creative_synthesis_parse_output() {
        let json = r#"{
            "summary": "Found interesting pattern",
            "novel_connections": [
                {
                    "domains": ["trust", "budget"],
                    "observation": "Trust levels correlate with budget usage",
                    "potential_value": "Could predict budget issues from trust trends"
                }
            ],
            "hypotheses": [
                {
                    "statement": "Higher trust leads to more efficient budget usage",
                    "evidence_for": ["Budget efficiency increased with trust level"],
                    "evidence_against": [],
                    "testable": true
                }
            ],
            "suggested_experiments": ["Track budget/trust correlation over 50 ticks"]
        }"#;
        let result = parse_output(json);
        assert_eq!(result.novel_connections.len(), 1);
        assert_eq!(result.novel_connections[0].domains, vec!["trust", "budget"]);
        assert_eq!(result.hypotheses.len(), 1);
        assert!(result.hypotheses[0].testable);
        assert_eq!(result.suggested_experiments.len(), 1);
    }

    // E5S3-T20
    #[test]
    fn creative_synthesis_generates_connections() {
        let json = r#"{
            "summary": "Cross-domain patterns detected",
            "novel_connections": [
                {
                    "domains": ["memory", "performance"],
                    "observation": "Memory consolidation frequency correlates with decision speed",
                    "potential_value": "Tuning consolidation schedule could improve latency"
                },
                {
                    "domains": ["security", "efficiency"],
                    "observation": "Threat monitoring overhead scales with tool count",
                    "potential_value": "Selective monitoring could reduce costs"
                }
            ],
            "hypotheses": [],
            "suggested_experiments": []
        }"#;
        let result = parse_output(json);
        assert_eq!(result.novel_connections.len(), 2);
        assert_eq!(result.novel_connections[0].domains.len(), 2);
        assert!(!result.novel_connections[0].observation.is_empty());
        assert!(!result.novel_connections[0].potential_value.is_empty());
    }

    #[test]
    fn creative_synthesis_parse_invalid_json() {
        let result = parse_output("not json");
        assert_eq!(result.summary, "Unable to parse creative synthesis output");
        assert!(result.novel_connections.is_empty());
    }
}

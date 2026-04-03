//! Tick model — durable record of one cognitive cycle (PODAARA).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::id::{ArtifactId, ThreadId, TickId};
use crate::ExoError;

/// Durable record of one cognitive cycle (one PODAARA tick on the Cognitive AQ).
///
/// Every tick produces a TickRecord that captures: what phase it was in,
/// what the LLM decided, what actions were taken (via Tool AQ), what
/// threads contributed, and timing. The TickRecord is stored as an artifact
/// (I3: everything replayable).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
pub struct TickRecord {
    /// Unique identity of this tick (corresponds to a Cognitive AQ RunId as UUID).
    pub tick_id: TickId,
    /// Monotonically increasing tick number (matches StateSnapshot.tick_number).
    pub tick_number: u64,
    /// Which PODAARA phase this tick record describes.
    pub phase: TickPhase,
    /// When the tick started.
    pub started_at: DateTime<Utc>,
    /// When the tick completed (`None` if still running or crashed).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<DateTime<Utc>>,
    /// Reference to the State Snapshot as it existed at tick start.
    pub snapshot_before: ArtifactId,
    /// Reference to the State Snapshot produced at tick end.
    /// `None` if the tick hasn't completed yet or failed before Amend.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot_after: Option<ArtifactId>,
    /// Artifacts contributed by cognitive threads during this tick.
    pub thread_contributions: Vec<ThreadContribution>,
    /// Actions executed in the Act step (via WI Host / Tool AQ).
    pub actions_taken: Vec<ActionRecord>,
    /// LLM invocations made during this tick (via Cognitive AQ).
    pub llm_calls: Vec<LlmCallRecord>,
    /// Reasoning summary from the Decide step.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision_rationale: Option<String>,
    /// Reference to the ContextBreakdown artifact (E3-S2).
    /// Contains per-section token allocation from the Orient step.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_breakdown_ref: Option<ArtifactId>,
}

/// PODAARA phase of the cognitive cycle.
///
/// The 7 phases run in strict order: Perceive -> Orient -> Decide -> Align ->
/// Act -> Reflect -> Amend. This sequence is a sacred invariant (Charter §5.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum TickPhase {
    /// Gather context and inputs.
    Perceive,
    /// Analyze situation and form understanding.
    Orient,
    /// Make decisions about what to do.
    Decide,
    /// Update relationship state and check alignment.
    Align,
    /// Execute actions via WI Host (Tool AQ).
    Act,
    /// Evaluate outcomes and learn.
    Reflect,
    /// Update snapshot and finalize tick.
    Amend,
}

impl TickPhase {
    /// The canonical PODAARA sequence.
    pub fn sequence() -> Vec<TickPhase> {
        vec![
            Self::Perceive,
            Self::Orient,
            Self::Decide,
            Self::Align,
            Self::Act,
            Self::Reflect,
            Self::Amend,
        ]
    }

    /// The next phase in the sequence, or `None` if this is Amend (last).
    pub fn next(&self) -> Option<TickPhase> {
        match self {
            Self::Perceive => Some(Self::Orient),
            Self::Orient => Some(Self::Decide),
            Self::Decide => Some(Self::Align),
            Self::Align => Some(Self::Act),
            Self::Act => Some(Self::Reflect),
            Self::Reflect => Some(Self::Amend),
            Self::Amend => None,
        }
    }
}

/// A cognitive thread's contribution to a tick.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
pub struct ThreadContribution {
    /// Which thread contributed.
    pub thread_id: ThreadId,
    /// Reference to the thread's output artifact.
    pub artifact_id: ArtifactId,
    /// One-line summary of what this thread contributed.
    pub summary: String,
}

/// Record of one action executed in the Act step (crossing to Tool AQ via WI Host).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
pub struct ActionRecord {
    /// What kind of action (connector name, e.g., "fs.write", "http.request").
    pub action_type: String,
    /// Target of the action (URL, path, etc.) — human-readable summary.
    pub target: String,
    /// Reference to the receipt artifact produced by the Tool AQ / WI Host.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipt_ref: Option<ArtifactId>,
    /// Outcome of the action.
    pub outcome: ActionOutcome,
}

/// Outcome of a tool invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum ActionOutcome {
    /// Action completed successfully.
    Success,
    /// Action failed.
    Failure,
    /// Action timed out.
    Timeout,
    /// Action was skipped (e.g., budget exhausted, capability revoked).
    Skipped,
    /// Action was rate-limited by the tool budget gate.
    RateLimited,
    /// Action was blocked by the tool policy gate.
    PolicyDenied,
}

/// Record of one LLM invocation (dispatched via the Cognitive AQ).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
pub struct LlmCallRecord {
    /// Which model was used.
    pub model: String,
    /// Input tokens consumed.
    pub tokens_in: u64,
    /// Output tokens produced.
    pub tokens_out: u64,
    /// Estimated cost in hundredths of a cent.
    pub cost_cents: f64,
    /// Wall-clock latency in milliseconds.
    pub latency_ms: u64,
    /// Reference to the stored LLM response artifact (I3: replayable).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_artifact_ref: Option<ArtifactId>,
    /// Number of LLM turns in this record (E5-S1: multi-turn Decide).
    /// Default: 1 for backward compatibility with single-turn Decide.
    #[serde(default = "default_turns")]
    pub turns: u32,
}

fn default_turns() -> u32 {
    1
}

impl LlmCallRecord {
    /// Total tokens (in + out).
    pub fn total_tokens(&self) -> u64 {
        self.tokens_in + self.tokens_out
    }

    /// Merge multiple call records into a single summary.
    /// Uses the first record's metadata (model) and sums tokens/costs.
    pub fn merge(records: &[LlmCallRecord]) -> LlmCallRecord {
        if records.is_empty() {
            return LlmCallRecord {
                model: String::new(),
                tokens_in: 0,
                tokens_out: 0,
                cost_cents: 0.0,
                latency_ms: 0,
                response_artifact_ref: None,
                turns: 0,
            };
        }
        let first = &records[0];
        LlmCallRecord {
            model: first.model.clone(),
            tokens_in: records.iter().map(|r| r.tokens_in).sum(),
            tokens_out: records.iter().map(|r| r.tokens_out).sum(),
            cost_cents: records.iter().map(|r| r.cost_cents).sum(),
            latency_ms: records.iter().map(|r| r.latency_ms).sum(),
            response_artifact_ref: records.last().and_then(|r| r.response_artifact_ref.clone()),
            turns: records.len() as u32,
        }
    }
}

/// Durable store for completed tick records (PODAARA cycles).
///
/// Each completed tick produces a `TickRecord` that captures the full cycle:
/// which phase it was in, what the LLM decided, what actions were taken, what
/// threads contributed. The TickStore provides indexed access by tick_id and
/// tick_number.
///
/// TickRecords are also stored as Artifacts (via the caller — Sprint 5). The
/// TickStore provides structured access; the ArtifactStore provides content-
/// addressed access.
pub trait TickStore: Send + Sync {
    /// Save a completed tick record.
    ///
    /// Keyed by `tick_id` (UUID) with a unique constraint on `tick_number`.
    /// Returns `ExoError::Storage` if a record with this tick_id or tick_number
    /// already exists.
    fn save(&self, record: &TickRecord) -> Result<(), ExoError>;

    /// Get the most recently saved tick record.
    fn latest(&self) -> Result<Option<TickRecord>, ExoError>;

    /// Get a tick record by its TickId.
    fn get(&self, tick_id: TickId) -> Result<Option<TickRecord>, ExoError>;

    /// Get tick records in a range of tick numbers (inclusive), oldest first.
    ///
    /// Used for replay and debugging.
    fn range(&self, from_tick: u64, to_tick: u64) -> Result<Vec<TickRecord>, ExoError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── T-4: Tick Model ──

    #[test]
    fn tick_phase_sequence() {
        let seq = TickPhase::sequence();
        assert_eq!(seq.len(), 7);
        assert_eq!(
            seq,
            vec![
                TickPhase::Perceive,
                TickPhase::Orient,
                TickPhase::Decide,
                TickPhase::Align,
                TickPhase::Act,
                TickPhase::Reflect,
                TickPhase::Amend,
            ]
        );
    }

    #[test]
    fn tick_phase_next() {
        assert_eq!(TickPhase::Perceive.next(), Some(TickPhase::Orient));
        assert_eq!(TickPhase::Orient.next(), Some(TickPhase::Decide));
        assert_eq!(TickPhase::Decide.next(), Some(TickPhase::Align));
        assert_eq!(TickPhase::Align.next(), Some(TickPhase::Act));
        assert_eq!(TickPhase::Act.next(), Some(TickPhase::Reflect));
        assert_eq!(TickPhase::Reflect.next(), Some(TickPhase::Amend));
        assert_eq!(TickPhase::Amend.next(), None);
    }

    #[test]
    fn tick_phase_all_variants_roundtrip() {
        for phase in TickPhase::sequence() {
            let json = serde_json::to_string(&phase).unwrap();
            let parsed: TickPhase = serde_json::from_str(&json).unwrap();
            assert_eq!(phase, parsed);
        }
    }

    #[test]
    fn tick_record_json_roundtrip() {
        let record = TickRecord {
            tick_id: TickId::new(),
            tick_number: 7,
            phase: TickPhase::Amend,
            started_at: Utc::now(),
            completed_at: Some(Utc::now()),
            snapshot_before: ArtifactId::from_content(b"before"),
            snapshot_after: Some(ArtifactId::from_content(b"after")),
            thread_contributions: vec![ThreadContribution {
                thread_id: ThreadId::new(),
                artifact_id: ArtifactId::from_content(b"thread out"),
                summary: "Threat analysis complete".into(),
            }],
            actions_taken: vec![ActionRecord {
                action_type: "fs.write".into(),
                target: "/tmp/output.txt".into(),
                receipt_ref: Some(ArtifactId::from_content(b"receipt")),
                outcome: ActionOutcome::Success,
            }],
            llm_calls: vec![LlmCallRecord {
                model: "local-7b".into(),
                tokens_in: 1000,
                tokens_out: 500,
                cost_cents: 0.0,
                latency_ms: 250,
                response_artifact_ref: Some(ArtifactId::from_content(b"llm response")),
                turns: 1,
            }],
            decision_rationale: Some("Decided to write output file".into()),
            context_breakdown_ref: None,
        };
        let json = serde_json::to_string(&record).unwrap();
        let parsed: TickRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(record, parsed);
    }

    #[test]
    fn action_outcome_all_variants_roundtrip() {
        let variants = [
            ActionOutcome::Success,
            ActionOutcome::Failure,
            ActionOutcome::Timeout,
            ActionOutcome::Skipped,
            ActionOutcome::RateLimited,
            ActionOutcome::PolicyDenied,
        ];
        for outcome in &variants {
            let json = serde_json::to_string(outcome).unwrap();
            let parsed: ActionOutcome = serde_json::from_str(&json).unwrap();
            assert_eq!(*outcome, parsed);
        }
    }

    #[test]
    fn action_outcome_policy_denied_serde() {
        let json = serde_json::to_string(&ActionOutcome::PolicyDenied).unwrap();
        assert_eq!(json, "\"policy_denied\"");
        let parsed: ActionOutcome = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, ActionOutcome::PolicyDenied);
    }

    #[test]
    fn llm_call_record_total_tokens() {
        let record = LlmCallRecord {
            model: "test".into(),
            tokens_in: 100,
            tokens_out: 50,
            cost_cents: 0.5,
            latency_ms: 100,
            response_artifact_ref: None,
            turns: 1,
        };
        assert_eq!(record.total_tokens(), 150);
    }

    #[test]
    fn llm_call_record_turns_default() {
        let json =
            r#"{"model":"m","tokens_in":10,"tokens_out":5,"cost_cents":0.1,"latency_ms":50}"#;
        let parsed: LlmCallRecord = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.turns, 1); // backward compat default
    }

    #[test]
    fn llm_call_record_merge() {
        let r1 = LlmCallRecord {
            model: "model-a".into(),
            tokens_in: 100,
            tokens_out: 50,
            cost_cents: 0.5,
            latency_ms: 200,
            response_artifact_ref: None,
            turns: 1,
        };
        let r2 = LlmCallRecord {
            model: "model-a".into(),
            tokens_in: 80,
            tokens_out: 40,
            cost_cents: 0.3,
            latency_ms: 150,
            response_artifact_ref: Some(ArtifactId::from_content(b"resp2")),
            turns: 1,
        };
        let merged = LlmCallRecord::merge(&[r1, r2]);
        assert_eq!(merged.model, "model-a");
        assert_eq!(merged.tokens_in, 180);
        assert_eq!(merged.tokens_out, 90);
        assert!((merged.cost_cents - 0.8).abs() < f64::EPSILON);
        assert_eq!(merged.latency_ms, 350);
        assert_eq!(merged.turns, 2);
        assert!(merged.response_artifact_ref.is_some());
    }

    #[test]
    fn llm_call_record_merge_empty() {
        let merged = LlmCallRecord::merge(&[]);
        assert_eq!(merged.turns, 0);
        assert_eq!(merged.tokens_in, 0);
    }

    #[test]
    fn thread_contribution_roundtrip() {
        let tc = ThreadContribution {
            thread_id: ThreadId::new(),
            artifact_id: ArtifactId::from_content(b"contribution"),
            summary: "Detected alignment drift".into(),
        };
        let json = serde_json::to_string(&tc).unwrap();
        let parsed: ThreadContribution = serde_json::from_str(&json).unwrap();
        assert_eq!(tc, parsed);
    }
}

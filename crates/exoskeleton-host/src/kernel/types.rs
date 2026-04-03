//! PODAARA result types — internal to the kernel module.
//!
//! Each PODAARA step produces a typed result that flows to the next step.
//! These types are internal to the kernel — they are not part of the public API.

use exoskeleton_core::conversation::Conversation;
use exoskeleton_core::plan::{Plan, PlanTaskStatus, PlanUpdate};
use exoskeleton_core::tick::{ActionRecord, LlmCallRecord, ThreadContribution};
use exoskeleton_core::working_memory::WorkingMemoryOp;
use exoskeleton_core::{
    ArtifactId, EventEntry, MessageEnvelope, PlanTaskId, RelationshipRecord, RelationshipSnapshot,
    ThreadId, TickId, VesselId, VesselMode,
};
use exoskeleton_memory::CompiledContext;
use serde::{Deserialize, Serialize};

/// Payload data for master loop tasks on the Cognitive AQ.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MasterLoopPayload {
    pub vessel_id: VesselId,
}

/// Payload for thread execution tasks on the Cognitive AQ.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreadPayload {
    pub thread_id: ThreadId,
    pub tick_id: TickId,
}

/// Output of the Perceive step.
#[derive(Debug, Clone)]
pub struct PerceptionResult {
    pub new_messages: Vec<MessageEnvelope>,
    /// Active conversations after envelope grouping (E1-S2).
    pub active_conversations: Vec<Conversation>,
    pub thread_outputs: Vec<ThreadContribution>,
    pub pending_action_results: Vec<EventEntry>,
}

/// Output of the Orient step.
#[derive(Debug, Clone)]
pub struct OrientationResult {
    pub compiled_context: CompiledContext,
}

/// A single turn in the multi-turn Decide loop (E5-S1).
///
/// The LLM can either issue introspection queries (resolved inline) or
/// produce a final decision (existing DecisionProtocol). If the response
/// has no `type` field, it is treated as a decision for backward compat.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DecideTurn {
    /// The LLM wants to query the vessel's stores before deciding.
    Query {
        queries: Vec<exoskeleton_core::introspection::IntrospectionQuery>,
    },
    /// The LLM has produced a final decision.
    Decide(DecisionProtocol),
}

/// The decision protocol: structured JSON format for LLM responses.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecisionProtocol {
    pub reasoning: String,
    /// Whether the agent requests inner-loop execution for this tick (E8-S1).
    /// Backward-compatible: old JSON without this field → false.
    #[serde(default, skip_serializing_if = "is_false")]
    pub inner_loop_requested: bool,
    /// Explicit reply to the user. When the vessel wants to respond to a
    /// message, it populates this field. None for idle ticks or internal-only
    /// decisions. Backward-compatible: old JSON without this field → None.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[serde(deserialize_with = "deserialize_plan_update_compat")]
    pub plan_update: Option<PlanUpdate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[serde(alias = "working_context_update")]
    #[serde(deserialize_with = "deserialize_working_memory_ops_compat")]
    pub working_memory_ops: Option<Vec<WorkingMemoryOp>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vessel_mode_request: Option<VesselMode>,
    #[serde(default)]
    pub actions: Vec<PlannedAction>,
    #[serde(default)]
    pub memory_notes: Vec<String>,
    /// Watch proposals — persistent observations the vessel wants to set up.
    /// Proposed by Decide, approved/blocked by Align (I4).
    #[serde(default)]
    pub watch_proposals: Vec<exoskeleton_core::watch::WatchProposal>,
}

/// A single action the LLM wants to execute.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlannedAction {
    pub tool_name: String,
    pub params: serde_json::Value,
    pub rationale: String,
    /// Optional reference to the plan task this action implements.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_task_id: Option<PlanTaskId>,
}

/// Output of the Decide step.
#[derive(Debug, Clone)]
pub struct DecisionResult {
    pub reasoning: String,
    pub reply: Option<String>,
    pub actions: Vec<PlannedAction>,
    pub snapshot_delta: SnapshotDelta,
    pub memory_notes: Vec<String>,
    pub llm_call_record: LlmCallRecord,
    pub response_artifact_id: ArtifactId,
    /// Watch proposals approved by Decide, awaiting Align approval.
    pub watch_proposals: Vec<exoskeleton_core::watch::WatchProposal>,
    /// Whether the agent requests inner-loop execution for this tick (E8-S1).
    /// When true and inner_loop.enabled in config, the tick enters the
    /// bounded inner loop instead of the single Decide→Act pass.
    pub inner_loop_requested: bool,
    /// Optional vessel mode transition request from Decide.
    pub vessel_mode_request: Option<VesselMode>,
}

/// Proposed changes to the StateSnapshot from the Decide step.
#[derive(Debug, Clone, Default)]
pub struct SnapshotDelta {
    pub plan_update: Option<PlanUpdate>,
    pub working_memory_ops: Option<Vec<WorkingMemoryOp>>,
}

/// Output of the Align step.
#[derive(Debug, Clone)]
pub struct AlignmentResult {
    pub approved_actions: Vec<PlannedAction>,
    pub blocked_actions: Vec<(PlannedAction, String)>,
    /// New relationship records to append to the ledger (AlignmentCheck entries).
    pub relationship_updates: Vec<RelationshipRecord>,
    /// Fresh relationship snapshot compiled during the Align step (Sprint 8).
    pub relationship_snapshot: Option<RelationshipSnapshot>,
}

/// Outcome of a single action execution in the Act step.
#[derive(Debug, Clone)]
pub struct ActionExecution {
    pub action: PlannedAction,
    pub result: Result<serde_json::Value, String>,
    pub record: ActionRecord,
    pub pending_question: bool,
}

/// Output of the Act step.
#[derive(Debug, Clone)]
pub struct ActResult {
    pub executions: Vec<ActionExecution>,
}

/// Output of the Reflect step.
#[derive(Debug, Clone)]
pub struct ReflectionResult {
    pub action_success_rate: f64,
    pub observations: Vec<String>,
    pub concerns: Vec<String>,
    /// Task status updates from Reflect LLM call.
    pub task_updates: Vec<TaskStatusUpdate>,
    /// Working memory operations from Reflect LLM call.
    pub working_memory_ops: Vec<WorkingMemoryOp>,
    /// Whether the Reflect step recommends replanning next tick.
    pub should_replan: bool,
    /// LLM call record (None if heuristic-only fallback).
    pub llm_call_record: Option<LlmCallRecord>,
}

/// The reflect protocol: structured JSON format for LLM Reflect responses.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReflectProtocol {
    pub outcome_assessment: String,
    #[serde(default)]
    pub task_updates: Vec<TaskStatusUpdate>,
    #[serde(default)]
    pub working_memory_ops: Vec<WorkingMemoryOp>,
    #[serde(default)]
    pub observations: Vec<String>,
    #[serde(default)]
    pub concerns: Vec<String>,
    #[serde(default)]
    pub should_replan: bool,
}

/// A task status update from the Reflect step.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskStatusUpdate {
    pub task_id: PlanTaskId,
    pub new_status: PlanTaskStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Custom deserializer: handles legacy string and new PlanUpdate.
fn deserialize_plan_update_compat<'de, D>(deserializer: D) -> Result<Option<PlanUpdate>, D::Error>
where
    D: serde::de::Deserializer<'de>,
{
    let value: Option<serde_json::Value> = Option::deserialize(deserializer)?;
    match value {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::String(s)) => {
            if s.is_empty() {
                Ok(None)
            } else {
                Ok(Some(PlanUpdate::Replace {
                    plan: Plan::from_legacy_string(s),
                }))
            }
        }
        Some(v @ serde_json::Value::Object(_)) => {
            let update: PlanUpdate = serde_json::from_value(v).map_err(serde::de::Error::custom)?;
            Ok(Some(update))
        }
        Some(other) => Err(serde::de::Error::custom(format!(
            "expected string, object, or null for plan_update, got: {other}"
        ))),
    }
}

/// Custom deserializer: handles legacy string and new Vec<WorkingMemoryOp>.
fn deserialize_working_memory_ops_compat<'de, D>(
    deserializer: D,
) -> Result<Option<Vec<WorkingMemoryOp>>, D::Error>
where
    D: serde::de::Deserializer<'de>,
{
    let value: Option<serde_json::Value> = Option::deserialize(deserializer)?;
    match value {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::String(s)) => {
            if s.is_empty() {
                Ok(None)
            } else {
                Ok(Some(vec![WorkingMemoryOp::Set {
                    key: "context".into(),
                    value: s,
                    ttl_ticks: None,
                }]))
            }
        }
        Some(serde_json::Value::Array(_)) => {
            let ops: Vec<WorkingMemoryOp> =
                serde_json::from_value(value.unwrap()).map_err(serde::de::Error::custom)?;
            Ok(Some(ops))
        }
        Some(other) => Err(serde::de::Error::custom(format!(
            "expected string, array, or null for working_memory_ops, got: {other}"
        ))),
    }
}

/// Helper for `skip_serializing_if` on boolean fields.
fn is_false(val: &bool) -> bool {
    !val
}

/// Extract JSON content from a markdown code fence.
///
/// Looks for ```json ... ``` and returns the content between the fences.
/// Returns `None` if no JSON code fence is found.
pub fn extract_json_from_code_fence(text: &str) -> Option<&str> {
    let start_marker = "```json";
    let end_marker = "```";

    let start_idx = text.find(start_marker)?;
    let content_start = start_idx + start_marker.len();
    let rest = &text[content_start..];
    let end_idx = rest.find(end_marker)?;
    let content = &rest[..end_idx];
    Some(content.trim())
}

#[cfg(test)]
mod tests {
    use exoskeleton_core::artifact::ArtifactKind;

    use super::*;

    #[test]
    fn master_loop_payload_roundtrip() {
        let payload = MasterLoopPayload {
            vessel_id: exoskeleton_core::VesselId::new(),
        };
        let json = serde_json::to_string(&payload).unwrap();
        let parsed: MasterLoopPayload = serde_json::from_str(&json).unwrap();
        assert_eq!(payload.vessel_id.to_string(), parsed.vessel_id.to_string());
    }

    #[test]
    fn decision_protocol_roundtrip_minimal() {
        let proto = DecisionProtocol {
            reasoning: "Nothing to do".into(),
            inner_loop_requested: false,
            reply: None,
            plan_update: None,
            working_memory_ops: None,
            vessel_mode_request: None,
            actions: vec![],
            memory_notes: vec![],
            watch_proposals: vec![],
        };
        let json = serde_json::to_string(&proto).unwrap();
        let parsed: DecisionProtocol = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.reasoning, "Nothing to do");
        assert!(parsed.plan_update.is_none());
        assert!(parsed.working_memory_ops.is_none());
        assert!(parsed.actions.is_empty());
        assert!(parsed.memory_notes.is_empty());
    }

    #[test]
    fn decision_protocol_optional_fields_omitted() {
        let proto = DecisionProtocol {
            reasoning: "Test".into(),
            inner_loop_requested: false,
            reply: None,
            plan_update: None,
            working_memory_ops: None,
            vessel_mode_request: None,
            actions: vec![],
            memory_notes: vec![],
            watch_proposals: vec![],
        };
        let value: serde_json::Value = serde_json::to_value(&proto).unwrap();
        let obj = value.as_object().unwrap();
        assert!(!obj.contains_key("plan_update"));
        assert!(!obj.contains_key("working_memory_ops"));
        assert!(!obj.contains_key("inner_loop_requested"));
    }

    // ── E1-T17: PlannedAction with plan_task_id roundtrip ──
    #[test]
    fn planned_action_with_task_id_roundtrip() {
        let task_id = exoskeleton_core::PlanTaskId::new();
        let action = PlannedAction {
            tool_name: "fs.write".into(),
            params: serde_json::json!({"path": "/tmp/out.txt"}),
            rationale: "Write output".into(),
            plan_task_id: Some(task_id),
        };
        let json = serde_json::to_string(&action).unwrap();
        let parsed: PlannedAction = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.plan_task_id, Some(task_id));
    }

    // ── E1-T18: PlannedAction without plan_task_id backward compat ──
    #[test]
    fn planned_action_without_task_id_compat() {
        let json = r#"{"tool_name":"fs.write","params":{},"rationale":"test"}"#;
        let parsed: PlannedAction = serde_json::from_str(json).unwrap();
        assert!(parsed.plan_task_id.is_none());
        assert_eq!(parsed.tool_name, "fs.write");
    }

    #[test]
    fn planned_action_roundtrip() {
        let action = PlannedAction {
            tool_name: "http.request".into(),
            params: serde_json::json!({
                "url": "https://example.com/api",
                "method": "POST",
                "body": {"key": "value", "nested": [1, 2, 3]}
            }),
            rationale: "Fetch data from API".into(),
            plan_task_id: None,
        };
        let json = serde_json::to_string(&action).unwrap();
        let parsed: PlannedAction = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.tool_name, "http.request");
        assert_eq!(parsed.params["url"], "https://example.com/api");
        assert_eq!(parsed.params["body"]["nested"][1], 2);
        assert_eq!(parsed.rationale, "Fetch data from API");
    }

    #[test]
    fn snapshot_delta_default_is_no_change() {
        let delta = SnapshotDelta::default();
        assert!(delta.plan_update.is_none());
        assert!(delta.working_memory_ops.is_none());
    }

    #[test]
    fn decision_protocol_empty_actions() {
        let json = r#"{"reasoning": "idle", "actions": [], "memory_notes": []}"#;
        let parsed: DecisionProtocol = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.reasoning, "idle");
        assert!(parsed.actions.is_empty());
        assert!(parsed.memory_notes.is_empty());
    }

    // ── E1-T14: Deserialize legacy "plan_update": "text" in DecisionProtocol ──
    #[test]
    fn decision_protocol_legacy_plan_update() {
        let json = r#"{"reasoning": "test", "plan_update": "Execute plan A", "actions": []}"#;
        let parsed: DecisionProtocol = serde_json::from_str(json).unwrap();
        match parsed.plan_update.unwrap() {
            exoskeleton_core::PlanUpdate::Replace { plan } => {
                assert_eq!(plan.objective, "Execute plan A");
            }
            other => panic!("expected Replace, got: {other:?}"),
        }
    }

    // ── E1-T15: Deserialize new "plan_update": {"type":"replace",...} ──
    #[test]
    fn decision_protocol_new_plan_update() {
        let json = r#"{
            "reasoning": "test",
            "plan_update": {"type": "replace", "plan": {"objective": "X", "tasks": [], "updated_at": "2026-01-01T00:00:00Z"}},
            "actions": []
        }"#;
        let parsed: DecisionProtocol = serde_json::from_str(json).unwrap();
        match parsed.plan_update.unwrap() {
            exoskeleton_core::PlanUpdate::Replace { plan } => {
                assert_eq!(plan.objective, "X");
            }
            other => panic!("expected Replace, got: {other:?}"),
        }
    }

    // ── E1-T16: Deserialize absent plan_update as None ──
    #[test]
    fn decision_protocol_absent_plan_update() {
        let json = r#"{"reasoning": "test", "actions": []}"#;
        let parsed: DecisionProtocol = serde_json::from_str(json).unwrap();
        assert!(parsed.plan_update.is_none());
    }

    // ── E1-T35: Deserialize legacy "working_context_update": "text" as ops ──
    #[test]
    fn decision_protocol_legacy_working_context_update() {
        let json = r#"{"reasoning": "test", "working_context_update": "New focus", "actions": []}"#;
        let parsed: DecisionProtocol = serde_json::from_str(json).unwrap();
        let ops = parsed.working_memory_ops.unwrap();
        assert_eq!(ops.len(), 1);
        match &ops[0] {
            exoskeleton_core::WorkingMemoryOp::Set { key, value, .. } => {
                assert_eq!(key, "context");
                assert_eq!(value, "New focus");
            }
            other => panic!("expected Set, got: {other:?}"),
        }
    }

    // ── E1-T36: Deserialize new "working_memory_ops": [...] as ops ──
    #[test]
    fn decision_protocol_new_working_memory_ops() {
        let json = r#"{
            "reasoning": "test",
            "working_memory_ops": [{"op": "set", "key": "k", "value": "v"}, {"op": "remove", "key": "old"}],
            "actions": []
        }"#;
        let parsed: DecisionProtocol = serde_json::from_str(json).unwrap();
        let ops = parsed.working_memory_ops.unwrap();
        assert_eq!(ops.len(), 2);
    }

    // ── E1-T37: Deserialize absent working_memory_ops as None ──
    #[test]
    fn decision_protocol_absent_working_memory_ops() {
        let json = r#"{"reasoning": "test", "actions": []}"#;
        let parsed: DecisionProtocol = serde_json::from_str(json).unwrap();
        assert!(parsed.working_memory_ops.is_none());
    }

    // ── E1-T50: ReflectProtocol JSON roundtrip ──
    #[test]
    fn reflect_protocol_roundtrip() {
        let proto = ReflectProtocol {
            outcome_assessment: "Action succeeded".into(),
            task_updates: vec![TaskStatusUpdate {
                task_id: exoskeleton_core::PlanTaskId::new(),
                new_status: exoskeleton_core::PlanTaskStatus::Completed,
                reason: Some("Action succeeded".into()),
            }],
            working_memory_ops: vec![exoskeleton_core::WorkingMemoryOp::Set {
                key: "obs".into(),
                value: "learned something".into(),
                ttl_ticks: Some(10),
            }],
            observations: vec!["All good".into()],
            concerns: vec![],
            should_replan: false,
        };
        let json = serde_json::to_string(&proto).unwrap();
        let parsed: ReflectProtocol = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.outcome_assessment, proto.outcome_assessment);
        assert_eq!(parsed.task_updates.len(), 1);
        assert_eq!(parsed.working_memory_ops.len(), 1);
    }

    // ── E1-T51: TaskStatusUpdate JSON roundtrip ──
    #[test]
    fn task_status_update_roundtrip() {
        let update = TaskStatusUpdate {
            task_id: exoskeleton_core::PlanTaskId::new(),
            new_status: exoskeleton_core::PlanTaskStatus::Failed,
            reason: Some("Timeout".into()),
        };
        let json = serde_json::to_string(&update).unwrap();
        let parsed: TaskStatusUpdate = serde_json::from_str(&json).unwrap();
        assert_eq!(update, parsed);
    }

    #[test]
    fn decision_protocol_from_markdown_code_fence() {
        let text = r#"Here is my decision:

```json
{
    "reasoning": "I should write a file",
    "actions": [{"tool_name": "fs.write", "params": {"path": "/tmp/x"}, "rationale": "write"}],
    "memory_notes": []
}
```

That's my plan."#;

        let json_str = extract_json_from_code_fence(text).unwrap();
        let parsed: DecisionProtocol = serde_json::from_str(json_str).unwrap();
        assert_eq!(parsed.reasoning, "I should write a file");
        assert_eq!(parsed.actions.len(), 1);
        assert_eq!(parsed.actions[0].tool_name, "fs.write");
        assert!(parsed.actions[0].plan_task_id.is_none());

        // No code fence
        assert!(extract_json_from_code_fence("no code fence here").is_none());

        // Empty code fence
        let empty = "```json\n```";
        let content = extract_json_from_code_fence(empty);
        assert_eq!(content, Some(""));
    }

    // ── OA-T5: DecisionProtocol with reply roundtrip ──
    #[test]
    fn decision_protocol_with_reply_roundtrip() {
        let json = r#"{
            "reasoning": "User asked a question",
            "reply": "Here is my answer",
            "actions": [],
            "memory_notes": []
        }"#;
        let parsed: DecisionProtocol = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.reply, Some("Here is my answer".into()));

        // Re-serialize and verify
        let reserialized = serde_json::to_string(&parsed).unwrap();
        let reparsed: DecisionProtocol = serde_json::from_str(&reserialized).unwrap();
        assert_eq!(reparsed.reply, Some("Here is my answer".into()));
    }

    // ── OA-T6: DecisionProtocol without reply backward compat ──
    #[test]
    fn decision_protocol_without_reply_backward_compat() {
        let json = r#"{"reasoning": "idle tick", "actions": []}"#;
        let parsed: DecisionProtocol = serde_json::from_str(json).unwrap();
        assert!(parsed.reply.is_none());
    }

    // ── OA-T7: DecisionProtocol with null reply ──
    #[test]
    fn decision_protocol_null_reply() {
        let json = r#"{"reasoning": "test", "reply": null, "actions": []}"#;
        let parsed: DecisionProtocol = serde_json::from_str(json).unwrap();
        assert!(parsed.reply.is_none());
    }

    #[test]
    fn artifact_kind_tick_roundtrip() {
        let kind = ArtifactKind::Tick;
        let json = serde_json::to_string(&kind).unwrap();
        assert_eq!(json, "\"tick\"");
        let parsed: ArtifactKind = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, ArtifactKind::Tick);
    }
}

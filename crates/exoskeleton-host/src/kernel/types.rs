//! PODAARA result types — internal to the kernel module.
//!
//! Each PODAARA step produces a typed result that flows to the next step.
//! These types are internal to the kernel — they are not part of the public API.

use exoskeleton_core::conversation::Conversation;
use exoskeleton_core::llm::ContentBlock;
use exoskeleton_core::plan::{PlanTaskStatus, PlanUpdate};
use exoskeleton_core::tick::{
    ActionRecord, ExecThreadContribution, LlmCallRecord, ThreadContribution,
};
use exoskeleton_core::working_memory::WorkingMemoryOp;
use exoskeleton_core::{
    ArtifactId, EventEntry, MessageEnvelope, PlanTaskId, RelationshipRecord, RelationshipSnapshot,
    ThreadId, VesselId, VesselMode,
};
use exoskeleton_memory::CompiledContext;
use serde::{Deserialize, Serialize};

/// Payload data for master loop tasks on the Cognitive AQ.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MasterLoopPayload {
    pub vessel_id: VesselId,
}

/// Output of the Perceive step.
#[derive(Debug, Clone)]
pub struct PerceptionResult {
    pub new_messages: Vec<MessageEnvelope>,
    /// Active conversations after envelope grouping (E1-S2).
    pub active_conversations: Vec<Conversation>,
    pub thread_outputs: Vec<ThreadContribution>,
    pub exec_thread_outputs: Vec<ExecThreadContribution>,
    pub pending_action_results: Vec<EventEntry>,
}

/// Output of the Orient step.
#[derive(Debug, Clone)]
pub struct OrientationResult {
    pub compiled_context: CompiledContext,
}

/// A single action the LLM wants to execute.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlannedAction {
    /// API-issued tool call ID, threaded back through the tool result.
    pub call_id: String,
    pub tool_name: String,
    pub params: serde_json::Value,
    pub rationale: String,
    /// Optional reference to the plan task this action implements.
    /// Uses lenient deserialization: non-UUID strings are accepted and mapped
    /// to `None` rather than causing the entire action to fail to parse.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[serde(deserialize_with = "deserialize_plan_task_id_lenient")]
    pub plan_task_id: Option<PlanTaskId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_exec_thread_id: Option<ThreadId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposal_id: Option<String>,
}

/// Lenient deserializer for plan_task_id: accepts UUIDs, maps non-UUID strings to None.
fn deserialize_plan_task_id_lenient<'de, D>(deserializer: D) -> Result<Option<PlanTaskId>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let opt: Option<String> = Option::deserialize(deserializer)?;
    match opt {
        None => Ok(None),
        Some(s) if s.is_empty() => Ok(None),
        Some(s) => match s.parse::<PlanTaskId>() {
            Ok(id) => Ok(Some(id)),
            Err(_) => Ok(None),
        },
    }
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
    pub tool_result: ContentBlock,
    pub pending_question: bool,
    /// CodeDiff artifact ID and content, if this action produced a code diff.
    pub code_diff: Option<(ArtifactId, exoskeleton_core::CodeDiffContent)>,
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
    use exoskeleton_core::llm::ContentBlock;
    use exoskeleton_core::tick::ActionOutcome;

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
    fn planned_action_with_task_id_roundtrip() {
        let task_id = exoskeleton_core::PlanTaskId::new();
        let action = PlannedAction {
            call_id: "call_1".into(),
            tool_name: "fs.write".into(),
            params: serde_json::json!({"path": "/tmp/out.txt"}),
            rationale: "Write output".into(),
            plan_task_id: Some(task_id),
            origin_exec_thread_id: None,
            proposal_id: None,
        };
        let json = serde_json::to_string(&action).unwrap();
        let parsed: PlannedAction = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.call_id, "call_1");
        assert_eq!(parsed.plan_task_id, Some(task_id));
    }

    #[test]
    fn planned_action_without_task_id_compat() {
        let json = r#"{"call_id":"call_2","tool_name":"fs.write","params":{},"rationale":"test"}"#;
        let parsed: PlannedAction = serde_json::from_str(json).unwrap();
        assert!(parsed.plan_task_id.is_none());
        assert_eq!(parsed.call_id, "call_2");
        assert_eq!(parsed.tool_name, "fs.write");
    }

    #[test]
    fn planned_action_non_uuid_task_id_is_dropped() {
        let json = r#"{"call_id":"call_3","tool_name":"fs.write","params":{},"rationale":"test","plan_task_id":"explore-project"}"#;
        let parsed: PlannedAction = serde_json::from_str(json).unwrap();
        assert!(parsed.plan_task_id.is_none());
    }

    #[test]
    fn snapshot_delta_default_is_no_change() {
        let delta = SnapshotDelta::default();
        assert!(delta.plan_update.is_none());
        assert!(delta.working_memory_ops.is_none());
    }

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
    fn extract_json_from_markdown_code_fence() {
        let text = r#"Here is data:

```json
{
    "tool_name": "fs.write"
}
```

Done."#;

        let json_str = extract_json_from_code_fence(text).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(json_str).unwrap();
        assert_eq!(parsed["tool_name"], "fs.write");
        assert!(extract_json_from_code_fence("no code fence here").is_none());
        assert_eq!(extract_json_from_code_fence("```json\n```"), Some(""));
    }

    #[test]
    fn action_execution_holds_tool_result() {
        let execution = ActionExecution {
            action: PlannedAction {
                call_id: "call_4".into(),
                tool_name: "delay".into(),
                params: serde_json::json!({"duration_ms": 1}),
                rationale: "test".into(),
                plan_task_id: None,
                origin_exec_thread_id: None,
                proposal_id: None,
            },
            result: Ok(serde_json::json!({"status": "ok"})),
            record: ActionRecord {
                action_type: "delay".into(),
                target: "{}".into(),
                receipt_ref: None,
                outcome: ActionOutcome::Success,
                origin_exec_thread_id: None,
                proposal_id: None,
            },
            tool_result: ContentBlock::ToolResult {
                tool_use_id: "call_4".into(),
                content: "{\"status\":\"ok\"}".into(),
                is_error: false,
            },
            pending_question: false,
            code_diff: None,
        };
        match execution.tool_result {
            ContentBlock::ToolResult { tool_use_id, .. } => assert_eq!(tool_use_id, "call_4"),
            other => panic!("expected tool result, got {other:?}"),
        }
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

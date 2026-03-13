//! PODAARA result types — internal to the kernel module.
//!
//! Each PODAARA step produces a typed result that flows to the next step.
//! These types are internal to the kernel — they are not part of the public API.

use exoskeleton_core::tick::{ActionRecord, LlmCallRecord, ThreadContribution};
use exoskeleton_core::{
    ArtifactId, EventEntry, MessageEnvelope, RelationshipRecord, RelationshipSnapshot, ThreadId,
    TickId, VesselId,
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
    pub thread_outputs: Vec<ThreadContribution>,
    pub pending_action_results: Vec<EventEntry>,
}

/// Output of the Orient step.
#[derive(Debug, Clone)]
pub struct OrientationResult {
    pub compiled_context: CompiledContext,
}

/// The decision protocol: structured JSON format for LLM responses.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecisionProtocol {
    pub reasoning: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_update: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_context_update: Option<String>,
    #[serde(default)]
    pub actions: Vec<PlannedAction>,
    #[serde(default)]
    pub memory_notes: Vec<String>,
}

/// A single action the LLM wants to execute.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlannedAction {
    pub tool_name: String,
    pub params: serde_json::Value,
    pub rationale: String,
}

/// Output of the Decide step.
#[derive(Debug, Clone)]
pub struct DecisionResult {
    pub reasoning: String,
    pub actions: Vec<PlannedAction>,
    pub snapshot_delta: SnapshotDelta,
    pub memory_notes: Vec<String>,
    pub llm_call_record: LlmCallRecord,
    pub response_artifact_id: ArtifactId,
}

/// Proposed changes to the StateSnapshot from the Decide step.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SnapshotDelta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_update: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_context_update: Option<String>,
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
    fn decision_protocol_roundtrip_full() {
        let proto = DecisionProtocol {
            reasoning: "I need to write a file".into(),
            plan_update: Some("Updated plan: write output".into()),
            working_context_update: Some("Writing output file".into()),
            actions: vec![
                PlannedAction {
                    tool_name: "fs.write".into(),
                    params: serde_json::json!({"path": "/tmp/out.txt", "content": "hello"}),
                    rationale: "Write the output".into(),
                },
                PlannedAction {
                    tool_name: "delay".into(),
                    params: serde_json::json!({"duration_ms": 100}),
                    rationale: "Wait for IO".into(),
                },
            ],
            memory_notes: vec![
                "Learned that fs.write works".into(),
                "Output path is /tmp/out.txt".into(),
            ],
        };
        let json = serde_json::to_string(&proto).unwrap();
        let parsed: DecisionProtocol = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.reasoning, proto.reasoning);
        assert_eq!(parsed.plan_update, proto.plan_update);
        assert_eq!(parsed.working_context_update, proto.working_context_update);
        assert_eq!(parsed.actions.len(), 2);
        assert_eq!(parsed.memory_notes.len(), 2);
    }

    #[test]
    fn decision_protocol_roundtrip_minimal() {
        let proto = DecisionProtocol {
            reasoning: "Nothing to do".into(),
            plan_update: None,
            working_context_update: None,
            actions: vec![],
            memory_notes: vec![],
        };
        let json = serde_json::to_string(&proto).unwrap();
        let parsed: DecisionProtocol = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.reasoning, "Nothing to do");
        assert!(parsed.plan_update.is_none());
        assert!(parsed.working_context_update.is_none());
        assert!(parsed.actions.is_empty());
        assert!(parsed.memory_notes.is_empty());
    }

    #[test]
    fn decision_protocol_optional_fields_omitted() {
        let proto = DecisionProtocol {
            reasoning: "Test".into(),
            plan_update: None,
            working_context_update: None,
            actions: vec![],
            memory_notes: vec![],
        };
        let value: serde_json::Value = serde_json::to_value(&proto).unwrap();
        let obj = value.as_object().unwrap();
        assert!(!obj.contains_key("plan_update"));
        assert!(!obj.contains_key("working_context_update"));
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
        };
        let json = serde_json::to_string(&action).unwrap();
        let parsed: PlannedAction = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.tool_name, "http.request");
        assert_eq!(parsed.params["url"], "https://example.com/api");
        assert_eq!(parsed.params["body"]["nested"][1], 2);
        assert_eq!(parsed.rationale, "Fetch data from API");
    }

    #[test]
    fn snapshot_delta_roundtrip() {
        // With updates
        let delta = SnapshotDelta {
            plan_update: Some("New plan".into()),
            working_context_update: Some("New context".into()),
        };
        let json = serde_json::to_string(&delta).unwrap();
        let parsed: SnapshotDelta = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.plan_update, Some("New plan".into()));
        assert_eq!(parsed.working_context_update, Some("New context".into()));

        // Without updates
        let delta_empty = SnapshotDelta {
            plan_update: None,
            working_context_update: None,
        };
        let json_empty = serde_json::to_string(&delta_empty).unwrap();
        let parsed_empty: SnapshotDelta = serde_json::from_str(&json_empty).unwrap();
        assert!(parsed_empty.plan_update.is_none());
        assert!(parsed_empty.working_context_update.is_none());
    }

    #[test]
    fn snapshot_delta_default_is_no_change() {
        let delta = SnapshotDelta::default();
        assert!(delta.plan_update.is_none());
        assert!(delta.working_context_update.is_none());
    }

    #[test]
    fn decision_protocol_empty_actions() {
        let json = r#"{"reasoning": "idle", "actions": [], "memory_notes": []}"#;
        let parsed: DecisionProtocol = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.reasoning, "idle");
        assert!(parsed.actions.is_empty());
        assert!(parsed.memory_notes.is_empty());
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

        // No code fence
        assert!(extract_json_from_code_fence("no code fence here").is_none());

        // Empty code fence
        let empty = "```json\n```";
        let content = extract_json_from_code_fence(empty);
        assert_eq!(content, Some(""));
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

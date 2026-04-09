//! Tool definitions, builders, and kernel-local tool processing.
//!
//! Three categories of tools:
//! - Cognitive tools: processed inside the kernel, never cross I9
//! - Introspection tools: query vessel state during Decide, never cross I9
//! - WI connector tools: exposed from WorldInterface and executed in Act

use exoskeleton_core::introspection::IntrospectionQuery;
use exoskeleton_core::llm::ToolDefinition;
use exoskeleton_core::plan::PlanUpdate;
use exoskeleton_core::watch::WatchProposal;
use exoskeleton_core::working_memory::WorkingMemoryOp;
use exoskeleton_core::{ExoError, VesselMode};
use serde_json::{json, Value};

use super::virtual_tools;
use super::KernelContext;

pub const COGNITIVE_TOOLS: &[&str] = &[
    "update_plan",
    "set_working_memory",
    "save_memory_note",
    "propose_watch",
    "request_vessel_mode",
    "reply_to_user",
];

pub const INTROSPECTION_TOOLS: &[&str] = &[
    "introspect_tick_history",
    "introspect_budget_status",
    "introspect_thread_status",
    "introspect_memory_search",
    "introspect_trust_scores",
    "introspect_connector_details",
    "introspect_watch_list",
    "introspect_event_history",
];

pub const FILTERED_TOOL_NAMES: &[&str] = &["signal.await", "signal.emit"];

#[derive(Debug, Clone, Default, PartialEq)]
pub struct CognitiveToolResult {
    pub plan_update: Option<PlanUpdate>,
    pub working_memory_ops: Vec<WorkingMemoryOp>,
    pub memory_note: Option<String>,
    pub watch_proposal: Option<WatchProposal>,
    pub vessel_mode_request: Option<VesselMode>,
    pub reply: Option<String>,
}

pub fn is_cognitive_tool(name: &str) -> bool {
    COGNITIVE_TOOLS.contains(&name)
}

pub fn is_introspection_tool(name: &str) -> bool {
    INTROSPECTION_TOOLS.contains(&name)
}

pub fn cognitive_tool_definitions() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            name: "update_plan".into(),
            description: "Replace the current plan or patch it with incremental operations.".into(),
            input_schema: json!({
                "type": "object",
                "required": ["type"],
                "properties": {
                    "type": { "type": "string", "enum": ["replace", "patch"] },
                    "plan": {
                        "type": "object",
                        "properties": {
                            "objective": { "type": "string" },
                            "tasks": {
                                "type": "array",
                                "items": {
                                    "type": "object",
                                    "properties": {
                                        "id": { "type": "string" },
                                        "description": { "type": "string" },
                                        "status": {
                                            "type": "string",
                                            "enum": ["pending", "in_progress", "completed", "failed", "blocked", "skipped"]
                                        },
                                        "depends_on": {
                                            "type": "array",
                                            "items": { "type": "string" }
                                        },
                                        "tool_hint": { "type": "string" }
                                    }
                                }
                            },
                            "updated_at": { "type": "string", "format": "date-time" }
                        }
                    },
                    "operations": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "op": {
                                    "type": "string",
                                    "enum": ["add_task", "update_status", "remove_task", "update_objective"]
                                },
                                "task_id": { "type": "string" },
                                "task": { "type": "object" },
                                "new_status": { "type": "string" },
                                "objective": { "type": "string" }
                            }
                        }
                    }
                }
            }),
        },
        ToolDefinition {
            name: "set_working_memory".into(),
            description: "Set or remove entries in working memory that persist across ticks."
                .into(),
            input_schema: json!({
                "type": "object",
                "required": ["ops"],
                "properties": {
                    "ops": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "required": ["op", "key"],
                            "properties": {
                                "op": { "type": "string", "enum": ["set", "remove"] },
                                "key": { "type": "string" },
                                "value": { "type": "string" },
                                "ttl_ticks": { "type": "integer", "minimum": 1 }
                            }
                        }
                    }
                }
            }),
        },
        ToolDefinition {
            name: "save_memory_note".into(),
            description: "Store an episodic memory note describing an observation worth keeping."
                .into(),
            input_schema: json!({
                "type": "object",
                "required": ["note"],
                "properties": {
                    "note": { "type": "string" }
                }
            }),
        },
        ToolDefinition {
            name: "propose_watch".into(),
            description: "Propose a persistent watch that should be reviewed by Align.".into(),
            input_schema: json!({
                "type": "object",
                "required": ["name", "description", "watch_type", "schedule"],
                "properties": {
                    "name": { "type": "string" },
                    "description": { "type": "string" },
                    "watch_type": {
                        "type": "object",
                        "description": "Tagged watch type (field \"type\" selects variant). Threshold example: {\"type\": \"threshold\", \"metric\": {\"metric\": \"budget_remaining\", \"dimension\": \"frontier_tokens\"}, \"condition\": {\"op\": \"below\", \"value\": 100}}. Poll example: {\"type\": \"poll\", \"connector\": \"http.request\", \"params\": {\"url\": \"https://example.com/health\"}, \"extract\": \"status\", \"condition\": {\"op\": \"above\", \"value\": 500}}. Metric variants: trust_level (principal_id), budget_remaining (dimension), consecutive_failures, tick_duration, event_count (event_type, lookback_ticks). Condition ops: above (value), below (value), changed."
                    },
                    "schedule": {
                        "type": "object",
                        "description": "Tagged schedule (field \"type\" selects variant). Examples: {\"type\": \"every_n_ticks\", \"n\": 5} or {\"type\": \"once\"}."
                    }
                }
            }),
        },
        ToolDefinition {
            name: "request_vessel_mode".into(),
            description: "Request a vessel mode transition to planning, executing, or normal."
                .into(),
            input_schema: json!({
                "type": "object",
                "required": ["mode"],
                "properties": {
                    "mode": { "type": "string", "enum": ["planning", "executing", "normal"] }
                }
            }),
        },
        ToolDefinition {
            name: "reply_to_user".into(),
            description: "Send a direct conversational reply to the user.".into(),
            input_schema: json!({
                "type": "object",
                "required": ["message"],
                "properties": {
                    "message": { "type": "string" }
                }
            }),
        },
    ]
}

pub fn introspection_tool_definitions() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            name: "introspect_tick_history".into(),
            description: "Query recent ticks with action counts, success rates, and token usage."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "limit": { "type": "integer", "default": 10, "minimum": 1, "maximum": 50 }
                }
            }),
        },
        ToolDefinition {
            name: "introspect_budget_status".into(),
            description: "Query current cognitive budget consumption and remaining allowance."
                .into(),
            input_schema: json!({ "type": "object", "properties": {} }),
        },
        ToolDefinition {
            name: "introspect_thread_status".into(),
            description: "Query registered threads with their statuses and recent outputs.".into(),
            input_schema: json!({ "type": "object", "properties": {} }),
        },
        ToolDefinition {
            name: "introspect_memory_search".into(),
            description: "Search episodic memory by topic and optional tags.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "topic": { "type": "string" },
                    "tags": { "type": "array", "items": { "type": "string" } }
                }
            }),
        },
        ToolDefinition {
            name: "introspect_trust_scores".into(),
            description: "Query current trust levels for known principals.".into(),
            input_schema: json!({ "type": "object", "properties": {} }),
        },
        ToolDefinition {
            name: "introspect_connector_details".into(),
            description: "Look up the full descriptor for a named connector.".into(),
            input_schema: json!({
                "type": "object",
                "required": ["name"],
                "properties": {
                    "name": { "type": "string" }
                }
            }),
        },
        ToolDefinition {
            name: "introspect_watch_list".into(),
            description: "List all active watches and their current status.".into(),
            input_schema: json!({ "type": "object", "properties": {} }),
        },
        ToolDefinition {
            name: "introspect_event_history".into(),
            description: "Query recent events, optionally filtered by event type.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "event_type": { "type": "string" },
                    "limit": { "type": "integer", "default": 20, "minimum": 1, "maximum": 50 }
                }
            }),
        },
    ]
}

pub fn introspection_tool_to_query(
    name: &str,
    input: &Value,
) -> Result<IntrospectionQuery, ExoError> {
    match name {
        "introspect_tick_history" => Ok(IntrospectionQuery::TickHistory {
            limit: input.get("limit").and_then(Value::as_u64).unwrap_or(10) as u32,
        }),
        "introspect_budget_status" => Ok(IntrospectionQuery::BudgetStatus),
        "introspect_thread_status" => Ok(IntrospectionQuery::ThreadStatus),
        "introspect_memory_search" => Ok(IntrospectionQuery::MemorySearch {
            topic: input
                .get("topic")
                .and_then(Value::as_str)
                .map(str::to_owned),
            tags: input
                .get("tags")
                .and_then(|value| serde_json::from_value::<Vec<String>>(value.clone()).ok()),
        }),
        "introspect_trust_scores" => Ok(IntrospectionQuery::TrustScores),
        "introspect_connector_details" => {
            let connector = input.get("name").and_then(Value::as_str).ok_or_else(|| {
                ExoError::Engine("introspect_connector_details requires string field 'name'".into())
            })?;
            Ok(IntrospectionQuery::ConnectorDetails {
                name: connector.into(),
            })
        }
        "introspect_watch_list" => Ok(IntrospectionQuery::WatchList),
        "introspect_event_history" => Ok(IntrospectionQuery::EventHistory {
            event_type: input
                .get("event_type")
                .and_then(Value::as_str)
                .map(str::to_owned),
            limit: input.get("limit").and_then(Value::as_u64).unwrap_or(20) as u32,
        }),
        _ => Err(ExoError::Engine(format!(
            "unknown introspection tool: {name}"
        ))),
    }
}

pub fn process_cognitive_tool(name: &str, input: &Value) -> Result<CognitiveToolResult, ExoError> {
    match name {
        "update_plan" => Ok(CognitiveToolResult {
            plan_update: Some(
                serde_json::from_value(input.clone())
                    .map_err(|e| ExoError::Engine(format!("update_plan invalid input: {e}")))?,
            ),
            ..CognitiveToolResult::default()
        }),
        "set_working_memory" => {
            let ops = input
                .get("ops")
                .cloned()
                .ok_or_else(|| ExoError::Engine("set_working_memory requires field 'ops'".into()))
                .and_then(|value| {
                    serde_json::from_value::<Vec<WorkingMemoryOp>>(value).map_err(|e| {
                        ExoError::Engine(format!("set_working_memory invalid ops: {e}"))
                    })
                })?;
            Ok(CognitiveToolResult {
                working_memory_ops: ops,
                ..CognitiveToolResult::default()
            })
        }
        "save_memory_note" => Ok(CognitiveToolResult {
            memory_note: Some(
                input
                    .get("note")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        ExoError::Engine("save_memory_note requires string field 'note'".into())
                    })?
                    .to_string(),
            ),
            ..CognitiveToolResult::default()
        }),
        "propose_watch" => Ok(CognitiveToolResult {
            watch_proposal: Some(serde_json::from_value(input.clone()).map_err(|e| {
                ExoError::Engine(format!("propose_watch invalid watch proposal: {e}"))
            })?),
            ..CognitiveToolResult::default()
        }),
        "request_vessel_mode" => Ok(CognitiveToolResult {
            vessel_mode_request: Some(
                serde_json::from_value(input.get("mode").cloned().ok_or_else(|| {
                    ExoError::Engine("request_vessel_mode requires field 'mode'".into())
                })?)
                .map_err(|e| ExoError::Engine(format!("request_vessel_mode invalid mode: {e}")))?,
            ),
            ..CognitiveToolResult::default()
        }),
        "reply_to_user" => Ok(CognitiveToolResult {
            reply: Some(
                input
                    .get("message")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        ExoError::Engine("reply_to_user requires string field 'message'".into())
                    })?
                    .to_string(),
            ),
            ..CognitiveToolResult::default()
        }),
        _ => Err(ExoError::Engine(format!("unknown cognitive tool: {name}"))),
    }
}

/// Shared accumulator state for cognitive tool results.
///
/// Used by both Decide (full) and DecideLite (inner loop) to avoid duplicating
/// the cognitive outcome folding logic.
#[derive(Default)]
pub struct CognitiveAccumulator {
    pub reasoning_parts: Vec<String>,
    pub reply: Option<String>,
    pub actions: Vec<super::types::PlannedAction>,
    pub snapshot_delta: super::types::SnapshotDelta,
    pub memory_notes: Vec<String>,
    pub watch_proposals: Vec<WatchProposal>,
    pub vessel_mode_request: Option<VesselMode>,
}

/// Fold a single cognitive tool result into the shared accumulator.
pub fn accumulate_cognitive_outcome(
    accumulator: &mut CognitiveAccumulator,
    result: CognitiveToolResult,
) {
    if let Some(plan_update) = result.plan_update {
        accumulator.snapshot_delta.plan_update = Some(plan_update);
    }
    if !result.working_memory_ops.is_empty() {
        accumulator
            .snapshot_delta
            .working_memory_ops
            .get_or_insert_with(Vec::new)
            .extend(result.working_memory_ops);
    }
    if let Some(note) = result.memory_note {
        accumulator.memory_notes.push(note);
    }
    if let Some(watch) = result.watch_proposal {
        accumulator.watch_proposals.push(watch);
    }
    if let Some(mode) = result.vessel_mode_request {
        accumulator.vessel_mode_request = Some(mode);
    }
    if let Some(message) = result.reply {
        accumulator.reply = Some(message);
    }
}

pub fn build_decide_tools(kernel: &KernelContext) -> Vec<ToolDefinition> {
    let mut tools = connector_tool_definitions(kernel);
    tools.extend(
        virtual_tools::virtual_tool_definitions()
            .into_iter()
            .filter(|tool| {
                tool.name != virtual_tools::PEER_RESOLVE || kernel.observatory_url.is_some()
            }),
    );
    tools.extend(cognitive_tool_definitions());
    tools.extend(introspection_tool_definitions());
    tools
}

pub fn build_inner_loop_tools(kernel: &KernelContext) -> Vec<ToolDefinition> {
    let mut tools = connector_tool_definitions(kernel);
    tools.extend(cognitive_tool_definitions());
    tools
}

fn connector_tool_definitions(kernel: &KernelContext) -> Vec<ToolDefinition> {
    let guard = kernel.wi_host_slot.try_lock();
    let capabilities = match guard {
        Ok(slot) => slot
            .as_ref()
            .map(|host| host.list_capabilities())
            .unwrap_or_default(),
        Err(_) => Vec::new(),
    };

    capabilities
        .into_iter()
        .filter(|descriptor| !FILTERED_TOOL_NAMES.contains(&descriptor.name.as_str()))
        .map(|descriptor| ToolDefinition {
            name: descriptor.name,
            description: descriptor.description,
            input_schema: descriptor
                .input_schema
                .unwrap_or_else(|| json!({ "type": "object", "properties": {} })),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cognitive_tool_definitions_count() {
        let tools = cognitive_tool_definitions();
        assert_eq!(tools.len(), 6, "expected 6 cognitive tools");
    }

    #[test]
    fn cognitive_tool_definitions_have_schemas() {
        for tool in cognitive_tool_definitions() {
            assert!(!tool.name.is_empty());
            assert!(!tool.description.is_empty());
            assert!(
                tool.input_schema.is_object(),
                "schema for {} must be an object",
                tool.name
            );
        }
    }

    #[test]
    fn is_cognitive_tool_recognizes_all() {
        assert!(is_cognitive_tool("update_plan"));
        assert!(is_cognitive_tool("set_working_memory"));
        assert!(is_cognitive_tool("save_memory_note"));
        assert!(is_cognitive_tool("propose_watch"));
        assert!(is_cognitive_tool("request_vessel_mode"));
        assert!(is_cognitive_tool("reply_to_user"));
    }

    #[test]
    fn is_cognitive_tool_rejects_non_cognitive() {
        assert!(!is_cognitive_tool("code.read"));
        assert!(!is_cognitive_tool("introspect_tick_history"));
        assert!(!is_cognitive_tool("agent.ask_user"));
        assert!(!is_cognitive_tool(""));
    }

    #[test]
    fn introspection_tool_definitions_count() {
        let tools = introspection_tool_definitions();
        assert_eq!(tools.len(), 8, "expected 8 introspection tools");
    }

    #[test]
    fn introspection_tool_definitions_have_schemas() {
        for tool in introspection_tool_definitions() {
            assert!(!tool.name.is_empty());
            assert!(!tool.description.is_empty());
            assert!(
                tool.input_schema.is_object(),
                "schema for {} must be an object",
                tool.name
            );
        }
    }

    #[test]
    fn is_introspection_tool_recognizes_all() {
        for name in INTROSPECTION_TOOLS {
            assert!(
                is_introspection_tool(name),
                "{name} should be introspection"
            );
        }
    }

    #[test]
    fn is_introspection_tool_rejects_non_introspection() {
        assert!(!is_introspection_tool("code.read"));
        assert!(!is_introspection_tool("update_plan"));
        assert!(!is_introspection_tool(""));
    }

    #[test]
    fn introspection_tool_to_query_tick_history() {
        let q =
            introspection_tool_to_query("introspect_tick_history", &json!({"limit": 5})).unwrap();
        assert!(matches!(q, IntrospectionQuery::TickHistory { limit: 5 }));
    }

    #[test]
    fn introspection_tool_to_query_budget_status() {
        let q = introspection_tool_to_query("introspect_budget_status", &json!({})).unwrap();
        assert!(matches!(q, IntrospectionQuery::BudgetStatus));
    }

    #[test]
    fn introspection_tool_to_query_unknown_tool_errors() {
        let result = introspection_tool_to_query("unknown_tool", &json!({}));
        assert!(result.is_err());
    }

    #[test]
    fn introspection_tool_to_query_memory_search_with_tags() {
        let q = introspection_tool_to_query(
            "introspect_memory_search",
            &json!({"topic": "deployment", "tags": ["ops", "infra"]}),
        )
        .unwrap();
        match q {
            exoskeleton_core::introspection::IntrospectionQuery::MemorySearch { topic, tags } => {
                assert_eq!(topic, Some("deployment".into()));
                assert_eq!(tags, Some(vec!["ops".into(), "infra".into()]));
            }
            other => panic!("expected MemorySearch, got: {other:?}"),
        }
    }

    #[test]
    fn process_cognitive_tool_reply_to_user() {
        let result = process_cognitive_tool("reply_to_user", &json!({"message": "hi"})).unwrap();
        assert_eq!(result.reply.as_deref(), Some("hi"));
    }

    #[test]
    fn process_cognitive_tool_set_working_memory() {
        let result = process_cognitive_tool(
            "set_working_memory",
            &json!({"ops": [{"op": "set", "key": "k", "value": "v", "ttl_ticks": 2}]}),
        )
        .unwrap();
        assert_eq!(result.working_memory_ops.len(), 1);
    }

    #[test]
    fn process_cognitive_tool_update_plan_patch() {
        let task_id = "00000000-0000-0000-0000-000000000001";
        let input = json!({
            "type": "patch",
            "operations": [
                {"op": "update_status", "task_id": task_id, "new_status": "completed"}
            ]
        });
        let result = process_cognitive_tool("update_plan", &input).unwrap();
        match result.plan_update {
            Some(exoskeleton_core::plan::PlanUpdate::Patch { operations }) => {
                assert_eq!(operations.len(), 1);
            }
            other => panic!("expected PlanUpdate::Patch, got: {other:?}"),
        }
    }
}

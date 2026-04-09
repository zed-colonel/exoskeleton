//! Decide step — multi-turn LLM reasoning with native tool use.
//!
//! The Decide step is an iterative loop. Each iteration:
//! 1. Calls the LLM backend directly
//! 2. Accumulates assistant text as reasoning
//! 3. Resolves introspection and cognitive tool calls inline
//! 4. Accumulates WI and virtual tool calls as planned actions
//! 5. Continues only when there are inline tool results to feed back

use actionqueue_executor_local::CancellationToken;
use exoskeleton_core::llm::{ContentBlock, LlmBackend, LlmMessage, LlmRequest, LlmRole};
use exoskeleton_core::tick::LlmCallRecord;
use exoskeleton_core::{Artifact, ArtifactId, ArtifactKind, ExoError};
use serde_json::json;

use super::tools::{
    accumulate_cognitive_outcome, build_decide_tools, introspection_tool_to_query,
    is_cognitive_tool, is_introspection_tool, process_cognitive_tool, CognitiveAccumulator,
};
use super::types::{DecisionResult, OrientationResult, PlannedAction};
use super::KernelContext;
use crate::cognitive_engine::CognitiveHandler;
use crate::introspection::IntrospectionService;

/// Execute the Decide step using native tool_use / function-calling.
pub fn decide(
    handler: &CognitiveHandler,
    kernel: &KernelContext,
    orientation: &OrientationResult,
    cancellation: &CancellationToken,
) -> Result<DecisionResult, ExoError> {
    if cancellation.is_cancelled() {
        return Err(ExoError::LlmInvocation(
            "cancelled before Decide step".into(),
        ));
    }

    let system_prompt = build_system_prompt(kernel)?;
    let backend_type = resolve_backend(handler, kernel, orientation);
    let introspection = IntrospectionService::new(kernel);
    let tools = build_decide_tools(kernel);
    let max_turns = kernel.max_decide_turns;

    let mut messages = vec![LlmMessage::text(
        LlmRole::User,
        orientation.compiled_context.prompt.clone(),
    )];
    let mut llm_records = Vec::new();
    let mut accumulator = CognitiveAccumulator::default();
    let mut last_response_artifact_id: Option<ArtifactId> = None;

    for turn in 0..max_turns {
        if cancellation.is_cancelled() {
            return Err(ExoError::LlmInvocation(
                "cancelled during Decide loop".into(),
            ));
        }

        if turn > 0 {
            if let Some(ref tracker) = kernel.budget_tracker {
                if let Ok(guard) = tracker.lock() {
                    if guard.budget_status(u64::MAX).local_tokens_remaining == 0 {
                        tracing::info!(turn, "budget exhausted, ending Decide loop");
                        break;
                    }
                }
            }
        }

        let request = LlmRequest {
            backend: Some(backend_type),
            system_prompt: Some(system_prompt.clone()),
            messages: messages.clone(),
            max_output_tokens: kernel.max_output_tokens,
            temperature: Some(0.7),
            stop_sequences: vec![],
            stream: true,
            tools: tools.clone(),
        };

        let result =
            crate::llm::direct::handler_direct_llm_call(handler, kernel, &request, cancellation)?;
        let response = result.response;
        llm_records.push(result.llm_call_record);
        last_response_artifact_id = Some(result.artifact_id.clone());

        let assistant_message = LlmMessage {
            role: LlmRole::Assistant,
            content: response.content_blocks.clone(),
        };

        let mut tool_results = Vec::new();
        let turn_reasoning = response.text();
        if !turn_reasoning.trim().is_empty() {
            accumulator.reasoning_parts.push(turn_reasoning.clone());
        }

        let actions_before = accumulator.actions.len();
        for block in &response.content_blocks {
            if let ContentBlock::ToolUse { id, name, input } = block {
                if is_introspection_tool(name) {
                    let tool_result = resolve_introspection_tool(name, input, id, &introspection)?;
                    tool_results.push(tool_result);
                    continue;
                }

                if is_cognitive_tool(name) {
                    let tool_result = resolve_cognitive_tool(name, input, id, &mut accumulator)?;
                    tool_results.push(tool_result);
                    continue;
                }

                accumulator.actions.push(PlannedAction {
                    call_id: id.clone(),
                    tool_name: name.clone(),
                    params: input.clone(),
                    rationale: action_rationale(&turn_reasoning, name),
                    plan_task_id: None,
                });
            }
        }
        let has_external_actions = accumulator.actions.len() > actions_before;

        messages.push(assistant_message);
        if !tool_results.is_empty() {
            messages.push(LlmMessage {
                role: LlmRole::User,
                content: tool_results.clone(),
            });
        }

        if has_external_actions {
            // When the LLM emits both cognitive and WI tools in a single turn, we break
            // immediately to pass WI actions to the Act step. The cognitive "ok" acknowledgments
            // are accumulated in messages but never sent back — this is intentional because the
            // Decide loop is ending and the inner loop (if activated) takes over from here.
            break;
        }

        if matches!(
            response.stop_reason,
            exoskeleton_core::llm::StopReason::ToolUse
        ) && !tool_results.is_empty()
        {
            continue;
        }

        break;
    }

    build_decision_result(
        kernel,
        accumulator,
        llm_records,
        last_response_artifact_id.ok_or_else(|| {
            ExoError::LlmInvocation("Decide produced no LLM response artifact".into())
        })?,
    )
}

fn resolve_introspection_tool(
    name: &str,
    input: &serde_json::Value,
    call_id: &str,
    introspection: &IntrospectionService<'_>,
) -> Result<ContentBlock, ExoError> {
    match introspection_tool_to_query(name, input) {
        Ok(query) => {
            let result = introspection
                .query(&query)
                .unwrap_or_else(|e| json!({ "error": e.to_string() }));
            Ok(ContentBlock::ToolResult {
                tool_use_id: call_id.to_string(),
                content: serde_json::to_string(&result).unwrap_or_else(|_| "{}".into()),
                is_error: result.get("error").is_some(),
            })
        }
        Err(error) => Ok(ContentBlock::ToolResult {
            tool_use_id: call_id.to_string(),
            content: error.to_string(),
            is_error: true,
        }),
    }
}

fn resolve_cognitive_tool(
    name: &str,
    input: &serde_json::Value,
    call_id: &str,
    accumulator: &mut CognitiveAccumulator,
) -> Result<ContentBlock, ExoError> {
    match process_cognitive_tool(name, input) {
        Ok(result) => {
            accumulate_cognitive_outcome(accumulator, result);
            Ok(ContentBlock::ToolResult {
                tool_use_id: call_id.to_string(),
                content: "ok".into(),
                is_error: false,
            })
        }
        Err(error) => Ok(ContentBlock::ToolResult {
            tool_use_id: call_id.to_string(),
            content: error.to_string(),
            is_error: true,
        }),
    }
}

fn action_rationale(reasoning: &str, tool_name: &str) -> String {
    let trimmed = reasoning.trim();
    if trimmed.is_empty() {
        format!("model selected {tool_name}")
    } else {
        trimmed.to_string()
    }
}

fn build_decision_result(
    kernel: &KernelContext,
    accumulator: CognitiveAccumulator,
    llm_records: Vec<LlmCallRecord>,
    response_artifact_id: ArtifactId,
) -> Result<DecisionResult, ExoError> {
    let reasoning = accumulator.reasoning_parts.join("\n");
    let inner_loop_requested = !accumulator.actions.is_empty();

    let decision_json = json!({
        "reasoning": reasoning,
        "reply": accumulator.reply,
        "actions": accumulator.actions,
        "plan_update": accumulator.snapshot_delta.plan_update,
        "working_memory_ops": accumulator.snapshot_delta.working_memory_ops,
        "memory_notes": accumulator.memory_notes,
        "watch_proposals": accumulator.watch_proposals,
        "vessel_mode_request": accumulator.vessel_mode_request,
        "inner_loop_requested": inner_loop_requested,
    });
    let decision_artifact = Artifact::from_json(ArtifactKind::Decision, &decision_json)?;
    kernel.artifact_store.put(&decision_artifact)?;

    let mut merged_record = LlmCallRecord::merge(&llm_records);
    merged_record.response_artifact_ref = Some(response_artifact_id.clone());

    Ok(DecisionResult {
        reasoning,
        reply: accumulator.reply,
        actions: accumulator.actions,
        snapshot_delta: accumulator.snapshot_delta,
        memory_notes: accumulator.memory_notes,
        llm_call_record: merged_record,
        response_artifact_id,
        watch_proposals: accumulator.watch_proposals,
        inner_loop_requested,
        vessel_mode_request: accumulator.vessel_mode_request,
    })
}

fn build_system_prompt(kernel: &KernelContext) -> Result<String, ExoError> {
    let mut system = kernel.prompt_registry.resolve(
        "decide-system",
        &[
            ("vessel_id", &kernel.vessel_id.to_string()),
            ("mission", &kernel.mission),
        ],
    )?;

    let vessel_mode = *kernel.vessel_mode.lock().unwrap();
    if kernel.inner_loop_config.enabled || vessel_mode != exoskeleton_core::VesselMode::Normal {
        let plan_section = match vessel_mode {
            exoskeleton_core::VesselMode::Planning => {
                "You are in Planning mode. Restrict yourself to read-only external tools and use cognitive tools to draft or refine a plan.".to_string()
            }
            exoskeleton_core::VesselMode::Executing => {
                "You are in Executing mode. Follow the approved plan and use cognitive tools to update task status as work progresses.".to_string()
            }
            exoskeleton_core::VesselMode::Normal => {
                "Use cognitive tools for plan, memory, mode, watch, and reply updates. External tool calls are threaded into Act automatically.".to_string()
            }
        };

        if let Ok(coding_section) = kernel
            .prompt_registry
            .resolve("coding-system", &[("plan_section", &plan_section)])
        {
            system.push_str("\n\n");
            system.push_str(&coding_section);
        }
    }

    Ok(system)
}

/// Determine which LLM backend to use for this tick's Decide step.
fn resolve_backend(
    handler: &CognitiveHandler,
    kernel: &KernelContext,
    _orientation: &OrientationResult,
) -> LlmBackend {
    let tracker = match kernel.budget_tracker {
        Some(ref t) => t,
        None => return handler.default_backend,
    };

    let guard = match tracker.lock() {
        Ok(g) => g,
        Err(_) => return handler.default_backend,
    };

    if handler.frontier_backend.is_none() {
        return handler.default_backend;
    }

    let should_escalate = check_escalation_triggers(kernel, &guard);

    if !should_escalate {
        return handler.default_backend;
    }

    if guard.remaining_frontier_tokens() == 0 || guard.remaining_frontier_cost() == 0 {
        tracing::info!("escalation triggered but frontier budget exhausted; staying local");
        return LlmBackend::Local;
    }

    let policy = &guard.config().escalation_policy;
    if guard.frontier_calls_this_window() >= policy.max_frontier_calls_per_window {
        tracing::info!("escalation triggered but frontier call limit reached; staying local");
        return LlmBackend::Local;
    }

    tracing::info!("escalating to frontier model");
    LlmBackend::Frontier
}

fn check_escalation_triggers(
    kernel: &KernelContext,
    tracker: &crate::budget::CognitiveBudgetTracker,
) -> bool {
    let policy = &tracker.config().escalation_policy;

    if tracker.consecutive_failures() >= policy.escalate_on_consecutive_failures {
        tracing::info!(
            failures = tracker.consecutive_failures(),
            "escalation trigger: consecutive failures"
        );
        return true;
    }

    if let Some(thrash) = extract_thrash_indicator(kernel) {
        if thrash > policy.escalate_on_uncertainty {
            tracing::info!(thrash, "escalation trigger: high uncertainty/thrash");
            return true;
        }
    }

    if extract_high_threat(kernel) {
        tracing::info!("escalation trigger: high threat severity");
        return true;
    }

    false
}

fn extract_thrash_indicator(kernel: &KernelContext) -> Option<f64> {
    let outputs = kernel
        .thread_registry
        .recent_outputs(exoskeleton_threads::SELF_CRITIQUE_ID, 1)
        .ok()?;
    let output = outputs.first()?;
    let artifact = kernel.artifact_store.get(&output.artifact_id).ok()??;
    let critique: exoskeleton_threads::SelfCritique =
        serde_json::from_slice(&artifact.content).ok()?;
    Some(critique.thrash_indicator)
}

fn extract_high_threat(kernel: &KernelContext) -> bool {
    let outputs = match kernel
        .thread_registry
        .recent_outputs(exoskeleton_threads::THREAT_MONITOR_ID, 1)
    {
        Ok(o) => o,
        Err(_) => return false,
    };
    let output = match outputs.first() {
        Some(o) => o,
        None => return false,
    };

    let artifact = match kernel.artifact_store.get(&output.artifact_id) {
        Ok(Some(a)) => a,
        _ => return false,
    };
    let assessment: exoskeleton_threads::ThreatAssessment =
        match serde_json::from_slice(&artifact.content) {
            Ok(a) => a,
            Err(_) => return false,
        };
    matches!(
        assessment.severity,
        exoskeleton_threads::ThreatSeverity::High | exoskeleton_threads::ThreatSeverity::Critical
    )
}

//! Bounded inner interaction loop (E8-S1).
//!
//! Runs within the Decide→Act section of a PODAARA tick, replacing the single
//! Decide→Act pass with an iterative DecideLite→Align→Act→Observe cycle:
//!
//! ```text
//! Perceive → Orient → [Inner Loop: DecideLite → Align → Act(1 tool) → Observe] × N → Reflect → Amend
//! ```
//!
//! The inner loop activates when BOTH conditions are true:
//! 1. `inner_loop.enabled = true` in VesselConfig (capability gate)
//! 2. The initial Decide response includes `inner_loop_requested: true` (agent decision)
//!
//! Every inner-loop action passes through the Align gate (I8) and budget enforcement (I6).
//! Every LLM response is stored as a content-addressed artifact (I3).

use actionqueue_executor_local::CancellationToken;
use exoskeleton_core::event::InnerLoopStepDetail;
use exoskeleton_core::llm::{ContentBlock, LlmMessage, LlmRequest, LlmRole, StopReason};
use exoskeleton_core::tick::LlmCallRecord;
use exoskeleton_core::RelationshipSnapshot;
use exoskeleton_core::{DiffSummary, EventType, ExoError, LiveEvent, RelationshipRecord};
use super::types::{
    ActionExecution, DecisionResult, OrientationResult, PerceptionResult, PlannedAction,
};
use super::tools::{
    accumulate_cognitive_outcome, build_inner_loop_tools, is_cognitive_tool, process_cognitive_tool,
    CognitiveAccumulator,
};
use super::{act, align, diff_tracker::DiffTracker, KernelContext};
use crate::budget::session::{
    DoomLoopStatus, SessionBudget, SessionBudgetCheck, SessionCompletionReason,
};
use crate::cognitive_engine::CognitiveHandler;
use crate::llm::direct::handler_direct_llm_call;

/// Result of the inner loop, consumed by the outer tick for Reflect and Amend.
pub struct InnerLoopResult {
    /// All actions executed across all steps.
    pub executions: Vec<ActionExecution>,
    /// Aggregated LLM call records from all inner-loop Decide calls.
    pub llm_call_records: Vec<LlmCallRecord>,
    /// Final decision result (from last DecideLite iteration).
    pub final_decision: DecisionResult,
    /// Why the inner loop ended.
    pub completion_reason: SessionCompletionReason,
    /// Total steps executed.
    pub steps_taken: u32,
    /// Accumulated relationship updates from all per-step Align calls (I8).
    pub relationship_updates: Vec<RelationshipRecord>,
    /// Relationship snapshot from the last Align call (most recent compilation).
    pub relationship_snapshot: Option<RelationshipSnapshot>,
}

/// Run the bounded inner interaction loop.
///
/// Called from run_tick() after Orient, replacing the single Decide→Act pass.
/// Returns an aggregated result that the outer tick uses for Reflect and Amend.
#[allow(clippy::too_many_arguments)]
pub fn run_inner_loop(
    handler: &CognitiveHandler,
    kernel: &KernelContext,
    orientation: &OrientationResult,
    perception: &PerceptionResult,
    initial_decision: DecisionResult,
    diff_tracker: &mut DiffTracker,
    cancellation: &CancellationToken,
    tick_number: u64,
    tick_id: exoskeleton_core::TickId,
) -> Result<InnerLoopResult, ExoError> {
    let config = &kernel.inner_loop_config;
    let mut session = SessionBudget::new(config);
    let mut all_executions = Vec::new();
    let mut all_llm_records = Vec::new();
    let mut all_relationship_updates: Vec<RelationshipRecord> = Vec::new();
    let mut last_relationship_snapshot: Option<RelationshipSnapshot> = None;
    let mut current_decision = initial_decision;
    let mut step = 0u32;

    // Broadcast InnerLoopStarted
    broadcast_inner_loop_event(
        kernel,
        tick_number,
        EventType::InnerLoopStarted,
        format!(
            "Inner loop started (max {} steps, {} token budget)",
            config.max_steps_per_tick, config.max_tokens_per_session
        ),
        None,
        None,
    );

    loop {
        // ── Check session budget ──
        if let SessionBudgetCheck::Stop(reason) = session.can_continue() {
            let completion = SessionCompletionReason::from(reason);
            broadcast_inner_loop_completed(
                kernel,
                tick_number,
                step,
                &session,
                &completion,
                diff_tracker,
            );
            return Ok(InnerLoopResult {
                executions: all_executions,
                llm_call_records: all_llm_records,
                final_decision: current_decision,
                completion_reason: completion,
                steps_taken: step,
                relationship_updates: all_relationship_updates,
                relationship_snapshot: last_relationship_snapshot,
            });
        }

        // ── Check cancellation ──
        if cancellation.is_cancelled() {
            broadcast_inner_loop_completed(
                kernel,
                tick_number,
                step,
                &session,
                &SessionCompletionReason::Cancelled,
                diff_tracker,
            );
            return Ok(InnerLoopResult {
                executions: all_executions,
                llm_call_records: all_llm_records,
                final_decision: current_decision,
                completion_reason: SessionCompletionReason::Cancelled,
                steps_taken: step,
                relationship_updates: all_relationship_updates,
                relationship_snapshot: last_relationship_snapshot,
            });
        }

        // ── Check if agent decided it's done ──
        if current_decision.actions.is_empty() {
            broadcast_inner_loop_completed(
                kernel,
                tick_number,
                step,
                &session,
                &SessionCompletionReason::AgentComplete,
                diff_tracker,
            );
            return Ok(InnerLoopResult {
                executions: all_executions,
                llm_call_records: all_llm_records,
                final_decision: current_decision,
                completion_reason: SessionCompletionReason::AgentComplete,
                steps_taken: step,
                relationship_updates: all_relationship_updates,
                relationship_snapshot: last_relationship_snapshot,
            });
        }

        step += 1;

        // ── Align ── (trust gate, same as outer tick)
        let alignment = align::align(kernel, &current_decision, perception, tick_id, tick_number);

        // Accumulate relationship data from this Align step (I8)
        all_relationship_updates.extend(alignment.relationship_updates.clone());
        if alignment.relationship_snapshot.is_some() {
            last_relationship_snapshot = alignment.relationship_snapshot.clone();
        }

        // ── Act (execute approved actions) ──
        let act_result = act::act(kernel, &alignment, tick_id, cancellation)?;
        let step_diff_summary = build_step_diff_summary(&act_result.executions);
        for exec in &act_result.executions {
            if let Some((ref id, ref content)) = exec.code_diff {
                diff_tracker.record(id.clone(), content.clone());
            }
        }

        // Record tool calls for doom-loop detection
        for exec in &act_result.executions {
            session.record_tool_call(
                &exec.action.tool_name,
                &exec.action.params,
                exec.result.as_ref().err().map(String::as_str),
            );
        }

        if act_result
            .executions
            .iter()
            .any(|exec| exec.pending_question)
        {
            broadcast_inner_loop_completed(
                kernel,
                tick_number,
                step,
                &session,
                &SessionCompletionReason::AwaitingInput,
                diff_tracker,
            );
            return Ok(InnerLoopResult {
                executions: {
                    all_executions.extend(act_result.executions.clone());
                    all_executions
                },
                llm_call_records: all_llm_records,
                final_decision: current_decision,
                completion_reason: SessionCompletionReason::AwaitingInput,
                steps_taken: step,
                relationship_updates: all_relationship_updates,
                relationship_snapshot: last_relationship_snapshot,
            });
        }

        let correction_message = match session.doom_loop_status() {
            DoomLoopStatus::CorrectionNeeded {
                tool,
                count,
                last_error,
                ..
            } => {
                session.acknowledge_correction();
                Some(build_doom_loop_correction(
                    &tool,
                    count,
                    last_error.as_deref(),
                ))
            }
            DoomLoopStatus::HardStop { .. } => {
                broadcast_inner_loop_completed(
                    kernel,
                    tick_number,
                    step,
                    &session,
                    &SessionCompletionReason::DoomLoop,
                    diff_tracker,
                );
                return Ok(InnerLoopResult {
                    executions: {
                        all_executions.extend(act_result.executions.clone());
                        all_executions
                    },
                    llm_call_records: all_llm_records,
                    final_decision: current_decision,
                    completion_reason: SessionCompletionReason::DoomLoop,
                    steps_taken: step,
                    relationship_updates: all_relationship_updates,
                    relationship_snapshot: last_relationship_snapshot,
                });
            }
            DoomLoopStatus::Clear => None,
        };

        // Capture tool summary before extending all_executions
        let tool_summary = act_result.executions.first().map(|e| {
            let outcome = if e.result.is_ok() {
                "success"
            } else {
                "failed"
            };
            (e.action.tool_name.clone(), outcome.to_string())
        });

        all_executions.extend(act_result.executions.clone());

        // ── DecideLite ── (next iteration)
        let decide_result = decide_lite(
            handler,
            kernel,
            orientation,
            &all_executions,
            &current_decision,
            correction_message.as_deref(),
            cancellation,
        )?;

        // Record tokens
        let tokens_this_step =
            decide_result.llm_call_record.tokens_in + decide_result.llm_call_record.tokens_out;
        session.record_llm_tokens(
            decide_result.llm_call_record.tokens_in,
            decide_result.llm_call_record.tokens_out,
        );
        all_llm_records.push(decide_result.llm_call_record.clone());

        // Broadcast InnerLoopStep AFTER DecideLite so tokens_this_step is accurate
        broadcast_inner_loop_event(
            kernel,
            tick_number,
            EventType::InnerLoopStep,
            format!(
                "Step {}/{}: {}",
                step,
                config.max_steps_per_tick,
                tool_summary
                    .as_ref()
                    .map(|(name, outcome)| format!("{name} ({outcome})"))
                    .unwrap_or_else(|| "no actions".into()),
            ),
            Some(InnerLoopStepDetail {
                step_number: step,
                max_steps: config.max_steps_per_tick,
                tool_name: tool_summary.as_ref().map(|(name, _)| name.clone()),
                tool_outcome: tool_summary.as_ref().map(|(_, outcome)| outcome.clone()),
                tokens_this_step,
                tokens_total: session.tokens_consumed(),
                completion_reason: None,
            }),
            step_diff_summary,
        );

        current_decision = decide_result;
    }
}

/// Lightweight Decide for inner-loop iterations.
///
/// Reuses the Orient context from the tick's initial Decide, supplemented
/// with tool results from previous inner-loop steps. Does not support
/// multi-turn introspection queries — those are for the full Decide step.
#[allow(clippy::too_many_arguments)]
fn decide_lite(
    handler: &CognitiveHandler,
    kernel: &KernelContext,
    orientation: &OrientationResult,
    previous_executions: &[ActionExecution],
    previous_decision: &DecisionResult,
    correction_message: Option<&str>,
    cancellation: &CancellationToken,
) -> Result<DecisionResult, ExoError> {
    let mode_context = build_mode_context(kernel);
    let system_prompt = kernel.prompt_registry.resolve(
        "inner-loop-system",
        &[("mode_context", &mode_context)],
    )?;

    let mut messages = vec![LlmMessage::text(
        LlmRole::User,
        orientation.compiled_context.prompt.clone(),
    )];

    let window_size = kernel.inner_loop_config.context_window_size.max(1) as usize;
    let start = previous_executions.len().saturating_sub(window_size);
    let windowed = &previous_executions[start..];

    for (i, execution) in windowed.iter().enumerate() {
        messages.push(LlmMessage {
            role: LlmRole::Assistant,
            content: assistant_blocks_for_action(&execution.action),
        });
        messages.push(LlmMessage {
            role: LlmRole::User,
            content: user_blocks_for_tool_result(
                execution.tool_result.clone(),
                correction_message.filter(|_| i == windowed.len() - 1),
            ),
        });
    }

    if windowed.is_empty() {
        messages.push(LlmMessage {
            role: LlmRole::Assistant,
            content: assistant_blocks_for_decision(previous_decision),
        });
    }

    let tools = build_inner_loop_tools(kernel);
    let mut llm_records = Vec::new();
    let mut last_artifact_id = None;
    let mut accumulator = CognitiveAccumulator::default();

    for _ in 0..kernel.max_decide_turns {
        let request = LlmRequest {
            backend: None,
            system_prompt: Some(system_prompt.clone()),
            messages: messages.clone(),
            max_output_tokens: kernel.max_output_tokens,
            temperature: Some(0.3),
            stop_sequences: vec![],
            stream: true,
            tools: tools.clone(),
        };

        let result = handler_direct_llm_call(handler, kernel, &request, cancellation)?;
        llm_records.push(result.llm_call_record);
        last_artifact_id = Some(result.artifact_id.clone());

        let response = result.response;
        let assistant_message = LlmMessage {
            role: LlmRole::Assistant,
            content: response.content_blocks.clone(),
        };
        let actions_before = accumulator.actions.len();
        let inline_tool_results = accumulate_lite_response(&response, &mut accumulator);
        let has_external_actions = accumulator.actions.len() > actions_before;

        messages.push(assistant_message);
        if has_external_actions {
            break;
        }
        if matches!(response.stop_reason, StopReason::ToolUse) && !inline_tool_results.is_empty() {
            messages.push(LlmMessage {
                role: LlmRole::User,
                content: inline_tool_results,
            });
            continue;
        }
        break;
    }

    let mut llm_call_record = LlmCallRecord::merge(&llm_records);
    let artifact_id = last_artifact_id.ok_or_else(|| {
        ExoError::LlmInvocation("DecideLite produced no LLM response artifact".into())
    })?;
    llm_call_record.response_artifact_ref = Some(artifact_id.clone());

    Ok(DecisionResult {
        reasoning: accumulator.reasoning_parts.join("\n"),
        reply: accumulator.reply,
        actions: accumulator.actions,
        snapshot_delta: accumulator.snapshot_delta,
        memory_notes: accumulator.memory_notes,
        llm_call_record,
        response_artifact_id: artifact_id,
        watch_proposals: accumulator.watch_proposals,
        // Always false for DecideLite — the inner loop is already running, so
        // re-requesting it is meaningless. Loop continuation is controlled by
        // the run_inner_loop() caller checking actions.is_empty() instead.
        inner_loop_requested: false,
        vessel_mode_request: accumulator.vessel_mode_request,
    })
}

fn build_mode_context(kernel: &KernelContext) -> String {
    let guard = kernel.vessel_mode.lock().unwrap();
    match *guard {
        exoskeleton_core::VesselMode::Planning => {
            "You are in **Planning** mode. Only read-only tools are allowed. Propose your plan."
                .to_string()
        }
        exoskeleton_core::VesselMode::Executing => {
            "You are **Executing** an approved plan. Follow the plan tasks in order.".to_string()
        }
        exoskeleton_core::VesselMode::Normal => String::new(),
    }
}

/// Format the previous decision as an assistant message summary.
fn assistant_blocks_for_decision(decision: &DecisionResult) -> Vec<ContentBlock> {
    let mut blocks = Vec::new();
    if !decision.reasoning.trim().is_empty() {
        blocks.push(ContentBlock::Text {
            text: decision.reasoning.clone(),
        });
    }
    for action in &decision.actions {
        blocks.push(ContentBlock::ToolUse {
            id: action.call_id.clone(),
            name: action.tool_name.clone(),
            input: action.params.clone(),
        });
    }
    if blocks.is_empty() {
        blocks.push(ContentBlock::Text {
            text: "Continuing.".into(),
        });
    }
    blocks
}

fn assistant_blocks_for_action(action: &PlannedAction) -> Vec<ContentBlock> {
    let mut blocks = Vec::new();
    if !action.rationale.trim().is_empty() {
        blocks.push(ContentBlock::Text {
            text: action.rationale.clone(),
        });
    }
    blocks.push(ContentBlock::ToolUse {
        id: action.call_id.clone(),
        name: action.tool_name.clone(),
        input: action.params.clone(),
    });
    blocks
}

fn user_blocks_for_tool_result(
    tool_result: ContentBlock,
    correction_message: Option<&str>,
) -> Vec<ContentBlock> {
    let mut blocks = Vec::new();
    if let Some(correction) = correction_message {
        blocks.push(ContentBlock::Text {
            text: correction.to_string(),
        });
    }
    blocks.push(tool_result);
    blocks
}

fn build_doom_loop_correction(tool: &str, count: u32, last_error: Option<&str>) -> String {
    let error_context = match last_error {
        Some(error) => format!("Previous attempts failed because: {error}."),
        None => "Previous attempts are repeating without progress.".into(),
    };
    format!(
        "You have attempted {tool} with the same arguments {count} times. {error_context} Try a different approach: read the file again, change the search pattern, or break the edit into smaller pieces."
    )
}

fn accumulate_lite_response(
    response: &exoskeleton_core::llm::LlmResponse,
    accumulator: &mut CognitiveAccumulator,
) -> Vec<ContentBlock> {
    let mut inline_tool_results = Vec::new();
    let response_text = response.text();
    if !response_text.trim().is_empty() {
        accumulator.reasoning_parts.push(response_text);
    }

    for block in &response.content_blocks {
        if let ContentBlock::ToolUse { id, name, input } = block {
            if is_cognitive_tool(name) {
                match process_cognitive_tool(name, input) {
                    Ok(result) => {
                        accumulate_cognitive_outcome(accumulator, result);
                        inline_tool_results.push(ContentBlock::ToolResult {
                            tool_use_id: id.clone(),
                            content: "ok".into(),
                            is_error: false,
                        });
                    }
                    Err(error) => inline_tool_results.push(ContentBlock::ToolResult {
                        tool_use_id: id.clone(),
                        content: error.to_string(),
                        is_error: true,
                    }),
                }
            } else {
                accumulator.actions.push(PlannedAction {
                    call_id: id.clone(),
                    tool_name: name.clone(),
                    params: input.clone(),
                    rationale: accumulator
                        .reasoning_parts
                        .last()
                        .cloned()
                        .unwrap_or_else(|| format!("model selected {name}")),
                    plan_task_id: None,
                });
            }
        }
    }
    inline_tool_results
}

/// Broadcast an inner-loop event via the event channel.
#[allow(clippy::too_many_arguments)]
fn broadcast_inner_loop_event(
    kernel: &KernelContext,
    tick_number: u64,
    event_type: EventType,
    summary: String,
    detail: Option<InnerLoopStepDetail>,
    diff_summary: Option<DiffSummary>,
) {
    let _ = kernel.event_tx.send(LiveEvent {
        event_type,
        summary,
        inner_loop_detail: detail,
        diff_summary,
        ..LiveEvent::new(Some(tick_number))
    });
}

/// Broadcast InnerLoopCompleted event.
#[allow(clippy::too_many_arguments)]
fn broadcast_inner_loop_completed(
    kernel: &KernelContext,
    tick_number: u64,
    steps: u32,
    session: &SessionBudget,
    reason: &SessionCompletionReason,
    diff_tracker: &DiffTracker,
) {
    broadcast_inner_loop_event(
        kernel,
        tick_number,
        EventType::InnerLoopCompleted,
        format!(
            "Inner loop completed after {} steps ({} tokens): {}",
            steps,
            session.tokens_consumed(),
            reason,
        ),
        Some(InnerLoopStepDetail {
            step_number: steps,
            max_steps: session.max_steps(),
            tool_name: None,
            tool_outcome: None,
            tokens_this_step: 0,
            tokens_total: session.tokens_consumed(),
            completion_reason: Some(reason.to_string()),
        }),
        diff_tracker.build_event_summary(),
    );
}

fn build_step_diff_summary(executions: &[ActionExecution]) -> Option<DiffSummary> {
    let files: Vec<exoskeleton_core::FileDiffEntry> = executions
        .iter()
        .filter_map(|exec| exec.code_diff.as_ref().map(|(_, content)| content))
        .map(|content| exoskeleton_core::FileDiffEntry {
            path: content.file_path.clone(),
            lines_added: content.lines_added,
            lines_removed: content.lines_removed,
            operation: content.operation,
        })
        .collect();

    if files.is_empty() {
        return None;
    }

    let lines_added = files.iter().map(|file| file.lines_added).sum();
    let lines_removed = files.iter().map(|file| file.lines_removed).sum();

    Some(DiffSummary {
        files_modified: files.len() as u32,
        lines_added,
        lines_removed,
        net_delta: lines_added - lines_removed,
        files,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use exoskeleton_core::conversation::InMemoryConversationStore;
    use exoskeleton_core::prompt::PromptRegistry;
    use exoskeleton_memory::{ApproximateTokenCounter, ContextCompiler};
    use exoskeleton_relationship::InMemoryRelationshipLedger;
    use exoskeleton_threads::{InMemoryThreadStore, ThreadRegistry};

    use super::*;
    use crate::inbox::InMemoryInbox;
    use crate::storage::StorageManager;

    fn test_kernel(dir: &std::path::Path) -> KernelContext {
        let storage = StorageManager::open(dir).unwrap();
        let counter = Arc::new(ApproximateTokenCounter);
        let compiler = ContextCompiler::with_defaults(counter, 4000);

        KernelContext {
            snapshot_store: storage.snapshot_store().clone(),
            event_ledger: storage.event_ledger().clone(),
            tick_store: storage.tick_store().clone(),
            memory_store: storage.memory_store().clone(),
            context_compiler: Arc::new(compiler),
            artifact_store: storage.artifact_store().clone(),
            wi_host_slot: Arc::new(tokio::sync::Mutex::new(None)),
            inbox: Arc::new(InMemoryInbox::new()),
            vessel_id: exoskeleton_core::VesselId::new(),
            mission: "test mission".into(),
            max_output_tokens: 4096,
            master_loop_interval_secs: 60,
            thread_registry: Arc::new(ThreadRegistry::new(Arc::new(InMemoryThreadStore::new()))),
            relationship_ledger: Arc::new(InMemoryRelationshipLedger::new()),
            conversation_store: Arc::new(InMemoryConversationStore::new()),
            budget_tracker: None,
            tool_budget_gate: None,
            metrics: None,
            event_tx: tokio::sync::broadcast::channel::<LiveEvent>(16).0,
            prompt_registry: Arc::new(PromptRegistry::with_defaults()),
            trust_decay_config: None,
            episodic_memory_capacity: None,
            bootstrap_grace_period_ticks: 0,
            max_decide_turns: 5,
            watch_store: Arc::new(exoskeleton_core::InMemoryWatchStore::new()),
            max_watches: 20,
            read_paths_this_tick: Arc::new(std::sync::Mutex::new(std::collections::HashSet::new())),
            inner_loop_config: crate::config::InnerLoopConfig::default(),
            tool_policy: crate::kernel::policy::ToolPolicyConfig::default(),
            session_approvals: crate::kernel::policy::SessionApprovals::new(),
            vessel_mode: Arc::new(std::sync::Mutex::new(exoskeleton_core::VesselMode::Normal)),
            wake_signal: None,
            observatory_url: None,
            observatory_token: None,
        }
    }

    #[test]
    fn inner_loop_system_resolves_mode_context() {
        let dir = tempfile::tempdir().unwrap();
        let kernel = test_kernel(dir.path());
        *kernel.vessel_mode.lock().unwrap() = exoskeleton_core::VesselMode::Planning;

        let resolved = kernel
            .prompt_registry
            .resolve(
                "inner-loop-system",
                &[
                    ("tools", "- code.read"),
                    ("mode_context", &build_mode_context(&kernel)),
                ],
            )
            .unwrap();

        assert!(resolved.contains("Planning"));
        assert!(resolved.contains("code.read"));
    }

    #[test]
    fn inner_loop_completed_event_has_diff_summary() {
        let dir = tempfile::tempdir().unwrap();
        let kernel = test_kernel(dir.path());
        let mut rx = kernel.event_tx.subscribe();
        let mut tracker = DiffTracker::new(3);
        tracker.record(
            exoskeleton_core::ArtifactId::from_content(b"diff-a"),
            exoskeleton_core::CodeDiffContent {
                file_path: "src/main.rs".into(),
                tool_name: "code.edit".into(),
                operation: exoskeleton_core::CodeDiffOperation::Edit,
                diff_text: "--- a/src/main.rs\n+++ b/src/main.rs\n".into(),
                lines_added: 4,
                lines_removed: 1,
                before_sha256: None,
                after_sha256: None,
            },
        );
        let session = SessionBudget::new(&crate::config::InnerLoopConfig::default());

        broadcast_inner_loop_completed(
            &kernel,
            3,
            2,
            &session,
            &SessionCompletionReason::AgentComplete,
            &tracker,
        );

        let event = rx.try_recv().unwrap();
        assert_eq!(event.event_type, EventType::InnerLoopCompleted);
        let diff_summary = event.diff_summary.expect("diff summary must be present");
        assert_eq!(diff_summary.files_modified, 1);
        assert_eq!(diff_summary.lines_added, 4);
        assert_eq!(diff_summary.lines_removed, 1);
    }

    #[test]
    fn doom_loop_correction_message_format() {
        let msg = build_doom_loop_correction("code.grep", 3, Some("pattern not found"));
        assert!(msg.contains("code.grep"));
        assert!(msg.contains("3 times"));
        assert!(msg.contains("pattern not found"));
        assert!(msg.contains("different approach"));
    }
}

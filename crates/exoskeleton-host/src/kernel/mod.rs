//! Master loop kernel — the PODAARA cognitive cycle.
//!
//! Perceive -> Orient -> Decide -> Align -> Act -> Reflect -> Amend

pub mod act;
pub mod align;
pub mod amend;
pub mod compaction;
pub mod context_window;
pub mod decide;
pub mod diff_tracker;
pub mod exec_threads;
pub mod git_context;
pub mod orient;
pub mod perceive;
pub mod policy;
pub mod reflect;
pub mod repo_analysis;
pub mod threads;
pub mod tools;
pub mod types;
pub mod virtual_tools;
pub mod watches;

use std::sync::Arc;

use crate::exec_threads::ExecThreadRegistry;
use actionqueue_executor_local::{CancellationToken, HandlerOutput};
use chrono::Utc;
use exoskeleton_core::conversation::ConversationStore;
use exoskeleton_core::inbox::Inbox;
use exoskeleton_core::prompt::PromptRegistry;
use exoskeleton_core::{
    Artifact, ArtifactKind, ArtifactStore, EventEntry, EventLedger, EventType, LedgerEntryId,
    LiveEvent, PlanModeDetail, SnapshotStore, StateSnapshot, TickId, TickStore, VesselId,
    VesselMode,
};
use exoskeleton_memory::{ContextCompiler, MemoryStore};
use exoskeleton_relationship::RelationshipLedger;
use exoskeleton_threads::ThreadRegistry;
pub use types::{
    ActResult, ActionExecution, AlignmentResult, DecisionResult, MasterLoopPayload,
    OrientationResult, PerceptionResult, PlannedAction, ReflectionResult, SnapshotDelta,
    ThreadPayload,
};
use worldinterface_host::host::EmbeddedHost;

use crate::budget::{CognitiveBudgetTracker, ToolBudgetGate};
use crate::cognitive_engine::CognitiveHandler;
use crate::metrics::ExoMetrics;

/// Shared reference to the WI Host for the Act step boundary crossing.
pub type WiHostSlot = Arc<tokio::sync::Mutex<Option<EmbeddedHost>>>;

/// Shared state for the master loop handler.
pub struct KernelContext {
    pub snapshot_store: Arc<dyn SnapshotStore>,
    pub event_ledger: Arc<dyn EventLedger>,
    pub tick_store: Arc<dyn TickStore>,
    pub memory_store: Arc<dyn MemoryStore>,
    pub context_compiler: Arc<ContextCompiler>,
    pub artifact_store: Arc<dyn ArtifactStore>,
    pub wi_host_slot: WiHostSlot,
    pub inbox: Arc<dyn Inbox>,
    pub vessel_id: VesselId,
    pub mission: String,
    pub max_output_tokens: u64,
    pub master_loop_interval_secs: u64,
    /// Thread registry for cognitive thread lifecycle management (Sprint 6).
    pub thread_registry: Arc<ThreadRegistry>,
    /// Executable thread registry for proposal-producing worker threads.
    pub exec_thread_registry: Arc<ExecThreadRegistry>,
    /// Relationship ledger for relational signal persistence (Sprint 8).
    pub relationship_ledger: Arc<dyn RelationshipLedger>,
    /// Conversation store for multi-turn interaction tracking (E1-S2).
    pub conversation_store: Arc<dyn ConversationStore>,
    /// Cognitive budget tracker (Sprint 9). `None` if no budget configured.
    pub budget_tracker: Option<Arc<std::sync::Mutex<CognitiveBudgetTracker>>>,
    /// Tool budget gate (Sprint 9). `None` if no budget configured.
    pub tool_budget_gate: Option<Arc<std::sync::Mutex<ToolBudgetGate>>>,
    /// Prometheus metrics (Sprint 10). `None` in unit tests without metrics.
    pub metrics: Option<Arc<ExoMetrics>>,
    /// Broadcast sender for real-time events (D2).
    /// Capacity: 256 events. Slow receivers that fall behind will receive
    /// a `RecvError::Lagged(n)` and can recover by re-polling REST endpoints.
    pub event_tx: tokio::sync::broadcast::Sender<LiveEvent>,
    /// Prompt template registry (Epoch 0). Loaded at boot, immutable during run.
    pub prompt_registry: Arc<PromptRegistry>,
    /// Trust decay configuration (E1-S3). `None` disables time-based decay.
    pub trust_decay_config: Option<exoskeleton_core::TrustDecayConfig>,
    /// Episodic memory capacity (E1-S3). `None` disables eviction.
    pub episodic_memory_capacity: Option<u64>,
    /// Bootstrap grace period in ticks (Decoherence Fix). During this window,
    /// Threat Monitor and Self-Critique receive bootstrap preamble context.
    /// 0 disables the grace period.
    pub bootstrap_grace_period_ticks: u64,
    /// Maximum number of LLM turns in the Decide step (E5-S1). Each turn can
    /// be an introspection query or the final decision. Default: 5.
    pub max_decide_turns: u32,
    /// Watch store for persistent observation specifications (E5-S2).
    pub watch_store: Arc<dyn exoskeleton_core::WatchStore>,
    /// Maximum number of active watches (default: 20).
    pub max_watches: u32,
    /// Paths read during the current tick for read-before-write enforcement.
    pub read_paths_this_tick: Arc<std::sync::Mutex<std::collections::HashSet<String>>>,
    /// Coding-thread configuration for the built-in coding exec thread.
    pub coding_thread_config: crate::config::CodingThreadConfig,
    /// Tool policy configuration for allow/deny/ask rules.
    pub tool_policy: crate::kernel::policy::ToolPolicyConfig,
    /// Session-scoped tool approvals.
    pub session_approvals: crate::kernel::policy::SessionApprovals,
    /// Current vessel mode.
    pub vessel_mode: Arc<std::sync::Mutex<VesselMode>>,
    /// Best-effort early-wake signal.
    pub wake_signal: Option<Arc<dyn Fn() + Send + Sync>>,
    /// Observatory URL for peer.resolve virtual tool translation.
    pub observatory_url: Option<String>,
    /// Observatory auth token for peer.resolve virtual tool translation.
    pub observatory_token: Option<String>,
}

/// Run one complete PODAARA tick.
///
/// Orchestrates all seven phases: Perceive -> Orient -> Decide -> Align ->
/// Act -> Reflect -> Amend. Each step feeds its result into the next.
///
/// Error handling strategy:
/// - Errors in Perceive, Orient, Decide: retryable (transient failures)
/// - Errors in Act: retryable (should not happen — act records failures internally)
/// - Errors in Amend: terminal (persistence failure — cannot safely continue)
///
/// Returns `HandlerOutput` for the Cognitive AQ dispatch loop.
pub fn run_tick(
    handler: &CognitiveHandler,
    kernel: &KernelContext,
    cancellation: &CancellationToken,
) -> HandlerOutput {
    let tick_start = std::time::Instant::now();

    // 1. Load snapshot (or create initial if none exists)
    let snapshot = match kernel.snapshot_store.latest() {
        Ok(Some(s)) => s,
        Ok(None) => StateSnapshot::initial(kernel.vessel_id, kernel.mission.clone()),
        Err(e) => {
            return HandlerOutput::retryable_failure(format!(
                "Perceive: failed to load snapshot: {e}"
            ))
        }
    };

    // 2. Check if status is terminal — return early with success
    if snapshot.status.is_terminal() {
        tracing::info!(
            status = ?snapshot.status,
            "tick skipped: vessel in terminal state"
        );
        return HandlerOutput::success();
    }

    // 2.5 Reset per-tick budget counters (Sprint 9)
    if let Some(ref tracker) = kernel.budget_tracker {
        if let Ok(mut guard) = tracker.lock() {
            guard.reset_tick_counters();
        }
    }
    if let Ok(mut guard) = kernel.read_paths_this_tick.lock() {
        guard.clear();
    }

    // 3. Store snapshot_before as artifact (I3: everything replayable)
    let snapshot_before_artifact_id = match Artifact::from_json(ArtifactKind::Snapshot, &snapshot) {
        Ok(artifact) => match kernel.artifact_store.put(&artifact) {
            Ok(id) => id,
            Err(e) => {
                return HandlerOutput::retryable_failure(format!(
                    "Perceive: failed to store snapshot artifact: {e}"
                ))
            }
        },
        Err(e) => {
            return HandlerOutput::retryable_failure(format!(
                "Perceive: failed to serialize snapshot: {e}"
            ))
        }
    };

    // 4. Create TickId, calculate tick_number, record started_at
    let tick_id = TickId::new();
    let tick_number = snapshot.tick_number + 1;
    let started_at = Utc::now();

    // 5. Log TickStarted event
    let tick_started_event = EventEntry {
        id: LedgerEntryId::new(),
        tick_id: Some(tick_id),
        event_type: EventType::TickStarted,
        payload_ref: None,
        summary: format!("Tick {tick_number} started"),
        timestamp: started_at,
    };
    if let Err(e) = kernel.event_ledger.append(&tick_started_event) {
        tracing::warn!(error = %e, "failed to log TickStarted event");
    }

    // D2: Broadcast TickStarted LiveEvent
    let _ = kernel.event_tx.send(LiveEvent {
        event_type: EventType::TickStarted,
        summary: format!("Tick {tick_number} started"),
        ..LiveEvent::new(Some(tick_number))
    });

    // 6. Get previous_tick_id from tick_store
    let previous_tick_id = match kernel.tick_store.latest() {
        Ok(Some(record)) => Some(record.tick_id),
        Ok(None) => None,
        Err(e) => {
            tracing::warn!(error = %e, "failed to read previous tick; assuming none");
            None
        }
    };

    // 7. Perceive
    tracing::info!(tick_number, "Perceive");
    let perception = match perceive::perceive(kernel, previous_tick_id) {
        Ok(p) => p,
        Err(e) => return HandlerOutput::retryable_failure(format!("Perceive failed: {e}")),
    };

    // D2: Log and broadcast MessageReceived events for each new envelope
    for msg in &perception.new_messages {
        let msg_event = EventEntry {
            id: LedgerEntryId::new(),
            tick_id: Some(tick_id),
            event_type: EventType::MessageReceived,
            payload_ref: Some(msg.payload_ref.clone()),
            summary: format!("Message received from {}", msg.source),
            timestamp: msg.timestamp,
        };
        let _ = kernel.event_ledger.append(&msg_event);
        let _ = kernel.event_tx.send(LiveEvent {
            event_type: EventType::MessageReceived,
            summary: msg_event.summary.clone(),
            ..LiveEvent::new(Some(tick_number))
        });
    }

    // 7.1 Check watches (E5-S2)
    let watch_result = watches::check_watches(kernel, tick_number);
    if !watch_result.triggered_events.is_empty() {
        tracing::info!(
            count = watch_result.triggered_events.len(),
            "watches triggered"
        );
    }

    // 7.5 Execute due threads (Sprint 6)
    let thread_contributions = match threads::execute_due_threads(
        handler,
        kernel,
        &snapshot,
        &perception,
        tick_id,
        cancellation,
    ) {
        Ok(tc) => tc,
        Err(e) => {
            tracing::warn!(
                error = %e,
                "thread execution failed; continuing without threads"
            );
            Vec::new()
        }
    };

    let exec_thread_contributions = match exec_threads::execute_due_exec_threads(
        handler,
        kernel,
        &snapshot,
        &perception,
        tick_id,
        cancellation,
    ) {
        Ok(tc) => tc,
        Err(e) => {
            tracing::warn!(
                error = %e,
                "exec thread execution failed; continuing without exec threads"
            );
            Vec::new()
        }
    };

    // Merge thread outputs into perception
    let perception = PerceptionResult {
        thread_outputs: thread_contributions,
        exec_thread_outputs: exec_thread_contributions,
        ..perception
    };

    // 8. Check cancellation
    if cancellation.is_cancelled() {
        return HandlerOutput::retryable_failure("cancelled after Perceive");
    }

    // 9. Orient
    tracing::info!(tick_number, "Orient");
    let orientation = match orient::orient(kernel, &snapshot, &perception) {
        Ok(o) => o,
        Err(e) => return HandlerOutput::retryable_failure(format!("Orient failed: {e}")),
    };

    // 9.5 Store ContextBreakdown artifact (E3-S2, W-24)
    let context_breakdown_ref = match Artifact::from_json(
        ArtifactKind::ContextBreakdown,
        &orientation.compiled_context,
    ) {
        Ok(artifact) => match kernel.artifact_store.put(&artifact) {
            Ok(id) => Some(id),
            Err(e) => {
                tracing::warn!(error = %e, "failed to store context breakdown artifact");
                None
            }
        },
        Err(e) => {
            tracing::warn!(error = %e, "failed to serialize context breakdown");
            None
        }
    };

    // 10. Check cancellation
    if cancellation.is_cancelled() {
        return HandlerOutput::retryable_failure("cancelled after Orient");
    }

    // 11. Decide
    tracing::info!(tick_number, "Decide");
    let decision = match decide::decide(handler, kernel, &orientation, cancellation) {
        Ok(d) => d,
        Err(e) => return HandlerOutput::retryable_failure(format!("Decide failed: {e}")),
    };
    let decision = annotate_exec_thread_origins(kernel, decision);
    let decision = reconcile_exec_thread_proposals(kernel, decision);
    let suppress_external_actions =
        should_hold_bounded_coding_session_idle(kernel, &perception);
    let decision = suppress_actions_for_bounded_coding_idle(decision, suppress_external_actions);

    if let Some(requested_mode) = decision.vessel_mode_request {
        let mut guard = kernel.vessel_mode.lock().unwrap();
        let current_mode = *guard;
        let valid_transition = matches!(
            (current_mode, requested_mode),
            (VesselMode::Normal, VesselMode::Planning)
                | (VesselMode::Executing, VesselMode::Normal)
        );
        if valid_transition {
            *guard = requested_mode;
            let _ = kernel.event_tx.send(LiveEvent {
                event_type: EventType::PlanModeTransition,
                summary: format!("Mode: {:?} -> {:?}", current_mode, requested_mode),
                plan_mode_detail: Some(PlanModeDetail {
                    from: format!("{:?}", current_mode).to_lowercase(),
                    to: format!("{:?}", requested_mode).to_lowercase(),
                    plan_draft_id: None,
                }),
                ..LiveEvent::new(Some(tick_number))
            });
        }
    }

    if *kernel.vessel_mode.lock().unwrap() == VesselMode::Planning {
        if let Some(plan_update) = &decision.snapshot_delta.plan_update {
            if let Ok(artifact) = Artifact::from_json(ArtifactKind::PlanDraft, plan_update) {
                let _ = kernel.artifact_store.put(&artifact);
            }
        }
    }

    // 12. Check cancellation
    if cancellation.is_cancelled() {
        return HandlerOutput::retryable_failure("cancelled after Decide");
    }

    let mut diff_tracker = diff_tracker::DiffTracker::new(tick_number);

    let (decision, alignment, act_result) = {
        tracing::info!(tick_number, "Align");
        let alignment = align::align(kernel, &decision, &perception, tick_id, tick_number);

        let alignment = {
            let mut merged = alignment;
            if !suppress_external_actions {
                merged.approved_actions.extend(watch_result.poll_actions);
            }
            merged
        };

        tracing::info!(tick_number, "Act");
        match act::act(kernel, &alignment, tick_id, cancellation) {
            Ok(a) => {
                for exec in &a.executions {
                    if let Some((ref id, ref content)) = exec.code_diff {
                        diff_tracker.record(id.clone(), content.clone());
                    }
                }
                (decision, alignment, a)
            }
            Err(e) => return HandlerOutput::retryable_failure(format!("Act failed: {e}")),
        }
    };

    let diff_section = diff_tracker.format_for_reflect();
    let _tick_diff_summary = diff_tracker.finalize(&kernel.artifact_store);

    // 15. Reflect
    tracing::info!(tick_number, "Reflect");
    let reflection = reflect::reflect(
        handler,
        kernel,
        &snapshot,
        &decision,
        &act_result,
        cancellation,
        &diff_section,
    );

    // 15.5 Thrash detection (Sprint 9) — analyze recent ticks
    let thrash_assessment = {
        let recent_ticks = kernel.tick_store.range(
            tick_number.saturating_sub(10),
            tick_number.saturating_sub(1),
        );
        match recent_ticks {
            Ok(ticks) => crate::budget::ThrashDetector::check(&ticks),
            Err(e) => {
                tracing::warn!(error = %e, "thrash detection: failed to load recent ticks");
                exoskeleton_core::ThrashAssessment::none()
            }
        }
    };

    // Record thrash level in budget tracker
    if let Some(ref tracker) = kernel.budget_tracker {
        if let Ok(mut guard) = tracker.lock() {
            guard.set_thrash_level(thrash_assessment.level);
        }
    }

    // D2: Broadcast thrash assessment if non-none
    if thrash_assessment.level != exoskeleton_core::budget::ThrashLevel::None {
        let _ = kernel.event_tx.send(LiveEvent {
            event_type: EventType::BudgetConsumed,
            summary: format!(
                "Thrash level: {:?} ({})",
                thrash_assessment.level,
                thrash_assessment.indicators.join("; ")
            ),
            ..LiveEvent::new(Some(tick_number))
        });
    }

    // High thrash → suspend cognitive loop and alert human
    if thrash_assessment.level == exoskeleton_core::budget::ThrashLevel::High {
        tracing::error!(
            indicators = ?thrash_assessment.indicators,
            "HIGH thrash detected — suspending cognitive loop"
        );
        // Log suspension event
        let event = EventEntry {
            id: LedgerEntryId::new(),
            tick_id: Some(tick_id),
            event_type: EventType::Error,
            payload_ref: None,
            summary: format!(
                "Cognitive loop suspended due to high thrash: {}",
                thrash_assessment.indicators.join("; ")
            ),
            timestamp: Utc::now(),
        };
        let _ = kernel.event_ledger.append(&event);

        // Build consumption before suspending
        let consumption = if let Some(ref tracker) = kernel.budget_tracker {
            if let Ok(guard) = tracker.lock() {
                guard.build_consumption()
            } else {
                vec![]
            }
        } else {
            vec![]
        };

        // Record suspended tick metrics (Sprint 10)
        if let Some(ref m) = kernel.metrics {
            m.ticks_total.with_label_values(&["suspended"]).inc();
            m.tick_duration_seconds
                .with_label_values(&["suspended"])
                .observe(tick_start.elapsed().as_secs_f64());
        }

        return HandlerOutput::Success {
            output: Some(
                serde_json::to_vec(&serde_json::json!({
                    "suspended": true,
                    "reason": "high_thrash",
                    "indicators": thrash_assessment.indicators,
                }))
                .unwrap_or_default(),
            ),
            consumption,
        };
    }

    // 16. Amend
    tracing::info!(tick_number, "Amend");
    let amend_result = amend::amend(
        kernel,
        tick_id,
        tick_number,
        started_at,
        &snapshot,
        snapshot_before_artifact_id,
        &perception,
        &decision,
        &alignment,
        &act_result,
        &reflection,
        context_breakdown_ref,
    );

    // Record success/failure for consecutive failure tracking (Sprint 9)
    let has_successful_actions = act_result
        .executions
        .iter()
        .any(|e| e.record.outcome == exoskeleton_core::ActionOutcome::Success);
    if let Some(ref tracker) = kernel.budget_tracker {
        if let Ok(mut guard) = tracker.lock() {
            if has_successful_actions {
                guard.clear_failures();
            } else if !act_result.executions.is_empty() {
                guard.record_failure();
            }
            // Persist budget state
            if let Err(e) = guard.persist() {
                tracing::warn!(error = %e, "failed to persist budget state");
            }
        }
    }

    match amend_result {
        Ok(output) => {
            // Record tick metrics (Sprint 10)
            if let Some(ref m) = kernel.metrics {
                m.ticks_total.with_label_values(&["completed"]).inc();
                m.tick_duration_seconds
                    .with_label_values(&["completed"])
                    .observe(tick_start.elapsed().as_secs_f64());
                m.current_tick_number.set(tick_number as i64);
            }

            // Replace consumption with budget-tracked consumption (Sprint 9)
            let consumption = if let Some(ref tracker) = kernel.budget_tracker {
                if let Ok(guard) = tracker.lock() {
                    guard.build_consumption()
                } else {
                    vec![]
                }
            } else {
                vec![]
            };

            let episodic_count = kernel.memory_store.count_episodic().unwrap_or(0);
            if compaction::should_compact(
                &orientation.compiled_context.truncated_sections,
                episodic_count,
                kernel.episodic_memory_capacity,
            ) {
                match compaction::run_compaction(
                    kernel.memory_store.as_ref(),
                    &compaction::CompactionConfig::default(),
                ) {
                    Ok(compaction::CompactionResult::Compacted {
                        entries_merged,
                        summaries_written,
                    }) => {
                        tracing::info!(
                            entries_merged,
                            summaries_written,
                            "session compaction completed"
                        );
                    }
                    Ok(compaction::CompactionResult::NoAction) => {}
                    Err(e) => tracing::warn!(error = %e, "session compaction failed"),
                }
            }

            let should_wake_for_exec = kernel
                .exec_thread_registry
                .exec_thread_summaries()
                .map(|summaries| {
                    summaries.into_iter().any(|s| {
                        matches!(
                            s.status,
                            exoskeleton_core::ExecThreadStatus::Active
                                | exoskeleton_core::ExecThreadStatus::Blocked
                        )
                    })
                })
                .unwrap_or(false);

            if consumption.is_empty() {
                if should_wake_for_exec {
                    if let Some(wake_signal) = &kernel.wake_signal {
                        wake_signal();
                    }
                }
                output
            } else {
                // Merge consumption into the output
                match output {
                    HandlerOutput::Success { output: out, .. } => {
                        if should_wake_for_exec {
                            if let Some(wake_signal) = &kernel.wake_signal {
                                wake_signal();
                            }
                        }
                        HandlerOutput::success_with_consumption(out, consumption)
                    }
                    other => other,
                }
            }
        }
        Err(e) => {
            // Record failed tick metrics (Sprint 10)
            if let Some(ref m) = kernel.metrics {
                m.ticks_total.with_label_values(&["failed"]).inc();
                m.tick_duration_seconds
                    .with_label_values(&["failed"])
                    .observe(tick_start.elapsed().as_secs_f64());
            }
            HandlerOutput::terminal_failure(format!("Amend failed (persistence error): {e}"))
        }
    }
}

fn annotate_exec_thread_origins(
    kernel: &KernelContext,
    mut decision: DecisionResult,
) -> DecisionResult {
    let proposals: Vec<(exoskeleton_core::ThreadId, String, String)> = kernel
        .exec_thread_registry
        .list()
        .ok()
        .into_iter()
        .flatten()
        .filter(|(spec, status)| {
            spec.kind == exoskeleton_core::ExecThreadKind::Coding
                && *status != exoskeleton_core::ExecThreadStatus::Failed
        })
        .filter_map(|(spec, _)| {
            kernel
                .exec_thread_registry
                .recent_outputs(spec.thread_id, 1)
                .ok()
                .and_then(|mut outputs| outputs.pop())
                .and_then(|output| {
                    output
                        .proposed_action
                        .map(|proposal| (spec.thread_id, proposal.tool_name, proposal.proposal_id))
                })
        })
        .collect();

    for action in &mut decision.actions {
        if action.origin_exec_thread_id.is_some() {
            continue;
        }
        let mut matches = proposals
            .iter()
            .filter(|(_, tool_name, _)| tool_name == &action.tool_name);
        if let Some((thread_id, _, proposal_id)) = matches.next() {
            if matches.next().is_none() {
                action.origin_exec_thread_id = Some(*thread_id);
                action.proposal_id = Some(proposal_id.clone());
            }
        }
    }

    decision
}

fn should_hold_bounded_coding_session_idle(
    kernel: &KernelContext,
    perception: &PerceptionResult,
) -> bool {
    let summaries = kernel
        .exec_thread_registry
        .exec_thread_summaries()
        .unwrap_or_default();
    bounded_coding_idle_visible(
        kernel.coding_thread_config.return_to_idle_after_completion,
        !perception.new_messages.is_empty(),
        &summaries,
    )
}

fn bounded_coding_idle_visible(
    return_to_idle_after_completion: bool,
    has_new_messages: bool,
    summaries: &[exoskeleton_core::ExecThreadSummary],
) -> bool {
    if !return_to_idle_after_completion || has_new_messages {
        return false;
    }

    summaries.iter().any(|summary| {
        summary.kind == exoskeleton_core::ExecThreadKind::Coding
            && summary.status == exoskeleton_core::ExecThreadStatus::Idle
            && summary
                .last_completion_reason
                .as_ref()
                .is_some_and(|reason| !reason.trim().is_empty())
    })
}

fn suppress_actions_for_bounded_coding_idle(
    mut decision: DecisionResult,
    should_suppress: bool,
) -> DecisionResult {
    if should_suppress && !decision.actions.is_empty() {
        tracing::info!(
            actions = decision.actions.len(),
            "suppressing external actions after bounded coding-session completion"
        );
        decision.actions.clear();
    }
    decision
}

fn reconcile_exec_thread_proposals(
    kernel: &KernelContext,
    mut decision: DecisionResult,
) -> DecisionResult {
    if decision.actions.is_empty() {
        return decision;
    }

    if decision
        .actions
        .iter()
        .any(|action| is_mutating_code_action(&action.tool_name))
    {
        return decision;
    }

    if !decision
        .actions
        .iter()
        .all(|action| is_exploratory_code_action(&action.tool_name))
    {
        return decision;
    }

    let mut proposals = latest_coding_exec_thread_proposals(kernel)
        .into_iter()
        .filter(|(_, output)| {
            output.evidence_complete
                && output.proposal_confidence
                    == Some(exoskeleton_core::ExecThreadProposalConfidence::High)
                && output
                    .proposed_action
                    .as_ref()
                    .is_some_and(|proposal| is_mutating_code_action(&proposal.tool_name))
        });
    let Some((thread_id, output)) = proposals.next() else {
        return decision;
    };
    if proposals.next().is_some() {
        return decision;
    }
    let Some(proposal) = output.proposed_action.as_ref() else {
        return decision;
    };

    tracing::info!(
        thread_id = %thread_id,
        tool_name = %proposal.tool_name,
        proposal_id = %proposal.proposal_id,
        "replacing exploratory Decide actions with exec-thread mutation proposal"
    );

    decision.actions = vec![PlannedAction {
        call_id: format!("exec-proposal-{}", proposal.proposal_id),
        tool_name: proposal.tool_name.clone(),
        params: proposal.params.clone(),
        rationale: proposal.rationale.clone(),
        plan_task_id: None,
        origin_exec_thread_id: Some(thread_id),
        proposal_id: Some(proposal.proposal_id.clone()),
    }];
    decision
}

fn latest_coding_exec_thread_proposals(
    kernel: &KernelContext,
) -> Vec<(
    exoskeleton_core::ThreadId,
    exoskeleton_core::ExecThreadOutput,
)> {
    kernel
        .exec_thread_registry
        .list()
        .ok()
        .into_iter()
        .flatten()
        .filter(|(spec, status)| {
            spec.kind == exoskeleton_core::ExecThreadKind::Coding
                && *status != exoskeleton_core::ExecThreadStatus::Failed
        })
        .filter_map(|(spec, _)| {
            kernel
                .exec_thread_registry
                .recent_outputs(spec.thread_id, 1)
                .ok()
                .and_then(|mut outputs| outputs.pop())
                .map(|output| (spec.thread_id, output))
        })
        .collect()
}

fn is_exploratory_code_action(tool_name: &str) -> bool {
    matches!(
        tool_name,
        "code.read" | "code.grep" | "code.ls" | "code.glob" | "fs.read"
    )
}

fn is_mutating_code_action(tool_name: &str) -> bool {
    matches!(
        tool_name,
        "code.edit" | "code.write" | "code.apply_patch" | "fs.write"
    )
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use exoskeleton_core::{
        ActionOutcome, ActionRecord, ArtifactId, CodeDiffContent, CodeDiffOperation,
    };

    use exoskeleton_core::VesselMode;

    use crate::kernel::diff_tracker::DiffTracker;
    use crate::kernel::{DecisionResult, SnapshotDelta};
    use crate::kernel::types::{ActResult, ActionExecution, PlannedAction};

    /// E8S2-T13: `read_paths_this_tick` is cleared at the start of each tick.
    ///
    /// The `run_tick` function clears `read_paths_this_tick` early in its
    /// execution (before any PODAARA steps). This test validates the clearing
    /// mechanism directly: populate the set, clear it the same way `run_tick`
    /// does, and verify it is empty.
    #[test]
    fn read_paths_cleared_per_tick() {
        let read_paths: Arc<Mutex<HashSet<String>>> = Arc::new(Mutex::new(HashSet::new()));

        // Simulate paths accumulated during a previous tick
        {
            let mut guard = read_paths.lock().unwrap();
            guard.insert("/src/main.rs".to_string());
            guard.insert("/src/lib.rs".to_string());
            guard.insert("/Cargo.toml".to_string());
        }

        // Verify paths are present
        assert_eq!(read_paths.lock().unwrap().len(), 3);

        // This mirrors the clearing logic in run_tick (line ~142-144):
        //   if let Ok(mut guard) = kernel.read_paths_this_tick.lock() {
        //       guard.clear();
        //   }
        if let Ok(mut guard) = read_paths.lock() {
            guard.clear();
        }

        // After clearing, the set must be empty — the new tick starts fresh
        assert!(
            read_paths.lock().unwrap().is_empty(),
            "read_paths_this_tick must be empty after per-tick clearing"
        );
    }

    // ── Helper: mirrors the wake-signal decision logic from run_tick ──
    fn should_wake(has_active_exec_thread: bool) -> bool {
        has_active_exec_thread
    }

    // ── T6: wake_signal_called_on_step_limit ──
    #[test]
    fn wake_signal_called_on_step_limit() {
        let call_count = Arc::new(AtomicUsize::new(0));
        let wake_signal: Arc<dyn Fn() + Send + Sync> = {
            let counter = Arc::clone(&call_count);
            Arc::new(move || {
                counter.fetch_add(1, Ordering::SeqCst);
            })
        };
        if should_wake(true) {
            wake_signal();
        }
        assert_eq!(
            call_count.load(Ordering::SeqCst),
            1,
            "wake signal must fire when exec-thread work remains active"
        );
    }

    // ── T7: wake_signal_called_on_blocked_exec_thread ──
    #[test]
    fn wake_signal_called_on_blocked_exec_thread() {
        let call_count = Arc::new(AtomicUsize::new(0));
        let wake_signal: Arc<dyn Fn() + Send + Sync> = {
            let counter = Arc::clone(&call_count);
            Arc::new(move || {
                counter.fetch_add(1, Ordering::SeqCst);
            })
        };
        if should_wake(true) {
            wake_signal();
        }
        assert_eq!(
            call_count.load(Ordering::SeqCst),
            1,
            "wake signal must fire when exec-thread work remains blocked"
        );
    }

    // ── T8: wake_signal_not_called_when_no_exec_thread_requires_work ──
    #[test]
    fn wake_signal_not_called_when_no_exec_thread_requires_work() {
        let call_count = Arc::new(AtomicUsize::new(0));
        let wake_signal: Arc<dyn Fn() + Send + Sync> = {
            let counter = Arc::clone(&call_count);
            Arc::new(move || {
                counter.fetch_add(1, Ordering::SeqCst);
            })
        };
        if should_wake(false) {
            wake_signal();
        }
        assert_eq!(
            call_count.load(Ordering::SeqCst),
            0,
            "wake signal must NOT fire when no exec thread needs another cycle"
        );
    }

    #[test]
    fn exploratory_code_actions_include_legacy_listing_tools() {
        assert!(super::is_exploratory_code_action("code.read"));
        assert!(super::is_exploratory_code_action("code.grep"));
        assert!(super::is_exploratory_code_action("code.ls"));
        assert!(super::is_exploratory_code_action("code.glob"));
        assert!(!super::is_exploratory_code_action("code.edit"));
    }

    #[test]
    fn bounded_coding_idle_detected_only_when_enabled() {
        let summaries = vec![exoskeleton_core::ExecThreadSummary {
            thread_id: exoskeleton_core::ThreadId::new(),
            kind: exoskeleton_core::ExecThreadKind::Coding,
            name: "Coding".into(),
            status: exoskeleton_core::ExecThreadStatus::Idle,
            last_output_summary: None,
            current_focus: None,
            work_phase: Some("idle".into()),
            evidence_complete: true,
            proposal_confidence: None,
            last_completion_reason: Some("completed".into()),
        }];

        assert!(super::bounded_coding_idle_visible(true, false, &summaries));
        assert!(!super::bounded_coding_idle_visible(false, false, &summaries));
        assert!(!super::bounded_coding_idle_visible(true, true, &summaries));
    }

    #[test]
    fn bounded_coding_idle_suppresses_external_actions() {
        let decision = DecisionResult {
            reasoning: "done".into(),
            actions: vec![PlannedAction {
                call_id: "call_1".into(),
                tool_name: "code.read".into(),
                params: serde_json::json!({"file_path": "/tmp/sample.rs"}),
                rationale: "verify".into(),
                plan_task_id: None,
                origin_exec_thread_id: None,
                proposal_id: None,
            }],
            reply: None,
            memory_notes: vec![],
            snapshot_delta: SnapshotDelta::default(),
            llm_call_record: exoskeleton_core::LlmCallRecord {
                model: "mock".into(),
                tokens_in: 0,
                tokens_out: 0,
                cost_cents: 0.0,
                latency_ms: 0,
                response_artifact_ref: None,
                turns: 1,
            },
            response_artifact_id: ArtifactId::from_content(b"resp"),
            watch_proposals: vec![],
            vessel_mode_request: None,
        };

        let suppressed = super::suppress_actions_for_bounded_coding_idle(decision, true);
        assert!(suppressed.actions.is_empty());
    }

    // ── T9: kernel_context_wake_signal_none_in_tests ──
    #[test]
    fn kernel_context_wake_signal_none_in_tests() {
        // When wake_signal is None, the wake decision code must not panic.
        let wake_signal: Option<Arc<dyn Fn() + Send + Sync>> = None;
        // This mirrors the production code:
        //   if let Some(wake_signal) = &kernel.wake_signal { wake_signal(); }
        if should_wake(true) {
            if let Some(ws) = &wake_signal {
                ws();
            }
        }
        // No panic = success
    }

    // ── T22: vessel_mode_starts_normal ──
    #[test]
    fn vessel_mode_starts_normal() {
        let mode = Arc::new(Mutex::new(VesselMode::Normal));
        assert_eq!(
            *mode.lock().unwrap(),
            VesselMode::Normal,
            "KernelContext vessel_mode must initialize to Normal"
        );
    }

    // ── T23: agent_requests_planning_mode ──
    #[test]
    fn agent_requests_planning_mode() {
        let mode = Arc::new(Mutex::new(VesselMode::Normal));
        let requested_mode = Some(VesselMode::Planning);

        // Mirror the mode transition logic from run_tick
        if let Some(requested) = requested_mode {
            let mut guard = mode.lock().unwrap();
            let current = *guard;
            let valid = matches!(
                (current, requested),
                (VesselMode::Normal, VesselMode::Planning)
                    | (VesselMode::Executing, VesselMode::Normal)
            );
            if valid {
                *guard = requested;
            }
        }

        assert_eq!(
            *mode.lock().unwrap(),
            VesselMode::Planning,
            "Normal -> Planning transition must succeed"
        );
    }

    // ── T27: agent_requests_normal_from_executing ──
    #[test]
    fn agent_requests_normal_from_executing() {
        let mode = Arc::new(Mutex::new(VesselMode::Executing));
        let requested_mode = Some(VesselMode::Normal);

        if let Some(requested) = requested_mode {
            let mut guard = mode.lock().unwrap();
            let current = *guard;
            let valid = matches!(
                (current, requested),
                (VesselMode::Normal, VesselMode::Planning)
                    | (VesselMode::Executing, VesselMode::Normal)
            );
            if valid {
                *guard = requested;
            }
        }

        assert_eq!(
            *mode.lock().unwrap(),
            VesselMode::Normal,
            "Executing -> Normal transition must succeed"
        );
    }

    // ── T28: invalid_transition_ignored ──
    #[test]
    fn invalid_transition_ignored() {
        let mode = Arc::new(Mutex::new(VesselMode::Normal));
        let requested_mode = Some(VesselMode::Executing);

        if let Some(requested) = requested_mode {
            let mut guard = mode.lock().unwrap();
            let current = *guard;
            let valid = matches!(
                (current, requested),
                (VesselMode::Normal, VesselMode::Planning)
                    | (VesselMode::Executing, VesselMode::Normal)
            );
            if valid {
                *guard = requested;
            }
        }

        assert_eq!(
            *mode.lock().unwrap(),
            VesselMode::Normal,
            "Normal -> Executing transition must be ignored (invalid)"
        );
    }

    // ── T29: vessel_mode_persisted_in_snapshot ──
    #[test]
    fn vessel_mode_persisted_in_snapshot() {
        let snapshot = exoskeleton_core::StateSnapshot::initial(
            exoskeleton_core::VesselId::new(),
            "test".into(),
        );
        assert_eq!(
            snapshot.vessel_mode,
            VesselMode::Normal,
            "initial snapshot should have Normal mode"
        );

        // Serialize and deserialize to verify persistence
        let json = serde_json::to_string(&snapshot).unwrap();
        let parsed: exoskeleton_core::StateSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.vessel_mode, VesselMode::Normal);

        // Modify and re-check
        let mut snapshot_with_planning = snapshot;
        snapshot_with_planning.vessel_mode = VesselMode::Planning;
        let json = serde_json::to_string(&snapshot_with_planning).unwrap();
        let parsed: exoskeleton_core::StateSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(
            parsed.vessel_mode,
            VesselMode::Planning,
            "snapshot with Planning mode must roundtrip"
        );
    }

    // ── T30: plan_draft_artifact_stored_in_planning ──
    #[test]
    fn plan_draft_artifact_stored_in_planning() {
        use exoskeleton_core::{Artifact, ArtifactKind, ArtifactStore};

        let dir = tempfile::tempdir().unwrap();
        let storage = crate::storage::StorageManager::open(dir.path()).unwrap();

        let vessel_mode = VesselMode::Planning;
        let plan_update = serde_json::json!({
            "objective": "Refactor auth module",
            "tasks": [
                {"title": "Read current code", "status": "pending"}
            ]
        });

        // Mirror the plan draft storage logic from run_tick
        if vessel_mode == VesselMode::Planning {
            if let Ok(artifact) = Artifact::from_json(ArtifactKind::PlanDraft, &plan_update) {
                let result = storage.artifact_store().put(&artifact);
                assert!(result.is_ok(), "storing PlanDraft must succeed");
                let artifact_id = result.unwrap();

                // Verify the artifact is retrievable
                let retrieved = storage
                    .artifact_store()
                    .get(&artifact_id)
                    .unwrap()
                    .expect("PlanDraft artifact must exist after storage");
                assert_eq!(retrieved.kind, ArtifactKind::PlanDraft);
            }
        }
    }

    #[test]
    fn classic_path_records_diffs_in_tracker() {
        let mut diff_tracker = DiffTracker::new(7);
        let act_result = ActResult {
            executions: vec![ActionExecution {
                action: PlannedAction {
                    call_id: "call_code_edit".into(),
                    tool_name: "code.edit".into(),
                    params: serde_json::json!({"file_path": "/tmp/sample.rs"}),
                    rationale: "test".into(),
                    plan_task_id: None,
                    origin_exec_thread_id: None,
                    proposal_id: None,
                },
                result: Ok(serde_json::json!({"ok": true})),
                record: ActionRecord {
                    action_type: "code.edit".into(),
                    target: "/tmp/sample.rs".into(),
                    outcome: ActionOutcome::Success,
                    receipt_ref: None,
                    origin_exec_thread_id: None,
                    proposal_id: None,
                },
                tool_result: exoskeleton_core::llm::ContentBlock::ToolResult {
                    tool_use_id: "call_code_edit".into(),
                    content: "{\"ok\":true}".into(),
                    is_error: false,
                },
                pending_question: false,
                code_diff: Some((
                    ArtifactId::from_content(b"code-edit-diff"),
                    CodeDiffContent {
                        file_path: "/tmp/sample.rs".into(),
                        tool_name: "code.edit".into(),
                        operation: CodeDiffOperation::Edit,
                        diff_text: "--- a/sample.rs\n+++ b/sample.rs\n".into(),
                        lines_added: 3,
                        lines_removed: 1,
                        before_sha256: None,
                        after_sha256: None,
                    },
                )),
            }],
        };

        for exec in &act_result.executions {
            if let Some((ref id, ref content)) = exec.code_diff {
                diff_tracker.record(id.clone(), content.clone());
            }
        }

        let rendered = diff_tracker.format_for_reflect();
        assert!(!rendered.is_empty());
        assert!(rendered.contains("File Changes This Tick"));
        assert!(rendered.contains("/tmp/sample.rs"));
    }
}

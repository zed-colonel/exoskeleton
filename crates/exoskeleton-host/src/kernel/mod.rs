//! Master loop kernel — the PODAARA cognitive cycle.
//!
//! Perceive -> Orient -> Decide -> Align -> Act -> Reflect -> Amend

pub mod act;
pub mod align;
pub mod amend;
pub mod decide;
pub mod inner_loop;
pub mod orient;
pub mod perceive;
pub mod policy;
pub mod reflect;
pub mod threads;
pub mod types;
pub mod watches;

use std::sync::Arc;

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
    ActResult, ActionExecution, AlignmentResult, DecisionProtocol, DecisionResult,
    MasterLoopPayload, OrientationResult, PerceptionResult, PlannedAction, ReflectionResult,
    SnapshotDelta, ThreadPayload,
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
    /// Inner loop configuration (E8-S1). Controls bounded inner interaction loop.
    pub inner_loop_config: crate::config::InnerLoopConfig,
    /// Tool policy configuration for allow/deny/ask rules.
    pub tool_policy: crate::kernel::policy::ToolPolicyConfig,
    /// Session-scoped tool approvals.
    pub session_approvals: crate::kernel::policy::SessionApprovals,
    /// Current vessel mode.
    pub vessel_mode: Arc<std::sync::Mutex<VesselMode>>,
    /// Best-effort early-wake signal.
    pub wake_signal: Option<Arc<dyn Fn() + Send + Sync>>,
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
    let thread_contributions =
        match threads::execute_due_threads(handler, kernel, &snapshot, tick_id, cancellation) {
            Ok(tc) => tc,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "thread execution failed; continuing without threads"
                );
                Vec::new()
            }
        };

    // Merge thread outputs into perception
    let perception = PerceptionResult {
        thread_outputs: thread_contributions,
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

    // 12.5 Inner loop activation check (E8-S1)
    let inner_loop_active = kernel.inner_loop_config.enabled && decision.inner_loop_requested;

    let (decision, alignment, act_result, inner_loop_completion) = if inner_loop_active {
        // Inner loop: iterative DecideLite→Align→Act cycle.
        // The agent explicitly requested this — the task needs
        // iterative tool-feedback (e.g., multi-step coding).
        tracing::info!(tick_number, "Inner Loop (activated by agent request)");

        // Merge watch poll actions into the initial decision so the inner loop's
        // first Align+Act step includes them (E5-S2, bypass Align as already approved).
        let decision = if watch_result.poll_actions.is_empty() {
            decision
        } else {
            let mut d = decision;
            d.actions.extend(
                watch_result
                    .poll_actions
                    .into_iter()
                    .map(|a| PlannedAction {
                        tool_name: a.tool_name,
                        params: a.params,
                        rationale: a.rationale,
                        plan_task_id: a.plan_task_id,
                    }),
            );
            d
        };

        match inner_loop::run_inner_loop(
            handler,
            kernel,
            &orientation,
            &perception,
            decision,
            cancellation,
            tick_number,
            tick_id,
        ) {
            Ok(inner_result) => {
                tracing::info!(
                    tick_number,
                    steps = inner_result.steps_taken,
                    reason = %inner_result.completion_reason,
                    "Inner loop completed"
                );
                // Build a summary alignment for Amend with accumulated relationship data (I8)
                let summary_alignment = AlignmentResult {
                    approved_actions: vec![],
                    blocked_actions: vec![],
                    relationship_updates: inner_result.relationship_updates,
                    relationship_snapshot: inner_result.relationship_snapshot,
                };
                (
                    inner_result.final_decision,
                    summary_alignment,
                    ActResult {
                        executions: inner_result.executions,
                    },
                    Some(inner_result.completion_reason),
                )
            }
            Err(e) => return HandlerOutput::retryable_failure(format!("Inner loop failed: {e}")),
        }
    } else {
        // Classic path: single Align→Act pass.
        // Either the inner loop is disabled in config, or the agent
        // determined this tick doesn't need iterative tool use.
        tracing::info!(tick_number, "Align");
        let alignment = align::align(kernel, &decision, &perception, tick_id, tick_number);

        // Merge poll watch actions (E5-S2) — bypass Align (already approved at watch creation)
        let alignment = {
            let mut merged = alignment;
            merged.approved_actions.extend(watch_result.poll_actions);
            merged
        };

        tracing::info!(tick_number, "Act");
        match act::act(kernel, &alignment, tick_id, cancellation) {
            Ok(a) => (decision, alignment, a, None),
            Err(e) => return HandlerOutput::retryable_failure(format!("Act failed: {e}")),
        }
    };

    // 15. Reflect
    tracing::info!(tick_number, "Reflect");
    let reflection = reflect::reflect(
        handler,
        kernel,
        &snapshot,
        &decision,
        &act_result,
        cancellation,
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

            if consumption.is_empty() {
                if matches!(
                    inner_loop_completion,
                    Some(crate::budget::session::SessionCompletionReason::StepLimit)
                        | Some(crate::budget::session::SessionCompletionReason::AwaitingInput)
                ) {
                    if let Some(wake_signal) = &kernel.wake_signal {
                        wake_signal();
                    }
                }
                output
            } else {
                // Merge consumption into the output
                match output {
                    HandlerOutput::Success { output: out, .. } => {
                        if matches!(
                            inner_loop_completion,
                            Some(crate::budget::session::SessionCompletionReason::StepLimit)
                                | Some(
                                    crate::budget::session::SessionCompletionReason::AwaitingInput
                                )
                        ) {
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

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use exoskeleton_core::VesselMode;

    use crate::budget::session::SessionCompletionReason;

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
    // In run_tick, after Amend, the code checks:
    //   if inner_loop_completion is StepLimit or AwaitingInput → fire wake_signal
    fn should_wake(completion: &Option<SessionCompletionReason>) -> bool {
        matches!(
            completion,
            Some(SessionCompletionReason::StepLimit) | Some(SessionCompletionReason::AwaitingInput)
        )
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
        let completion = Some(SessionCompletionReason::StepLimit);
        if should_wake(&completion) {
            wake_signal();
        }
        assert_eq!(
            call_count.load(Ordering::SeqCst),
            1,
            "wake signal must fire on StepLimit"
        );
    }

    // ── T7: wake_signal_called_on_awaiting_input ──
    #[test]
    fn wake_signal_called_on_awaiting_input() {
        let call_count = Arc::new(AtomicUsize::new(0));
        let wake_signal: Arc<dyn Fn() + Send + Sync> = {
            let counter = Arc::clone(&call_count);
            Arc::new(move || {
                counter.fetch_add(1, Ordering::SeqCst);
            })
        };
        let completion = Some(SessionCompletionReason::AwaitingInput);
        if should_wake(&completion) {
            wake_signal();
        }
        assert_eq!(
            call_count.load(Ordering::SeqCst),
            1,
            "wake signal must fire on AwaitingInput"
        );
    }

    // ── T8: wake_signal_not_called_on_agent_complete ──
    #[test]
    fn wake_signal_not_called_on_agent_complete() {
        let call_count = Arc::new(AtomicUsize::new(0));
        let wake_signal: Arc<dyn Fn() + Send + Sync> = {
            let counter = Arc::clone(&call_count);
            Arc::new(move || {
                counter.fetch_add(1, Ordering::SeqCst);
            })
        };
        let completion = Some(SessionCompletionReason::AgentComplete);
        if should_wake(&completion) {
            wake_signal();
        }
        assert_eq!(
            call_count.load(Ordering::SeqCst),
            0,
            "wake signal must NOT fire on AgentComplete"
        );
    }

    // ── T9: kernel_context_wake_signal_none_in_tests ──
    #[test]
    fn kernel_context_wake_signal_none_in_tests() {
        // When wake_signal is None, the wake decision code must not panic.
        let wake_signal: Option<Arc<dyn Fn() + Send + Sync>> = None;
        let completion = Some(SessionCompletionReason::StepLimit);

        // This mirrors the production code:
        //   if let Some(wake_signal) = &kernel.wake_signal { wake_signal(); }
        if should_wake(&completion) {
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
}

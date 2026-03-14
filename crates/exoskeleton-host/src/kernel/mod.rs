//! Master loop kernel — the PODAARA cognitive cycle.
//!
//! Perceive -> Orient -> Decide -> Align -> Act -> Reflect -> Amend

pub mod act;
pub mod align;
pub mod amend;
pub mod decide;
pub mod orient;
pub mod perceive;
pub mod reflect;
pub mod threads;
pub mod types;

use std::sync::Arc;

use actionqueue_executor_local::{CancellationToken, HandlerOutput};
use chrono::Utc;
use exoskeleton_core::inbox::Inbox;
use exoskeleton_core::{
    Artifact, ArtifactKind, ArtifactStore, EventEntry, EventLedger, EventType, LedgerEntryId,
    LiveEvent, SnapshotStore, StateSnapshot, TickId, TickStore, VesselId,
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
    /// Cognitive budget tracker (Sprint 9). `None` if no budget configured.
    pub budget_tracker: Option<Arc<tokio::sync::Mutex<CognitiveBudgetTracker>>>,
    /// Tool budget gate (Sprint 9). `None` if no budget configured.
    pub tool_budget_gate: Option<Arc<tokio::sync::Mutex<ToolBudgetGate>>>,
    /// Prometheus metrics (Sprint 10). `None` in unit tests without metrics.
    pub metrics: Option<Arc<ExoMetrics>>,
    /// Broadcast sender for real-time events (D2).
    /// Capacity: 256 events. Slow receivers that fall behind will receive
    /// a `RecvError::Lagged(n)` and can recover by re-polling REST endpoints.
    pub event_tx: tokio::sync::broadcast::Sender<LiveEvent>,
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
        if let Ok(mut guard) = tracker.try_lock() {
            guard.reset_tick_counters();
        }
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
        tick_number: Some(tick_number),
        summary: format!("Tick {tick_number} started"),
        timestamp: started_at,
        snapshot: None,
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
            tick_number: Some(tick_number),
            summary: msg_event.summary.clone(),
            timestamp: msg.timestamp,
            snapshot: None,
        });
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

    // 12. Check cancellation
    if cancellation.is_cancelled() {
        return HandlerOutput::retryable_failure("cancelled after Decide");
    }

    // 13. Align
    tracing::info!(tick_number, "Align");
    let alignment = align::align(kernel, &decision, &perception, tick_id);

    // 14. Act
    tracing::info!(tick_number, "Act");
    let act_result = match act::act(kernel, &alignment, tick_id, cancellation) {
        Ok(a) => a,
        Err(e) => return HandlerOutput::retryable_failure(format!("Act failed: {e}")),
    };

    // 15. Reflect
    tracing::info!(tick_number, "Reflect");
    let reflection = reflect::reflect(&act_result);

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
        if let Ok(mut guard) = tracker.try_lock() {
            guard.set_thrash_level(thrash_assessment.level);
        }
    }

    // D2: Broadcast thrash assessment if non-none
    if thrash_assessment.level != exoskeleton_core::budget::ThrashLevel::None {
        let _ = kernel.event_tx.send(LiveEvent {
            event_type: EventType::BudgetConsumed,
            tick_number: Some(tick_number),
            summary: format!(
                "Thrash level: {:?} ({})",
                thrash_assessment.level,
                thrash_assessment.indicators.join("; ")
            ),
            timestamp: Utc::now(),
            snapshot: None,
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
            if let Ok(guard) = tracker.try_lock() {
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
    );

    // Record success/failure for consecutive failure tracking (Sprint 9)
    let has_successful_actions = act_result
        .executions
        .iter()
        .any(|e| e.record.outcome == exoskeleton_core::ActionOutcome::Success);
    if let Some(ref tracker) = kernel.budget_tracker {
        if let Ok(mut guard) = tracker.try_lock() {
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
                if let Ok(guard) = tracker.try_lock() {
                    guard.build_consumption()
                } else {
                    vec![]
                }
            } else {
                vec![]
            };

            if consumption.is_empty() {
                output
            } else {
                // Merge consumption into the output
                match output {
                    HandlerOutput::Success { output: out, .. } => {
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

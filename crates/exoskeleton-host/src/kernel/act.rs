//! Act step — the sole boundary crossing from Cognitive AQ to Tool AQ.
//!
//! THIS IS THE SOLE BOUNDARY CROSSING FROM COGNITIVE AQ TO TOOL AQ.
//! (I9, IBP §3.3, Charter §3.1.4)
//!
//! No other PODAARA step, cognitive thread, or LLM handler may invoke
//! WI Host methods that trigger tool execution.

use actionqueue_executor_local::CancellationToken;
use chrono::Utc;
use exoskeleton_core::tick::{ActionOutcome, ActionRecord};
use exoskeleton_core::{
    Artifact, ArtifactKind, EventEntry, EventType, ExoError, LedgerEntryId, LiveEvent, TickId,
};

use super::types::{ActResult, ActionExecution, AlignmentResult};
use super::KernelContext;

/// Execute the Act step: invoke tools via WI Host (Tool AQ).
///
/// For each approved action from the Align step, invokes the corresponding
/// connector through the WI Host's `invoke_single` API. This crosses the
/// boundary from the Cognitive AQ domain into the Tool AQ domain (I9).
///
/// Each invocation:
/// - Calls `host.invoke_single(tool_name, params)` via the Tool AQ
/// - On success: stores the result as a Receipt artifact (I3)
/// - On failure: records the failure but continues processing remaining actions
/// - Logs an `ActionExecuted` event to the Event Ledger for every action
///
/// Returns `ActResult` containing all execution outcomes.
///
/// # Errors
/// - `ExoError::Engine` if the WI Host slot is locked or unavailable
pub fn act(
    kernel: &KernelContext,
    alignment: &AlignmentResult,
    tick_id: TickId,
    cancellation: &CancellationToken,
) -> Result<ActResult, ExoError> {
    if alignment.approved_actions.is_empty() {
        return Ok(ActResult { executions: vec![] });
    }

    // Lock the WI Host slot. Use try_lock to avoid blocking in sync context.
    let guard = kernel
        .wi_host_slot
        .try_lock()
        .map_err(|_| ExoError::Engine("WI host slot locked (concurrent access)".into()))?;
    let host = guard
        .as_ref()
        .ok_or_else(|| ExoError::Engine("WI host not available".into()))?;

    let mut executions = Vec::new();

    for (i, action) in alignment.approved_actions.iter().enumerate() {
        // Tool budget gate check (Sprint 9, I6/I9)
        if let Some(ref gate) = kernel.tool_budget_gate {
            if let Ok(gate_guard) = gate.try_lock() {
                if !gate_guard.check() {
                    tracing::warn!(
                        action = %action.tool_name,
                        "action skipped: tool invocation rate limit reached"
                    );
                    let record = ActionRecord {
                        action_type: action.tool_name.clone(),
                        target: action.params.to_string(),
                        receipt_ref: None,
                        outcome: ActionOutcome::RateLimited,
                    };
                    let event = EventEntry {
                        id: LedgerEntryId::new(),
                        tick_id: Some(tick_id),
                        event_type: EventType::ActionExecuted,
                        payload_ref: None,
                        summary: format!(
                            "Action {}: rate-limited ({})",
                            action.tool_name, action.rationale,
                        ),
                        timestamp: Utc::now(),
                    };
                    let _ = kernel.event_ledger.append(&event);
                    executions.push(ActionExecution {
                        action: action.clone(),
                        result: Err("rate_limited".into()),
                        record,
                    });
                    continue;
                }
            }
        }

        // Execute via WI Host (boundary crossing to Tool AQ).
        // invoke_single is async; we use Handle::current().block_on() because
        // the Act step runs on a blocking thread in the AQ dispatch loop.
        let result = tokio::runtime::Handle::current()
            .block_on(host.invoke_single(&action.tool_name, action.params.clone()));

        let (outcome, result_value, receipt_ref) = match result {
            Ok(value) => {
                // Store receipt artifact (I3: everything replayable)
                let receipt_ref = match Artifact::from_json(ArtifactKind::Receipt, &value) {
                    Ok(artifact) => match kernel.artifact_store.put(&artifact) {
                        Ok(id) => Some(id),
                        Err(e) => {
                            tracing::warn!(error = %e, "failed to store receipt artifact");
                            None
                        }
                    },
                    Err(e) => {
                        tracing::warn!(error = %e, "failed to serialize receipt");
                        None
                    }
                };
                (ActionOutcome::Success, Ok(value), receipt_ref)
            }
            Err(e) => {
                tracing::warn!(
                    tool = %action.tool_name,
                    error = %e,
                    "tool invocation failed"
                );
                // Store error as receipt artifact so Observatory can display it
                let error_json = serde_json::json!({
                    "error": e.to_string(),
                    "tool": action.tool_name,
                    "params": action.params,
                });
                let receipt_ref = match Artifact::from_json(ArtifactKind::Receipt, &error_json)
                {
                    Ok(artifact) => match kernel.artifact_store.put(&artifact) {
                        Ok(id) => Some(id),
                        Err(store_err) => {
                            tracing::warn!(error = %store_err, "failed to store error receipt");
                            None
                        }
                    },
                    Err(_) => None,
                };
                (ActionOutcome::Failure, Err(e.to_string()), receipt_ref)
            }
        };

        let record = ActionRecord {
            action_type: action.tool_name.clone(),
            target: action.params.to_string(),
            receipt_ref,
            outcome,
        };

        // Log ActionExecuted event to the Event Ledger.
        // Include the result output so the vessel can see what its tools returned
        // in subsequent ticks (e.g., file paths from fs.write, response bodies, etc.)
        let result_summary = match &result_value {
            Ok(val) => {
                let json_str = val.to_string();
                // Truncate very large outputs to keep event summaries reasonable
                if json_str.len() > 500 {
                    format!("succeeded — {}...", &json_str[..497])
                } else {
                    format!("succeeded — {json_str}")
                }
            }
            Err(e) => format!("failed: {e}"),
        };
        let event = EventEntry {
            id: LedgerEntryId::new(),
            tick_id: Some(tick_id),
            event_type: EventType::ActionExecuted,
            payload_ref: record.receipt_ref.clone(),
            summary: format!(
                "Action {}: {} ({})",
                action.tool_name, result_summary, action.rationale,
            ),
            timestamp: Utc::now(),
        };
        if let Err(e) = kernel.event_ledger.append(&event) {
            tracing::warn!(error = %e, "failed to log ActionExecuted event");
        }

        // D2: Broadcast ActionExecuted LiveEvent
        let _ = kernel.event_tx.send(LiveEvent {
            event_type: EventType::ActionExecuted,
            tick_number: None,
            summary: event.summary.clone(),
            timestamp: event.timestamp,
            snapshot: None,
        });

        // Record tool invocation in budget gate (Sprint 9)
        if let Some(ref gate) = kernel.tool_budget_gate {
            if let Ok(mut gate_guard) = gate.try_lock() {
                gate_guard.record_invocation();
            }
        }

        // Record action metrics (Sprint 10)
        if let Some(ref m) = kernel.metrics {
            let outcome_label = match outcome {
                ActionOutcome::Success => "success",
                ActionOutcome::Failure => "failure",
                ActionOutcome::RateLimited => "rate_limited",
                ActionOutcome::Skipped => "skipped",
                ActionOutcome::Timeout => "timeout",
            };
            m.actions_total
                .with_label_values(&[&action.tool_name, outcome_label])
                .inc();
        }

        executions.push(ActionExecution {
            action: action.clone(),
            result: result_value,
            record,
        });

        // Cancellation check between actions (I9: governable execution).
        // After each action completes, check if we've been cancelled.
        // If so, record remaining actions as Skipped and stop.
        if cancellation.is_cancelled() {
            for remaining in &alignment.approved_actions[i + 1..] {
                let record = ActionRecord {
                    action_type: remaining.tool_name.clone(),
                    target: remaining.params.to_string(),
                    receipt_ref: None,
                    outcome: ActionOutcome::Skipped,
                };

                let event = EventEntry {
                    id: LedgerEntryId::new(),
                    tick_id: Some(tick_id),
                    event_type: EventType::ActionExecuted,
                    payload_ref: None,
                    summary: format!(
                        "Action {}: skipped (cancelled) ({})",
                        remaining.tool_name, remaining.rationale,
                    ),
                    timestamp: Utc::now(),
                };
                if let Err(e) = kernel.event_ledger.append(&event) {
                    tracing::warn!(error = %e, "failed to log skipped ActionExecuted event");
                }

                executions.push(ActionExecution {
                    action: remaining.clone(),
                    result: Err("cancelled".into()),
                    record,
                });
            }
            break;
        }
    }

    Ok(ActResult { executions })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use actionqueue_executor_local::CancellationToken;
    use exoskeleton_core::conversation::InMemoryConversationStore;
    use exoskeleton_core::prompt::PromptRegistry;
    use exoskeleton_core::{ArtifactKind, EventType};
    use exoskeleton_memory::{ApproximateTokenCounter, ContextCompiler};
    use exoskeleton_relationship::InMemoryRelationshipLedger;
    use exoskeleton_threads::{InMemoryThreadStore, ThreadRegistry};
    use worldinterface_connector::connectors::DelayConnector;
    use worldinterface_connector::registry::ConnectorRegistry;
    use worldinterface_host::config::HostConfig;
    use worldinterface_host::host::EmbeddedHost;

    use super::*;
    use crate::kernel::types::PlannedAction;
    use crate::storage::StorageManager;

    // ── Test helpers ──

    /// Create a KernelContext with no WI Host (slot is None).
    fn test_kernel_no_host(dir: &std::path::Path) -> KernelContext {
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
            inbox: Arc::new(crate::inbox::InMemoryInbox::new()),
            vessel_id: exoskeleton_core::VesselId::new(),
            mission: "test".into(),
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
        }
    }

    /// Create a KernelContext with a real WI Host (delay connector registered).
    async fn test_kernel_with_host(dir: &std::path::Path) -> KernelContext {
        let storage = StorageManager::open(dir).unwrap();

        // Boot a real WI Host with the delay connector
        let registry = ConnectorRegistry::new();
        registry.register(Arc::new(DelayConnector));
        let host_config = HostConfig {
            aq_data_dir: dir.join("wi").join("aq"),
            context_store_path: dir.join("wi").join("context.db"),
            tick_interval: Duration::from_millis(10),
            ..Default::default()
        };
        std::fs::create_dir_all(dir.join("wi").join("aq")).unwrap();
        let host = EmbeddedHost::start(host_config, registry, None)
            .await
            .unwrap();

        let wi_host_slot = Arc::new(tokio::sync::Mutex::new(Some(host)));
        let counter = Arc::new(ApproximateTokenCounter);
        let compiler = ContextCompiler::with_defaults(counter, 4000);

        KernelContext {
            snapshot_store: storage.snapshot_store().clone(),
            event_ledger: storage.event_ledger().clone(),
            tick_store: storage.tick_store().clone(),
            memory_store: storage.memory_store().clone(),
            context_compiler: Arc::new(compiler),
            artifact_store: storage.artifact_store().clone(),
            wi_host_slot,
            inbox: Arc::new(crate::inbox::InMemoryInbox::new()),
            vessel_id: exoskeleton_core::VesselId::new(),
            mission: "test".into(),
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
        }
    }

    /// Shut down the WI Host from the kernel's slot to avoid resource leaks.
    async fn shutdown_host(kernel: KernelContext) {
        let mut guard = kernel.wi_host_slot.lock().await;
        if let Some(host) = guard.take() {
            host.shutdown().await.ok();
        }
    }

    fn delay_action(ms: u64) -> PlannedAction {
        PlannedAction {
            tool_name: "delay".into(),
            params: serde_json::json!({"duration_ms": ms}),
            rationale: "test delay".into(),
            plan_task_id: None,
        }
    }

    fn unknown_action() -> PlannedAction {
        PlannedAction {
            tool_name: "nonexistent.tool".into(),
            params: serde_json::json!({"key": "value"}),
            rationale: "test unknown tool".into(),
            plan_task_id: None,
        }
    }

    fn empty_alignment() -> AlignmentResult {
        AlignmentResult {
            approved_actions: vec![],
            blocked_actions: vec![],
            relationship_updates: vec![],
            relationship_snapshot: None,
        }
    }

    fn alignment_with(actions: Vec<PlannedAction>) -> AlignmentResult {
        AlignmentResult {
            approved_actions: actions,
            blocked_actions: vec![],
            relationship_updates: vec![],
            relationship_snapshot: None,
        }
    }

    // ── T-7 Tests: Act Step ──

    #[test]
    fn act_with_no_actions() {
        // Empty approved actions should return early with empty ActResult.
        // No WI Host needed since it returns before accessing the slot.
        let dir = tempfile::tempdir().unwrap();
        let kernel = test_kernel_no_host(dir.path());
        let alignment = empty_alignment();
        let tick_id = TickId::new();
        let token = CancellationToken::new();

        let result = act(&kernel, &alignment, tick_id, &token).unwrap();

        assert!(result.executions.is_empty());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn act_executes_via_wi_host() {
        let dir = tempfile::tempdir().unwrap();
        let kernel = test_kernel_with_host(dir.path()).await;
        let alignment = alignment_with(vec![delay_action(10)]);
        let tick_id = TickId::new();
        let token = CancellationToken::new();

        // Must run from a blocking thread because act() uses block_on internally
        let result = tokio::task::spawn_blocking({
            let kernel = clone_kernel_for_blocking(&kernel);
            move || act(&kernel, &alignment, tick_id, &token)
        })
        .await
        .unwrap()
        .unwrap();

        assert_eq!(result.executions.len(), 1);
        assert_eq!(result.executions[0].action.tool_name, "delay");
        assert!(result.executions[0].result.is_ok());

        shutdown_host(kernel).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn act_records_success() {
        let dir = tempfile::tempdir().unwrap();
        let kernel = test_kernel_with_host(dir.path()).await;
        let alignment = alignment_with(vec![delay_action(10)]);
        let tick_id = TickId::new();
        let token = CancellationToken::new();

        let result = tokio::task::spawn_blocking({
            let kernel = clone_kernel_for_blocking(&kernel);
            move || act(&kernel, &alignment, tick_id, &token)
        })
        .await
        .unwrap()
        .unwrap();

        assert_eq!(result.executions.len(), 1);
        assert_eq!(result.executions[0].record.outcome, ActionOutcome::Success);
        assert_eq!(result.executions[0].record.action_type, "delay");

        shutdown_host(kernel).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn act_records_failure() {
        let dir = tempfile::tempdir().unwrap();
        let kernel = test_kernel_with_host(dir.path()).await;
        // Use an unknown tool to trigger a failure
        let alignment = alignment_with(vec![unknown_action()]);
        let tick_id = TickId::new();
        let token = CancellationToken::new();

        let result = tokio::task::spawn_blocking({
            let kernel = clone_kernel_for_blocking(&kernel);
            move || act(&kernel, &alignment, tick_id, &token)
        })
        .await
        .unwrap()
        .unwrap();

        // Act step should not return Err — it records failure per-action
        assert_eq!(result.executions.len(), 1);
        assert_eq!(result.executions[0].record.outcome, ActionOutcome::Failure);
        assert!(result.executions[0].result.is_err());

        shutdown_host(kernel).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn act_stores_receipt_artifact() {
        let dir = tempfile::tempdir().unwrap();
        let kernel = test_kernel_with_host(dir.path()).await;
        let alignment = alignment_with(vec![delay_action(10)]);
        let tick_id = TickId::new();
        let token = CancellationToken::new();
        let artifact_store = kernel.artifact_store.clone();

        let result = tokio::task::spawn_blocking({
            let kernel = clone_kernel_for_blocking(&kernel);
            move || act(&kernel, &alignment, tick_id, &token)
        })
        .await
        .unwrap()
        .unwrap();

        // I3: receipt artifact must be stored
        let receipt_ref = result.executions[0].record.receipt_ref.as_ref();
        assert!(
            receipt_ref.is_some(),
            "receipt_ref should be present for success"
        );

        let artifact = artifact_store.get(receipt_ref.unwrap()).unwrap();
        assert!(artifact.is_some(), "receipt artifact should exist in store");
        let artifact = artifact.unwrap();
        assert_eq!(artifact.kind, ArtifactKind::Receipt);

        // Verify the receipt content is valid JSON containing slept_ms
        let content: serde_json::Value = serde_json::from_slice(&artifact.content).unwrap();
        assert_eq!(content["slept_ms"], 10);

        shutdown_host(kernel).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn act_logs_action_executed_events() {
        let dir = tempfile::tempdir().unwrap();
        let kernel = test_kernel_with_host(dir.path()).await;
        let tick_id = TickId::new();
        let token = CancellationToken::new();
        let alignment = alignment_with(vec![delay_action(10), unknown_action()]);
        let event_ledger = kernel.event_ledger.clone();

        let _result = tokio::task::spawn_blocking({
            let kernel = clone_kernel_for_blocking(&kernel);
            move || act(&kernel, &alignment, tick_id, &token)
        })
        .await
        .unwrap()
        .unwrap();

        // Check that ActionExecuted events were logged for both actions
        let events = event_ledger.for_tick(tick_id).unwrap();
        let action_events: Vec<_> = events
            .iter()
            .filter(|e| e.event_type == EventType::ActionExecuted)
            .collect();
        assert_eq!(
            action_events.len(),
            2,
            "expected 2 ActionExecuted events, got {}",
            action_events.len()
        );

        // First event should be for the delay (success)
        assert!(
            action_events[0].summary.contains("succeeded"),
            "expected 'succeeded' in summary: {}",
            action_events[0].summary
        );
        // Second event should be for the unknown tool (failure)
        assert!(
            action_events[1].summary.contains("failed"),
            "expected 'failed' in summary: {}",
            action_events[1].summary
        );

        shutdown_host(kernel).await;
    }

    #[test]
    fn act_wi_host_not_available() {
        // Slot is None — should return an error
        let dir = tempfile::tempdir().unwrap();
        let kernel = test_kernel_no_host(dir.path());
        let alignment = alignment_with(vec![delay_action(10)]);
        let tick_id = TickId::new();
        let token = CancellationToken::new();

        let result = act(&kernel, &alignment, tick_id, &token);

        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("WI host not available"),
            "expected 'WI host not available' in error: {err_msg}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn act_multiple_actions_sequential() {
        // Submit 3 delay actions; verify all execute in order and all succeed.
        let dir = tempfile::tempdir().unwrap();
        let kernel = test_kernel_with_host(dir.path()).await;
        let alignment = alignment_with(vec![delay_action(1), delay_action(1), delay_action(1)]);
        let tick_id = TickId::new();
        let token = CancellationToken::new();

        let result = tokio::task::spawn_blocking({
            let kernel = clone_kernel_for_blocking(&kernel);
            move || act(&kernel, &alignment, tick_id, &token)
        })
        .await
        .unwrap()
        .unwrap();

        assert_eq!(result.executions.len(), 3, "expected 3 executions");
        for (i, exec) in result.executions.iter().enumerate() {
            assert_eq!(
                exec.record.outcome,
                ActionOutcome::Success,
                "action {i} should succeed"
            );
            assert_eq!(exec.action.tool_name, "delay");
            assert!(exec.result.is_ok(), "action {i} result should be Ok");
        }

        shutdown_host(kernel).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn act_cancellation_skips_remaining() {
        // Pre-cancelled token: first action executes, then cancellation is
        // detected and remaining actions are recorded as Skipped.
        let dir = tempfile::tempdir().unwrap();
        let kernel = test_kernel_with_host(dir.path()).await;
        let alignment = alignment_with(vec![delay_action(1), delay_action(1)]);
        let tick_id = TickId::new();
        let token = CancellationToken::new();
        token.cancel();

        let result = tokio::task::spawn_blocking({
            let kernel = clone_kernel_for_blocking(&kernel);
            move || act(&kernel, &alignment, tick_id, &token)
        })
        .await
        .unwrap()
        .unwrap();

        // Both actions should be present in executions
        assert_eq!(result.executions.len(), 2, "expected 2 executions");

        // First action executes (before the between-actions check)
        assert_eq!(
            result.executions[0].record.outcome,
            ActionOutcome::Success,
            "first action should execute before cancellation check"
        );

        // Second action is skipped (cancellation detected after first action)
        assert_eq!(
            result.executions[1].record.outcome,
            ActionOutcome::Skipped,
            "second action should be skipped after cancellation"
        );
        assert!(
            result.executions[1].result.is_err(),
            "skipped action result should be Err"
        );
        assert_eq!(
            result.executions[1].result.as_ref().unwrap_err(),
            "cancelled",
        );

        shutdown_host(kernel).await;
    }

    #[test]
    fn act_does_not_call_wi_host_from_other_steps() {
        // Code audit verification: the Act step (act.rs) is the ONLY kernel module
        // that invokes WI Host methods. Other PODAARA steps (perceive, orient,
        // decide, align, reflect, amend) must never call invoke_single or access
        // the WI Host for tool execution.
        //
        // This is the physical manifestation of I9 (IBP §3.3):
        // "The master loop's Act step is the only code path where a cognitive
        // decision results in tool execution."
        let act_source = include_str!("act.rs");
        let perceive_source = include_str!("perceive.rs");
        let orient_source = include_str!("orient.rs");
        let decide_source = include_str!("decide.rs");
        let align_source = include_str!("align.rs");
        let reflect_source = include_str!("reflect.rs");
        let amend_source = include_str!("amend.rs");

        // Act step SHOULD reference invoke_single (it's the boundary crossing)
        assert!(
            act_source.contains("invoke_single"),
            "Act step must call invoke_single"
        );

        // No other step should reference invoke_single
        assert!(
            !perceive_source.contains("invoke_single"),
            "Perceive must not call invoke_single"
        );
        assert!(
            !orient_source.contains("invoke_single"),
            "Orient must not call invoke_single"
        );
        assert!(
            !decide_source.contains("invoke_single"),
            "Decide must not call invoke_single"
        );
        assert!(
            !align_source.contains("invoke_single"),
            "Align must not call invoke_single"
        );
        assert!(
            !reflect_source.contains("invoke_single"),
            "Reflect must not call invoke_single"
        );
        assert!(
            !amend_source.contains("invoke_single"),
            "Amend must not call invoke_single"
        );
    }

    // ── Helper for spawn_blocking ──
    //
    // KernelContext contains Arc fields that are Send+Sync, but the struct itself
    // isn't automatically Send because it holds Arc<dyn Trait> pointers. We need
    // to move it into spawn_blocking. Since all inner Arc types are Send+Sync
    // and we only use shared references inside spawn_blocking, this is safe.
    //
    // In production, the AQ handler thread receives these through thread-safe
    // shared state. For tests, we use this helper.

    /// Clone the kernel's Arc fields into a new KernelContext that can be sent
    /// to a blocking thread.
    fn clone_kernel_for_blocking(kernel: &KernelContext) -> KernelContext {
        KernelContext {
            snapshot_store: kernel.snapshot_store.clone(),
            event_ledger: kernel.event_ledger.clone(),
            tick_store: kernel.tick_store.clone(),
            memory_store: kernel.memory_store.clone(),
            context_compiler: kernel.context_compiler.clone(),
            artifact_store: kernel.artifact_store.clone(),
            wi_host_slot: kernel.wi_host_slot.clone(),
            inbox: kernel.inbox.clone(),
            vessel_id: kernel.vessel_id,
            mission: kernel.mission.clone(),
            max_output_tokens: kernel.max_output_tokens,
            master_loop_interval_secs: kernel.master_loop_interval_secs,
            thread_registry: kernel.thread_registry.clone(),
            relationship_ledger: kernel.relationship_ledger.clone(),
            conversation_store: kernel.conversation_store.clone(),
            budget_tracker: kernel.budget_tracker.clone(),
            tool_budget_gate: kernel.tool_budget_gate.clone(),
            metrics: kernel.metrics.clone(),
            event_tx: kernel.event_tx.clone(),
            prompt_registry: kernel.prompt_registry.clone(),
            trust_decay_config: kernel.trust_decay_config.clone(),
            episodic_memory_capacity: kernel.episodic_memory_capacity,
            bootstrap_grace_period_ticks: kernel.bootstrap_grace_period_ticks,
            max_decide_turns: kernel.max_decide_turns,
            watch_store: kernel.watch_store.clone(),
            max_watches: kernel.max_watches,
        }
    }
}

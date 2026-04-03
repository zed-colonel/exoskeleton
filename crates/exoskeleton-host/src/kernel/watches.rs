//! WatchExecutor — checks due watches during Perceive and emits events/actions.

use exoskeleton_core::watch::WatchType;
use exoskeleton_core::{Artifact, ArtifactKind, EventEntry, EventType, LedgerEntryId, LiveEvent};

use super::types::PlannedAction;
use super::KernelContext;
use crate::introspection::IntrospectionService;

/// Result of checking all due watches for this tick.
pub struct WatchCheckResult {
    /// Events emitted by triggered threshold watches.
    pub triggered_events: Vec<EventEntry>,
    /// Actions to execute for due poll watches (bypass Align — already approved at creation).
    pub poll_actions: Vec<PlannedAction>,
}

/// Check all due watches for this tick.
///
/// Threshold watches: evaluate condition via IntrospectionService, emit
/// WatchTriggered events if condition met.
///
/// Poll watches: create PlannedAction for connector invocation, to be
/// executed in the Act step. Results arrive as events in the next Perceive.
pub fn check_watches(kernel: &KernelContext, tick_number: u64) -> WatchCheckResult {
    let introspection = IntrospectionService::new(kernel);
    let mut triggered_events = Vec::new();
    let mut poll_actions = Vec::new();

    let due = match kernel.watch_store.due_watches(tick_number) {
        Ok(watches) => watches,
        Err(e) => {
            tracing::warn!(error = %e, "failed to query due watches");
            return WatchCheckResult {
                triggered_events,
                poll_actions,
            };
        }
    };

    for watch in &due {
        match &watch.watch_type {
            WatchType::Threshold { metric, condition } => {
                match introspection.query_metric(metric) {
                    Ok(value) => {
                        // For Changed conditions, we'd need the previous value.
                        // Use None for now — Changed triggers on first non-NaN reading
                        // after the watch is created (since there's no stored previous value).
                        let previous = None;
                        if condition.is_triggered(value, previous) {
                            let payload = serde_json::json!({
                                "watch_id": watch.id.to_string(),
                                "watch_name": watch.name,
                                "metric": watch.watch_type,
                                "value": value,
                                "condition": condition,
                            });
                            let artifact = Artifact::new(
                                ArtifactKind::Event,
                                serde_json::to_vec(&payload).unwrap_or_default(),
                                "application/json".into(),
                            );
                            let payload_ref = kernel.artifact_store.put(&artifact).ok();

                            let event = EventEntry {
                                id: LedgerEntryId::new(),
                                tick_id: None, // Will be set by caller
                                event_type: EventType::WatchTriggered,
                                payload_ref,
                                summary: format!(
                                    "Watch '{}' triggered: value={:.2}",
                                    watch.name, value
                                ),
                                timestamp: chrono::Utc::now(),
                            };
                            let _ = kernel.event_ledger.append(&event);

                            let _ = kernel.event_tx.send(LiveEvent {
                                event_type: EventType::WatchTriggered,
                                summary: event.summary.clone(),
                                ..LiveEvent::new(Some(tick_number))
                            });

                            triggered_events.push(event);
                            let _ = kernel.watch_store.record_trigger(watch.id, tick_number);
                        } else {
                            let _ = kernel.watch_store.record_check(watch.id, tick_number);
                        }
                    }
                    Err(e) => {
                        tracing::warn!(
                            watch_name = %watch.name,
                            error = %e,
                            "failed to query metric for threshold watch"
                        );
                        let _ = kernel.watch_store.record_check(watch.id, tick_number);
                    }
                }
            }
            WatchType::Poll {
                connector, params, ..
            } => {
                poll_actions.push(PlannedAction {
                    tool_name: connector.clone(),
                    params: params.clone(),
                    rationale: format!("Poll watch '{}' scheduled check", watch.name),
                    plan_task_id: None,
                });
                let _ = kernel.watch_store.record_check(watch.id, tick_number);
            }
        }
    }

    WatchCheckResult {
        triggered_events,
        poll_actions,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use chrono::Utc;
    use exoskeleton_core::conversation::InMemoryConversationStore;
    use exoskeleton_core::prompt::PromptRegistry;
    use exoskeleton_core::watch::*;
    use exoskeleton_core::*;
    use exoskeleton_memory::{ApproximateTokenCounter, ContextCompiler};
    use exoskeleton_relationship::InMemoryRelationshipLedger;
    use exoskeleton_threads::{InMemoryThreadStore, ThreadRegistry};

    use super::*;
    use crate::inbox::InMemoryInbox;
    use crate::storage::StorageManager;

    fn make_test_kernel_context() -> KernelContext {
        let dir = tempfile::tempdir().unwrap();
        let storage = StorageManager::open(dir.path()).unwrap();
        let counter = Arc::new(ApproximateTokenCounter);
        let compiler = ContextCompiler::with_defaults(counter, 4000);

        // Leak the tempdir so it doesn't get cleaned up during the test
        let _ = std::mem::ManuallyDrop::new(dir);

        KernelContext {
            snapshot_store: storage.snapshot_store().clone(),
            event_ledger: storage.event_ledger().clone(),
            tick_store: storage.tick_store().clone(),
            memory_store: storage.memory_store().clone(),
            context_compiler: Arc::new(compiler),
            artifact_store: storage.artifact_store().clone(),
            wi_host_slot: Arc::new(tokio::sync::Mutex::new(None)),
            inbox: Arc::new(InMemoryInbox::new()),
            vessel_id: VesselId::new(),
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
            watch_store: Arc::new(InMemoryWatchStore::new()),
            max_watches: 20,
            read_paths_this_tick: Arc::new(std::sync::Mutex::new(std::collections::HashSet::new())),
            inner_loop_config: crate::config::InnerLoopConfig::default(),
            tool_policy: crate::kernel::policy::ToolPolicyConfig::default(),
            session_approvals: crate::kernel::policy::SessionApprovals::new(),
            vessel_mode: Arc::new(std::sync::Mutex::new(exoskeleton_core::VesselMode::Normal)),
            wake_signal: None,
        }
    }

    fn make_threshold_watch(
        name: &str,
        metric: MetricKind,
        condition: WatchCondition,
    ) -> WatchDefinition {
        WatchDefinition {
            id: WatchId::new(),
            name: name.into(),
            description: format!("Test: {name}"),
            watch_type: WatchType::Threshold { metric, condition },
            schedule: WatchSchedule::EveryNTicks { n: 1 },
            status: WatchStatus::Active,
            created_at_tick: 0,
            last_checked_tick: None,
            trigger_count: 0,
            created_at: Utc::now(),
        }
    }

    fn make_poll_watch(name: &str) -> WatchDefinition {
        WatchDefinition {
            id: WatchId::new(),
            name: name.into(),
            description: format!("Test: {name}"),
            watch_type: WatchType::Poll {
                connector: "http.request".into(),
                params: serde_json::json!({"url": "https://example.com"}),
                extract: None,
                condition: None,
            },
            schedule: WatchSchedule::EveryNTicks { n: 1 },
            status: WatchStatus::Active,
            created_at_tick: 0,
            last_checked_tick: None,
            trigger_count: 0,
            created_at: Utc::now(),
        }
    }

    // ── E5S2-T4: watch_executor_threshold_triggers ──

    #[test]
    fn watch_executor_threshold_triggers() {
        let kernel = make_test_kernel_context();

        // Create a watch that monitors consecutive failures > 0
        // Since there are no ticks, consecutive failures = 0, so Above(0) should NOT trigger.
        // But we can test with Above(-1) which will always trigger.
        let watch = make_threshold_watch(
            "always-triggers",
            MetricKind::ConsecutiveFailures,
            WatchCondition::Above { value: -1.0 },
        );
        kernel.watch_store.save(&watch).unwrap();

        let result = check_watches(&kernel, 1);
        assert_eq!(result.triggered_events.len(), 1);
        assert!(result.triggered_events[0]
            .summary
            .contains("always-triggers"));

        // Watch should have been triggered
        let updated = kernel.watch_store.get(watch.id).unwrap().unwrap();
        assert_eq!(updated.trigger_count, 1);
        assert_eq!(updated.last_checked_tick, Some(1));
    }

    // ── E5S2-T5: watch_executor_threshold_no_trigger ──

    #[test]
    fn watch_executor_threshold_no_trigger() {
        let kernel = make_test_kernel_context();

        // Consecutive failures = 0, so Above(100) should not trigger
        let watch = make_threshold_watch(
            "never-triggers",
            MetricKind::ConsecutiveFailures,
            WatchCondition::Above { value: 100.0 },
        );
        kernel.watch_store.save(&watch).unwrap();

        let result = check_watches(&kernel, 1);
        assert_eq!(result.triggered_events.len(), 0);

        // Watch should have been checked but not triggered
        let updated = kernel.watch_store.get(watch.id).unwrap().unwrap();
        assert_eq!(updated.trigger_count, 0);
        assert_eq!(updated.last_checked_tick, Some(1));
    }

    // ── E5S2-T6: watch_executor_poll_queued ──

    #[test]
    fn watch_executor_poll_queued() {
        let kernel = make_test_kernel_context();

        let watch = make_poll_watch("api-health");
        kernel.watch_store.save(&watch).unwrap();

        let result = check_watches(&kernel, 1);
        assert_eq!(result.poll_actions.len(), 1);
        assert_eq!(result.poll_actions[0].tool_name, "http.request");
        assert!(result.poll_actions[0].rationale.contains("api-health"));
    }

    // ── E5S2-T7: watch_executor_schedule_respected ──

    #[test]
    fn watch_executor_schedule_respected() {
        let kernel = make_test_kernel_context();

        let mut watch = make_threshold_watch(
            "every-5",
            MetricKind::ConsecutiveFailures,
            WatchCondition::Above { value: -1.0 },
        );
        watch.schedule = WatchSchedule::EveryNTicks { n: 5 };
        kernel.watch_store.save(&watch).unwrap();

        // Tick 3: not due (3-0 < 5)
        let result = check_watches(&kernel, 3);
        assert_eq!(result.triggered_events.len(), 0);

        // Tick 5: due (5-0 >= 5)
        let result = check_watches(&kernel, 5);
        assert_eq!(result.triggered_events.len(), 1);
    }

    // ── E5S2-T8: watch_executor_once_completes ──

    #[test]
    fn watch_executor_once_completes() {
        let kernel = make_test_kernel_context();

        let mut watch = make_threshold_watch(
            "once-watch",
            MetricKind::ConsecutiveFailures,
            WatchCondition::Above { value: -1.0 },
        );
        watch.schedule = WatchSchedule::Once;
        kernel.watch_store.save(&watch).unwrap();

        // First check: triggers and completes
        let result = check_watches(&kernel, 1);
        assert_eq!(result.triggered_events.len(), 1);

        let updated = kernel.watch_store.get(watch.id).unwrap().unwrap();
        assert_eq!(updated.status, WatchStatus::Completed);

        // Second check: not due (completed)
        let result = check_watches(&kernel, 2);
        assert_eq!(result.triggered_events.len(), 0);
    }

    // ── E5S2-T10: watch_triggered_event_in_ledger ──

    #[test]
    fn watch_triggered_event_in_ledger() {
        let kernel = make_test_kernel_context();

        let watch = make_threshold_watch(
            "ledger-test",
            MetricKind::ConsecutiveFailures,
            WatchCondition::Above { value: -1.0 },
        );
        kernel.watch_store.save(&watch).unwrap();

        check_watches(&kernel, 1);

        // Event should be in the ledger
        let events = kernel
            .event_ledger
            .by_type(EventType::WatchTriggered, 10)
            .unwrap();
        assert_eq!(events.len(), 1);
        assert!(events[0].summary.contains("ledger-test"));
    }
}

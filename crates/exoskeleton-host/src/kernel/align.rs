//! Align step — relationship-aware action filtering (Sprint 8).
//!
//! Replaces the Sprint 5 stub. Processes relational signals, compiles a fresh
//! RelationshipSnapshot, and checks proposed actions against relationship
//! constraints before they reach the Act step.

use exoskeleton_core::TickId;
use exoskeleton_relationship::{AlignAction, AlignConfig};

use super::types::{AlignmentResult, DecisionResult, PerceptionResult};
use super::KernelContext;

/// Execute the Align step: process signals, compile snapshot, filter actions.
///
/// 1. Process relational signals from Perceive → append to ledger (IBP §4.4)
/// 2. Compile fresh RelationshipSnapshot from entire ledger (I5)
/// 3. Check each proposed action against relationship constraints
/// 4. Split into approved/blocked actions
/// 5. Generate relationship_updates (new ledger entries from alignment decisions)
pub fn align(
    kernel: &KernelContext,
    decision: &DecisionResult,
    perception: &PerceptionResult,
    tick_id: TickId,
    tick_number: u64,
) -> AlignmentResult {
    // 1. Process relational signals from inbox messages → ledger entries
    let _signal_ids = exoskeleton_relationship::process_relational_signals(
        &perception.new_messages,
        kernel.relationship_ledger.as_ref(),
        kernel.artifact_store.as_ref(),
        tick_id,
    );

    // 2. Compile fresh RelationshipSnapshot from ledger
    let snapshot = match exoskeleton_relationship::compile_relationship_snapshot(
        kernel.relationship_ledger.as_ref(),
        kernel.trust_decay_config.as_ref(),
        chrono::Utc::now(),
    ) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(error = %e, "failed to compile relationship snapshot — passthrough");
            // On error, pass all actions through (graceful degradation)
            return AlignmentResult {
                approved_actions: decision.actions.clone(),
                blocked_actions: Vec::new(),
                relationship_updates: Vec::new(),
                relationship_snapshot: None,
            };
        }
    };

    // 3. Check alignment for each proposed action
    let config = AlignConfig::default();
    let align_actions: Vec<AlignAction> = decision
        .actions
        .iter()
        .map(|a| AlignAction {
            tool_name: a.tool_name.clone(),
            rationale: a.rationale.clone(),
        })
        .collect();

    let (approved_indices, blocked_indices, relationship_updates) =
        exoskeleton_relationship::check_alignment(&align_actions, &snapshot, &config, tick_id);

    // 4. Build result
    let approved_actions: Vec<_> = approved_indices
        .iter()
        .map(|&i| decision.actions[i].clone())
        .collect();

    let blocked_actions: Vec<_> = blocked_indices
        .iter()
        .map(|&(i, ref reason)| (decision.actions[i].clone(), reason.clone()))
        .collect();

    // Emit CapabilityRequest events for blocked actions
    for (action, reason) in &blocked_actions {
        tracing::info!(
            tool = %action.tool_name,
            reason = %reason,
            "action blocked by Align step"
        );

        // Create capability request payload artifact
        let payload = exoskeleton_core::CapabilityRequestPayload {
            capability: action.tool_name.clone(),
            reason: reason.clone(),
            context: action.rationale.clone(),
            acknowledged: false,
        };
        let payload_json = serde_json::to_vec(&payload).unwrap_or_default();
        let artifact = exoskeleton_core::Artifact::new(
            exoskeleton_core::ArtifactKind::Event,
            payload_json,
            "application/json".into(),
        );
        let payload_ref = kernel.artifact_store.put(&artifact).ok();

        // Append EventEntry to ledger
        let event = exoskeleton_core::EventEntry {
            id: exoskeleton_core::LedgerEntryId::new(),
            tick_id: Some(tick_id),
            event_type: exoskeleton_core::EventType::CapabilityRequest,
            payload_ref,
            summary: format!(
                "Capability request: {} blocked — {}",
                action.tool_name, reason
            ),
            timestamp: chrono::Utc::now(),
        };
        let _ = kernel.event_ledger.append(&event);

        // Broadcast LiveEvent for real-time observers
        let live_event = exoskeleton_core::LiveEvent {
            event_type: exoskeleton_core::EventType::CapabilityRequest,
            summary: event.summary.clone(),
            ..exoskeleton_core::LiveEvent::new(None)
        };
        let _ = kernel.event_tx.send(live_event);
    }

    // Process watch proposals (E5-S2)
    for proposal in &decision.watch_proposals {
        let current_count = kernel.watch_store.active_count().unwrap_or(0);
        if current_count >= kernel.max_watches {
            tracing::info!(
                watch_name = %proposal.name,
                current_count,
                max = kernel.max_watches,
                "watch proposal blocked: limit reached"
            );
            continue;
        }

        let watch = exoskeleton_core::watch::WatchDefinition {
            id: exoskeleton_core::WatchId::new(),
            name: proposal.name.clone(),
            description: proposal.description.clone(),
            watch_type: proposal.watch_type.clone(),
            schedule: proposal.schedule,
            status: exoskeleton_core::watch::WatchStatus::Active,
            created_at_tick: tick_number,
            last_checked_tick: None,
            trigger_count: 0,
            created_at: chrono::Utc::now(),
        };

        if let Err(e) = kernel.watch_store.save(&watch) {
            tracing::warn!(error = %e, "failed to save approved watch");
            continue;
        }

        tracing::info!(
            watch_name = %watch.name,
            watch_id = %watch.id,
            "watch proposal approved and saved"
        );
    }

    AlignmentResult {
        approved_actions,
        blocked_actions,
        relationship_updates,
        relationship_snapshot: Some(snapshot),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use exoskeleton_core::conversation::InMemoryConversationStore;
    use exoskeleton_core::prompt::PromptRegistry;
    use exoskeleton_core::tick::LlmCallRecord;
    use exoskeleton_core::{ArtifactId, LiveEvent, VesselId};
    use exoskeleton_memory::{ApproximateTokenCounter, ContextCompiler};
    use exoskeleton_relationship::{InMemoryRelationshipLedger, RelationshipLedger};
    use exoskeleton_threads::{InMemoryThreadStore, ThreadRegistry};

    use super::super::types::{DecisionResult, PerceptionResult, PlannedAction, SnapshotDelta};
    use super::super::KernelContext;
    use super::*;
    use crate::inbox::InMemoryInbox;
    use crate::storage::StorageManager;

    fn test_kernel_with_ledger(
        dir: &std::path::Path,
        ledger: Arc<InMemoryRelationshipLedger>,
    ) -> KernelContext {
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
            vessel_id: VesselId::new(),
            mission: "test".into(),
            max_output_tokens: 4096,
            master_loop_interval_secs: 60,
            thread_registry: Arc::new(ThreadRegistry::new(Arc::new(InMemoryThreadStore::new()))),
            exec_thread_registry: Arc::new(crate::exec_threads::ExecThreadRegistry::new(Arc::new(
                crate::exec_threads::InMemoryExecThreadStore::new(),
            ))),
            relationship_ledger: ledger,
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
            coding_thread_config: crate::config::CodingThreadConfig::default(),
            tool_policy: crate::kernel::policy::ToolPolicyConfig::default(),
            session_approvals: crate::kernel::policy::SessionApprovals::new(),
            vessel_mode: Arc::new(std::sync::Mutex::new(exoskeleton_core::VesselMode::Normal)),
            wake_signal: None,
            observatory_url: None,
            observatory_token: None,
        }
    }

    fn test_decision(actions: Vec<PlannedAction>) -> DecisionResult {
        DecisionResult {
            reasoning: "test reasoning".into(),
            reply: None,
            actions,
            snapshot_delta: SnapshotDelta::default(),
            memory_notes: vec![],
            llm_call_record: LlmCallRecord {
                model: "test".into(),
                tokens_in: 0,
                tokens_out: 0,
                cost_cents: 0.0,
                latency_ms: 0,
                response_artifact_ref: None,
                turns: 1,
            },
            response_artifact_id: ArtifactId::from_content(b"test"),
            watch_proposals: vec![],
            vessel_mode_request: None,
        }
    }

    fn test_action(name: &str) -> PlannedAction {
        PlannedAction {
            call_id: format!("call_{name}"),
            tool_name: name.into(),
            params: serde_json::json!({}),
            rationale: format!("test {name}"),
            plan_task_id: None,
            origin_exec_thread_id: None,
            proposal_id: None,
        }
    }

    fn empty_perception() -> PerceptionResult {
        PerceptionResult {
            new_messages: vec![],
            active_conversations: vec![],
            thread_outputs: vec![],
            exec_thread_outputs: vec![],
            pending_action_results: vec![],
        }
    }

    // ── T-6: Kernel Align Integration ──

    #[test]
    fn align_with_empty_relationship_state_passthrough() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = Arc::new(InMemoryRelationshipLedger::new());
        let kernel = test_kernel_with_ledger(dir.path(), ledger);
        let decision = test_decision(vec![test_action("fs.write"), test_action("delay")]);
        let perception = empty_perception();
        let tick_id = TickId::new();

        let result = align(&kernel, &decision, &perception, tick_id, 1);
        assert_eq!(result.approved_actions.len(), 2);
        assert!(result.blocked_actions.is_empty());
        assert!(result.relationship_snapshot.is_some());
        assert!(result
            .relationship_snapshot
            .as_ref()
            .unwrap()
            .principals
            .is_empty());
    }

    #[test]
    fn align_with_no_actions_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = Arc::new(InMemoryRelationshipLedger::new());
        let kernel = test_kernel_with_ledger(dir.path(), ledger);
        let decision = test_decision(vec![]);
        let perception = empty_perception();
        let tick_id = TickId::new();

        let result = align(&kernel, &decision, &perception, tick_id, 1);
        assert!(result.approved_actions.is_empty());
        assert!(result.blocked_actions.is_empty());
    }

    #[test]
    fn align_with_low_trust_blocks_actions() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = Arc::new(InMemoryRelationshipLedger::new());

        // Build a principal with very low trust
        let principal = exoskeleton_core::PrincipalId::new();
        let ts = chrono::Utc::now();

        // Multiple broken commitments to drive trust down
        for i in 0..5 {
            ledger
                .append(&exoskeleton_core::RelationshipRecord {
                    id: exoskeleton_core::LedgerEntryId::new(),
                    principal_id: principal,
                    signal_type: exoskeleton_core::RelationalSignalType::CommitmentMade,
                    content_ref: ArtifactId::from_content(format!("made-{i}").as_bytes()),
                    tick_id: TickId::new(),
                    timestamp: ts + chrono::Duration::seconds(i * 2),
                    metadata: Default::default(),
                })
                .unwrap();
            ledger
                .append(&exoskeleton_core::RelationshipRecord {
                    id: exoskeleton_core::LedgerEntryId::new(),
                    principal_id: principal,
                    signal_type: exoskeleton_core::RelationalSignalType::CommitmentBroken,
                    content_ref: ArtifactId::from_content(format!("broken-{i}").as_bytes()),
                    tick_id: TickId::new(),
                    timestamp: ts + chrono::Duration::seconds(i * 2 + 1),
                    metadata: Default::default(),
                })
                .unwrap();
        }

        let kernel = test_kernel_with_ledger(dir.path(), ledger);
        let decision = test_decision(vec![test_action("delay")]);
        let perception = empty_perception();
        let tick_id = TickId::new();

        let result = align(&kernel, &decision, &perception, tick_id, 1);
        // Trust should be well below 0.3 after 5 broken commitments (-0.15 each from 0.5)
        assert!(result.approved_actions.is_empty());
        assert_eq!(result.blocked_actions.len(), 1);
        assert!(result.blocked_actions[0].1.contains("trust gate"));
    }

    // ── E4S4-T3: align_emits_capability_request_on_block ──

    #[test]
    fn align_emits_capability_request_on_block() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = Arc::new(InMemoryRelationshipLedger::new());

        // Build a principal with very low trust (same setup as align_with_low_trust_blocks_actions)
        let principal = exoskeleton_core::PrincipalId::new();
        let ts = chrono::Utc::now();
        for i in 0..5 {
            ledger
                .append(&exoskeleton_core::RelationshipRecord {
                    id: exoskeleton_core::LedgerEntryId::new(),
                    principal_id: principal,
                    signal_type: exoskeleton_core::RelationalSignalType::CommitmentMade,
                    content_ref: ArtifactId::from_content(format!("cap-made-{i}").as_bytes()),
                    tick_id: TickId::new(),
                    timestamp: ts + chrono::Duration::seconds(i * 2),
                    metadata: Default::default(),
                })
                .unwrap();
            ledger
                .append(&exoskeleton_core::RelationshipRecord {
                    id: exoskeleton_core::LedgerEntryId::new(),
                    principal_id: principal,
                    signal_type: exoskeleton_core::RelationalSignalType::CommitmentBroken,
                    content_ref: ArtifactId::from_content(format!("cap-broken-{i}").as_bytes()),
                    tick_id: TickId::new(),
                    timestamp: ts + chrono::Duration::seconds(i * 2 + 1),
                    metadata: Default::default(),
                })
                .unwrap();
        }

        let kernel = test_kernel_with_ledger(dir.path(), ledger);
        let decision = test_decision(vec![test_action("discord")]);
        let perception = empty_perception();
        let tick_id = TickId::new();

        let _result = align(&kernel, &decision, &perception, tick_id, 1);

        // Verify CapabilityRequest event was appended to ledger
        let events = kernel
            .event_ledger
            .by_type(exoskeleton_core::EventType::CapabilityRequest, 10)
            .unwrap();
        assert_eq!(events.len(), 1);
        assert!(events[0].summary.contains("discord"));
        assert!(events[0].payload_ref.is_some());

        // Verify payload artifact content
        let artifact = kernel
            .artifact_store
            .get(events[0].payload_ref.as_ref().unwrap())
            .unwrap()
            .unwrap();
        let payload: exoskeleton_core::CapabilityRequestPayload =
            serde_json::from_slice(&artifact.content).unwrap();
        assert_eq!(payload.capability, "discord");
        assert!(!payload.acknowledged);
    }

    // ── E4S4-T4: align_no_capability_request_on_approve ──

    #[test]
    fn align_no_capability_request_on_approve() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = Arc::new(InMemoryRelationshipLedger::new());
        let kernel = test_kernel_with_ledger(dir.path(), ledger);
        let decision = test_decision(vec![test_action("delay"), test_action("fs.read")]);
        let perception = empty_perception();
        let tick_id = TickId::new();

        let _result = align(&kernel, &decision, &perception, tick_id, 1);

        let events = kernel
            .event_ledger
            .by_type(exoskeleton_core::EventType::CapabilityRequest, 10)
            .unwrap();
        assert!(events.is_empty());
    }

    // ── E4S4-T5: align_capability_request_broadcasts_live_event ──

    #[test]
    fn align_capability_request_broadcasts_live_event() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = Arc::new(InMemoryRelationshipLedger::new());

        let principal = exoskeleton_core::PrincipalId::new();
        let ts = chrono::Utc::now();
        for i in 0..5 {
            ledger
                .append(&exoskeleton_core::RelationshipRecord {
                    id: exoskeleton_core::LedgerEntryId::new(),
                    principal_id: principal,
                    signal_type: exoskeleton_core::RelationalSignalType::CommitmentMade,
                    content_ref: ArtifactId::from_content(format!("bcast-made-{i}").as_bytes()),
                    tick_id: TickId::new(),
                    timestamp: ts + chrono::Duration::seconds(i * 2),
                    metadata: Default::default(),
                })
                .unwrap();
            ledger
                .append(&exoskeleton_core::RelationshipRecord {
                    id: exoskeleton_core::LedgerEntryId::new(),
                    principal_id: principal,
                    signal_type: exoskeleton_core::RelationalSignalType::CommitmentBroken,
                    content_ref: ArtifactId::from_content(format!("bcast-broken-{i}").as_bytes()),
                    tick_id: TickId::new(),
                    timestamp: ts + chrono::Duration::seconds(i * 2 + 1),
                    metadata: Default::default(),
                })
                .unwrap();
        }

        let kernel = test_kernel_with_ledger(dir.path(), ledger);
        // Subscribe BEFORE calling align
        let mut rx = kernel.event_tx.subscribe();
        let decision = test_decision(vec![test_action("webhook.send")]);
        let perception = empty_perception();
        let tick_id = TickId::new();

        let _result = align(&kernel, &decision, &perception, tick_id, 1);

        // Check broadcast
        let live_event = rx.try_recv().unwrap();
        assert_eq!(
            live_event.event_type,
            exoskeleton_core::EventType::CapabilityRequest
        );
        assert!(live_event.summary.contains("webhook.send"));
    }

    // ── E5S2-T9: watch_creation_align_gated ──

    #[test]
    fn watch_creation_align_gated() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = Arc::new(InMemoryRelationshipLedger::new());
        let kernel = test_kernel_with_ledger(dir.path(), ledger);

        let mut decision = test_decision(vec![]);
        decision.watch_proposals = vec![exoskeleton_core::watch::WatchProposal {
            name: "test-watch".into(),
            description: "A test watch".into(),
            watch_type: exoskeleton_core::watch::WatchType::Threshold {
                metric: exoskeleton_core::watch::MetricKind::ConsecutiveFailures,
                condition: exoskeleton_core::watch::WatchCondition::Above { value: 3.0 },
            },
            schedule: exoskeleton_core::watch::WatchSchedule::EveryNTicks { n: 1 },
        }];

        let perception = empty_perception();
        let tick_id = TickId::new();

        align(&kernel, &decision, &perception, tick_id, 42);

        // Watch should have been saved
        assert_eq!(kernel.watch_store.list().unwrap().len(), 1);
        assert_eq!(kernel.watch_store.active_count().unwrap(), 1);
        // Verify created_at_tick uses the actual tick number
        let watches = kernel.watch_store.list().unwrap();
        assert_eq!(watches[0].created_at_tick, 42);
    }

    // ── E5S2-T11: watch_count_limit_enforced ──

    #[test]
    fn watch_count_limit_enforced() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = Arc::new(InMemoryRelationshipLedger::new());
        let mut kernel = test_kernel_with_ledger(dir.path(), ledger);
        kernel.max_watches = 1; // Limit to 1

        // First watch should be saved
        let mut decision = test_decision(vec![]);
        decision.watch_proposals = vec![
            exoskeleton_core::watch::WatchProposal {
                name: "first-watch".into(),
                description: "First".into(),
                watch_type: exoskeleton_core::watch::WatchType::Threshold {
                    metric: exoskeleton_core::watch::MetricKind::TickDuration,
                    condition: exoskeleton_core::watch::WatchCondition::Above { value: 1000.0 },
                },
                schedule: exoskeleton_core::watch::WatchSchedule::EveryNTicks { n: 1 },
            },
            exoskeleton_core::watch::WatchProposal {
                name: "second-watch".into(),
                description: "Second — should be blocked".into(),
                watch_type: exoskeleton_core::watch::WatchType::Threshold {
                    metric: exoskeleton_core::watch::MetricKind::TickDuration,
                    condition: exoskeleton_core::watch::WatchCondition::Above { value: 2000.0 },
                },
                schedule: exoskeleton_core::watch::WatchSchedule::EveryNTicks { n: 1 },
            },
        ];

        let perception = empty_perception();
        let tick_id = TickId::new();

        align(&kernel, &decision, &perception, tick_id, 1);

        // Only 1 watch should have been saved (limit is 1)
        assert_eq!(kernel.watch_store.list().unwrap().len(), 1);
        let watches = kernel.watch_store.list().unwrap();
        assert_eq!(watches[0].name, "first-watch");
    }
}

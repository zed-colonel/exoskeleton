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

    // Log blocked actions
    for (action, reason) in &blocked_actions {
        tracing::info!(
            tool = %action.tool_name,
            reason = %reason,
            "action blocked by Align step"
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
            relationship_ledger: ledger,
            conversation_store: Arc::new(InMemoryConversationStore::new()),
            budget_tracker: None,
            tool_budget_gate: None,
            metrics: None,
            event_tx: tokio::sync::broadcast::channel::<LiveEvent>(16).0,
            prompt_registry: Arc::new(PromptRegistry::with_defaults()),
        }
    }

    fn test_decision(actions: Vec<PlannedAction>) -> DecisionResult {
        DecisionResult {
            reasoning: "test reasoning".into(),
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
            },
            response_artifact_id: ArtifactId::from_content(b"test"),
        }
    }

    fn test_action(name: &str) -> PlannedAction {
        PlannedAction {
            tool_name: name.into(),
            params: serde_json::json!({}),
            rationale: format!("test {name}"),
            plan_task_id: None,
        }
    }

    fn empty_perception() -> PerceptionResult {
        PerceptionResult {
            new_messages: vec![],
            active_conversations: vec![],
            thread_outputs: vec![],
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

        let result = align(&kernel, &decision, &perception, tick_id);
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

        let result = align(&kernel, &decision, &perception, tick_id);
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

        let result = align(&kernel, &decision, &perception, tick_id);
        // Trust should be well below 0.3 after 5 broken commitments (-0.15 each from 0.5)
        assert!(result.approved_actions.is_empty());
        assert_eq!(result.blocked_actions.len(), 1);
        assert!(result.blocked_actions[0].1.contains("trust gate"));
    }
}

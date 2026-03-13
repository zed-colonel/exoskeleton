//! Perceive step — gather inputs for this tick.

use exoskeleton_core::{EventType, ExoError, TickId};

use super::types::PerceptionResult;
use super::KernelContext;

/// Execute the Perceive step.
pub fn perceive(
    kernel: &KernelContext,
    previous_tick_id: Option<TickId>,
) -> Result<PerceptionResult, ExoError> {
    let new_messages = kernel.inbox.receive()?;
    let thread_outputs = Vec::new(); // Sprint 6

    let pending_action_results = match previous_tick_id {
        Some(tid) => {
            let events = kernel.event_ledger.for_tick(tid)?;
            events
                .into_iter()
                .filter(|e| e.event_type == EventType::ActionExecuted)
                .collect()
        }
        None => Vec::new(),
    };

    Ok(PerceptionResult {
        new_messages,
        thread_outputs,
        pending_action_results,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use chrono::Utc;
    use exoskeleton_core::{
        ArtifactId, EnvelopeId, EnvelopeKind, EventEntry, EventType, LedgerEntryId,
        MessageEnvelope, PrincipalId, TickId,
    };
    use exoskeleton_memory::{ApproximateTokenCounter, ContextCompiler};
    use exoskeleton_relationship::InMemoryRelationshipLedger;
    use exoskeleton_threads::{InMemoryThreadStore, ThreadRegistry};

    use super::super::KernelContext;
    use super::*;
    use crate::inbox::InMemoryInbox;
    use crate::storage::StorageManager;

    // ── Test helpers ──

    fn test_kernel_with_inbox(dir: &std::path::Path, inbox: Arc<InMemoryInbox>) -> KernelContext {
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
            inbox: inbox.clone(),
            vessel_id: exoskeleton_core::VesselId::new(),
            mission: "test".into(),
            max_output_tokens: 4096,
            master_loop_interval_secs: 60,
            thread_registry: Arc::new(ThreadRegistry::new(Arc::new(InMemoryThreadStore::new()))),
            relationship_ledger: Arc::new(InMemoryRelationshipLedger::new()),
            budget_tracker: None,
            tool_budget_gate: None,
            metrics: None,
        }
    }

    fn test_envelope() -> MessageEnvelope {
        MessageEnvelope {
            id: EnvelopeId::new(),
            source: PrincipalId::new(),
            target: None,
            kind: EnvelopeKind::HumanMessage,
            payload_ref: ArtifactId::from_content(b"test-msg"),
            timestamp: Utc::now(),
            in_reply_to: None,
        }
    }

    // ── T-3 Tests: Perceive Step ──

    #[test]
    fn perceive_empty_inputs() {
        let dir = tempfile::tempdir().unwrap();
        let inbox = Arc::new(InMemoryInbox::new());
        let kernel = test_kernel_with_inbox(dir.path(), inbox);

        let result = perceive(&kernel, None).unwrap();

        assert!(result.new_messages.is_empty());
        assert!(result.thread_outputs.is_empty());
        assert!(result.pending_action_results.is_empty());
    }

    #[test]
    fn perceive_with_inbox_messages() {
        let dir = tempfile::tempdir().unwrap();
        let inbox = Arc::new(InMemoryInbox::new());

        let env1 = test_envelope();
        let env2 = test_envelope();
        let id1 = env1.id;
        let id2 = env2.id;
        inbox.push(env1);
        inbox.push(env2);

        let kernel = test_kernel_with_inbox(dir.path(), inbox);

        let result = perceive(&kernel, None).unwrap();

        assert_eq!(result.new_messages.len(), 2);
        let msg_ids: Vec<EnvelopeId> = result.new_messages.iter().map(|m| m.id).collect();
        assert!(msg_ids.contains(&id1));
        assert!(msg_ids.contains(&id2));
    }

    #[test]
    fn perceive_with_previous_tick_events() {
        let dir = tempfile::tempdir().unwrap();
        let inbox = Arc::new(InMemoryInbox::new());
        let kernel = test_kernel_with_inbox(dir.path(), inbox);

        let tick_id = TickId::new();

        // Add ActionExecuted events for the previous tick
        let event1 = EventEntry {
            id: LedgerEntryId::new(),
            tick_id: Some(tick_id),
            event_type: EventType::ActionExecuted,
            payload_ref: None,
            summary: "Action delay: succeeded".into(),
            timestamp: Utc::now(),
        };
        let event2 = EventEntry {
            id: LedgerEntryId::new(),
            tick_id: Some(tick_id),
            event_type: EventType::ActionExecuted,
            payload_ref: None,
            summary: "Action fs.write: failed".into(),
            timestamp: Utc::now(),
        };
        kernel.event_ledger.append(&event1).unwrap();
        kernel.event_ledger.append(&event2).unwrap();

        let result = perceive(&kernel, Some(tick_id)).unwrap();

        assert_eq!(result.pending_action_results.len(), 2);
        assert!(result
            .pending_action_results
            .iter()
            .all(|e| e.event_type == EventType::ActionExecuted));
    }

    #[test]
    fn perceive_ignores_non_action_events() {
        let dir = tempfile::tempdir().unwrap();
        let inbox = Arc::new(InMemoryInbox::new());
        let kernel = test_kernel_with_inbox(dir.path(), inbox);

        let tick_id = TickId::new();

        // Add a TickStarted event (should be filtered out)
        let tick_started = EventEntry {
            id: LedgerEntryId::new(),
            tick_id: Some(tick_id),
            event_type: EventType::TickStarted,
            payload_ref: None,
            summary: "Tick started".into(),
            timestamp: Utc::now(),
        };
        // Add an ActionExecuted event (should be included)
        let action_event = EventEntry {
            id: LedgerEntryId::new(),
            tick_id: Some(tick_id),
            event_type: EventType::ActionExecuted,
            payload_ref: None,
            summary: "Action executed".into(),
            timestamp: Utc::now(),
        };
        kernel.event_ledger.append(&tick_started).unwrap();
        kernel.event_ledger.append(&action_event).unwrap();

        let result = perceive(&kernel, Some(tick_id)).unwrap();

        // Only the ActionExecuted event should be present
        assert_eq!(result.pending_action_results.len(), 1);
        assert_eq!(
            result.pending_action_results[0].event_type,
            EventType::ActionExecuted
        );
    }

    #[test]
    fn perceive_no_previous_tick() {
        let dir = tempfile::tempdir().unwrap();
        let inbox = Arc::new(InMemoryInbox::new());
        let kernel = test_kernel_with_inbox(dir.path(), inbox);

        // previous_tick_id is None => empty action results
        let result = perceive(&kernel, None).unwrap();

        assert!(result.pending_action_results.is_empty());
    }

    #[test]
    fn perceive_does_not_acknowledge_messages() {
        let dir = tempfile::tempdir().unwrap();
        let inbox = Arc::new(InMemoryInbox::new());
        inbox.push(test_envelope());
        inbox.push(test_envelope());

        let kernel = test_kernel_with_inbox(dir.path(), inbox.clone());

        // Perceive should read but not acknowledge
        let result = perceive(&kernel, None).unwrap();
        assert_eq!(result.new_messages.len(), 2);

        // Messages should still be pending (inbox.receive still returns them)
        let still_pending = kernel.inbox.receive().unwrap();
        assert_eq!(still_pending.len(), 2);

        // No messages should have been acknowledged
        assert!(inbox.acknowledged_ids().is_empty());
    }
}

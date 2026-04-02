//! Perceive step — gather inputs for this tick.
//!
//! Includes conversation grouping (E1-S2): incoming envelopes are assigned
//! to existing or new conversations before flowing to Orient.

use exoskeleton_core::conversation::{Conversation, ConversationStore};
use exoskeleton_core::{EnvelopeKind, EventType, ExoError, MessageEnvelope, TickId};

use super::types::PerceptionResult;
use super::KernelContext;

/// Time window for grouping messages from the same principal into an
/// existing conversation (when no explicit `in_reply_to` chain exists).
const GROUPING_WINDOW_SECS: i64 = 600; // 10 minutes

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

    // Conversation grouping (E1-S2)
    group_envelopes_into_conversations(&new_messages, kernel.conversation_store.as_ref())?;

    // Load active conversations for Orient
    let active_conversations = kernel.conversation_store.active_conversations(20)?;

    Ok(PerceptionResult {
        new_messages,
        active_conversations,
        thread_outputs,
        pending_action_results,
    })
}

/// Group incoming envelopes into conversations.
///
/// Algorithm:
/// 1. FILTER: Only HumanMessage and AgentMessage envelopes are conversational.
/// 2. EXPLICIT REPLY CHAIN: If `in_reply_to` is set, find that envelope's conversation.
/// 3. TIME-WINDOW HEURISTIC: Same principal, active conversation, within 10 minutes.
/// 4. NEW CONVERSATION: No match → create a new conversation.
fn group_envelopes_into_conversations(
    messages: &[MessageEnvelope],
    store: &dyn ConversationStore,
) -> Result<(), ExoError> {
    for envelope in messages {
        // Step 1: Only group conversational envelopes
        match envelope.kind {
            EnvelopeKind::HumanMessage | EnvelopeKind::AgentMessage => {}
            EnvelopeKind::SystemEvent | EnvelopeKind::RelationalSignal => continue,
        }

        // Step 2: Explicit reply chain
        if let Some(parent_id) = envelope.in_reply_to {
            if let Some(conv_id) = store.find_by_envelope(parent_id)? {
                if let Some(mut conv) = store.get(conv_id)? {
                    conv.add_message(
                        envelope.source,
                        envelope.id,
                        envelope.payload_ref.clone(),
                        envelope.timestamp,
                    );
                    store.save(&conv)?;
                    continue;
                }
            }
            // Parent not found — fall through to time-window heuristic
        }

        // Step 3: Time-window heuristic
        let active = store.active_conversations(50)?;
        let window = chrono::Duration::seconds(GROUPING_WINDOW_SECS);
        let mut matched = false;
        for mut conv in active {
            if conv.participants.contains(&envelope.source)
                && (envelope.timestamp - conv.updated_at) < window
            {
                conv.add_message(
                    envelope.source,
                    envelope.id,
                    envelope.payload_ref.clone(),
                    envelope.timestamp,
                );
                store.save(&conv)?;
                matched = true;
                break;
            }
        }

        if matched {
            continue;
        }

        // Step 4: New conversation
        let conv = Conversation::from_first_message(
            envelope.source,
            envelope.id,
            envelope.payload_ref.clone(),
            envelope.timestamp,
        );
        store.save(&conv)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use chrono::Utc;
    use exoskeleton_core::conversation::{ConversationState, InMemoryConversationStore};
    use exoskeleton_core::prompt::PromptRegistry;
    use exoskeleton_core::{
        ArtifactId, EnvelopeId, EnvelopeKind, EventEntry, EventType, LedgerEntryId, LiveEvent,
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

    // ── E1-S2: Conversation Grouping Tests ──

    // E1-T48: Envelope with in_reply_to joins existing conversation
    #[test]
    fn envelope_with_reply_to_joins_conversation() {
        let store = InMemoryConversationStore::new();
        let p = PrincipalId::new();
        let first_env_id = EnvelopeId::new();
        let now = Utc::now();

        // Create a conversation with one message
        let conv = Conversation::from_first_message(
            p,
            first_env_id,
            ArtifactId::from_content(b"first"),
            now,
        );
        store.save(&conv).unwrap();

        // Create a reply envelope
        let reply = MessageEnvelope {
            id: EnvelopeId::new(),
            source: p,
            target: None,
            kind: EnvelopeKind::HumanMessage,
            payload_ref: ArtifactId::from_content(b"reply"),
            timestamp: now + chrono::Duration::seconds(5),
            in_reply_to: Some(first_env_id),
        };

        group_envelopes_into_conversations(&[reply], &store).unwrap();

        let updated = store.get(conv.id).unwrap().unwrap();
        assert_eq!(updated.message_count(), 2);
    }

    // E1-T49: Time-window heuristic groups same principal within window
    #[test]
    fn time_window_groups_same_principal() {
        let store = InMemoryConversationStore::new();
        let p = PrincipalId::new();
        let now = Utc::now();

        // Create existing conversation
        let conv = Conversation::from_first_message(
            p,
            EnvelopeId::new(),
            ArtifactId::from_content(b"first"),
            now,
        );
        store.save(&conv).unwrap();

        // New message from same principal within 10 min window, no in_reply_to
        let msg = MessageEnvelope {
            id: EnvelopeId::new(),
            source: p,
            target: None,
            kind: EnvelopeKind::HumanMessage,
            payload_ref: ArtifactId::from_content(b"second"),
            timestamp: now + chrono::Duration::minutes(5),
            in_reply_to: None,
        };

        group_envelopes_into_conversations(&[msg], &store).unwrap();

        let updated = store.get(conv.id).unwrap().unwrap();
        assert_eq!(updated.message_count(), 2);
        // Should NOT create a new conversation
        assert_eq!(store.active_conversations(10).unwrap().len(), 1);
    }

    // E1-T50: Envelope from new principal creates new conversation
    #[test]
    fn new_principal_creates_new_conversation() {
        let store = InMemoryConversationStore::new();
        let p1 = PrincipalId::new();
        let p2 = PrincipalId::new();
        let now = Utc::now();

        // Existing conversation from p1
        let conv = Conversation::from_first_message(
            p1,
            EnvelopeId::new(),
            ArtifactId::from_content(b"p1-msg"),
            now,
        );
        store.save(&conv).unwrap();

        // New message from p2 — should create a new conversation
        let msg = MessageEnvelope {
            id: EnvelopeId::new(),
            source: p2,
            target: None,
            kind: EnvelopeKind::HumanMessage,
            payload_ref: ArtifactId::from_content(b"p2-msg"),
            timestamp: now + chrono::Duration::seconds(5),
            in_reply_to: None,
        };

        group_envelopes_into_conversations(&[msg], &store).unwrap();

        assert_eq!(store.active_conversations(10).unwrap().len(), 2);
    }

    // E1-T51: AgentMessage envelopes group identically
    #[test]
    fn agent_message_groups_identically() {
        let store = InMemoryConversationStore::new();
        let agent = PrincipalId::new();
        let now = Utc::now();

        let msg = MessageEnvelope {
            id: EnvelopeId::new(),
            source: agent,
            target: None,
            kind: EnvelopeKind::AgentMessage,
            payload_ref: ArtifactId::from_content(b"agent-msg"),
            timestamp: now,
            in_reply_to: None,
        };

        group_envelopes_into_conversations(&[msg], &store).unwrap();

        let convs = store.active_conversations(10).unwrap();
        assert_eq!(convs.len(), 1);
        assert_eq!(convs[0].participants, vec![agent]);
    }

    // E1-T52: SystemEvent envelopes are NOT grouped
    #[test]
    fn system_event_not_grouped() {
        let store = InMemoryConversationStore::new();

        let msg = MessageEnvelope {
            id: EnvelopeId::new(),
            source: PrincipalId::new(),
            target: None,
            kind: EnvelopeKind::SystemEvent,
            payload_ref: ArtifactId::from_content(b"sys-event"),
            timestamp: Utc::now(),
            in_reply_to: None,
        };

        group_envelopes_into_conversations(&[msg], &store).unwrap();

        assert!(store.active_conversations(10).unwrap().is_empty());
    }

    // E1-T53: Stale conversation reactivated
    #[test]
    fn stale_conversation_reactivated() {
        let store = InMemoryConversationStore::new();
        let p = PrincipalId::new();
        let now = Utc::now();

        // Create a stale conversation
        let mut conv = Conversation::from_first_message(
            p,
            EnvelopeId::new(),
            ArtifactId::from_content(b"old"),
            now - chrono::Duration::minutes(5),
        );
        conv.state = ConversationState::Stale;
        store.save(&conv).unwrap();

        // New message from same principal — should reactivate via add_message
        let msg = MessageEnvelope {
            id: EnvelopeId::new(),
            source: p,
            target: None,
            kind: EnvelopeKind::HumanMessage,
            payload_ref: ArtifactId::from_content(b"new"),
            timestamp: now,
            in_reply_to: None,
        };

        group_envelopes_into_conversations(&[msg], &store).unwrap();

        // The stale conversation doesn't appear in active_conversations,
        // so a new conversation should be created
        let all_active = store.active_conversations(10).unwrap();
        assert!(!all_active.is_empty());
    }

    // E1-T54: active_conversations populated on PerceptionResult
    #[test]
    fn active_conversations_populated() {
        let dir = tempfile::tempdir().unwrap();
        let inbox = Arc::new(InMemoryInbox::new());
        inbox.push(test_envelope());

        let kernel = test_kernel_with_inbox(dir.path(), inbox);
        let result = perceive(&kernel, None).unwrap();

        // The test envelope should have been grouped into a new conversation
        assert_eq!(result.active_conversations.len(), 1);
        assert_eq!(result.active_conversations[0].message_count(), 1);
    }
}

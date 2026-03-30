//! Orient step — compile context for the LLM.

use std::collections::HashMap;

use exoskeleton_core::{ArtifactId, ExoError, RelationshipSnapshot, StateSnapshot};
use exoskeleton_memory::ContextSources;

use super::types::{OrientationResult, PerceptionResult};
use super::KernelContext;

/// Execute the Orient step.
pub fn orient(
    kernel: &KernelContext,
    snapshot: &StateSnapshot,
    perception: &PerceptionResult,
) -> Result<OrientationResult, ExoError> {
    let episodic_summaries = kernel.memory_store.recent_episodic(10)?;
    let long_term_notes = kernel.memory_store.all_long_term(100)?;
    let recent_events = kernel.event_ledger.recent(20)?;

    // Load RelationshipSnapshot from artifact store (Sprint 8)
    let relationship_snapshot: Option<RelationshipSnapshot> =
        match &snapshot.relationship_snapshot_ref {
            Some(artifact_id) => match kernel.artifact_store.get(artifact_id) {
                Ok(Some(artifact)) => {
                    match serde_json::from_slice::<RelationshipSnapshot>(&artifact.content) {
                        Ok(rs) => Some(rs),
                        Err(e) => {
                            tracing::warn!(error = %e, "failed to parse relationship snapshot");
                            None
                        }
                    }
                }
                Ok(None) => {
                    tracing::warn!(
                        artifact_id = %artifact_id,
                        "relationship snapshot artifact not found"
                    );
                    None
                }
                Err(e) => {
                    tracing::warn!(error = %e, "failed to load relationship snapshot artifact");
                    None
                }
            },
            None => None,
        };

    // Resolve conversation message content from artifact store.
    // Each ConversationMessage holds a payload_ref (ArtifactId) pointing to the
    // actual text in the artifact store. We resolve these here so the context
    // compiler can render readable message content instead of opaque references.
    let resolved_message_content: HashMap<ArtifactId, String> = {
        let mut map = HashMap::new();
        for conv in &perception.active_conversations {
            // Resolve last 3 messages per conversation (matches renderer limit)
            for msg in conv.message_refs.iter().rev().take(3) {
                if map.contains_key(&msg.payload_ref) {
                    continue;
                }
                match kernel.artifact_store.get(&msg.payload_ref) {
                    Ok(Some(artifact)) => {
                        if let Ok(text) = String::from_utf8(artifact.content) {
                            map.insert(msg.payload_ref.clone(), text);
                        }
                    }
                    Ok(None) => {
                        tracing::debug!(
                            payload_ref = %msg.payload_ref,
                            "conversation message artifact not found"
                        );
                    }
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            payload_ref = %msg.payload_ref,
                            "failed to load conversation message artifact"
                        );
                    }
                }
            }
        }
        map
    };

    // Resolve the system section template (Epoch 0)
    let system_section = kernel
        .prompt_registry
        .resolve(
            "context-system-section",
            &[
                ("vessel_id", &kernel.vessel_id.to_string()),
                ("mission", &kernel.mission),
            ],
        )
        .ok();

    let sources = ContextSources {
        vessel_id: kernel.vessel_id,
        mission: &kernel.mission,
        snapshot,
        relationship_snapshot: relationship_snapshot.as_ref(),
        thread_contributions: &perception.thread_outputs,
        recent_events: &recent_events,
        episodic_summaries: &episodic_summaries,
        long_term_notes: &long_term_notes,
        plan: snapshot.plan.as_ref(),
        working_memory: &snapshot.working_memory,
        conversations: &perception.active_conversations,
        resolved_message_content: Some(&resolved_message_content),
        system_section_override: system_section.as_deref(),
    };

    let compiled_context = kernel.context_compiler.compile(&sources)?;

    Ok(OrientationResult { compiled_context })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use chrono::Utc;
    use exoskeleton_core::conversation::InMemoryConversationStore;
    use exoskeleton_core::prompt::PromptRegistry;
    use exoskeleton_core::{
        EventEntry, EventType, LedgerEntryId, LiveEvent, StateSnapshot, VesselId,
    };
    use exoskeleton_memory::{ApproximateTokenCounter, ContextCompiler, TokenCounter};
    use exoskeleton_relationship::InMemoryRelationshipLedger;
    use exoskeleton_threads::{InMemoryThreadStore, ThreadRegistry};

    use super::super::types::PerceptionResult;
    use super::super::KernelContext;
    use super::*;
    use crate::inbox::InMemoryInbox;
    use crate::storage::StorageManager;

    // ── Test helpers ──

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
            vessel_id: VesselId::new(),
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

    // ── T-4 Tests: Orient Step ──

    #[test]
    fn orient_compiles_within_budget() {
        let dir = tempfile::tempdir().unwrap();
        let kernel = test_kernel(dir.path());
        let snapshot = StateSnapshot::initial(kernel.vessel_id, "test mission".into());
        let perception = empty_perception();

        let result = orient(&kernel, &snapshot, &perception).unwrap();

        // Compiled context should respect the 4000 token budget
        assert!(
            result.compiled_context.total_tokens <= result.compiled_context.budget,
            "total_tokens ({}) should be <= budget ({})",
            result.compiled_context.total_tokens,
            result.compiled_context.budget
        );
    }

    #[test]
    fn orient_includes_snapshot_data() {
        let dir = tempfile::tempdir().unwrap();
        let kernel = test_kernel(dir.path());
        let snapshot = StateSnapshot::initial(kernel.vessel_id, "test mission".into());
        let perception = empty_perception();

        let result = orient(&kernel, &snapshot, &perception).unwrap();

        // The compiled prompt should contain the mission
        assert!(
            result.compiled_context.prompt.contains("test mission"),
            "prompt should include mission, got: {}",
            result.compiled_context.prompt
        );
    }

    #[test]
    fn orient_handles_empty_memory() {
        let dir = tempfile::tempdir().unwrap();
        let kernel = test_kernel(dir.path());
        // Fresh vessel: no episodic summaries, no long-term notes
        let snapshot = StateSnapshot::initial(kernel.vessel_id, "test mission".into());
        let perception = empty_perception();

        // Should compile successfully even with no memory
        let result = orient(&kernel, &snapshot, &perception);
        assert!(result.is_ok());
        let orientation = result.unwrap();
        assert!(!orientation.compiled_context.prompt.is_empty());
    }

    #[test]
    fn orient_handles_empty_perception() {
        let dir = tempfile::tempdir().unwrap();
        let kernel = test_kernel(dir.path());
        let snapshot = StateSnapshot::initial(kernel.vessel_id, "test mission".into());

        // No messages, no threads, no action results
        let perception = empty_perception();

        let result = orient(&kernel, &snapshot, &perception);
        assert!(result.is_ok());
        let orientation = result.unwrap();
        assert!(!orientation.compiled_context.prompt.is_empty());
    }

    #[test]
    fn orient_includes_recent_events() {
        let dir = tempfile::tempdir().unwrap();
        let kernel = test_kernel(dir.path());
        let snapshot = StateSnapshot::initial(kernel.vessel_id, "test mission".into());
        let perception = empty_perception();

        // Append distinctive events to the event ledger
        let event1 = EventEntry {
            id: LedgerEntryId::new(),
            tick_id: None,
            event_type: EventType::VesselStarted,
            payload_ref: None,
            summary: "Distinctive vessel boot alpha".into(),
            timestamp: Utc::now(),
        };
        let event2 = EventEntry {
            id: LedgerEntryId::new(),
            tick_id: None,
            event_type: EventType::ActionExecuted,
            payload_ref: None,
            summary: "Distinctive action beta completed".into(),
            timestamp: Utc::now(),
        };
        kernel.event_ledger.append(&event1).unwrap();
        kernel.event_ledger.append(&event2).unwrap();

        let result = orient(&kernel, &snapshot, &perception).unwrap();

        // Event summaries should appear in the compiled prompt
        assert!(
            result
                .compiled_context
                .prompt
                .contains("Distinctive vessel boot alpha"),
            "prompt should contain first event summary, got: {}",
            result.compiled_context.prompt
        );
        assert!(
            result
                .compiled_context
                .prompt
                .contains("Distinctive action beta completed"),
            "prompt should contain second event summary, got: {}",
            result.compiled_context.prompt
        );
        // The RECENT EVENTS header should be present
        assert!(
            result
                .compiled_context
                .prompt
                .contains("=== RECENT EVENTS ==="),
            "prompt should contain RECENT EVENTS header"
        );
    }

    #[test]
    fn orient_token_count_accurate() {
        let dir = tempfile::tempdir().unwrap();
        let kernel = test_kernel(dir.path());
        let snapshot = StateSnapshot::initial(kernel.vessel_id, "test mission".into());
        let perception = empty_perception();

        let result = orient(&kernel, &snapshot, &perception).unwrap();

        // Verify total_tokens matches what ApproximateTokenCounter would count
        let counter = ApproximateTokenCounter;
        let expected_tokens = counter.count(&result.compiled_context.prompt);
        assert_eq!(
            result.compiled_context.total_tokens, expected_tokens,
            "total_tokens ({}) should equal ApproximateTokenCounter::count() ({})",
            result.compiled_context.total_tokens, expected_tokens
        );
    }
}

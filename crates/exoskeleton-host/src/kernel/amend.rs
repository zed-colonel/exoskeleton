//! Amend step — update snapshot and persist tick state.
//!
//! The Amend step finalizes a PODAARA tick by:
//! 1. Building the new StateSnapshot from old + SnapshotDelta + reflect results
//! 2. Storing the new snapshot as an artifact (I3)
//! 3. Saving the snapshot to SnapshotStore (I7)
//! 4. Building and saving the TickRecord to TickStore
//! 5. Storing the TickRecord as an artifact (I3) using ArtifactKind::Tick
//! 6. Logging TickCompleted event to EventLedger
//! 7. Acknowledging consumed inbox messages
//! 8. Persisting memory notes to MemoryStore as episodic summaries
//! 9. Returning HandlerOutput::success_with_output(serialized tick summary)

use actionqueue_executor_local::HandlerOutput;
use chrono::{DateTime, Utc};
use exoskeleton_core::tick::{TickPhase, TickRecord};
use exoskeleton_core::{
    Artifact, ArtifactId, ArtifactKind, EnvelopeId, EpisodicSummary, EventEntry, EventType,
    ExoError, LedgerEntryId, LiveEvent, LongTermNote, PrincipalId, ThreadSchedule, TickId,
    VesselStatus,
};
use exoskeleton_memory::approximate_token_count;
use exoskeleton_threads::builtin::memory_consolidation;
use exoskeleton_threads::MEMORY_CONSOLIDATION_ID;

use super::types::{
    ActResult, AlignmentResult, DecisionResult, PerceptionResult, ReflectionResult,
};
use super::KernelContext;

/// Execute the Amend step.
///
/// Finalizes the tick by building and persisting the new StateSnapshot,
/// TickRecord, and associated artifacts. Acknowledges consumed inbox messages
/// and persists memory notes as episodic summaries.
///
/// Takes all prior PODAARA step results as parameters because the Amend step
/// needs data from every preceding phase to build the complete TickRecord.
#[allow(clippy::too_many_arguments)]
pub fn amend(
    kernel: &KernelContext,
    tick_id: TickId,
    tick_number: u64,
    started_at: DateTime<Utc>,
    snapshot_before: &exoskeleton_core::StateSnapshot,
    snapshot_before_artifact_id: ArtifactId,
    perception: &PerceptionResult,
    decision: &DecisionResult,
    alignment: &AlignmentResult,
    act_result: &ActResult,
    reflection: &ReflectionResult,
    context_breakdown_ref: Option<ArtifactId>,
) -> Result<HandlerOutput, ExoError> {
    // 1. Build new snapshot from old + SnapshotDelta
    let mut new_snapshot = snapshot_before.clone();
    new_snapshot.tick_number = tick_number;
    new_snapshot.updated_at = Utc::now();

    // Apply plan update from Decide step
    if let Some(plan_update) = &decision.snapshot_delta.plan_update {
        let current_plan = new_snapshot
            .plan
            .take()
            .unwrap_or_else(|| exoskeleton_core::Plan {
                objective: String::new(),
                tasks: Vec::new(),
                updated_at: Utc::now(),
            });
        new_snapshot.plan = Some(current_plan.apply_update(plan_update.clone()));
    }

    // Apply working memory ops from Decide step
    if let Some(ops) = &decision.snapshot_delta.working_memory_ops {
        new_snapshot.working_memory.apply_ops(ops, tick_number);
    }

    // Apply task status updates from Reflect step (overrides Decide)
    for task_update in &reflection.task_updates {
        if let Some(ref mut plan) = new_snapshot.plan {
            plan.apply_op(exoskeleton_core::PlanOp::UpdateStatus {
                task_id: task_update.task_id,
                new_status: task_update.new_status,
            });
            plan.updated_at = Utc::now();
        }
    }

    // Apply working memory ops from Reflect step (additive)
    for op in &reflection.working_memory_ops {
        new_snapshot.working_memory.apply_op(op, tick_number);
    }

    // TTL eviction
    new_snapshot.working_memory.evict_expired(tick_number);

    // Entry cap enforcement (50 entries max)
    new_snapshot.working_memory.enforce_cap(50);

    // Update status back to Idle after completing the tick
    new_snapshot.status = VesselStatus::Idle;
    new_snapshot.vessel_mode = *kernel.vessel_mode.lock().unwrap();

    // Build last_action_summary from act results
    new_snapshot.last_action_summary = if act_result.executions.is_empty() {
        Some("No actions taken".into())
    } else {
        let succeeded = act_result
            .executions
            .iter()
            .filter(|e| e.result.is_ok())
            .count();
        let failed = act_result
            .executions
            .iter()
            .filter(|e| e.result.is_err())
            .count();
        Some(format!(
            "{} actions: {} succeeded, {} failed",
            act_result.executions.len(),
            succeeded,
            failed,
        ))
    };

    // Update thread summaries in snapshot (Sprint 6)
    new_snapshot.thread_summaries = kernel
        .thread_registry
        .thread_summaries()
        .unwrap_or_default();
    new_snapshot.exec_thread_summaries = kernel
        .exec_thread_registry
        .exec_thread_summaries()
        .unwrap_or_default();
    prune_working_memory_for_bounded_coding_idle(kernel, &mut new_snapshot, perception);

    // Process Memory Consolidation thread outputs (Sprint 7)
    // The thread produces artifacts; the master loop writes to MemoryStore (IBP §4.3).
    process_memory_consolidation_outputs(kernel, perception, tick_number);

    // Episodic memory eviction (E1-S3, W-16)
    // Runs AFTER Memory Consolidation outputs are processed, ensuring important
    // entries get promoted to long-term notes before eviction.
    if let Some(capacity) = kernel.episodic_memory_capacity {
        match kernel.memory_store.evict_episodic_beyond(capacity) {
            Ok(evicted) if evicted > 0 => {
                tracing::info!(evicted, capacity, "evicted old episodic memory entries");
                let event = EventEntry {
                    id: LedgerEntryId::new(),
                    tick_id: Some(tick_id),
                    event_type: EventType::EpisodicEvicted,
                    payload_ref: None,
                    summary: format!("Evicted {evicted} episodic entries (capacity: {capacity})"),
                    timestamp: Utc::now(),
                };
                if let Err(e) = kernel.event_ledger.append(&event) {
                    tracing::warn!(error = %e, "failed to log EpisodicEvicted event");
                }
            }
            Ok(_) => {} // No eviction needed
            Err(e) => {
                tracing::warn!(error = %e, "failed to evict episodic memory");
            }
        }
    }

    // Stale conversation detection (E1-S2)
    mark_stale_conversations(kernel);

    // ── OA-S1: Process vessel reply ──────────────────────────────────
    if let Some(ref reply_text) = decision.reply {
        // Store reply content as artifact (I3: replayable).
        let reply_artifact = Artifact::new(
            ArtifactKind::Envelope,
            reply_text.as_bytes().to_vec(),
            "text/plain".into(),
        );
        match kernel.artifact_store.put(&reply_artifact) {
            Ok(artifact_id) => {
                // Append vessel ConversationMessage to the most recently active conversation.
                let vessel_principal = PrincipalId::from(*new_snapshot.vessel_id.as_ref());
                if let Some(conv) = perception.active_conversations.first() {
                    let mut updated_conv = conv.clone();
                    updated_conv.add_message(
                        vessel_principal,
                        EnvelopeId::new(),
                        artifact_id.clone(),
                        Utc::now(),
                    );
                    let _ = kernel.conversation_store.save(&updated_conv);
                }

                // Emit VesselResponseSent event.
                let reply_summary = if reply_text.len() > 80 {
                    format!("Vessel reply: {}...", &reply_text[..80])
                } else {
                    format!("Vessel reply: {reply_text}")
                };
                let reply_event = EventEntry {
                    id: LedgerEntryId::new(),
                    tick_id: Some(tick_id),
                    event_type: EventType::VesselResponseSent,
                    payload_ref: Some(artifact_id),
                    summary: reply_summary,
                    timestamp: Utc::now(),
                };
                let _ = kernel.event_ledger.append(&reply_event);

                // Broadcast LiveEvent for real-time observers.
                let _ = kernel.event_tx.send(LiveEvent {
                    event_type: EventType::VesselResponseSent,
                    summary: reply_event.summary.clone(),
                    ..LiveEvent::new(Some(tick_number))
                });
            }
            Err(e) => {
                tracing::warn!(error = %e, "failed to store vessel reply artifact");
            }
        }
    }

    // Store RelationshipSnapshot as artifact and update ref (Sprint 8)
    if let Some(rel_snapshot) = &alignment.relationship_snapshot {
        match Artifact::from_json(ArtifactKind::RelationshipEntry, rel_snapshot) {
            Ok(rel_artifact) => match kernel.artifact_store.put(&rel_artifact) {
                Ok(artifact_id) => {
                    new_snapshot.relationship_snapshot_ref = Some(artifact_id);
                }
                Err(e) => {
                    tracing::warn!(error = %e, "failed to store relationship snapshot artifact");
                }
            },
            Err(e) => {
                tracing::warn!(error = %e, "failed to serialize relationship snapshot");
            }
        }
    }

    // Append relationship_updates to ledger and log events (Sprint 8)
    for record in &alignment.relationship_updates {
        if let Err(e) = kernel.relationship_ledger.append(record) {
            tracing::warn!(error = %e, "failed to append relationship update to ledger");
        }
    }
    if !alignment.relationship_updates.is_empty() {
        let rel_event = EventEntry {
            id: LedgerEntryId::new(),
            tick_id: Some(tick_id),
            event_type: EventType::RelationshipUpdated,
            payload_ref: None,
            summary: format!(
                "{} relationship updates from Align step",
                alignment.relationship_updates.len()
            ),
            timestamp: Utc::now(),
        };
        if let Err(e) = kernel.event_ledger.append(&rel_event) {
            tracing::warn!(error = %e, "failed to log RelationshipUpdated event");
        }

        // D2: Broadcast RelationshipUpdated LiveEvent
        let _ = kernel.event_tx.send(LiveEvent {
            event_type: EventType::RelationshipUpdated,
            summary: rel_event.summary.clone(),
            ..LiveEvent::new(Some(tick_number))
        });
    }

    // Populate BudgetStatus from trackers (Sprint 9, I6)
    if let Some(ref tracker) = kernel.budget_tracker {
        if let Ok(guard) = tracker.try_lock() {
            let tool_remaining = if let Some(ref gate) = kernel.tool_budget_gate {
                if let Ok(gate_guard) = gate.try_lock() {
                    gate_guard.remaining()
                } else {
                    u64::MAX
                }
            } else {
                u64::MAX
            };
            new_snapshot.budget_status = guard.budget_status(tool_remaining);
        }
    }

    // 1.5 Record budget and relationship metrics (Sprint 10)
    if let Some(ref m) = kernel.metrics {
        // Budget remaining gauges
        if let Some(ref tracker) = kernel.budget_tracker {
            if let Ok(guard) = tracker.try_lock() {
                m.budget_remaining
                    .with_label_values(&["local_tokens"])
                    .set(guard.remaining_local_tokens() as f64);
                m.budget_remaining
                    .with_label_values(&["frontier_tokens"])
                    .set(guard.remaining_frontier_tokens() as f64);
                m.budget_remaining
                    .with_label_values(&["frontier_cost_cents"])
                    .set(guard.remaining_frontier_cost() as f64);
            }
        }
        if let Some(ref gate) = kernel.tool_budget_gate {
            if let Ok(guard) = gate.try_lock() {
                m.budget_remaining
                    .with_label_values(&["tool_invocations"])
                    .set(guard.remaining() as f64);
            }
        }

        // Relationship entries count
        if let Ok(count) = kernel.relationship_ledger.count() {
            m.relationship_entries_total.set(count as i64);
        }
    }

    // 2. Store new snapshot as artifact (I3: everything replayable)
    let snapshot_artifact = Artifact::from_json(ArtifactKind::Snapshot, &new_snapshot)?;
    let snapshot_after_artifact_id = kernel.artifact_store.put(&snapshot_artifact)?;

    // 3. Save snapshot to SnapshotStore (I7: single coherent workspace)
    kernel.snapshot_store.save(&new_snapshot)?;

    // 4. Build TickRecord
    let tick_record = TickRecord {
        tick_id,
        tick_number,
        phase: TickPhase::Amend,
        started_at,
        completed_at: Some(Utc::now()),
        snapshot_before: snapshot_before_artifact_id,
        snapshot_after: Some(snapshot_after_artifact_id),
        thread_contributions: perception.thread_outputs.clone(),
        exec_thread_contributions: perception.exec_thread_outputs.clone(),
        actions_taken: act_result
            .executions
            .iter()
            .map(|e| e.record.clone())
            .collect(),
        llm_calls: {
            let mut calls = vec![decision.llm_call_record.clone()];
            if let Some(ref record) = reflection.llm_call_record {
                calls.push(record.clone());
            }
            calls
        },
        decision_rationale: Some(decision.reasoning.clone()),
        context_breakdown_ref,
    };

    // 5. Store TickRecord as artifact (I3)
    let tick_artifact = Artifact::from_json(ArtifactKind::Tick, &tick_record)?;
    kernel.artifact_store.put(&tick_artifact)?;

    // Save TickRecord to TickStore
    kernel.tick_store.save(&tick_record)?;

    // 6. Log TickCompleted event to EventLedger
    let event = EventEntry {
        id: LedgerEntryId::new(),
        tick_id: Some(tick_id),
        event_type: EventType::TickCompleted,
        payload_ref: None,
        summary: format!(
            "Tick {} completed: {}",
            tick_number,
            new_snapshot.last_action_summary.as_deref().unwrap_or(""),
        ),
        timestamp: Utc::now(),
    };
    kernel.event_ledger.append(&event)?;

    // D2: Broadcast TickCompleted LiveEvent with snapshot
    let _ = kernel.event_tx.send(LiveEvent {
        event_type: EventType::TickCompleted,
        summary: event.summary.clone(),
        snapshot: Some(new_snapshot.clone()),
        ..LiveEvent::new(Some(tick_number))
    });

    // 7. Acknowledge consumed inbox messages
    let message_ids: Vec<_> = perception.new_messages.iter().map(|m| m.id).collect();
    if !message_ids.is_empty() {
        kernel.inbox.acknowledge(&message_ids)?;
    }

    // 8. Persist memory notes as episodic summaries
    for note in &decision.memory_notes {
        let summary = EpisodicSummary {
            id: ArtifactId::from_content(note.as_bytes()),
            start_tick: tick_number,
            end_tick: tick_number,
            summary: note.clone(),
            key_events: vec![],
            token_count: approximate_token_count(note),
            created_at: Utc::now(),
        };
        if let Err(e) = kernel.memory_store.write_episodic(&summary) {
            tracing::warn!(error = %e, "failed to persist memory note");
        }
    }

    // 9. Return success with serialized tick summary
    let tick_summary = serde_json::json!({
        "tick_id": tick_id.to_string(),
        "tick_number": tick_number,
        "actions": act_result.executions.len(),
        "success_rate": reflection.action_success_rate,
    });
    let output_bytes = serde_json::to_vec(&tick_summary)?;
    Ok(HandlerOutput::success_with_output(output_bytes))
}

fn prune_working_memory_for_bounded_coding_idle(
    kernel: &KernelContext,
    snapshot: &mut exoskeleton_core::StateSnapshot,
    perception: &PerceptionResult,
) {
    if !kernel.coding_thread_config.return_to_idle_after_completion
        || !perception.new_messages.is_empty()
        || !snapshot.exec_thread_summaries.iter().any(|summary| {
            summary.kind == exoskeleton_core::ExecThreadKind::Coding
                && summary.status == exoskeleton_core::ExecThreadStatus::Idle
                && summary
                    .last_completion_reason
                    .as_ref()
                    .is_some_and(|reason| !reason.trim().is_empty())
        })
    {
        return;
    }

    snapshot.working_memory.entries.retain(|entry| {
        matches!(entry.key.as_str(), "session_status" | "user_question_pending")
    });
}

/// Process Memory Consolidation thread outputs by writing to MemoryStore.
///
/// Scans thread contributions for the Memory Consolidation thread (by its
/// deterministic ID). When found, retrieves the output, parses the
/// `MemoryConsolidation` struct, and writes episodic summaries and long-term
/// notes to the MemoryStore.
///
/// This maintains IBP §4.3: the thread produces an artifact; the master loop
/// processes it. The thread itself never writes to any store.
fn process_memory_consolidation_outputs(
    kernel: &KernelContext,
    perception: &PerceptionResult,
    tick_number: u64,
) {
    // Find Memory Consolidation contributions
    let mc_contributions: Vec<_> = perception
        .thread_outputs
        .iter()
        .filter(|tc| tc.thread_id == MEMORY_CONSOLIDATION_ID)
        .collect();

    for contribution in mc_contributions {
        // Retrieve the full ThreadOutput from the registry
        let outputs = match kernel
            .thread_registry
            .recent_outputs(MEMORY_CONSOLIDATION_ID, 1)
        {
            Ok(o) => o,
            Err(e) => {
                tracing::warn!(error = %e, "failed to retrieve Memory Consolidation output");
                continue;
            }
        };

        let output = match outputs.first() {
            Some(o) => o,
            None => continue,
        };

        // The output's summary field contains the ThreadResponse JSON from the LLM.
        // Try to parse the full MemoryConsolidation from the raw LLM content.
        // We retrieve the artifact to get the original LLM response content.
        let mc_content = match kernel.artifact_store.get(&contribution.artifact_id) {
            Ok(Some(artifact)) => {
                // The artifact is a ThreadOutput JSON. Parse it to get the summary.
                String::from_utf8_lossy(&artifact.content).to_string()
            }
            _ => output.summary.clone(),
        };

        // Try to parse as MemoryConsolidation from the stored ThreadOutput
        // The ThreadOutput artifact contains the full ThreadOutput struct as JSON.
        // We need to extract the summary and recommendations to reconstruct.
        let mc = memory_consolidation::parse_output(&mc_content);

        // If episodic_summary is empty, also try parsing from the output summary
        let mc = if mc.episodic_summary.is_empty() {
            // The thread's summary field may contain the consolidation info
            let alt = memory_consolidation::parse_output(&output.summary);
            if alt.episodic_summary.is_empty() {
                // Use the contribution summary as the episodic text
                exoskeleton_threads::MemoryConsolidation {
                    episodic_summary: contribution.summary.clone(),
                    long_term_notes: mc.long_term_notes,
                    deprecated_notes: mc.deprecated_notes,
                }
            } else {
                alt
            }
        } else {
            mc
        };

        // Derive the span covered by this consolidation from the MC schedule.
        // EveryNTicks(n) means this consolidation covers approximately the last n ticks.
        let mc_spec = memory_consolidation::spec();
        let span = match mc_spec.schedule {
            ThreadSchedule::EveryNTicks(n) => n as u64,
            _ => 5, // fallback if schedule is ever changed
        };

        // Write episodic summary to MemoryStore
        if !mc.episodic_summary.is_empty() {
            let token_count = approximate_token_count(&mc.episodic_summary);
            let episodic = EpisodicSummary {
                id: ArtifactId::from_content(mc.episodic_summary.as_bytes()),
                start_tick: tick_number.saturating_sub(span.saturating_sub(1)),
                end_tick: tick_number,
                summary: mc.episodic_summary,
                key_events: vec![],
                token_count,
                created_at: Utc::now(),
            };
            if let Err(e) = kernel.memory_store.write_episodic(&episodic) {
                tracing::warn!(error = %e, "failed to write Memory Consolidation episodic summary");
            } else {
                tracing::debug!("wrote Memory Consolidation episodic summary");
            }
        }

        // Write long-term notes to MemoryStore
        for note in &mc.long_term_notes {
            let lt_note = LongTermNote {
                id: ArtifactId::from_content(note.content.as_bytes()),
                topic: note.topic.clone(),
                content: note.content.clone(),
                tags: note.tags.clone(),
                token_count: approximate_token_count(&note.content),
                created_at: Utc::now(),
            };
            if let Err(e) = kernel.memory_store.write_long_term(&lt_note) {
                tracing::warn!(
                    error = %e,
                    topic = %note.topic,
                    "failed to write Memory Consolidation long-term note"
                );
            } else {
                tracing::debug!(topic = %note.topic, "wrote Memory Consolidation long-term note");
            }
        }
    }
}

/// Mark active conversations as stale if no new messages within threshold.
///
/// Runs once per tick. Lightweight: reads active conversations, checks timestamps,
/// updates state. Errors are logged but do not halt the tick.
const STALE_THRESHOLD_MINUTES: i64 = 30;

fn mark_stale_conversations(kernel: &KernelContext) {
    use exoskeleton_core::conversation::ConversationState;

    let active = match kernel.conversation_store.active_conversations(100) {
        Ok(convs) => convs,
        Err(e) => {
            tracing::warn!(error = %e, "failed to load active conversations for stale detection");
            return;
        }
    };

    let now = Utc::now();
    let threshold = chrono::Duration::minutes(STALE_THRESHOLD_MINUTES);

    for conv in active {
        if (now - conv.updated_at) > threshold {
            let mut stale_conv = conv;
            stale_conv.state = ConversationState::Stale;
            stale_conv.updated_at = now;
            if let Err(e) = kernel.conversation_store.save(&stale_conv) {
                tracing::warn!(
                    error = %e,
                    conv_id = %stale_conv.id,
                    "failed to mark conversation stale"
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use chrono::Utc;
    use exoskeleton_core::conversation::InMemoryConversationStore;
    use exoskeleton_core::prompt::PromptRegistry;
    use exoskeleton_core::tick::LlmCallRecord;
    use exoskeleton_core::{
        ArtifactId, ArtifactKind, EnvelopeId, EnvelopeKind, EventType, MessageEnvelope,
        PrincipalId, StateSnapshot, TickId, VesselId,
    };
    use exoskeleton_memory::{ApproximateTokenCounter, ContextCompiler};
    use exoskeleton_relationship::InMemoryRelationshipLedger;
    use exoskeleton_threads::{InMemoryThreadStore, ThreadRegistry};

    use super::super::types::{
        ActResult, ActionExecution, AlignmentResult, DecisionResult, PerceptionResult,
        ReflectionResult, SnapshotDelta,
    };
    use super::super::KernelContext;
    use super::*;
    use crate::inbox::InMemoryInbox;
    use crate::storage::StorageManager;

    // ── Test helpers ──

    fn test_kernel(dir: &std::path::Path) -> (KernelContext, Arc<InMemoryInbox>) {
        let storage = StorageManager::open(dir).unwrap();
        let counter = Arc::new(ApproximateTokenCounter);
        let compiler = ContextCompiler::with_defaults(counter, 4000);
        let inbox = Arc::new(InMemoryInbox::new());

        let kernel = KernelContext {
            snapshot_store: storage.snapshot_store().clone(),
            event_ledger: storage.event_ledger().clone(),
            tick_store: storage.tick_store().clone(),
            memory_store: storage.memory_store().clone(),
            context_compiler: Arc::new(compiler),
            artifact_store: storage.artifact_store().clone(),
            wi_host_slot: Arc::new(tokio::sync::Mutex::new(None)),
            inbox: inbox.clone(),
            vessel_id: VesselId::new(),
            mission: "test".into(),
            max_output_tokens: 4096,
            master_loop_interval_secs: 60,
            thread_registry: Arc::new(ThreadRegistry::new(Arc::new(InMemoryThreadStore::new()))),
            exec_thread_registry: Arc::new(crate::exec_threads::ExecThreadRegistry::new(Arc::new(
                crate::exec_threads::InMemoryExecThreadStore::new(),
            ))),
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
            coding_thread_config: crate::config::CodingThreadConfig::default(),
            tool_policy: crate::kernel::policy::ToolPolicyConfig::default(),
            session_approvals: crate::kernel::policy::SessionApprovals::new(),
            vessel_mode: Arc::new(std::sync::Mutex::new(exoskeleton_core::VesselMode::Normal)),
            wake_signal: None,
            observatory_url: None,
            observatory_token: None,
        };
        (kernel, inbox)
    }

    fn test_snapshot(vessel_id: VesselId) -> StateSnapshot {
        StateSnapshot::initial(vessel_id, "test mission".into())
    }

    fn test_params() -> (
        DecisionResult,
        PerceptionResult,
        AlignmentResult,
        ActResult,
        ReflectionResult,
    ) {
        let decision = DecisionResult {
            reasoning: "test reasoning".into(),
            reply: None,
            actions: vec![],
            snapshot_delta: SnapshotDelta::default(),
            memory_notes: vec![],
            llm_call_record: LlmCallRecord {
                model: "test".into(),
                tokens_in: 10,
                tokens_out: 5,
                cost_cents: 0.0,
                latency_ms: 100,
                response_artifact_ref: None,
                turns: 1,
            },
            response_artifact_id: ArtifactId::from_content(b"test-resp"),
            watch_proposals: vec![],
            vessel_mode_request: None,
        };
        let perception = PerceptionResult {
            new_messages: vec![],
            active_conversations: vec![],
            thread_outputs: vec![],
            exec_thread_outputs: vec![],
            pending_action_results: vec![],
        };
        let alignment = AlignmentResult {
            approved_actions: vec![],
            blocked_actions: vec![],
            relationship_updates: vec![],
            relationship_snapshot: None,
        };
        let act_result = ActResult { executions: vec![] };
        let reflection = ReflectionResult {
            action_success_rate: f64::NAN,
            observations: vec![],
            concerns: vec![],
            task_updates: vec![],
            working_memory_ops: vec![],
            should_replan: false,
            llm_call_record: None,
        };
        (decision, perception, alignment, act_result, reflection)
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

    // ── T-9 Tests: Amend Step ──

    #[test]
    fn amend_increments_tick_number() {
        let dir = tempfile::tempdir().unwrap();
        let (kernel, _inbox) = test_kernel(dir.path());
        let tick_id = TickId::new();
        let tick_number = 42;
        let started_at = Utc::now();
        let snapshot_before = test_snapshot(kernel.vessel_id);
        let snapshot_before_artifact_id = ArtifactId::from_content(b"snap-before");
        let (decision, perception, alignment, act_result, reflection) = test_params();

        let _output = amend(
            &kernel,
            tick_id,
            tick_number,
            started_at,
            &snapshot_before,
            snapshot_before_artifact_id,
            &perception,
            &decision,
            &alignment,
            &act_result,
            &reflection,
            None,
        )
        .unwrap();

        // The new snapshot saved to SnapshotStore should have the passed tick_number
        let saved = kernel.snapshot_store.latest().unwrap().unwrap();
        assert_eq!(saved.tick_number, tick_number);
    }

    #[test]
    fn amend_applies_plan_update() {
        let dir = tempfile::tempdir().unwrap();
        let (kernel, _inbox) = test_kernel(dir.path());
        let tick_id = TickId::new();
        let snapshot_before = test_snapshot(kernel.vessel_id);
        let snapshot_before_artifact_id = ArtifactId::from_content(b"snap-before");
        let (mut decision, perception, alignment, act_result, reflection) = test_params();
        decision.snapshot_delta.plan_update = Some(exoskeleton_core::PlanUpdate::Replace {
            plan: exoskeleton_core::Plan::from_legacy_string("Updated plan from decide".into()),
        });

        amend(
            &kernel,
            tick_id,
            1,
            Utc::now(),
            &snapshot_before,
            snapshot_before_artifact_id,
            &perception,
            &decision,
            &alignment,
            &act_result,
            &reflection,
            None,
        )
        .unwrap();

        let saved = kernel.snapshot_store.latest().unwrap().unwrap();
        let plan = saved.plan.unwrap();
        assert_eq!(plan.objective, "Updated plan from decide");
    }

    #[test]
    fn amend_applies_working_memory_ops() {
        let dir = tempfile::tempdir().unwrap();
        let (kernel, _inbox) = test_kernel(dir.path());
        let tick_id = TickId::new();
        let snapshot_before = test_snapshot(kernel.vessel_id);
        let snapshot_before_artifact_id = ArtifactId::from_content(b"snap-before");
        let (mut decision, perception, alignment, act_result, reflection) = test_params();
        decision.snapshot_delta.working_memory_ops =
            Some(vec![exoskeleton_core::WorkingMemoryOp::Set {
                key: "context".into(),
                value: "New working context".into(),
                ttl_ticks: None,
            }]);

        amend(
            &kernel,
            tick_id,
            1,
            Utc::now(),
            &snapshot_before,
            snapshot_before_artifact_id,
            &perception,
            &decision,
            &alignment,
            &act_result,
            &reflection,
            None,
        )
        .unwrap();

        let saved = kernel.snapshot_store.latest().unwrap().unwrap();
        assert_eq!(saved.working_memory.entries.len(), 1);
        assert_eq!(saved.working_memory.entries[0].value, "New working context");
    }

    #[test]
    fn amend_prunes_task_working_memory_for_bounded_coding_idle() {
        let dir = tempfile::tempdir().unwrap();
        let (mut kernel, _inbox) = test_kernel(dir.path());
        kernel.coding_thread_config.return_to_idle_after_completion = true;
        let thread_id = exoskeleton_core::ThreadId::new();
        kernel
            .exec_thread_registry
            .register(exoskeleton_core::ExecThreadSpec {
                thread_id,
                kind: exoskeleton_core::ExecThreadKind::Coding,
                name: "Coding".into(),
                charter: "charter".into(),
                token_budget: 1000,
                workspace_root: None,
            })
            .unwrap();
        kernel
            .exec_thread_registry
            .update_status(thread_id, exoskeleton_core::ExecThreadStatus::Idle)
            .unwrap();
        kernel
            .exec_thread_registry
            .save_local_state(
                thread_id,
                &exoskeleton_core::ExecThreadLocalState {
                    last_completion_reason: Some("completed".into()),
                    ..Default::default()
                },
            )
            .unwrap();

        let tick_id = TickId::new();
        let mut snapshot_before = test_snapshot(kernel.vessel_id);
        snapshot_before
            .working_memory
            .apply_op(
                &exoskeleton_core::WorkingMemoryOp::Set {
                    key: "session_status".into(),
                    value: "awaiting_user_direction".into(),
                    ttl_ticks: None,
                },
                0,
            );
        snapshot_before
            .working_memory
            .apply_op(
                &exoskeleton_core::WorkingMemoryOp::Set {
                    key: "task_ready".into(),
                    value: "ready".into(),
                    ttl_ticks: None,
                },
                0,
            );
        let snapshot_before_artifact_id = ArtifactId::from_content(b"snap-before");
        let (mut decision, perception, alignment, act_result, reflection) = test_params();
        decision.snapshot_delta.working_memory_ops =
            Some(vec![exoskeleton_core::WorkingMemoryOp::Set {
                key: "lib_rs_content".into(),
                value: "stale".into(),
                ttl_ticks: None,
            }]);

        amend(
            &kernel,
            tick_id,
            1,
            Utc::now(),
            &snapshot_before,
            snapshot_before_artifact_id,
            &perception,
            &decision,
            &alignment,
            &act_result,
            &reflection,
            None,
        )
        .unwrap();

        let saved = kernel.snapshot_store.latest().unwrap().unwrap();
        let keys: Vec<&str> = saved
            .working_memory
            .entries
            .iter()
            .map(|entry| entry.key.as_str())
            .collect();
        assert_eq!(keys, vec!["session_status"]);
    }

    #[test]
    fn amend_saves_snapshot_to_store() {
        let dir = tempfile::tempdir().unwrap();
        let (kernel, _inbox) = test_kernel(dir.path());
        let tick_id = TickId::new();
        let tick_number = 7;
        let snapshot_before = test_snapshot(kernel.vessel_id);
        let snapshot_before_artifact_id = ArtifactId::from_content(b"snap-before");
        let (decision, perception, alignment, act_result, reflection) = test_params();

        amend(
            &kernel,
            tick_id,
            tick_number,
            Utc::now(),
            &snapshot_before,
            snapshot_before_artifact_id,
            &perception,
            &decision,
            &alignment,
            &act_result,
            &reflection,
            None,
        )
        .unwrap();

        // SnapshotStore.latest() should return the new snapshot
        let latest = kernel.snapshot_store.latest().unwrap();
        assert!(latest.is_some());
        let snap = latest.unwrap();
        assert_eq!(snap.tick_number, tick_number);
        assert_eq!(snap.status, VesselStatus::Idle);
        assert_eq!(snap.vessel_id, kernel.vessel_id);
    }

    #[test]
    fn amend_saves_tick_record_to_store() {
        let dir = tempfile::tempdir().unwrap();
        let (kernel, _inbox) = test_kernel(dir.path());
        let tick_id = TickId::new();
        let tick_number = 3;
        let started_at = Utc::now();
        let snapshot_before = test_snapshot(kernel.vessel_id);
        let snapshot_before_artifact_id = ArtifactId::from_content(b"snap-before");
        let (decision, perception, alignment, act_result, reflection) = test_params();

        amend(
            &kernel,
            tick_id,
            tick_number,
            started_at,
            &snapshot_before,
            snapshot_before_artifact_id,
            &perception,
            &decision,
            &alignment,
            &act_result,
            &reflection,
            None,
        )
        .unwrap();

        // TickStore.latest() should return the record
        let latest = kernel.tick_store.latest().unwrap();
        assert!(latest.is_some());
        let record = latest.unwrap();
        assert_eq!(record.tick_id, tick_id);
        assert_eq!(record.tick_number, tick_number);
        assert_eq!(record.phase, TickPhase::Amend);
        assert!(record.completed_at.is_some());
        assert_eq!(record.decision_rationale, Some("test reasoning".into()));
    }

    #[test]
    fn amend_stores_snapshot_artifact() {
        let dir = tempfile::tempdir().unwrap();
        let (kernel, _inbox) = test_kernel(dir.path());
        let tick_id = TickId::new();
        let snapshot_before = test_snapshot(kernel.vessel_id);
        let snapshot_before_artifact_id = ArtifactId::from_content(b"snap-before");
        let (decision, perception, alignment, act_result, reflection) = test_params();

        amend(
            &kernel,
            tick_id,
            1,
            Utc::now(),
            &snapshot_before,
            snapshot_before_artifact_id,
            &perception,
            &decision,
            &alignment,
            &act_result,
            &reflection,
            None,
        )
        .unwrap();

        // I3: ArtifactKind::Snapshot artifact must exist in store
        let snapshots = kernel
            .artifact_store
            .list_by_kind(ArtifactKind::Snapshot, 10)
            .unwrap();
        assert!(
            !snapshots.is_empty(),
            "expected at least one Snapshot artifact"
        );
        // Verify the artifact content is valid JSON representing a StateSnapshot
        let artifact = kernel
            .artifact_store
            .get(&snapshots[0].id)
            .unwrap()
            .unwrap();
        assert_eq!(artifact.kind, ArtifactKind::Snapshot);
        let parsed: StateSnapshot = serde_json::from_slice(&artifact.content).unwrap();
        assert_eq!(parsed.tick_number, 1);
    }

    #[test]
    fn amend_stores_tick_record_artifact() {
        let dir = tempfile::tempdir().unwrap();
        let (kernel, _inbox) = test_kernel(dir.path());
        let tick_id = TickId::new();
        let snapshot_before = test_snapshot(kernel.vessel_id);
        let snapshot_before_artifact_id = ArtifactId::from_content(b"snap-before");
        let (decision, perception, alignment, act_result, reflection) = test_params();

        amend(
            &kernel,
            tick_id,
            1,
            Utc::now(),
            &snapshot_before,
            snapshot_before_artifact_id,
            &perception,
            &decision,
            &alignment,
            &act_result,
            &reflection,
            None,
        )
        .unwrap();

        // I3: ArtifactKind::Tick artifact must exist in store
        let tick_artifacts = kernel
            .artifact_store
            .list_by_kind(ArtifactKind::Tick, 10)
            .unwrap();
        assert!(
            !tick_artifacts.is_empty(),
            "expected at least one Tick artifact"
        );
        let artifact = kernel
            .artifact_store
            .get(&tick_artifacts[0].id)
            .unwrap()
            .unwrap();
        assert_eq!(artifact.kind, ArtifactKind::Tick);
        // Verify the artifact content is valid JSON representing a TickRecord
        let parsed: TickRecord = serde_json::from_slice(&artifact.content).unwrap();
        assert_eq!(parsed.tick_id, tick_id);
    }

    #[test]
    fn amend_acknowledges_inbox_messages() {
        let dir = tempfile::tempdir().unwrap();
        let (kernel, inbox) = test_kernel(dir.path());
        let tick_id = TickId::new();
        let snapshot_before = test_snapshot(kernel.vessel_id);
        let snapshot_before_artifact_id = ArtifactId::from_content(b"snap-before");
        let (decision, mut perception, alignment, act_result, reflection) = test_params();

        // Push two messages into the inbox and include them in perception
        let env1 = test_envelope();
        let env2 = test_envelope();
        let id1 = env1.id;
        let id2 = env2.id;
        inbox.push(env1.clone());
        inbox.push(env2.clone());
        perception.new_messages = vec![env1, env2];

        amend(
            &kernel,
            tick_id,
            1,
            Utc::now(),
            &snapshot_before,
            snapshot_before_artifact_id,
            &perception,
            &decision,
            &alignment,
            &act_result,
            &reflection,
            None,
        )
        .unwrap();

        // InMemoryInbox.acknowledged_ids() should contain both message IDs
        let acked = inbox.acknowledged_ids();
        assert!(acked.contains(&id1), "message 1 should be acknowledged");
        assert!(acked.contains(&id2), "message 2 should be acknowledged");

        // receive() should return empty since both are acknowledged
        let remaining = kernel.inbox.receive().unwrap();
        assert!(
            remaining.is_empty(),
            "no pending messages after acknowledge"
        );
    }

    #[test]
    fn amend_persists_memory_notes() {
        let dir = tempfile::tempdir().unwrap();
        let (kernel, _inbox) = test_kernel(dir.path());
        let tick_id = TickId::new();
        let snapshot_before = test_snapshot(kernel.vessel_id);
        let snapshot_before_artifact_id = ArtifactId::from_content(b"snap-before");
        let (mut decision, perception, alignment, act_result, reflection) = test_params();
        decision.memory_notes = vec![
            "Learned something important".into(),
            "Another observation".into(),
        ];

        amend(
            &kernel,
            tick_id,
            5,
            Utc::now(),
            &snapshot_before,
            snapshot_before_artifact_id,
            &perception,
            &decision,
            &alignment,
            &act_result,
            &reflection,
            None,
        )
        .unwrap();

        // Memory notes should be persisted as episodic summaries
        let episodics = kernel.memory_store.recent_episodic(10).unwrap();
        assert_eq!(episodics.len(), 2);
        let summaries: Vec<&str> = episodics.iter().map(|e| e.summary.as_str()).collect();
        assert!(summaries.contains(&"Learned something important"));
        assert!(summaries.contains(&"Another observation"));
    }

    #[test]
    fn amend_logs_tick_completed_event() {
        let dir = tempfile::tempdir().unwrap();
        let (kernel, _inbox) = test_kernel(dir.path());
        let tick_id = TickId::new();
        let tick_number = 10;
        let snapshot_before = test_snapshot(kernel.vessel_id);
        let snapshot_before_artifact_id = ArtifactId::from_content(b"snap-before");
        let (decision, perception, alignment, act_result, reflection) = test_params();

        amend(
            &kernel,
            tick_id,
            tick_number,
            Utc::now(),
            &snapshot_before,
            snapshot_before_artifact_id,
            &perception,
            &decision,
            &alignment,
            &act_result,
            &reflection,
            None,
        )
        .unwrap();

        // EventLedger should have a TickCompleted event for this tick
        let events = kernel.event_ledger.for_tick(tick_id).unwrap();
        let completed_events: Vec<_> = events
            .iter()
            .filter(|e| e.event_type == EventType::TickCompleted)
            .collect();
        assert_eq!(completed_events.len(), 1);
        assert!(completed_events[0]
            .summary
            .contains(&format!("Tick {tick_number}")));
    }

    #[test]
    fn amend_returns_success_output() {
        let dir = tempfile::tempdir().unwrap();
        let (kernel, _inbox) = test_kernel(dir.path());
        let tick_id = TickId::new();
        let tick_number = 1;
        let snapshot_before = test_snapshot(kernel.vessel_id);
        let snapshot_before_artifact_id = ArtifactId::from_content(b"snap-before");
        let (mut decision, perception, alignment, mut act_result, reflection) = test_params();

        // Add an execution to verify the output JSON
        let exec = ActionExecution {
            action: super::super::types::PlannedAction {
                call_id: "call_delay".into(),
                tool_name: "delay".into(),
                params: serde_json::json!({"duration_ms": 10}),
                rationale: "test".into(),
                plan_task_id: None,
                origin_exec_thread_id: None,
                proposal_id: None,
            },
            result: Ok(serde_json::json!({"slept_ms": 10})),
            record: exoskeleton_core::tick::ActionRecord {
                action_type: "delay".into(),
                target: "{}".into(),
                receipt_ref: None,
                outcome: exoskeleton_core::tick::ActionOutcome::Success,
                origin_exec_thread_id: None,
                proposal_id: None,
            },
            tool_result: exoskeleton_core::llm::ContentBlock::ToolResult {
                tool_use_id: "call_delay".into(),
                content: "{\"slept_ms\":10}".into(),
                is_error: false,
            },
            pending_question: false,
            code_diff: None,
        };
        act_result.executions.push(exec);
        decision.snapshot_delta = SnapshotDelta::default();

        let output = amend(
            &kernel,
            tick_id,
            tick_number,
            Utc::now(),
            &snapshot_before,
            snapshot_before_artifact_id,
            &perception,
            &decision,
            &alignment,
            &act_result,
            &reflection,
            None,
        )
        .unwrap();

        // HandlerOutput should be Success variant
        match output {
            HandlerOutput::Success { output, .. } => {
                let bytes = output.expect("should have output bytes");
                let parsed: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
                assert_eq!(parsed["tick_number"], 1);
                assert_eq!(parsed["actions"], 1);
            }
            other => panic!("expected HandlerOutput::Success, got {other:?}"),
        }
    }

    // ── T-2: Memory Consolidation Post-Processing Tests ──

    #[test]
    fn amend_processes_memory_consolidation_episodic_summary() {
        let dir = tempfile::tempdir().unwrap();
        let (kernel, _inbox) = test_kernel(dir.path());
        let tick_id = TickId::new();
        let tick_number = 5;
        let snapshot_before = test_snapshot(kernel.vessel_id);
        let snapshot_before_artifact_id = ArtifactId::from_content(b"snap-before");
        let (decision, mut perception, alignment, act_result, reflection) = test_params();

        // Register Memory Consolidation thread and simulate its output
        use exoskeleton_threads::builtin::memory_consolidation;
        let mc_spec = memory_consolidation::spec();
        kernel.thread_registry.register(mc_spec.clone()).unwrap();

        // Create a ThreadOutput with a summary that references the MC thread
        let mc_summary = "Consolidated ticks 1-5 into episodic memory";
        let mc_output = exoskeleton_core::ThreadOutput {
            thread_id: exoskeleton_threads::MEMORY_CONSOLIDATION_ID,
            tick_id,
            artifact_id: ArtifactId::from_content(mc_summary.as_bytes()),
            summary: mc_summary.into(),
            recommendations: vec![],
        };
        kernel.thread_registry.save_output(&mc_output).unwrap();

        // Add a ThreadContribution for Memory Consolidation
        perception.thread_outputs = vec![exoskeleton_core::tick::ThreadContribution {
            thread_id: exoskeleton_threads::MEMORY_CONSOLIDATION_ID,
            artifact_id: mc_output.artifact_id.clone(),
            summary: mc_summary.into(),
        }];

        amend(
            &kernel,
            tick_id,
            tick_number,
            Utc::now(),
            &snapshot_before,
            snapshot_before_artifact_id,
            &perception,
            &decision,
            &alignment,
            &act_result,
            &reflection,
            None,
        )
        .unwrap();

        // Verify episodic summary was written to MemoryStore
        let episodics = kernel.memory_store.recent_episodic(10).unwrap();
        assert!(
            !episodics.is_empty(),
            "expected at least one episodic summary from Memory Consolidation"
        );
        assert!(
            episodics
                .iter()
                .any(|e| e.summary.contains("Consolidated ticks")),
            "episodic summary should contain MC output"
        );
    }

    #[test]
    fn amend_processes_memory_consolidation_long_term_notes() {
        let dir = tempfile::tempdir().unwrap();
        let (kernel, _inbox) = test_kernel(dir.path());
        let tick_id = TickId::new();
        let tick_number = 5;
        let snapshot_before = test_snapshot(kernel.vessel_id);
        let snapshot_before_artifact_id = ArtifactId::from_content(b"snap-before");
        let (decision, mut perception, alignment, act_result, reflection) = test_params();

        // Register MC thread
        use exoskeleton_threads::builtin::memory_consolidation;
        let mc_spec = memory_consolidation::spec();
        kernel.thread_registry.register(mc_spec.clone()).unwrap();

        // Create a valid MemoryConsolidation JSON and store it as an artifact
        let mc_json = r#"{"episodic_summary":"Ticks 1-5 summary","long_term_notes":[{"topic":"strategy","content":"Incremental approach works","tags":["reliability"]}],"deprecated_notes":[]}"#;
        let mc_artifact = exoskeleton_core::Artifact::from_json(
            ArtifactKind::ThreadOutput,
            &serde_json::from_str::<serde_json::Value>(mc_json).unwrap(),
        )
        .unwrap();
        let artifact_id = kernel.artifact_store.put(&mc_artifact).unwrap();

        // Create a ThreadOutput pointing to the artifact
        let mc_output = exoskeleton_core::ThreadOutput {
            thread_id: exoskeleton_threads::MEMORY_CONSOLIDATION_ID,
            tick_id,
            artifact_id: artifact_id.clone(),
            summary: "Consolidated".into(),
            recommendations: vec![],
        };
        kernel.thread_registry.save_output(&mc_output).unwrap();

        perception.thread_outputs = vec![exoskeleton_core::tick::ThreadContribution {
            thread_id: exoskeleton_threads::MEMORY_CONSOLIDATION_ID,
            artifact_id: artifact_id.clone(),
            summary: "Consolidated".into(),
        }];

        amend(
            &kernel,
            tick_id,
            tick_number,
            Utc::now(),
            &snapshot_before,
            snapshot_before_artifact_id,
            &perception,
            &decision,
            &alignment,
            &act_result,
            &reflection,
            None,
        )
        .unwrap();

        // Verify long-term note was written
        let lt_notes = kernel.memory_store.all_long_term(10).unwrap();
        assert!(
            lt_notes.iter().any(|n| n.topic == "strategy"),
            "expected long-term note with topic 'strategy'"
        );
    }

    #[test]
    fn amend_handles_invalid_memory_consolidation_json() {
        let dir = tempfile::tempdir().unwrap();
        let (kernel, _inbox) = test_kernel(dir.path());
        let tick_id = TickId::new();
        let tick_number = 5;
        let snapshot_before = test_snapshot(kernel.vessel_id);
        let snapshot_before_artifact_id = ArtifactId::from_content(b"snap-before");
        let (decision, mut perception, alignment, act_result, reflection) = test_params();

        // Register MC thread
        use exoskeleton_threads::builtin::memory_consolidation;
        kernel
            .thread_registry
            .register(memory_consolidation::spec())
            .unwrap();

        let mc_output = exoskeleton_core::ThreadOutput {
            thread_id: exoskeleton_threads::MEMORY_CONSOLIDATION_ID,
            tick_id,
            artifact_id: ArtifactId::from_content(b"bad-mc-output"),
            summary: "not valid json for MemoryConsolidation".into(),
            recommendations: vec![],
        };
        kernel.thread_registry.save_output(&mc_output).unwrap();

        perception.thread_outputs = vec![exoskeleton_core::tick::ThreadContribution {
            thread_id: exoskeleton_threads::MEMORY_CONSOLIDATION_ID,
            artifact_id: mc_output.artifact_id.clone(),
            summary: "not valid".into(),
        }];

        // Should NOT crash — graceful degradation
        let result = amend(
            &kernel,
            tick_id,
            tick_number,
            Utc::now(),
            &snapshot_before,
            snapshot_before_artifact_id,
            &perception,
            &decision,
            &alignment,
            &act_result,
            &reflection,
            None,
        );
        assert!(
            result.is_ok(),
            "amend should succeed even with invalid MC output"
        );
    }

    #[test]
    fn amend_no_memory_writes_without_mc_contribution() {
        let dir = tempfile::tempdir().unwrap();
        let (kernel, _inbox) = test_kernel(dir.path());
        let tick_id = TickId::new();
        let tick_number = 1;
        let snapshot_before = test_snapshot(kernel.vessel_id);
        let snapshot_before_artifact_id = ArtifactId::from_content(b"snap-before");
        let (decision, perception, alignment, act_result, reflection) = test_params();

        // No MC contribution in perception.thread_outputs (empty by default)
        amend(
            &kernel,
            tick_id,
            tick_number,
            Utc::now(),
            &snapshot_before,
            snapshot_before_artifact_id,
            &perception,
            &decision,
            &alignment,
            &act_result,
            &reflection,
            None,
        )
        .unwrap();

        // No episodic summaries from MC (there may be some from decision.memory_notes)
        let episodics = kernel.memory_store.recent_episodic(10).unwrap();
        // memory_notes is empty in test_params, so no episodics at all
        assert!(episodics.is_empty());
    }

    // ── E1-S3 W-16: Episodic Memory Eviction (Amend-Level) ──

    /// E1-T81: Eviction runs AFTER Memory Consolidation outputs are processed,
    /// proving the consolidation-before-eviction ordering guarantee.
    #[test]
    fn amend_eviction_runs_after_mc_outputs_processed() {
        let dir = tempfile::tempdir().unwrap();
        let (mut kernel, _inbox) = test_kernel(dir.path());
        kernel.episodic_memory_capacity = Some(5);

        // Pre-populate 5 episodic entries (ticks 1-5)
        for tick in 1..=5u64 {
            let entry = EpisodicSummary {
                id: ArtifactId::from_content(format!("pre-existing-{tick}").as_bytes()),
                start_tick: tick,
                end_tick: tick,
                summary: format!("Pre-existing entry {tick}"),
                key_events: vec![],
                token_count: 10,
                created_at: Utc::now(),
            };
            kernel.memory_store.write_episodic(&entry).unwrap();
        }
        assert_eq!(kernel.memory_store.count_episodic().unwrap(), 5);

        // Set up MC contribution that will write an episodic summary
        let tick_id = TickId::new();
        let tick_number = 6;
        let snapshot_before = test_snapshot(kernel.vessel_id);
        let snapshot_before_artifact_id = ArtifactId::from_content(b"snap-before");
        let (decision, mut perception, alignment, act_result, reflection) = test_params();

        use exoskeleton_threads::builtin::memory_consolidation;
        kernel
            .thread_registry
            .register(memory_consolidation::spec())
            .unwrap();

        let mc_summary = "MC consolidated ticks 1-5";
        let mc_output = exoskeleton_core::ThreadOutput {
            thread_id: exoskeleton_threads::MEMORY_CONSOLIDATION_ID,
            tick_id,
            artifact_id: ArtifactId::from_content(mc_summary.as_bytes()),
            summary: mc_summary.into(),
            recommendations: vec![],
        };
        kernel.thread_registry.save_output(&mc_output).unwrap();

        perception.thread_outputs = vec![exoskeleton_core::tick::ThreadContribution {
            thread_id: exoskeleton_threads::MEMORY_CONSOLIDATION_ID,
            artifact_id: mc_output.artifact_id.clone(),
            summary: mc_summary.into(),
        }];

        amend(
            &kernel,
            tick_id,
            tick_number,
            Utc::now(),
            &snapshot_before,
            snapshot_before_artifact_id,
            &perception,
            &decision,
            &alignment,
            &act_result,
            &reflection,
            None,
        )
        .unwrap();

        // MC added 1 episodic entry → 6 total → capacity 5 → 1 evicted
        // The MC entry must survive (consolidation happened before eviction)
        let episodics = kernel.memory_store.recent_episodic(10).unwrap();
        assert_eq!(episodics.len(), 5, "should be at capacity after eviction");
        assert!(
            episodics
                .iter()
                .any(|e| e.summary.contains("MC consolidated")),
            "MC episodic summary must survive eviction (consolidation-before-eviction)"
        );
        // Oldest pre-existing entry (tick 1) should have been evicted
        assert!(
            !episodics
                .iter()
                .any(|e| e.summary == "Pre-existing entry 1"),
            "oldest entry should be evicted"
        );
    }

    /// E1-T82: EpisodicEvicted event is logged with the correct eviction count.
    #[test]
    fn amend_logs_episodic_evicted_event_with_count() {
        let dir = tempfile::tempdir().unwrap();
        let (mut kernel, _inbox) = test_kernel(dir.path());
        kernel.episodic_memory_capacity = Some(3);

        // Pre-populate 5 episodic entries
        for tick in 1..=5u64 {
            let entry = EpisodicSummary {
                id: ArtifactId::from_content(format!("entry-{tick}").as_bytes()),
                start_tick: tick,
                end_tick: tick,
                summary: format!("Entry {tick}"),
                key_events: vec![],
                token_count: 10,
                created_at: Utc::now(),
            };
            kernel.memory_store.write_episodic(&entry).unwrap();
        }

        let tick_id = TickId::new();
        let snapshot_before = test_snapshot(kernel.vessel_id);
        let snapshot_before_artifact_id = ArtifactId::from_content(b"snap-before");
        let (decision, perception, alignment, act_result, reflection) = test_params();

        amend(
            &kernel,
            tick_id,
            1,
            Utc::now(),
            &snapshot_before,
            snapshot_before_artifact_id,
            &perception,
            &decision,
            &alignment,
            &act_result,
            &reflection,
            None,
        )
        .unwrap();

        // Should have evicted 2 entries (5 - 3 = 2)
        assert_eq!(kernel.memory_store.count_episodic().unwrap(), 3);

        // EpisodicEvicted event should be in the ledger
        let events = kernel.event_ledger.for_tick(tick_id).unwrap();
        let evicted_events: Vec<_> = events
            .iter()
            .filter(|e| e.event_type == EventType::EpisodicEvicted)
            .collect();
        assert_eq!(evicted_events.len(), 1, "exactly one EpisodicEvicted event");
        assert!(
            evicted_events[0].summary.contains("2"),
            "summary should mention 2 evicted entries, got: {}",
            evicted_events[0].summary
        );
    }

    /// E1-T83: No eviction occurs when episodic_memory_capacity is None.
    #[test]
    fn amend_no_eviction_when_capacity_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let (kernel, _inbox) = test_kernel(dir.path());
        // episodic_memory_capacity defaults to None in test_kernel

        // Pre-populate 100 episodic entries
        for tick in 1..=100u64 {
            let entry = EpisodicSummary {
                id: ArtifactId::from_content(format!("bulk-{tick}").as_bytes()),
                start_tick: tick,
                end_tick: tick,
                summary: format!("Bulk entry {tick}"),
                key_events: vec![],
                token_count: 10,
                created_at: Utc::now(),
            };
            kernel.memory_store.write_episodic(&entry).unwrap();
        }

        let tick_id = TickId::new();
        let snapshot_before = test_snapshot(kernel.vessel_id);
        let snapshot_before_artifact_id = ArtifactId::from_content(b"snap-before");
        let (decision, perception, alignment, act_result, reflection) = test_params();

        amend(
            &kernel,
            tick_id,
            1,
            Utc::now(),
            &snapshot_before,
            snapshot_before_artifact_id,
            &perception,
            &decision,
            &alignment,
            &act_result,
            &reflection,
            None,
        )
        .unwrap();

        // All 100 entries should remain
        assert_eq!(kernel.memory_store.count_episodic().unwrap(), 100);

        // No EpisodicEvicted events
        let events = kernel.event_ledger.for_tick(tick_id).unwrap();
        assert!(
            !events
                .iter()
                .any(|e| e.event_type == EventType::EpisodicEvicted),
            "no eviction events when capacity is None"
        );
    }

    // ── OA-T10: Amend stores reply artifact when decision.reply is Some ──
    #[test]
    fn amend_stores_reply_artifact() {
        let dir = tempfile::tempdir().unwrap();
        let (kernel, _inbox) = test_kernel(dir.path());
        let tick_id = TickId::new();
        let snapshot_before = test_snapshot(kernel.vessel_id);
        let snapshot_before_artifact_id = ArtifactId::from_content(b"snap-before");
        let (mut decision, perception, alignment, act_result, reflection) = test_params();
        decision.reply = Some("Hello, I'm your vessel!".into());

        amend(
            &kernel,
            tick_id,
            1,
            Utc::now(),
            &snapshot_before,
            snapshot_before_artifact_id,
            &perception,
            &decision,
            &alignment,
            &act_result,
            &reflection,
            None,
        )
        .unwrap();

        // The reply content should be retrievable as an artifact
        let reply_artifact_id = ArtifactId::from_content(b"Hello, I'm your vessel!");
        let artifact = kernel.artifact_store.get(&reply_artifact_id).unwrap();
        assert!(artifact.is_some(), "reply artifact should exist");
        let artifact = artifact.unwrap();
        assert_eq!(artifact.kind, ArtifactKind::Envelope);
        assert_eq!(
            String::from_utf8_lossy(&artifact.content),
            "Hello, I'm your vessel!"
        );
    }

    // ── OA-T11: Amend appends vessel ConversationMessage to active conversation ──
    #[test]
    fn amend_appends_vessel_reply_to_conversation() {
        use exoskeleton_core::conversation::Conversation;

        let dir = tempfile::tempdir().unwrap();
        let (kernel, _inbox) = test_kernel(dir.path());
        let tick_id = TickId::new();
        let snapshot_before = test_snapshot(kernel.vessel_id);
        let snapshot_before_artifact_id = ArtifactId::from_content(b"snap-before");
        let (mut decision, mut perception, alignment, act_result, reflection) = test_params();
        decision.reply = Some("I see your message.".into());

        // Create an active conversation
        let user_principal = PrincipalId::new();
        let conv = Conversation::from_first_message(
            user_principal,
            EnvelopeId::new(),
            ArtifactId::from_content(b"user-msg"),
            Utc::now(),
        );
        kernel.conversation_store.save(&conv).unwrap();
        perception.active_conversations = vec![conv.clone()];

        amend(
            &kernel,
            tick_id,
            1,
            Utc::now(),
            &snapshot_before,
            snapshot_before_artifact_id,
            &perception,
            &decision,
            &alignment,
            &act_result,
            &reflection,
            None,
        )
        .unwrap();

        // Conversation should now have 2 messages (user + vessel)
        let updated = kernel.conversation_store.get(conv.id).unwrap().unwrap();
        assert_eq!(updated.message_refs.len(), 2);
        let vessel_principal = PrincipalId::from(*kernel.vessel_id.as_ref());
        assert_eq!(updated.message_refs[1].source, vessel_principal);
    }

    // ── OA-T12: Amend emits VesselResponseSent event and LiveEvent ──
    #[test]
    fn amend_emits_vessel_response_sent_event() {
        let dir = tempfile::tempdir().unwrap();
        let (kernel, _inbox) = test_kernel(dir.path());
        let mut rx = kernel.event_tx.subscribe();
        let tick_id = TickId::new();
        let snapshot_before = test_snapshot(kernel.vessel_id);
        let snapshot_before_artifact_id = ArtifactId::from_content(b"snap-before");
        let (mut decision, perception, alignment, act_result, reflection) = test_params();
        decision.reply = Some("My response".into());

        amend(
            &kernel,
            tick_id,
            1,
            Utc::now(),
            &snapshot_before,
            snapshot_before_artifact_id,
            &perception,
            &decision,
            &alignment,
            &act_result,
            &reflection,
            None,
        )
        .unwrap();

        // Event ledger should have VesselResponseSent
        let events = kernel.event_ledger.for_tick(tick_id).unwrap();
        let response_events: Vec<_> = events
            .iter()
            .filter(|e| e.event_type == EventType::VesselResponseSent)
            .collect();
        assert_eq!(response_events.len(), 1);
        assert!(response_events[0].summary.contains("Vessel reply"));
        assert!(response_events[0].payload_ref.is_some());

        // Broadcast channel should contain VesselResponseSent
        let mut found_response = false;
        while let Ok(event) = rx.try_recv() {
            if event.event_type == EventType::VesselResponseSent {
                found_response = true;
                assert!(event.summary.contains("Vessel reply"));
            }
        }
        assert!(
            found_response,
            "VesselResponseSent LiveEvent should be broadcast"
        );
    }

    // ── OA-T13: Amend no-ops when decision.reply is None ──
    #[test]
    fn amend_no_reply_when_none() {
        let dir = tempfile::tempdir().unwrap();
        let (kernel, _inbox) = test_kernel(dir.path());
        let tick_id = TickId::new();
        let snapshot_before = test_snapshot(kernel.vessel_id);
        let snapshot_before_artifact_id = ArtifactId::from_content(b"snap-before");
        let (decision, perception, alignment, act_result, reflection) = test_params();
        // decision.reply is None by default

        amend(
            &kernel,
            tick_id,
            1,
            Utc::now(),
            &snapshot_before,
            snapshot_before_artifact_id,
            &perception,
            &decision,
            &alignment,
            &act_result,
            &reflection,
            None,
        )
        .unwrap();

        // No VesselResponseSent events
        let events = kernel.event_ledger.for_tick(tick_id).unwrap();
        assert!(
            !events
                .iter()
                .any(|e| e.event_type == EventType::VesselResponseSent),
            "no reply event when decision.reply is None"
        );
    }

    // ── OA-T14: Amend tolerates empty active_conversations when reply present ──
    #[test]
    fn amend_reply_with_no_active_conversations() {
        let dir = tempfile::tempdir().unwrap();
        let (kernel, _inbox) = test_kernel(dir.path());
        let tick_id = TickId::new();
        let snapshot_before = test_snapshot(kernel.vessel_id);
        let snapshot_before_artifact_id = ArtifactId::from_content(b"snap-before");
        let (mut decision, perception, alignment, act_result, reflection) = test_params();
        decision.reply = Some("Reply without conversation".into());
        // perception.active_conversations is empty by default

        amend(
            &kernel,
            tick_id,
            1,
            Utc::now(),
            &snapshot_before,
            snapshot_before_artifact_id,
            &perception,
            &decision,
            &alignment,
            &act_result,
            &reflection,
            None,
        )
        .unwrap();

        // Reply artifact and event should still exist even without a conversation
        let reply_artifact_id = ArtifactId::from_content(b"Reply without conversation");
        let artifact = kernel.artifact_store.get(&reply_artifact_id).unwrap();
        assert!(
            artifact.is_some(),
            "reply artifact stored even without conversation"
        );

        let events = kernel.event_ledger.for_tick(tick_id).unwrap();
        assert!(
            events
                .iter()
                .any(|e| e.event_type == EventType::VesselResponseSent),
            "reply event emitted even without active conversation"
        );
    }
}

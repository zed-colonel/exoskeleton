//! IntrospectionService — read-only query interface over the vessel's stores.
//!
//! Used by the multi-turn Decide step to resolve introspection queries
//! synchronously within the Cognitive AQ handler. All methods are &self
//! (no mutation). All reads are bounded by MAX_INTROSPECTION_LIMIT.

use chrono::Utc;
use exoskeleton_core::introspection::{IntrospectionQuery, MAX_INTROSPECTION_LIMIT};
use exoskeleton_core::ExoError;
use serde_json::Value;

use crate::kernel::KernelContext;

/// Read-only query interface over the vessel's stores.
///
/// Created at the start of `decide()` and dropped at the end. The lifetime
/// parameter ensures it cannot outlive the `KernelContext` reference.
pub struct IntrospectionService<'a> {
    kernel: &'a KernelContext,
}

impl<'a> IntrospectionService<'a> {
    pub fn new(kernel: &'a KernelContext) -> Self {
        Self { kernel }
    }

    /// Resolve a query and return the result as human-readable JSON.
    pub fn query(&self, q: &IntrospectionQuery) -> Result<Value, ExoError> {
        match q {
            IntrospectionQuery::TickHistory { limit } => self.tick_history(*limit),
            IntrospectionQuery::TickDetail { tick_id } => self.tick_detail(tick_id),
            IntrospectionQuery::EventHistory { event_type, limit } => {
                self.event_history(event_type.as_deref(), *limit)
            }
            IntrospectionQuery::TrustScores => self.trust_scores(),
            IntrospectionQuery::TrustHistory {
                principal_id,
                limit,
            } => self.trust_history(principal_id, *limit),
            IntrospectionQuery::BudgetStatus => self.budget_status(),
            IntrospectionQuery::ThreadStatus => self.thread_status(),
            IntrospectionQuery::MemorySearch { topic, tags } => {
                self.memory_search(topic.as_deref(), tags.as_deref())
            }
            IntrospectionQuery::ConnectorDetails { name } => self.connector_details(name),
            IntrospectionQuery::WatchList => self.watch_list(),
        }
    }

    fn tick_history(&self, limit: u32) -> Result<Value, ExoError> {
        let limit = limit.min(MAX_INTROSPECTION_LIMIT) as usize;
        // TickStore doesn't have a recent() method, so we use range() with
        // the latest tick number and work backward.
        let latest = self.kernel.tick_store.latest()?;
        let ticks = match latest {
            Some(ref latest_tick) => {
                let end = latest_tick.tick_number;
                let start = end.saturating_sub(limit as u64);
                let mut ticks = self.kernel.tick_store.range(start, end)?;
                ticks.reverse(); // newest first
                ticks.truncate(limit);
                ticks
            }
            None => vec![],
        };
        let summaries: Vec<Value> = ticks
            .iter()
            .map(|t| {
                let action_count = t.actions_taken.len();
                let success_count = t
                    .actions_taken
                    .iter()
                    .filter(|a| a.outcome == exoskeleton_core::ActionOutcome::Success)
                    .count();
                serde_json::json!({
                    "tick_number": t.tick_number,
                    "started_at": t.started_at.to_rfc3339(),
                    "actions_taken": action_count,
                    "actions_succeeded": success_count,
                    "llm_calls": t.llm_calls.len(),
                    "total_tokens": t.llm_calls.iter()
                        .map(|c| c.tokens_in + c.tokens_out).sum::<u64>(),
                })
            })
            .collect();
        Ok(Value::Array(summaries))
    }

    fn tick_detail(&self, tick_id: &str) -> Result<Value, ExoError> {
        let id: exoskeleton_core::TickId = tick_id
            .parse()
            .map_err(|_| ExoError::Config("invalid tick_id".into()))?;
        match self.kernel.tick_store.get(id)? {
            Some(tick) => Ok(serde_json::to_value(&tick)?),
            None => Ok(serde_json::json!({"error": "tick not found"})),
        }
    }

    fn event_history(&self, event_type: Option<&str>, limit: u32) -> Result<Value, ExoError> {
        let limit = limit.min(MAX_INTROSPECTION_LIMIT) as usize;
        let events = match event_type {
            Some(type_str) => {
                let et: exoskeleton_core::EventType =
                    serde_json::from_value(serde_json::Value::String(type_str.into()))
                        .map_err(|_| ExoError::Config(format!("unknown event type: {type_str}")))?;
                self.kernel.event_ledger.by_type(et, limit)?
            }
            None => self.kernel.event_ledger.recent(limit)?,
        };
        let summaries: Vec<Value> = events
            .iter()
            .map(|e| {
                serde_json::json!({
                    "id": e.id.to_string(),
                    "event_type": e.event_type,
                    "summary": e.summary,
                    "timestamp": e.timestamp.to_rfc3339(),
                    "tick_id": e.tick_id.map(|t| t.to_string()),
                    "payload_ref": e.payload_ref.as_ref().map(|r| r.to_string()),
                })
            })
            .collect();
        Ok(Value::Array(summaries))
    }

    fn trust_scores(&self) -> Result<Value, ExoError> {
        let snapshot = exoskeleton_relationship::compile_relationship_snapshot(
            self.kernel.relationship_ledger.as_ref(),
            self.kernel.trust_decay_config.as_ref(),
            Utc::now(),
        )?;
        let principals: Vec<Value> = snapshot
            .principals
            .iter()
            .map(|p| {
                serde_json::json!({
                    "principal_id": p.principal_id.to_string(),
                    "display_name": p.display_name,
                    "role": p.role,
                    "trust_level": p.trust_level,
                    "active_commitments": p.active_commitments,
                })
            })
            .collect();
        Ok(Value::Array(principals))
    }

    fn trust_history(&self, principal_id: &str, limit: u32) -> Result<Value, ExoError> {
        let pid: exoskeleton_core::PrincipalId = principal_id
            .parse()
            .map_err(|_| ExoError::Config("invalid principal_id".into()))?;
        let limit = limit.min(MAX_INTROSPECTION_LIMIT) as usize;
        let records = self.kernel.relationship_ledger.for_principal(pid, limit)?;
        let history: Vec<Value> = records
            .iter()
            .map(|r| {
                serde_json::json!({
                    "signal_type": r.signal_type,
                    "timestamp": r.timestamp.to_rfc3339(),
                    "metadata": r.metadata,
                })
            })
            .collect();
        Ok(Value::Array(history))
    }

    fn budget_status(&self) -> Result<Value, ExoError> {
        match &self.kernel.budget_tracker {
            Some(tracker) => {
                // Try-lock to avoid blocking. Budget reads are best-effort.
                match tracker.try_lock() {
                    Ok(guard) => {
                        let status = guard.budget_status(u64::MAX);
                        Ok(serde_json::to_value(&status)?)
                    }
                    Err(_) => Ok(serde_json::json!({"error": "budget tracker locked"})),
                }
            }
            None => Ok(serde_json::json!({"message": "no budget configured"})),
        }
    }

    fn thread_status(&self) -> Result<Value, ExoError> {
        let summaries = self.kernel.thread_registry.thread_summaries()?;
        Ok(serde_json::to_value(&summaries)?)
    }

    fn memory_search(
        &self,
        topic: Option<&str>,
        tags: Option<&[String]>,
    ) -> Result<Value, ExoError> {
        let long_term = self
            .kernel
            .memory_store
            .all_long_term(MAX_INTROSPECTION_LIMIT as usize)?;
        let filtered: Vec<&_> = long_term
            .iter()
            .filter(|note| {
                let topic_match = topic.is_none_or(|t| {
                    note.topic.to_lowercase().contains(&t.to_lowercase())
                        || note.content.to_lowercase().contains(&t.to_lowercase())
                });
                let tag_match = tags.is_none_or(|ts| ts.iter().any(|t| note.tags.contains(t)));
                topic_match && tag_match
            })
            .collect();
        Ok(serde_json::to_value(&filtered)?)
    }

    fn connector_details(&self, name: &str) -> Result<Value, ExoError> {
        let guard = self
            .kernel
            .wi_host_slot
            .try_lock()
            .map_err(|_| ExoError::Engine("WI host slot locked".into()))?;
        match guard.as_ref() {
            Some(host) => match host.describe(name) {
                Some(desc) => Ok(serde_json::to_value(&desc)?),
                None => Ok(serde_json::json!({"error": "connector not found"})),
            },
            None => Ok(serde_json::json!({"error": "WI host not ready"})),
        }
    }

    fn watch_list(&self) -> Result<Value, ExoError> {
        let watches = self.kernel.watch_store.list()?;
        let summaries: Vec<Value> = watches
            .iter()
            .map(|w| {
                serde_json::json!({
                    "id": w.id.to_string(),
                    "name": w.name,
                    "description": w.description,
                    "watch_type": w.watch_type,
                    "schedule": w.schedule,
                    "status": w.status,
                    "trigger_count": w.trigger_count,
                    "last_checked_tick": w.last_checked_tick,
                })
            })
            .collect();
        Ok(Value::Array(summaries))
    }

    /// Resolve an internal metric to a single f64 value.
    ///
    /// Used by the WatchExecutor for threshold watch evaluation.
    /// Not exposed as an LLM tool — watches use this internally.
    pub fn query_metric(
        &self,
        metric: &exoskeleton_core::watch::MetricKind,
    ) -> Result<f64, ExoError> {
        use exoskeleton_core::watch::MetricKind;
        match metric {
            MetricKind::TrustLevel { principal_id } => {
                let snapshot = exoskeleton_relationship::compile_relationship_snapshot(
                    self.kernel.relationship_ledger.as_ref(),
                    self.kernel.trust_decay_config.as_ref(),
                    Utc::now(),
                )?;
                let trust = snapshot
                    .principals
                    .iter()
                    .find(|p| p.principal_id == *principal_id)
                    .map(|p| p.trust_level)
                    .unwrap_or(0.5);
                Ok(trust)
            }
            MetricKind::BudgetRemaining { dimension } => match &self.kernel.budget_tracker {
                Some(tracker) => match tracker.try_lock() {
                    Ok(guard) => {
                        let status = guard.budget_status(u64::MAX);
                        let remaining = match dimension.as_str() {
                            "local_tokens" => status.local_tokens_remaining as f64,
                            "frontier_tokens" => status.frontier_tokens_remaining as f64,
                            "frontier_cost_cents" => status.frontier_cost_cents_remaining as f64,
                            "time_secs" => status.time_secs_remaining as f64,
                            "tool_invocations" => status.tool_invocations_remaining as f64,
                            _ => 0.0,
                        };
                        Ok(remaining)
                    }
                    Err(_) => Ok(f64::NAN),
                },
                None => Ok(f64::INFINITY),
            },
            MetricKind::ConsecutiveFailures => {
                let latest = self.kernel.tick_store.latest()?;
                let end = latest.as_ref().map(|t| t.tick_number).unwrap_or(0);
                let start = end.saturating_sub(50);
                let ticks = self.kernel.tick_store.range(start, end)?;
                let mut consecutive = 0u64;
                for tick in ticks.iter().rev() {
                    let all_failed = !tick.actions_taken.is_empty()
                        && tick
                            .actions_taken
                            .iter()
                            .all(|a| a.outcome != exoskeleton_core::ActionOutcome::Success);
                    if all_failed {
                        consecutive += 1;
                    } else {
                        break;
                    }
                }
                Ok(consecutive as f64)
            }
            MetricKind::TickDuration => {
                let latest = self.kernel.tick_store.latest()?;
                match latest {
                    Some(tick) => match tick.completed_at {
                        Some(completed) => {
                            let duration = completed - tick.started_at;
                            Ok(duration.num_milliseconds() as f64)
                        }
                        None => Ok(0.0),
                    },
                    None => Ok(0.0),
                }
            }
            MetricKind::EventCount {
                event_type,
                lookback_ticks,
            } => {
                let et: exoskeleton_core::EventType = serde_json::from_value(
                    serde_json::Value::String(event_type.clone()),
                )
                .map_err(|_| ExoError::Config(format!("unknown event type: {event_type}")))?;
                let limit = (*lookback_ticks as usize) * 10;
                let events = self.kernel.event_ledger.by_type(et, limit)?;
                Ok(events.len() as f64)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use exoskeleton_core::conversation::InMemoryConversationStore;
    use exoskeleton_core::introspection::IntrospectionQuery;
    use exoskeleton_core::prompt::PromptRegistry;
    use exoskeleton_core::tick::{LlmCallRecord, TickPhase};
    use exoskeleton_core::{
        ActionOutcome, ActionRecord, ArtifactId, EventEntry, EventType, LedgerEntryId, LiveEvent,
        LongTermNote, TickId, TickRecord, VesselId,
    };
    use exoskeleton_memory::{ApproximateTokenCounter, ContextCompiler};
    use exoskeleton_relationship::InMemoryRelationshipLedger;
    use exoskeleton_threads::{InMemoryThreadStore, ThreadRegistry};

    use super::*;
    use crate::inbox::InMemoryInbox;
    use crate::storage::StorageManager;

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
            read_paths_this_tick: Arc::new(std::sync::Mutex::new(std::collections::HashSet::new())),
            inner_loop_config: crate::config::InnerLoopConfig::default(),
        }
    }

    fn populate_ticks(kernel: &KernelContext, count: u64) {
        for i in 1..=count {
            let tick = TickRecord {
                tick_id: TickId::new(),
                tick_number: i,
                phase: TickPhase::Amend,
                started_at: chrono::Utc::now(),
                completed_at: Some(chrono::Utc::now()),
                snapshot_before: ArtifactId::from_content(format!("before-{i}").as_bytes()),
                snapshot_after: None,
                thread_contributions: vec![],
                actions_taken: vec![ActionRecord {
                    action_type: "fs.write".into(),
                    target: "/tmp/test".into(),
                    receipt_ref: None,
                    outcome: if i % 2 == 0 {
                        ActionOutcome::Success
                    } else {
                        ActionOutcome::Failure
                    },
                }],
                llm_calls: vec![LlmCallRecord {
                    model: "test-model".into(),
                    tokens_in: 100,
                    tokens_out: 50,
                    cost_cents: 0.1,
                    latency_ms: 200,
                    response_artifact_ref: None,
                    turns: 1,
                }],
                decision_rationale: Some(format!("Tick {i} reasoning")),
                context_breakdown_ref: None,
            };
            kernel.tick_store.save(&tick).unwrap();
        }
    }

    fn populate_events(kernel: &KernelContext, count: usize) {
        for i in 0..count {
            let entry = EventEntry {
                id: LedgerEntryId::new(),
                event_type: EventType::TickStarted,
                summary: format!("Event {i}"),
                timestamp: chrono::Utc::now(),
                tick_id: Some(TickId::new()),
                payload_ref: None,
            };
            kernel.event_ledger.append(&entry).unwrap();
        }
    }

    fn populate_memory(kernel: &KernelContext) {
        let note1 = LongTermNote {
            id: ArtifactId::from_content(b"note1"),
            topic: "deployment".into(),
            content: "Learned about CI/CD".into(),
            tags: vec!["ops".into(), "ci".into()],
            token_count: 10,
            created_at: chrono::Utc::now(),
        };
        let note2 = LongTermNote {
            id: ArtifactId::from_content(b"note2"),
            topic: "architecture".into(),
            content: "Dual engine design is critical".into(),
            tags: vec!["design".into()],
            token_count: 12,
            created_at: chrono::Utc::now(),
        };
        kernel.memory_store.write_long_term(&note1).unwrap();
        kernel.memory_store.write_long_term(&note2).unwrap();
    }

    // E5S1-T3: Returns recent ticks with action counts
    #[test]
    fn introspection_tick_history() {
        let dir = tempfile::tempdir().unwrap();
        let kernel = test_kernel(dir.path());
        populate_ticks(&kernel, 5);

        let service = IntrospectionService::new(&kernel);
        let result = service
            .query(&IntrospectionQuery::TickHistory { limit: 3 })
            .unwrap();
        let array = result.as_array().unwrap();
        assert_eq!(array.len(), 3);
        assert!(array[0].get("tick_number").is_some());
        assert!(array[0].get("actions_taken").is_some());
    }

    // E5S1-T4: Returns full detail for specific tick
    #[test]
    fn introspection_tick_detail() {
        let dir = tempfile::tempdir().unwrap();
        let kernel = test_kernel(dir.path());
        populate_ticks(&kernel, 1);

        let tick = kernel.tick_store.latest().unwrap().unwrap();
        let service = IntrospectionService::new(&kernel);
        let result = service
            .query(&IntrospectionQuery::TickDetail {
                tick_id: tick.tick_id.to_string(),
            })
            .unwrap();
        assert!(result.get("tick_number").is_some());
        assert_eq!(result["tick_number"], 1);
    }

    // E5S1-T5: Filters by event type, respects limit
    #[test]
    fn introspection_event_history() {
        let dir = tempfile::tempdir().unwrap();
        let kernel = test_kernel(dir.path());
        populate_events(&kernel, 5);

        let service = IntrospectionService::new(&kernel);
        let result = service
            .query(&IntrospectionQuery::EventHistory {
                event_type: None,
                limit: 3,
            })
            .unwrap();
        let array = result.as_array().unwrap();
        assert_eq!(array.len(), 3);

        // Filter by type
        let result2 = service
            .query(&IntrospectionQuery::EventHistory {
                event_type: Some("tick_started".into()),
                limit: 10,
            })
            .unwrap();
        let array2 = result2.as_array().unwrap();
        assert_eq!(array2.len(), 5);
    }

    // E5S1-T6: Returns all principals with trust levels
    #[test]
    fn introspection_trust_scores() {
        let dir = tempfile::tempdir().unwrap();
        let kernel = test_kernel(dir.path());

        // Add a relationship record so there's something to report
        let record = exoskeleton_core::RelationshipRecord {
            id: LedgerEntryId::new(),
            principal_id: exoskeleton_core::PrincipalId::new(),
            signal_type: exoskeleton_core::RelationalSignalType::FeedbackReceived,
            content_ref: ArtifactId::from_content(b"trust-signal"),
            timestamp: chrono::Utc::now(),
            tick_id: TickId::new(),
            metadata: Default::default(),
        };
        kernel.relationship_ledger.append(&record).unwrap();

        let service = IntrospectionService::new(&kernel);
        let result = service.query(&IntrospectionQuery::TrustScores).unwrap();
        let array = result.as_array().unwrap();
        assert_eq!(array.len(), 1);
        assert!(array[0].get("principal_id").is_some());
        assert!(array[0].get("trust_level").is_some());
    }

    // E5S1-T7: Returns trust changes for one principal
    #[test]
    fn introspection_trust_history() {
        let dir = tempfile::tempdir().unwrap();
        let kernel = test_kernel(dir.path());
        let pid = exoskeleton_core::PrincipalId::new();

        // Add relationship records for the principal
        for _ in 0..3 {
            let record = exoskeleton_core::RelationshipRecord {
                id: LedgerEntryId::new(),
                principal_id: pid,
                signal_type: exoskeleton_core::RelationalSignalType::FeedbackReceived,
                content_ref: ArtifactId::from_content(b"trust-signal"),
                timestamp: chrono::Utc::now(),
                tick_id: TickId::new(),
                metadata: Default::default(),
            };
            kernel.relationship_ledger.append(&record).unwrap();
        }

        let service = IntrospectionService::new(&kernel);
        let result = service
            .query(&IntrospectionQuery::TrustHistory {
                principal_id: pid.to_string(),
                limit: 10,
            })
            .unwrap();
        let array = result.as_array().unwrap();
        assert_eq!(array.len(), 3);
        assert!(array[0].get("signal_type").is_some());
    }

    // E5S1-T8: Returns budget dimensions (or "no budget" message)
    #[test]
    fn introspection_budget_status() {
        let dir = tempfile::tempdir().unwrap();
        let kernel = test_kernel(dir.path());

        let service = IntrospectionService::new(&kernel);
        let result = service.query(&IntrospectionQuery::BudgetStatus).unwrap();
        // No budget configured → message
        assert!(result.get("message").is_some());
    }

    // E5S1-T9: Returns all threads with statuses
    #[test]
    fn introspection_thread_status() {
        let dir = tempfile::tempdir().unwrap();
        let kernel = test_kernel(dir.path());

        // Register built-in threads so there's something to report
        exoskeleton_threads::register_builtin_threads(
            &kernel.thread_registry,
            &PromptRegistry::with_defaults(),
            None,
        )
        .unwrap();

        let service = IntrospectionService::new(&kernel);
        let result = service.query(&IntrospectionQuery::ThreadStatus).unwrap();
        let array = result.as_array().unwrap();
        assert!(!array.is_empty());
    }

    // E5S1-T10: Filters long-term notes by topic and tags
    #[test]
    fn introspection_memory_search() {
        let dir = tempfile::tempdir().unwrap();
        let kernel = test_kernel(dir.path());
        populate_memory(&kernel);

        let service = IntrospectionService::new(&kernel);

        // Search by topic
        let result = service
            .query(&IntrospectionQuery::MemorySearch {
                topic: Some("deployment".into()),
                tags: None,
            })
            .unwrap();
        let array = result.as_array().unwrap();
        assert_eq!(array.len(), 1);

        // Search by tag
        let result2 = service
            .query(&IntrospectionQuery::MemorySearch {
                topic: None,
                tags: Some(vec!["design".into()]),
            })
            .unwrap();
        let array2 = result2.as_array().unwrap();
        assert_eq!(array2.len(), 1);

        // No filter → all
        let result3 = service
            .query(&IntrospectionQuery::MemorySearch {
                topic: None,
                tags: None,
            })
            .unwrap();
        let array3 = result3.as_array().unwrap();
        assert_eq!(array3.len(), 2);
    }

    // E5S1-T11: Returns full descriptor for named connector
    #[test]
    fn introspection_connector_details() {
        let dir = tempfile::tempdir().unwrap();
        let kernel = test_kernel(dir.path());

        let service = IntrospectionService::new(&kernel);
        // No WI host → "not ready"
        let result = service
            .query(&IntrospectionQuery::ConnectorDetails {
                name: "http.request".into(),
            })
            .unwrap();
        assert!(result.get("error").is_some());
    }

    // E5S1-T12: Limit > MAX_INTROSPECTION_LIMIT clamped to 100
    #[test]
    fn introspection_bounded_results() {
        let dir = tempfile::tempdir().unwrap();
        let kernel = test_kernel(dir.path());
        populate_events(&kernel, 5);

        let service = IntrospectionService::new(&kernel);
        // Request 200 but should be clamped
        let result = service
            .query(&IntrospectionQuery::EventHistory {
                event_type: None,
                limit: 200,
            })
            .unwrap();
        let array = result.as_array().unwrap();
        // We only have 5 events, so result is 5 (clamped limit doesn't add more)
        assert_eq!(array.len(), 5);
    }
}

//! Read-only inspection surface over all vessel state.
//!
//! The [`VesselInspector`] aggregates data from stores, the thread registry,
//! budget trackers, and engine slots into coherent query results. All methods
//! are non-mutating and safe to call concurrently from the HTTP daemon.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use exoskeleton_core::budget::ThrashLevel;
use exoskeleton_core::conversation::{Conversation, ConversationStore};
use exoskeleton_core::relationship::{RelationshipRecord, RelationshipSnapshot};
use exoskeleton_core::tick::TickRecord;
use exoskeleton_core::{
    Artifact, ArtifactId, ArtifactKind, ArtifactStore, BudgetStatus, ConversationId,
    EpisodicSummary, EventEntry, EventLedger, EventType, ExoError, LedgerEntryId, LongTermNote,
    PrincipalId, SnapshotStore, StateSnapshot, ThreadPriority, ThreadSchedule, ThreadStatus,
    TickId, TickStore, VesselId, VesselStatus,
};
use exoskeleton_memory::MemoryStore;
use exoskeleton_relationship::{compile_relationship_snapshot, RelationshipLedger};
use exoskeleton_threads::ThreadRegistry;
use serde::{Deserialize, Serialize};
use worldinterface_core::descriptor::Descriptor;

use crate::budget::{CognitiveBudgetTracker, ToolBudgetGate};
use crate::config::VesselConfig;
use crate::kernel::WiHostSlot;
use crate::storage::StorageManager;

/// Fork result returned by `fork_from_snapshot()`.
pub struct ForkResult {
    /// The new vessel's unique identity.
    pub vessel_id: VesselId,
    /// Path to the generated vessel.toml.
    pub config_path: PathBuf,
    /// Path to the new data directory.
    pub data_dir: PathBuf,
    /// The source tick number.
    pub forked_from_tick: u64,
    /// The source vessel's ID.
    pub source_vessel_id: VesselId,
}

/// Read-only inspection surface over all vessel state.
///
/// Holds shared references (Arc) to stores and subsystems. All methods are
/// non-mutating and safe to call concurrently from the HTTP daemon.
#[allow(dead_code)]
pub struct VesselInspector {
    storage: StorageManager,
    thread_registry: Arc<ThreadRegistry>,
    relationship_ledger: Arc<dyn RelationshipLedger>,
    budget_tracker: Option<Arc<tokio::sync::Mutex<CognitiveBudgetTracker>>>,
    tool_budget_gate: Option<Arc<tokio::sync::Mutex<ToolBudgetGate>>>,
    wi_host_slot: WiHostSlot,
    config: VesselConfig,
}

/// Thread information for inspection display.
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
pub struct ThreadStatusEntry {
    pub thread_id: exoskeleton_core::ThreadId,
    pub name: String,
    pub charter: String,
    pub priority: ThreadPriority,
    pub schedule: ThreadSchedule,
    pub status: ThreadStatus,
    pub latest_output_summary: Option<String>,
    pub token_budget: u64,
}

/// Combined budget status for inspection (richer than BudgetStatus in snapshot).
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
pub struct InspectionBudgetStatus {
    /// Cognitive budget from CognitiveBudgetTracker.
    pub cognitive: Option<CognitiveBudgetDetail>,
    /// Tool budget from ToolBudgetGate.
    pub tool: Option<ToolBudgetDetail>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
pub struct CognitiveBudgetDetail {
    pub local_tokens_remaining: u64,
    pub frontier_tokens_remaining: u64,
    pub frontier_cost_cents_remaining: u64,
    pub frontier_calls_this_window: u64,
    pub consecutive_failures: u32,
    pub thrash_level: ThrashLevel,
    pub window_start: DateTime<Utc>,
    pub window_duration_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
pub struct ToolBudgetDetail {
    pub invocations_remaining: u64,
    pub invocations_this_window: u64,
    pub window_start: DateTime<Utc>,
}

/// Health and status of both engines.
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
pub struct EngineStatus {
    pub cognitive: CognitiveEngineStatus,
    pub tool: ToolEngineStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
pub struct CognitiveEngineStatus {
    /// Whether the engine slot contains an engine (not shut down).
    pub available: bool,
    /// Latest tick number from snapshot store.
    pub last_tick_number: Option<u64>,
    /// Timestamp of last completed tick.
    pub last_tick_at: Option<DateTime<Utc>>,
    /// Current vessel status from latest snapshot.
    pub vessel_status: Option<VesselStatus>,
    /// Number of active (non-suspended, non-completed) threads.
    pub active_threads: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
pub struct ToolEngineStatus {
    /// Whether the WI Host slot contains a host (not shut down).
    pub available: bool,
    /// Number of registered tool capabilities.
    pub capabilities_count: usize,
    /// Number of currently active (non-terminal) WI flows.
    pub active_flows: usize,
}

impl VesselInspector {
    /// Create a new VesselInspector with all shared references.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        storage: StorageManager,
        thread_registry: Arc<ThreadRegistry>,
        relationship_ledger: Arc<dyn RelationshipLedger>,
        budget_tracker: Option<Arc<tokio::sync::Mutex<CognitiveBudgetTracker>>>,
        tool_budget_gate: Option<Arc<tokio::sync::Mutex<ToolBudgetGate>>>,
        wi_host_slot: WiHostSlot,
        config: VesselConfig,
    ) -> Self {
        Self {
            storage,
            thread_registry,
            relationship_ledger,
            budget_tracker,
            tool_budget_gate,
            wi_host_slot,
            config,
        }
    }

    /// Current state snapshot (latest from store).
    pub fn snapshot(&self) -> Result<Option<StateSnapshot>, ExoError> {
        self.storage.snapshot_store().latest()
    }

    /// Recent tick history.
    pub fn tick_history(&self, limit: usize) -> Result<Vec<TickRecord>, ExoError> {
        // Get latest tick number, then use range to get recent ticks
        let latest = self.storage.tick_store().latest()?;
        match latest {
            Some(record) => {
                let to = record.tick_number;
                let from = to.saturating_sub(limit as u64 - 1);
                let mut ticks = self.storage.tick_store().range(from, to)?;
                // Return newest first
                ticks.reverse();
                // Limit in case the range is larger than requested
                ticks.truncate(limit);
                Ok(ticks)
            }
            None => Ok(vec![]),
        }
    }

    /// Full detail for a specific tick.
    pub fn tick_detail(&self, tick_id: TickId) -> Result<Option<TickRecord>, ExoError> {
        self.storage.tick_store().get(tick_id)
    }

    /// All registered threads with their current status and latest output.
    pub fn thread_status(&self) -> Result<Vec<ThreadStatusEntry>, ExoError> {
        let threads = self.thread_registry.list()?;
        let mut entries = Vec::with_capacity(threads.len());

        for (spec, status) in threads {
            let latest_output_summary = self
                .thread_registry
                .recent_outputs(spec.thread_id, 1)
                .ok()
                .and_then(|outputs| outputs.into_iter().next())
                .map(|o| o.summary);

            entries.push(ThreadStatusEntry {
                thread_id: spec.thread_id,
                name: spec.name,
                charter: spec.charter,
                priority: spec.priority,
                schedule: spec.schedule,
                status,
                latest_output_summary,
                token_budget: spec.token_budget,
            });
        }

        Ok(entries)
    }

    /// Current relationship snapshot (compiled from ledger).
    pub fn relationship_snapshot(&self) -> Result<RelationshipSnapshot, ExoError> {
        compile_relationship_snapshot(self.relationship_ledger.as_ref(), None, chrono::Utc::now())
    }

    /// Relationship history for a specific principal.
    pub fn relationship_history(
        &self,
        principal_id: PrincipalId,
        limit: usize,
    ) -> Result<Vec<RelationshipRecord>, ExoError> {
        self.relationship_ledger.for_principal(principal_id, limit)
    }

    /// Current budget state (cognitive + tool).
    pub async fn budget_status(&self) -> Result<InspectionBudgetStatus, ExoError> {
        let cognitive = if let Some(ref tracker) = self.budget_tracker {
            match tracker.try_lock() {
                Ok(guard) => Some(CognitiveBudgetDetail {
                    local_tokens_remaining: guard.remaining_local_tokens(),
                    frontier_tokens_remaining: guard.remaining_frontier_tokens(),
                    frontier_cost_cents_remaining: guard.remaining_frontier_cost(),
                    frontier_calls_this_window: guard.frontier_calls_this_window(),
                    consecutive_failures: guard.consecutive_failures(),
                    thrash_level: guard.thrash_level(),
                    window_start: guard.window_start(),
                    window_duration_secs: guard.config().time_window_secs,
                }),
                Err(_) => None,
            }
        } else {
            None
        };

        let tool = if let Some(ref gate) = self.tool_budget_gate {
            match gate.try_lock() {
                Ok(guard) => Some(ToolBudgetDetail {
                    invocations_remaining: guard.remaining(),
                    invocations_this_window: guard.invocations_this_window(),
                    window_start: guard.window_start(),
                }),
                Err(_) => None,
            }
        } else {
            None
        };

        Ok(InspectionBudgetStatus { cognitive, tool })
    }

    /// Recent events from the event ledger.
    pub fn recent_events(&self, limit: usize) -> Result<Vec<EventEntry>, ExoError> {
        self.storage.event_ledger().recent(limit)
    }

    /// Retrieve a specific artifact by ID.
    pub fn artifact(&self, id: &ArtifactId) -> Result<Option<Artifact>, ExoError> {
        self.storage.artifact_store().get(id)
    }

    /// Health and status of both engines.
    pub async fn engine_status(&self) -> EngineStatus {
        // Cognitive engine status from stores (no lock needed for stores)
        let snapshot = self.storage.snapshot_store().latest().ok().flatten();
        let last_tick = self.storage.tick_store().latest().ok().flatten();

        let active_threads = self
            .thread_registry
            .list()
            .ok()
            .map(|threads| {
                threads
                    .iter()
                    .filter(|(_, status)| *status == ThreadStatus::Active)
                    .count()
            })
            .unwrap_or(0);

        let cognitive = CognitiveEngineStatus {
            available: snapshot.is_some() || last_tick.is_some() || active_threads > 0,
            last_tick_number: snapshot.as_ref().map(|s| s.tick_number),
            last_tick_at: last_tick.and_then(|t| t.completed_at),
            vessel_status: snapshot.as_ref().map(|s| s.status),
            active_threads,
        };

        // Tool engine status via WI Host slot (try_lock to avoid blocking)
        let (tool_available, capabilities_count, active_flows) = match self.wi_host_slot.try_lock()
        {
            Ok(guard) => match guard.as_ref() {
                Some(host) => {
                    let caps = host.list_capabilities().len();
                    // WI Host doesn't expose active_flows directly; use 0 for now
                    (true, caps, 0)
                }
                None => (false, 0, 0),
            },
            Err(_) => (true, 0, 0), // Lock contended = host is being used = available
        };

        let tool = ToolEngineStatus {
            available: tool_available,
            capabilities_count,
            active_flows,
        };

        EngineStatus { cognitive, tool }
    }

    /// WI Host tool capabilities.
    pub fn capabilities(&self) -> Vec<Descriptor> {
        match self.wi_host_slot.try_lock() {
            Ok(guard) => match guard.as_ref() {
                Some(host) => host.list_capabilities(),
                None => vec![],
            },
            Err(_) => vec![],
        }
    }

    // ── D2: New inspection methods ──

    /// Recent episodic summaries from memory store.
    pub fn memory_episodic(&self, limit: usize) -> Result<Vec<EpisodicSummary>, ExoError> {
        self.storage.memory_store().recent_episodic(limit)
    }

    /// All long-term notes from memory store.
    pub fn memory_long_term(&self, limit: usize) -> Result<Vec<LongTermNote>, ExoError> {
        self.storage.memory_store().all_long_term(limit)
    }

    /// Snapshot history (newest first).
    pub fn snapshot_history(&self, limit: usize) -> Result<Vec<StateSnapshot>, ExoError> {
        self.storage.snapshot_store().history(limit)
    }

    /// Snapshot at a specific tick number.
    pub fn snapshot_at_tick(&self, tick_number: u64) -> Result<Option<StateSnapshot>, ExoError> {
        self.storage.snapshot_store().at_tick(tick_number)
    }

    /// Inbox history reconstructed from MessageReceived events in the event ledger.
    pub fn inbox_history(&self, limit: usize) -> Result<Vec<InboxHistoryEntry>, ExoError> {
        let events = self
            .storage
            .event_ledger()
            .by_type(EventType::MessageReceived, limit)?;
        let mut entries = Vec::with_capacity(events.len());
        for event in events {
            // Try to reconstruct envelope details from the referenced artifact
            let (source, content, envelope_id, in_reply_to) =
                if let Some(ref artifact_id) = event.payload_ref {
                    match self.storage.artifact_store().get(artifact_id) {
                        Ok(Some(artifact)) => {
                            // Artifact may contain an envelope or a payload
                            if let Ok(envelope) = serde_json::from_slice::<
                                exoskeleton_core::MessageEnvelope,
                            >(&artifact.content)
                            {
                                (
                                    Some(envelope.source),
                                    None,
                                    Some(envelope.id),
                                    envelope.in_reply_to,
                                )
                            } else {
                                // Raw payload content
                                let text = String::from_utf8_lossy(&artifact.content).into_owned();
                                (None, Some(text), None, None)
                            }
                        }
                        _ => (None, None, None, None),
                    }
                } else {
                    (None, None, None, None)
                };

            entries.push(InboxHistoryEntry {
                envelope_id,
                source,
                content: content.unwrap_or_else(|| event.summary.clone()),
                timestamp: event.timestamp,
                in_reply_to,
            });
        }
        Ok(entries)
    }

    /// Vessel configuration (for sanitized display).
    pub fn config(&self) -> &VesselConfig {
        &self.config
    }

    /// Get the context breakdown for a specific tick.
    ///
    /// Returns the CompiledContext that was assembled during the Orient step,
    /// showing per-section token allocation and truncation information.
    pub fn context_breakdown(
        &self,
        tick_id: TickId,
    ) -> Result<Option<exoskeleton_memory::compiler::CompiledContext>, ExoError> {
        let record = match self.storage.tick_store().get(tick_id)? {
            Some(r) => r,
            None => return Ok(None),
        };
        let ref_id = match record.context_breakdown_ref {
            Some(id) => id,
            None => return Ok(None),
        };
        match self.storage.artifact_store().get(&ref_id)? {
            Some(artifact) => {
                let context: exoskeleton_memory::compiler::CompiledContext =
                    serde_json::from_slice(&artifact.content).map_err(|e| {
                        ExoError::Storage(format!("invalid context breakdown: {e}"))
                    })?;
                Ok(Some(context))
            }
            None => Ok(None),
        }
    }

    // ── E1-S2: Conversation inspection ──

    /// Get active conversations.
    pub fn conversations(&self, limit: usize) -> Result<Vec<Conversation>, ExoError> {
        self.storage
            .conversation_store()
            .active_conversations(limit)
    }

    /// Get a single conversation by ID.
    pub fn conversation(&self, id: ConversationId) -> Result<Option<Conversation>, ExoError> {
        self.storage.conversation_store().get(id)
    }

    // ── Epoch 0: Charter hot-reload ──

    /// Reload thread charters from prompt files on disk.
    ///
    /// Creates a fresh PromptRegistry, loads overrides from the data directory
    /// and project-level prompts, then applies any changed charters to the
    /// thread registry. Returns the number of charters updated.
    ///
    /// Note: this is a mutation operation (updates thread store), not a read.
    pub fn reload_charters(&self) -> Result<u32, ExoError> {
        let mut prompts = exoskeleton_core::prompt::PromptRegistry::with_defaults();
        crate::prompt_loader::load_prompt_overrides(&mut prompts, &self.config.data_dir);
        self.thread_registry.reload_charters(&prompts)
    }

    // ── E3-S3: Snapshot Fork ──

    /// Fork a new vessel from a historical snapshot.
    ///
    /// Creates a new data directory with empty stores, seeds the initial
    /// snapshot from the source vessel's state at `tick_number`, generates
    /// a vessel.toml, and records a VesselForked event in the source ledger.
    ///
    /// Note: this is a creation/mutation operation, not a read.
    pub fn fork_from_snapshot(
        &self,
        tick_number: u64,
        target_data_dir: &Path,
        mission_override: Option<&str>,
    ) -> Result<ForkResult, ExoError> {
        // 1. Load source snapshot at the requested tick
        let source_snapshot = self
            .storage
            .snapshot_store()
            .at_tick(tick_number)?
            .ok_or_else(|| ExoError::NotFound(format!("no snapshot at tick {tick_number}")))?;

        // 2. Validate target data directory
        if target_data_dir.exists() {
            return Err(ExoError::Config(format!(
                "target data directory already exists: {}",
                target_data_dir.display()
            )));
        }

        // 3. Generate new VesselId
        let new_vessel_id = VesselId::new();

        // 4. Create data directory structure
        // Note: exo/ is created by StorageManager::create_fresh() in step 5.
        let dirs = [
            target_data_dir.join("cognitive-aq"),
            target_data_dir.join("wi/aq"),
            target_data_dir.join("wi"),
            target_data_dir.join("inbox"),
        ];
        for dir in &dirs {
            std::fs::create_dir_all(dir).map_err(|e| {
                ExoError::Storage(format!(
                    "failed to create fork directory {}: {e}",
                    dir.display()
                ))
            })?;
        }

        // 5. Initialize empty stores
        let fork_storage = StorageManager::create_fresh(target_data_dir)?;

        // 6. Seed initial snapshot (tick 0, new vessel_id, forked cognitive state)
        let forked_snapshot = StateSnapshot {
            vessel_id: new_vessel_id,
            tick_number: 0,
            mission: mission_override
                .unwrap_or(&source_snapshot.mission)
                .to_string(),
            plan: source_snapshot.plan.clone(),
            status: VesselStatus::Idle,
            working_memory: source_snapshot.working_memory.clone(),
            thread_summaries: Vec::new(),
            relationship_snapshot_ref: None,
            budget_status: BudgetStatus::unlimited(),
            last_action_summary: None,
            started_at: None,
            updated_at: Utc::now(),
        };
        fork_storage.snapshot_store().save(&forked_snapshot)?;

        // 7. Generate vessel.toml
        let config_path =
            self.config
                .generate_fork_config(new_vessel_id, target_data_dir, mission_override)?;

        // 8. Record VesselForked event in source vessel's event ledger
        let fork_metadata = serde_json::json!({
            "source_vessel_id": self.config.vessel_id.to_string(),
            "source_tick": tick_number,
            "fork_vessel_id": new_vessel_id.to_string(),
            "fork_data_dir": target_data_dir.to_string_lossy(),
        });
        let fork_artifact = Artifact::new(
            ArtifactKind::Receipt,
            serde_json::to_vec(&fork_metadata).unwrap_or_default(),
            "application/json".into(),
        );
        self.storage.artifact_store().put(&fork_artifact)?;

        let event = EventEntry {
            id: LedgerEntryId::new(),
            tick_id: None,
            event_type: EventType::VesselForked,
            payload_ref: Some(fork_artifact.id),
            summary: format!(
                "Forked vessel {} from tick {} → new vessel {} at {}",
                self.config.vessel_id,
                tick_number,
                new_vessel_id,
                target_data_dir.display()
            ),
            timestamp: Utc::now(),
        };
        self.storage.event_ledger().append(&event)?;

        Ok(ForkResult {
            vessel_id: new_vessel_id,
            config_path,
            data_dir: target_data_dir.to_path_buf(),
            forked_from_tick: tick_number,
            source_vessel_id: self.config.vessel_id,
        })
    }
}

/// A reconstructed inbox history entry from event ledger data.
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
pub struct InboxHistoryEntry {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub envelope_id: Option<exoskeleton_core::EnvelopeId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<exoskeleton_core::PrincipalId>,
    pub content: String,
    pub timestamp: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub in_reply_to: Option<exoskeleton_core::EnvelopeId>,
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use exoskeleton_core::prompt::PromptRegistry;
    use exoskeleton_core::tick::{TickPhase, TickRecord};
    use exoskeleton_core::{EventEntry, EventType, LedgerEntryId, StateSnapshot, TickId, VesselId};
    use exoskeleton_relationship::InMemoryRelationshipLedger;
    use exoskeleton_threads::{InMemoryThreadStore, ThreadRegistry};

    use super::*;
    use crate::config::VesselConfig;
    use crate::storage::StorageManager;

    fn test_inspector(dir: &std::path::Path) -> VesselInspector {
        let storage = StorageManager::open(dir).unwrap();
        let thread_store = Arc::new(InMemoryThreadStore::new());
        let thread_registry = Arc::new(ThreadRegistry::new(thread_store));
        let relationship_ledger: Arc<dyn RelationshipLedger> =
            Arc::new(InMemoryRelationshipLedger::new());
        let wi_host_slot: WiHostSlot = Arc::new(tokio::sync::Mutex::new(None));
        let config = VesselConfig {
            mission: "test".into(),
            ..Default::default()
        };

        VesselInspector::new(
            storage,
            thread_registry,
            relationship_ledger,
            None,
            None,
            wi_host_slot,
            config,
        )
    }

    #[test]
    fn snapshot_returns_latest_from_store() {
        let dir = tempfile::tempdir().unwrap();
        let inspector = test_inspector(dir.path());

        // Save a snapshot
        let snap = StateSnapshot::initial(VesselId::new(), "test mission".into());
        inspector.storage.snapshot_store().save(&snap).unwrap();

        let result = inspector.snapshot().unwrap();
        assert!(result.is_some());
        assert_eq!(result.unwrap().mission, "test mission");
    }

    #[test]
    fn snapshot_returns_none_when_empty() {
        let dir = tempfile::tempdir().unwrap();
        let inspector = test_inspector(dir.path());

        let result = inspector.snapshot().unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn tick_history_respects_limit() {
        let dir = tempfile::tempdir().unwrap();
        let inspector = test_inspector(dir.path());

        // Save 5 ticks
        for i in 1..=5 {
            let record = TickRecord {
                tick_id: TickId::new(),
                tick_number: i,
                phase: TickPhase::Amend,
                started_at: Utc::now(),
                completed_at: Some(Utc::now()),
                snapshot_before: ArtifactId::from_content(format!("before-{i}").as_bytes()),
                snapshot_after: None,
                thread_contributions: vec![],
                actions_taken: vec![],
                llm_calls: vec![],
                decision_rationale: None,
                context_breakdown_ref: None,
            };
            inspector.storage.tick_store().save(&record).unwrap();
        }

        let history = inspector.tick_history(3).unwrap();
        assert_eq!(history.len(), 3);
        // Should be newest first
        assert_eq!(history[0].tick_number, 5);
        assert_eq!(history[1].tick_number, 4);
        assert_eq!(history[2].tick_number, 3);
    }

    #[test]
    fn tick_detail_returns_none_for_unknown() {
        let dir = tempfile::tempdir().unwrap();
        let inspector = test_inspector(dir.path());

        let result = inspector.tick_detail(TickId::new()).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn thread_status_returns_all_registered() {
        let dir = tempfile::tempdir().unwrap();
        let inspector = test_inspector(dir.path());

        // Register built-in threads
        exoskeleton_threads::register_builtin_threads(
            &inspector.thread_registry,
            &PromptRegistry::with_defaults(),
        )
        .unwrap();

        let threads = inspector.thread_status().unwrap();
        assert_eq!(threads.len(), 3, "expected 3 built-in threads");
    }

    #[test]
    fn recent_events_respects_limit() {
        let dir = tempfile::tempdir().unwrap();
        let inspector = test_inspector(dir.path());

        // Append 5 events
        for i in 0..5 {
            let event = EventEntry {
                id: LedgerEntryId::new(),
                tick_id: None,
                event_type: EventType::VesselStarted,
                payload_ref: None,
                summary: format!("event {i}"),
                timestamp: Utc::now(),
            };
            inspector.storage.event_ledger().append(&event).unwrap();
        }

        let events = inspector.recent_events(3).unwrap();
        assert_eq!(events.len(), 3);
    }

    #[tokio::test]
    async fn budget_status_none_when_no_trackers() {
        let dir = tempfile::tempdir().unwrap();
        let inspector = test_inspector(dir.path());

        let status = inspector.budget_status().await.unwrap();
        assert!(status.cognitive.is_none());
        assert!(status.tool.is_none());
    }

    #[tokio::test]
    async fn engine_status_reports_unavailable_when_empty() {
        let dir = tempfile::tempdir().unwrap();
        let inspector = test_inspector(dir.path());

        let status = inspector.engine_status().await;
        // No snapshot data, no ticks, no threads => not available
        assert!(!status.cognitive.available);
        assert!(!status.tool.available);
    }

    #[test]
    fn capabilities_empty_when_no_host() {
        let dir = tempfile::tempdir().unwrap();
        let inspector = test_inspector(dir.path());

        let caps = inspector.capabilities();
        assert!(caps.is_empty());
    }

    #[test]
    fn relationship_snapshot_compiles_from_ledger() {
        let dir = tempfile::tempdir().unwrap();
        let inspector = test_inspector(dir.path());

        // Empty ledger should produce an empty snapshot
        let snapshot = inspector.relationship_snapshot().unwrap();
        assert!(snapshot.principals.is_empty());
    }

    // ── E3-S3: Snapshot Fork Tests ──

    #[test]
    fn fork_creates_data_directory_structure() {
        let source_dir = tempfile::tempdir().unwrap();
        let inspector = test_inspector(source_dir.path());

        // Seed a source snapshot at tick 5
        let snap = StateSnapshot {
            vessel_id: VesselId::new(),
            tick_number: 5,
            mission: "source mission".into(),
            plan: Some(exoskeleton_core::Plan::from_legacy_string(
                "the plan".into(),
            )),
            status: VesselStatus::Idle,
            working_memory: exoskeleton_core::working_memory::WorkingMemory::from_legacy_string(
                "working on something".into(),
            ),
            thread_summaries: Vec::new(),
            relationship_snapshot_ref: None,
            budget_status: BudgetStatus::unlimited(),
            last_action_summary: Some("did something".into()),
            started_at: Some(Utc::now()),
            updated_at: Utc::now(),
        };
        inspector.storage.snapshot_store().save(&snap).unwrap();

        let fork_dir = tempfile::tempdir().unwrap();
        let fork_path = fork_dir.path().join("my-fork");
        let _result = inspector.fork_from_snapshot(5, &fork_path, None).unwrap();

        // Verify directory structure (E3-T23)
        assert!(fork_path.join("cognitive-aq").is_dir());
        assert!(fork_path.join("wi/aq").is_dir());
        assert!(fork_path.join("wi").is_dir());
        assert!(fork_path.join("exo").is_dir());
        assert!(fork_path.join("inbox").is_dir());
    }

    #[test]
    fn fork_initializes_empty_stores() {
        let source_dir = tempfile::tempdir().unwrap();
        let inspector = test_inspector(source_dir.path());

        let mut snap = StateSnapshot::initial(VesselId::new(), "test".into());
        snap.tick_number = 1;
        snap.updated_at = Utc::now();
        inspector.storage.snapshot_store().save(&snap).unwrap();

        let fork_dir = tempfile::tempdir().unwrap();
        let fork_path = fork_dir.path().join("fork");
        inspector.fork_from_snapshot(1, &fork_path, None).unwrap();

        // Verify stores are created and empty (E3-T24)
        let fork_storage = StorageManager::open(&fork_path).unwrap();
        assert!(fork_storage.tick_store().latest().unwrap().is_none());
        assert!(fork_storage.event_ledger().recent(1).unwrap().is_empty());
    }

    #[test]
    fn fork_seeds_initial_snapshot() {
        let source_dir = tempfile::tempdir().unwrap();
        let inspector = test_inspector(source_dir.path());

        let source_snap = StateSnapshot {
            vessel_id: VesselId::new(),
            tick_number: 3,
            mission: "original mission".into(),
            plan: Some(exoskeleton_core::Plan::from_legacy_string(
                "execute plan B".into(),
            )),
            status: VesselStatus::Acting,
            working_memory: exoskeleton_core::working_memory::WorkingMemory::from_legacy_string(
                "analyzing data".into(),
            ),
            thread_summaries: vec![exoskeleton_core::snapshot::ThreadSummary {
                thread_id: exoskeleton_core::ThreadId::new(),
                name: "Threat Monitor".into(),
                status: ThreadStatus::Active,
                last_output_summary: Some("no threats".into()),
                token_budget_remaining: 5000,
            }],
            relationship_snapshot_ref: Some(ArtifactId::from_content(b"rel")),
            budget_status: BudgetStatus::unlimited(),
            last_action_summary: Some("wrote file".into()),
            started_at: Some(Utc::now()),
            updated_at: Utc::now(),
        };
        inspector
            .storage
            .snapshot_store()
            .save(&source_snap)
            .unwrap();

        let fork_dir = tempfile::tempdir().unwrap();
        let fork_path = fork_dir.path().join("fork");
        let result = inspector.fork_from_snapshot(3, &fork_path, None).unwrap();

        // Verify seeded snapshot (E3-T25)
        let fork_storage = StorageManager::open(&fork_path).unwrap();
        let forked = fork_storage.snapshot_store().latest().unwrap().unwrap();
        assert_eq!(forked.tick_number, 0);
        assert_eq!(forked.vessel_id, result.vessel_id);
        assert_ne!(forked.vessel_id, source_snap.vessel_id);
        assert_eq!(forked.mission, "original mission");
        assert_eq!(forked.plan.as_ref().unwrap().objective, "execute plan B");
        assert_eq!(forked.working_memory.entries[0].value, "analyzing data");
        assert_eq!(forked.status, VesselStatus::Idle);
        assert!(forked.thread_summaries.is_empty());
        assert!(forked.relationship_snapshot_ref.is_none());
        assert!(forked.started_at.is_none());
        assert!(forked.last_action_summary.is_none());
    }

    #[test]
    fn fork_records_vessel_forked_event_in_source() {
        let source_dir = tempfile::tempdir().unwrap();
        let inspector = test_inspector(source_dir.path());

        let snap = StateSnapshot::initial(VesselId::new(), "test".into());
        inspector.storage.snapshot_store().save(&snap).unwrap();

        let fork_dir = tempfile::tempdir().unwrap();
        let fork_path = fork_dir.path().join("fork");
        inspector.fork_from_snapshot(0, &fork_path, None).unwrap();

        // Verify VesselForked event in source (E3-T27)
        let events = inspector
            .storage
            .event_ledger()
            .by_type(EventType::VesselForked, 1)
            .unwrap();
        assert_eq!(events.len(), 1);
        assert!(events[0].payload_ref.is_some());
        assert!(events[0].summary.contains("Forked vessel"));
    }

    #[test]
    fn fork_returns_not_found_for_nonexistent_tick() {
        let source_dir = tempfile::tempdir().unwrap();
        let inspector = test_inspector(source_dir.path());

        let fork_dir = tempfile::tempdir().unwrap();
        let fork_path = fork_dir.path().join("fork");
        let result = inspector.fork_from_snapshot(99, &fork_path, None);

        // E3-T28
        assert!(matches!(result, Err(ExoError::NotFound(_))));
    }

    #[test]
    fn fork_returns_error_if_data_dir_exists() {
        let source_dir = tempfile::tempdir().unwrap();
        let inspector = test_inspector(source_dir.path());

        let snap = StateSnapshot::initial(VesselId::new(), "test".into());
        inspector.storage.snapshot_store().save(&snap).unwrap();

        let fork_dir = tempfile::tempdir().unwrap();
        // fork_dir.path() already exists (tempdir creates it)
        let result = inspector.fork_from_snapshot(0, fork_dir.path(), None);

        // E3-T29
        assert!(matches!(result, Err(ExoError::Config(msg)) if msg.contains("already exists")));
    }

    // ── E1-T60: VesselInspector.conversations() ──

    #[test]
    fn conversations_returns_active() {
        use exoskeleton_core::conversation::Conversation;
        use exoskeleton_core::{EnvelopeId, PrincipalId};

        let dir = tempfile::tempdir().unwrap();
        let inspector = test_inspector(dir.path());

        // Save a conversation directly to the store
        let conv = Conversation::from_first_message(
            PrincipalId::new(),
            EnvelopeId::new(),
            ArtifactId::from_content(b"inspector-test"),
            Utc::now(),
        );
        inspector.storage.conversation_store().save(&conv).unwrap();

        let convs = inspector.conversations(10).unwrap();
        assert_eq!(convs.len(), 1);
        assert_eq!(convs[0].id, conv.id);

        // Get by ID
        let found = inspector.conversation(conv.id).unwrap();
        assert!(found.is_some());
        assert!(inspector
            .conversation(exoskeleton_core::ConversationId::new())
            .unwrap()
            .is_none());
    }

    #[test]
    fn fork_with_mission_override() {
        let source_dir = tempfile::tempdir().unwrap();
        let inspector = test_inspector(source_dir.path());

        let snap = StateSnapshot::initial(VesselId::new(), "original".into());
        inspector.storage.snapshot_store().save(&snap).unwrap();

        let fork_dir = tempfile::tempdir().unwrap();
        let fork_path = fork_dir.path().join("fork");
        inspector
            .fork_from_snapshot(0, &fork_path, Some("new mission"))
            .unwrap();

        let fork_storage = StorageManager::open(&fork_path).unwrap();
        let forked = fork_storage.snapshot_store().latest().unwrap().unwrap();
        assert_eq!(forked.mission, "new mission");
    }
}

//! Read-only inspection surface over all vessel state.
//!
//! The [`VesselInspector`] aggregates data from stores, the thread registry,
//! budget trackers, and engine slots into coherent query results. All methods
//! are non-mutating and safe to call concurrently from the HTTP daemon.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use exoskeleton_core::budget::ThrashLevel;
use exoskeleton_core::relationship::{RelationshipRecord, RelationshipSnapshot};
use exoskeleton_core::tick::TickRecord;
use exoskeleton_core::{
    Artifact, ArtifactId, ArtifactStore, EventEntry, EventLedger, ExoError, PrincipalId,
    SnapshotStore, StateSnapshot, ThreadPriority, ThreadSchedule, ThreadStatus, TickId, TickStore,
    VesselStatus,
};
use exoskeleton_relationship::{compile_relationship_snapshot, RelationshipLedger};
use exoskeleton_threads::ThreadRegistry;
use serde::{Deserialize, Serialize};
use worldinterface_core::descriptor::Descriptor;

use crate::budget::{CognitiveBudgetTracker, ToolBudgetGate};
use crate::config::VesselConfig;
use crate::kernel::WiHostSlot;
use crate::storage::StorageManager;

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
#[derive(Debug, Clone, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InspectionBudgetStatus {
    /// Cognitive budget from CognitiveBudgetTracker.
    pub cognitive: Option<CognitiveBudgetDetail>,
    /// Tool budget from ToolBudgetGate.
    pub tool: Option<ToolBudgetDetail>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolBudgetDetail {
    pub invocations_remaining: u64,
    pub invocations_this_window: u64,
    pub window_start: DateTime<Utc>,
}

/// Health and status of both engines.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineStatus {
    pub cognitive: CognitiveEngineStatus,
    pub tool: ToolEngineStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
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
        compile_relationship_snapshot(self.relationship_ledger.as_ref())
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
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

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
        exoskeleton_threads::register_builtin_threads(&inspector.thread_registry).unwrap();

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
}

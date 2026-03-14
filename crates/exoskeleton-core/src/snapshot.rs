//! State Snapshot model — the single authoritative view of vessel state (I7).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::id::{ArtifactId, ThreadId, VesselId};
use crate::thread::ThreadStatus;
use crate::ExoError;

/// The single authoritative view of a vessel's cognitive state (I7).
///
/// Updated once per tick. The master loop reads the Snapshot at tick start,
/// runs PODAARA, and writes a new Snapshot at tick end. Threads contribute
/// artifacts that inform but never directly mutate the Snapshot (IBP §4.3:
/// "Thread outputs never replace master loop").
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StateSnapshot {
    /// Which vessel this snapshot belongs to.
    pub vessel_id: VesselId,
    /// Monotonically increasing tick counter. Starts at 0 for a fresh vessel.
    pub tick_number: u64,
    /// Current high-level objective (the vessel's mission statement).
    pub mission: String,
    /// Current plan or strategy summary. Updated by the Decide/Amend steps.
    /// `None` if no plan has been formulated yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<String>,
    /// Current operational status of the vessel.
    pub status: VesselStatus,
    /// Current task or focus summary — what the vessel is working on right now.
    pub working_context: String,
    /// Compact summary of each active cognitive thread's state.
    pub thread_summaries: Vec<ThreadSummary>,
    /// Reference to the compiled relationship snapshot artifact.
    /// `None` if no relationship data exists yet (first tick, or no principals).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relationship_snapshot_ref: Option<ArtifactId>,
    /// Remaining budget across all tracked dimensions.
    pub budget_status: BudgetStatus,
    /// Summary of what happened in the most recent tick.
    /// `None` for the initial snapshot before any tick has run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_action_summary: Option<String>,
    /// When the vessel was started. Immutable after initial creation — preserved
    /// through all subsequent tick snapshots. `None` for snapshots persisted
    /// before this field was added (backward compatibility).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<DateTime<Utc>>,
    /// When this snapshot was created/updated.
    pub updated_at: DateTime<Utc>,
}

impl StateSnapshot {
    /// Create the initial snapshot for a new vessel.
    pub fn initial(vessel_id: VesselId, mission: String) -> Self {
        let now = Utc::now();
        Self {
            vessel_id,
            tick_number: 0,
            mission,
            plan: None,
            status: VesselStatus::Idle,
            working_context: String::new(),
            thread_summaries: Vec::new(),
            relationship_snapshot_ref: None,
            budget_status: BudgetStatus::unlimited(),
            last_action_summary: None,
            started_at: Some(now),
            updated_at: now,
        }
    }
}

/// Operational status of a vessel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VesselStatus {
    /// Vessel is idle, waiting for the next tick or external input.
    Idle,
    /// Vessel is in the Perceive/Orient phase — gathering context.
    Thinking,
    /// Vessel is in the Act phase — executing actions via the WI Host (Tool AQ).
    Acting,
    /// Vessel is in the Reflect/Amend phase — evaluating outcomes.
    Reflecting,
    /// Vessel has been suspended (budget exhaustion, operator pause, etc.).
    Suspended,
    /// Vessel is shutting down gracefully.
    Shutdown,
}

impl VesselStatus {
    /// Whether the vessel is in a terminal state (not expected to produce
    /// further ticks without external intervention).
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Suspended | Self::Shutdown)
    }
}

/// Compact summary of one cognitive thread's state for inclusion in the
/// State Snapshot. This is a compiled view, not the full thread state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThreadSummary {
    /// Which thread this summarizes.
    pub thread_id: ThreadId,
    /// Human-readable thread name (e.g., "Threat Monitor").
    pub name: String,
    /// Current operational status.
    pub status: ThreadStatus,
    /// One-line summary of the thread's most recent output.
    /// `None` if the thread hasn't produced output yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_output_summary: Option<String>,
    /// Remaining token budget for this thread (Cognitive AQ budget, I6).
    pub token_budget_remaining: u64,
}

/// Remaining budget across all tracked dimensions.
///
/// Reflects cognitive budget state (I6). Tool budget is managed separately
/// by the WI Host and reported independently (I9).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BudgetStatus {
    /// Remaining local model tokens this window.
    pub local_tokens_remaining: u64,
    /// Remaining frontier model tokens this window.
    pub frontier_tokens_remaining: u64,
    /// Remaining frontier cost in hundredths of a cent this window.
    pub frontier_cost_cents_remaining: u64,
    /// Remaining wall-clock seconds in this budget window.
    pub time_secs_remaining: u64,
    /// Current thrash level detected.
    pub thrash_level: crate::budget::ThrashLevel,
    /// Tool invocations remaining this window.
    pub tool_invocations_remaining: u64,
}

impl BudgetStatus {
    /// A budget with no constraints (all maximums).
    pub fn unlimited() -> Self {
        Self {
            local_tokens_remaining: u64::MAX,
            frontier_tokens_remaining: u64::MAX,
            frontier_cost_cents_remaining: u64::MAX,
            time_secs_remaining: u64::MAX,
            thrash_level: crate::budget::ThrashLevel::None,
            tool_invocations_remaining: u64::MAX,
        }
    }

    /// Whether any cognitive dimension is exhausted (zero remaining).
    pub fn is_exhausted(&self) -> bool {
        (self.local_tokens_remaining == 0 && self.frontier_tokens_remaining == 0)
            || self.frontier_cost_cents_remaining == 0
            || self.time_secs_remaining == 0
    }

    /// Total tokens remaining (local + frontier).
    pub fn total_tokens_remaining(&self) -> u64 {
        self.local_tokens_remaining
            .saturating_add(self.frontier_tokens_remaining)
    }
}

/// Durable store for State Snapshots (I7: single coherent workspace).
///
/// Each tick produces a new StateSnapshot. The store retains a configurable
/// number of recent snapshots for history and debugging.
///
/// Every snapshot saved here is also stored as an Artifact in the ArtifactStore
/// (via the caller — Sprint 5's Amend step). The SnapshotStore provides
/// tick-indexed access; the ArtifactStore provides content-addressed access.
/// Both are needed: tick-indexed for "what was the state at tick N?" and
/// content-addressed for "is this exact snapshot already stored?" (I3).
pub trait SnapshotStore: Send + Sync {
    /// Save a snapshot. Keyed by `tick_number` — overwrites are not allowed.
    ///
    /// Returns `ExoError::Storage` if a snapshot for this tick_number already
    /// exists. This enforces the invariant that each tick produces exactly one
    /// snapshot (I7).
    fn save(&self, snapshot: &StateSnapshot) -> Result<(), ExoError>;

    /// Get the most recently saved snapshot.
    ///
    /// Returns `None` if no snapshots have been saved yet (fresh vessel).
    fn latest(&self) -> Result<Option<StateSnapshot>, ExoError>;

    /// Get the snapshot at a specific tick number.
    ///
    /// Returns `None` if no snapshot exists for this tick.
    fn at_tick(&self, tick_number: u64) -> Result<Option<StateSnapshot>, ExoError>;

    /// Get the N most recent snapshots, newest first.
    ///
    /// Used for debugging and the `exo inspect` command (Sprint 10).
    fn history(&self, limit: usize) -> Result<Vec<StateSnapshot>, ExoError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── T-3: State Snapshot ──

    #[test]
    fn snapshot_json_roundtrip_full() {
        let snap = StateSnapshot {
            vessel_id: VesselId::new(),
            tick_number: 42,
            mission: "Test mission".into(),
            plan: Some("Execute plan A".into()),
            status: VesselStatus::Thinking,
            working_context: "Evaluating options".into(),
            thread_summaries: vec![ThreadSummary {
                thread_id: ThreadId::new(),
                name: "Threat Monitor".into(),
                status: ThreadStatus::Active,
                last_output_summary: Some("No threats detected".into()),
                token_budget_remaining: 5000,
            }],
            relationship_snapshot_ref: Some(ArtifactId::from_content(b"rel")),
            budget_status: BudgetStatus {
                local_tokens_remaining: 80_000,
                frontier_tokens_remaining: 20_000,
                frontier_cost_cents_remaining: 500,
                time_secs_remaining: 3600,
                thrash_level: crate::budget::ThrashLevel::None,
                tool_invocations_remaining: 950,
            },
            last_action_summary: Some("Created file".into()),
            started_at: Some(Utc::now()),
            updated_at: Utc::now(),
        };
        let json = serde_json::to_string(&snap).unwrap();
        let parsed: StateSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(snap, parsed);
    }

    #[test]
    fn snapshot_json_roundtrip_minimal() {
        let snap = StateSnapshot::initial(VesselId::new(), "Minimal mission".into());
        let json = serde_json::to_string(&snap).unwrap();
        let parsed: StateSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(snap, parsed);
    }

    #[test]
    fn snapshot_optional_fields_omitted() {
        let snap = StateSnapshot::initial(VesselId::new(), "Test".into());
        let value: serde_json::Value = serde_json::to_value(&snap).unwrap();
        let obj = value.as_object().unwrap();
        assert!(!obj.contains_key("plan"));
        assert!(!obj.contains_key("relationship_snapshot_ref"));
        assert!(!obj.contains_key("last_action_summary"));
    }

    #[test]
    fn snapshot_initial_constructor() {
        let vessel_id = VesselId::new();
        let snap = StateSnapshot::initial(vessel_id, "Mission Alpha".into());
        assert_eq!(snap.vessel_id, vessel_id);
        assert_eq!(snap.tick_number, 0);
        assert_eq!(snap.status, VesselStatus::Idle);
        assert_eq!(snap.budget_status, BudgetStatus::unlimited());
        assert!(snap.plan.is_none());
        assert!(snap.thread_summaries.is_empty());
        assert!(snap.relationship_snapshot_ref.is_none());
        assert!(snap.last_action_summary.is_none());
        assert!(snap.started_at.is_some());
    }

    #[test]
    fn vessel_status_all_variants_roundtrip() {
        let variants = [
            VesselStatus::Idle,
            VesselStatus::Thinking,
            VesselStatus::Acting,
            VesselStatus::Reflecting,
            VesselStatus::Suspended,
            VesselStatus::Shutdown,
        ];
        for status in &variants {
            let json = serde_json::to_string(status).unwrap();
            let parsed: VesselStatus = serde_json::from_str(&json).unwrap();
            assert_eq!(*status, parsed);
        }
    }

    #[test]
    fn vessel_status_is_terminal() {
        assert!(VesselStatus::Suspended.is_terminal());
        assert!(VesselStatus::Shutdown.is_terminal());
        assert!(!VesselStatus::Idle.is_terminal());
        assert!(!VesselStatus::Thinking.is_terminal());
        assert!(!VesselStatus::Acting.is_terminal());
        assert!(!VesselStatus::Reflecting.is_terminal());
    }

    #[test]
    fn budget_status_unlimited() {
        let b = BudgetStatus::unlimited();
        assert_eq!(b.local_tokens_remaining, u64::MAX);
        assert_eq!(b.frontier_tokens_remaining, u64::MAX);
        assert_eq!(b.frontier_cost_cents_remaining, u64::MAX);
        assert_eq!(b.time_secs_remaining, u64::MAX);
        assert_eq!(b.thrash_level, crate::budget::ThrashLevel::None);
        assert_eq!(b.tool_invocations_remaining, u64::MAX);
    }

    #[test]
    fn budget_status_is_exhausted() {
        // Both token dimensions zero → exhausted
        assert!(BudgetStatus {
            local_tokens_remaining: 0,
            frontier_tokens_remaining: 0,
            frontier_cost_cents_remaining: 100,
            time_secs_remaining: 100,
            thrash_level: crate::budget::ThrashLevel::None,
            tool_invocations_remaining: 100,
        }
        .is_exhausted());
        // Cost zero → exhausted
        assert!(BudgetStatus {
            local_tokens_remaining: 100,
            frontier_tokens_remaining: 100,
            frontier_cost_cents_remaining: 0,
            time_secs_remaining: 100,
            thrash_level: crate::budget::ThrashLevel::None,
            tool_invocations_remaining: 100,
        }
        .is_exhausted());
        // Time zero → exhausted
        assert!(BudgetStatus {
            local_tokens_remaining: 100,
            frontier_tokens_remaining: 100,
            frontier_cost_cents_remaining: 100,
            time_secs_remaining: 0,
            thrash_level: crate::budget::ThrashLevel::None,
            tool_invocations_remaining: 100,
        }
        .is_exhausted());
        // Only local zero but frontier has tokens → NOT exhausted
        assert!(!BudgetStatus {
            local_tokens_remaining: 0,
            frontier_tokens_remaining: 100,
            frontier_cost_cents_remaining: 100,
            time_secs_remaining: 100,
            thrash_level: crate::budget::ThrashLevel::None,
            tool_invocations_remaining: 100,
        }
        .is_exhausted());
        // All non-zero → not exhausted
        assert!(!BudgetStatus {
            local_tokens_remaining: 1,
            frontier_tokens_remaining: 1,
            frontier_cost_cents_remaining: 1,
            time_secs_remaining: 1,
            thrash_level: crate::budget::ThrashLevel::None,
            tool_invocations_remaining: 1,
        }
        .is_exhausted());
    }

    #[test]
    fn budget_status_total_tokens() {
        let b = BudgetStatus {
            local_tokens_remaining: 500,
            frontier_tokens_remaining: 300,
            frontier_cost_cents_remaining: 100,
            time_secs_remaining: 100,
            thrash_level: crate::budget::ThrashLevel::None,
            tool_invocations_remaining: 100,
        };
        assert_eq!(b.total_tokens_remaining(), 800);
    }

    #[test]
    fn thread_summary_roundtrip() {
        let ts = ThreadSummary {
            thread_id: ThreadId::new(),
            name: "Self-Critique".into(),
            status: ThreadStatus::Active,
            last_output_summary: Some("All checks passed".into()),
            token_budget_remaining: 2000,
        };
        let json = serde_json::to_string(&ts).unwrap();
        let parsed: ThreadSummary = serde_json::from_str(&json).unwrap();
        assert_eq!(ts, parsed);
    }
}

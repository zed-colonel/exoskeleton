//! Core domain types for Exoskeleton.
//!
//! Pure domain model crate with zero external runtime dependencies beyond
//! serde, uuid, chrono, sha2, and thiserror. No I/O, no ActionQueue, no
//! WorldInterface — this crate is the leaf of the dependency DAG.

pub mod artifact;
pub mod budget;
pub mod envelope;
pub mod error;
pub mod event;
pub mod id;
pub mod inbox;
pub mod llm;
pub mod memory;
pub mod relationship;
pub mod snapshot;
pub mod thread;
pub mod tick;

// Re-export all public types for ergonomic imports.
pub use artifact::{Artifact, ArtifactKind, ArtifactRef, ArtifactStore};
pub use budget::{
    BudgetDimensionState, BudgetState, BudgetStore, CognitiveBudgetConfig, EscalationPolicy,
    PersistedBudgetState, ThrashAssessment, ThrashLevel, ToolBudgetConfig,
};
pub use envelope::{EnvelopeKind, MessageEnvelope, RelationalSignal};
pub use error::ExoError;
pub use event::{EventEntry, EventLedger, EventType, LiveEvent};
pub use id::{
    sha256_hex, ArtifactId, ArtifactIdError, EnvelopeId, LedgerEntryId, PrincipalId, ThreadId,
    TickId, VesselId,
};
pub use inbox::Inbox;
pub use llm::{LlmBackend, LlmMessage, LlmRequest, LlmResponse, LlmRole, StopReason};
pub use memory::{EpisodicSummary, LongTermNote, MemoryTier};
pub use relationship::{
    PrincipalSummary, RelationalSignalType, RelationshipRecord, RelationshipSnapshot,
};
pub use snapshot::{BudgetStatus, SnapshotStore, StateSnapshot, ThreadSummary, VesselStatus};
pub use thread::{ThreadOutput, ThreadPriority, ThreadSchedule, ThreadSpec, ThreadStatus};
pub use tick::{
    ActionOutcome, ActionRecord, LlmCallRecord, ThreadContribution, TickPhase, TickRecord,
    TickStore,
};

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use proptest::prelude::*;

    use super::*;

    // ── T-10: Property-Based Tests ──

    proptest! {
        #[test]
        fn snapshot_survives_json_roundtrip(
            tick_number in 0u64..u64::MAX,
            status_idx in 0usize..6,
        ) {
            let statuses = [
                VesselStatus::Idle,
                VesselStatus::Thinking,
                VesselStatus::Acting,
                VesselStatus::Reflecting,
                VesselStatus::Suspended,
                VesselStatus::Shutdown,
            ];
            let status = statuses[status_idx];

            let snap = StateSnapshot {
                vessel_id: VesselId::new(),
                tick_number,
                mission: "proptest mission".into(),
                plan: None,
                status,
                working_context: String::new(),
                thread_summaries: Vec::new(),
                relationship_snapshot_ref: None,
                budget_status: BudgetStatus::unlimited(),
                last_action_summary: None,
                started_at: Some(Utc::now()),
                updated_at: Utc::now(),
            };
            let json = serde_json::to_string(&snap).unwrap();
            let parsed: StateSnapshot = serde_json::from_str(&json).unwrap();
            prop_assert_eq!(snap, parsed);
        }

        #[test]
        fn artifact_id_is_deterministic(ref bytes in proptest::collection::vec(any::<u8>(), 0..1024)) {
            let a = ArtifactId::from_content(bytes);
            let b = ArtifactId::from_content(bytes);
            prop_assert_eq!(a, b);
        }
    }

    #[test]
    fn tick_phase_next_covers_all() {
        let phases = TickPhase::sequence();
        for (i, phase) in phases.iter().enumerate() {
            if i < 6 {
                assert!(
                    phase.next().is_some(),
                    "Phase {:?} should have a successor",
                    phase
                );
            } else {
                assert!(phase.next().is_none(), "Amend should have no successor");
            }
        }
    }

    #[test]
    fn thread_priority_ord_is_total() {
        let priorities = [
            ThreadPriority::Background,
            ThreadPriority::Low,
            ThreadPriority::Normal,
            ThreadPriority::High,
            ThreadPriority::Critical,
        ];
        // Every earlier element is less than every later element
        for i in 0..priorities.len() {
            for j in (i + 1)..priorities.len() {
                assert!(
                    priorities[i] < priorities[j],
                    "{:?} should be less than {:?}",
                    priorities[i],
                    priorities[j]
                );
            }
        }
    }

    // ── T-11: Cross-Type Integration ──

    #[test]
    fn snapshot_as_artifact() {
        let snap = StateSnapshot::initial(VesselId::new(), "Integration test".into());
        let artifact = Artifact::from_json(ArtifactKind::Snapshot, &snap).unwrap();
        assert_eq!(artifact.kind, ArtifactKind::Snapshot);
        assert_eq!(artifact.content_type, "application/json");

        // ArtifactId is deterministic from content
        let artifact2 = Artifact::from_json(ArtifactKind::Snapshot, &snap).unwrap();
        assert_eq!(artifact.id, artifact2.id);
    }

    #[test]
    fn tick_record_references_valid_artifacts() {
        let snap = StateSnapshot::initial(VesselId::new(), "Artifact ref test".into());
        let snap_artifact = Artifact::from_json(ArtifactKind::Snapshot, &snap).unwrap();

        let record = TickRecord {
            tick_id: TickId::new(),
            tick_number: 1,
            phase: TickPhase::Amend,
            started_at: Utc::now(),
            completed_at: Some(Utc::now()),
            snapshot_before: snap_artifact.id.clone(),
            snapshot_after: None,
            thread_contributions: Vec::new(),
            actions_taken: Vec::new(),
            llm_calls: Vec::new(),
            decision_rationale: None,
        };

        assert_eq!(record.snapshot_before, snap_artifact.id);
    }

    #[test]
    fn envelope_with_relational_signal() {
        // Create a relational signal
        let signal = RelationalSignal {
            signal_type: RelationalSignalType::FeedbackReceived,
            principal_id: PrincipalId::new(),
            content: "Positive feedback on last output".into(),
            metadata: Default::default(),
        };

        // Store it as an artifact
        let artifact = Artifact::from_json(ArtifactKind::RelationshipEntry, &signal).unwrap();

        // Reference it in an envelope
        let envelope = MessageEnvelope {
            id: EnvelopeId::new(),
            source: signal.principal_id,
            target: None,
            kind: EnvelopeKind::RelationalSignal,
            payload_ref: artifact.id.clone(),
            timestamp: Utc::now(),
            in_reply_to: None,
        };

        // Full chain round-trips
        let env_json = serde_json::to_string(&envelope).unwrap();
        let parsed_env: MessageEnvelope = serde_json::from_str(&env_json).unwrap();
        assert_eq!(envelope, parsed_env);
        assert_eq!(parsed_env.payload_ref, artifact.id);

        // Signal round-trips from artifact content
        let parsed_signal: RelationalSignal = serde_json::from_slice(&artifact.content).unwrap();
        assert_eq!(signal, parsed_signal);
    }
}

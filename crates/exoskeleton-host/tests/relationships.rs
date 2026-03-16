//! Sprint 8: Relationship Substrate + Align Step integration tests.

mod common;

use std::sync::Arc;

use chrono::Utc;
use exoskeleton_core::{
    ArtifactId, ArtifactKind, ArtifactStore, LedgerEntryId, PrincipalId, RelationalSignalType,
    RelationshipRecord, RelationshipSnapshot, TickId, VesselId,
};
use exoskeleton_host::storage::StorageManager;
use exoskeleton_memory::ApproximateTokenCounter;
use exoskeleton_relationship::{
    compile_relationship_snapshot, InMemoryRelationshipLedger, RelationshipLedger,
};

fn make_record(
    principal_id: PrincipalId,
    signal_type: RelationalSignalType,
    tick_id: TickId,
    ts_offset: i64,
) -> RelationshipRecord {
    RelationshipRecord {
        id: LedgerEntryId::new(),
        principal_id,
        signal_type,
        content_ref: ArtifactId::from_content(
            format!("{:?}-{}", signal_type, ts_offset).as_bytes(),
        ),
        tick_id,
        timestamp: Utc::now() + chrono::Duration::seconds(ts_offset),
        metadata: Default::default(),
    }
}

// ── T-7: Basic Relationship Lifecycle ──

#[test]
fn relationship_ledger_lifecycle() {
    let dir = tempfile::tempdir().unwrap();
    let storage = StorageManager::open(dir.path()).unwrap();
    let ledger = storage.relationship_store();
    let p = PrincipalId::new();
    let tick_id = TickId::new();

    // Append signals
    ledger
        .append(&make_record(
            p,
            RelationalSignalType::TrustUpdate,
            tick_id,
            0,
        ))
        .unwrap();
    ledger
        .append(&make_record(
            p,
            RelationalSignalType::CommitmentMade,
            tick_id,
            1,
        ))
        .unwrap();

    // Verify
    assert_eq!(ledger.count().unwrap(), 2);
    let records = ledger.for_principal(p, 10).unwrap();
    assert_eq!(records.len(), 2);
    assert!(ledger.distinct_principals().unwrap().contains(&p));
}

#[test]
fn relationship_snapshot_from_ledger() {
    let dir = tempfile::tempdir().unwrap();
    let storage = StorageManager::open(dir.path()).unwrap();
    let ledger = storage.relationship_store();
    let p = PrincipalId::new();
    let tick_id = TickId::new();

    // Build some relationship history
    ledger
        .append(&make_record(
            p,
            RelationalSignalType::CommitmentMade,
            tick_id,
            0,
        ))
        .unwrap();
    ledger
        .append(&make_record(
            p,
            RelationalSignalType::CommitmentFulfilled,
            tick_id,
            1,
        ))
        .unwrap();

    // Compile snapshot
    let snapshot = compile_relationship_snapshot(ledger.as_ref()).unwrap();
    assert_eq!(snapshot.principals.len(), 1);
    assert_eq!(snapshot.principals[0].principal_id, p);
    // 0.5 + 0.05 = 0.55
    assert!(
        (snapshot.principals[0].trust_level - 0.55).abs() < f64::EPSILON,
        "expected 0.55, got {}",
        snapshot.principals[0].trust_level
    );
    assert_eq!(snapshot.principals[0].active_commitments, 0);
}

// ── T-9: Align Passthrough with No Relationships ──

#[test]
fn align_passthrough_no_relationships() {
    let ledger = InMemoryRelationshipLedger::new();
    let snapshot = compile_relationship_snapshot(&ledger).unwrap();
    assert!(snapshot.principals.is_empty());

    // With no principals, all actions should pass through
    let config = exoskeleton_relationship::AlignConfig::default();
    let actions = vec![
        exoskeleton_relationship::AlignAction {
            tool_name: "fs.write".into(),
            rationale: "test".into(),
        },
        exoskeleton_relationship::AlignAction {
            tool_name: "delay".into(),
            rationale: "test".into(),
        },
    ];
    let tick_id = TickId::new();

    let (approved, blocked, updates) =
        exoskeleton_relationship::check_alignment(&actions, &snapshot, &config, tick_id);
    assert_eq!(approved, vec![0, 1]);
    assert!(blocked.is_empty());
    assert!(updates.is_empty());
}

// ── T-8: Align Action Filtering ──

#[test]
fn align_blocks_actions_with_low_trust() {
    let ledger = InMemoryRelationshipLedger::new();
    let p = PrincipalId::new();
    let tick_id = TickId::new();

    // Drive trust well below threshold with broken commitments
    for i in 0..5 {
        ledger
            .append(&make_record(
                p,
                RelationalSignalType::CommitmentMade,
                tick_id,
                i * 2,
            ))
            .unwrap();
        ledger
            .append(&make_record(
                p,
                RelationalSignalType::CommitmentBroken,
                tick_id,
                i * 2 + 1,
            ))
            .unwrap();
    }

    let snapshot = compile_relationship_snapshot(&ledger).unwrap();
    assert!(snapshot.principals[0].trust_level < 0.3);

    let config = exoskeleton_relationship::AlignConfig::default();
    let actions = vec![exoskeleton_relationship::AlignAction {
        tool_name: "delay".into(),
        rationale: "test".into(),
    }];

    let (approved, blocked, _) =
        exoskeleton_relationship::check_alignment(&actions, &snapshot, &config, tick_id);
    assert!(approved.is_empty());
    assert_eq!(blocked.len(), 1);
    assert!(blocked[0].1.contains("trust gate"));
}

// ── T-10: Multi-Tick Relationship Evolution ──

#[test]
fn multi_tick_relationship_evolution() {
    let ledger = InMemoryRelationshipLedger::new();
    let p = PrincipalId::new();

    // Tick 1: initial signal
    let tick1 = TickId::new();
    ledger
        .append(&make_record(p, RelationalSignalType::TrustUpdate, tick1, 0))
        .unwrap();
    let snap1 = compile_relationship_snapshot(&ledger).unwrap();
    assert_eq!(snap1.principals.len(), 1);
    assert!((snap1.principals[0].trust_level - 0.5).abs() < f64::EPSILON);

    // Tick 2: commitment made
    let tick2 = TickId::new();
    ledger
        .append(&make_record(
            p,
            RelationalSignalType::CommitmentMade,
            tick2,
            10,
        ))
        .unwrap();
    let snap2 = compile_relationship_snapshot(&ledger).unwrap();
    assert_eq!(snap2.principals[0].active_commitments, 1);

    // Tick 3: commitment fulfilled
    let tick3 = TickId::new();
    ledger
        .append(&make_record(
            p,
            RelationalSignalType::CommitmentFulfilled,
            tick3,
            20,
        ))
        .unwrap();
    let snap3 = compile_relationship_snapshot(&ledger).unwrap();
    assert_eq!(snap3.principals[0].active_commitments, 0);
    assert!((snap3.principals[0].trust_level - 0.55).abs() < f64::EPSILON);

    // Each snapshot is recompiled fresh (I5), not carried forward
    // Re-compile from the same ledger produces identical results
    let snap3_again = compile_relationship_snapshot(&ledger).unwrap();
    assert_eq!(
        snap3.principals[0].trust_level,
        snap3_again.principals[0].trust_level
    );
}

// ── T-12: Relationship Durability (AC-B) ──

#[test]
fn relationship_durability_across_restart() {
    let dir = tempfile::tempdir().unwrap();
    let p1 = PrincipalId::new();
    let p2 = PrincipalId::new();

    // Phase 1: Build relationship history
    {
        let storage = StorageManager::open(dir.path()).unwrap();
        let ledger = storage.relationship_store();
        let tick1 = TickId::new();
        let tick2 = TickId::new();
        let tick3 = TickId::new();

        // p1: trust update, commitment made
        ledger
            .append(&make_record(
                p1,
                RelationalSignalType::TrustUpdate,
                tick1,
                0,
            ))
            .unwrap();
        ledger
            .append(&make_record(
                p1,
                RelationalSignalType::CommitmentMade,
                tick1,
                1,
            ))
            .unwrap();

        // p2: feedback
        ledger
            .append(&make_record(
                p2,
                RelationalSignalType::FeedbackReceived,
                tick2,
                2,
            ))
            .unwrap();

        // p1: commitment fulfilled
        ledger
            .append(&make_record(
                p1,
                RelationalSignalType::CommitmentFulfilled,
                tick3,
                3,
            ))
            .unwrap();

        // Verify pre-crash state
        assert_eq!(ledger.count().unwrap(), 4);
        let snap = compile_relationship_snapshot(ledger.as_ref()).unwrap();
        assert_eq!(snap.principals.len(), 2);
    }
    // Drop — simulates crash

    // Phase 2: Reopen and verify durability
    {
        let storage = StorageManager::open(dir.path()).unwrap();
        let ledger = storage.relationship_store();

        // All records preserved
        assert_eq!(ledger.count().unwrap(), 4);
        let principals = ledger.distinct_principals().unwrap();
        assert_eq!(principals.len(), 2);
        assert!(principals.contains(&p1));
        assert!(principals.contains(&p2));

        // Snapshot recompiled from ledger matches pre-crash state
        let snap = compile_relationship_snapshot(ledger.as_ref()).unwrap();
        assert_eq!(snap.principals.len(), 2);

        let s1 = snap
            .principals
            .iter()
            .find(|s| s.principal_id == p1)
            .unwrap();
        let s2 = snap
            .principals
            .iter()
            .find(|s| s.principal_id == p2)
            .unwrap();

        // p1: 0.5 (TrustUpdate) + 0.05 (CommitmentFulfilled) = 0.55
        assert!(
            (s1.trust_level - 0.55).abs() < f64::EPSILON,
            "expected 0.55, got {}",
            s1.trust_level
        );
        assert_eq!(s1.active_commitments, 0);

        // p2: 0.5 (neutral) + 0.02 (FeedbackReceived) = 0.52
        assert!(
            (s2.trust_level - 0.52).abs() < f64::EPSILON,
            "expected 0.52, got {}",
            s2.trust_level
        );
    }
}

// ── T-13: Atomic Ledger Consistency (AC-D) ──

#[test]
fn atomic_ledger_crash_consistency() {
    let dir = tempfile::tempdir().unwrap();
    let p = PrincipalId::new();

    // Write records
    {
        let storage = StorageManager::open(dir.path()).unwrap();
        let ledger = storage.relationship_store();

        for i in 0..10 {
            ledger
                .append(&make_record(
                    p,
                    RelationalSignalType::FeedbackReceived,
                    TickId::new(),
                    i,
                ))
                .unwrap();
        }

        assert_eq!(ledger.count().unwrap(), 10);
    }
    // Drop — simulate crash

    // Reopen and verify
    {
        let storage = StorageManager::open(dir.path()).unwrap();
        let ledger = storage.relationship_store();

        assert_eq!(ledger.count().unwrap(), 10);
        let records = ledger.for_principal(p, 20).unwrap();
        assert_eq!(records.len(), 10);

        // All records are complete (no partial writes)
        for r in &records {
            assert_eq!(r.principal_id, p);
            assert_eq!(r.signal_type, RelationalSignalType::FeedbackReceived);
        }

        // Snapshot compilation works correctly
        let snap = compile_relationship_snapshot(ledger.as_ref()).unwrap();
        assert_eq!(snap.principals.len(), 1);
    }
}

// ── T-14: Context Compilation with Relationships ──

#[test]
fn context_compilation_with_relationships() {
    let dir = tempfile::tempdir().unwrap();
    let storage = StorageManager::open(dir.path()).unwrap();
    let ledger = storage.relationship_store();
    let p = PrincipalId::new();

    // Build relationship data
    ledger
        .append(&make_record(
            p,
            RelationalSignalType::TrustUpdate,
            TickId::new(),
            0,
        ))
        .unwrap();
    ledger
        .append(&make_record(
            p,
            RelationalSignalType::FeedbackReceived,
            TickId::new(),
            1,
        ))
        .unwrap();

    // Compile relationship snapshot
    let rel_snapshot = compile_relationship_snapshot(ledger.as_ref()).unwrap();

    // Store as artifact and retrieve
    let artifact =
        exoskeleton_core::Artifact::from_json(ArtifactKind::RelationshipEntry, &rel_snapshot)
            .unwrap();
    let artifact_id = storage.artifact_store().put(&artifact).unwrap();

    // Verify we can deserialize the artifact
    let loaded = storage.artifact_store().get(&artifact_id).unwrap().unwrap();
    let loaded_snap: RelationshipSnapshot = serde_json::from_slice(&loaded.content).unwrap();
    assert_eq!(loaded_snap.principals.len(), 1);

    // Context compilation with relationship snapshot
    let vessel_id = VesselId::new();
    let snapshot = exoskeleton_core::StateSnapshot::initial(vessel_id, "test".into());
    let counter = Arc::new(ApproximateTokenCounter);
    let compiler = exoskeleton_memory::ContextCompiler::with_defaults(counter, 4000);

    let sources = exoskeleton_memory::ContextSources {
        vessel_id,
        mission: "test mission",
        snapshot: &snapshot,
        relationship_snapshot: Some(&loaded_snap),
        thread_contributions: &[],
        recent_events: &[],
        episodic_summaries: &[],
        long_term_notes: &[],
        working_context: "",
        system_section_override: None,
    };

    let compiled = compiler.compile(&sources).unwrap();

    // The prompt should contain the RELATIONSHIPS header
    assert!(
        compiled.prompt.contains("RELATIONSHIPS"),
        "prompt should contain RELATIONSHIPS header, got: {}",
        compiled.prompt
    );
}

// ── T-8 (extended): Destructive tool threshold ──

#[test]
fn align_destructive_tool_blocked_at_intermediate_trust() {
    let ledger = InMemoryRelationshipLedger::new();
    let p = PrincipalId::new();
    let tick_id = TickId::new();

    // Set trust to 0.4 — above normal threshold (0.3) but below destructive (0.6)
    let mut record = make_record(p, RelationalSignalType::TrustUpdate, tick_id, 0);
    record.metadata.insert("trust_level".into(), "0.4".into());
    ledger.append(&record).unwrap();

    let snapshot = compile_relationship_snapshot(&ledger).unwrap();
    let config = exoskeleton_relationship::AlignConfig::default();

    // Non-destructive tool should pass
    let non_destructive = vec![exoskeleton_relationship::AlignAction {
        tool_name: "delay".into(),
        rationale: "test".into(),
    }];
    let (approved, blocked, _) =
        exoskeleton_relationship::check_alignment(&non_destructive, &snapshot, &config, tick_id);
    assert_eq!(approved.len(), 1);
    assert!(blocked.is_empty());

    // Destructive tool should be blocked
    let destructive = vec![exoskeleton_relationship::AlignAction {
        tool_name: "fs.write".into(),
        rationale: "test".into(),
    }];
    let (approved, blocked, _) =
        exoskeleton_relationship::check_alignment(&destructive, &snapshot, &config, tick_id);
    assert!(approved.is_empty());
    assert_eq!(blocked.len(), 1);
    assert!(blocked[0].1.contains("destructive"));
}

// ── T-11: Signal Processing End-to-End ──

#[test]
fn signal_processing_end_to_end() {
    let dir = tempfile::tempdir().unwrap();
    let storage = StorageManager::open(dir.path()).unwrap();
    let artifact_store = storage.artifact_store();
    let relationship_store = storage.relationship_store();
    let tick_id = TickId::new();
    let principal = PrincipalId::new();

    // Create a RelationalSignal and store as artifact
    let signal = exoskeleton_core::RelationalSignal {
        signal_type: RelationalSignalType::TrustUpdate,
        principal_id: principal,
        content: "Trust established at 0.9".into(),
        metadata: {
            let mut m = std::collections::HashMap::new();
            m.insert("trust_level".into(), "0.9".into());
            m.insert("display_name".into(), "TestUser".into());
            m
        },
    };
    let artifact =
        exoskeleton_core::Artifact::from_json(ArtifactKind::RelationshipEntry, &signal).unwrap();
    let artifact_id = artifact_store.put(&artifact).unwrap();

    // Create an envelope referencing the artifact
    let envelope = exoskeleton_core::MessageEnvelope {
        id: exoskeleton_core::EnvelopeId::new(),
        source: principal,
        target: None,
        kind: exoskeleton_core::EnvelopeKind::RelationalSignal,
        payload_ref: artifact_id,
        timestamp: Utc::now(),
        in_reply_to: None,
    };

    // Process the signal
    let ids = exoskeleton_relationship::process_relational_signals(
        &[envelope],
        relationship_store.as_ref(),
        artifact_store.as_ref(),
        tick_id,
    );

    // Verify: signal processed
    assert_eq!(ids.len(), 1);
    assert_eq!(relationship_store.count().unwrap(), 1);

    // Verify: record has metadata from signal
    let records = relationship_store.for_principal(principal, 10).unwrap();
    assert_eq!(records[0].metadata.get("trust_level").unwrap(), "0.9");
    assert_eq!(records[0].metadata.get("display_name").unwrap(), "TestUser");

    // Verify: snapshot reflects the signal with correct trust
    let snapshot = compile_relationship_snapshot(relationship_store.as_ref()).unwrap();
    assert_eq!(snapshot.principals.len(), 1);
    assert!(
        (snapshot.principals[0].trust_level - 0.9).abs() < f64::EPSILON,
        "expected 0.9, got {}",
        snapshot.principals[0].trust_level
    );
    assert_eq!(snapshot.principals[0].display_name, "TestUser");
}

// ── T-14 (extended): Principal names and trust in prompt ──

#[test]
fn context_prompt_contains_principal_details() {
    let dir = tempfile::tempdir().unwrap();
    let storage = StorageManager::open(dir.path()).unwrap();
    let ledger = storage.relationship_store();
    let p = PrincipalId::new();

    let mut record = make_record(p, RelationalSignalType::TrustUpdate, TickId::new(), 0);
    record
        .metadata
        .insert("display_name".into(), "Alice".into());
    record.metadata.insert("role".into(), "operator".into());
    record.metadata.insert("trust_level".into(), "0.95".into());
    ledger.append(&record).unwrap();

    let rel_snapshot = compile_relationship_snapshot(ledger.as_ref()).unwrap();

    let vessel_id = VesselId::new();
    let snapshot = exoskeleton_core::StateSnapshot::initial(vessel_id, "test".into());
    let counter = Arc::new(ApproximateTokenCounter);
    let compiler = exoskeleton_memory::ContextCompiler::with_defaults(counter, 4000);

    let sources = exoskeleton_memory::ContextSources {
        vessel_id,
        mission: "test",
        snapshot: &snapshot,
        relationship_snapshot: Some(&rel_snapshot),
        thread_contributions: &[],
        recent_events: &[],
        episodic_summaries: &[],
        long_term_notes: &[],
        working_context: "",
        system_section_override: None,
    };

    let compiled = compiler.compile(&sources).unwrap();

    assert!(
        compiled.prompt.contains("Alice"),
        "prompt should contain principal name 'Alice'"
    );
    assert!(
        compiled.prompt.contains("0.95"),
        "prompt should contain trust level '0.95'"
    );
}

// ── T-5 (extended): SQLite metadata roundtrip ──

#[test]
fn sqlite_metadata_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("exo").join("relationships.db");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();

    use exoskeleton_host::storage::SqliteRelationshipLedger;
    let ledger = SqliteRelationshipLedger::open(&path).unwrap();
    let p = PrincipalId::new();

    let record = RelationshipRecord {
        id: LedgerEntryId::new(),
        principal_id: p,
        signal_type: RelationalSignalType::TrustUpdate,
        content_ref: ArtifactId::from_content(b"meta-test"),
        tick_id: TickId::new(),
        timestamp: Utc::now(),
        metadata: {
            let mut m = std::collections::HashMap::new();
            m.insert("trust_level".into(), "0.75".into());
            m.insert("display_name".into(), "Bob".into());
            m
        },
    };
    ledger.append(&record).unwrap();

    // Retrieve and check metadata survived the roundtrip
    let records = ledger.for_principal(p, 10).unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].metadata.get("trust_level").unwrap(), "0.75");
    assert_eq!(records[0].metadata.get("display_name").unwrap(), "Bob");

    // Reopen and verify durability
    drop(ledger);
    let ledger2 = SqliteRelationshipLedger::open(&path).unwrap();
    let records2 = ledger2.for_principal(p, 10).unwrap();
    assert_eq!(records2[0].metadata.get("trust_level").unwrap(), "0.75");
}

// ── Sprint 8 Property-Based Tests ──

mod proptest_tests {
    use chrono::Utc;
    use exoskeleton_core::{
        ArtifactId, LedgerEntryId, PrincipalId, RelationalSignalType, RelationshipRecord, TickId,
    };
    use exoskeleton_relationship::{
        compile_relationship_snapshot, AlignAction, AlignConfig, InMemoryRelationshipLedger,
        RelationshipLedger,
    };
    use proptest::prelude::*;

    fn arb_signal_type() -> impl Strategy<Value = RelationalSignalType> {
        prop_oneof![
            Just(RelationalSignalType::TrustUpdate),
            Just(RelationalSignalType::CommitmentMade),
            Just(RelationalSignalType::CommitmentFulfilled),
            Just(RelationalSignalType::CommitmentBroken),
            Just(RelationalSignalType::FeedbackReceived),
            Just(RelationalSignalType::AlignmentCheck),
            Just(RelationalSignalType::AlignmentMismatch),
            Just(RelationalSignalType::ToneObservation),
        ]
    }

    proptest! {
        /// Snapshot compilation never panics for arbitrary signal sequences.
        #[test]
        fn snapshot_compilation_never_panics(
            signal_count in 0usize..50,
            signal_types in proptest::collection::vec(arb_signal_type(), 0..50),
        ) {
            let ledger = InMemoryRelationshipLedger::new();
            let p = PrincipalId::new();
            let tick_id = TickId::new();
            let base = Utc::now();

            for (i, st) in signal_types.iter().take(signal_count).enumerate() {
                let record = RelationshipRecord {
                    id: LedgerEntryId::new(),
                    principal_id: p,
                    signal_type: *st,
                    content_ref: ArtifactId::from_content(format!("sig-{i}").as_bytes()),
                    tick_id,
                    timestamp: base + chrono::Duration::seconds(i as i64),
                    metadata: Default::default(),
                };
                let _ = ledger.append(&record);
            }

            let result = compile_relationship_snapshot(&ledger);
            prop_assert!(result.is_ok());
        }

        /// Trust is always in [0.0, 1.0] after arbitrary signal sequences.
        #[test]
        fn trust_always_in_range(
            signal_types in proptest::collection::vec(arb_signal_type(), 1..50),
        ) {
            let ledger = InMemoryRelationshipLedger::new();
            let p = PrincipalId::new();
            let tick_id = TickId::new();
            let base = Utc::now();

            for (i, st) in signal_types.iter().enumerate() {
                let record = RelationshipRecord {
                    id: LedgerEntryId::new(),
                    principal_id: p,
                    signal_type: *st,
                    content_ref: ArtifactId::from_content(format!("sig-{i}").as_bytes()),
                    tick_id,
                    timestamp: base + chrono::Duration::seconds(i as i64),
                    metadata: Default::default(),
                };
                let _ = ledger.append(&record);
            }

            let snap = compile_relationship_snapshot(&ledger).unwrap();
            for principal in &snap.principals {
                prop_assert!(principal.trust_level >= 0.0);
                prop_assert!(principal.trust_level <= 1.0);
            }
        }

        /// check_alignment never panics for arbitrary trust levels and action lists.
        #[test]
        fn check_alignment_never_panics(
            trust in 0.0f64..1.0,
            action_count in 0usize..10,
        ) {
            let snapshot = exoskeleton_core::RelationshipSnapshot {
                principals: vec![exoskeleton_core::PrincipalSummary {
                    principal_id: PrincipalId::new(),
                    display_name: "test".into(),
                    role: "test".into(),
                    trust_level: trust,
                    active_commitments: 0,
                    last_interaction: None,
                    notes: None,
                }],
                compiled_at: Utc::now(),
            };

            let config = AlignConfig::default();
            let actions: Vec<AlignAction> = (0..action_count)
                .map(|i| AlignAction {
                    tool_name: format!("tool_{i}"),
                    rationale: "test".into(),
                })
                .collect();

            let tick_id = TickId::new();
            let (approved, blocked, _) =
                exoskeleton_relationship::check_alignment(&actions, &snapshot, &config, tick_id);

            // All actions accounted for
            prop_assert_eq!(approved.len() + blocked.len(), action_count);
        }
    }
}

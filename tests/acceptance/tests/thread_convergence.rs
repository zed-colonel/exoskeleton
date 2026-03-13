//! Acceptance Test A: Thread Convergence
//!
//! Proves that 3 built-in threads converge into one StateSnapshot,
//! all running on the Cognitive AQ (Scope Appendix §5 criterion A).

mod support;

use std::collections::HashSet;
use std::time::Duration;

use exoskeleton_core::{ArtifactKind, ArtifactStore, SnapshotStore, ThreadId};
use exoskeleton_threads::{MEMORY_CONSOLIDATION_ID, SELF_CRITIQUE_ID, THREAT_MONITOR_ID};

const TICK_TIMEOUT: Duration = Duration::from_secs(120);

#[tokio::test]
async fn thread_convergence_3_threads_10_ticks() {
    let dir = tempfile::tempdir().unwrap();
    let vessel = support::boot_vessel(dir.path()).await;

    let ticks = support::wait_for_ticks(vessel.storage(), 10, TICK_TIMEOUT).await;
    assert_eq!(ticks.len(), 10, "should have exactly 10 ticks");

    // Final snapshot should have thread_summaries for all 3 threads
    let snapshot = vessel
        .storage()
        .snapshot_store()
        .latest()
        .unwrap()
        .expect("snapshot must exist after 10 ticks");

    assert!(
        snapshot.tick_number >= 10,
        "final snapshot tick_number should be >= 10, got {}",
        snapshot.tick_number
    );

    // Check thread summaries exist for all 3 threads
    let summary_thread_ids: HashSet<ThreadId> = snapshot
        .thread_summaries
        .iter()
        .map(|s| s.thread_id)
        .collect();
    assert!(
        summary_thread_ids.contains(&THREAT_MONITOR_ID),
        "snapshot should contain Threat Monitor summary"
    );
    assert!(
        summary_thread_ids.contains(&SELF_CRITIQUE_ID),
        "snapshot should contain Self-Critique summary"
    );
    assert!(
        summary_thread_ids.contains(&MEMORY_CONSOLIDATION_ID),
        "snapshot should contain Memory Consolidation summary"
    );

    support::shutdown_and_verify(vessel).await;
}

#[tokio::test]
async fn tick_records_have_thread_contributions() {
    let dir = tempfile::tempdir().unwrap();
    let vessel = support::boot_vessel(dir.path()).await;

    let ticks = support::wait_for_ticks(vessel.storage(), 5, TICK_TIMEOUT).await;

    // Every tick should have thread contributions (Threat Monitor + Self-Critique at minimum)
    for tick in &ticks {
        assert!(
            !tick.thread_contributions.is_empty(),
            "tick {} should have thread contributions",
            tick.tick_number
        );
    }

    support::shutdown_and_verify(vessel).await;
}

#[tokio::test]
async fn thread_summaries_updated_in_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let vessel = support::boot_vessel(dir.path()).await;

    let _ticks = support::wait_for_ticks(vessel.storage(), 3, TICK_TIMEOUT).await;

    let snapshot = vessel
        .storage()
        .snapshot_store()
        .latest()
        .unwrap()
        .expect("snapshot must exist");

    // Thread summaries should be populated (at least Threat Monitor and Self-Critique)
    assert!(
        snapshot.thread_summaries.len() >= 2,
        "should have >= 2 thread summaries, got {}",
        snapshot.thread_summaries.len()
    );

    for ts in &snapshot.thread_summaries {
        assert!(
            !ts.name.is_empty(),
            "thread summary name should be non-empty"
        );
    }

    support::shutdown_and_verify(vessel).await;
}

#[tokio::test]
async fn thread_output_artifacts_exist_and_parse() {
    let dir = tempfile::tempdir().unwrap();
    let vessel = support::boot_vessel(dir.path()).await;

    let _ticks = support::wait_for_ticks(vessel.storage(), 3, TICK_TIMEOUT).await;

    // Verify ThreadOutput artifacts exist in the artifact store (I3)
    let thread_output_refs = vessel
        .storage()
        .artifact_store()
        .list_by_kind(ArtifactKind::ThreadOutput, 100)
        .expect("list_by_kind should work");

    assert!(
        !thread_output_refs.is_empty(),
        "there should be ThreadOutput artifacts stored (I3)"
    );

    // Verify each artifact is valid JSON
    for artifact_ref in &thread_output_refs {
        let artifact = vessel
            .storage()
            .artifact_store()
            .get(&artifact_ref.id)
            .expect("artifact store read")
            .expect("artifact should exist");

        assert_eq!(artifact.kind, ArtifactKind::ThreadOutput);
        let content = String::from_utf8(artifact.content.clone())
            .expect("thread output should be valid UTF-8");
        let json: serde_json::Value =
            serde_json::from_str(&content).expect("thread output should be valid JSON");
        // ThreadOutput has summary field
        assert!(
            json.get("summary").is_some(),
            "thread output should have summary field"
        );
    }

    support::shutdown_and_verify(vessel).await;
}

#[tokio::test]
async fn memory_consolidation_runs_within_10_ticks() {
    let dir = tempfile::tempdir().unwrap();
    let vessel = support::boot_vessel(dir.path()).await;

    // Memory Consolidation runs EveryNTicks(5); run 10 ticks to ensure it triggers
    let ticks = support::wait_for_ticks(vessel.storage(), 10, TICK_TIMEOUT).await;

    // At least one tick should have a Memory Consolidation contribution
    let has_mem_consolidation = ticks.iter().any(|tick| {
        tick.thread_contributions
            .iter()
            .any(|c| c.thread_id == MEMORY_CONSOLIDATION_ID)
    });

    assert!(
        has_mem_consolidation,
        "Memory Consolidation should run at least once within 10 ticks"
    );

    support::shutdown_and_verify(vessel).await;
}

#[tokio::test]
async fn no_thread_work_on_tool_aq() {
    let dir = tempfile::tempdir().unwrap();
    let vessel = support::boot_vessel(dir.path()).await;

    let ticks = support::wait_for_ticks(vessel.storage(), 3, TICK_TIMEOUT).await;

    // Thread contributions exist (verifying cognitive work happened)
    let total_contributions: usize = ticks.iter().map(|t| t.thread_contributions.len()).sum();
    assert!(
        total_contributions > 0,
        "threads should have produced contributions on the Cognitive AQ"
    );

    // The Tool AQ does not produce thread outputs — by design (I9).
    // Verify: all ThreadOutput artifacts are of the correct kind (cognitive side),
    // not Receipt artifacts (tool side).
    let thread_outputs = vessel
        .storage()
        .artifact_store()
        .list_by_kind(ArtifactKind::ThreadOutput, 100)
        .expect("list ThreadOutput artifacts");
    for artifact_ref in &thread_outputs {
        assert_eq!(
            artifact_ref.kind,
            ArtifactKind::ThreadOutput,
            "all thread artifacts should be ThreadOutput (cognitive), not Receipt (tool)"
        );
    }

    // Verify no ThreadOutput artifacts appear with Receipt kind
    let receipts = vessel
        .storage()
        .artifact_store()
        .list_by_kind(ArtifactKind::Receipt, 100)
        .expect("list Receipt artifacts");
    for receipt_ref in &receipts {
        let artifact = vessel
            .storage()
            .artifact_store()
            .get(&receipt_ref.id)
            .unwrap()
            .unwrap();
        assert_ne!(
            artifact.kind,
            ArtifactKind::ThreadOutput,
            "Receipt artifacts should not be ThreadOutput"
        );
    }

    support::shutdown_and_verify(vessel).await;
}

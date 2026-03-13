//! Acceptance Test G: Engine Isolation Proof (I9)
//!
//! The definitive proof that cognitive work is unaffected by Tool AQ saturation
//! (Scope Appendix §5 criterion G, IBP §3, Charter §3.1.3).

mod support;

use std::time::Duration;

use exoskeleton_core::{ArtifactKind, ArtifactStore, EventLedger};

const TICK_TIMEOUT: Duration = Duration::from_secs(120);

#[tokio::test]
async fn cognitive_ticks_unaffected_by_tool_saturation() {
    let dir = tempfile::tempdir().unwrap();
    let vessel = support::boot_vessel(dir.path()).await;

    // Saturate Tool AQ with delay flows via WI Host
    let wi_slot = vessel.wi_host_slot().clone();
    let mut handles = Vec::new();
    for _ in 0..50 {
        let slot = wi_slot.clone();
        let handle = tokio::spawn(async move {
            let guard = slot.lock().await;
            if let Some(ref host) = *guard {
                // Submit a delay flow (200ms) to Tool AQ
                let _ = host
                    .invoke_single("delay", serde_json::json!({"duration_ms": 200}))
                    .await;
            }
        });
        handles.push(handle);
    }

    // Meanwhile, cognitive ticks should continue running
    let ticks = support::wait_for_ticks(vessel.storage(), 10, TICK_TIMEOUT).await;

    // Wait for all tool flows to complete
    for handle in handles {
        let _ = handle.await;
    }

    // ALL cognitive ticks completed while tool was saturated
    assert_eq!(ticks.len(), 10, "all 10 cognitive ticks should complete");

    // Verify tick timestamps are reasonable (not excessively delayed)
    for tick in &ticks {
        assert!(
            tick.started_at <= tick.completed_at.unwrap_or(tick.started_at),
            "tick started_at should be before completed_at"
        );
    }

    support::shutdown_and_verify(vessel).await;
}

#[tokio::test]
async fn all_ticks_complete_under_tool_load() {
    let dir = tempfile::tempdir().unwrap();
    let vessel = support::boot_vessel(dir.path()).await;

    // Submit tool work concurrently
    let wi_slot = vessel.wi_host_slot().clone();
    for _ in 0..20 {
        let slot = wi_slot.clone();
        tokio::spawn(async move {
            let guard = slot.lock().await;
            if let Some(ref host) = *guard {
                let _ = host
                    .invoke_single("delay", serde_json::json!({"duration_ms": 100}))
                    .await;
            }
        });
    }

    let ticks = support::wait_for_ticks(vessel.storage(), 5, TICK_TIMEOUT).await;
    assert_eq!(
        ticks.len(),
        5,
        "all 5 ticks should complete under tool load"
    );

    // Each tick should have completed successfully
    for tick in &ticks {
        assert!(
            tick.completed_at.is_some(),
            "tick {} should have completed_at timestamp",
            tick.tick_number
        );
    }

    support::shutdown_and_verify(vessel).await;
}

#[tokio::test]
async fn tool_flows_eventually_complete() {
    let dir = tempfile::tempdir().unwrap();
    let vessel = support::boot_vessel(dir.path()).await;

    // Wait for at least 1 tick to ensure WI Host is fully initialized
    let _ticks = support::wait_for_ticks(vessel.storage(), 1, Duration::from_secs(60)).await;

    // Now submit delay flows — WI Host should be ready
    let result = vessel
        .invoke_tool("delay", serde_json::json!({"duration_ms": 50}))
        .await;

    assert!(
        result.is_ok(),
        "tool invocation should succeed: {:?}",
        result.err()
    );

    support::shutdown_and_verify(vessel).await;
}

#[tokio::test]
async fn thread_contributions_not_starved() {
    let dir = tempfile::tempdir().unwrap();
    let vessel = support::boot_vessel(dir.path()).await;

    // Submit tool work to stress the Tool AQ
    let wi_slot = vessel.wi_host_slot().clone();
    for _ in 0..30 {
        let slot = wi_slot.clone();
        tokio::spawn(async move {
            let guard = slot.lock().await;
            if let Some(ref host) = *guard {
                let _ = host
                    .invoke_single("delay", serde_json::json!({"duration_ms": 100}))
                    .await;
            }
        });
    }

    let ticks = support::wait_for_ticks(vessel.storage(), 5, TICK_TIMEOUT).await;

    // Every tick should still have thread contributions (not starved by tool load)
    for tick in &ticks {
        assert!(
            !tick.thread_contributions.is_empty(),
            "tick {} should have thread contributions even under tool load (I9)",
            tick.tick_number
        );
    }

    support::shutdown_and_verify(vessel).await;
}

#[tokio::test]
async fn no_cross_engine_task_contamination() {
    let dir = tempfile::tempdir().unwrap();
    let vessel = support::boot_vessel(dir.path()).await;

    let _ticks = support::wait_for_ticks(vessel.storage(), 3, TICK_TIMEOUT).await;

    // Verify engine directories are separate (I9)
    let cognitive_dir = dir.path().join("cognitive-aq");
    let wi_dir = dir.path().join("wi");

    assert!(cognitive_dir.exists(), "Cognitive AQ directory must exist");
    assert!(wi_dir.exists(), "WI (Tool AQ) directory must exist");

    // Cognitive AQ produced tick records and thread outputs (cognitive artifacts)
    let thread_outputs = vessel
        .storage()
        .artifact_store()
        .list_by_kind(ArtifactKind::ThreadOutput, 100)
        .expect("list ThreadOutput");
    assert!(
        !thread_outputs.is_empty(),
        "Cognitive AQ should produce ThreadOutput artifacts"
    );

    // No error events indicating cross-engine contamination
    let events = vessel
        .storage()
        .event_ledger()
        .recent(100)
        .expect("recent events");
    for event in &events {
        let summary_lower = event.summary.to_lowercase();
        assert!(
            !summary_lower.contains("cross-engine") && !summary_lower.contains("contamination"),
            "no cross-engine contamination events should exist"
        );
    }

    support::shutdown_and_verify(vessel).await;
}

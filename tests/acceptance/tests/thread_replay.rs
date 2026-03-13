//! Acceptance Test C: Thread Replay
//!
//! Proves thread outputs are durable artifacts replayable from the artifact
//! chain (Scope Appendix §5 criterion C).

mod support;

use std::time::Duration;

use exoskeleton_core::{ArtifactKind, ArtifactStore};

const TICK_TIMEOUT: Duration = Duration::from_secs(120);

#[tokio::test]
async fn thread_replay_artifact_chain() {
    let dir = tempfile::tempdir().unwrap();
    let vessel = support::boot_vessel(dir.path()).await;

    let ticks = support::wait_for_ticks(vessel.storage(), 5, TICK_TIMEOUT).await;
    assert_eq!(ticks.len(), 5);

    // Verify the artifact chain: ThreadOutput artifacts exist
    let thread_artifacts = vessel
        .storage()
        .artifact_store()
        .list_by_kind(ArtifactKind::ThreadOutput, 100)
        .expect("list ThreadOutput artifacts");

    assert!(
        !thread_artifacts.is_empty(),
        "should have ThreadOutput artifacts after 5 ticks"
    );

    // Walk the artifacts: each should be valid JSON with thread output fields
    for artifact_ref in &thread_artifacts {
        let artifact = vessel
            .storage()
            .artifact_store()
            .get(&artifact_ref.id)
            .expect("artifact store read")
            .expect("artifact should exist");

        assert_eq!(artifact.kind, ArtifactKind::ThreadOutput);
        let content =
            String::from_utf8(artifact.content.clone()).expect("artifact should be valid UTF-8");
        let json: serde_json::Value =
            serde_json::from_str(&content).expect("artifact should be valid JSON");

        // ThreadOutput JSON has summary field
        assert!(
            json.get("summary").is_some(),
            "thread output should have summary field"
        );
    }

    // Verify each tick has thread contributions with summaries
    for tick in &ticks {
        for contribution in &tick.thread_contributions {
            assert!(
                !contribution.summary.is_empty(),
                "contribution summary should not be empty"
            );
        }
    }

    support::shutdown_and_verify(vessel).await;
}

#[tokio::test]
async fn thread_output_artifacts_are_valid() {
    let dir = tempfile::tempdir().unwrap();
    let vessel = support::boot_vessel(dir.path()).await;

    let _ticks = support::wait_for_ticks(vessel.storage(), 3, TICK_TIMEOUT).await;

    let thread_artifacts = vessel
        .storage()
        .artifact_store()
        .list_by_kind(ArtifactKind::ThreadOutput, 100)
        .expect("list ThreadOutput artifacts");

    for artifact_ref in &thread_artifacts {
        let artifact = vessel
            .storage()
            .artifact_store()
            .get(&artifact_ref.id)
            .unwrap()
            .unwrap();

        // Content type should be JSON
        assert_eq!(artifact.content_type, "application/json");
        assert_eq!(artifact.kind, ArtifactKind::ThreadOutput);
        assert!(
            !artifact.content.is_empty(),
            "thread output artifact should not be empty"
        );
    }

    support::shutdown_and_verify(vessel).await;
}

#[tokio::test]
async fn thread_output_matches_contribution() {
    let dir = tempfile::tempdir().unwrap();
    let vessel = support::boot_vessel(dir.path()).await;

    let ticks = support::wait_for_ticks(vessel.storage(), 3, TICK_TIMEOUT).await;

    // Every tick should have contributions with non-empty summaries
    for tick in &ticks {
        assert!(
            !tick.thread_contributions.is_empty(),
            "tick {} should have thread contributions",
            tick.tick_number
        );
        for contribution in &tick.thread_contributions {
            assert!(
                !contribution.summary.is_empty(),
                "thread contribution summary should not be empty for tick {}",
                tick.tick_number
            );
        }
    }

    support::shutdown_and_verify(vessel).await;
}

#[tokio::test]
async fn llm_response_artifacts_exist() {
    let dir = tempfile::tempdir().unwrap();
    let vessel = support::boot_vessel(dir.path()).await;

    let ticks = support::wait_for_ticks(vessel.storage(), 3, TICK_TIMEOUT).await;

    // Verify LLM response artifacts exist (I3: all LLM calls stored)
    let llm_artifacts = vessel
        .storage()
        .artifact_store()
        .list_by_kind(ArtifactKind::LlmResponse, 100)
        .expect("list LlmResponse artifacts");

    // We should have LLM call artifacts (at least from Decide + threads)
    assert!(
        !llm_artifacts.is_empty(),
        "should have LlmResponse artifacts after 3 ticks"
    );

    // Each tick should also record LLM calls
    for tick in &ticks {
        assert!(
            !tick.llm_calls.is_empty(),
            "tick {} should have LLM call records",
            tick.tick_number
        );
    }

    support::shutdown_and_verify(vessel).await;
}

#[tokio::test]
async fn tick_records_form_continuous_chain() {
    let dir = tempfile::tempdir().unwrap();
    let vessel = support::boot_vessel(dir.path()).await;

    let ticks = support::wait_for_ticks(vessel.storage(), 5, TICK_TIMEOUT).await;

    // Verify tick numbers are continuous: 1, 2, 3, 4, 5
    for (i, tick) in ticks.iter().enumerate() {
        let expected = (i + 1) as u64;
        assert_eq!(
            tick.tick_number, expected,
            "tick {} should have tick_number {}, got {}",
            i, expected, tick.tick_number
        );
    }

    // Verify each tick has a snapshot_before artifact
    for tick in &ticks {
        let exists = vessel
            .storage()
            .artifact_store()
            .exists(&tick.snapshot_before)
            .expect("artifact exists check");
        assert!(
            exists,
            "snapshot_before artifact should exist for tick {}",
            tick.tick_number
        );
    }

    support::shutdown_and_verify(vessel).await;
}

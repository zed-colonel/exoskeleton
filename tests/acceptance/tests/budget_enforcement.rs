//! Acceptance Test E: Budget Enforcement
//!
//! Proves cognitive and tool budgets are configured and tracked independently
//! (Scope Appendix §5 criterion E, I6, I9).
//!
//! Tests verify:
//! - Budget config validation works end-to-end
//! - BudgetStatus appears correctly in StateSnapshot
//! - Cognitive and tool budget dimensions are independent
//! - Budget-less vessel runs without interference
//! - Budget-enabled vessel boots, ticks, and shuts down (no deadlock)
//! - Cognitive budget exhaustion is reflected in BudgetStatus
//! - Tool budget gate is initialized and enforces rate limits
//! - Budget window timer resets counters

mod support;

use std::time::Duration;

use exoskeleton_core::{CognitiveBudgetConfig, EscalationPolicy, SnapshotStore, ToolBudgetConfig};
use exoskeleton_host::budget::tracker::CognitiveBudgetCheck;

const TICK_TIMEOUT: Duration = Duration::from_secs(120);

// ── Existing config-level tests ──

#[tokio::test]
async fn budget_status_in_snapshot_defaults_to_unlimited() {
    let dir = tempfile::tempdir().unwrap();
    let vessel = support::boot_vessel(dir.path()).await;

    let _ticks = support::wait_for_ticks(vessel.storage(), 2, TICK_TIMEOUT).await;

    let snapshot = vessel
        .storage()
        .snapshot_store()
        .latest()
        .unwrap()
        .expect("snapshot should exist");

    // Without budget config, BudgetStatus should show unlimited defaults
    // (u64::MAX or large values indicating no cap)
    assert!(
        snapshot.budget_status.local_tokens_remaining > 0,
        "default budget should show tokens remaining > 0"
    );

    support::shutdown_and_verify(vessel).await;
}

#[tokio::test]
async fn budget_config_validation_works() {
    let dir = tempfile::tempdir().unwrap();

    // Valid budget config
    let mut config = support::test_config(dir.path());
    config.cognitive_budget = Some(CognitiveBudgetConfig::default());
    config.tool_budget = Some(ToolBudgetConfig::default());
    config
        .validate()
        .expect("valid config should pass validation");

    // Invalid: zero time window
    let mut bad_config = support::test_config(dir.path());
    bad_config.cognitive_budget = Some(CognitiveBudgetConfig {
        time_window_secs: 0,
        ..Default::default()
    });
    assert!(
        bad_config.validate().is_err(),
        "zero time_window_secs should fail validation"
    );

    // Invalid: zero tool invocations
    let mut bad_config = support::test_config(dir.path());
    bad_config.tool_budget = Some(ToolBudgetConfig {
        max_invocations_per_window: 0,
        ..Default::default()
    });
    assert!(
        bad_config.validate().is_err(),
        "zero max_invocations should fail validation"
    );
}

#[tokio::test]
async fn cognitive_and_tool_budgets_independent_types() {
    // Verify the budget types are structurally independent (I9, I6)
    let cognitive = CognitiveBudgetConfig {
        local_token_budget: 1_000_000,
        frontier_token_budget: 100_000,
        frontier_cost_budget_cents: 500,
        time_window_secs: 3600,
        per_tick_token_cap: 50_000,
        per_thread_token_cap: 10_000,
        escalation_policy: EscalationPolicy::default(),
    };

    let tool = ToolBudgetConfig {
        max_invocations_per_window: 1000,
        time_window_secs: 3600,
    };

    // These are completely separate structs with no shared state
    assert_eq!(cognitive.local_token_budget, 1_000_000);
    assert_eq!(tool.max_invocations_per_window, 1000);

    // Validation is independent
    cognitive.validate().expect("cognitive config valid");
    tool.validate().expect("tool config valid");
}

#[tokio::test]
async fn vessel_ticks_without_budget_config() {
    let dir = tempfile::tempdir().unwrap();
    let vessel = support::boot_vessel(dir.path()).await;

    // Without budgets, vessel should tick freely
    let ticks = support::wait_for_ticks(vessel.storage(), 5, TICK_TIMEOUT).await;
    assert_eq!(
        ticks.len(),
        5,
        "5 ticks should complete without budget config"
    );

    // Budget tracker and gate should be None
    assert!(
        vessel.budget_tracker().is_none(),
        "no budget tracker when cognitive_budget is None"
    );
    assert!(
        vessel.tool_budget_gate().is_none(),
        "no tool gate when tool_budget is None"
    );

    support::shutdown_and_verify(vessel).await;
}

#[tokio::test]
async fn budget_status_snapshot_reflects_no_budget() {
    let dir = tempfile::tempdir().unwrap();
    let vessel = support::boot_vessel(dir.path()).await;

    let _ticks = support::wait_for_ticks(vessel.storage(), 3, TICK_TIMEOUT).await;

    let snapshot = vessel
        .storage()
        .snapshot_store()
        .latest()
        .unwrap()
        .expect("snapshot");

    // With no budget config, BudgetStatus should be the "unlimited" default
    let bs = &snapshot.budget_status;
    assert!(
        !bs.is_exhausted(),
        "budget should not be exhausted without configuration"
    );
    assert!(
        bs.total_tokens_remaining() > 0,
        "total tokens remaining should be > 0"
    );

    support::shutdown_and_verify(vessel).await;
}

#[tokio::test]
async fn escalation_policy_defaults_reasonable() {
    let policy = EscalationPolicy::default();

    // Default should allow escalation after 3 consecutive failures
    assert!(
        policy.escalate_on_consecutive_failures > 0,
        "escalation threshold should be > 0"
    );

    // Default frontier call limit should be reasonable
    assert!(
        policy.max_frontier_calls_per_window > 0,
        "frontier call limit should be > 0"
    );
}

// ── Runtime budget enforcement tests ──

/// Proves the shutdown deadlock fix: a budget-enabled Vessel boots, runs ticks,
/// and shuts down cleanly without hanging.
#[tokio::test]
async fn budget_enabled_vessel_boots_ticks_and_shuts_down() {
    let dir = tempfile::tempdir().unwrap();
    let config = support::test_config_with_budget(
        dir.path(),
        Some(CognitiveBudgetConfig::default()),
        Some(ToolBudgetConfig::default()),
    );
    let vessel = support::boot_vessel_with_config(config).await;

    // Verify budget components are initialized
    assert!(
        vessel.budget_tracker().is_some(),
        "cognitive budget tracker should be Some with config"
    );
    assert!(
        vessel.tool_budget_gate().is_some(),
        "tool budget gate should be Some with config"
    );

    // Run ticks to prove the vessel is functional
    let ticks = support::wait_for_ticks(vessel.storage(), 5, TICK_TIMEOUT).await;
    assert_eq!(ticks.len(), 5, "5 ticks should complete with budget config");

    // Shutdown must complete without hanging (the deadlock fix)
    let shutdown_timeout = tokio::time::timeout(Duration::from_secs(30), vessel.shutdown());
    shutdown_timeout
        .await
        .expect("shutdown must not hang (deadlock fix)")
        .expect("shutdown must succeed");
}

/// Cognitive budget tracker records consumption during ticks.
/// After several ticks, remaining tokens should be less than the initial budget.
#[tokio::test]
async fn cognitive_budget_tracks_consumption() {
    let dir = tempfile::tempdir().unwrap();
    let cognitive = CognitiveBudgetConfig {
        local_token_budget: 100_000,
        frontier_token_budget: 0,
        time_window_secs: 3600,
        ..Default::default()
    };
    let config = support::test_config_with_budget(dir.path(), Some(cognitive), None);
    let vessel = support::boot_vessel_with_config(config).await;

    // Run enough ticks for measurable consumption
    // Each tick: ~3 LLM calls × 15 tokens = ~45 tokens
    let _ticks = support::wait_for_ticks(vessel.storage(), 5, TICK_TIMEOUT).await;

    // Check that the tracker has recorded some consumption
    let tracker = vessel.budget_tracker().as_ref().unwrap();
    let remaining = {
        let guard = tracker.lock().await;
        guard.remaining_local_tokens()
    };

    assert!(
        remaining < 100_000,
        "local tokens remaining ({remaining}) should be less than initial budget (100_000)"
    );

    // BudgetStatus in snapshot should also reflect consumption
    let snapshot = vessel
        .storage()
        .snapshot_store()
        .latest()
        .unwrap()
        .expect("snapshot");
    assert!(
        snapshot.budget_status.local_tokens_remaining < 100_000,
        "snapshot BudgetStatus should reflect token consumption"
    );

    support::shutdown_and_verify(vessel).await;
}

/// With a very small cognitive budget, exhaustion is reached and reflected
/// in the BudgetStatus snapshot.
#[tokio::test]
async fn cognitive_budget_exhaustion_reflected_in_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    // Very small budget: ~200 tokens. With ~45 tokens/tick, exhausted after ~4 ticks.
    // frontier_cost_budget_cents must be > 0 because AQ allocates CostCents dimension.
    let cognitive = CognitiveBudgetConfig {
        local_token_budget: 200,
        frontier_token_budget: 0,
        frontier_cost_budget_cents: 1,
        time_window_secs: 3600,
        per_tick_token_cap: 200,
        per_thread_token_cap: 200,
        escalation_policy: EscalationPolicy::default(),
    };
    let config = support::test_config_with_budget(dir.path(), Some(cognitive), None);
    let vessel = support::boot_vessel_with_config(config).await;

    // Wait for enough ticks that budget should be exhausted.
    // 200 tokens / ~45 per tick ≈ 4-5 ticks. After tick 5, the AQ BudgetGate
    // blocks further dispatch since cumulative consumption exceeds the budget.
    let _ticks = support::wait_for_ticks(vessel.storage(), 5, TICK_TIMEOUT).await;

    // Give a moment for the final tick's consumption to be recorded
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Budget tracker should report exhausted
    let tracker = vessel.budget_tracker().as_ref().unwrap();
    let (remaining, check) = {
        let guard = tracker.lock().await;
        (
            guard.remaining_local_tokens(),
            guard.check_cognitive_budget(),
        )
    };
    assert_eq!(
        remaining, 0,
        "local tokens should be fully consumed after exhaustion"
    );
    assert_eq!(
        check,
        CognitiveBudgetCheck::Exhausted,
        "check_cognitive_budget() should return Exhausted"
    );

    // BudgetStatus in snapshot should reflect exhaustion
    let snapshot = vessel
        .storage()
        .snapshot_store()
        .latest()
        .unwrap()
        .expect("snapshot");
    assert!(
        snapshot.budget_status.is_exhausted(),
        "BudgetStatus.is_exhausted() should be true"
    );
    assert_eq!(
        snapshot.budget_status.local_tokens_remaining, 0,
        "snapshot should show 0 local tokens remaining"
    );

    support::shutdown_and_verify(vessel).await;
}

/// Tool budget gate is initialized from config and enforces rate limits.
#[tokio::test]
async fn tool_budget_gate_initialized_and_enforces() {
    let dir = tempfile::tempdir().unwrap();
    let tool = ToolBudgetConfig {
        max_invocations_per_window: 3,
        time_window_secs: 3600,
    };
    let config = support::test_config_with_budget(dir.path(), None, Some(tool));
    let vessel = support::boot_vessel_with_config(config).await;

    // Gate should exist
    let gate = vessel
        .tool_budget_gate()
        .as_ref()
        .expect("gate should exist");

    // Initially, all 3 invocations should be allowed
    {
        let guard = gate.lock().await;
        assert!(guard.check(), "gate should allow invocations initially");
        assert_eq!(guard.remaining(), 3, "should have 3 remaining");
    }

    // Simulate 3 tool invocations (as the Act step would)
    {
        let mut guard = gate.lock().await;
        guard.record_invocation();
        guard.record_invocation();
        guard.record_invocation();
    }

    // Now the gate should block
    {
        let guard = gate.lock().await;
        assert!(
            !guard.check(),
            "gate should block after max_invocations reached"
        );
        assert_eq!(guard.remaining(), 0, "should have 0 remaining");
    }

    // After reset, invocations should be allowed again
    {
        let mut guard = gate.lock().await;
        guard.reset_window();
        assert!(guard.check(), "gate should allow invocations after reset");
        assert_eq!(guard.remaining(), 3, "should have 3 remaining after reset");
    }

    support::shutdown_and_verify(vessel).await;
}

/// Budget window timer fires and resets counters.
///
/// Uses a 2-second window. After ticks consume tokens, we wait for the timer
/// to fire and verify that counters are replenished.
#[tokio::test]
async fn budget_window_timer_resets_counters() {
    let dir = tempfile::tempdir().unwrap();
    // frontier_cost_budget_cents must be > 0 because AQ allocates CostCents dimension.
    let cognitive = CognitiveBudgetConfig {
        local_token_budget: 500,
        frontier_token_budget: 0,
        frontier_cost_budget_cents: 1,
        time_window_secs: 2, // 2-second window for fast test
        per_tick_token_cap: 500,
        per_thread_token_cap: 500,
        escalation_policy: EscalationPolicy::default(),
    };
    let config = support::test_config_with_budget(dir.path(), Some(cognitive), None);
    let vessel = support::boot_vessel_with_config(config).await;

    // Run ticks to consume some budget
    let _ticks = support::wait_for_ticks(vessel.storage(), 3, TICK_TIMEOUT).await;

    // Verify some consumption happened
    let tracker = vessel.budget_tracker().as_ref().unwrap();
    let pre_reset_remaining = {
        let guard = tracker.lock().await;
        guard.remaining_local_tokens()
    };
    assert!(
        pre_reset_remaining < 500,
        "tokens should be partially consumed before reset ({pre_reset_remaining})"
    );

    // Wait for the 2-second window to expire and timer to fire.
    // Add margin for timer scheduling (window timer skips first tick).
    tokio::time::sleep(Duration::from_secs(3)).await;

    // After window reset, budget should be replenished
    let post_reset_remaining = {
        let guard = tracker.lock().await;
        guard.remaining_local_tokens()
    };
    assert!(
        post_reset_remaining > pre_reset_remaining,
        "tokens should be replenished after window reset \
         (before={pre_reset_remaining}, after={post_reset_remaining})"
    );

    support::shutdown_and_verify(vessel).await;
}

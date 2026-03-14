//! Sprint 9: Budget Enforcement + Escalation integration tests.

mod common;

use std::sync::Arc;

use chrono::Utc;
use exoskeleton_core::llm::LlmBackend;
use exoskeleton_core::{ArtifactId, TickId};
use exoskeleton_host::config::VesselConfig;
use exoskeleton_host::storage::StorageManager;

#[tokio::test]
async fn s9_cognitive_budget_tracker_records_consumption() {
    // CognitiveBudgetTracker tracks LLM consumption by backend
    use exoskeleton_core::budget::{BudgetStore, CognitiveBudgetConfig};
    use exoskeleton_host::budget::tracker::{CognitiveBudgetTracker, InMemoryBudgetStore};

    let config = CognitiveBudgetConfig {
        local_token_budget: 10_000,
        frontier_token_budget: 5_000,
        frontier_cost_budget_cents: 100,
        time_window_secs: 3600,
        per_tick_token_cap: 5_000,
        per_thread_token_cap: 1_000,
        ..Default::default()
    };
    let store: Arc<dyn BudgetStore> = Arc::new(InMemoryBudgetStore::new());
    let mut tracker = CognitiveBudgetTracker::new(config, store);

    // Record local call
    tracker.record_llm_call(LlmBackend::Local, 500, 300, 0.0);
    assert_eq!(tracker.remaining_local_tokens(), 9_200);
    assert_eq!(tracker.remaining_frontier_tokens(), 5_000);

    // Record frontier call
    tracker.record_llm_call(LlmBackend::Frontier, 200, 100, 0.25);
    assert_eq!(tracker.remaining_frontier_tokens(), 4_700);
    assert_eq!(tracker.remaining_local_tokens(), 9_200); // unaffected

    // BudgetStatus reflects state
    let status = tracker.budget_status(950);
    assert_eq!(status.local_tokens_remaining, 9_200);
    assert_eq!(status.frontier_tokens_remaining, 4_700);
    assert_eq!(status.tool_invocations_remaining, 950);
}

#[tokio::test]
async fn s9_tool_budget_gate_independent_of_cognitive() {
    // Tool budget gate operates independently from cognitive budget (I6, I9)
    use exoskeleton_core::budget::{BudgetStore, CognitiveBudgetConfig, ToolBudgetConfig};
    use exoskeleton_host::budget::tracker::{CognitiveBudgetTracker, InMemoryBudgetStore};
    use exoskeleton_host::budget::ToolBudgetGate;

    let cog_config = CognitiveBudgetConfig {
        local_token_budget: 1_000, // very tight
        ..Default::default()
    };
    let tool_config = ToolBudgetConfig {
        max_invocations_per_window: 5,
        time_window_secs: 3600,
    };
    let store: Arc<dyn BudgetStore> = Arc::new(InMemoryBudgetStore::new());
    let mut cog_tracker = CognitiveBudgetTracker::new(cog_config, store);
    let mut tool_gate = ToolBudgetGate::new(tool_config);

    // Exhaust cognitive budget
    cog_tracker.record_llm_call(LlmBackend::Local, 500, 600, 0.0); // 1100 > 1000
    assert_eq!(cog_tracker.remaining_local_tokens(), 0);

    // Tool budget should be unaffected (I9)
    assert!(tool_gate.check());
    assert_eq!(tool_gate.remaining(), 5);
    tool_gate.record_invocation();
    assert_eq!(tool_gate.remaining(), 4);

    // Exhaust tool budget
    for _ in 0..4 {
        tool_gate.record_invocation();
    }
    assert!(!tool_gate.check());

    // Cognitive tracker still reports exhaustion independently
    assert_eq!(cog_tracker.remaining_local_tokens(), 0);
}

#[tokio::test]
async fn s9_budget_window_reset() {
    use exoskeleton_core::budget::{BudgetStore, CognitiveBudgetConfig, ToolBudgetConfig};
    use exoskeleton_host::budget::tracker::{CognitiveBudgetTracker, InMemoryBudgetStore};
    use exoskeleton_host::budget::ToolBudgetGate;

    let config = CognitiveBudgetConfig {
        local_token_budget: 5_000,
        frontier_token_budget: 2_000,
        ..Default::default()
    };
    let tool_config = ToolBudgetConfig {
        max_invocations_per_window: 10,
        time_window_secs: 3600,
    };
    let store: Arc<dyn BudgetStore> = Arc::new(InMemoryBudgetStore::new());
    let mut tracker = CognitiveBudgetTracker::new(config, store);
    let mut gate = ToolBudgetGate::new(tool_config);

    // Consume budget
    tracker.record_llm_call(LlmBackend::Local, 2000, 1500, 0.0);
    gate.record_invocation();
    gate.record_invocation();
    gate.record_invocation();

    assert_eq!(tracker.remaining_local_tokens(), 1_500);
    assert_eq!(gate.remaining(), 7);

    // Reset window
    tracker.reset_window().unwrap();
    gate.reset_window();

    assert_eq!(tracker.remaining_local_tokens(), 5_000);
    assert_eq!(tracker.remaining_frontier_tokens(), 2_000);
    assert_eq!(gate.remaining(), 10);
}

#[tokio::test]
async fn s9_budget_persistence_across_restart() {
    use exoskeleton_core::budget::{BudgetStore, CognitiveBudgetConfig};
    use exoskeleton_host::budget::tracker::CognitiveBudgetTracker;
    use exoskeleton_host::storage::SqliteBudgetStore;

    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("budget.db");

    let config = CognitiveBudgetConfig {
        local_token_budget: 10_000,
        frontier_token_budget: 5_000,
        ..Default::default()
    };

    // First session: consume some budget and persist
    {
        let store: Arc<dyn BudgetStore> = Arc::new(SqliteBudgetStore::open(&db_path).unwrap());
        let mut tracker = CognitiveBudgetTracker::new(config.clone(), store);
        tracker.record_llm_call(LlmBackend::Local, 3000, 1000, 0.0);
        tracker.record_llm_call(LlmBackend::Frontier, 500, 200, 0.10);
        tracker.record_failure();
        tracker.persist().unwrap();
    }

    // Second session: load state
    {
        let store: Arc<dyn BudgetStore> = Arc::new(SqliteBudgetStore::open(&db_path).unwrap());
        let mut tracker = CognitiveBudgetTracker::new(config, store);
        tracker.load_or_reset().unwrap();

        assert_eq!(tracker.remaining_local_tokens(), 6_000); // 10000 - 4000
        assert_eq!(tracker.remaining_frontier_tokens(), 4_300); // 5000 - 700
        assert_eq!(tracker.consecutive_failures(), 1);
    }
}

#[tokio::test]
async fn s9_model_escalation_on_consecutive_failures() {
    use exoskeleton_core::budget::{BudgetStore, CognitiveBudgetConfig, EscalationPolicy};
    use exoskeleton_host::budget::tracker::{
        CognitiveBudgetCheck, CognitiveBudgetTracker, InMemoryBudgetStore,
    };

    let config = CognitiveBudgetConfig {
        escalation_policy: EscalationPolicy {
            escalate_on_consecutive_failures: 2,
            ..Default::default()
        },
        ..Default::default()
    };
    let store: Arc<dyn BudgetStore> = Arc::new(InMemoryBudgetStore::new());
    let mut tracker = CognitiveBudgetTracker::new(config, store);

    // Initially: budget available (local)
    assert_eq!(
        tracker.check_cognitive_budget(),
        CognitiveBudgetCheck::Available(LlmBackend::Local)
    );

    // Record failures
    tracker.record_failure();
    assert_eq!(tracker.consecutive_failures(), 1);
    tracker.record_failure();
    assert_eq!(tracker.consecutive_failures(), 2);

    // Clear on success
    tracker.clear_failures();
    assert_eq!(tracker.consecutive_failures(), 0);
}

#[tokio::test]
async fn s9_thrash_detection_identifies_patterns() {
    use exoskeleton_core::budget::ThrashLevel;
    use exoskeleton_core::tick::{
        ActionOutcome, ActionRecord, LlmCallRecord, TickPhase, TickRecord,
    };
    use exoskeleton_host::budget::ThrashDetector;

    // Helper to build ticks
    let make_tick = |actions: Vec<ActionRecord>, llm_calls: Vec<LlmCallRecord>| -> TickRecord {
        TickRecord {
            tick_id: TickId::new(),
            tick_number: 0,
            phase: TickPhase::Amend,
            started_at: Utc::now(),
            completed_at: Some(Utc::now()),
            snapshot_before: ArtifactId::from_content(b"before"),
            snapshot_after: None,
            thread_contributions: vec![],
            actions_taken: actions,
            llm_calls,
            decision_rationale: None,
        }
    };

    // No thrash on empty
    assert_eq!(ThrashDetector::check(&[]).level, ThrashLevel::None);

    // No thrash on successful ticks
    let successful_ticks: Vec<_> = (0..5)
        .map(|_| {
            make_tick(
                vec![ActionRecord {
                    action_type: "fs.write".into(),
                    target: "t".into(),
                    receipt_ref: None,
                    outcome: ActionOutcome::Success,
                }],
                vec![LlmCallRecord {
                    model: "m".into(),
                    tokens_in: 500,
                    tokens_out: 300,
                    cost_cents: 0.0,
                    latency_ms: 100,
                    response_artifact_ref: None,
                }],
            )
        })
        .collect();
    assert_eq!(
        ThrashDetector::check(&successful_ticks).level,
        ThrashLevel::None
    );

    // High thrash on repeated failures
    let failing_ticks: Vec<_> = (0..5)
        .map(|_| {
            make_tick(
                vec![ActionRecord {
                    action_type: "http.request".into(),
                    target: "t".into(),
                    receipt_ref: None,
                    outcome: ActionOutcome::Failure,
                }],
                vec![LlmCallRecord {
                    model: "m".into(),
                    tokens_in: 500,
                    tokens_out: 300,
                    cost_cents: 0.0,
                    latency_ms: 100,
                    response_artifact_ref: None,
                }],
            )
        })
        .collect();
    let assessment = ThrashDetector::check(&failing_ticks);
    assert_eq!(assessment.level, ThrashLevel::High);
    assert!(!assessment.indicators.is_empty());
    assert!(assessment.recommendation.is_some());
}

#[tokio::test]
async fn s9_per_thread_budget_enforcement() {
    use exoskeleton_core::budget::{BudgetStore, CognitiveBudgetConfig};
    use exoskeleton_core::ThreadId;
    use exoskeleton_host::budget::tracker::{CognitiveBudgetTracker, InMemoryBudgetStore};

    let config = CognitiveBudgetConfig {
        per_thread_token_cap: 500,
        ..Default::default()
    };
    let store: Arc<dyn BudgetStore> = Arc::new(InMemoryBudgetStore::new());
    let mut tracker = CognitiveBudgetTracker::new(config, store);

    let thread_a = ThreadId::new();
    let thread_b = ThreadId::new();

    // Both threads start within budget
    assert!(tracker.check_thread_budget(thread_a));
    assert!(tracker.check_thread_budget(thread_b));

    // Thread A consumes 400 tokens — still within cap
    tracker.record_thread_consumption(thread_a, 400);
    assert!(tracker.check_thread_budget(thread_a));
    assert!(tracker.check_thread_budget(thread_b)); // unaffected

    // Thread A consumes 200 more — over cap (600 > 500)
    tracker.record_thread_consumption(thread_a, 200);
    assert!(!tracker.check_thread_budget(thread_a));
    assert!(tracker.check_thread_budget(thread_b)); // still unaffected

    // Thread B consumes 500 — exactly at cap, not over
    tracker.record_thread_consumption(thread_b, 500);
    assert!(!tracker.check_thread_budget(thread_b)); // >= cap
}

#[tokio::test]
async fn s9_vessel_config_with_budgets() {
    use exoskeleton_core::budget::{CognitiveBudgetConfig, ToolBudgetConfig};

    let dir = tempfile::tempdir().unwrap();
    let config = VesselConfig {
        cognitive_budget: Some(CognitiveBudgetConfig::default()),
        tool_budget: Some(ToolBudgetConfig::default()),
        ..common::test_config(dir.path())
    };

    // Validation should pass
    config.validate().unwrap();

    // Fields present
    assert!(config.cognitive_budget.is_some());
    assert!(config.tool_budget.is_some());
    let cb = config.cognitive_budget.unwrap();
    assert_eq!(cb.local_token_budget, 1_000_000);
    assert_eq!(cb.frontier_token_budget, 100_000);
    assert_eq!(cb.frontier_cost_budget_cents, 500);
    assert_eq!(cb.escalation_policy.escalate_on_consecutive_failures, 3);

    let tb = config.tool_budget.unwrap();
    assert_eq!(tb.max_invocations_per_window, 1000);
}

#[tokio::test]
async fn s9_vessel_config_budget_validation() {
    use exoskeleton_core::budget::{CognitiveBudgetConfig, ToolBudgetConfig};

    let dir = tempfile::tempdir().unwrap();

    // Zero window should fail validation
    let config = VesselConfig {
        cognitive_budget: Some(CognitiveBudgetConfig {
            time_window_secs: 0,
            ..Default::default()
        }),
        ..common::test_config(dir.path())
    };
    assert!(config.validate().is_err());

    // Zero tool invocations should fail
    let config = VesselConfig {
        tool_budget: Some(ToolBudgetConfig {
            max_invocations_per_window: 0,
            ..Default::default()
        }),
        ..common::test_config(dir.path())
    };
    assert!(config.validate().is_err());
}

#[tokio::test]
async fn s9_storage_manager_budget_store() {
    // Budget store accessible via StorageManager
    use exoskeleton_core::budget::BudgetStore;

    let dir = tempfile::tempdir().unwrap();
    let mgr = StorageManager::open(dir.path()).unwrap();

    // Initially empty
    assert!(mgr.budget_store().load().unwrap().is_none());

    // Budget.db file created
    assert!(dir.path().join("exo").join("budget.db").exists());
}

#[tokio::test]
async fn s9_thrash_assessment_none_is_none() {
    use exoskeleton_core::budget::{ThrashAssessment, ThrashLevel};

    let none = ThrashAssessment::none();
    assert_eq!(none.level, ThrashLevel::None);
    assert!(none.indicators.is_empty());
    assert!(none.recommendation.is_none());
}

#[tokio::test]
async fn s9_budget_status_exhaustion_logic() {
    use exoskeleton_core::budget::ThrashLevel;
    use exoskeleton_core::BudgetStatus;

    // Both token types zero -> exhausted
    let status = BudgetStatus {
        local_tokens_remaining: 0,
        frontier_tokens_remaining: 0,
        frontier_cost_cents_remaining: 100,
        time_secs_remaining: 100,
        thrash_level: ThrashLevel::None,
        tool_invocations_remaining: 100,
    };
    assert!(status.is_exhausted());

    // Only local zero, frontier available -> NOT exhausted
    let status = BudgetStatus {
        local_tokens_remaining: 0,
        frontier_tokens_remaining: 100,
        frontier_cost_cents_remaining: 100,
        time_secs_remaining: 100,
        thrash_level: ThrashLevel::None,
        tool_invocations_remaining: 100,
    };
    assert!(!status.is_exhausted());

    // Total tokens calculation
    assert_eq!(status.total_tokens_remaining(), 100);
}

#[tokio::test]
async fn s9_budget_config_toml_roundtrip() {
    use exoskeleton_host::config::VesselConfigFile;

    let toml_str = r#"
[vessel]
mission = "budget test"
data_dir = "/tmp/exo"

[cognitive]
tick_interval_ms = 100

[cognitive.budget]
local_token_budget = 500000
frontier_token_budget = 50000
frontier_cost_budget_cents = 200
time_window_secs = 3600
per_tick_token_cap = 25000
per_thread_token_cap = 8000

[cognitive.budget.escalation_policy]
escalate_on_uncertainty = 0.7
escalate_on_stakes = ["fs.write"]
escalate_on_consecutive_failures = 3
max_frontier_calls_per_window = 20

[tool]
tick_interval_ms = 50

[tool.budget]
max_invocations_per_window = 500
time_window_secs = 3600
"#;
    let file: VesselConfigFile = toml::from_str(toml_str).unwrap();
    let config = VesselConfig::try_from(file).unwrap();

    let cb = config.cognitive_budget.unwrap();
    assert_eq!(cb.local_token_budget, 500_000);
    assert_eq!(cb.frontier_token_budget, 50_000);
    assert_eq!(cb.frontier_cost_budget_cents, 200);
    assert_eq!(cb.per_tick_token_cap, 25_000);
    assert_eq!(cb.per_thread_token_cap, 8_000);
    assert_eq!(cb.escalation_policy.escalate_on_consecutive_failures, 3);
    assert_eq!(cb.escalation_policy.escalate_on_stakes, vec!["fs.write"]);
    assert!((cb.escalation_policy.escalate_on_uncertainty - 0.7).abs() < f64::EPSILON);
    assert_eq!(cb.escalation_policy.max_frontier_calls_per_window, 20);

    let tb = config.tool_budget.unwrap();
    assert_eq!(tb.max_invocations_per_window, 500);
    assert_eq!(tb.time_window_secs, 3600);
}

#[tokio::test]
async fn s9_tool_budget_gate_rate_limiting() {
    use exoskeleton_core::budget::ToolBudgetConfig;
    use exoskeleton_host::budget::ToolBudgetGate;

    let config = ToolBudgetConfig {
        max_invocations_per_window: 3,
        time_window_secs: 3600,
    };
    let mut gate = ToolBudgetGate::new(config);

    // First 3 invocations allowed
    assert!(gate.check());
    gate.record_invocation();
    assert!(gate.check());
    gate.record_invocation();
    assert!(gate.check());
    gate.record_invocation();

    // 4th blocked
    assert!(!gate.check());
    assert_eq!(gate.remaining(), 0);

    // Reset restores budget
    gate.reset_window();
    assert!(gate.check());
    assert_eq!(gate.remaining(), 3);
}

#[tokio::test]
async fn s9_per_tick_token_cap() {
    use exoskeleton_core::budget::{BudgetStore, CognitiveBudgetConfig};
    use exoskeleton_host::budget::tracker::{CognitiveBudgetTracker, InMemoryBudgetStore};

    let config = CognitiveBudgetConfig {
        per_tick_token_cap: 1_000,
        ..Default::default()
    };
    let store: Arc<dyn BudgetStore> = Arc::new(InMemoryBudgetStore::new());
    let mut tracker = CognitiveBudgetTracker::new(config, store);

    // Within cap
    assert!(tracker.check_tick_cap());
    tracker.record_llm_call(LlmBackend::Local, 400, 300, 0.0);
    assert!(tracker.check_tick_cap());

    // Over cap
    tracker.record_llm_call(LlmBackend::Local, 200, 200, 0.0);
    assert!(!tracker.check_tick_cap());

    // Reset tick counters
    tracker.reset_tick_counters();
    assert!(tracker.check_tick_cap());
}

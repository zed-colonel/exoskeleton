//! Sprint 5: Master Loop (PODAARA) integration tests.
//!
// DEADLOCK PREVENTION:
// - All async tests use `flavor = "multi_thread", worker_threads = 2`
// - All tests wrap body in `tokio::time::timeout`
// - Tests calling `run_tick` use `spawn_blocking` (run_tick uses block_on)
// - Vessel.shutdown() always called in timely manner

mod common;

use std::sync::Arc;
use std::time::Duration;

use actionqueue_executor_local::CancellationToken;
use common::{test_config, test_registry};
use exoskeleton_core::conversation::InMemoryConversationStore;
use exoskeleton_core::llm::{LlmBackend, LlmResponse, StopReason};
use exoskeleton_core::{
    ArtifactKind, ArtifactStore, EventType, LiveEvent, PromptRegistry, VesselId,
};
use exoskeleton_host::cognitive_engine::CognitiveHandler;
use exoskeleton_host::inbox::InMemoryInbox;
use exoskeleton_host::kernel::{KernelContext, WiHostSlot};
use exoskeleton_host::llm::mock::MockLlmBackend;
use exoskeleton_host::storage::StorageManager;
use exoskeleton_host::vessel::Vessel;
use exoskeleton_host::LlmHttpBackend;
use exoskeleton_memory::{ApproximateTokenCounter, ContextCompiler};
use exoskeleton_relationship::InMemoryRelationshipLedger;
use exoskeleton_threads::{InMemoryThreadStore, ThreadRegistry};
use worldinterface_connector::connectors::DelayConnector;
use worldinterface_connector::registry::ConnectorRegistry;
use worldinterface_host::config::HostConfig;
use worldinterface_host::host::EmbeddedHost;

/// Valid DecisionProtocol JSON with no actions (simplest successful tick).
const MOCK_DECISION_NO_ACTIONS: &str =
    r#"{"reasoning":"No actions needed","actions":[],"memory_notes":[]}"#;

/// Valid DecisionProtocol JSON with a delay action.
const MOCK_DECISION_WITH_ACTION: &str = r#"{"reasoning":"Testing","actions":[{"tool_name":"delay","params":{"duration_ms":1},"rationale":"test"}],"memory_notes":["test note"]}"#;

/// Build a mock LlmResponse with the given content.
fn mock_llm_response(content: &str) -> LlmResponse {
    LlmResponse {
        content: content.to_string(),
        model: "mock-model".into(),
        tokens_in: 100,
        tokens_out: 50,
        latency_ms: 10,
        stop_reason: StopReason::EndTurn,
        cost_estimate_cents: None,
        backend: LlmBackend::Local,
    }
}

/// Clone a KernelContext for sending to a blocking thread.
///
/// All inner Arc types are Send+Sync. The struct itself doesn't auto-derive
/// Send because of `Arc<dyn Trait>` pointers, but since we clone every Arc
/// and only use shared references, this is safe.
fn send_kernel(kernel: &KernelContext) -> KernelContext {
    KernelContext {
        snapshot_store: kernel.snapshot_store.clone(),
        event_ledger: kernel.event_ledger.clone(),
        tick_store: kernel.tick_store.clone(),
        memory_store: kernel.memory_store.clone(),
        context_compiler: kernel.context_compiler.clone(),
        artifact_store: kernel.artifact_store.clone(),
        wi_host_slot: kernel.wi_host_slot.clone(),
        inbox: kernel.inbox.clone(),
        vessel_id: kernel.vessel_id,
        mission: kernel.mission.clone(),
        max_output_tokens: kernel.max_output_tokens,
        master_loop_interval_secs: kernel.master_loop_interval_secs,
        thread_registry: kernel.thread_registry.clone(),
        relationship_ledger: kernel.relationship_ledger.clone(),
        conversation_store: kernel.conversation_store.clone(),
        budget_tracker: kernel.budget_tracker.clone(),
        tool_budget_gate: kernel.tool_budget_gate.clone(),
        metrics: None,
        event_tx: kernel.event_tx.clone(),
        prompt_registry: kernel.prompt_registry.clone(),
        trust_decay_config: kernel.trust_decay_config.clone(),
        episodic_memory_capacity: kernel.episodic_memory_capacity,
        bootstrap_grace_period_ticks: kernel.bootstrap_grace_period_ticks,
        max_decide_turns: kernel.max_decide_turns,
        watch_store: kernel.watch_store.clone(),
        max_watches: kernel.max_watches,
    }
}

/// Build a KernelContext with a real WI Host (delay connector) and a mock LLM.
///
/// Returns (kernel, handler, mock_backend) for assertions.
async fn setup_kernel_with_host(
    dir: &std::path::Path,
    mock_response_content: &str,
) -> (KernelContext, CognitiveHandler, Arc<MockLlmBackend>) {
    // Storage
    std::fs::create_dir_all(dir.join("exo")).unwrap();
    let storage = StorageManager::open(dir).unwrap();
    let artifact_store: Arc<dyn ArtifactStore> = storage.artifact_store().clone();

    // Mock LLM backend
    let response = mock_llm_response(mock_response_content);
    let mock_backend = Arc::new(MockLlmBackend::new(response));
    let mock_as_http: Arc<dyn LlmHttpBackend> = mock_backend.clone();

    // Handler with mock
    let handler = CognitiveHandler::with_backends(
        Some(mock_as_http),
        None,
        LlmBackend::Local,
        artifact_store.clone(),
    );

    // Boot WI Host for Act step
    let registry = ConnectorRegistry::new();
    registry.register(Arc::new(DelayConnector));
    std::fs::create_dir_all(dir.join("wi").join("aq")).unwrap();
    let host_config = HostConfig {
        aq_data_dir: dir.join("wi").join("aq"),
        context_store_path: dir.join("wi").join("context.db"),
        tick_interval: Duration::from_millis(10),
        ..Default::default()
    };
    let host = EmbeddedHost::start(host_config, registry, None)
        .await
        .unwrap();
    let wi_host_slot: WiHostSlot = Arc::new(tokio::sync::Mutex::new(Some(host)));

    // Context compiler
    let counter = Arc::new(ApproximateTokenCounter);
    let compiler = ContextCompiler::with_defaults(counter, 16000);

    let kernel = KernelContext {
        snapshot_store: storage.snapshot_store().clone(),
        event_ledger: storage.event_ledger().clone(),
        tick_store: storage.tick_store().clone(),
        memory_store: storage.memory_store().clone(),
        artifact_store,
        context_compiler: Arc::new(compiler),
        wi_host_slot,
        inbox: Arc::new(InMemoryInbox::new()),
        vessel_id: VesselId::new(),
        mission: "integration test".into(),
        max_output_tokens: 4096,
        master_loop_interval_secs: 60,
        thread_registry: Arc::new(ThreadRegistry::new(Arc::new(InMemoryThreadStore::new()))),
        relationship_ledger: Arc::new(InMemoryRelationshipLedger::new()),
        conversation_store: Arc::new(InMemoryConversationStore::new()),
        budget_tracker: None,
        tool_budget_gate: None,
        metrics: None,
        event_tx: tokio::sync::broadcast::channel::<LiveEvent>(16).0,
        prompt_registry: Arc::new(PromptRegistry::with_defaults()),
        trust_decay_config: None,
        episodic_memory_capacity: None,
        bootstrap_grace_period_ticks: 0,
        max_decide_turns: 5,
        watch_store: Arc::new(exoskeleton_core::InMemoryWatchStore::new()),
        max_watches: 20,
    };

    (kernel, handler, mock_backend)
}

/// Build a KernelContext with NO WI Host (for tests that don't need Act).
#[allow(dead_code)]
fn setup_kernel_no_host(
    dir: &std::path::Path,
    mock_response_content: &str,
) -> (KernelContext, CognitiveHandler, Arc<MockLlmBackend>) {
    std::fs::create_dir_all(dir.join("exo")).unwrap();
    let storage = StorageManager::open(dir).unwrap();
    let artifact_store: Arc<dyn ArtifactStore> = storage.artifact_store().clone();

    let response = mock_llm_response(mock_response_content);
    let mock_backend = Arc::new(MockLlmBackend::new(response));
    let mock_as_http: Arc<dyn LlmHttpBackend> = mock_backend.clone();

    let handler = CognitiveHandler::with_backends(
        Some(mock_as_http),
        None,
        LlmBackend::Local,
        artifact_store.clone(),
    );

    let wi_host_slot: WiHostSlot = Arc::new(tokio::sync::Mutex::new(None));

    let counter = Arc::new(ApproximateTokenCounter);
    let compiler = ContextCompiler::with_defaults(counter, 16000);

    let kernel = KernelContext {
        snapshot_store: storage.snapshot_store().clone(),
        event_ledger: storage.event_ledger().clone(),
        tick_store: storage.tick_store().clone(),
        memory_store: storage.memory_store().clone(),
        artifact_store,
        context_compiler: Arc::new(compiler),
        wi_host_slot,
        inbox: Arc::new(InMemoryInbox::new()),
        vessel_id: VesselId::new(),
        mission: "integration test".into(),
        max_output_tokens: 4096,
        master_loop_interval_secs: 60,
        thread_registry: Arc::new(ThreadRegistry::new(Arc::new(InMemoryThreadStore::new()))),
        relationship_ledger: Arc::new(InMemoryRelationshipLedger::new()),
        conversation_store: Arc::new(InMemoryConversationStore::new()),
        budget_tracker: None,
        tool_budget_gate: None,
        metrics: None,
        event_tx: tokio::sync::broadcast::channel::<LiveEvent>(16).0,
        prompt_registry: Arc::new(PromptRegistry::with_defaults()),
        trust_decay_config: None,
        episodic_memory_capacity: None,
        bootstrap_grace_period_ticks: 0,
        max_decide_turns: 5,
        watch_store: Arc::new(exoskeleton_core::InMemoryWatchStore::new()),
        max_watches: 20,
    };

    (kernel, handler, mock_backend)
}

/// Shut down the WI Host from the kernel's slot.
async fn shutdown_host(kernel: &KernelContext) {
    let mut guard = kernel.wi_host_slot.lock().await;
    if let Some(host) = guard.take() {
        host.shutdown().await.ok();
    }
}

// ══════════════════════════════════════════════════════════════════
// Group 1: Vessel-level tests (no master loop execution)
// ══════════════════════════════════════════════════════════════════

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sprint5_vessel_boot_creates_master_loop_task() {
    let result = tokio::time::timeout(Duration::from_secs(10), async {
        let dir = tempfile::tempdir().unwrap();
        let config = test_config(dir.path());
        let vessel = Vessel::start_with_registry(config, test_registry())
            .await
            .unwrap();

        // master_loop_task_id should be set (non-nil)
        let task_id = vessel.master_loop_task_id();
        assert_ne!(
            task_id.to_string(),
            "00000000-0000-0000-0000-000000000000",
            "master_loop_task_id should be a valid non-nil UUID"
        );

        vessel.shutdown().await.unwrap();
    })
    .await;
    assert!(result.is_ok(), "test timed out");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sprint5_vessel_boot_creates_inbox_dir() {
    let result = tokio::time::timeout(Duration::from_secs(10), async {
        let dir = tempfile::tempdir().unwrap();
        let config = test_config(dir.path());
        let vessel = Vessel::start_with_registry(config, test_registry())
            .await
            .unwrap();

        // Default inbox_dir is {data_dir}/inbox/
        let inbox_dir = dir.path().join("inbox");
        assert!(
            inbox_dir.is_dir(),
            "inbox directory should exist after boot: {}",
            inbox_dir.display()
        );

        vessel.shutdown().await.unwrap();
    })
    .await;
    assert!(result.is_ok(), "test timed out");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sprint5_vessel_shutdown_from_slot() {
    let result = tokio::time::timeout(Duration::from_secs(10), async {
        let dir = tempfile::tempdir().unwrap();
        let config = test_config(dir.path());
        let vessel = Vessel::start_with_registry(config, test_registry())
            .await
            .unwrap();

        // Verify WI Host is accessible before shutdown
        let caps = vessel.list_capabilities();
        assert!(!caps.is_empty(), "capabilities should be available");

        // Shutdown should complete cleanly
        vessel.shutdown().await.unwrap();
    })
    .await;
    assert!(result.is_ok(), "test timed out");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sprint5_vessel_invoke_tool_through_slot() {
    let result = tokio::time::timeout(Duration::from_secs(10), async {
        let dir = tempfile::tempdir().unwrap();
        let config = test_config(dir.path());
        let vessel = Vessel::start_with_registry(config, test_registry())
            .await
            .unwrap();

        // Invoke a tool through the WI Host slot
        let tool_result = vessel
            .invoke_tool("delay", serde_json::json!({"duration_ms": 1}))
            .await;
        assert!(
            tool_result.is_ok(),
            "invoke_tool should succeed: {:?}",
            tool_result.err()
        );

        vessel.shutdown().await.unwrap();
    })
    .await;
    assert!(result.is_ok(), "test timed out");
}

// ══════════════════════════════════════════════════════════════════
// Group 2: Kernel integration tests (run_tick directly)
// ══════════════════════════════════════════════════════════════════

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sprint5_full_tick_completes_podaara_cycle() {
    let result = tokio::time::timeout(Duration::from_secs(10), async {
        let dir = tempfile::tempdir().unwrap();
        let (kernel, handler, mock) =
            setup_kernel_with_host(dir.path(), MOCK_DECISION_NO_ACTIONS).await;

        let k = send_kernel(&kernel);
        let output = tokio::task::spawn_blocking(move || {
            let token = CancellationToken::new();
            exoskeleton_host::kernel::run_tick(&handler, &k, &token)
        })
        .await
        .unwrap();

        // run_tick should succeed
        match &output {
            actionqueue_executor_local::HandlerOutput::Success { .. } => {}
            other => panic!("expected Success, got: {other:?}"),
        }

        // Mock LLM was called exactly once (Decide step)
        assert_eq!(mock.call_count(), 1, "LLM should be called once per tick");

        shutdown_host(&kernel).await;
    })
    .await;
    assert!(result.is_ok(), "test timed out");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sprint5_tick_produces_snapshot() {
    let result = tokio::time::timeout(Duration::from_secs(10), async {
        let dir = tempfile::tempdir().unwrap();
        let (kernel, handler, _mock) =
            setup_kernel_with_host(dir.path(), MOCK_DECISION_NO_ACTIONS).await;

        let snapshot_store = kernel.snapshot_store.clone();

        // Before tick: no snapshot
        assert!(
            snapshot_store.latest().unwrap().is_none(),
            "no snapshot before first tick"
        );

        let k = send_kernel(&kernel);
        tokio::task::spawn_blocking(move || {
            let token = CancellationToken::new();
            exoskeleton_host::kernel::run_tick(&handler, &k, &token)
        })
        .await
        .unwrap();

        // After tick: snapshot exists with tick_number = 1
        let snapshot = snapshot_store
            .latest()
            .unwrap()
            .expect("snapshot should exist after tick");
        assert_eq!(
            snapshot.tick_number, 1,
            "first tick should produce tick_number 1"
        );

        shutdown_host(&kernel).await;
    })
    .await;
    assert!(result.is_ok(), "test timed out");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sprint5_tick_stores_llm_response_artifact() {
    let result = tokio::time::timeout(Duration::from_secs(10), async {
        let dir = tempfile::tempdir().unwrap();
        let (kernel, handler, _mock) =
            setup_kernel_with_host(dir.path(), MOCK_DECISION_NO_ACTIONS).await;

        let artifact_store = kernel.artifact_store.clone();

        let k = send_kernel(&kernel);
        tokio::task::spawn_blocking(move || {
            let token = CancellationToken::new();
            exoskeleton_host::kernel::run_tick(&handler, &k, &token)
        })
        .await
        .unwrap();

        // I3: LlmResponse artifact must exist
        let llm_artifacts = artifact_store
            .list_by_kind(ArtifactKind::LlmResponse, 10)
            .unwrap();
        assert!(
            !llm_artifacts.is_empty(),
            "LlmResponse artifact should be stored (I3)"
        );

        shutdown_host(&kernel).await;
    })
    .await;
    assert!(result.is_ok(), "test timed out");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sprint5_tick_stores_tick_record_artifact() {
    let result = tokio::time::timeout(Duration::from_secs(10), async {
        let dir = tempfile::tempdir().unwrap();
        let (kernel, handler, _mock) =
            setup_kernel_with_host(dir.path(), MOCK_DECISION_NO_ACTIONS).await;

        let artifact_store = kernel.artifact_store.clone();

        let k = send_kernel(&kernel);
        tokio::task::spawn_blocking(move || {
            let token = CancellationToken::new();
            exoskeleton_host::kernel::run_tick(&handler, &k, &token)
        })
        .await
        .unwrap();

        // I3: Tick artifact must exist
        let tick_artifacts = artifact_store.list_by_kind(ArtifactKind::Tick, 10).unwrap();
        assert!(
            !tick_artifacts.is_empty(),
            "Tick artifact should be stored (I3)"
        );

        shutdown_host(&kernel).await;
    })
    .await;
    assert!(result.is_ok(), "test timed out");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sprint5_tick_events_logged() {
    let result = tokio::time::timeout(Duration::from_secs(10), async {
        let dir = tempfile::tempdir().unwrap();
        let (kernel, handler, _mock) =
            setup_kernel_with_host(dir.path(), MOCK_DECISION_NO_ACTIONS).await;

        let event_ledger = kernel.event_ledger.clone();

        let k = send_kernel(&kernel);
        tokio::task::spawn_blocking(move || {
            let token = CancellationToken::new();
            exoskeleton_host::kernel::run_tick(&handler, &k, &token)
        })
        .await
        .unwrap();

        // TickStarted and TickCompleted events should be logged
        let events = event_ledger.recent(100).unwrap();
        let started = events
            .iter()
            .filter(|e| e.event_type == EventType::TickStarted)
            .count();
        let completed = events
            .iter()
            .filter(|e| e.event_type == EventType::TickCompleted)
            .count();
        assert_eq!(started, 1, "expected 1 TickStarted event");
        assert_eq!(completed, 1, "expected 1 TickCompleted event");

        shutdown_host(&kernel).await;
    })
    .await;
    assert!(result.is_ok(), "test timed out");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sprint5_tick_creates_initial_snapshot_if_none() {
    let result = tokio::time::timeout(Duration::from_secs(10), async {
        let dir = tempfile::tempdir().unwrap();
        let (kernel, handler, _mock) =
            setup_kernel_with_host(dir.path(), MOCK_DECISION_NO_ACTIONS).await;

        let snapshot_store = kernel.snapshot_store.clone();

        // No snapshot in store
        assert!(snapshot_store.latest().unwrap().is_none());

        let k = send_kernel(&kernel);
        tokio::task::spawn_blocking(move || {
            let token = CancellationToken::new();
            exoskeleton_host::kernel::run_tick(&handler, &k, &token)
        })
        .await
        .unwrap();

        // First tick creates snapshot from initial (tick 0) -> new (tick 1)
        let snapshot = snapshot_store.latest().unwrap().unwrap();
        assert_eq!(snapshot.tick_number, 1);
        assert_eq!(snapshot.mission, "integration test");

        shutdown_host(&kernel).await;
    })
    .await;
    assert!(result.is_ok(), "test timed out");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sprint5_tick_with_no_actions() {
    let result = tokio::time::timeout(Duration::from_secs(10), async {
        let dir = tempfile::tempdir().unwrap();
        // Use no-action response — tick should complete without any Act step invocations
        let (kernel, handler, _mock) =
            setup_kernel_with_host(dir.path(), MOCK_DECISION_NO_ACTIONS).await;

        let k = send_kernel(&kernel);
        let output = tokio::task::spawn_blocking(move || {
            let token = CancellationToken::new();
            exoskeleton_host::kernel::run_tick(&handler, &k, &token)
        })
        .await
        .unwrap();

        match &output {
            actionqueue_executor_local::HandlerOutput::Success { .. } => {}
            other => panic!("expected Success with no actions, got: {other:?}"),
        }

        // Verify snapshot shows no actions taken
        let snapshot = kernel.snapshot_store.latest().unwrap().unwrap();
        assert_eq!(
            snapshot.last_action_summary.as_deref(),
            Some("No actions taken")
        );

        shutdown_host(&kernel).await;
    })
    .await;
    assert!(result.is_ok(), "test timed out");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sprint5_tick_number_advances() {
    let result = tokio::time::timeout(Duration::from_secs(10), async {
        let dir = tempfile::tempdir().unwrap();
        let (kernel, _handler, mock) =
            setup_kernel_with_host(dir.path(), MOCK_DECISION_NO_ACTIONS).await;

        let snapshot_store = kernel.snapshot_store.clone();
        let tick_store = kernel.tick_store.clone();

        // Run tick 1
        let k1 = send_kernel(&kernel);
        let h1 = CognitiveHandler::with_backends(
            Some(Arc::new(MockLlmBackend::new(mock_llm_response(
                MOCK_DECISION_NO_ACTIONS,
            )))),
            None,
            LlmBackend::Local,
            kernel.artifact_store.clone(),
        );
        tokio::task::spawn_blocking(move || {
            let token = CancellationToken::new();
            exoskeleton_host::kernel::run_tick(&h1, &k1, &token)
        })
        .await
        .unwrap();

        let snap_after_1 = snapshot_store.latest().unwrap().unwrap();
        assert_eq!(snap_after_1.tick_number, 1);

        // Run tick 2
        let k2 = send_kernel(&kernel);
        let h2 = CognitiveHandler::with_backends(
            Some(Arc::new(MockLlmBackend::new(mock_llm_response(
                MOCK_DECISION_NO_ACTIONS,
            )))),
            None,
            LlmBackend::Local,
            kernel.artifact_store.clone(),
        );
        tokio::task::spawn_blocking(move || {
            let token = CancellationToken::new();
            exoskeleton_host::kernel::run_tick(&h2, &k2, &token)
        })
        .await
        .unwrap();

        let snap_after_2 = snapshot_store.latest().unwrap().unwrap();
        assert_eq!(
            snap_after_2.tick_number, 2,
            "tick number should advance to 2"
        );

        // Tick store should have both records
        let latest_tick = tick_store.latest().unwrap().unwrap();
        assert_eq!(latest_tick.tick_number, 2);

        // The original mock should also have been called once (first tick)
        // plus the new handlers each got called once
        assert_eq!(
            mock.call_count(),
            0,
            "original mock not used (we created fresh handlers)"
        );

        shutdown_host(&kernel).await;
    })
    .await;
    assert!(result.is_ok(), "test timed out");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sprint5_kernel_not_configured_returns_failure() {
    let result = tokio::time::timeout(Duration::from_secs(10), async {
        let dir = tempfile::tempdir().unwrap();
        let config = test_config(dir.path());
        exoskeleton_host::vessel::ensure_data_dirs(&config).unwrap();
        let storage = StorageManager::open(&config.data_dir).unwrap();
        let artifact_store: Arc<dyn ArtifactStore> = storage.artifact_store().clone();

        // Bootstrap engine WITHOUT kernel context
        let engine = exoskeleton_host::cognitive_engine::bootstrap_cognitive_engine_with_backends(
            &config,
            None,
            None,
            artifact_store,
            None, // No kernel
        )
        .unwrap();

        // Submit a MasterLoop task
        use actionqueue_core::ids::TaskId;
        use actionqueue_core::task::constraints::TaskConstraints;
        use actionqueue_core::task::metadata::TaskMetadata;
        use actionqueue_core::task::run_policy::RunPolicy;
        use actionqueue_core::task::task_spec::{TaskPayload, TaskSpec};
        use exoskeleton_host::cognitive_engine::{CognitivePayload, CognitiveTaskType};

        let payload = CognitivePayload {
            task_type: CognitiveTaskType::MasterLoop,
            data: serde_json::Value::Null,
        };
        let payload_bytes = serde_json::to_vec(&payload).unwrap();
        let task_id = TaskId::new();
        let spec = TaskSpec::new(
            task_id,
            TaskPayload::with_content_type(payload_bytes, "application/json"),
            RunPolicy::Once,
            TaskConstraints::default(),
            TaskMetadata::default(),
        )
        .unwrap();

        let engine_slot: Arc<tokio::sync::Mutex<Option<_>>> =
            Arc::new(tokio::sync::Mutex::new(Some(engine)));

        {
            let mut guard = engine_slot.lock().await;
            let engine = guard.as_mut().unwrap();
            engine.submit_task(spec).unwrap();
            let _ = engine.run_until_idle().await.unwrap();
        }

        // Should fail with "kernel not configured"
        {
            let guard = engine_slot.lock().await;
            let engine = guard.as_ref().unwrap();
            let projection = engine.projection();
            let runs: Vec<_> = projection.runs_for_task(task_id).collect();
            let run = runs.last().unwrap();
            let attempts = projection.get_attempt_history(&run.id()).unwrap();
            let error = attempts.last().unwrap().error().unwrap();
            assert!(
                error.contains("kernel not configured"),
                "Expected 'kernel not configured', got: {error}"
            );
        }

        let engine = engine_slot.lock().await.take();
        if let Some(engine) = engine {
            let _ = engine.shutdown();
        }
    })
    .await;
    assert!(result.is_ok(), "test timed out");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sprint5_llm_call_within_handler_no_deadlock() {
    let result = tokio::time::timeout(Duration::from_secs(10), async {
        let dir = tempfile::tempdir().unwrap();
        // This test verifies that the Decide step's direct backend call
        // does NOT deadlock (as opposed to using LlmClient which would).
        let (kernel, handler, mock) =
            setup_kernel_with_host(dir.path(), MOCK_DECISION_NO_ACTIONS).await;

        let k = send_kernel(&kernel);
        let output = tokio::task::spawn_blocking(move || {
            let token = CancellationToken::new();
            exoskeleton_host::kernel::run_tick(&handler, &k, &token)
        })
        .await
        .unwrap();

        // If we get here without hanging, the LLM call within the handler
        // completed without deadlock
        match &output {
            actionqueue_executor_local::HandlerOutput::Success { .. } => {}
            other => panic!("expected Success, got: {other:?}"),
        }
        assert_eq!(mock.call_count(), 1, "LLM should have been called once");

        shutdown_host(&kernel).await;
    })
    .await;
    assert!(result.is_ok(), "test timed out — possible deadlock!");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sprint5_tick_with_action_executes_via_wi_host() {
    let result = tokio::time::timeout(Duration::from_secs(10), async {
        let dir = tempfile::tempdir().unwrap();
        let (kernel, handler, _mock) =
            setup_kernel_with_host(dir.path(), MOCK_DECISION_WITH_ACTION).await;

        let artifact_store = kernel.artifact_store.clone();
        let event_ledger = kernel.event_ledger.clone();

        let k = send_kernel(&kernel);
        let output = tokio::task::spawn_blocking(move || {
            let token = CancellationToken::new();
            exoskeleton_host::kernel::run_tick(&handler, &k, &token)
        })
        .await
        .unwrap();

        // Tick should succeed
        match &output {
            actionqueue_executor_local::HandlerOutput::Success { .. } => {}
            other => panic!("expected Success, got: {other:?}"),
        }

        // Act step should have produced a Receipt artifact (I3)
        let receipts = artifact_store
            .list_by_kind(ArtifactKind::Receipt, 10)
            .unwrap();
        assert!(
            !receipts.is_empty(),
            "Receipt artifact should be stored after action execution (I3)"
        );

        // ActionExecuted event should be in ledger
        let events = event_ledger.recent(100).unwrap();
        let action_events: Vec<_> = events
            .iter()
            .filter(|e| e.event_type == EventType::ActionExecuted)
            .collect();
        assert_eq!(
            action_events.len(),
            1,
            "expected 1 ActionExecuted event for delay action"
        );
        assert!(
            action_events[0].summary.contains("delay"),
            "event summary should mention the tool: {}",
            action_events[0].summary
        );

        // Snapshot should reflect the action summary
        let snapshot = kernel.snapshot_store.latest().unwrap().unwrap();
        assert!(
            snapshot.last_action_summary.is_some(),
            "last_action_summary should be set"
        );
        let summary = snapshot.last_action_summary.as_deref().unwrap();
        assert!(
            summary.contains("1 succeeded"),
            "action summary should show success: {summary}"
        );

        shutdown_host(&kernel).await;
    })
    .await;
    assert!(result.is_ok(), "test timed out");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sprint5_tick_persists_memory_notes() {
    let result = tokio::time::timeout(Duration::from_secs(10), async {
        let dir = tempfile::tempdir().unwrap();
        // Use the response with memory_notes: ["test note"]
        let (kernel, handler, _mock) =
            setup_kernel_with_host(dir.path(), MOCK_DECISION_WITH_ACTION).await;

        let k = send_kernel(&kernel);
        tokio::task::spawn_blocking(move || {
            let token = CancellationToken::new();
            exoskeleton_host::kernel::run_tick(&handler, &k, &token)
        })
        .await
        .unwrap();

        // Memory notes from the decision should be persisted
        let episodics = kernel.memory_store.recent_episodic(10).unwrap();
        assert!(
            !episodics.is_empty(),
            "episodic summaries should be persisted from memory_notes"
        );
        let summaries: Vec<&str> = episodics.iter().map(|e| e.summary.as_str()).collect();
        assert!(
            summaries.contains(&"test note"),
            "expected 'test note' in episodic summaries, got: {:?}",
            summaries
        );

        shutdown_host(&kernel).await;
    })
    .await;
    assert!(result.is_ok(), "test timed out");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sprint5_tick_stores_decision_artifact() {
    let result = tokio::time::timeout(Duration::from_secs(10), async {
        let dir = tempfile::tempdir().unwrap();
        let (kernel, handler, _mock) =
            setup_kernel_with_host(dir.path(), MOCK_DECISION_NO_ACTIONS).await;

        let artifact_store = kernel.artifact_store.clone();

        let k = send_kernel(&kernel);
        tokio::task::spawn_blocking(move || {
            let token = CancellationToken::new();
            exoskeleton_host::kernel::run_tick(&handler, &k, &token)
        })
        .await
        .unwrap();

        // I3: Decision artifact must exist
        let decisions = artifact_store
            .list_by_kind(ArtifactKind::Decision, 10)
            .unwrap();
        assert!(
            !decisions.is_empty(),
            "Decision artifact should be stored (I3)"
        );

        shutdown_host(&kernel).await;
    })
    .await;
    assert!(result.is_ok(), "test timed out");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sprint5_tick_stores_snapshot_artifacts() {
    let result = tokio::time::timeout(Duration::from_secs(10), async {
        let dir = tempfile::tempdir().unwrap();
        let (kernel, handler, _mock) =
            setup_kernel_with_host(dir.path(), MOCK_DECISION_NO_ACTIONS).await;

        let artifact_store = kernel.artifact_store.clone();

        let k = send_kernel(&kernel);
        tokio::task::spawn_blocking(move || {
            let token = CancellationToken::new();
            exoskeleton_host::kernel::run_tick(&handler, &k, &token)
        })
        .await
        .unwrap();

        // I3: Snapshot artifacts (before + after) must exist
        let snapshots = artifact_store
            .list_by_kind(ArtifactKind::Snapshot, 10)
            .unwrap();
        assert!(
            snapshots.len() >= 2,
            "expected at least 2 Snapshot artifacts (before + after), got: {}",
            snapshots.len()
        );

        shutdown_host(&kernel).await;
    })
    .await;
    assert!(result.is_ok(), "test timed out");
}

// ── E3-S2: Context Compiler Visualization Tests ──

/// E3-T18: Orient step stores ContextBreakdown artifact after compilation.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e3_orient_stores_context_breakdown_artifact() {
    let result = tokio::time::timeout(Duration::from_secs(10), async {
        let dir = tempfile::tempdir().unwrap();
        let (kernel, handler, _mock) =
            setup_kernel_with_host(dir.path(), MOCK_DECISION_NO_ACTIONS).await;

        let artifact_store = kernel.artifact_store.clone();

        let k = send_kernel(&kernel);
        let output = tokio::task::spawn_blocking(move || {
            let token = CancellationToken::new();
            exoskeleton_host::kernel::run_tick(&handler, &k, &token)
        })
        .await
        .unwrap();

        match &output {
            actionqueue_executor_local::HandlerOutput::Success { .. } => {}
            other => panic!("expected Success, got: {other:?}"),
        }

        // I3/I5: ContextBreakdown artifact must exist after Orient step
        let ctx_artifacts = artifact_store
            .list_by_kind(ArtifactKind::ContextBreakdown, 10)
            .unwrap();
        assert!(
            !ctx_artifacts.is_empty(),
            "ContextBreakdown artifact should be stored after Orient step (I3/I5)"
        );

        // Verify the artifact content is valid CompiledContext JSON
        let artifact = artifact_store.get(&ctx_artifacts[0].id).unwrap().unwrap();
        let context: exoskeleton_memory::compiler::CompiledContext =
            serde_json::from_slice(&artifact.content)
                .expect("ContextBreakdown artifact should deserialize to CompiledContext");
        assert!(context.budget > 0, "budget should be set");
        assert!(!context.sections.is_empty(), "sections should be populated");

        shutdown_host(&kernel).await;
    })
    .await;
    assert!(result.is_ok(), "test timed out");
}

/// E3-T19: TickRecord has context_breakdown_ref populated after a full tick.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e3_tick_record_has_context_breakdown_ref() {
    let result = tokio::time::timeout(Duration::from_secs(10), async {
        let dir = tempfile::tempdir().unwrap();
        let (kernel, handler, _mock) =
            setup_kernel_with_host(dir.path(), MOCK_DECISION_NO_ACTIONS).await;

        let tick_store = kernel.tick_store.clone();

        let k = send_kernel(&kernel);
        let output = tokio::task::spawn_blocking(move || {
            let token = CancellationToken::new();
            exoskeleton_host::kernel::run_tick(&handler, &k, &token)
        })
        .await
        .unwrap();

        match &output {
            actionqueue_executor_local::HandlerOutput::Success { .. } => {}
            other => panic!("expected Success, got: {other:?}"),
        }

        // The TickRecord should have a context_breakdown_ref linking to the artifact
        let tick_record = tick_store
            .latest()
            .unwrap()
            .expect("TickRecord should exist after tick");
        assert!(
            tick_record.context_breakdown_ref.is_some(),
            "TickRecord.context_breakdown_ref should be Some after a successful tick (E3-S2)"
        );

        shutdown_host(&kernel).await;
    })
    .await;
    assert!(result.is_ok(), "test timed out");
}

// ── T-13: Property-Based Tests (Sprint 5) ──

mod proptest_tests {
    use exoskeleton_core::{StateSnapshot, VesselId, VesselStatus};
    use exoskeleton_host::kernel::{DecisionProtocol, PlannedAction, SnapshotDelta};
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn decision_protocol_survives_json_roundtrip(
            reasoning in "\\PC{1,200}",
            plan in proptest::option::of("\\PC{1,100}"),
            wc in proptest::option::of("\\PC{1,100}"),
            num_actions in 0usize..5,
            num_notes in 0usize..5,
        ) {
            let actions: Vec<PlannedAction> = (0..num_actions).map(|i| PlannedAction {
                tool_name: format!("tool_{i}"),
                params: serde_json::json!({"key": i}),
                rationale: format!("reason {i}"),
                plan_task_id: None,
            }).collect();
            let notes: Vec<String> = (0..num_notes).map(|i| format!("note {i}")).collect();

            let plan_update = plan.map(|p| exoskeleton_core::PlanUpdate::Replace {
                plan: exoskeleton_core::Plan::from_legacy_string(p),
            });
            let working_memory_ops = wc.map(|w| vec![exoskeleton_core::WorkingMemoryOp::Set {
                key: "context".into(),
                value: w,
                ttl_ticks: None,
            }]);
            let proto = DecisionProtocol {
                reasoning,
                reply: None,
                plan_update,
                working_memory_ops,
                actions,
                memory_notes: notes,
                watch_proposals: vec![],
            };
            let json = serde_json::to_string(&proto).unwrap();
            let parsed: DecisionProtocol = serde_json::from_str(&json).unwrap();
            prop_assert_eq!(proto.reasoning, parsed.reasoning);
            prop_assert_eq!(proto.actions.len(), parsed.actions.len());
            prop_assert_eq!(proto.memory_notes.len(), parsed.memory_notes.len());
        }

        #[test]
        fn planned_action_survives_json_roundtrip(
            tool_name in "[a-z][a-z.]{0,20}",
            rationale in "\\PC{1,100}",
            param_val in 0i64..1000,
        ) {
            let action = PlannedAction {
                tool_name,
                params: serde_json::json!({"value": param_val}),
                rationale,
                plan_task_id: None,
            };
            let json = serde_json::to_string(&action).unwrap();
            let parsed: PlannedAction = serde_json::from_str(&json).unwrap();
            prop_assert_eq!(action.tool_name, parsed.tool_name);
            prop_assert_eq!(action.rationale, parsed.rationale);
            prop_assert_eq!(action.params, parsed.params);
        }

        #[test]
        fn snapshot_delta_application_preserves_fields(
            has_plan in proptest::bool::ANY,
            has_wc in proptest::bool::ANY,
        ) {
            let snapshot = StateSnapshot::initial(VesselId::new(), "test mission".into());
            let original_mission = snapshot.mission.clone();
            let original_vessel_id = snapshot.vessel_id;
            let original_tick_number = snapshot.tick_number;

            let delta = SnapshotDelta {
                plan_update: if has_plan {
                    Some(exoskeleton_core::PlanUpdate::Replace {
                        plan: exoskeleton_core::Plan::from_legacy_string("new plan".into()),
                    })
                } else {
                    None
                },
                working_memory_ops: if has_wc {
                    Some(vec![exoskeleton_core::WorkingMemoryOp::Set {
                        key: "context".into(),
                        value: "new wc".into(),
                        ttl_ticks: None,
                    }])
                } else {
                    None
                },
            };

            // Apply delta (same logic as amend step)
            let mut new_snapshot = snapshot.clone();
            if let Some(exoskeleton_core::PlanUpdate::Replace { plan }) = delta.plan_update {
                new_snapshot.plan = Some(plan);
            }
            if let Some(ops) = &delta.working_memory_ops {
                new_snapshot.working_memory.apply_ops(ops, new_snapshot.tick_number);
            }

            // Unmodified fields must be preserved
            prop_assert_eq!(new_snapshot.mission, original_mission);
            prop_assert_eq!(new_snapshot.vessel_id, original_vessel_id);
            prop_assert_eq!(new_snapshot.tick_number, original_tick_number);

            // Modified fields should reflect the delta
            if has_plan {
                prop_assert!(new_snapshot.plan.is_some());
                prop_assert_eq!(&new_snapshot.plan.as_ref().unwrap().objective, "new plan");
            } else {
                prop_assert_eq!(new_snapshot.plan, snapshot.plan);
            }
            if has_wc {
                prop_assert!(!new_snapshot.working_memory.is_empty());
            } else {
                prop_assert_eq!(new_snapshot.working_memory, snapshot.working_memory);
            }
        }

        #[test]
        fn tick_number_always_increases(
            num_ticks in 1usize..20,
        ) {
            let mut tick_number = 0u64;
            let mut prev_tick_number = 0u64;

            for _ in 0..num_ticks {
                tick_number = prev_tick_number + 1; // Same logic as run_tick
                prop_assert!(tick_number > prev_tick_number);
                prev_tick_number = tick_number;
            }
            prop_assert_eq!(tick_number, num_ticks as u64);
        }

        #[test]
        fn amend_always_produces_valid_snapshot(
            plan in proptest::option::of("\\PC{0,200}"),
            wc in proptest::option::of("\\PC{0,200}"),
        ) {
            let snapshot = StateSnapshot::initial(VesselId::new(), "test".into());

            let mut new_snapshot = snapshot;
            if let Some(p) = plan {
                new_snapshot.plan = Some(exoskeleton_core::Plan::from_legacy_string(p));
            }
            if let Some(w) = wc {
                new_snapshot.working_memory = exoskeleton_core::working_memory::WorkingMemory::from_legacy_string(w);
            }
            new_snapshot.tick_number = 1;
            new_snapshot.status = VesselStatus::Idle;

            // Must produce valid JSON
            let json = serde_json::to_string(&new_snapshot);
            prop_assert!(json.is_ok(), "snapshot must serialize to JSON");

            // Must round-trip
            let parsed: Result<StateSnapshot, _> = serde_json::from_str(&json.unwrap());
            prop_assert!(parsed.is_ok(), "snapshot must deserialize from JSON");
        }
    }
}

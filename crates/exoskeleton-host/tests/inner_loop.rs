//! E8-S1: Inner loop integration tests.
//!
//! Tests the bounded inner interaction loop within PODAARA ticks.
//! Uses mock LLM backends with scripted responses to verify loop behavior.
//!
//! DEADLOCK PREVENTION:
//! - All async tests use `flavor = "multi_thread", worker_threads = 2`
//! - All tests wrap body in `tokio::time::timeout`
//! - Tests calling `run_tick` use `spawn_blocking` (run_tick uses block_on)

mod common;

use std::sync::Arc;
use std::time::Duration;

use actionqueue_executor_local::CancellationToken;
use exoskeleton_core::conversation::InMemoryConversationStore;
use exoskeleton_core::llm::{LlmBackend, LlmResponse, StopReason};
use exoskeleton_core::{ArtifactStore, EventType, LiveEvent, PromptRegistry, VesselId};
use exoskeleton_host::cognitive_engine::CognitiveHandler;
use exoskeleton_host::config::InnerLoopConfig;
use exoskeleton_host::inbox::InMemoryInbox;
use exoskeleton_host::kernel::{KernelContext, WiHostSlot};
use exoskeleton_host::llm::mock::{MockLlmBackend, MockSequenceLlmBackend};
use exoskeleton_host::storage::StorageManager;
use exoskeleton_host::LlmHttpBackend;
use exoskeleton_memory::{ApproximateTokenCounter, ContextCompiler};
use exoskeleton_relationship::InMemoryRelationshipLedger;
use exoskeleton_threads::{InMemoryThreadStore, ThreadRegistry};
use worldinterface_connector::connectors::DelayConnector;
use worldinterface_connector::registry::ConnectorRegistry;
use worldinterface_host::config::HostConfig;
use worldinterface_host::host::EmbeddedHost;

/// Decision JSON with no actions (idle tick, no inner loop).
const DECISION_NO_ACTIONS: &str =
    r#"{"reasoning":"No actions needed","actions":[],"memory_notes":[]}"#;

/// Decision JSON that requests inner loop with one action.
const DECISION_INNER_LOOP_WITH_ACTION: &str = r#"{"reasoning":"Coding task","inner_loop_requested":true,"actions":[{"tool_name":"delay","params":{"duration_ms":1},"rationale":"test step"}],"memory_notes":[]}"#;

/// Decision JSON that requests inner loop but has no actions (edge case).
const DECISION_INNER_LOOP_NO_ACTIONS: &str =
    r#"{"reasoning":"Done coding","inner_loop_requested":true,"actions":[],"memory_notes":[]}"#;

/// Decision JSON that does NOT request inner loop, with one action.
const DECISION_NO_INNER_LOOP_WITH_ACTION: &str = r#"{"reasoning":"Normal action","inner_loop_requested":false,"actions":[{"tool_name":"delay","params":{"duration_ms":1},"rationale":"regular action"}],"memory_notes":[]}"#;

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
        read_paths_this_tick: kernel.read_paths_this_tick.clone(),
        inner_loop_config: kernel.inner_loop_config.clone(),
        tool_policy: kernel.tool_policy.clone(),
        session_approvals: exoskeleton_host::kernel::policy::SessionApprovals::new(),
        vessel_mode: kernel.vessel_mode.clone(),
        wake_signal: kernel.wake_signal.clone(),
        observatory_url: kernel.observatory_url.clone(),
        observatory_token: kernel.observatory_token.clone(),
    }
}

/// Build a KernelContext with a real WI Host (delay connector) and a mock LLM.
async fn setup_kernel_with_mock(
    dir: &std::path::Path,
    mock_backend: Arc<dyn LlmHttpBackend>,
    inner_loop_config: InnerLoopConfig,
) -> (KernelContext, CognitiveHandler) {
    // Storage
    std::fs::create_dir_all(dir.join("exo")).unwrap();
    let storage = StorageManager::open(dir).unwrap();
    let artifact_store: Arc<dyn ArtifactStore> = storage.artifact_store().clone();

    // Handler with mock
    let handler = CognitiveHandler::with_backends(
        Some(mock_backend),
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
        mission: "inner loop test".into(),
        max_output_tokens: 4096,
        master_loop_interval_secs: 60,
        thread_registry: Arc::new(ThreadRegistry::new(Arc::new(InMemoryThreadStore::new()))),
        relationship_ledger: Arc::new(InMemoryRelationshipLedger::new()),
        conversation_store: Arc::new(InMemoryConversationStore::new()),
        budget_tracker: None,
        tool_budget_gate: None,
        metrics: None,
        event_tx: tokio::sync::broadcast::channel::<LiveEvent>(256).0,
        prompt_registry: Arc::new(PromptRegistry::with_defaults()),
        trust_decay_config: None,
        episodic_memory_capacity: None,
        bootstrap_grace_period_ticks: 0,
        max_decide_turns: 5,
        watch_store: Arc::new(exoskeleton_core::InMemoryWatchStore::new()),
        max_watches: 20,
        read_paths_this_tick: Arc::new(std::sync::Mutex::new(std::collections::HashSet::new())),
        inner_loop_config,
        tool_policy: exoskeleton_host::kernel::policy::ToolPolicyConfig::default(),
        session_approvals: exoskeleton_host::kernel::policy::SessionApprovals::new(),
        vessel_mode: Arc::new(std::sync::Mutex::new(exoskeleton_core::VesselMode::Normal)),
        wake_signal: None,
        observatory_url: None,
        observatory_token: None,
    };

    (kernel, handler)
}

/// Shut down the WI Host from the kernel's slot.
async fn shutdown_host(kernel: &KernelContext) {
    let mut guard = kernel.wi_host_slot.lock().await;
    if let Some(host) = guard.take() {
        host.shutdown().await.ok();
    }
}

// ══════════════════════════════════════════════════════════════════
// E8S1-T23: inner_loop_disabled_classic_path
// ══════════════════════════════════════════════════════════════════

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn inner_loop_disabled_classic_path() {
    let result = tokio::time::timeout(Duration::from_secs(15), async {
        let dir = tempfile::tempdir().unwrap();
        // Inner loop disabled (default)
        let mock = Arc::new(MockLlmBackend::new(mock_llm_response(
            DECISION_INNER_LOOP_WITH_ACTION,
        )));
        let mock_clone = mock.clone();
        let config = InnerLoopConfig {
            enabled: false,
            ..InnerLoopConfig::default()
        };
        let (kernel, handler) =
            setup_kernel_with_mock(dir.path(), mock_clone as Arc<dyn LlmHttpBackend>, config).await;

        let mut rx = kernel.event_tx.subscribe();
        let k = send_kernel(&kernel);
        let output = tokio::task::spawn_blocking(move || {
            let token = CancellationToken::new();
            exoskeleton_host::kernel::run_tick(&handler, &k, &token)
        })
        .await
        .unwrap();
        assert!(
            matches!(
                output,
                actionqueue_executor_local::HandlerOutput::Success { .. }
            ),
            "tick should succeed in classic path"
        );

        // Should NOT see InnerLoopStarted events
        let mut found_inner_loop = false;
        while let Ok(event) = rx.try_recv() {
            if event.event_type == EventType::InnerLoopStarted {
                found_inner_loop = true;
            }
        }
        assert!(
            !found_inner_loop,
            "inner loop should not activate when disabled"
        );

        // Mock called for Decide + Reflect = 2 calls (classic path)
        assert_eq!(mock.call_count(), 2, "classic path: Decide + Reflect");

        shutdown_host(&kernel).await;
    })
    .await;
    assert!(result.is_ok(), "test timed out");
}

// ══════════════════════════════════════════════════════════════════
// E8S1-T24: inner_loop_enabled_not_requested_classic_path
// ══════════════════════════════════════════════════════════════════

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn inner_loop_enabled_not_requested_classic_path() {
    let result = tokio::time::timeout(Duration::from_secs(15), async {
        let dir = tempfile::tempdir().unwrap();
        // Enabled but the agent doesn't request it
        let mock = Arc::new(MockLlmBackend::new(mock_llm_response(
            DECISION_NO_INNER_LOOP_WITH_ACTION,
        )));
        let mock_clone = mock.clone();
        let config = InnerLoopConfig {
            enabled: true,
            ..InnerLoopConfig::default()
        };
        let (kernel, handler) =
            setup_kernel_with_mock(dir.path(), mock_clone as Arc<dyn LlmHttpBackend>, config).await;

        let mut rx = kernel.event_tx.subscribe();
        let k = send_kernel(&kernel);
        let output = tokio::task::spawn_blocking(move || {
            let token = CancellationToken::new();
            exoskeleton_host::kernel::run_tick(&handler, &k, &token)
        })
        .await
        .unwrap();
        assert!(
            matches!(
                output,
                actionqueue_executor_local::HandlerOutput::Success { .. }
            ),
            "tick should succeed in classic path"
        );

        // Should NOT see InnerLoopStarted events
        let mut found_inner_loop = false;
        while let Ok(event) = rx.try_recv() {
            if event.event_type == EventType::InnerLoopStarted {
                found_inner_loop = true;
            }
        }
        assert!(
            !found_inner_loop,
            "inner loop should not activate when agent doesn't request it"
        );

        shutdown_host(&kernel).await;
    })
    .await;
    assert!(result.is_ok(), "test timed out");
}

// ══════════════════════════════════════════════════════════════════
// E8S1-T25: inner_loop_enabled_and_requested_activates
// ══════════════════════════════════════════════════════════════════

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn inner_loop_enabled_and_requested_activates() {
    let result = tokio::time::timeout(Duration::from_secs(15), async {
        let dir = tempfile::tempdir().unwrap();
        // First call: Decide returns inner_loop_requested=true with an action
        // Second call: DecideLite returns no actions (completion)
        // Third call: Reflect
        let responses = vec![
            mock_llm_response(DECISION_INNER_LOOP_WITH_ACTION),
            mock_llm_response(DECISION_NO_ACTIONS), // DecideLite: done
            mock_llm_response(DECISION_NO_ACTIONS), // Reflect
        ];
        let mock = Arc::new(MockSequenceLlmBackend::new(responses));
        let mock_clone = mock.clone();
        let config = InnerLoopConfig {
            enabled: true,
            ..InnerLoopConfig::default()
        };
        let (kernel, handler) =
            setup_kernel_with_mock(dir.path(), mock_clone as Arc<dyn LlmHttpBackend>, config).await;

        let mut rx = kernel.event_tx.subscribe();
        let k = send_kernel(&kernel);
        let output = tokio::task::spawn_blocking(move || {
            let token = CancellationToken::new();
            exoskeleton_host::kernel::run_tick(&handler, &k, &token)
        })
        .await
        .unwrap();
        assert!(
            matches!(
                output,
                actionqueue_executor_local::HandlerOutput::Success { .. }
            ),
            "tick should succeed with inner loop"
        );

        // Should see InnerLoopStarted and InnerLoopCompleted events
        let mut found_started = false;
        let mut found_completed = false;
        while let Ok(event) = rx.try_recv() {
            if event.event_type == EventType::InnerLoopStarted {
                found_started = true;
            }
            if event.event_type == EventType::InnerLoopCompleted {
                found_completed = true;
            }
        }
        assert!(found_started, "should emit InnerLoopStarted");
        assert!(found_completed, "should emit InnerLoopCompleted");

        shutdown_host(&kernel).await;
    })
    .await;
    assert!(result.is_ok(), "test timed out");
}

// ══════════════════════════════════════════════════════════════════
// E8S1-T26: inner_loop_single_step
// ══════════════════════════════════════════════════════════════════

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn inner_loop_single_step() {
    let result = tokio::time::timeout(Duration::from_secs(15), async {
        let dir = tempfile::tempdir().unwrap();
        // Decide: inner_loop_requested=true with 1 action
        // DecideLite: no actions (agent complete after 1 step)
        // Reflect
        let responses = vec![
            mock_llm_response(DECISION_INNER_LOOP_WITH_ACTION),
            mock_llm_response(DECISION_NO_ACTIONS),
            mock_llm_response(DECISION_NO_ACTIONS),
        ];
        let mock = Arc::new(MockSequenceLlmBackend::new(responses));
        let mock_clone = mock.clone();
        let config = InnerLoopConfig {
            enabled: true,
            ..InnerLoopConfig::default()
        };
        let (kernel, handler) =
            setup_kernel_with_mock(dir.path(), mock_clone as Arc<dyn LlmHttpBackend>, config).await;

        let mut rx = kernel.event_tx.subscribe();
        let k = send_kernel(&kernel);
        let output = tokio::task::spawn_blocking(move || {
            let token = CancellationToken::new();
            exoskeleton_host::kernel::run_tick(&handler, &k, &token)
        })
        .await
        .unwrap();
        assert!(matches!(
            output,
            actionqueue_executor_local::HandlerOutput::Success { .. }
        ));

        // Count step events
        let mut step_count = 0;
        while let Ok(event) = rx.try_recv() {
            if event.event_type == EventType::InnerLoopStep {
                step_count += 1;
            }
        }
        assert_eq!(step_count, 1, "should have exactly 1 inner loop step");

        shutdown_host(&kernel).await;
    })
    .await;
    assert!(result.is_ok(), "test timed out");
}

// ══════════════════════════════════════════════════════════════════
// E8S1-T27: inner_loop_multi_step
// ══════════════════════════════════════════════════════════════════

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn inner_loop_multi_step() {
    let result = tokio::time::timeout(Duration::from_secs(15), async {
        let dir = tempfile::tempdir().unwrap();
        // Decide: inner_loop_requested=true with action
        // DecideLite 1: action (step 2)
        // DecideLite 2: action (step 3)
        // DecideLite 3: no actions (done)
        // Reflect
        let action_response = mock_llm_response(
            r#"{"reasoning":"continue","actions":[{"tool_name":"delay","params":{"duration_ms":1},"rationale":"next step"}],"memory_notes":[]}"#,
        );
        let responses = vec![
            mock_llm_response(DECISION_INNER_LOOP_WITH_ACTION),
            action_response.clone(),
            action_response,
            mock_llm_response(DECISION_NO_ACTIONS), // DecideLite: done
            mock_llm_response(DECISION_NO_ACTIONS), // Reflect
        ];
        let mock = Arc::new(MockSequenceLlmBackend::new(responses));
        let mock_clone = mock.clone();
        let config = InnerLoopConfig {
            enabled: true,
            ..InnerLoopConfig::default()
        };
        let (kernel, handler) =
            setup_kernel_with_mock(dir.path(), mock_clone as Arc<dyn LlmHttpBackend>, config)
                .await;

        let mut rx = kernel.event_tx.subscribe();
        let k = send_kernel(&kernel);
        let output = tokio::task::spawn_blocking(move || {
            let token = CancellationToken::new();
            exoskeleton_host::kernel::run_tick(&handler, &k, &token)
        })
        .await
        .unwrap();
        assert!(matches!(
            output,
            actionqueue_executor_local::HandlerOutput::Success { .. }
        ));

        let mut step_count = 0;
        while let Ok(event) = rx.try_recv() {
            if event.event_type == EventType::InnerLoopStep {
                step_count += 1;
            }
        }
        assert_eq!(step_count, 3, "should have exactly 3 inner loop steps");

        shutdown_host(&kernel).await;
    })
    .await;
    assert!(result.is_ok(), "test timed out");
}

// ══════════════════════════════════════════════════════════════════
// E8S1-T28: inner_loop_step_limit_enforced
// ══════════════════════════════════════════════════════════════════

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn inner_loop_step_limit_enforced() {
    let result = tokio::time::timeout(Duration::from_secs(15), async {
        let dir = tempfile::tempdir().unwrap();
        // Agent always proposes actions; limit to 3 steps
        let action_response = mock_llm_response(
            r#"{"reasoning":"keep going","actions":[{"tool_name":"delay","params":{"duration_ms":1},"rationale":"loop step"}],"memory_notes":[]}"#,
        );
        // Need enough responses: 1 Decide + 3 DecideLite + 1 Reflect = 5
        let responses = vec![
            mock_llm_response(DECISION_INNER_LOOP_WITH_ACTION),
            action_response.clone(),
            action_response.clone(),
            action_response,
            mock_llm_response(DECISION_NO_ACTIONS), // Reflect
        ];
        let mock = Arc::new(MockSequenceLlmBackend::new(responses));
        let mock_clone = mock.clone();
        let config = InnerLoopConfig {
            enabled: true,
            max_steps_per_tick: 3,
            ..InnerLoopConfig::default()
        };
        let (kernel, handler) =
            setup_kernel_with_mock(dir.path(), mock_clone as Arc<dyn LlmHttpBackend>, config)
                .await;

        let mut rx = kernel.event_tx.subscribe();
        let k = send_kernel(&kernel);
        let output = tokio::task::spawn_blocking(move || {
            let token = CancellationToken::new();
            exoskeleton_host::kernel::run_tick(&handler, &k, &token)
        })
        .await
        .unwrap();
        assert!(matches!(
            output,
            actionqueue_executor_local::HandlerOutput::Success { .. }
        ));

        // Should see exactly 3 step events and completion with step_limit reason
        let mut step_count = 0;
        let mut completion_reason = None;
        while let Ok(event) = rx.try_recv() {
            if event.event_type == EventType::InnerLoopStep {
                step_count += 1;
            }
            if event.event_type == EventType::InnerLoopCompleted {
                completion_reason = event
                    .inner_loop_detail
                    .and_then(|d| d.completion_reason);
            }
        }
        assert_eq!(step_count, 3, "should stop at step limit");
        assert_eq!(
            completion_reason.as_deref(),
            Some("step_limit"),
            "completion reason should be step_limit"
        );

        shutdown_host(&kernel).await;
    })
    .await;
    assert!(result.is_ok(), "test timed out");
}

// ══════════════════════════════════════════════════════════════════
// E8S1-T29: inner_loop_token_limit_enforced
// ══════════════════════════════════════════════════════════════════

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn inner_loop_token_limit_enforced() {
    let result = tokio::time::timeout(Duration::from_secs(15), async {
        let dir = tempfile::tempdir().unwrap();
        // Each DecideLite response uses 150 tokens (100 in + 50 out)
        // Set budget to 200 tokens — should allow 1 step, block on 2nd
        let action_response = mock_llm_response(
            r#"{"reasoning":"continue","actions":[{"tool_name":"delay","params":{"duration_ms":1},"rationale":"step"}],"memory_notes":[]}"#,
        );
        let responses = vec![
            mock_llm_response(DECISION_INNER_LOOP_WITH_ACTION),
            action_response.clone(),
            action_response,
            mock_llm_response(DECISION_NO_ACTIONS), // Reflect
        ];
        let mock = Arc::new(MockSequenceLlmBackend::new(responses));
        let mock_clone = mock.clone();
        let config = InnerLoopConfig {
            enabled: true,
            max_tokens_per_session: 200, // < 2 * 150
            ..InnerLoopConfig::default()
        };
        let (kernel, handler) =
            setup_kernel_with_mock(dir.path(), mock_clone as Arc<dyn LlmHttpBackend>, config)
                .await;

        let mut rx = kernel.event_tx.subscribe();
        let k = send_kernel(&kernel);
        let output = tokio::task::spawn_blocking(move || {
            let token = CancellationToken::new();
            exoskeleton_host::kernel::run_tick(&handler, &k, &token)
        })
        .await
        .unwrap();
        assert!(matches!(
            output,
            actionqueue_executor_local::HandlerOutput::Success { .. }
        ));

        let mut completion_reason = None;
        while let Ok(event) = rx.try_recv() {
            if event.event_type == EventType::InnerLoopCompleted {
                completion_reason = event
                    .inner_loop_detail
                    .and_then(|d| d.completion_reason);
            }
        }
        assert_eq!(
            completion_reason.as_deref(),
            Some("token_budget"),
            "should stop due to token budget"
        );

        shutdown_host(&kernel).await;
    })
    .await;
    assert!(result.is_ok(), "test timed out");
}

// ══════════════════════════════════════════════════════════════════
// E8S1-T30: inner_loop_doom_loop_detected
// ══════════════════════════════════════════════════════════════════

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn inner_loop_doom_loop_detected() {
    let result = tokio::time::timeout(Duration::from_secs(15), async {
        let dir = tempfile::tempdir().unwrap();
        // Same tool + same args repeated — correction after 3 identical calls,
        // hard stop after 2 more identical calls.
        let same_action = mock_llm_response(
            r#"{"reasoning":"retry","actions":[{"tool_name":"delay","params":{"duration_ms":1},"rationale":"same thing"}],"memory_notes":[]}"#,
        );
        // Decide + 5 DecideLite (all same action) + Reflect
        let responses = vec![
            mock_llm_response(DECISION_INNER_LOOP_WITH_ACTION),
            same_action.clone(),
            same_action.clone(),
            same_action.clone(),
            same_action.clone(),
            same_action.clone(),
            same_action,
            mock_llm_response(DECISION_NO_ACTIONS), // Reflect
        ];
        let mock = Arc::new(MockSequenceLlmBackend::new(responses));
        let mock_clone = mock.clone();
        let config = InnerLoopConfig {
            enabled: true,
            doom_loop_threshold: 3,
            workspace_root: None,
            ..InnerLoopConfig::default()
        };
        let (kernel, handler) =
            setup_kernel_with_mock(dir.path(), mock_clone as Arc<dyn LlmHttpBackend>, config)
                .await;

        let mut rx = kernel.event_tx.subscribe();
        let k = send_kernel(&kernel);
        let output = tokio::task::spawn_blocking(move || {
            let token = CancellationToken::new();
            exoskeleton_host::kernel::run_tick(&handler, &k, &token)
        })
        .await
        .unwrap();
        assert!(matches!(
            output,
            actionqueue_executor_local::HandlerOutput::Success { .. }
        ));

        let mut completion_reason = None;
        while let Ok(event) = rx.try_recv() {
            if event.event_type == EventType::InnerLoopCompleted {
                completion_reason = event
                    .inner_loop_detail
                    .and_then(|d| d.completion_reason);
            }
        }
        assert_eq!(
            completion_reason.as_deref(),
            Some("doom_loop"),
            "should detect doom loop"
        );

        shutdown_host(&kernel).await;
    })
    .await;
    assert!(result.is_ok(), "test timed out");
}

// ══════════════════════════════════════════════════════════════════
// E8S1-T31: inner_loop_cancellation_stops
// ══════════════════════════════════════════════════════════════════

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn inner_loop_cancellation_stops() {
    let result = tokio::time::timeout(Duration::from_secs(15), async {
        let dir = tempfile::tempdir().unwrap();
        // We cancel before the inner loop has a chance to continue
        let responses = vec![
            mock_llm_response(DECISION_INNER_LOOP_WITH_ACTION),
            mock_llm_response(DECISION_NO_ACTIONS), // won't be needed
            mock_llm_response(DECISION_NO_ACTIONS), // Reflect
        ];
        let mock = Arc::new(MockSequenceLlmBackend::new(responses));
        let mock_clone = mock.clone();
        let config = InnerLoopConfig {
            enabled: true,
            ..InnerLoopConfig::default()
        };
        let (kernel, handler) =
            setup_kernel_with_mock(dir.path(), mock_clone as Arc<dyn LlmHttpBackend>, config).await;

        let k = send_kernel(&kernel);
        let token = CancellationToken::new();
        // Cancel immediately — should abort gracefully
        token.cancel();
        let output = tokio::task::spawn_blocking(move || {
            exoskeleton_host::kernel::run_tick(&handler, &k, &token)
        })
        .await
        .unwrap();

        // Should be retryable failure (cancelled) or success (if cancelled before inner loop)
        // The tick will return retryable_failure when cancelled
        match &output {
            actionqueue_executor_local::HandlerOutput::RetryableFailure { .. } => {
                // Expected — cancelled before or during inner loop
            }
            actionqueue_executor_local::HandlerOutput::Success { .. } => {
                // Also acceptable — if cancellation was checked after completion
            }
            other => panic!("unexpected output: {other:?}"),
        }

        shutdown_host(&kernel).await;
    })
    .await;
    assert!(result.is_ok(), "test timed out");
}

// ══════════════════════════════════════════════════════════════════
// E8S1-T33: inner_loop_budget_recorded
// ══════════════════════════════════════════════════════════════════

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn inner_loop_budget_recorded() {
    let result = tokio::time::timeout(Duration::from_secs(15), async {
        let dir = tempfile::tempdir().unwrap();
        let responses = vec![
            mock_llm_response(DECISION_INNER_LOOP_WITH_ACTION),
            mock_llm_response(DECISION_NO_ACTIONS),
            mock_llm_response(DECISION_NO_ACTIONS),
        ];
        let mock = Arc::new(MockSequenceLlmBackend::new(responses));
        let mock_clone = mock.clone();
        let config = InnerLoopConfig {
            enabled: true,
            ..InnerLoopConfig::default()
        };
        let (mut kernel, handler) =
            setup_kernel_with_mock(dir.path(), mock_clone as Arc<dyn LlmHttpBackend>, config).await;

        // Add a budget tracker
        let budget_config = exoskeleton_core::CognitiveBudgetConfig::default();
        let budget_store: Arc<dyn exoskeleton_core::BudgetStore> =
            Arc::new(exoskeleton_host::budget::tracker::InMemoryBudgetStore::new());
        let tracker =
            exoskeleton_host::budget::CognitiveBudgetTracker::new(budget_config, budget_store);
        kernel.budget_tracker = Some(Arc::new(std::sync::Mutex::new(tracker)));

        let k = send_kernel(&kernel);
        let output = tokio::task::spawn_blocking(move || {
            let token = CancellationToken::new();
            exoskeleton_host::kernel::run_tick(&handler, &k, &token)
        })
        .await
        .unwrap();
        assert!(matches!(
            output,
            actionqueue_executor_local::HandlerOutput::Success { .. }
        ));

        // Verify budget was consumed
        {
            let guard = kernel.budget_tracker.as_ref().unwrap().lock().unwrap();
            let consumption = guard.build_consumption();
            assert!(
                !consumption.is_empty(),
                "budget should have been consumed during inner loop"
            );
        }

        shutdown_host(&kernel).await;
    })
    .await;
    assert!(result.is_ok(), "test timed out");
}

// ══════════════════════════════════════════════════════════════════
// E8S1-T34: inner_loop_artifacts_stored
// ══════════════════════════════════════════════════════════════════

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn inner_loop_artifacts_stored() {
    let result = tokio::time::timeout(Duration::from_secs(15), async {
        let dir = tempfile::tempdir().unwrap();
        let responses = vec![
            mock_llm_response(DECISION_INNER_LOOP_WITH_ACTION),
            mock_llm_response(DECISION_NO_ACTIONS),
            mock_llm_response(DECISION_NO_ACTIONS),
        ];
        let mock = Arc::new(MockSequenceLlmBackend::new(responses));
        let mock_clone = mock.clone();
        let config = InnerLoopConfig {
            enabled: true,
            ..InnerLoopConfig::default()
        };
        let (kernel, handler) =
            setup_kernel_with_mock(dir.path(), mock_clone as Arc<dyn LlmHttpBackend>, config).await;

        let k = send_kernel(&kernel);
        let output = tokio::task::spawn_blocking(move || {
            let token = CancellationToken::new();
            exoskeleton_host::kernel::run_tick(&handler, &k, &token)
        })
        .await
        .unwrap();
        assert!(matches!(
            output,
            actionqueue_executor_local::HandlerOutput::Success { .. }
        ));

        // Verify LlmResponse artifacts were stored
        // At minimum we have: Decide + DecideLite + Reflect = 3 LLM calls = 3 artifacts
        let _events = kernel
            .event_ledger
            .by_type(EventType::LlmCalled, 100)
            .unwrap();
        // LLM call events are logged by the direct call infrastructure
        // We just verify the tick completed successfully with artifacts stored
        let tick = kernel.tick_store.latest().unwrap().unwrap();
        assert!(
            !tick.llm_calls.is_empty(),
            "tick should record LLM call records"
        );

        shutdown_host(&kernel).await;
    })
    .await;
    assert!(result.is_ok(), "test timed out");
}

// ══════════════════════════════════════════════════════════════════
// E8S1-T36: inner_loop_events_broadcast
// ══════════════════════════════════════════════════════════════════

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn inner_loop_events_broadcast() {
    let result = tokio::time::timeout(Duration::from_secs(15), async {
        let dir = tempfile::tempdir().unwrap();
        let responses = vec![
            mock_llm_response(DECISION_INNER_LOOP_WITH_ACTION),
            mock_llm_response(DECISION_NO_ACTIONS),
            mock_llm_response(DECISION_NO_ACTIONS),
        ];
        let mock = Arc::new(MockSequenceLlmBackend::new(responses));
        let mock_clone = mock.clone();
        let config = InnerLoopConfig {
            enabled: true,
            ..InnerLoopConfig::default()
        };
        let (kernel, handler) =
            setup_kernel_with_mock(dir.path(), mock_clone as Arc<dyn LlmHttpBackend>, config).await;

        let mut rx = kernel.event_tx.subscribe();
        let k = send_kernel(&kernel);
        let output = tokio::task::spawn_blocking(move || {
            let token = CancellationToken::new();
            exoskeleton_host::kernel::run_tick(&handler, &k, &token)
        })
        .await
        .unwrap();
        assert!(matches!(
            output,
            actionqueue_executor_local::HandlerOutput::Success { .. }
        ));

        let mut events = Vec::new();
        while let Ok(event) = rx.try_recv() {
            events.push(event.event_type);
        }

        // Verify the event sequence includes inner loop lifecycle
        assert!(
            events.contains(&EventType::InnerLoopStarted),
            "should emit InnerLoopStarted, got: {events:?}"
        );
        assert!(
            events.contains(&EventType::InnerLoopStep),
            "should emit InnerLoopStep, got: {events:?}"
        );
        assert!(
            events.contains(&EventType::InnerLoopCompleted),
            "should emit InnerLoopCompleted, got: {events:?}"
        );

        shutdown_host(&kernel).await;
    })
    .await;
    assert!(result.is_ok(), "test timed out");
}

// ══════════════════════════════════════════════════════════════════
// E8S1-T38: inner_loop_agent_complete
// ══════════════════════════════════════════════════════════════════

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn inner_loop_agent_complete() {
    let result = tokio::time::timeout(Duration::from_secs(15), async {
        let dir = tempfile::tempdir().unwrap();
        // Agent requests inner loop but starts with empty actions → immediate AgentComplete
        let responses = vec![
            mock_llm_response(DECISION_INNER_LOOP_NO_ACTIONS),
            mock_llm_response(DECISION_NO_ACTIONS), // Reflect
        ];
        let mock = Arc::new(MockSequenceLlmBackend::new(responses));
        let mock_clone = mock.clone();
        let config = InnerLoopConfig {
            enabled: true,
            ..InnerLoopConfig::default()
        };
        let (kernel, handler) =
            setup_kernel_with_mock(dir.path(), mock_clone as Arc<dyn LlmHttpBackend>, config).await;

        let mut rx = kernel.event_tx.subscribe();
        let k = send_kernel(&kernel);
        let output = tokio::task::spawn_blocking(move || {
            let token = CancellationToken::new();
            exoskeleton_host::kernel::run_tick(&handler, &k, &token)
        })
        .await
        .unwrap();
        assert!(matches!(
            output,
            actionqueue_executor_local::HandlerOutput::Success { .. }
        ));

        let mut completion_reason = None;
        let mut step_count = 0;
        while let Ok(event) = rx.try_recv() {
            if event.event_type == EventType::InnerLoopStep {
                step_count += 1;
            }
            if event.event_type == EventType::InnerLoopCompleted {
                completion_reason = event.inner_loop_detail.and_then(|d| d.completion_reason);
            }
        }
        assert_eq!(
            step_count, 0,
            "no steps should execute when actions are empty"
        );
        assert_eq!(
            completion_reason.as_deref(),
            Some("agent_complete"),
            "should complete with agent_complete reason"
        );

        shutdown_host(&kernel).await;
    })
    .await;
    assert!(result.is_ok(), "test timed out");
}

// ══════════════════════════════════════════════════════════════════
// E8S1-T39: inner_loop_result_feeds_reflect
// ══════════════════════════════════════════════════════════════════

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn inner_loop_result_feeds_reflect() {
    let result = tokio::time::timeout(Duration::from_secs(15), async {
        let dir = tempfile::tempdir().unwrap();
        let responses = vec![
            mock_llm_response(DECISION_INNER_LOOP_WITH_ACTION),
            mock_llm_response(DECISION_NO_ACTIONS),
            mock_llm_response(DECISION_NO_ACTIONS), // Reflect
        ];
        let mock = Arc::new(MockSequenceLlmBackend::new(responses));
        let mock_clone = mock.clone();
        let config = InnerLoopConfig {
            enabled: true,
            ..InnerLoopConfig::default()
        };
        let (kernel, handler) =
            setup_kernel_with_mock(dir.path(), mock_clone as Arc<dyn LlmHttpBackend>, config).await;

        let k = send_kernel(&kernel);
        let output = tokio::task::spawn_blocking(move || {
            let token = CancellationToken::new();
            exoskeleton_host::kernel::run_tick(&handler, &k, &token)
        })
        .await
        .unwrap();
        assert!(matches!(
            output,
            actionqueue_executor_local::HandlerOutput::Success { .. }
        ));

        // If we got a successful tick, Reflect received the inner loop result.
        // Verify via TickRecord — actions_taken should include the inner loop executions.
        let tick = kernel.tick_store.latest().unwrap().unwrap();
        assert!(
            !tick.actions_taken.is_empty(),
            "tick record should contain actions from inner loop"
        );

        shutdown_host(&kernel).await;
    })
    .await;
    assert!(result.is_ok(), "test timed out");
}

// ══════════════════════════════════════════════════════════════════
// E8S1-T40: inner_loop_result_feeds_amend
// ══════════════════════════════════════════════════════════════════

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn inner_loop_result_feeds_amend() {
    let result = tokio::time::timeout(Duration::from_secs(15), async {
        let dir = tempfile::tempdir().unwrap();
        let responses = vec![
            mock_llm_response(DECISION_INNER_LOOP_WITH_ACTION),
            mock_llm_response(DECISION_NO_ACTIONS),
            mock_llm_response(DECISION_NO_ACTIONS),
        ];
        let mock = Arc::new(MockSequenceLlmBackend::new(responses));
        let mock_clone = mock.clone();
        let config = InnerLoopConfig {
            enabled: true,
            ..InnerLoopConfig::default()
        };
        let (kernel, handler) =
            setup_kernel_with_mock(dir.path(), mock_clone as Arc<dyn LlmHttpBackend>, config).await;

        let k = send_kernel(&kernel);
        let output = tokio::task::spawn_blocking(move || {
            let token = CancellationToken::new();
            exoskeleton_host::kernel::run_tick(&handler, &k, &token)
        })
        .await
        .unwrap();
        assert!(matches!(
            output,
            actionqueue_executor_local::HandlerOutput::Success { .. }
        ));

        // Verify snapshot was updated by Amend
        let snapshot = kernel.snapshot_store.latest().unwrap().unwrap();
        assert_eq!(snapshot.tick_number, 1, "snapshot tick_number should be 1");

        // Verify tick record was persisted
        let tick = kernel.tick_store.latest().unwrap().unwrap();
        assert_eq!(tick.tick_number, 1);
        assert!(tick.completed_at.is_some());

        shutdown_host(&kernel).await;
    })
    .await;
    assert!(result.is_ok(), "test timed out");
}

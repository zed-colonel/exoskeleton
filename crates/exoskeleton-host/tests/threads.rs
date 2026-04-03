//! Sprint 6: Thread Integration Tests (T-7 / T-9)
//!
//! T-7: Master Loop Thread Integration — full PODAARA tick with threads
//! T-9: Thread Lifecycle — registration, suspension, deregistration,
//!       persistence across restarts
//!
//! DEADLOCK PREVENTION:
//! - All async tests use `flavor = "multi_thread", worker_threads = 2`
//! - All tests wrap body in `tokio::time::timeout`
//! - Tests calling `run_tick` use `spawn_blocking` (run_tick uses block_on)
//! - WI Host always shut down at end of test

mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use actionqueue_executor_local::CancellationToken;
use exoskeleton_core::conversation::InMemoryConversationStore;
use exoskeleton_core::llm::{LlmBackend, LlmRequest, LlmResponse, StopReason};
use exoskeleton_core::{
    ArtifactStore, EventType, LiveEvent, PromptRegistry, ThreadId, ThreadPriority, ThreadSchedule,
    ThreadSpec, ThreadStatus, VesselId,
};
use exoskeleton_host::cognitive_engine::CognitiveHandler;
use exoskeleton_host::inbox::InMemoryInbox;
use exoskeleton_host::kernel::{KernelContext, WiHostSlot};
use exoskeleton_host::storage::StorageManager;
use exoskeleton_host::LlmHttpBackend;
use exoskeleton_memory::{ApproximateTokenCounter, ContextCompiler};
use exoskeleton_relationship::InMemoryRelationshipLedger;
use exoskeleton_threads::{InMemoryThreadStore, ThreadRegistry};
use worldinterface_connector::connectors::DelayConnector;
use worldinterface_connector::registry::ConnectorRegistry;
use worldinterface_host::config::HostConfig;
use worldinterface_host::host::EmbeddedHost;

// ── Mock responses ──

/// Thread JSON response.
const THREAD_RESPONSE: &str =
    r#"{"summary":"all clear","recommendations":["continue monitoring"]}"#;

/// Master loop decision JSON with no actions.
const DECISION_NO_ACTIONS: &str =
    r#"{"reasoning":"No actions needed","actions":[],"memory_notes":[]}"#;

// ── Multi-response mock backend ──

/// A mock LLM backend that cycles through a list of responses.
///
/// Used for tests where threads AND the master loop's Decide step
/// both need LLM calls in one tick. Threads call first (by priority
/// order), then Decide calls.
struct MultiMockBackend {
    responses: Vec<String>,
    call_count: AtomicUsize,
}

impl MultiMockBackend {
    fn new(responses: Vec<String>) -> Self {
        Self {
            responses,
            call_count: AtomicUsize::new(0),
        }
    }

    fn call_count(&self) -> usize {
        self.call_count.load(Ordering::SeqCst)
    }
}

impl LlmHttpBackend for MultiMockBackend {
    fn call(
        &self,
        _client: &reqwest::Client,
        _request: &LlmRequest,
        _cancellation: &CancellationToken,
    ) -> Result<LlmResponse, exoskeleton_core::ExoError> {
        let idx = self.call_count.fetch_add(1, Ordering::SeqCst);
        let content = &self.responses[idx % self.responses.len()];
        Ok(LlmResponse {
            content: content.clone(),
            model: "mock".into(),
            tokens_in: 100,
            tokens_out: 50,
            latency_ms: 10,
            stop_reason: StopReason::EndTurn,
            cost_estimate_cents: None,
            backend: LlmBackend::Local,
        })
    }
}

/// A mock LLM backend that fails on certain call indices.
struct FailThenSucceedBackend {
    fail_indices: Vec<usize>,
    success_content: String,
    call_count: AtomicUsize,
}

impl FailThenSucceedBackend {
    fn new(fail_indices: Vec<usize>, success_content: String) -> Self {
        Self {
            fail_indices,
            success_content,
            call_count: AtomicUsize::new(0),
        }
    }
}

impl LlmHttpBackend for FailThenSucceedBackend {
    fn call(
        &self,
        _client: &reqwest::Client,
        _request: &LlmRequest,
        _cancellation: &CancellationToken,
    ) -> Result<LlmResponse, exoskeleton_core::ExoError> {
        let idx = self.call_count.fetch_add(1, Ordering::SeqCst);
        if self.fail_indices.contains(&idx) {
            return Err(exoskeleton_core::ExoError::LlmInvocation(
                "mock LLM failure".into(),
            ));
        }
        Ok(LlmResponse {
            content: self.success_content.clone(),
            model: "mock".into(),
            tokens_in: 100,
            tokens_out: 50,
            latency_ms: 10,
            stop_reason: StopReason::EndTurn,
            cost_estimate_cents: None,
            backend: LlmBackend::Local,
        })
    }
}

// ── Helper: build a ThreadSpec ──

fn make_thread(name: &str, priority: ThreadPriority, schedule: ThreadSchedule) -> ThreadSpec {
    ThreadSpec {
        thread_id: ThreadId::new(),
        name: name.into(),
        charter: format!("Charter for {name}"),
        priority,
        token_budget: 4000,
        schedule,
    }
}

// ── Helper: clone KernelContext for spawn_blocking ──

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
    }
}

// ── Helper: shut down WI Host ──

async fn shutdown_host(kernel: &KernelContext) {
    let mut guard = kernel.wi_host_slot.lock().await;
    if let Some(host) = guard.take() {
        host.shutdown().await.ok();
    }
}

// ── Helper: setup kernel with threads and multi-mock ──

async fn setup_with_threads(
    dir: &std::path::Path,
    responses: Vec<String>,
    thread_specs: Vec<ThreadSpec>,
) -> (KernelContext, CognitiveHandler, Arc<MultiMockBackend>) {
    // Storage
    std::fs::create_dir_all(dir.join("exo")).unwrap();
    std::fs::create_dir_all(dir.join("wi").join("aq")).unwrap();
    let storage = StorageManager::open(dir).unwrap();
    let artifact_store: Arc<dyn ArtifactStore> = storage.artifact_store().clone();

    // Multi-response mock
    let mock = Arc::new(MultiMockBackend::new(responses));
    let mock_as_http: Arc<dyn LlmHttpBackend> = mock.clone();

    let handler = CognitiveHandler::with_backends(
        Some(mock_as_http),
        None,
        LlmBackend::Local,
        artifact_store.clone(),
    );

    // WI Host
    let registry = ConnectorRegistry::new();
    registry.register(Arc::new(DelayConnector));
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

    // Thread registry
    let thread_store = Arc::new(InMemoryThreadStore::new());
    let thread_registry = Arc::new(ThreadRegistry::new(thread_store));
    for spec in thread_specs {
        thread_registry.register(spec).unwrap();
    }

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
        mission: "thread test".into(),
        max_output_tokens: 4096,
        master_loop_interval_secs: 60,
        thread_registry,
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
        read_paths_this_tick: Arc::new(std::sync::Mutex::new(std::collections::HashSet::new())),
        inner_loop_config: exoskeleton_host::config::InnerLoopConfig::default(),
        tool_policy: exoskeleton_host::kernel::policy::ToolPolicyConfig::default(),
        session_approvals: exoskeleton_host::kernel::policy::SessionApprovals::new(),
        vessel_mode: Arc::new(std::sync::Mutex::new(exoskeleton_core::VesselMode::Normal)),
        wake_signal: None,
    };

    (kernel, handler, mock)
}

/// Setup with a custom LlmHttpBackend (not MultiMockBackend).
async fn setup_with_threads_custom_backend(
    dir: &std::path::Path,
    backend: Arc<dyn LlmHttpBackend>,
    thread_specs: Vec<ThreadSpec>,
) -> (KernelContext, CognitiveHandler) {
    std::fs::create_dir_all(dir.join("exo")).unwrap();
    std::fs::create_dir_all(dir.join("wi").join("aq")).unwrap();
    let storage = StorageManager::open(dir).unwrap();
    let artifact_store: Arc<dyn ArtifactStore> = storage.artifact_store().clone();

    let handler = CognitiveHandler::with_backends(
        Some(backend),
        None,
        LlmBackend::Local,
        artifact_store.clone(),
    );

    let registry = ConnectorRegistry::new();
    registry.register(Arc::new(DelayConnector));
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

    let counter = Arc::new(ApproximateTokenCounter);
    let compiler = ContextCompiler::with_defaults(counter, 16000);

    let thread_store = Arc::new(InMemoryThreadStore::new());
    let thread_registry = Arc::new(ThreadRegistry::new(thread_store));
    for spec in thread_specs {
        thread_registry.register(spec).unwrap();
    }

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
        mission: "thread test".into(),
        max_output_tokens: 4096,
        master_loop_interval_secs: 60,
        thread_registry,
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
        read_paths_this_tick: Arc::new(std::sync::Mutex::new(std::collections::HashSet::new())),
        inner_loop_config: exoskeleton_host::config::InnerLoopConfig::default(),
        tool_policy: exoskeleton_host::kernel::policy::ToolPolicyConfig::default(),
        session_approvals: exoskeleton_host::kernel::policy::SessionApprovals::new(),
        vessel_mode: Arc::new(std::sync::Mutex::new(exoskeleton_core::VesselMode::Normal)),
        wake_signal: None,
    };

    (kernel, handler)
}

// ══════════════════════════════════════════════════════════════════
// T-7: Master Loop Thread Integration
// ══════════════════════════════════════════════════════════════════

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tick_with_no_threads() {
    let result = tokio::time::timeout(Duration::from_secs(10), async {
        let dir = tempfile::tempdir().unwrap();
        let (kernel, handler, _mock) =
            setup_with_threads(dir.path(), vec![DECISION_NO_ACTIONS.into()], vec![]).await;

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
            other => {
                panic!("expected Success, got: {other:?}")
            }
        }

        let record = tick_store.latest().unwrap().unwrap();
        assert!(
            record.thread_contributions.is_empty(),
            "expected no thread contributions, got: {:?}",
            record.thread_contributions
        );

        shutdown_host(&kernel).await;
    })
    .await;
    assert!(result.is_ok(), "test timed out");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tick_with_one_thread() {
    let result = tokio::time::timeout(Duration::from_secs(10), async {
        let dir = tempfile::tempdir().unwrap();
        let thread = make_thread(
            "ThreatMon",
            ThreadPriority::Normal,
            ThreadSchedule::EveryTick,
        );
        let thread_id = thread.thread_id;

        let (kernel, handler, _mock) = setup_with_threads(
            dir.path(),
            vec![THREAD_RESPONSE.into(), DECISION_NO_ACTIONS.into()],
            vec![thread],
        )
        .await;

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
            other => {
                panic!("expected Success, got: {other:?}")
            }
        }

        let record = tick_store.latest().unwrap().unwrap();
        assert_eq!(
            record.thread_contributions.len(),
            1,
            "expected 1 thread contribution"
        );
        assert_eq!(record.thread_contributions[0].thread_id, thread_id,);
        assert_eq!(record.thread_contributions[0].summary, "all clear",);

        shutdown_host(&kernel).await;
    })
    .await;
    assert!(result.is_ok(), "test timed out");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tick_with_multiple_threads() {
    let result = tokio::time::timeout(Duration::from_secs(10), async {
        let dir = tempfile::tempdir().unwrap();
        let t_crit = make_thread(
            "Critical",
            ThreadPriority::Critical,
            ThreadSchedule::EveryTick,
        );
        let t_norm = make_thread("Normal", ThreadPriority::Normal, ThreadSchedule::EveryTick);
        let t_bg = make_thread(
            "Background",
            ThreadPriority::Background,
            ThreadSchedule::EveryTick,
        );

        let (kernel, handler, mock) = setup_with_threads(
            dir.path(),
            vec![
                THREAD_RESPONSE.into(),
                THREAD_RESPONSE.into(),
                THREAD_RESPONSE.into(),
                DECISION_NO_ACTIONS.into(),
            ],
            vec![t_crit.clone(), t_norm.clone(), t_bg.clone()],
        )
        .await;

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
            other => {
                panic!("expected Success, got: {other:?}")
            }
        }

        let record = tick_store.latest().unwrap().unwrap();
        assert_eq!(
            record.thread_contributions.len(),
            3,
            "expected 3 thread contributions"
        );

        // All 4 calls made (3 threads + 1 decision)
        assert_eq!(mock.call_count(), 4);

        shutdown_host(&kernel).await;
    })
    .await;
    assert!(result.is_ok(), "test timed out");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn thread_output_in_compiled_context() {
    let result = tokio::time::timeout(Duration::from_secs(10), async {
        let dir = tempfile::tempdir().unwrap();
        let thread = make_thread(
            "ContextThread",
            ThreadPriority::Normal,
            ThreadSchedule::EveryTick,
        );

        use std::sync::Mutex;

        struct CapturingMultiMock {
            responses: Vec<String>,
            call_count: AtomicUsize,
            captured_requests: Mutex<Vec<LlmRequest>>,
        }

        impl LlmHttpBackend for CapturingMultiMock {
            fn call(
                &self,
                _client: &reqwest::Client,
                request: &LlmRequest,
                _cancellation: &CancellationToken,
            ) -> Result<LlmResponse, exoskeleton_core::ExoError> {
                let idx = self.call_count.fetch_add(1, Ordering::SeqCst);
                self.captured_requests.lock().unwrap().push(request.clone());
                let content = &self.responses[idx % self.responses.len()];
                Ok(LlmResponse {
                    content: content.clone(),
                    model: "mock".into(),
                    tokens_in: 100,
                    tokens_out: 50,
                    latency_ms: 10,
                    stop_reason: StopReason::EndTurn,
                    cost_estimate_cents: None,
                    backend: LlmBackend::Local,
                })
            }
        }

        let capturing = Arc::new(CapturingMultiMock {
            responses: vec![THREAD_RESPONSE.into(), DECISION_NO_ACTIONS.into()],
            call_count: AtomicUsize::new(0),
            captured_requests: Mutex::new(Vec::new()),
        });

        let (kernel, handler) = setup_with_threads_custom_backend(
            dir.path(),
            capturing.clone() as Arc<dyn LlmHttpBackend>,
            vec![thread],
        )
        .await;

        let k = send_kernel(&kernel);
        tokio::task::spawn_blocking(move || {
            let token = CancellationToken::new();
            exoskeleton_host::kernel::run_tick(&handler, &k, &token)
        })
        .await
        .unwrap();

        {
            let requests = capturing.captured_requests.lock().unwrap();
            assert!(
                requests.len() >= 2,
                "expected >= 2 LLM calls, got {}",
                requests.len()
            );
            // The Decide step sends compiled context as the
            // user message (not the system prompt). Thread
            // outputs are rendered in the compiled context.
            let decide_request = &requests[1];
            let user_content = decide_request
                .messages
                .first()
                .map(|m| m.content.as_str())
                .unwrap_or("");
            assert!(
                user_content.contains("all clear"),
                "Decide user message should contain \
                     thread output 'all clear', got: {}",
                &user_content[..user_content.len().min(500)]
            );
        }

        shutdown_host(&kernel).await;
    })
    .await;
    assert!(result.is_ok(), "test timed out");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn thread_output_in_tick_record() {
    let result = tokio::time::timeout(Duration::from_secs(10), async {
        let dir = tempfile::tempdir().unwrap();
        let thread = make_thread(
            "RecordThread",
            ThreadPriority::Normal,
            ThreadSchedule::EveryTick,
        );
        let thread_id = thread.thread_id;

        let (kernel, handler, _mock) = setup_with_threads(
            dir.path(),
            vec![THREAD_RESPONSE.into(), DECISION_NO_ACTIONS.into()],
            vec![thread],
        )
        .await;

        let tick_store = kernel.tick_store.clone();
        let k = send_kernel(&kernel);
        tokio::task::spawn_blocking(move || {
            let token = CancellationToken::new();
            exoskeleton_host::kernel::run_tick(&handler, &k, &token)
        })
        .await
        .unwrap();

        let record = tick_store.latest().unwrap().unwrap();
        assert_eq!(record.thread_contributions.len(), 1);
        let tc = &record.thread_contributions[0];
        assert_eq!(tc.thread_id, thread_id);
        assert_eq!(tc.summary, "all clear");
        assert!(
            !tc.artifact_id.to_string().is_empty(),
            "artifact_id should be set"
        );

        shutdown_host(&kernel).await;
    })
    .await;
    assert!(result.is_ok(), "test timed out");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn thread_failure_does_not_abort_tick() {
    let result = tokio::time::timeout(Duration::from_secs(10), async {
        let dir = tempfile::tempdir().unwrap();
        let thread = make_thread(
            "FailThread",
            ThreadPriority::Normal,
            ThreadSchedule::EveryTick,
        );

        let backend = Arc::new(FailThenSucceedBackend::new(
            vec![0],
            DECISION_NO_ACTIONS.into(),
        ));

        let (kernel, handler) = setup_with_threads_custom_backend(
            dir.path(),
            backend as Arc<dyn LlmHttpBackend>,
            vec![thread],
        )
        .await;

        let k = send_kernel(&kernel);
        let output = tokio::task::spawn_blocking(move || {
            let token = CancellationToken::new();
            exoskeleton_host::kernel::run_tick(&handler, &k, &token)
        })
        .await
        .unwrap();

        match &output {
            actionqueue_executor_local::HandlerOutput::Success { .. } => {}
            other => panic!(
                "expected Success despite thread \
                     failure, got: {other:?}"
            ),
        }

        let record = kernel.tick_store.latest().unwrap().unwrap();
        assert!(
            record.thread_contributions.is_empty(),
            "failed thread should not contribute"
        );

        shutdown_host(&kernel).await;
    })
    .await;
    assert!(result.is_ok(), "test timed out");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn thread_cancellation_skips_remaining() {
    let result = tokio::time::timeout(Duration::from_secs(10), async {
        let dir = tempfile::tempdir().unwrap();
        let t1 = make_thread(
            "Thread1",
            ThreadPriority::Critical,
            ThreadSchedule::EveryTick,
        );
        let t2 = make_thread("Thread2", ThreadPriority::Normal, ThreadSchedule::EveryTick);

        let (kernel, handler, mock) = setup_with_threads(
            dir.path(),
            vec![
                THREAD_RESPONSE.into(),
                THREAD_RESPONSE.into(),
                DECISION_NO_ACTIONS.into(),
            ],
            vec![t1, t2],
        )
        .await;

        let k = send_kernel(&kernel);
        let output = tokio::task::spawn_blocking(move || {
            let token = CancellationToken::new();
            token.cancel();
            exoskeleton_host::kernel::run_tick(&handler, &k, &token)
        })
        .await
        .unwrap();

        // Pre-cancelled token: threads get skipped,
        // tick returns retryable failure or partial.
        match &output {
            actionqueue_executor_local::HandlerOutput::RetryableFailure { .. } => {}
            actionqueue_executor_local::HandlerOutput::Success { .. } => {}
            other => panic!(
                "expected RetryableFailure or \
                     Success, got: {other:?}"
            ),
        }

        let calls = mock.call_count();
        assert!(
            calls <= 3,
            "expected <= 3 calls with cancellation, \
                 got {calls}"
        );

        shutdown_host(&kernel).await;
    })
    .await;
    assert!(result.is_ok(), "test timed out");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn thread_summaries_in_snapshot() {
    let result = tokio::time::timeout(Duration::from_secs(10), async {
        let dir = tempfile::tempdir().unwrap();
        let thread = make_thread(
            "SnapshotThread",
            ThreadPriority::Normal,
            ThreadSchedule::EveryTick,
        );
        let thread_name = thread.name.clone();

        let (kernel, handler, _mock) = setup_with_threads(
            dir.path(),
            vec![THREAD_RESPONSE.into(), DECISION_NO_ACTIONS.into()],
            vec![thread],
        )
        .await;

        let snapshot_store = kernel.snapshot_store.clone();
        let k = send_kernel(&kernel);
        tokio::task::spawn_blocking(move || {
            let token = CancellationToken::new();
            exoskeleton_host::kernel::run_tick(&handler, &k, &token)
        })
        .await
        .unwrap();

        let snapshot = snapshot_store.latest().unwrap().unwrap();
        assert_eq!(
            snapshot.thread_summaries.len(),
            1,
            "expected 1 thread summary in snapshot"
        );
        let ts = &snapshot.thread_summaries[0];
        assert_eq!(ts.name, thread_name);
        assert_eq!(ts.status, ThreadStatus::Active);

        shutdown_host(&kernel).await;
    })
    .await;
    assert!(result.is_ok(), "test timed out");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_n_ticks_scheduling() {
    let result = tokio::time::timeout(Duration::from_secs(15), async {
        let dir = tempfile::tempdir().unwrap();
        let thread = make_thread(
            "Periodic",
            ThreadPriority::Normal,
            ThreadSchedule::EveryNTicks(2),
        );

        // Tick 1: due (never run) -> thread + decision
        // Tick 2: not due (elapsed=1<2) -> decision only
        // Tick 3: due (elapsed=2>=2) -> thread + decision
        // Total LLM calls: 2 + 1 + 2 = 5
        let responses: Vec<String> = vec![
            THREAD_RESPONSE.into(),
            DECISION_NO_ACTIONS.into(),
            DECISION_NO_ACTIONS.into(),
            THREAD_RESPONSE.into(),
            DECISION_NO_ACTIONS.into(),
        ];

        let (kernel, _handler, mock) =
            setup_with_threads(dir.path(), responses, vec![thread]).await;

        let tick_store = kernel.tick_store.clone();

        // Tick 1
        let k1 = send_kernel(&kernel);
        let b1: Arc<dyn LlmHttpBackend> = mock.clone();
        let a1 = kernel.artifact_store.clone();
        let h1 = CognitiveHandler::with_backends(Some(b1), None, LlmBackend::Local, a1);
        tokio::task::spawn_blocking(move || {
            let token = CancellationToken::new();
            exoskeleton_host::kernel::run_tick(&h1, &k1, &token)
        })
        .await
        .unwrap();

        let r1 = tick_store.latest().unwrap().unwrap();
        assert_eq!(r1.tick_number, 1);
        assert_eq!(
            r1.thread_contributions.len(),
            1,
            "tick 1: thread should be due"
        );

        // Tick 2
        let k2 = send_kernel(&kernel);
        let b2: Arc<dyn LlmHttpBackend> = mock.clone();
        let a2 = kernel.artifact_store.clone();
        let h2 = CognitiveHandler::with_backends(Some(b2), None, LlmBackend::Local, a2);
        tokio::task::spawn_blocking(move || {
            let token = CancellationToken::new();
            exoskeleton_host::kernel::run_tick(&h2, &k2, &token)
        })
        .await
        .unwrap();

        let r2 = tick_store.latest().unwrap().unwrap();
        assert_eq!(r2.tick_number, 2);
        assert!(
            r2.thread_contributions.is_empty(),
            "tick 2: thread should NOT be due"
        );

        // Tick 3
        let k3 = send_kernel(&kernel);
        let b3: Arc<dyn LlmHttpBackend> = mock.clone();
        let a3 = kernel.artifact_store.clone();
        let h3 = CognitiveHandler::with_backends(Some(b3), None, LlmBackend::Local, a3);
        tokio::task::spawn_blocking(move || {
            let token = CancellationToken::new();
            exoskeleton_host::kernel::run_tick(&h3, &k3, &token)
        })
        .await
        .unwrap();

        let r3 = tick_store.latest().unwrap().unwrap();
        assert_eq!(r3.tick_number, 3);
        assert_eq!(
            r3.thread_contributions.len(),
            1,
            "tick 3: thread should be due"
        );

        assert_eq!(mock.call_count(), 5, "expected 5 total LLM calls (2+1+2)");

        shutdown_host(&kernel).await;
    })
    .await;
    assert!(result.is_ok(), "test timed out");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn suspended_thread_not_executed() {
    let result = tokio::time::timeout(Duration::from_secs(10), async {
        let dir = tempfile::tempdir().unwrap();
        let thread = make_thread(
            "SuspendMe",
            ThreadPriority::Normal,
            ThreadSchedule::EveryTick,
        );
        let thread_id = thread.thread_id;

        let (kernel, handler, mock) =
            setup_with_threads(dir.path(), vec![DECISION_NO_ACTIONS.into()], vec![thread]).await;

        kernel
            .thread_registry
            .update_status(thread_id, ThreadStatus::Suspended)
            .unwrap();

        let tick_store = kernel.tick_store.clone();
        let k = send_kernel(&kernel);
        tokio::task::spawn_blocking(move || {
            let token = CancellationToken::new();
            exoskeleton_host::kernel::run_tick(&handler, &k, &token)
        })
        .await
        .unwrap();

        let record = tick_store.latest().unwrap().unwrap();
        assert!(
            record.thread_contributions.is_empty(),
            "suspended thread should not contribute"
        );
        assert_eq!(mock.call_count(), 1, "only decision LLM call expected");

        shutdown_host(&kernel).await;
    })
    .await;
    assert!(result.is_ok(), "test timed out");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn thread_event_logged() {
    let result = tokio::time::timeout(Duration::from_secs(10), async {
        let dir = tempfile::tempdir().unwrap();
        let thread = make_thread(
            "EventThread",
            ThreadPriority::Normal,
            ThreadSchedule::EveryTick,
        );

        let (kernel, handler, _mock) = setup_with_threads(
            dir.path(),
            vec![THREAD_RESPONSE.into(), DECISION_NO_ACTIONS.into()],
            vec![thread],
        )
        .await;

        let event_ledger = kernel.event_ledger.clone();
        let k = send_kernel(&kernel);
        tokio::task::spawn_blocking(move || {
            let token = CancellationToken::new();
            exoskeleton_host::kernel::run_tick(&handler, &k, &token)
        })
        .await
        .unwrap();

        let events = event_ledger.recent(100).unwrap();
        let thread_events: Vec<_> = events
            .iter()
            .filter(|e| e.event_type == EventType::ThreadRan)
            .collect();
        assert_eq!(thread_events.len(), 1, "expected 1 ThreadRan event");
        assert!(
            thread_events[0].summary.contains("EventThread"),
            "ThreadRan event should mention thread \
                 name"
        );
        assert!(
            thread_events[0].summary.contains("all clear"),
            "ThreadRan event should include summary"
        );

        shutdown_host(&kernel).await;
    })
    .await;
    assert!(result.is_ok(), "test timed out");
}

#[test]
fn thread_never_invokes_tools() {
    // Only audit production code (before the #[cfg(test)] marker).
    let full_source = include_str!("../src/kernel/threads.rs");
    let production_code = full_source
        .split("#[cfg(test)]")
        .next()
        .unwrap_or(full_source);
    assert!(
        !production_code.contains("invoke_single"),
        "threads.rs production code must not call \
         invoke_single (threads never invoke tools)"
    );
    assert!(
        !production_code.contains("wi_host_slot"),
        "threads.rs production code must not reference \
         wi_host_slot (threads never cross to Tool AQ)"
    );
}

// ══════════════════════════════════════════════════════════════════
// T-9: Thread Lifecycle
// ══════════════════════════════════════════════════════════════════

#[test]
fn thread_survives_storage_restart() {
    use exoskeleton_host::SqliteThreadStore;
    use exoskeleton_threads::ThreadStore;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("threads.db");
    let spec = make_thread(
        "Persistent",
        ThreadPriority::Normal,
        ThreadSchedule::EveryTick,
    );
    let thread_id = spec.thread_id;

    {
        let store = SqliteThreadStore::open(&path).unwrap();
        store.save(&spec, ThreadStatus::Active).unwrap();
    }

    {
        let store = SqliteThreadStore::open(&path).unwrap();
        let result = store.get(thread_id).unwrap();
        assert!(result.is_some(), "thread should survive restart");
        let (got_spec, got_status) = result.unwrap();
        assert_eq!(got_spec, spec);
        assert_eq!(got_status, ThreadStatus::Active);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deregistered_thread_stops_executing() {
    let result = tokio::time::timeout(Duration::from_secs(15), async {
        let dir = tempfile::tempdir().unwrap();
        let thread = make_thread(
            "Deregister",
            ThreadPriority::Normal,
            ThreadSchedule::EveryTick,
        );
        let thread_id = thread.thread_id;

        let (kernel, _handler, mock) = setup_with_threads(
            dir.path(),
            vec![
                THREAD_RESPONSE.into(),
                DECISION_NO_ACTIONS.into(),
                DECISION_NO_ACTIONS.into(),
            ],
            vec![thread],
        )
        .await;

        let tick_store = kernel.tick_store.clone();

        // Tick 1: thread executes
        let k1 = send_kernel(&kernel);
        let b1: Arc<dyn LlmHttpBackend> = mock.clone();
        let a1 = kernel.artifact_store.clone();
        let h1 = CognitiveHandler::with_backends(Some(b1), None, LlmBackend::Local, a1);
        tokio::task::spawn_blocking(move || {
            let token = CancellationToken::new();
            exoskeleton_host::kernel::run_tick(&h1, &k1, &token)
        })
        .await
        .unwrap();

        let r1 = tick_store.latest().unwrap().unwrap();
        assert_eq!(
            r1.thread_contributions.len(),
            1,
            "tick 1: thread should execute"
        );

        // Deregister
        kernel.thread_registry.deregister(thread_id).unwrap();

        // Tick 2: no thread
        let k2 = send_kernel(&kernel);
        let b2: Arc<dyn LlmHttpBackend> = mock.clone();
        let a2 = kernel.artifact_store.clone();
        let h2 = CognitiveHandler::with_backends(Some(b2), None, LlmBackend::Local, a2);
        tokio::task::spawn_blocking(move || {
            let token = CancellationToken::new();
            exoskeleton_host::kernel::run_tick(&h2, &k2, &token)
        })
        .await
        .unwrap();

        let r2 = tick_store.latest().unwrap().unwrap();
        assert_eq!(r2.tick_number, 2);
        assert!(
            r2.thread_contributions.is_empty(),
            "tick 2: deregistered thread should \
                 not execute"
        );

        assert_eq!(mock.call_count(), 3);

        shutdown_host(&kernel).await;
    })
    .await;
    assert!(result.is_ok(), "test timed out");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn suspended_thread_skips_execution() {
    let result = tokio::time::timeout(Duration::from_secs(10), async {
        let dir = tempfile::tempdir().unwrap();
        let thread = make_thread(
            "SuspendSkip",
            ThreadPriority::Normal,
            ThreadSchedule::EveryTick,
        );
        let thread_id = thread.thread_id;

        let (kernel, handler, mock) =
            setup_with_threads(dir.path(), vec![DECISION_NO_ACTIONS.into()], vec![thread]).await;

        kernel
            .thread_registry
            .update_status(thread_id, ThreadStatus::Suspended)
            .unwrap();

        let tick_store = kernel.tick_store.clone();
        let k = send_kernel(&kernel);
        tokio::task::spawn_blocking(move || {
            let token = CancellationToken::new();
            exoskeleton_host::kernel::run_tick(&handler, &k, &token)
        })
        .await
        .unwrap();

        let record = tick_store.latest().unwrap().unwrap();
        assert!(
            record.thread_contributions.is_empty(),
            "suspended thread should not contribute"
        );
        assert_eq!(mock.call_count(), 1, "only decision LLM call expected");

        shutdown_host(&kernel).await;
    })
    .await;
    assert!(result.is_ok(), "test timed out");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn thread_status_persists_across_ticks() {
    let result = tokio::time::timeout(Duration::from_secs(10), async {
        let dir = tempfile::tempdir().unwrap();
        let thread = make_thread(
            "StatusPersist",
            ThreadPriority::Normal,
            ThreadSchedule::EveryTick,
        );
        let thread_id = thread.thread_id;

        let (kernel, handler, _mock) = setup_with_threads(
            dir.path(),
            vec![THREAD_RESPONSE.into(), DECISION_NO_ACTIONS.into()],
            vec![thread],
        )
        .await;

        let k = send_kernel(&kernel);
        tokio::task::spawn_blocking(move || {
            let token = CancellationToken::new();
            exoskeleton_host::kernel::run_tick(&handler, &k, &token)
        })
        .await
        .unwrap();

        let entry = kernel.thread_registry.get(thread_id).unwrap().unwrap();
        assert_eq!(
            entry.1,
            ThreadStatus::Active,
            "thread should remain Active after tick"
        );

        shutdown_host(&kernel).await;
    })
    .await;
    assert!(result.is_ok(), "test timed out");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn completed_thread_never_runs() {
    let result = tokio::time::timeout(Duration::from_secs(10), async {
        let dir = tempfile::tempdir().unwrap();
        let thread = make_thread(
            "CompletedThread",
            ThreadPriority::Normal,
            ThreadSchedule::EveryTick,
        );
        let thread_id = thread.thread_id;

        let (kernel, handler, mock) =
            setup_with_threads(dir.path(), vec![DECISION_NO_ACTIONS.into()], vec![thread]).await;

        kernel
            .thread_registry
            .update_status(thread_id, ThreadStatus::Completed)
            .unwrap();

        let tick_store = kernel.tick_store.clone();
        let k = send_kernel(&kernel);
        tokio::task::spawn_blocking(move || {
            let token = CancellationToken::new();
            exoskeleton_host::kernel::run_tick(&handler, &k, &token)
        })
        .await
        .unwrap();

        let record = tick_store.latest().unwrap().unwrap();
        assert!(
            record.thread_contributions.is_empty(),
            "completed thread should never run"
        );
        assert_eq!(mock.call_count(), 1, "only decision LLM call expected");

        shutdown_host(&kernel).await;
    })
    .await;
    assert!(result.is_ok(), "test timed out");
}

// ── T-13: Property-Based Tests (Sprint 6) ──

mod proptest_tests {
    use exoskeleton_core::{
        ArtifactId, StateSnapshot, ThreadId, ThreadOutput, ThreadPriority, ThreadSchedule,
        ThreadSpec, ThreadStatus, TickId, VesselId,
    };
    use exoskeleton_memory::ApproximateTokenCounter;
    use exoskeleton_threads::{compile_thread_context, is_thread_due};
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn thread_spec_survives_json_roundtrip(
            name in "[a-zA-Z ]{1,50}",
            charter in "[a-zA-Z ]{1,200}",
            budget in 100u64..10000,
        ) {
            let spec = ThreadSpec {
                thread_id: ThreadId::new(),
                name,
                charter,
                priority: ThreadPriority::Normal,
                token_budget: budget,
                schedule: ThreadSchedule::EveryTick,
            };
            let json =
                serde_json::to_string(&spec).unwrap();
            let parsed: ThreadSpec =
                serde_json::from_str(&json).unwrap();
            prop_assert_eq!(spec, parsed);
        }

        #[test]
        fn thread_output_survives_json_roundtrip(
            summary in "[a-zA-Z ]{1,100}",
            rec1 in "[a-zA-Z ]{1,50}",
            rec2 in "[a-zA-Z ]{1,50}",
        ) {
            let output = ThreadOutput {
                thread_id: ThreadId::new(),
                tick_id: TickId::new(),
                artifact_id: ArtifactId::from_content(
                    summary.as_bytes(),
                ),
                summary,
                recommendations: vec![rec1, rec2],
            };
            let json =
                serde_json::to_string(&output).unwrap();
            let parsed: ThreadOutput =
                serde_json::from_str(&json).unwrap();
            prop_assert_eq!(output, parsed);
        }

        #[test]
        fn is_thread_due_consistent_for_every_tick(
            tick in 0u64..10000,
        ) {
            prop_assert!(is_thread_due(
                ThreadSchedule::EveryTick,
                ThreadStatus::Active,
                tick,
                Some(tick.saturating_sub(1)),
            ));
        }

        #[test]
        fn thread_context_within_budget(
            budget in 200u64..5000,
        ) {
            let counter = ApproximateTokenCounter;
            let spec = ThreadSpec {
                thread_id: ThreadId::new(),
                name: "PropTest Thread".into(),
                charter: "Analyze for test".into(),
                priority: ThreadPriority::Normal,
                token_budget: budget,
                schedule: ThreadSchedule::EveryTick,
            };
            let snapshot = StateSnapshot::initial(
                VesselId::new(),
                "test".into(),
            );
            match compile_thread_context(
                &counter,
                &spec,
                &snapshot,
                &[],
                TickId::new(),
                None,
                None,
            ) {
                Ok(ctx) => {
                    prop_assert!(
                        ctx.total_tokens <= budget,
                        "tokens={} > budget={budget}",
                        ctx.total_tokens,
                    );
                }
                Err(_) => {
                    // Budget too small for charter
                }
            }
        }
    }
}

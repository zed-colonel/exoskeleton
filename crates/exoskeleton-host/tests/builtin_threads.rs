//! Sprint 7: Built-in Threads integration tests.

mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use actionqueue_executor_local::CancellationToken;
use exoskeleton_core::conversation::InMemoryConversationStore;
use exoskeleton_core::llm::{LlmBackend, LlmRequest, LlmResponse, StopReason};
use exoskeleton_core::{
    ArtifactKind, ArtifactStore, EventType, LiveEvent, PromptRegistry, ThreadPriority,
    ThreadSchedule, ThreadStatus, VesselId,
};
use exoskeleton_host::cognitive_engine::CognitiveHandler;
use exoskeleton_host::inbox::InMemoryInbox;
use exoskeleton_host::kernel::{KernelContext, WiHostSlot};
use exoskeleton_host::storage::StorageManager;
use exoskeleton_host::LlmHttpBackend;
use exoskeleton_memory::{ApproximateTokenCounter, ContextCompiler};
use exoskeleton_relationship::InMemoryRelationshipLedger;
use exoskeleton_threads::{
    builtin, register_builtin_threads, InMemoryThreadStore, ThreadRegistry,
    MEMORY_CONSOLIDATION_ID, SELF_CRITIQUE_ID, THREAT_MONITOR_ID,
};
use worldinterface_connector::connectors::DelayConnector;
use worldinterface_connector::registry::ConnectorRegistry;
use worldinterface_host::config::HostConfig;
use worldinterface_host::host::EmbeddedHost;

// ── Mock responses ──

const THREAT_RESPONSE: &str =
    r#"{"summary":"No threats detected","recommendations":[],"severity":"none","threats":[]}"#;

const CRITIQUE_RESPONSE: &str = r#"{"summary":"Good progress","recommendations":[],"progress_rating":0.8,"concerns":[],"suggestions":[],"thrash_indicator":0.0}"#;

const MC_RESPONSE: &str = r#"{"summary":"Consolidated recent ticks","recommendations":[],"episodic_summary":"Ticks covered in this span were productive","long_term_notes":[{"topic":"strategy","content":"Incremental approach is effective","tags":["reliability"]}],"deprecated_notes":[]}"#;

const DECISION_NO_ACTIONS: &str =
    r#"{"reasoning":"No actions needed","actions":[],"memory_notes":[]}"#;

// ── Multi-response mock (reuse pattern from sprint6) ──

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

/// A mock that fails on specified call indices.
struct FailOnIndexBackend {
    fail_indices: Vec<usize>,
    success_content: String,
    call_count: AtomicUsize,
}

impl FailOnIndexBackend {
    fn new(fail_indices: Vec<usize>, success_content: String) -> Self {
        Self {
            fail_indices,
            success_content,
            call_count: AtomicUsize::new(0),
        }
    }
}

impl LlmHttpBackend for FailOnIndexBackend {
    fn call(
        &self,
        _client: &reqwest::Client,
        _request: &LlmRequest,
        _cancellation: &CancellationToken,
    ) -> Result<LlmResponse, exoskeleton_core::ExoError> {
        let idx = self.call_count.fetch_add(1, Ordering::SeqCst);
        if self.fail_indices.contains(&idx) {
            return Err(exoskeleton_core::ExoError::LlmInvocation(
                "simulated failure".into(),
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

// ── Helpers ──

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
    }
}

async fn shutdown_host(kernel: &KernelContext) {
    let mut guard = kernel.wi_host_slot.lock().await;
    if let Some(host) = guard.take() {
        host.shutdown().await.ok();
    }
}

/// Setup kernel with all three built-in threads registered.
async fn setup_builtin_threads(
    dir: &std::path::Path,
    responses: Vec<String>,
) -> (KernelContext, CognitiveHandler, Arc<MultiMockBackend>) {
    std::fs::create_dir_all(dir.join("exo")).unwrap();
    std::fs::create_dir_all(dir.join("wi").join("aq")).unwrap();
    let storage = StorageManager::open(dir).unwrap();
    let artifact_store: Arc<dyn ArtifactStore> = storage.artifact_store().clone();

    let mock = Arc::new(MultiMockBackend::new(responses));
    let mock_as_http: Arc<dyn LlmHttpBackend> = mock.clone();

    let handler = CognitiveHandler::with_backends(
        Some(mock_as_http),
        None,
        LlmBackend::Local,
        artifact_store.clone(),
    );

    let mut registry = ConnectorRegistry::new();
    registry.register(Arc::new(DelayConnector));
    let host_config = HostConfig {
        aq_data_dir: dir.join("wi").join("aq"),
        context_store_path: dir.join("wi").join("context.db"),
        tick_interval: Duration::from_millis(10),
        ..Default::default()
    };
    let host = EmbeddedHost::start(host_config, registry).await.unwrap();
    let wi_host_slot: WiHostSlot = Arc::new(tokio::sync::Mutex::new(Some(host)));

    let counter = Arc::new(ApproximateTokenCounter);
    let compiler = ContextCompiler::with_defaults(counter, 16000);

    let thread_store = Arc::new(InMemoryThreadStore::new());
    let thread_registry = Arc::new(ThreadRegistry::new(thread_store));
    register_builtin_threads(&thread_registry, &PromptRegistry::with_defaults()).unwrap();

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
        mission: "sprint7 test".into(),
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
    };

    (kernel, handler, mock)
}

/// Setup with a custom backend.
async fn setup_builtin_threads_custom(
    dir: &std::path::Path,
    backend: Arc<dyn LlmHttpBackend>,
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

    let mut registry = ConnectorRegistry::new();
    registry.register(Arc::new(DelayConnector));
    let host_config = HostConfig {
        aq_data_dir: dir.join("wi").join("aq"),
        context_store_path: dir.join("wi").join("context.db"),
        tick_interval: Duration::from_millis(10),
        ..Default::default()
    };
    let host = EmbeddedHost::start(host_config, registry).await.unwrap();
    let wi_host_slot: WiHostSlot = Arc::new(tokio::sync::Mutex::new(Some(host)));

    let counter = Arc::new(ApproximateTokenCounter);
    let compiler = ContextCompiler::with_defaults(counter, 16000);

    let thread_store = Arc::new(InMemoryThreadStore::new());
    let thread_registry = Arc::new(ThreadRegistry::new(thread_store));
    register_builtin_threads(&thread_registry, &PromptRegistry::with_defaults()).unwrap();

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
        mission: "sprint7 test".into(),
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
    };

    (kernel, handler)
}

// ══════════════════════════════════════════════════════════════════
// T-3: Vessel Boot Registration
// ══════════════════════════════════════════════════════════════════

#[test]
fn builtin_threads_registered_on_setup() {
    let store = Arc::new(InMemoryThreadStore::new());
    let registry = ThreadRegistry::new(store);
    register_builtin_threads(&registry, &PromptRegistry::with_defaults()).unwrap();

    let all = registry.list().unwrap();
    assert_eq!(all.len(), 3);

    // Verify correct types
    let (tm, tm_status) = registry.get(THREAT_MONITOR_ID).unwrap().unwrap();
    assert_eq!(tm.name, "Threat Monitor");
    assert_eq!(tm.priority, ThreadPriority::Critical);
    assert_eq!(tm_status, ThreadStatus::Active);

    let (sc, sc_status) = registry.get(SELF_CRITIQUE_ID).unwrap().unwrap();
    assert_eq!(sc.name, "Self-Critique");
    assert_eq!(sc.priority, ThreadPriority::High);
    assert_eq!(sc_status, ThreadStatus::Active);

    let (mc, mc_status) = registry.get(MEMORY_CONSOLIDATION_ID).unwrap().unwrap();
    assert_eq!(mc.name, "Memory Consolidation");
    assert_eq!(mc.priority, ThreadPriority::Normal);
    assert_eq!(mc.schedule, ThreadSchedule::EveryNTicks(5));
    assert_eq!(mc_status, ThreadStatus::Active);
}

#[test]
fn builtin_registration_idempotent() {
    let store = Arc::new(InMemoryThreadStore::new());
    let registry = ThreadRegistry::new(store);
    register_builtin_threads(&registry, &PromptRegistry::with_defaults()).unwrap();
    register_builtin_threads(&registry, &PromptRegistry::with_defaults()).unwrap();
    register_builtin_threads(&registry, &PromptRegistry::with_defaults()).unwrap();
    assert_eq!(registry.list().unwrap().len(), 3);
}

#[test]
fn builtin_registration_preserves_suspended() {
    let store = Arc::new(InMemoryThreadStore::new());
    let registry = ThreadRegistry::new(store);
    register_builtin_threads(&registry, &PromptRegistry::with_defaults()).unwrap();

    // Operator suspends Threat Monitor
    registry
        .update_status(THREAT_MONITOR_ID, ThreadStatus::Suspended)
        .unwrap();

    // Re-register (simulate restart)
    register_builtin_threads(&registry, &PromptRegistry::with_defaults()).unwrap();

    let (_, status) = registry.get(THREAT_MONITOR_ID).unwrap().unwrap();
    assert_eq!(
        status,
        ThreadStatus::Suspended,
        "should preserve operator's suspension"
    );
    assert_eq!(registry.list().unwrap().len(), 3);
}

// ══════════════════════════════════════════════════════════════════
// T-4: Multi-Thread Tick Execution
// ══════════════════════════════════════════════════════════════════

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tick_with_all_builtin_threads() {
    let dir = tempfile::tempdir().unwrap();
    // Tick 1: all 3 threads due (MC due because never run) + decide
    let responses = vec![
        THREAT_RESPONSE.into(),
        CRITIQUE_RESPONSE.into(),
        MC_RESPONSE.into(),
        DECISION_NO_ACTIONS.into(),
    ];
    let (kernel, handler, mock) = setup_builtin_threads(dir.path(), responses).await;

    let k = send_kernel(&kernel);
    let result = tokio::time::timeout(
        Duration::from_secs(10),
        tokio::task::spawn_blocking(move || {
            let token = CancellationToken::new();
            exoskeleton_host::kernel::run_tick(&handler, &k, &token)
        }),
    )
    .await
    .unwrap()
    .unwrap();

    // Verify tick succeeded
    assert!(
        matches!(
            result,
            actionqueue_executor_local::HandlerOutput::Success { .. }
        ),
        "tick should succeed, got: {result:?}"
    );

    // Verify all 3 threads + decide were called
    assert_eq!(mock.call_count(), 4, "3 threads + 1 decide = 4 LLM calls");

    // Verify thread summaries in snapshot
    let snapshot = kernel.snapshot_store.latest().unwrap().unwrap();
    assert_eq!(
        snapshot.thread_summaries.len(),
        3,
        "all 3 built-in threads should appear in summaries"
    );

    shutdown_host(&kernel).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tick_2_only_everytick_threads_due() {
    let dir = tempfile::tempdir().unwrap();
    // Tick 1: 3 threads + decide; Tick 2: 2 threads + decide
    let responses = vec![
        // Tick 1
        THREAT_RESPONSE.into(),
        CRITIQUE_RESPONSE.into(),
        MC_RESPONSE.into(),
        DECISION_NO_ACTIONS.into(),
        // Tick 2
        THREAT_RESPONSE.into(),
        CRITIQUE_RESPONSE.into(),
        DECISION_NO_ACTIONS.into(),
    ];
    let (kernel, handler, mock) = setup_builtin_threads(dir.path(), responses).await;

    // Tick 1
    let k1 = send_kernel(&kernel);
    let h1 = handler;
    let result1 = tokio::time::timeout(
        Duration::from_secs(10),
        tokio::task::spawn_blocking(move || {
            let token = CancellationToken::new();
            let r = exoskeleton_host::kernel::run_tick(&h1, &k1, &token);
            (r, h1)
        }),
    )
    .await
    .unwrap()
    .unwrap();
    let handler = result1.1;
    assert!(matches!(
        result1.0,
        actionqueue_executor_local::HandlerOutput::Success { .. }
    ));

    // Tick 2
    let k2 = send_kernel(&kernel);
    let result2 = tokio::time::timeout(
        Duration::from_secs(10),
        tokio::task::spawn_blocking(move || {
            let token = CancellationToken::new();
            exoskeleton_host::kernel::run_tick(&handler, &k2, &token)
        }),
    )
    .await
    .unwrap()
    .unwrap();

    assert!(matches!(
        result2,
        actionqueue_executor_local::HandlerOutput::Success { .. }
    ));

    // Tick 1: 4 calls, Tick 2: 3 calls = 7 total
    assert_eq!(mock.call_count(), 7, "tick 1 (4) + tick 2 (3) = 7");

    shutdown_host(&kernel).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn thread_outputs_in_tick_record() {
    let dir = tempfile::tempdir().unwrap();
    let responses = vec![
        THREAT_RESPONSE.into(),
        CRITIQUE_RESPONSE.into(),
        MC_RESPONSE.into(),
        DECISION_NO_ACTIONS.into(),
    ];
    let (kernel, handler, _mock) = setup_builtin_threads(dir.path(), responses).await;

    let k = send_kernel(&kernel);
    tokio::time::timeout(
        Duration::from_secs(10),
        tokio::task::spawn_blocking(move || {
            let token = CancellationToken::new();
            exoskeleton_host::kernel::run_tick(&handler, &k, &token)
        }),
    )
    .await
    .unwrap()
    .unwrap();

    // Check tick record has thread contributions
    let tick_record = kernel.tick_store.latest().unwrap().unwrap();
    assert_eq!(
        tick_record.thread_contributions.len(),
        3,
        "3 thread contributions expected"
    );

    // Verify thread IDs in contributions
    let thread_ids: Vec<_> = tick_record
        .thread_contributions
        .iter()
        .map(|tc| tc.thread_id)
        .collect();
    assert!(thread_ids.contains(&THREAT_MONITOR_ID));
    assert!(thread_ids.contains(&SELF_CRITIQUE_ID));
    assert!(thread_ids.contains(&MEMORY_CONSOLIDATION_ID));

    shutdown_host(&kernel).await;
}

// ══════════════════════════════════════════════════════════════════
// T-5: Memory Consolidation Trigger
// ══════════════════════════════════════════════════════════════════

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn memory_consolidation_writes_to_memory_store() {
    let dir = tempfile::tempdir().unwrap();
    // MC runs on tick 1 (never run => due immediately)
    let responses = vec![
        THREAT_RESPONSE.into(),
        CRITIQUE_RESPONSE.into(),
        MC_RESPONSE.into(),
        DECISION_NO_ACTIONS.into(),
    ];
    let (kernel, handler, _mock) = setup_builtin_threads(dir.path(), responses).await;

    let k = send_kernel(&kernel);
    tokio::time::timeout(
        Duration::from_secs(10),
        tokio::task::spawn_blocking(move || {
            let token = CancellationToken::new();
            exoskeleton_host::kernel::run_tick(&handler, &k, &token)
        }),
    )
    .await
    .unwrap()
    .unwrap();

    // MC output should have triggered memory writes via Amend.
    // The MC_RESPONSE contains episodic_summary and a long_term_note.
    // The process_memory_consolidation_outputs function in amend.rs
    // reads the contribution and writes to memory store.

    // Check that at least some episodic summary was written
    let episodics = kernel.memory_store.recent_episodic(10).unwrap();
    assert!(
        !episodics.is_empty(),
        "Memory Consolidation should have written episodic summary"
    );

    shutdown_host(&kernel).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn memory_consolidation_artifact_stored() {
    let dir = tempfile::tempdir().unwrap();
    let responses = vec![
        THREAT_RESPONSE.into(),
        CRITIQUE_RESPONSE.into(),
        MC_RESPONSE.into(),
        DECISION_NO_ACTIONS.into(),
    ];
    let (kernel, handler, _mock) = setup_builtin_threads(dir.path(), responses).await;

    let k = send_kernel(&kernel);
    tokio::time::timeout(
        Duration::from_secs(10),
        tokio::task::spawn_blocking(move || {
            let token = CancellationToken::new();
            exoskeleton_host::kernel::run_tick(&handler, &k, &token)
        }),
    )
    .await
    .unwrap()
    .unwrap();

    // ThreadOutput artifacts should exist for MC
    let thread_outputs = kernel
        .artifact_store
        .list_by_kind(ArtifactKind::ThreadOutput, 10)
        .unwrap();
    assert!(
        thread_outputs.len() >= 3,
        "expected at least 3 ThreadOutput artifacts (one per thread)"
    );

    shutdown_host(&kernel).await;
}

// ══════════════════════════════════════════════════════════════════
// T-6: Multi-Thread Convergence (Acceptance Criterion A)
// ══════════════════════════════════════════════════════════════════

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn convergence_thread_summaries_in_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    // Just run one tick with all 3 threads
    let responses = vec![
        THREAT_RESPONSE.into(),
        CRITIQUE_RESPONSE.into(),
        MC_RESPONSE.into(),
        DECISION_NO_ACTIONS.into(),
    ];
    let (kernel, handler, _mock) = setup_builtin_threads(dir.path(), responses).await;

    let k = send_kernel(&kernel);
    tokio::time::timeout(
        Duration::from_secs(10),
        tokio::task::spawn_blocking(move || {
            let token = CancellationToken::new();
            exoskeleton_host::kernel::run_tick(&handler, &k, &token)
        }),
    )
    .await
    .unwrap()
    .unwrap();

    let snapshot = kernel.snapshot_store.latest().unwrap().unwrap();
    assert_eq!(snapshot.thread_summaries.len(), 3);

    // Verify names
    let names: Vec<&str> = snapshot
        .thread_summaries
        .iter()
        .map(|s| s.name.as_str())
        .collect();
    assert!(names.contains(&"Threat Monitor"));
    assert!(names.contains(&"Self-Critique"));
    assert!(names.contains(&"Memory Consolidation"));

    // Verify summaries have content from thread outputs
    for ts in &snapshot.thread_summaries {
        assert!(
            ts.last_output_summary.is_some(),
            "thread {} should have last_output_summary",
            ts.name
        );
    }

    shutdown_host(&kernel).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn convergence_all_outputs_are_durable_artifacts() {
    let dir = tempfile::tempdir().unwrap();
    let responses = vec![
        THREAT_RESPONSE.into(),
        CRITIQUE_RESPONSE.into(),
        MC_RESPONSE.into(),
        DECISION_NO_ACTIONS.into(),
    ];
    let (kernel, handler, _mock) = setup_builtin_threads(dir.path(), responses).await;

    let k = send_kernel(&kernel);
    tokio::time::timeout(
        Duration::from_secs(10),
        tokio::task::spawn_blocking(move || {
            let token = CancellationToken::new();
            exoskeleton_host::kernel::run_tick(&handler, &k, &token)
        }),
    )
    .await
    .unwrap()
    .unwrap();

    // All thread outputs should exist as durable artifacts in the store
    let tick_record = kernel.tick_store.latest().unwrap().unwrap();
    assert_eq!(
        tick_record.thread_contributions.len(),
        3,
        "expected 3 thread contributions in tick record"
    );

    // ThreadOutput artifacts should exist (one per thread)
    let thread_output_artifacts = kernel
        .artifact_store
        .list_by_kind(ArtifactKind::ThreadOutput, 10)
        .unwrap();
    assert!(
        thread_output_artifacts.len() >= 3,
        "expected at least 3 ThreadOutput artifacts, got {}",
        thread_output_artifacts.len()
    );

    // LlmResponse artifacts should exist (one per thread + one for Decide)
    let llm_responses = kernel
        .artifact_store
        .list_by_kind(ArtifactKind::LlmResponse, 10)
        .unwrap();
    assert!(
        llm_responses.len() >= 3,
        "expected at least 3 LlmResponse artifacts from threads"
    );

    shutdown_host(&kernel).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn convergence_all_work_on_cognitive_aq() {
    // This is verified implicitly: all thread work runs through
    // execute_thread() which calls backend.call() directly.
    // No WI Host invocations should appear.
    let dir = tempfile::tempdir().unwrap();
    let responses = vec![
        THREAT_RESPONSE.into(),
        CRITIQUE_RESPONSE.into(),
        MC_RESPONSE.into(),
        DECISION_NO_ACTIONS.into(),
    ];
    let (kernel, handler, _mock) = setup_builtin_threads(dir.path(), responses).await;

    let k = send_kernel(&kernel);
    tokio::time::timeout(
        Duration::from_secs(10),
        tokio::task::spawn_blocking(move || {
            let token = CancellationToken::new();
            exoskeleton_host::kernel::run_tick(&handler, &k, &token)
        }),
    )
    .await
    .unwrap()
    .unwrap();

    // Verify no ActionExecuted events (threads don't invoke tools)
    let events = kernel.event_ledger.recent(100).unwrap();
    let action_events: Vec<_> = events
        .iter()
        .filter(|e| e.event_type == EventType::ActionExecuted)
        .collect();
    assert!(
        action_events.is_empty(),
        "threads should not produce ActionExecuted events (IBP S3.4)"
    );

    // ThreadRan events should exist
    let thread_events: Vec<_> = events
        .iter()
        .filter(|e| e.event_type == EventType::ThreadRan)
        .collect();
    assert_eq!(thread_events.len(), 3, "3 ThreadRan events expected");

    shutdown_host(&kernel).await;
}

// ══════════════════════════════════════════════════════════════════
// T-7: Thread Replay (Acceptance Criterion C)
// ══════════════════════════════════════════════════════════════════

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn replay_tick_record_references_correct_artifacts() {
    let dir = tempfile::tempdir().unwrap();
    let responses = vec![
        THREAT_RESPONSE.into(),
        CRITIQUE_RESPONSE.into(),
        MC_RESPONSE.into(),
        DECISION_NO_ACTIONS.into(),
    ];
    let (kernel, handler, _mock) = setup_builtin_threads(dir.path(), responses).await;

    let k = send_kernel(&kernel);
    tokio::time::timeout(
        Duration::from_secs(10),
        tokio::task::spawn_blocking(move || {
            let token = CancellationToken::new();
            exoskeleton_host::kernel::run_tick(&handler, &k, &token)
        }),
    )
    .await
    .unwrap()
    .unwrap();

    let tick_record = kernel.tick_store.latest().unwrap().unwrap();

    // For each thread contribution, verify the artifact chain
    for tc in &tick_record.thread_contributions {
        // 1. Contribution has a non-empty summary
        assert!(
            !tc.summary.is_empty(),
            "contribution summary should not be empty for thread {}",
            tc.thread_id
        );

        // 2. Contribution has a valid artifact_id (content-addressed)
        assert!(
            !tc.artifact_id.to_string().is_empty(),
            "contribution artifact_id should be set for thread {}",
            tc.thread_id
        );
    }

    // 3. ThreadOutput artifacts stored durably (one per thread)
    let thread_output_artifacts = kernel
        .artifact_store
        .list_by_kind(ArtifactKind::ThreadOutput, 10)
        .unwrap();
    assert!(
        thread_output_artifacts.len() >= 3,
        "expected at least 3 ThreadOutput artifacts, got {}",
        thread_output_artifacts.len()
    );

    // LlmResponse artifacts should exist (one per thread + one for Decide)
    let llm_artifacts = kernel
        .artifact_store
        .list_by_kind(ArtifactKind::LlmResponse, 20)
        .unwrap();
    assert!(
        llm_artifacts.len() >= 4,
        "expected at least 4 LlmResponse artifacts (3 threads + 1 decide), got {}",
        llm_artifacts.len()
    );

    // Snapshot artifacts should exist
    let snap_artifacts = kernel
        .artifact_store
        .list_by_kind(ArtifactKind::Snapshot, 10)
        .unwrap();
    assert!(
        snap_artifacts.len() >= 2,
        "expected at least 2 Snapshot artifacts (before + after)"
    );

    shutdown_host(&kernel).await;
}

// ══════════════════════════════════════════════════════════════════
// T-8: Priority Ordering
// ══════════════════════════════════════════════════════════════════

#[test]
fn builtin_threads_due_in_priority_order() {
    let store = Arc::new(InMemoryThreadStore::new());
    let registry = ThreadRegistry::new(store);
    register_builtin_threads(&registry, &PromptRegistry::with_defaults()).unwrap();

    let due = registry.due_threads(1).unwrap();
    assert_eq!(due.len(), 3);
    // Critical first, then High, then Normal
    assert_eq!(due[0].name, "Threat Monitor");
    assert_eq!(due[1].name, "Self-Critique");
    assert_eq!(due[2].name, "Memory Consolidation");
}

#[test]
fn builtin_threads_priority_values() {
    let tm = builtin::threat_monitor::spec();
    let sc = builtin::self_critique::spec();
    let mc = builtin::memory_consolidation::spec();

    assert!(tm.priority > sc.priority);
    assert!(sc.priority > mc.priority);
}

// ══════════════════════════════════════════════════════════════════
// T-9: Thread Failure Isolation
// ══════════════════════════════════════════════════════════════════

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn one_thread_failure_doesnt_abort_tick() {
    let dir = tempfile::tempdir().unwrap();
    // Self-Critique (call index 1) fails, others succeed
    // Call order: Threat Monitor (0), Self-Critique (1), MC (2), Decide (3)
    let backend = Arc::new(FailOnIndexBackend::new(
        vec![1], // Self-Critique fails
        THREAT_RESPONSE.into(),
    ));
    let (kernel, handler) = setup_builtin_threads_custom(dir.path(), backend).await;

    let k = send_kernel(&kernel);
    let result = tokio::time::timeout(
        Duration::from_secs(10),
        tokio::task::spawn_blocking(move || {
            let token = CancellationToken::new();
            exoskeleton_host::kernel::run_tick(&handler, &k, &token)
        }),
    )
    .await
    .unwrap()
    .unwrap();

    // Tick should still succeed
    assert!(
        matches!(
            result,
            actionqueue_executor_local::HandlerOutput::Success { .. }
        ),
        "tick should succeed despite Self-Critique failure"
    );

    // Threat Monitor and MC should have outputs
    let tm_outputs = kernel
        .thread_registry
        .recent_outputs(THREAT_MONITOR_ID, 1)
        .unwrap();
    assert_eq!(tm_outputs.len(), 1, "Threat Monitor should have output");

    // Self-Critique should have no output
    let sc_outputs = kernel
        .thread_registry
        .recent_outputs(SELF_CRITIQUE_ID, 1)
        .unwrap();
    assert!(
        sc_outputs.is_empty(),
        "Self-Critique should have no output (it failed)"
    );

    // Error event should be logged for Self-Critique
    let events = kernel.event_ledger.recent(100).unwrap();
    let error_events: Vec<_> = events
        .iter()
        .filter(|e| e.event_type == EventType::Error)
        .collect();
    assert!(
        error_events
            .iter()
            .any(|e| e.summary.contains("Self-Critique")),
        "should have error event mentioning Self-Critique"
    );

    shutdown_host(&kernel).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn all_thread_failures_still_completes_tick() {
    let dir = tempfile::tempdir().unwrap();
    // All 3 threads fail (indices 0, 1, 2), Decide succeeds (index 3)
    let backend = Arc::new(FailOnIndexBackend::new(
        vec![0, 1, 2],
        DECISION_NO_ACTIONS.into(),
    ));
    let (kernel, handler) = setup_builtin_threads_custom(dir.path(), backend).await;

    let k = send_kernel(&kernel);
    let result = tokio::time::timeout(
        Duration::from_secs(10),
        tokio::task::spawn_blocking(move || {
            let token = CancellationToken::new();
            exoskeleton_host::kernel::run_tick(&handler, &k, &token)
        }),
    )
    .await
    .unwrap()
    .unwrap();

    assert!(
        matches!(
            result,
            actionqueue_executor_local::HandlerOutput::Success { .. }
        ),
        "tick should succeed even when all threads fail"
    );

    // Verify snapshot still has 3 thread summaries (from registry, not from outputs)
    let snapshot = kernel.snapshot_store.latest().unwrap().unwrap();
    assert_eq!(snapshot.thread_summaries.len(), 3);

    shutdown_host(&kernel).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn thread_error_events_logged() {
    let dir = tempfile::tempdir().unwrap();
    // MC (call index 2) fails
    let backend = Arc::new(FailOnIndexBackend::new(vec![2], THREAT_RESPONSE.into()));
    let (kernel, handler) = setup_builtin_threads_custom(dir.path(), backend).await;

    let k = send_kernel(&kernel);
    tokio::time::timeout(
        Duration::from_secs(10),
        tokio::task::spawn_blocking(move || {
            let token = CancellationToken::new();
            exoskeleton_host::kernel::run_tick(&handler, &k, &token)
        }),
    )
    .await
    .unwrap()
    .unwrap();

    let events = kernel.event_ledger.recent(100).unwrap();
    let error_events: Vec<_> = events
        .iter()
        .filter(|e| e.event_type == EventType::Error)
        .collect();
    assert!(
        error_events
            .iter()
            .any(|e| e.summary.contains("Memory Consolidation")),
        "should log error for Memory Consolidation failure"
    );

    // ThreadRan events for the successful threads
    let thread_ran: Vec<_> = events
        .iter()
        .filter(|e| e.event_type == EventType::ThreadRan)
        .collect();
    assert_eq!(
        thread_ran.len(),
        2,
        "2 successful threads should have ThreadRan events"
    );

    shutdown_host(&kernel).await;
}

// ── T-10: Property-Based Tests (Sprint 7) ──

mod proptest_tests {
    use exoskeleton_threads::builtin::{memory_consolidation, self_critique, threat_monitor};
    use proptest::prelude::*;

    proptest! {
        /// Threat Monitor parse_output never panics on arbitrary input.
        #[test]
        fn threat_monitor_parse_never_panics(ref s in "\\PC{0,500}") {
            let result = threat_monitor::parse_output(s);
            // Fallback: severity should be None for non-JSON input
            prop_assert_eq!(result.severity, threat_monitor::ThreatSeverity::None);
        }

        /// Self-Critique parse_output never panics on arbitrary input.
        #[test]
        fn self_critique_parse_never_panics(ref s in "\\PC{0,500}") {
            let result = self_critique::parse_output(s);
            // Fallback: progress_rating should be 0.5
            prop_assert!((result.progress_rating - 0.5).abs() < f64::EPSILON);
        }

        /// Memory Consolidation parse_output never panics on arbitrary input.
        #[test]
        fn memory_consolidation_parse_never_panics(ref s in "\\PC{0,500}") {
            let result = memory_consolidation::parse_output(s);
            // Fallback: empty episodic_summary
            prop_assert!(result.episodic_summary.is_empty());
        }
    }
}

//! Direct LLM calls without AQ infrastructure.
//!
//! Used for bootstrap and other pre-vessel scenarios where the full
//! Cognitive AQ engine isn't running. Builds the appropriate HTTP backend
//! from config and makes a single synchronous call on a blocking thread.

use std::sync::Arc;
use std::time::Duration;

use actionqueue_executor_local::CancellationToken;
use exoskeleton_core::llm::{LlmBackend, LlmRequest, LlmResponse};
use exoskeleton_core::ExoError;

use super::http::{AnthropicBackend, LlmHttpBackend, OllamaNativeBackend, OpenAiCompatBackend};
use crate::config::{FrontierProvider, LlmConfig, LocalApiFormat};

/// Make a direct LLM call without AQ infrastructure.
///
/// Builds the appropriate HTTP backend from config, then executes the call
/// on a blocking thread (the backends use `Handle::current().block_on()`
/// internally for async HTTP).
///
/// This is the entry point for bootstrap conversations and connectivity
/// verification — anywhere you need an LLM response before the Vessel
/// is fully booted.
pub async fn direct_llm_call(
    llm_config: &LlmConfig,
    request: LlmRequest,
) -> Result<LlmResponse, ExoError> {
    let backend = build_default_backend(llm_config)?;
    let http_client = reqwest::Client::builder()
        .timeout(Duration::from_secs(llm_config.timeout_secs))
        .build()
        .map_err(|e| ExoError::Config(format!("HTTP client build failed: {e}")))?;
    let token = CancellationToken::new();

    tokio::task::spawn_blocking(move || backend.call(&http_client, &request, &token))
        .await
        .map_err(|e| ExoError::LlmInvocation(format!("blocking task join error: {e}")))?
}

/// Build the default backend based on `LlmConfig.default_backend`.
fn build_default_backend(config: &LlmConfig) -> Result<Arc<dyn LlmHttpBackend>, ExoError> {
    match config.default_backend {
        LlmBackend::Local => build_local_backend(config)?
            .ok_or_else(|| ExoError::Config("default_backend is Local but no local config".into())),
        LlmBackend::Frontier => build_frontier_backend(config)?.ok_or_else(|| {
            ExoError::Config("default_backend is Frontier but no frontier config".into())
        }),
    }
}

fn build_local_backend(config: &LlmConfig) -> Result<Option<Arc<dyn LlmHttpBackend>>, ExoError> {
    let local_config = match config.local.as_ref() {
        Some(c) => c,
        None => return Ok(None),
    };
    let backend: Arc<dyn LlmHttpBackend> = match local_config.api_format {
        LocalApiFormat::OpenAICompat => Arc::new(OpenAiCompatBackend::new(
            local_config.endpoint.clone(),
            local_config.model.clone(),
            None,
        )),
        LocalApiFormat::Ollama => Arc::new(OllamaNativeBackend::new(
            local_config.endpoint.clone(),
            local_config.model.clone(),
        )),
    };
    Ok(Some(backend))
}

fn build_frontier_backend(config: &LlmConfig) -> Result<Option<Arc<dyn LlmHttpBackend>>, ExoError> {
    let frontier_config = match config.frontier.as_ref() {
        Some(c) => c,
        None => return Ok(None),
    };

    let api_key = std::env::var(&frontier_config.api_key_env).map_err(|_| {
        ExoError::Config(format!(
            "API key env var '{}' not set (required for frontier backend)",
            frontier_config.api_key_env
        ))
    })?;

    let base_url =
        frontier_config
            .endpoint
            .clone()
            .unwrap_or_else(|| match frontier_config.provider {
                FrontierProvider::Anthropic => "https://api.anthropic.com".into(),
                FrontierProvider::OpenAI => "https://api.openai.com".into(),
                FrontierProvider::Gemini => {
                    "https://generativelanguage.googleapis.com/v1beta/openai".into()
                }
                FrontierProvider::Grok => "https://api.x.ai".into(),
                FrontierProvider::OpenRouter => "https://openrouter.ai/api".into(),
                FrontierProvider::DeepSeek => "https://api.deepseek.com".into(),
            });

    let backend: Arc<dyn LlmHttpBackend> = match frontier_config.provider {
        FrontierProvider::Anthropic => Arc::new(AnthropicBackend::new(
            base_url,
            frontier_config.model.clone(),
            api_key,
        )),
        // All other providers use OpenAI-compatible Chat Completions API
        FrontierProvider::OpenAI
        | FrontierProvider::Gemini
        | FrontierProvider::Grok
        | FrontierProvider::OpenRouter
        | FrontierProvider::DeepSeek => Arc::new(OpenAiCompatBackend::new(
            base_url,
            frontier_config.model.clone(),
            Some(api_key),
        )),
    };
    Ok(Some(backend))
}

// ── Handler-context unified LLM call (E8-S1) ──

use exoskeleton_core::tick::LlmCallRecord;
use exoskeleton_core::{Artifact, ArtifactId, ArtifactKind};

use crate::cognitive_engine::CognitiveHandler;
use crate::kernel::KernelContext;

/// Result of a handler-context direct LLM call, including the response and its artifact ID.
#[derive(Debug)]
pub struct DirectLlmResult {
    pub response: LlmResponse,
    pub artifact_id: ArtifactId,
    pub llm_call_record: LlmCallRecord,
}

/// Unified LLM call for handler-context code (H-1 pattern).
///
/// Resolves the backend, makes the HTTP call, records latency, records
/// budget consumption, and stores the response as a content-addressed
/// artifact (I3). Used by Decide, Reflect, Threads, and the inner loop.
///
/// This function exists because handler code runs synchronously inside
/// the Cognitive AQ dispatch loop and cannot use LlmClient (which calls
/// run_until_idle(), causing deadlock). See cognitive_engine.rs IBP §3.1.
///
/// When ActionQueue gains async handler support, this function should be
/// replaced by LlmClient::call(). The interface is designed to make that
/// migration mechanical.
pub fn handler_direct_llm_call(
    handler: &CognitiveHandler,
    kernel: &KernelContext,
    request: &LlmRequest,
    cancellation: &CancellationToken,
) -> Result<DirectLlmResult, ExoError> {
    // 1. Resolve backend (with escalation fallback)
    let (backend, backend_type) = resolve_handler_backend(handler, request)?;

    // 2. Direct HTTP call
    let start = std::time::Instant::now();
    let mut response = backend.call(&handler.http_client, request, cancellation)?;
    let latency_ms = start.elapsed().as_millis() as u64;
    response.latency_ms = latency_ms;

    // 3. Record budget consumption
    if let Some(ref tracker) = kernel.budget_tracker {
        if let Ok(mut guard) = tracker.lock() {
            guard.record_llm_call(
                backend_type,
                response.tokens_in,
                response.tokens_out,
                response.cost_estimate_cents.unwrap_or(0.0),
            );
        }
    }

    // 4. Record metrics (Sprint 10)
    if let Some(ref m) = kernel.metrics {
        let backend_label = match backend_type {
            LlmBackend::Local => "local",
            LlmBackend::Frontier => "frontier",
        };
        m.llm_calls_total.with_label_values(&[backend_label]).inc();
        m.llm_tokens_total
            .with_label_values(&[backend_label, "input"])
            .inc_by(response.tokens_in);
        m.llm_tokens_total
            .with_label_values(&[backend_label, "output"])
            .inc_by(response.tokens_out);
        m.llm_cost_cents_total
            .with_label_values(&[backend_label])
            .inc_by(response.cost_estimate_cents.unwrap_or(0.0));
        m.llm_latency_seconds
            .with_label_values(&[backend_label])
            .observe(latency_ms as f64 / 1000.0);
    }

    // 5. Store response as artifact (I3)
    let artifact = Artifact::from_json(ArtifactKind::LlmResponse, &response)?;
    let artifact_id = handler.artifact_store.put(&artifact)?;

    // 6. Build LLM call record
    let llm_call_record = LlmCallRecord {
        model: response.model.clone(),
        tokens_in: response.tokens_in,
        tokens_out: response.tokens_out,
        cost_cents: response.cost_estimate_cents.unwrap_or(0.0),
        latency_ms,
        response_artifact_ref: Some(artifact_id.clone()),
        turns: 1,
    };

    Ok(DirectLlmResult {
        response,
        artifact_id,
        llm_call_record,
    })
}

/// Resolve the LLM backend for handler-context code.
///
/// If the request specifies a backend, use it. Otherwise use the handler's
/// default, falling back to the other backend if the default is unavailable.
fn resolve_handler_backend(
    handler: &CognitiveHandler,
    request: &LlmRequest,
) -> Result<(Arc<dyn LlmHttpBackend>, LlmBackend), ExoError> {
    let preferred = request.backend.unwrap_or(handler.default_backend);

    let backend = match preferred {
        LlmBackend::Local => handler.local_backend.as_ref(),
        LlmBackend::Frontier => handler.frontier_backend.as_ref(),
    };

    if let Some(b) = backend {
        return Ok((Arc::clone(b), preferred));
    }

    // Fallback to the other backend
    let fallback_type = match preferred {
        LlmBackend::Local => LlmBackend::Frontier,
        LlmBackend::Frontier => LlmBackend::Local,
    };
    let fallback = match fallback_type {
        LlmBackend::Local => handler.local_backend.as_ref(),
        LlmBackend::Frontier => handler.frontier_backend.as_ref(),
    };

    match fallback {
        Some(b) => Ok((Arc::clone(b), fallback_type)),
        None => Err(ExoError::LlmInvocation("no LLM backend configured".into())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_default_backend_rejects_missing_local() {
        let config = LlmConfig {
            local: None,
            frontier: None,
            default_backend: LlmBackend::Local,
            ..Default::default()
        };
        assert!(build_default_backend(&config).is_err());
    }

    #[test]
    fn build_default_backend_rejects_missing_frontier() {
        let config = LlmConfig {
            local: None,
            frontier: None,
            default_backend: LlmBackend::Frontier,
            ..Default::default()
        };
        assert!(build_default_backend(&config).is_err());
    }

    #[test]
    fn build_local_backend_openai_compat() {
        let config = LlmConfig {
            local: Some(crate::config::LocalModelConfig {
                endpoint: "http://localhost:11434".into(),
                model: "llama3.2:latest".into(),
                api_format: LocalApiFormat::OpenAICompat,
            }),
            ..Default::default()
        };
        assert!(build_local_backend(&config).unwrap().is_some());
    }

    #[test]
    fn build_local_backend_ollama() {
        let config = LlmConfig {
            local: Some(crate::config::LocalModelConfig {
                endpoint: "http://localhost:11434".into(),
                model: "llama3.2:latest".into(),
                api_format: LocalApiFormat::Ollama,
            }),
            ..Default::default()
        };
        assert!(build_local_backend(&config).unwrap().is_some());
    }

    #[test]
    fn build_local_backend_none_when_not_configured() {
        let config = LlmConfig::default();
        assert!(build_local_backend(&config).unwrap().is_none());
    }

    // ── handler_direct_llm_call tests (E8S1-T1 through T6) ──

    use exoskeleton_core::conversation::InMemoryConversationStore;
    use exoskeleton_core::llm::{LlmMessage, LlmRole, StopReason};
    use exoskeleton_core::prompt::PromptRegistry;
    use exoskeleton_core::{ArtifactStore, LiveEvent, VesselId};
    use exoskeleton_memory::{ApproximateTokenCounter, ContextCompiler};
    use exoskeleton_relationship::InMemoryRelationshipLedger;
    use exoskeleton_threads::{InMemoryThreadStore, ThreadRegistry};

    use crate::inbox::InMemoryInbox;
    use crate::kernel::KernelContext;
    use crate::llm::mock::MockLlmBackend;
    use crate::storage::StorageManager;

    fn mock_response() -> LlmResponse {
        LlmResponse {
            content: "test response".into(),
            model: "mock-model".into(),
            tokens_in: 100,
            tokens_out: 50,
            latency_ms: 0, // will be overwritten
            stop_reason: StopReason::EndTurn,
            cost_estimate_cents: Some(0.5),
            backend: LlmBackend::Local,
        }
    }

    fn test_request() -> LlmRequest {
        LlmRequest {
            backend: None,
            system_prompt: Some("system".into()),
            messages: vec![LlmMessage {
                role: LlmRole::User,
                content: "test".into(),
            }],
            max_output_tokens: 256,
            temperature: Some(0.0),
            stop_sequences: vec![],
        }
    }

    fn setup_handler_and_kernel(
        dir: &std::path::Path,
        mock: Arc<MockLlmBackend>,
        use_local: bool,
    ) -> (CognitiveHandler, KernelContext) {
        std::fs::create_dir_all(dir.join("exo")).unwrap();
        let storage = StorageManager::open(dir).unwrap();
        let artifact_store: Arc<dyn ArtifactStore> = storage.artifact_store().clone();

        let (local, frontier) = if use_local {
            (Some(mock as Arc<dyn LlmHttpBackend>), None)
        } else {
            (None, Some(mock as Arc<dyn LlmHttpBackend>))
        };

        let handler = CognitiveHandler::with_backends(
            local,
            frontier,
            if use_local {
                LlmBackend::Local
            } else {
                LlmBackend::Frontier
            },
            artifact_store.clone(),
        );

        let counter = Arc::new(ApproximateTokenCounter);
        let compiler = ContextCompiler::with_defaults(counter, 4000);

        let kernel = KernelContext {
            snapshot_store: storage.snapshot_store().clone(),
            event_ledger: storage.event_ledger().clone(),
            tick_store: storage.tick_store().clone(),
            memory_store: storage.memory_store().clone(),
            context_compiler: Arc::new(compiler),
            artifact_store,
            wi_host_slot: Arc::new(tokio::sync::Mutex::new(None)),
            inbox: Arc::new(InMemoryInbox::new()),
            vessel_id: VesselId::new(),
            mission: "test".into(),
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
            read_paths_this_tick: Arc::new(std::sync::Mutex::new(std::collections::HashSet::new())),
            inner_loop_config: crate::config::InnerLoopConfig::default(),
            tool_policy: crate::kernel::policy::ToolPolicyConfig::default(),
            session_approvals: crate::kernel::policy::SessionApprovals::new(),
            vessel_mode: Arc::new(std::sync::Mutex::new(exoskeleton_core::VesselMode::Normal)),
            wake_signal: None,
        };

        (handler, kernel)
    }

    // ── E8S1-T1: direct_llm_call_records_budget ──

    #[test]
    fn direct_llm_call_records_budget() {
        let dir = tempfile::tempdir().unwrap();
        let mock = Arc::new(MockLlmBackend::new(mock_response()));
        let (handler, mut kernel) = setup_handler_and_kernel(dir.path(), mock, true);

        // Add a budget tracker using the correct types
        let budget_config = exoskeleton_core::CognitiveBudgetConfig::default();
        let budget_store: Arc<dyn exoskeleton_core::BudgetStore> =
            Arc::new(crate::budget::tracker::InMemoryBudgetStore::new());
        let tracker = crate::budget::CognitiveBudgetTracker::new(budget_config, budget_store);
        kernel.budget_tracker = Some(Arc::new(std::sync::Mutex::new(tracker)));

        let token = CancellationToken::new();
        let result = handler_direct_llm_call(&handler, &kernel, &test_request(), &token).unwrap();

        // Verify tokens recorded
        assert_eq!(result.llm_call_record.tokens_in, 100);
        assert_eq!(result.llm_call_record.tokens_out, 50);

        // Verify budget tracker was updated
        let guard = kernel.budget_tracker.as_ref().unwrap().lock().unwrap();
        let consumption = guard.build_consumption();
        assert!(
            !consumption.is_empty(),
            "budget tracker should have recorded the call"
        );
    }

    // ── E8S1-T2: direct_llm_call_stores_artifact ──

    #[test]
    fn direct_llm_call_stores_artifact() {
        let dir = tempfile::tempdir().unwrap();
        let mock = Arc::new(MockLlmBackend::new(mock_response()));
        let (handler, kernel) = setup_handler_and_kernel(dir.path(), mock, true);

        let token = CancellationToken::new();
        let result = handler_direct_llm_call(&handler, &kernel, &test_request(), &token).unwrap();

        // Verify artifact is stored and retrievable
        let artifact = kernel
            .artifact_store
            .get(&result.artifact_id)
            .unwrap()
            .unwrap();
        assert_eq!(artifact.kind, ArtifactKind::LlmResponse);

        // Verify the call record points to same artifact
        assert_eq!(
            result.llm_call_record.response_artifact_ref,
            Some(result.artifact_id)
        );
    }

    // ── E8S1-T3: direct_llm_call_resolves_local_backend ──

    #[test]
    fn direct_llm_call_resolves_local_backend() {
        let dir = tempfile::tempdir().unwrap();
        let mock = Arc::new(MockLlmBackend::new(mock_response()));
        let mock_clone = mock.clone();
        let (handler, kernel) = setup_handler_and_kernel(dir.path(), mock, true);

        let token = CancellationToken::new();
        let _result = handler_direct_llm_call(&handler, &kernel, &test_request(), &token).unwrap();

        // Verify the mock was called
        assert_eq!(mock_clone.call_count(), 1);
    }

    // ── E8S1-T4: direct_llm_call_escalates_to_frontier ──

    #[test]
    fn direct_llm_call_escalates_to_frontier() {
        let dir = tempfile::tempdir().unwrap();
        let mock = Arc::new(MockLlmBackend::new(mock_response()));
        let mock_clone = mock.clone();

        std::fs::create_dir_all(dir.path().join("exo")).unwrap();
        let storage = StorageManager::open(dir.path()).unwrap();
        let artifact_store: Arc<dyn ArtifactStore> = storage.artifact_store().clone();

        // Local backend is None, frontier is the mock — should escalate
        let handler = CognitiveHandler::with_backends(
            None,
            Some(mock_clone.clone() as Arc<dyn LlmHttpBackend>),
            LlmBackend::Local, // default is Local, but it's unavailable
            artifact_store.clone(),
        );

        let counter = Arc::new(ApproximateTokenCounter);
        let compiler = ContextCompiler::with_defaults(counter, 4000);

        let kernel = KernelContext {
            snapshot_store: storage.snapshot_store().clone(),
            event_ledger: storage.event_ledger().clone(),
            tick_store: storage.tick_store().clone(),
            memory_store: storage.memory_store().clone(),
            context_compiler: Arc::new(compiler),
            artifact_store,
            wi_host_slot: Arc::new(tokio::sync::Mutex::new(None)),
            inbox: Arc::new(InMemoryInbox::new()),
            vessel_id: VesselId::new(),
            mission: "test".into(),
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
            read_paths_this_tick: Arc::new(std::sync::Mutex::new(std::collections::HashSet::new())),
            inner_loop_config: crate::config::InnerLoopConfig::default(),
            tool_policy: crate::kernel::policy::ToolPolicyConfig::default(),
            session_approvals: crate::kernel::policy::SessionApprovals::new(),
            vessel_mode: Arc::new(std::sync::Mutex::new(exoskeleton_core::VesselMode::Normal)),
            wake_signal: None,
        };

        let token = CancellationToken::new();
        let result = handler_direct_llm_call(&handler, &kernel, &test_request(), &token).unwrap();

        // Verify the frontier mock was called (escalation worked)
        assert_eq!(mock_clone.call_count(), 1);
        assert_eq!(result.response.content, "test response");
    }

    // ── E8S1-T5: direct_llm_call_no_backend_errors ──

    #[test]
    fn direct_llm_call_no_backend_errors() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("exo")).unwrap();
        let storage = StorageManager::open(dir.path()).unwrap();
        let artifact_store: Arc<dyn ArtifactStore> = storage.artifact_store().clone();

        // No backends configured
        let handler =
            CognitiveHandler::with_backends(None, None, LlmBackend::Local, artifact_store.clone());

        let counter = Arc::new(ApproximateTokenCounter);
        let compiler = ContextCompiler::with_defaults(counter, 4000);

        let kernel = KernelContext {
            snapshot_store: storage.snapshot_store().clone(),
            event_ledger: storage.event_ledger().clone(),
            tick_store: storage.tick_store().clone(),
            memory_store: storage.memory_store().clone(),
            context_compiler: Arc::new(compiler),
            artifact_store,
            wi_host_slot: Arc::new(tokio::sync::Mutex::new(None)),
            inbox: Arc::new(InMemoryInbox::new()),
            vessel_id: VesselId::new(),
            mission: "test".into(),
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
            read_paths_this_tick: Arc::new(std::sync::Mutex::new(std::collections::HashSet::new())),
            inner_loop_config: crate::config::InnerLoopConfig::default(),
            tool_policy: crate::kernel::policy::ToolPolicyConfig::default(),
            session_approvals: crate::kernel::policy::SessionApprovals::new(),
            vessel_mode: Arc::new(std::sync::Mutex::new(exoskeleton_core::VesselMode::Normal)),
            wake_signal: None,
        };

        let token = CancellationToken::new();
        let result = handler_direct_llm_call(&handler, &kernel, &test_request(), &token);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            matches!(err, ExoError::LlmInvocation(_)),
            "expected LlmInvocation error, got: {err:?}"
        );
    }

    // ── E8S1-T6: direct_llm_call_measures_latency ──

    #[test]
    fn direct_llm_call_measures_latency() {
        let dir = tempfile::tempdir().unwrap();
        let mock = Arc::new(MockLlmBackend::new(mock_response()));
        let (handler, kernel) = setup_handler_and_kernel(dir.path(), mock, true);

        let token = CancellationToken::new();
        let result = handler_direct_llm_call(&handler, &kernel, &test_request(), &token).unwrap();

        // The latency_ms should be set by the handler (overwriting the mock's 0)
        // Since the mock returns nearly instantly, latency should be >= 0
        assert!(
            result.llm_call_record.latency_ms < 1000,
            "latency should be reasonable: {}ms",
            result.llm_call_record.latency_ms
        );
        // Also check the response latency was set
        assert_eq!(
            result.response.latency_ms,
            result.llm_call_record.latency_ms
        );
    }
}

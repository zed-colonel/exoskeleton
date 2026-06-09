//! Cognitive AQ engine: handler, task types, and bootstrap.
//!
//! The Cognitive AQ handles exactly two kinds of work:
//! - Master loop ticks (PODAARA cycles)
//! - LLM inference calls
//!
//! Tool invocations are NEVER submitted to this engine (I9). They go through
//! the WI Host's Tool AQ via the Act step boundary.

use std::sync::Arc;
use std::time::Duration;

use actionqueue_core::time::clock::SystemClock;
use actionqueue_executor_local::{ExecutorContext, ExecutorHandler, HandlerOutput};
use actionqueue_runtime::config::RuntimeConfig;
use actionqueue_runtime::engine::{ActionQueueEngine, BootstrappedEngine};
use exoskeleton_core::llm::{LlmBackend, LlmRequest};
use exoskeleton_core::{Artifact, ArtifactKind, ArtifactStore, ExoError};
use serde::{Deserialize, Serialize};

use crate::config::{FrontierProvider, LlmConfig, LocalApiFormat, VesselConfig};
use crate::llm::http::{
    classify_llm_error, AnthropicBackend, LlmHttpBackend, OllamaNativeBackend, OpenAiCompatBackend,
};

/// Discriminator for Cognitive AQ task routing.
///
/// Every task submitted to the Cognitive AQ carries a `CognitiveTaskType` in its
/// JSON payload. The `CognitiveHandler` routes to the appropriate handler based
/// on this field.
///
/// Only two task types are permitted on the Cognitive AQ:
/// - Master loop ticks (PODAARA cycles)
/// - LLM inference calls
///
/// Tool invocations are NEVER submitted to the Cognitive AQ (I9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CognitiveTaskType {
    /// A master loop tick (one PODAARA cycle). Sprint 5.
    MasterLoop,
    /// An LLM inference call (local or frontier). Sprint 4.
    LlmCall,
}

/// JSON payload for tasks on the Cognitive AQ.
///
/// The `task_type` field is used for routing. The `data` field carries
/// type-specific parameters (different structure per task type).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CognitivePayload {
    /// Which kind of cognitive work this task represents.
    pub task_type: CognitiveTaskType,
    /// Type-specific payload data. Structure varies by `task_type`:
    /// - `MasterLoop`: tick parameters
    /// - `LlmCall`: LlmRequest
    #[serde(default)]
    pub data: serde_json::Value,
}

/// Type alias for the bootstrapped Cognitive AQ engine.
pub type CognitiveEngine = BootstrappedEngine<CognitiveHandler, SystemClock>;

/// ExecutorHandler for the Cognitive AQ engine.
///
/// Routes tasks based on `CognitiveTaskType` in the JSON payload.
/// - `LlmCall` handler (makes HTTP calls to LLM APIs)
/// - `MasterLoop` handler
///
/// This handler runs exclusively on the Cognitive AQ. It NEVER executes tool
/// invocations (I9). Thread execution is orchestrated inside the master loop,
/// not dispatched as a separate AQ task.
pub struct CognitiveHandler {
    /// Shared HTTP client for LLM API calls (connection pooling).
    pub(crate) http_client: reqwest::Client,
    /// LLM backend for local model calls. `None` if not configured.
    pub(crate) local_backend: Option<Arc<dyn LlmHttpBackend>>,
    /// LLM backend for frontier model calls. `None` if not configured.
    pub(crate) frontier_backend: Option<Arc<dyn LlmHttpBackend>>,
    /// Which backend to use by default.
    pub(crate) default_backend: LlmBackend,
    /// Artifact store for persisting LLM responses (I3).
    pub(crate) artifact_store: Arc<dyn ArtifactStore>,
    /// Master loop kernel context. `None` until configured via `with_kernel()`.
    pub(crate) kernel: Option<Arc<super::kernel::KernelContext>>,
    /// Request timeout.
    #[allow(dead_code)]
    timeout: Duration,
}

impl std::fmt::Debug for CognitiveHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CognitiveHandler")
            .field("default_backend", &self.default_backend)
            .field("has_local", &self.local_backend.is_some())
            .field("has_frontier", &self.frontier_backend.is_some())
            .field("has_kernel", &self.kernel.is_some())
            .finish()
    }
}

impl CognitiveHandler {
    /// Create a new CognitiveHandler with LLM configuration and artifact storage.
    ///
    /// Reads API keys from environment variables (I4: never stored in config).
    /// If the env var for a frontier backend is not set, the frontend is marked
    /// as unavailable (calls will return TerminalFailure).
    pub fn new(
        llm_config: &LlmConfig,
        artifact_store: Arc<dyn ArtifactStore>,
    ) -> Result<Self, ExoError> {
        let http_client = reqwest::Client::builder()
            .timeout(Duration::from_secs(llm_config.timeout_secs))
            .build()
            .map_err(|e| ExoError::Config(format!("HTTP client build failed: {e}")))?;

        let local_backend = Self::build_local_backend(llm_config);
        let frontier_backend = Self::build_frontier_backend(llm_config)?;

        Ok(Self {
            http_client,
            local_backend,
            frontier_backend,
            default_backend: llm_config.default_backend,
            artifact_store,
            kernel: None,
            timeout: Duration::from_secs(llm_config.timeout_secs),
        })
    }

    /// Create a CognitiveHandler with custom backends (for testing).
    pub fn with_backends(
        local_backend: Option<Arc<dyn LlmHttpBackend>>,
        frontier_backend: Option<Arc<dyn LlmHttpBackend>>,
        default_backend: LlmBackend,
        artifact_store: Arc<dyn ArtifactStore>,
    ) -> Self {
        Self {
            http_client: reqwest::Client::new(),
            local_backend,
            frontier_backend,
            default_backend,
            artifact_store,
            kernel: None,
            timeout: Duration::from_secs(120),
        }
    }

    /// Set the master loop kernel context (builder pattern).
    ///
    /// After calling this, `MasterLoop` tasks will be handled by the PODAARA
    /// kernel instead of returning a terminal failure.
    pub fn with_kernel(mut self, kernel: Arc<super::kernel::KernelContext>) -> Self {
        self.kernel = Some(kernel);
        self
    }

    fn build_local_backend(config: &LlmConfig) -> Option<Arc<dyn LlmHttpBackend>> {
        let local_config = config.local.as_ref()?;
        let backend: Arc<dyn LlmHttpBackend> = match local_config.api_format {
            LocalApiFormat::OpenAICompat => Arc::new(OpenAiCompatBackend::new(
                local_config.endpoint.clone(),
                local_config.model.clone(),
                None, // Local models don't need API keys
            )),
            LocalApiFormat::Ollama => Arc::new(OllamaNativeBackend::new(
                local_config.endpoint.clone(),
                local_config.model.clone(),
            )),
        };
        Some(backend)
    }

    fn build_frontier_backend(
        config: &LlmConfig,
    ) -> Result<Option<Arc<dyn LlmHttpBackend>>, ExoError> {
        let frontier_config = match config.frontier.as_ref() {
            Some(c) => c,
            None => return Ok(None),
        };

        // Read API key from environment variable (I4: never stored in config)
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

    /// Handle an LlmCall task.
    fn handle_llm_call(&self, ctx: &ExecutorContext, payload: &CognitivePayload) -> HandlerOutput {
        // 1. Deserialize LlmRequest from payload.data
        let request: LlmRequest = match serde_json::from_value(payload.data.clone()) {
            Ok(r) => r,
            Err(e) => return HandlerOutput::terminal_failure(format!("invalid LlmRequest: {e}")),
        };

        // 2. Check cancellation before starting HTTP call
        let cancellation = ctx.input.cancellation_context.token();
        if cancellation.is_cancelled() {
            return HandlerOutput::retryable_failure("cancelled before LLM call");
        }

        // 3. Use unified handler_direct_llm_call when kernel is available (E8-S1)
        if let Some(ref kernel) = self.kernel {
            match crate::llm::direct::handler_direct_llm_call(self, kernel, &request, cancellation)
            {
                Ok(result) => match serde_json::to_vec(&result.response) {
                    Ok(bytes) => HandlerOutput::success_with_output(bytes),
                    Err(e) => HandlerOutput::terminal_failure(format!(
                        "failed to serialize LLM response: {e}"
                    )),
                },
                Err(e) => classify_llm_error(e),
            }
        } else {
            // Fallback for pre-boot calls (no kernel context yet)
            let backend_type = request.backend.unwrap_or(self.default_backend);
            let backend = match backend_type {
                LlmBackend::Local => self.local_backend.as_ref(),
                LlmBackend::Frontier => self.frontier_backend.as_ref(),
            };
            let backend = match backend {
                Some(b) => b,
                None => {
                    return HandlerOutput::terminal_failure(format!(
                        "{backend_type:?} backend not configured"
                    ))
                }
            };

            let start = std::time::Instant::now();
            let response = backend.call(&self.http_client, &request, cancellation);
            let latency_ms = start.elapsed().as_millis() as u64;

            match response {
                Ok(mut llm_response) => {
                    llm_response.latency_ms = latency_ms;

                    let artifact =
                        match Artifact::from_json(ArtifactKind::LlmResponse, &llm_response) {
                            Ok(a) => a,
                            Err(e) => {
                                return HandlerOutput::terminal_failure(format!(
                                    "failed to serialize LLM response artifact: {e}"
                                ))
                            }
                        };

                    if let Err(e) = self.artifact_store.put(&artifact) {
                        tracing::warn!(
                            error = %e,
                            "failed to store LLM response artifact (I3 violation)"
                        );
                    }

                    match serde_json::to_vec(&llm_response) {
                        Ok(bytes) => HandlerOutput::success_with_output(bytes),
                        Err(e) => HandlerOutput::terminal_failure(format!(
                            "failed to serialize LLM response: {e}"
                        )),
                    }
                }
                Err(e) => classify_llm_error(e),
            }
        }
    }
}

impl ExecutorHandler for CognitiveHandler {
    fn execute(&self, ctx: ExecutorContext) -> HandlerOutput {
        // Parse payload to extract task type
        let payload: CognitivePayload = match serde_json::from_slice(&ctx.input.payload) {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(
                    run_id = %ctx.input.run_id,
                    error = %e,
                    "invalid cognitive payload"
                );
                return HandlerOutput::terminal_failure(format!("invalid cognitive payload: {e}"));
            }
        };

        tracing::debug!(
            run_id = %ctx.input.run_id,
            task_type = ?payload.task_type,
            "routing cognitive task"
        );

        // Route based on task type
        match payload.task_type {
            CognitiveTaskType::MasterLoop => match &self.kernel {
                Some(kernel) => {
                    super::kernel::run_tick(self, kernel, ctx.input.cancellation_context.token())
                }
                None => HandlerOutput::terminal_failure("master loop kernel not configured"),
            },
            CognitiveTaskType::LlmCall => self.handle_llm_call(&ctx, &payload),
        }
    }
}

/// Bootstrap the Cognitive AQ engine from VesselConfig.
///
/// Creates a `RuntimeConfig` mapped from the cognitive-specific settings in
/// `VesselConfig`, then bootstraps the engine with a `CognitiveHandler`.
///
/// The engine's data directory is `{config.data_dir}/cognitive-aq/`. WAL files
/// appear at `{config.data_dir}/cognitive-aq/wal/actionqueue.wal`.
///
/// On restart, the engine recovers from its WAL — this is independent of the
/// Tool AQ's recovery (I9, IBP §3.6).
///
/// If `kernel` is provided, the handler will delegate `MasterLoop` tasks to the
/// PODAARA kernel. If `None`, `MasterLoop` tasks will return terminal failure.
pub fn bootstrap_cognitive_engine(
    config: &VesselConfig,
    artifact_store: Arc<dyn ArtifactStore>,
    kernel: Option<Arc<super::kernel::KernelContext>>,
) -> Result<CognitiveEngine, ExoError> {
    let runtime_config = RuntimeConfig {
        data_dir: config.data_dir.join("cognitive-aq"),
        tick_interval: config.cognitive_tick_interval,
        dispatch_concurrency: config.cognitive_dispatch_concurrency,
        lease_timeout_secs: config.cognitive_lease_timeout_secs,
        ..Default::default()
    };

    let mut handler = CognitiveHandler::new(&config.llm_config, artifact_store)?;
    if let Some(k) = kernel {
        handler.kernel = Some(k);
    }
    let engine = ActionQueueEngine::new(runtime_config, handler);

    engine
        .bootstrap()
        .map_err(|e| ExoError::Engine(format!("cognitive AQ bootstrap failed: {e}")))
}

/// Bootstrap the Cognitive AQ engine with custom backends (for testing).
pub fn bootstrap_cognitive_engine_with_backends(
    config: &VesselConfig,
    local_backend: Option<Arc<dyn LlmHttpBackend>>,
    frontier_backend: Option<Arc<dyn LlmHttpBackend>>,
    artifact_store: Arc<dyn ArtifactStore>,
    kernel: Option<Arc<super::kernel::KernelContext>>,
) -> Result<CognitiveEngine, ExoError> {
    let runtime_config = RuntimeConfig {
        data_dir: config.data_dir.join("cognitive-aq"),
        tick_interval: config.cognitive_tick_interval,
        dispatch_concurrency: config.cognitive_dispatch_concurrency,
        lease_timeout_secs: config.cognitive_lease_timeout_secs,
        ..Default::default()
    };

    let mut handler = CognitiveHandler::with_backends(
        local_backend,
        frontier_backend,
        config.llm_config.default_backend,
        artifact_store,
    );
    if let Some(k) = kernel {
        handler.kernel = Some(k);
    }
    let engine = ActionQueueEngine::new(runtime_config, handler);

    engine
        .bootstrap()
        .map_err(|e| ExoError::Engine(format!("cognitive AQ bootstrap failed: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cognitive_task_type_roundtrip() {
        let types = [CognitiveTaskType::MasterLoop, CognitiveTaskType::LlmCall];
        for t in &types {
            let json = serde_json::to_string(t).unwrap();
            let parsed: CognitiveTaskType = serde_json::from_str(&json).unwrap();
            assert_eq!(*t, parsed);
        }
    }

    #[test]
    fn cognitive_task_type_snake_case() {
        assert_eq!(
            serde_json::to_string(&CognitiveTaskType::MasterLoop).unwrap(),
            "\"master_loop\""
        );
        assert_eq!(
            serde_json::to_string(&CognitiveTaskType::LlmCall).unwrap(),
            "\"llm_call\""
        );
    }

    #[test]
    fn cognitive_payload_roundtrip() {
        let payload = CognitivePayload {
            task_type: CognitiveTaskType::MasterLoop,
            data: serde_json::json!({"tick": 1}),
        };
        let json = serde_json::to_string(&payload).unwrap();
        let parsed: CognitivePayload = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.task_type, CognitiveTaskType::MasterLoop);
    }

    #[test]
    fn cognitive_payload_data_defaults_to_null() {
        let json = r#"{"task_type": "llm_call"}"#;
        let payload: CognitivePayload = serde_json::from_str(json).unwrap();
        assert_eq!(payload.task_type, CognitiveTaskType::LlmCall);
        assert!(payload.data.is_null());
    }
}

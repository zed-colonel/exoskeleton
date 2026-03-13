//! LlmClient: convenience wrapper for LLM inference via the Cognitive AQ.
//!
//! All LLM calls go through the Cognitive AQ (I9: cognitive work on cognitive
//! engine). The LlmClient creates a TaskSpec with `CognitiveTaskType::LlmCall`,
//! submits it to the engine, waits for completion, and returns the response.

use exoskeleton_core::llm::{LlmBackend, LlmRequest, LlmResponse};
use exoskeleton_core::ExoError;

use crate::cognitive_engine::{CognitivePayload, CognitiveTaskType};
use crate::config::LlmConfig;
use crate::vessel::CognitiveEngineSlot;

/// Convenience wrapper for LLM inference via the Cognitive AQ.
///
/// This is the public API for LLM inference. Internal components (master loop,
/// threads) use this rather than calling the handler directly.
pub struct LlmClient {
    engine_slot: CognitiveEngineSlot,
    default_backend: LlmBackend,
    max_output_tokens: u64,
}

impl LlmClient {
    /// Create a new LlmClient.
    pub fn new(engine_slot: CognitiveEngineSlot, llm_config: &LlmConfig) -> Self {
        Self {
            engine_slot,
            default_backend: llm_config.default_backend,
            max_output_tokens: llm_config.max_output_tokens,
        }
    }

    /// The default backend configured for this client.
    pub fn default_backend(&self) -> LlmBackend {
        self.default_backend
    }

    /// The default max output tokens configured for this client.
    pub fn max_output_tokens(&self) -> u64 {
        self.max_output_tokens
    }

    /// Submit an LLM request to the Cognitive AQ and wait for the response.
    ///
    /// # Errors
    /// - `ExoError::LlmInvocation` — model returned an error
    /// - `ExoError::Engine` — Cognitive AQ task submission or processing failed
    /// - `ExoError::Serde` — response deserialization failed
    pub async fn call(&self, request: LlmRequest) -> Result<LlmResponse, ExoError> {
        use actionqueue_core::ids::TaskId;
        use actionqueue_core::task::constraints::TaskConstraints;
        use actionqueue_core::task::metadata::TaskMetadata;
        use actionqueue_core::task::run_policy::RunPolicy;
        use actionqueue_core::task::task_spec::{TaskPayload, TaskSpec};

        let payload = CognitivePayload {
            task_type: CognitiveTaskType::LlmCall,
            data: serde_json::to_value(&request)
                .map_err(|e| ExoError::LlmInvocation(format!("request serialization: {e}")))?,
        };
        let payload_bytes = serde_json::to_vec(&payload)
            .map_err(|e| ExoError::LlmInvocation(format!("payload serialization: {e}")))?;

        let task_id = TaskId::new();
        let spec = TaskSpec::new(
            task_id,
            TaskPayload::with_content_type(payload_bytes, "application/json"),
            RunPolicy::Once,
            TaskConstraints::default(),
            TaskMetadata::default(),
        )
        .map_err(|e| ExoError::Engine(format!("task spec creation: {e}")))?;

        // Submit to Cognitive AQ
        {
            let mut guard = self.engine_slot.lock().await;
            let engine = guard
                .as_mut()
                .ok_or_else(|| ExoError::Engine("cognitive engine not available".into()))?;
            engine
                .submit_task(spec)
                .map_err(|e| ExoError::Engine(format!("task submission: {e}")))?;

            // Process until the task completes
            let _ = engine
                .run_until_idle()
                .await
                .map_err(|e| ExoError::Engine(format!("engine run: {e}")))?;
        }

        // Extract the result from the completed task
        self.extract_response(task_id).await
    }

    /// Submit an LLM request using the default backend.
    pub async fn call_default(&self, request: LlmRequest) -> Result<LlmResponse, ExoError> {
        self.call(request).await
    }

    /// Extract the LLM response from a completed task.
    async fn extract_response(
        &self,
        task_id: actionqueue_core::ids::TaskId,
    ) -> Result<LlmResponse, ExoError> {
        let guard = self.engine_slot.lock().await;
        let engine = guard
            .as_ref()
            .ok_or_else(|| ExoError::Engine("cognitive engine not available".into()))?;

        let projection = engine.projection();

        // Find the run(s) for our task
        let runs: Vec<_> = projection.runs_for_task(task_id).collect();
        let run = runs
            .last()
            .ok_or_else(|| ExoError::Engine("no runs found for task".into()))?;

        // Check run state
        use actionqueue_core::run::state::RunState;
        match run.state() {
            RunState::Completed => {}
            RunState::Failed => {
                // Try to get error from attempt history
                let run_id = run.id();
                let error_msg = projection
                    .get_attempt_history(&run_id)
                    .and_then(|attempts| attempts.last())
                    .and_then(|a| a.error())
                    .unwrap_or("LLM task failed (check handler logs)");
                return Err(ExoError::LlmInvocation(error_msg.to_string()));
            }
            other => {
                return Err(ExoError::Engine(format!("unexpected run state: {other:?}")));
            }
        }

        // Get output from the latest attempt
        let run_id = run.id();
        let attempts = projection
            .get_attempt_history(&run_id)
            .ok_or_else(|| ExoError::Engine("no attempt history for run".into()))?;

        let latest_attempt = attempts
            .last()
            .ok_or_else(|| ExoError::Engine("no attempts in history".into()))?;

        let output = latest_attempt
            .output()
            .ok_or_else(|| ExoError::Engine("task completed but no output".into()))?;

        let response: LlmResponse = serde_json::from_slice(output)
            .map_err(|e| ExoError::LlmInvocation(format!("response deserialization: {e}")))?;

        Ok(response)
    }
}

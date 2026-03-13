//! Decide step — call LLM and parse the decision.
//!
//! **Why direct backend call instead of LlmClient:** The LlmClient::call()
//! method locks the CognitiveEngineSlot and calls engine.run_until_idle().
//! But the master loop handler is already running inside the Cognitive AQ
//! dispatch loop — calling run_until_idle() from within a handler would
//! deadlock. Instead, the Decide step calls the HTTP backend directly.

use std::time::Instant;

use actionqueue_executor_local::CancellationToken;
use exoskeleton_core::llm::{LlmBackend, LlmMessage, LlmRequest, LlmRole};
use exoskeleton_core::tick::LlmCallRecord;
use exoskeleton_core::{Artifact, ArtifactKind, ExoError};

use super::types::{
    extract_json_from_code_fence, DecisionProtocol, DecisionResult, OrientationResult,
    SnapshotDelta,
};
use super::KernelContext;
use crate::cognitive_engine::CognitiveHandler;

/// Execute the Decide step: call LLM and parse the decision.
pub fn decide(
    handler: &CognitiveHandler,
    kernel: &KernelContext,
    orientation: &OrientationResult,
    cancellation: &CancellationToken,
) -> Result<DecisionResult, ExoError> {
    // 1. Check cancellation
    if cancellation.is_cancelled() {
        return Err(ExoError::LlmInvocation(
            "cancelled before Decide step".into(),
        ));
    }

    // 2. Build system prompt with available tools from WI Host
    let tools_description = build_tools_description(kernel);
    let system_prompt = build_system_prompt(kernel, &tools_description);

    // 3. Build user message from compiled context
    let user_message = orientation.compiled_context.prompt.clone();

    // 4. Build LLM request
    let request = LlmRequest {
        backend: None, // use default
        system_prompt: Some(system_prompt),
        messages: vec![LlmMessage {
            role: LlmRole::User,
            content: user_message,
        }],
        max_output_tokens: kernel.max_output_tokens,
        temperature: Some(0.7),
        stop_sequences: vec![],
    };

    // 5. Resolve backend — with escalation logic (Sprint 9)
    let backend_type = resolve_backend(handler, kernel, orientation);
    let backend = match backend_type {
        LlmBackend::Local => handler.local_backend.as_ref(),
        LlmBackend::Frontier => handler.frontier_backend.as_ref(),
    };
    // Fallback: if chosen backend isn't configured, try the other
    let (backend, backend_type) = match backend {
        Some(b) => (b, backend_type),
        None => {
            let fallback = match backend_type {
                LlmBackend::Local => LlmBackend::Frontier,
                LlmBackend::Frontier => LlmBackend::Local,
            };
            match match fallback {
                LlmBackend::Local => handler.local_backend.as_ref(),
                LlmBackend::Frontier => handler.frontier_backend.as_ref(),
            } {
                Some(b) => (b, fallback),
                None => {
                    return Err(ExoError::LlmInvocation("no LLM backend configured".into()));
                }
            }
        }
    };

    // 6. Make LLM call (direct backend call, NOT LlmClient — avoids deadlock H-1)
    let start = Instant::now();
    let mut llm_response = backend.call(&handler.http_client, &request, cancellation)?;
    let latency_ms = start.elapsed().as_millis() as u64;
    llm_response.latency_ms = latency_ms;

    // 6.5 Record LLM consumption in budget tracker (Sprint 9)
    if let Some(ref tracker) = kernel.budget_tracker {
        if let Ok(mut guard) = tracker.try_lock() {
            guard.record_llm_call(
                backend_type,
                llm_response.tokens_in,
                llm_response.tokens_out,
                llm_response.cost_estimate_cents.unwrap_or(0.0),
            );
        }
    }

    // 6.6 Record LLM metrics (Sprint 10)
    if let Some(ref m) = kernel.metrics {
        let backend_label = match backend_type {
            LlmBackend::Local => "local",
            LlmBackend::Frontier => "frontier",
        };
        m.llm_calls_total.with_label_values(&[backend_label]).inc();
        m.llm_tokens_total
            .with_label_values(&[backend_label, "input"])
            .inc_by(llm_response.tokens_in);
        m.llm_tokens_total
            .with_label_values(&[backend_label, "output"])
            .inc_by(llm_response.tokens_out);
        m.llm_cost_cents_total
            .with_label_values(&[backend_label])
            .inc_by(llm_response.cost_estimate_cents.unwrap_or(0.0));
        m.llm_latency_seconds
            .with_label_values(&[backend_label])
            .observe(latency_ms as f64 / 1000.0);
    }

    // 7. Store LLM response as artifact BEFORE proceeding (I3, IBP §4.5)
    let response_artifact = Artifact::from_json(ArtifactKind::LlmResponse, &llm_response)?;
    let response_artifact_id = handler.artifact_store.put(&response_artifact)?;

    // 8. Parse DecisionProtocol from response
    let (protocol, raw_reasoning) = parse_decision(&llm_response.content);

    // 9. Store Decision artifact (I3)
    let decision_artifact = Artifact::from_json(ArtifactKind::Decision, &protocol)?;
    handler.artifact_store.put(&decision_artifact)?;

    // 10. Build LlmCallRecord
    let llm_call_record = LlmCallRecord {
        model: llm_response.model,
        tokens_in: llm_response.tokens_in,
        tokens_out: llm_response.tokens_out,
        cost_cents: llm_response.cost_estimate_cents.unwrap_or(0.0),
        latency_ms,
        response_artifact_ref: Some(response_artifact_id.clone()),
    };

    // 11. Build DecisionResult
    Ok(DecisionResult {
        reasoning: raw_reasoning,
        actions: protocol.actions,
        snapshot_delta: SnapshotDelta {
            plan_update: protocol.plan_update,
            working_context_update: protocol.working_context_update,
        },
        memory_notes: protocol.memory_notes,
        llm_call_record,
        response_artifact_id,
    })
}

/// Determine which LLM backend to use for this tick's Decide step.
///
/// Default: use handler's default_backend (usually local). Escalates to
/// frontier when configurable triggers fire. Falls back to local if frontier
/// budget is exhausted (never halts entirely).
fn resolve_backend(
    handler: &CognitiveHandler,
    kernel: &KernelContext,
    _orientation: &OrientationResult,
) -> LlmBackend {
    // No budget tracker → no escalation, use default
    let tracker = match kernel.budget_tracker {
        Some(ref t) => t,
        None => return handler.default_backend,
    };

    // Try to lock tracker; if contended (window timer resetting), use default (H-1)
    let guard = match tracker.try_lock() {
        Ok(g) => g,
        Err(_) => return handler.default_backend,
    };

    // No frontier backend → no escalation possible
    if handler.frontier_backend.is_none() {
        return handler.default_backend;
    }

    // Check escalation triggers
    let should_escalate = check_escalation_triggers(kernel, &guard);

    if !should_escalate {
        return handler.default_backend;
    }

    // Check frontier budget
    if guard.remaining_frontier_tokens() == 0 || guard.remaining_frontier_cost() == 0 {
        tracing::info!("escalation triggered but frontier budget exhausted; staying local");
        return LlmBackend::Local;
    }

    // Check frontier call count
    let policy = &guard.config().escalation_policy;
    if guard.frontier_calls_this_window() >= policy.max_frontier_calls_per_window {
        tracing::info!("escalation triggered but frontier call limit reached; staying local");
        return LlmBackend::Local;
    }

    tracing::info!("escalating to frontier model");
    LlmBackend::Frontier
}

/// Check if any escalation trigger fires.
fn check_escalation_triggers(
    kernel: &KernelContext,
    tracker: &crate::budget::CognitiveBudgetTracker,
) -> bool {
    let policy = &tracker.config().escalation_policy;

    // a. Consecutive failures >= threshold
    if tracker.consecutive_failures() >= policy.escalate_on_consecutive_failures {
        tracing::info!(
            failures = tracker.consecutive_failures(),
            "escalation trigger: consecutive failures"
        );
        return true;
    }

    // b. Self-Critique thrash_indicator > escalation_policy.escalate_on_uncertainty
    if let Some(thrash) = extract_thrash_indicator(kernel) {
        if thrash > policy.escalate_on_uncertainty {
            tracing::info!(thrash, "escalation trigger: high uncertainty/thrash");
            return true;
        }
    }

    // c. Threat Monitor severity >= High
    if extract_high_threat(kernel) {
        tracing::info!("escalation trigger: high threat severity");
        return true;
    }

    false
}

/// Extract the thrash_indicator from the most recent Self-Critique output.
fn extract_thrash_indicator(kernel: &KernelContext) -> Option<f64> {
    let outputs = kernel
        .thread_registry
        .recent_outputs(exoskeleton_threads::SELF_CRITIQUE_ID, 1)
        .ok()?;
    let output = outputs.first()?;

    // Load the artifact content and try to parse as SelfCritique
    let artifact = kernel.artifact_store.get(&output.artifact_id).ok()??;
    let critique: exoskeleton_threads::SelfCritique =
        serde_json::from_slice(&artifact.content).ok()?;
    Some(critique.thrash_indicator)
}

/// Check if the most recent Threat Monitor output has severity >= High.
fn extract_high_threat(kernel: &KernelContext) -> bool {
    let outputs = match kernel
        .thread_registry
        .recent_outputs(exoskeleton_threads::THREAT_MONITOR_ID, 1)
    {
        Ok(o) => o,
        Err(_) => return false,
    };
    let output = match outputs.first() {
        Some(o) => o,
        None => return false,
    };

    // Load artifact and try to parse as ThreatAssessment
    let artifact = match kernel.artifact_store.get(&output.artifact_id) {
        Ok(Some(a)) => a,
        _ => return false,
    };
    let assessment: exoskeleton_threads::ThreatAssessment =
        match serde_json::from_slice(&artifact.content) {
            Ok(a) => a,
            Err(_) => return false,
        };
    matches!(
        assessment.severity,
        exoskeleton_threads::ThreatSeverity::High | exoskeleton_threads::ThreatSeverity::Critical
    )
}

fn build_tools_description(kernel: &KernelContext) -> String {
    // Read tool capabilities from WI Host slot.
    // NOTE: We use try_lock here because we're in a sync context.
    // The slot should be populated by boot time.
    let guard = kernel.wi_host_slot.try_lock();
    match guard {
        Ok(slot) => match slot.as_ref() {
            Some(host) => {
                let caps = host.list_capabilities();
                if caps.is_empty() {
                    "No tools available.".into()
                } else {
                    caps.iter()
                        .map(|d| format!("- {}: {}", d.name, d.description))
                        .collect::<Vec<_>>()
                        .join("\n")
                }
            }
            None => "No tools available (WI Host not ready).".into(),
        },
        Err(_) => "Tools unavailable (slot locked).".into(),
    }
}

fn build_system_prompt(kernel: &KernelContext, tools: &str) -> String {
    format!(
        r#"You are an autonomous agent (vessel {vessel_id}).
Mission: {mission}

You are in the Decide phase of your PODAARA cognitive loop. Based on the context below, decide what actions to take.

Available tools:
{tools}

Respond with JSON in this exact format:
{{
  "reasoning": "Your analysis and chain of thought",
  "plan_update": "New plan (omit if unchanged)",
  "working_context_update": "New focus (omit if unchanged)",
  "actions": [
    {{"tool_name": "...", "params": {{}}, "rationale": "..."}}
  ],
  "memory_notes": ["Observations to remember"]
}}

If no actions are needed, return an empty actions array.
Always include reasoning."#,
        vessel_id = kernel.vessel_id,
        mission = kernel.mission,
        tools = tools,
    )
}

/// Parse a DecisionProtocol from LLM response text.
///
/// 1. Try direct JSON parse
/// 2. Try extracting from code fences
/// 3. Fall back to no-action decision with raw text as reasoning
fn parse_decision(response_text: &str) -> (DecisionProtocol, String) {
    // Attempt 1: direct JSON parse
    if let Ok(protocol) = serde_json::from_str::<DecisionProtocol>(response_text) {
        let reasoning = protocol.reasoning.clone();
        return (protocol, reasoning);
    }

    // Attempt 2: extract from code fence
    if let Some(json_str) = extract_json_from_code_fence(response_text) {
        if let Ok(protocol) = serde_json::from_str::<DecisionProtocol>(json_str) {
            let reasoning = protocol.reasoning.clone();
            return (protocol, reasoning);
        }
    }

    // Attempt 3: fallback — no-action decision with raw text as reasoning
    tracing::warn!("LLM returned unparseable response, treating as no-action tick");
    let protocol = DecisionProtocol {
        reasoning: response_text.to_string(),
        plan_update: None,
        working_context_update: None,
        actions: vec![],
        memory_notes: vec![],
    };
    (protocol, response_text.to_string())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use exoskeleton_core::artifact::ArtifactKind;
    use exoskeleton_core::llm::{LlmBackend, LlmResponse, StopReason};
    use exoskeleton_core::ArtifactStore;
    use exoskeleton_memory::{ApproximateTokenCounter, ContextCompiler};
    use exoskeleton_relationship::InMemoryRelationshipLedger;
    use exoskeleton_threads::{InMemoryThreadStore, ThreadRegistry};

    use super::super::types::PlannedAction;
    use super::super::KernelContext;
    use super::*;
    use crate::cognitive_engine::CognitiveHandler;
    use crate::inbox::InMemoryInbox;
    use crate::llm::mock::MockLlmBackend;
    use crate::storage::StorageManager;

    // ── Test helpers ──

    fn test_kernel(dir: &std::path::Path) -> KernelContext {
        let storage = StorageManager::open(dir).unwrap();
        let counter = Arc::new(ApproximateTokenCounter);
        let compiler = ContextCompiler::with_defaults(counter, 4000);

        KernelContext {
            snapshot_store: storage.snapshot_store().clone(),
            event_ledger: storage.event_ledger().clone(),
            tick_store: storage.tick_store().clone(),
            memory_store: storage.memory_store().clone(),
            context_compiler: Arc::new(compiler),
            artifact_store: storage.artifact_store().clone(),
            wi_host_slot: Arc::new(tokio::sync::Mutex::new(None)),
            inbox: Arc::new(InMemoryInbox::new()),
            vessel_id: exoskeleton_core::VesselId::new(),
            mission: "test mission".into(),
            max_output_tokens: 4096,
            master_loop_interval_secs: 60,
            thread_registry: Arc::new(ThreadRegistry::new(Arc::new(InMemoryThreadStore::new()))),
            relationship_ledger: Arc::new(InMemoryRelationshipLedger::new()),
            budget_tracker: None,
            tool_budget_gate: None,
            metrics: None,
        }
    }

    fn test_handler_with_mock(
        response: LlmResponse,
        artifact_store: Arc<dyn ArtifactStore>,
    ) -> CognitiveHandler {
        let mock = Arc::new(MockLlmBackend::new(response));
        CognitiveHandler::with_backends(Some(mock), None, LlmBackend::Local, artifact_store)
    }

    fn valid_decision_json() -> String {
        serde_json::to_string(&DecisionProtocol {
            reasoning: "I need to write a file".into(),
            plan_update: Some("Write output".into()),
            working_context_update: Some("Writing file".into()),
            actions: vec![PlannedAction {
                tool_name: "fs.write".into(),
                params: serde_json::json!({"path": "/tmp/out.txt"}),
                rationale: "Write output".into(),
            }],
            memory_notes: vec!["File written".into()],
        })
        .unwrap()
    }

    fn mock_response_with_content(content: String) -> LlmResponse {
        LlmResponse {
            content,
            model: "mock-model".into(),
            tokens_in: 100,
            tokens_out: 50,
            latency_ms: 0,
            stop_reason: StopReason::EndTurn,
            cost_estimate_cents: Some(0.5),
            backend: LlmBackend::Local,
        }
    }

    fn test_orientation() -> OrientationResult {
        use exoskeleton_memory::CompiledContext;
        OrientationResult {
            compiled_context: CompiledContext {
                prompt: "You have no pending tasks.".into(),
                total_tokens: 10,
                budget: 4000,
                sections: vec![],
                truncated_sections: vec![],
            },
        }
    }

    // ── T-5 Tests ──

    #[test]
    fn decide_calls_llm_and_parses_valid_json() {
        let dir = tempfile::tempdir().unwrap();
        let kernel = test_kernel(dir.path());
        let response = mock_response_with_content(valid_decision_json());
        let handler = test_handler_with_mock(response, kernel.artifact_store.clone());
        let orientation = test_orientation();
        let token = CancellationToken::new();

        let result = decide(&handler, &kernel, &orientation, &token).unwrap();

        assert_eq!(result.reasoning, "I need to write a file");
        assert_eq!(result.actions.len(), 1);
        assert_eq!(result.actions[0].tool_name, "fs.write");
        assert_eq!(
            result.snapshot_delta.plan_update,
            Some("Write output".into())
        );
        assert_eq!(
            result.snapshot_delta.working_context_update,
            Some("Writing file".into())
        );
        assert_eq!(result.memory_notes, vec!["File written"]);
    }

    #[test]
    fn decide_handles_code_fenced_json() {
        let dir = tempfile::tempdir().unwrap();
        let kernel = test_kernel(dir.path());
        let fenced = format!(
            "Here is my decision:\n\n```json\n{}\n```\n\nThat's it.",
            valid_decision_json()
        );
        let response = mock_response_with_content(fenced);
        let handler = test_handler_with_mock(response, kernel.artifact_store.clone());
        let orientation = test_orientation();
        let token = CancellationToken::new();

        let result = decide(&handler, &kernel, &orientation, &token).unwrap();

        assert_eq!(result.reasoning, "I need to write a file");
        assert_eq!(result.actions.len(), 1);
        assert_eq!(result.actions[0].tool_name, "fs.write");
    }

    #[test]
    fn decide_fallback_on_invalid_json() {
        let dir = tempfile::tempdir().unwrap();
        let kernel = test_kernel(dir.path());
        let response = mock_response_with_content("I don't know what to do.".into());
        let handler = test_handler_with_mock(response, kernel.artifact_store.clone());
        let orientation = test_orientation();
        let token = CancellationToken::new();

        let result = decide(&handler, &kernel, &orientation, &token).unwrap();

        assert_eq!(result.reasoning, "I don't know what to do.");
        assert!(result.actions.is_empty());
        assert!(result.snapshot_delta.plan_update.is_none());
        assert!(result.memory_notes.is_empty());
    }

    #[test]
    fn decide_stores_response_artifact() {
        let dir = tempfile::tempdir().unwrap();
        let kernel = test_kernel(dir.path());
        let response = mock_response_with_content(valid_decision_json());
        let handler = test_handler_with_mock(response, kernel.artifact_store.clone());
        let orientation = test_orientation();
        let token = CancellationToken::new();

        let result = decide(&handler, &kernel, &orientation, &token).unwrap();

        // I3: LlmResponse artifact must exist in the store
        let stored = kernel
            .artifact_store
            .get(&result.response_artifact_id)
            .unwrap();
        assert!(stored.is_some());
        let artifact = stored.unwrap();
        assert_eq!(artifact.kind, ArtifactKind::LlmResponse);
    }

    #[test]
    fn decide_stores_decision_artifact() {
        let dir = tempfile::tempdir().unwrap();
        let kernel = test_kernel(dir.path());
        let response = mock_response_with_content(valid_decision_json());
        let handler = test_handler_with_mock(response, kernel.artifact_store.clone());
        let orientation = test_orientation();
        let token = CancellationToken::new();

        let _result = decide(&handler, &kernel, &orientation, &token).unwrap();

        // I3: Decision artifact must exist in the store
        let decisions = kernel
            .artifact_store
            .list_by_kind(ArtifactKind::Decision, 10)
            .unwrap();
        assert!(!decisions.is_empty());
    }

    #[test]
    fn decide_creates_llm_call_record() {
        let dir = tempfile::tempdir().unwrap();
        let kernel = test_kernel(dir.path());
        let response = mock_response_with_content(valid_decision_json());
        let handler = test_handler_with_mock(response, kernel.artifact_store.clone());
        let orientation = test_orientation();
        let token = CancellationToken::new();

        let result = decide(&handler, &kernel, &orientation, &token).unwrap();

        assert_eq!(result.llm_call_record.model, "mock-model");
        assert_eq!(result.llm_call_record.tokens_in, 100);
        assert_eq!(result.llm_call_record.tokens_out, 50);
        assert!(result.llm_call_record.response_artifact_ref.is_some());
    }

    #[test]
    fn decide_respects_cancellation() {
        let dir = tempfile::tempdir().unwrap();
        let kernel = test_kernel(dir.path());
        let response = mock_response_with_content(valid_decision_json());
        let handler = test_handler_with_mock(response, kernel.artifact_store.clone());
        let orientation = test_orientation();

        // Create an already-cancelled token
        let token = CancellationToken::new();
        token.cancel();

        let result = decide(&handler, &kernel, &orientation, &token);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("cancelled"));
    }

    #[test]
    fn decide_handles_backend_error() {
        let dir = tempfile::tempdir().unwrap();
        let kernel = test_kernel(dir.path());
        let mock = Arc::new(MockLlmBackend::failing("503: service unavailable"));
        let handler = CognitiveHandler::with_backends(
            Some(mock),
            None,
            LlmBackend::Local,
            kernel.artifact_store.clone(),
        );
        let orientation = test_orientation();
        let token = CancellationToken::new();

        let result = decide(&handler, &kernel, &orientation, &token);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("503"));
    }

    #[test]
    fn decide_system_prompt_includes_tools() {
        let kernel_ctx = {
            let dir = tempfile::tempdir().unwrap();
            test_kernel(dir.path())
        };
        let prompt = build_system_prompt(&kernel_ctx, "- fs.write: Write a file");
        assert!(prompt.contains("Available tools:"));
        assert!(prompt.contains("fs.write: Write a file"));
        assert!(prompt.contains("test mission"));
        assert!(prompt.contains("PODAARA"));
    }

    #[test]
    fn decide_backend_not_configured() {
        let dir = tempfile::tempdir().unwrap();
        let kernel = test_kernel(dir.path());
        // No backends configured at all
        let handler = CognitiveHandler::with_backends(
            None,
            None,
            LlmBackend::Local,
            kernel.artifact_store.clone(),
        );
        let orientation = test_orientation();
        let token = CancellationToken::new();

        let result = decide(&handler, &kernel, &orientation, &token);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("no LLM backend configured"),
            "expected 'no LLM backend configured' in: {err_msg}"
        );
    }

    // ── Escalation trigger tests ──

    fn test_kernel_with_budget(
        dir: &std::path::Path,
        config: exoskeleton_core::budget::CognitiveBudgetConfig,
    ) -> KernelContext {
        let storage = StorageManager::open(dir).unwrap();
        let counter = Arc::new(ApproximateTokenCounter);
        let compiler = ContextCompiler::with_defaults(counter, 4000);
        let budget_store: Arc<dyn exoskeleton_core::budget::BudgetStore> =
            Arc::new(crate::budget::tracker::InMemoryBudgetStore::new());
        let tracker = crate::budget::CognitiveBudgetTracker::new(config, budget_store);

        KernelContext {
            snapshot_store: storage.snapshot_store().clone(),
            event_ledger: storage.event_ledger().clone(),
            tick_store: storage.tick_store().clone(),
            memory_store: storage.memory_store().clone(),
            context_compiler: Arc::new(compiler),
            artifact_store: storage.artifact_store().clone(),
            wi_host_slot: Arc::new(tokio::sync::Mutex::new(None)),
            inbox: Arc::new(InMemoryInbox::new()),
            vessel_id: exoskeleton_core::VesselId::new(),
            mission: "test mission".into(),
            max_output_tokens: 4096,
            master_loop_interval_secs: 60,
            thread_registry: Arc::new(ThreadRegistry::new(Arc::new(InMemoryThreadStore::new()))),
            relationship_ledger: Arc::new(InMemoryRelationshipLedger::new()),
            budget_tracker: Some(Arc::new(tokio::sync::Mutex::new(tracker))),
            tool_budget_gate: None,
            metrics: None,
        }
    }

    fn default_budget_config() -> exoskeleton_core::budget::CognitiveBudgetConfig {
        exoskeleton_core::budget::CognitiveBudgetConfig::default()
    }

    #[test]
    fn resolve_backend_default_local_no_tracker() {
        let dir = tempfile::tempdir().unwrap();
        let kernel = test_kernel(dir.path());
        let handler = CognitiveHandler::with_backends(
            None,
            None,
            LlmBackend::Local,
            kernel.artifact_store.clone(),
        );
        let orientation = test_orientation();

        let result = resolve_backend(&handler, &kernel, &orientation);
        assert_eq!(result, LlmBackend::Local);
    }

    #[test]
    fn resolve_backend_consecutive_failures_escalates() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = default_budget_config();
        config.escalation_policy.escalate_on_consecutive_failures = 3;
        let kernel = test_kernel_with_budget(dir.path(), config);

        // Set up handler with both backends
        let mock_local = Arc::new(MockLlmBackend::new(mock_response_with_content(
            "local".into(),
        )));
        let mock_frontier = Arc::new(MockLlmBackend::new(mock_response_with_content(
            "frontier".into(),
        )));
        let handler = CognitiveHandler::with_backends(
            Some(mock_local),
            Some(mock_frontier),
            LlmBackend::Local,
            kernel.artifact_store.clone(),
        );
        let orientation = test_orientation();

        // Record 3 failures to trigger escalation
        {
            let tracker = kernel.budget_tracker.as_ref().unwrap();
            let mut guard = tracker.try_lock().unwrap();
            guard.record_failure();
            guard.record_failure();
            guard.record_failure();
        }

        let result = resolve_backend(&handler, &kernel, &orientation);
        assert_eq!(result, LlmBackend::Frontier);
    }

    #[test]
    fn resolve_backend_frontier_exhausted_stays_local() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = default_budget_config();
        config.escalation_policy.escalate_on_consecutive_failures = 2;
        config.frontier_token_budget = 0; // No frontier tokens
        let kernel = test_kernel_with_budget(dir.path(), config);

        let mock_local = Arc::new(MockLlmBackend::new(mock_response_with_content(
            "local".into(),
        )));
        let mock_frontier = Arc::new(MockLlmBackend::new(mock_response_with_content(
            "frontier".into(),
        )));
        let handler = CognitiveHandler::with_backends(
            Some(mock_local),
            Some(mock_frontier),
            LlmBackend::Local,
            kernel.artifact_store.clone(),
        );
        let orientation = test_orientation();

        // Record failures to trigger escalation
        {
            let tracker = kernel.budget_tracker.as_ref().unwrap();
            let mut guard = tracker.try_lock().unwrap();
            guard.record_failure();
            guard.record_failure();
        }

        let result = resolve_backend(&handler, &kernel, &orientation);
        assert_eq!(result, LlmBackend::Local);
    }

    #[test]
    fn resolve_backend_frontier_not_configured_stays_default() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = default_budget_config();
        config.escalation_policy.escalate_on_consecutive_failures = 1;
        let kernel = test_kernel_with_budget(dir.path(), config);

        // Only local backend configured, no frontier
        let mock_local = Arc::new(MockLlmBackend::new(mock_response_with_content(
            "local".into(),
        )));
        let handler = CognitiveHandler::with_backends(
            Some(mock_local),
            None,
            LlmBackend::Local,
            kernel.artifact_store.clone(),
        );
        let orientation = test_orientation();

        // Record failures — but no frontier backend so should stay default
        {
            let tracker = kernel.budget_tracker.as_ref().unwrap();
            let mut guard = tracker.try_lock().unwrap();
            guard.record_failure();
        }

        let result = resolve_backend(&handler, &kernel, &orientation);
        assert_eq!(result, LlmBackend::Local);
    }

    #[test]
    fn resolve_backend_self_critique_thrash_escalates() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = default_budget_config();
        config.escalation_policy.escalate_on_uncertainty = 0.6;
        let kernel = test_kernel_with_budget(dir.path(), config);

        // Store a SelfCritique artifact with high thrash
        let critique = exoskeleton_threads::SelfCritique {
            progress_rating: 0.3,
            concerns: vec!["thrashing".into()],
            suggestions: vec![],
            thrash_indicator: 0.8, // > 0.6 threshold
        };
        let artifact = exoskeleton_core::Artifact::from_json(
            exoskeleton_core::ArtifactKind::ThreadOutput,
            &critique,
        )
        .unwrap();
        let artifact_id = kernel.artifact_store.put(&artifact).unwrap();

        // Save a thread output referencing the artifact
        let thread_output = exoskeleton_core::ThreadOutput {
            thread_id: exoskeleton_threads::SELF_CRITIQUE_ID,
            tick_id: exoskeleton_core::TickId::new(),
            artifact_id,
            summary: "High thrash detected".into(),
            recommendations: vec![],
        };
        kernel.thread_registry.save_output(&thread_output).unwrap();

        // Register the Self-Critique thread so it exists
        exoskeleton_threads::register_builtin_threads(&kernel.thread_registry).unwrap();

        let mock_local = Arc::new(MockLlmBackend::new(mock_response_with_content(
            "local".into(),
        )));
        let mock_frontier = Arc::new(MockLlmBackend::new(mock_response_with_content(
            "frontier".into(),
        )));
        let handler = CognitiveHandler::with_backends(
            Some(mock_local),
            Some(mock_frontier),
            LlmBackend::Local,
            kernel.artifact_store.clone(),
        );
        let orientation = test_orientation();

        let result = resolve_backend(&handler, &kernel, &orientation);
        assert_eq!(result, LlmBackend::Frontier);
    }

    #[test]
    fn resolve_backend_threat_monitor_high_escalates() {
        let dir = tempfile::tempdir().unwrap();
        let config = default_budget_config();
        let kernel = test_kernel_with_budget(dir.path(), config);

        // Store a ThreatAssessment artifact with High severity
        let assessment = exoskeleton_threads::ThreatAssessment {
            severity: exoskeleton_threads::ThreatSeverity::High,
            threats: vec![],
            recommendations: vec!["escalate".into()],
        };
        let artifact = exoskeleton_core::Artifact::from_json(
            exoskeleton_core::ArtifactKind::ThreadOutput,
            &assessment,
        )
        .unwrap();
        let artifact_id = kernel.artifact_store.put(&artifact).unwrap();

        // Save a thread output referencing the artifact
        let thread_output = exoskeleton_core::ThreadOutput {
            thread_id: exoskeleton_threads::THREAT_MONITOR_ID,
            tick_id: exoskeleton_core::TickId::new(),
            artifact_id,
            summary: "High threat".into(),
            recommendations: vec!["escalate".into()],
        };
        kernel.thread_registry.save_output(&thread_output).unwrap();

        // Register the Threat Monitor thread so it exists
        exoskeleton_threads::register_builtin_threads(&kernel.thread_registry).unwrap();

        let mock_local = Arc::new(MockLlmBackend::new(mock_response_with_content(
            "local".into(),
        )));
        let mock_frontier = Arc::new(MockLlmBackend::new(mock_response_with_content(
            "frontier".into(),
        )));
        let handler = CognitiveHandler::with_backends(
            Some(mock_local),
            Some(mock_frontier),
            LlmBackend::Local,
            kernel.artifact_store.clone(),
        );
        let orientation = test_orientation();

        let result = resolve_backend(&handler, &kernel, &orientation);
        assert_eq!(result, LlmBackend::Frontier);
    }

    #[test]
    fn resolve_backend_threat_monitor_low_no_escalation() {
        let dir = tempfile::tempdir().unwrap();
        let config = default_budget_config();
        let kernel = test_kernel_with_budget(dir.path(), config);

        // Store a ThreatAssessment artifact with Low severity (should NOT escalate)
        let assessment = exoskeleton_threads::ThreatAssessment {
            severity: exoskeleton_threads::ThreatSeverity::Low,
            threats: vec![],
            recommendations: vec![],
        };
        let artifact = exoskeleton_core::Artifact::from_json(
            exoskeleton_core::ArtifactKind::ThreadOutput,
            &assessment,
        )
        .unwrap();
        let artifact_id = kernel.artifact_store.put(&artifact).unwrap();

        let thread_output = exoskeleton_core::ThreadOutput {
            thread_id: exoskeleton_threads::THREAT_MONITOR_ID,
            tick_id: exoskeleton_core::TickId::new(),
            artifact_id,
            summary: "Low threat".into(),
            recommendations: vec![],
        };
        kernel.thread_registry.save_output(&thread_output).unwrap();

        exoskeleton_threads::register_builtin_threads(&kernel.thread_registry).unwrap();

        let mock_local = Arc::new(MockLlmBackend::new(mock_response_with_content(
            "local".into(),
        )));
        let mock_frontier = Arc::new(MockLlmBackend::new(mock_response_with_content(
            "frontier".into(),
        )));
        let handler = CognitiveHandler::with_backends(
            Some(mock_local),
            Some(mock_frontier),
            LlmBackend::Local,
            kernel.artifact_store.clone(),
        );
        let orientation = test_orientation();

        let result = resolve_backend(&handler, &kernel, &orientation);
        // No escalation triggers → default (Local)
        assert_eq!(result, LlmBackend::Local);
    }

    #[test]
    fn resolve_backend_self_critique_below_threshold_no_escalation() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = default_budget_config();
        config.escalation_policy.escalate_on_uncertainty = 0.7;
        let kernel = test_kernel_with_budget(dir.path(), config);

        // Store a SelfCritique artifact with thrash below threshold
        let critique = exoskeleton_threads::SelfCritique {
            progress_rating: 0.6,
            concerns: vec![],
            suggestions: vec![],
            thrash_indicator: 0.5, // < 0.7 threshold
        };
        let artifact = exoskeleton_core::Artifact::from_json(
            exoskeleton_core::ArtifactKind::ThreadOutput,
            &critique,
        )
        .unwrap();
        let artifact_id = kernel.artifact_store.put(&artifact).unwrap();

        let thread_output = exoskeleton_core::ThreadOutput {
            thread_id: exoskeleton_threads::SELF_CRITIQUE_ID,
            tick_id: exoskeleton_core::TickId::new(),
            artifact_id,
            summary: "Moderate concern".into(),
            recommendations: vec![],
        };
        kernel.thread_registry.save_output(&thread_output).unwrap();

        exoskeleton_threads::register_builtin_threads(&kernel.thread_registry).unwrap();

        let mock_local = Arc::new(MockLlmBackend::new(mock_response_with_content(
            "local".into(),
        )));
        let mock_frontier = Arc::new(MockLlmBackend::new(mock_response_with_content(
            "frontier".into(),
        )));
        let handler = CognitiveHandler::with_backends(
            Some(mock_local),
            Some(mock_frontier),
            LlmBackend::Local,
            kernel.artifact_store.clone(),
        );
        let orientation = test_orientation();

        let result = resolve_backend(&handler, &kernel, &orientation);
        // Below threshold → no escalation → default (Local)
        assert_eq!(result, LlmBackend::Local);
    }
}

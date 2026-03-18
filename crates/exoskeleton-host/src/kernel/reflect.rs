//! Reflect step — LLM-powered evaluation with heuristic fallback.

use actionqueue_executor_local::CancellationToken;
use exoskeleton_core::llm::{LlmMessage, LlmRequest, LlmRole};
use exoskeleton_core::tick::{ActionOutcome, LlmCallRecord};
use exoskeleton_core::{Artifact, ArtifactKind, ExoError, StateSnapshot};

use super::types::{
    extract_json_from_code_fence, ActResult, DecisionResult, ReflectProtocol, ReflectionResult,
};
use super::KernelContext;
use crate::cognitive_engine::CognitiveHandler;

/// Execute the Reflect step: LLM-powered evaluation with heuristic fallback.
#[allow(clippy::too_many_arguments)]
pub fn reflect(
    handler: &CognitiveHandler,
    kernel: &KernelContext,
    snapshot: &StateSnapshot,
    decision: &DecisionResult,
    act_result: &ActResult,
    cancellation: &CancellationToken,
) -> ReflectionResult {
    // Always compute heuristic baseline
    let heuristic = heuristic_reflect(act_result);

    // Skip LLM if no actions taken or cancelled
    if act_result.executions.is_empty() || cancellation.is_cancelled() {
        return heuristic;
    }

    // Try LLM-powered reflection; fall back to heuristic on any error
    match llm_reflect(
        handler,
        kernel,
        snapshot,
        decision,
        act_result,
        cancellation,
    ) {
        Ok(result) => result,
        Err(e) => {
            tracing::warn!(error = %e, "Reflect LLM call failed, using heuristic fallback");
            heuristic
        }
    }
}

/// Heuristic evaluation (the original Sprint 5 logic).
fn heuristic_reflect(act_result: &ActResult) -> ReflectionResult {
    let total = act_result.executions.len();

    if total == 0 {
        return ReflectionResult {
            action_success_rate: f64::NAN,
            observations: vec!["No actions taken".into()],
            concerns: vec![],
            task_updates: vec![],
            working_memory_ops: vec![],
            should_replan: false,
            llm_call_record: None,
        };
    }

    let successes = act_result
        .executions
        .iter()
        .filter(|e| e.record.outcome == ActionOutcome::Success)
        .count();
    let failures = total - successes;
    let rate = successes as f64 / total as f64;

    let mut observations = vec![format!(
        "Executed {total} actions: {successes} succeeded, {failures} failed"
    )];

    for exec in &act_result.executions {
        if exec.result.is_err() {
            observations.push(format!(
                "Failed action: {}: {}",
                exec.action.tool_name,
                exec.result.as_ref().unwrap_err()
            ));
        }
    }

    let mut concerns = vec![];
    if rate == 0.0 {
        concerns.push("All actions failed — possible misconfiguration".into());
    } else if rate < 0.5 {
        concerns.push("More than half of actions failed".into());
    }

    ReflectionResult {
        action_success_rate: rate,
        observations,
        concerns,
        task_updates: vec![],
        working_memory_ops: vec![],
        should_replan: false,
        llm_call_record: None,
    }
}

/// LLM-powered reflection (H-1 pattern: direct backend call).
#[allow(clippy::too_many_arguments)]
fn llm_reflect(
    handler: &CognitiveHandler,
    kernel: &KernelContext,
    snapshot: &StateSnapshot,
    decision: &DecisionResult,
    act_result: &ActResult,
    cancellation: &CancellationToken,
) -> Result<ReflectionResult, ExoError> {
    // 1. Build system prompt from template
    let system_prompt = kernel.prompt_registry.resolve(
        "reflect-system",
        &[
            ("vessel_id", &kernel.vessel_id.to_string()),
            ("mission", &kernel.mission),
        ],
    )?;

    // 2. Build user message with action outcomes + plan + working memory
    let user_message = build_reflect_user_message(snapshot, decision, act_result);

    // 3. Build request (always default backend, no escalation)
    let request = LlmRequest {
        backend: None,
        system_prompt: Some(system_prompt),
        messages: vec![LlmMessage {
            role: LlmRole::User,
            content: user_message,
        }],
        max_output_tokens: kernel.max_output_tokens / 2,
        temperature: Some(0.3),
        stop_sequences: vec![],
    };

    // 4. Use default backend (no escalation for Reflect)
    let backend = handler
        .local_backend
        .as_ref()
        .or(handler.frontier_backend.as_ref())
        .ok_or_else(|| ExoError::LlmInvocation("no LLM backend configured".into()))?;

    // 5. Direct backend call (H-1 pattern)
    let start = std::time::Instant::now();
    let mut response = backend.call(&handler.http_client, &request, cancellation)?;
    response.latency_ms = start.elapsed().as_millis() as u64;

    // 6. Record budget consumption
    if let Some(ref tracker) = kernel.budget_tracker {
        if let Ok(mut guard) = tracker.try_lock() {
            guard.record_llm_call(
                handler.default_backend,
                response.tokens_in,
                response.tokens_out,
                response.cost_estimate_cents.unwrap_or(0.0),
            );
        }
    }

    // 7. Store response as artifact (I3)
    let response_artifact = Artifact::from_json(ArtifactKind::LlmResponse, &response)?;
    let response_artifact_id = handler.artifact_store.put(&response_artifact)?;

    // 8. Parse ReflectProtocol
    let protocol = parse_reflect_protocol(&response.content);

    // 9. Compute heuristic for action_success_rate
    let heuristic = heuristic_reflect(act_result);

    // 10. Build LLM call record
    let llm_call_record = LlmCallRecord {
        model: response.model,
        tokens_in: response.tokens_in,
        tokens_out: response.tokens_out,
        cost_cents: response.cost_estimate_cents.unwrap_or(0.0),
        latency_ms: response.latency_ms,
        response_artifact_ref: Some(response_artifact_id),
    };

    Ok(ReflectionResult {
        action_success_rate: heuristic.action_success_rate,
        observations: protocol.observations,
        concerns: protocol.concerns,
        task_updates: protocol.task_updates,
        working_memory_ops: protocol.working_memory_ops,
        should_replan: protocol.should_replan,
        llm_call_record: Some(llm_call_record),
    })
}

/// Parse a ReflectProtocol from LLM response text.
fn parse_reflect_protocol(response_text: &str) -> ReflectProtocol {
    // Attempt 1: direct JSON
    if let Ok(p) = serde_json::from_str::<ReflectProtocol>(response_text) {
        return p;
    }
    // Attempt 2: code fence extraction
    if let Some(json_str) = extract_json_from_code_fence(response_text) {
        if let Ok(p) = serde_json::from_str::<ReflectProtocol>(json_str) {
            return p;
        }
    }
    // Fallback: treat response as observation text
    ReflectProtocol {
        outcome_assessment: response_text.to_string(),
        task_updates: vec![],
        working_memory_ops: vec![],
        observations: vec![response_text.to_string()],
        concerns: vec![],
        should_replan: false,
    }
}

/// Assemble context for the Reflect LLM.
fn build_reflect_user_message(
    snapshot: &StateSnapshot,
    decision: &DecisionResult,
    act_result: &ActResult,
) -> String {
    let mut msg = String::new();
    msg.push_str("## Action Outcomes\n\n");
    for exec in &act_result.executions {
        let status = if exec.result.is_ok() {
            "SUCCESS"
        } else {
            "FAILED"
        };
        msg.push_str(&format!(
            "- {} [{}]: {}\n",
            exec.action.tool_name, status, exec.action.rationale
        ));
        if let Err(ref e) = exec.result {
            msg.push_str(&format!("  Error: {e}\n"));
        }
    }
    if let Some(ref plan) = snapshot.plan {
        msg.push_str(&format!(
            "\n## Current Plan\nObjective: {}\n",
            plan.objective
        ));
        for task in &plan.tasks {
            msg.push_str(&format!(
                "- [{}] {} ({})\n",
                serde_json::to_string(&task.status)
                    .unwrap_or_default()
                    .trim_matches('"'),
                task.description,
                task.id
            ));
        }
    }
    msg.push_str(&format!(
        "\n## Decision Reasoning\n{}\n",
        decision.reasoning
    ));
    msg
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use exoskeleton_core::conversation::InMemoryConversationStore;
    use exoskeleton_core::llm::{LlmBackend, LlmResponse, StopReason};
    use exoskeleton_core::prompt::PromptRegistry;
    use exoskeleton_core::tick::ActionRecord;
    use exoskeleton_core::working_memory::WorkingMemoryOp;
    use exoskeleton_core::{LiveEvent, PlanTaskId, PlanTaskStatus};
    use exoskeleton_memory::{ApproximateTokenCounter, ContextCompiler};
    use exoskeleton_relationship::InMemoryRelationshipLedger;
    use exoskeleton_threads::{InMemoryThreadStore, ThreadRegistry};

    use super::super::KernelContext;
    use super::*;
    use crate::cognitive_engine::CognitiveHandler;
    use crate::inbox::InMemoryInbox;
    use crate::kernel::types::{ActionExecution, PlannedAction};
    use crate::llm::mock::MockLlmBackend;
    use crate::storage::StorageManager;

    fn success_execution(name: &str) -> ActionExecution {
        ActionExecution {
            action: PlannedAction {
                tool_name: name.into(),
                params: serde_json::json!({}),
                rationale: "test".into(),
                plan_task_id: None,
            },
            result: Ok(serde_json::json!({"ok": true})),
            record: ActionRecord {
                action_type: name.into(),
                target: "test".into(),
                receipt_ref: None,
                outcome: ActionOutcome::Success,
            },
        }
    }

    fn failure_execution(name: &str, error: &str) -> ActionExecution {
        ActionExecution {
            action: PlannedAction {
                tool_name: name.into(),
                params: serde_json::json!({}),
                rationale: "test".into(),
                plan_task_id: None,
            },
            result: Err(error.into()),
            record: ActionRecord {
                action_type: name.into(),
                target: "test".into(),
                receipt_ref: None,
                outcome: ActionOutcome::Failure,
            },
        }
    }

    // ── E1-T39: Heuristic fallback: all success ──
    #[test]
    fn heuristic_all_success() {
        let result = heuristic_reflect(&ActResult {
            executions: vec![
                success_execution("a"),
                success_execution("b"),
                success_execution("c"),
            ],
        });
        assert_eq!(result.action_success_rate, 1.0);
        assert!(result.concerns.is_empty());
        assert!(result.task_updates.is_empty());
        assert!(result.working_memory_ops.is_empty());
        assert!(!result.should_replan);
        assert!(result.llm_call_record.is_none());
    }

    // ── E1-T40: Heuristic fallback: mixed results ──
    #[test]
    fn heuristic_mixed_results() {
        let result = heuristic_reflect(&ActResult {
            executions: vec![
                success_execution("a"),
                success_execution("b"),
                failure_execution("c", "timeout"),
            ],
        });
        assert!((result.action_success_rate - 2.0 / 3.0).abs() < 0.01);
        assert!(result.concerns.is_empty());
    }

    // ── E1-T41: Heuristic fallback: no actions ──
    #[test]
    fn heuristic_no_actions() {
        let result = heuristic_reflect(&ActResult { executions: vec![] });
        assert!(result.action_success_rate.is_nan());
        assert!(result
            .observations
            .iter()
            .any(|o| o.contains("No actions taken")));
    }

    // ── E1-T42: Heuristic new fields defaults ──
    #[test]
    fn heuristic_new_fields_defaulted() {
        let result = heuristic_reflect(&ActResult {
            executions: vec![success_execution("a")],
        });
        assert!(result.task_updates.is_empty());
        assert!(result.working_memory_ops.is_empty());
        assert!(!result.should_replan);
        assert!(result.llm_call_record.is_none());
    }

    // ── E1-T48: parse_reflect_protocol unparseable -> fallback ──
    #[test]
    fn parse_reflect_protocol_unparseable() {
        let result = parse_reflect_protocol("This is just free text");
        assert_eq!(result.outcome_assessment, "This is just free text");
        assert!(result.task_updates.is_empty());
        assert!(result.observations.len() == 1);
    }

    // ── E1-T49: parse_reflect_protocol code-fenced JSON ──
    #[test]
    fn parse_reflect_protocol_code_fence() {
        let text = r#"Here is my reflection:

```json
{
    "outcome_assessment": "Actions succeeded",
    "task_updates": [],
    "working_memory_ops": [],
    "observations": ["All good"],
    "concerns": [],
    "should_replan": false
}
```
"#;
        let result = parse_reflect_protocol(text);
        assert_eq!(result.outcome_assessment, "Actions succeeded");
        assert_eq!(result.observations, vec!["All good"]);
    }

    // ── Heuristic: all failures ──
    #[test]
    fn heuristic_all_failures() {
        let result = heuristic_reflect(&ActResult {
            executions: vec![
                failure_execution("a", "err1"),
                failure_execution("b", "err2"),
                failure_execution("c", "err3"),
            ],
        });
        assert_eq!(result.action_success_rate, 0.0);
        assert!(result
            .concerns
            .iter()
            .any(|c| c.contains("All actions failed")));
    }

    // ── Heuristic: generates observation summary ──
    #[test]
    fn heuristic_generates_observation_summary() {
        let result = heuristic_reflect(&ActResult {
            executions: vec![
                success_execution("delay"),
                failure_execution("fs.write", "denied"),
            ],
        });
        assert!(result.observations[0].contains("2 actions"));
        assert!(result.observations[0].contains("1 succeeded"));
        assert!(result.observations[0].contains("1 failed"));
    }

    // ── LLM Reflect test helpers ──

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
            conversation_store: Arc::new(InMemoryConversationStore::new()),
            budget_tracker: None,
            tool_budget_gate: None,
            metrics: None,
            event_tx: tokio::sync::broadcast::channel::<LiveEvent>(16).0,
            prompt_registry: Arc::new(PromptRegistry::with_defaults()),
        }
    }

    fn test_kernel_with_budget(dir: &std::path::Path) -> KernelContext {
        let storage = StorageManager::open(dir).unwrap();
        let counter = Arc::new(ApproximateTokenCounter);
        let compiler = ContextCompiler::with_defaults(counter, 4000);
        let budget_store: Arc<dyn exoskeleton_core::budget::BudgetStore> =
            Arc::new(crate::budget::tracker::InMemoryBudgetStore::new());
        let config = exoskeleton_core::budget::CognitiveBudgetConfig::default();
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
            conversation_store: Arc::new(InMemoryConversationStore::new()),
            budget_tracker: Some(Arc::new(tokio::sync::Mutex::new(tracker))),
            tool_budget_gate: None,
            metrics: None,
            event_tx: tokio::sync::broadcast::channel::<LiveEvent>(16).0,
            prompt_registry: Arc::new(PromptRegistry::with_defaults()),
        }
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

    fn test_decision() -> DecisionResult {
        DecisionResult {
            reasoning: "Test reasoning".into(),
            actions: vec![PlannedAction {
                tool_name: "fs.write".into(),
                params: serde_json::json!({}),
                rationale: "write output".into(),
                plan_task_id: None,
            }],
            snapshot_delta: super::super::types::SnapshotDelta::default(),
            memory_notes: vec![],
            llm_call_record: exoskeleton_core::tick::LlmCallRecord {
                model: "mock".into(),
                tokens_in: 10,
                tokens_out: 5,
                cost_cents: 0.0,
                latency_ms: 100,
                response_artifact_ref: None,
            },
            response_artifact_id: exoskeleton_core::ArtifactId::from_content(b"test"),
        }
    }

    fn test_act_result() -> ActResult {
        ActResult {
            executions: vec![success_execution("fs.write")],
        }
    }

    // ── E1-T43: LLM reflect: mock returns task_updates ──
    #[test]
    fn llm_reflect_returns_task_updates() {
        let dir = tempfile::tempdir().unwrap();
        let kernel = test_kernel(dir.path());
        let task_id = PlanTaskId::new();
        let reflect_json = serde_json::json!({
            "outcome_assessment": "Task completed successfully",
            "task_updates": [{
                "task_id": task_id.to_string(),
                "new_status": "completed",
                "reason": "Action succeeded"
            }],
            "working_memory_ops": [],
            "observations": ["File written"],
            "concerns": [],
            "should_replan": false
        });
        let mock = Arc::new(MockLlmBackend::new(mock_response_with_content(
            serde_json::to_string(&reflect_json).unwrap(),
        )));
        let handler = CognitiveHandler::with_backends(
            Some(mock),
            None,
            LlmBackend::Local,
            kernel.artifact_store.clone(),
        );
        let snapshot = StateSnapshot::initial(kernel.vessel_id, kernel.mission.clone());
        let decision = test_decision();
        let act_result = test_act_result();
        let token = CancellationToken::new();

        let result = reflect(&handler, &kernel, &snapshot, &decision, &act_result, &token);
        assert_eq!(result.task_updates.len(), 1);
        assert_eq!(result.task_updates[0].task_id, task_id);
        assert_eq!(result.task_updates[0].new_status, PlanTaskStatus::Completed);
        assert!(result.llm_call_record.is_some());
    }

    // ── E1-T44: LLM reflect: mock returns working_memory_ops ──
    #[test]
    fn llm_reflect_returns_working_memory_ops() {
        let dir = tempfile::tempdir().unwrap();
        let kernel = test_kernel(dir.path());
        let reflect_json = serde_json::json!({
            "outcome_assessment": "Learned new info",
            "task_updates": [],
            "working_memory_ops": [
                {"op": "set", "key": "discovery", "value": "found config file"}
            ],
            "observations": ["Config found"],
            "concerns": [],
            "should_replan": false
        });
        let mock = Arc::new(MockLlmBackend::new(mock_response_with_content(
            serde_json::to_string(&reflect_json).unwrap(),
        )));
        let handler = CognitiveHandler::with_backends(
            Some(mock),
            None,
            LlmBackend::Local,
            kernel.artifact_store.clone(),
        );
        let snapshot = StateSnapshot::initial(kernel.vessel_id, kernel.mission.clone());
        let decision = test_decision();
        let act_result = test_act_result();
        let token = CancellationToken::new();

        let result = reflect(&handler, &kernel, &snapshot, &decision, &act_result, &token);
        assert_eq!(result.working_memory_ops.len(), 1);
        match &result.working_memory_ops[0] {
            WorkingMemoryOp::Set { key, value, .. } => {
                assert_eq!(key, "discovery");
                assert_eq!(value, "found config file");
            }
            other => panic!("expected Set, got: {other:?}"),
        }
    }

    // ── E1-T45: LLM reflect: mock returns should_replan=true ──
    #[test]
    fn llm_reflect_returns_should_replan() {
        let dir = tempfile::tempdir().unwrap();
        let kernel = test_kernel(dir.path());
        let reflect_json = serde_json::json!({
            "outcome_assessment": "Plan is no longer viable",
            "task_updates": [],
            "working_memory_ops": [],
            "observations": ["Unexpected error pattern"],
            "concerns": ["Plan assumptions invalidated"],
            "should_replan": true
        });
        let mock = Arc::new(MockLlmBackend::new(mock_response_with_content(
            serde_json::to_string(&reflect_json).unwrap(),
        )));
        let handler = CognitiveHandler::with_backends(
            Some(mock),
            None,
            LlmBackend::Local,
            kernel.artifact_store.clone(),
        );
        let snapshot = StateSnapshot::initial(kernel.vessel_id, kernel.mission.clone());
        let decision = test_decision();
        let act_result = test_act_result();
        let token = CancellationToken::new();

        let result = reflect(&handler, &kernel, &snapshot, &decision, &act_result, &token);
        assert!(result.should_replan);
        assert!(!result.concerns.is_empty());
    }

    // ── E1-T46: LLM reflect: mock fails → heuristic fallback ──
    #[test]
    fn llm_reflect_failure_falls_back_to_heuristic() {
        let dir = tempfile::tempdir().unwrap();
        let kernel = test_kernel(dir.path());
        let mock = Arc::new(MockLlmBackend::failing("503: service unavailable"));
        let handler = CognitiveHandler::with_backends(
            Some(mock),
            None,
            LlmBackend::Local,
            kernel.artifact_store.clone(),
        );
        let snapshot = StateSnapshot::initial(kernel.vessel_id, kernel.mission.clone());
        let decision = test_decision();
        let act_result = test_act_result();
        let token = CancellationToken::new();

        let result = reflect(&handler, &kernel, &snapshot, &decision, &act_result, &token);
        // Heuristic fallback: 1 success out of 1
        assert_eq!(result.action_success_rate, 1.0);
        // Heuristic doesn't produce LLM call record
        assert!(result.llm_call_record.is_none());
        // Heuristic defaults for new fields
        assert!(result.task_updates.is_empty());
        assert!(result.working_memory_ops.is_empty());
        assert!(!result.should_replan);
    }

    // ── E1-T47: LLM reflect: cancelled → heuristic fallback ──
    #[test]
    fn llm_reflect_cancelled_returns_heuristic() {
        let dir = tempfile::tempdir().unwrap();
        let kernel = test_kernel(dir.path());
        let mock = Arc::new(MockLlmBackend::new(mock_response_with_content(
            r#"{"outcome_assessment":"should not see this"}"#.into(),
        )));
        let handler = CognitiveHandler::with_backends(
            Some(mock.clone()),
            None,
            LlmBackend::Local,
            kernel.artifact_store.clone(),
        );
        let snapshot = StateSnapshot::initial(kernel.vessel_id, kernel.mission.clone());
        let decision = test_decision();
        let act_result = test_act_result();
        let token = CancellationToken::new();
        token.cancel(); // Cancel before reflect

        let result = reflect(&handler, &kernel, &snapshot, &decision, &act_result, &token);
        // Should return heuristic (no LLM call)
        assert!(result.llm_call_record.is_none());
        assert_eq!(result.action_success_rate, 1.0);
        // Mock should never have been called
        assert_eq!(mock.call_count(), 0);
    }

    // ── E1-T52: Reflect stores LLM response as artifact (I3) ──
    #[test]
    fn llm_reflect_stores_response_artifact() {
        let dir = tempfile::tempdir().unwrap();
        let kernel = test_kernel(dir.path());
        let reflect_json = serde_json::json!({
            "outcome_assessment": "OK",
            "task_updates": [],
            "working_memory_ops": [],
            "observations": ["Done"],
            "concerns": [],
            "should_replan": false
        });
        let mock = Arc::new(MockLlmBackend::new(mock_response_with_content(
            serde_json::to_string(&reflect_json).unwrap(),
        )));
        let handler = CognitiveHandler::with_backends(
            Some(mock),
            None,
            LlmBackend::Local,
            kernel.artifact_store.clone(),
        );
        let snapshot = StateSnapshot::initial(kernel.vessel_id, kernel.mission.clone());
        let decision = test_decision();
        let act_result = test_act_result();
        let token = CancellationToken::new();

        let result = reflect(&handler, &kernel, &snapshot, &decision, &act_result, &token);
        // LLM path should produce a call record with an artifact ref
        let record = result.llm_call_record.expect("should have LLM call record");
        let artifact_id = record
            .response_artifact_ref
            .expect("should have artifact ref");
        // Verify the artifact exists in the store
        let artifact = kernel.artifact_store.get(&artifact_id).unwrap();
        assert!(artifact.is_some(), "artifact should be persisted in store");
        let artifact = artifact.unwrap();
        assert_eq!(artifact.kind, exoskeleton_core::ArtifactKind::LlmResponse);
    }

    // ── E1-T53: Reflect records budget consumption ──
    #[test]
    fn llm_reflect_records_budget_consumption() {
        let dir = tempfile::tempdir().unwrap();
        let kernel = test_kernel_with_budget(dir.path());
        let reflect_json = serde_json::json!({
            "outcome_assessment": "Budget test",
            "task_updates": [],
            "working_memory_ops": [],
            "observations": ["Tracked"],
            "concerns": [],
            "should_replan": false
        });
        let mock = Arc::new(MockLlmBackend::new(mock_response_with_content(
            serde_json::to_string(&reflect_json).unwrap(),
        )));
        let handler = CognitiveHandler::with_backends(
            Some(mock),
            None,
            LlmBackend::Local,
            kernel.artifact_store.clone(),
        );
        let snapshot = StateSnapshot::initial(kernel.vessel_id, kernel.mission.clone());
        let decision = test_decision();
        let act_result = test_act_result();
        let token = CancellationToken::new();

        let result = reflect(&handler, &kernel, &snapshot, &decision, &act_result, &token);
        assert!(result.llm_call_record.is_some());

        // Verify budget tracker recorded consumption
        let tracker = kernel.budget_tracker.as_ref().unwrap();
        let guard = tracker.try_lock().unwrap();
        let remaining = guard.remaining_local_tokens();
        let budget = exoskeleton_core::budget::CognitiveBudgetConfig::default().local_token_budget;
        // The mock returns tokens_in=100, tokens_out=50 → 150 total consumed
        assert_eq!(
            remaining,
            budget - 150,
            "budget tracker should have recorded 150 tokens consumed"
        );
    }

    // ── E1-T54: No actions → skip LLM entirely ──
    #[test]
    fn reflect_no_actions_skips_llm() {
        let dir = tempfile::tempdir().unwrap();
        let kernel = test_kernel(dir.path());
        let mock = Arc::new(MockLlmBackend::new(mock_response_with_content(
            r#"{"outcome_assessment":"should not be called"}"#.into(),
        )));
        let handler = CognitiveHandler::with_backends(
            Some(mock.clone()),
            None,
            LlmBackend::Local,
            kernel.artifact_store.clone(),
        );
        let snapshot = StateSnapshot::initial(kernel.vessel_id, kernel.mission.clone());
        let decision = test_decision();
        let empty_act = ActResult { executions: vec![] };
        let token = CancellationToken::new();

        let result = reflect(&handler, &kernel, &snapshot, &decision, &empty_act, &token);
        // Should be heuristic (no LLM call)
        assert!(result.action_success_rate.is_nan());
        assert!(result.llm_call_record.is_none());
        // Mock should never have been called
        assert_eq!(mock.call_count(), 0);
    }
}

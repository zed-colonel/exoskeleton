//! Decide step — multi-turn LLM reasoning with introspection (E5-S1).
//!
//! The Decide step is an iterative loop. Each iteration:
//! 1. Calls the LLM backend directly (same deadlock-prevention pattern as before)
//! 2. Parses the response as `DecideTurn`
//! 3. If `Query`: resolves via `IntrospectionService`, appends results, continues
//! 4. If `Decide`: returns the `DecisionResult`
//! 5. Terminates at `max_decide_turns` or on budget exhaustion
//!
//! **Why direct backend call instead of LlmClient:** The LlmClient::call()
//! method locks the CognitiveEngineSlot and calls engine.run_until_idle().
//! But the master loop handler is already running inside the Cognitive AQ
//! dispatch loop — calling run_until_idle() from within a handler would
//! deadlock. Instead, the Decide step calls the HTTP backend directly.

use actionqueue_executor_local::CancellationToken;
use exoskeleton_core::llm::{LlmBackend, LlmMessage, LlmRequest, LlmRole};
use exoskeleton_core::tick::LlmCallRecord;
use exoskeleton_core::{Artifact, ArtifactKind, ExoError};

use super::types::{
    extract_json_from_code_fence, DecideTurn, DecisionProtocol, DecisionResult, OrientationResult,
    SnapshotDelta,
};
use super::KernelContext;
use crate::cognitive_engine::CognitiveHandler;
use crate::introspection::IntrospectionService;

/// Execute the Decide step: multi-turn LLM reasoning with introspection.
///
/// The loop runs synchronously within the Cognitive AQ handler. No AQ tasks
/// are spawned. Each turn is a separate HTTP call to the LLM backend.
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

    // 2. Build system prompt with available tools and introspection description
    let tools_description = build_tools_description(kernel);
    let introspection_description = build_introspection_description();
    let system_prompt =
        build_system_prompt(kernel, &tools_description, &introspection_description)?;

    // 3. Build user message from compiled context
    let user_message = orientation.compiled_context.prompt.clone();

    // 4. Resolve backend type — with escalation logic (Sprint 9)
    // handler_direct_llm_call handles the actual backend resolution and fallback.
    let backend_type = resolve_backend(handler, kernel, orientation);

    // 5. Multi-turn Decide loop
    let introspection = IntrospectionService::new(kernel);
    let max_turns = kernel.max_decide_turns;

    let mut messages: Vec<LlmMessage> = vec![LlmMessage {
        role: LlmRole::User,
        content: user_message,
    }];
    let mut llm_records: Vec<LlmCallRecord> = Vec::new();
    let mut all_response_text = String::new();

    for turn in 0..max_turns {
        // Check cancellation
        if cancellation.is_cancelled() {
            return Err(ExoError::LlmInvocation(
                "cancelled during Decide loop".into(),
            ));
        }

        // Check budget before each turn (except the first)
        if turn > 0 {
            if let Some(ref tracker) = kernel.budget_tracker {
                if let Ok(guard) = tracker.lock() {
                    if guard.budget_status(u64::MAX).local_tokens_remaining == 0 {
                        tracing::info!(turn, "budget exhausted, ending Decide loop");
                        break;
                    }
                }
            }
        }

        // Build LLM request for this turn
        let request = LlmRequest {
            backend: Some(backend_type),
            system_prompt: Some(system_prompt.clone()),
            messages: messages.clone(),
            max_output_tokens: kernel.max_output_tokens,
            temperature: Some(0.7),
            stop_sequences: vec![],
        };

        // Unified LLM call (H-1 pattern, E8-S1)
        let result =
            crate::llm::direct::handler_direct_llm_call(handler, kernel, &request, cancellation)?;
        let llm_response = result.response;
        llm_records.push(result.llm_call_record);
        all_response_text.push_str(&llm_response.content);
        all_response_text.push('\n');

        // Parse this turn
        let turn_result = parse_decide_turn(&llm_response.content);

        match turn_result {
            DecideTurn::Query { queries } => {
                tracing::info!(
                    turn,
                    query_count = queries.len(),
                    "Decide turn: introspection queries"
                );

                // Resolve each query
                let mut results = Vec::new();
                for q in &queries {
                    let result = introspection
                        .query(q)
                        .unwrap_or_else(|e| serde_json::json!({"error": e.to_string()}));
                    results.push(serde_json::json!({
                        "query": q,
                        "result": result,
                    }));
                }

                // Append assistant turn + introspection results
                messages.push(LlmMessage {
                    role: LlmRole::Assistant,
                    content: llm_response.content.clone(),
                });
                messages.push(LlmMessage {
                    role: LlmRole::User,
                    content: format!(
                        "Introspection results:\n```json\n{}\n```\n\n\
                         Continue your analysis and provide your final decision.",
                        serde_json::to_string_pretty(&results).unwrap_or_else(|_| "[]".into()),
                    ),
                });
            }
            DecideTurn::Decide(protocol) => {
                tracing::info!(
                    turn,
                    action_count = protocol.actions.len(),
                    "Decide turn: final decision"
                );
                return build_decision_result(
                    handler,
                    kernel,
                    protocol,
                    llm_records,
                    &all_response_text,
                );
            }
        }
    }

    // Max turns reached — produce fallback decision
    tracing::warn!(max_turns, "Decide loop reached max turns, forcing decision");
    let fallback = DecisionProtocol {
        reasoning: all_response_text.clone(),
        inner_loop_requested: false,
        reply: None,
        plan_update: None,
        working_memory_ops: None,
        vessel_mode_request: None,
        actions: vec![],
        memory_notes: vec![],
        watch_proposals: vec![],
    };
    build_decision_result(handler, kernel, fallback, llm_records, &all_response_text)
}

/// Build the final DecisionResult from the protocol and accumulated records.
fn build_decision_result(
    handler: &CognitiveHandler,
    kernel: &KernelContext,
    protocol: DecisionProtocol,
    llm_records: Vec<LlmCallRecord>,
    full_response_text: &str,
) -> Result<DecisionResult, ExoError> {
    // Store full response as artifact (I3)
    let response_artifact = Artifact::new(
        ArtifactKind::LlmResponse,
        full_response_text.as_bytes().to_vec(),
        "text/plain".into(),
    );
    let response_artifact_id = handler.artifact_store.put(&response_artifact)?;

    // Store decision as artifact (I3)
    let decision_artifact = Artifact::from_json(ArtifactKind::Decision, &protocol)?;
    kernel.artifact_store.put(&decision_artifact)?;

    // Merge LLM call records into single summary
    let mut merged_record = LlmCallRecord::merge(&llm_records);
    merged_record.response_artifact_ref = Some(response_artifact_id.clone());

    Ok(DecisionResult {
        reasoning: protocol.reasoning.clone(),
        reply: protocol.reply,
        actions: protocol.actions,
        snapshot_delta: SnapshotDelta {
            plan_update: protocol.plan_update,
            working_memory_ops: protocol.working_memory_ops,
        },
        memory_notes: protocol.memory_notes,
        llm_call_record: merged_record,
        response_artifact_id,
        watch_proposals: protocol.watch_proposals,
        inner_loop_requested: protocol.inner_loop_requested,
        vessel_mode_request: protocol.vessel_mode_request,
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

    // Lock tracker; if poisoned, use default backend (poison recovery)
    let guard = match tracker.lock() {
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
    let mut tools = match guard {
        Ok(slot) => match slot.as_ref() {
            Some(host) => host.list_capabilities(),
            None => vec![],
        },
        Err(_) => return "Tools unavailable (slot locked).".into(),
    };

    // Append virtual tool descriptors (LLM sees these as normal tools)
    let mut virtual_descs = super::virtual_tools::virtual_tool_descriptors();
    // Only include peer.resolve if observatory_url is configured
    if kernel.observatory_url.is_none() {
        virtual_descs.retain(|d| d.name != super::virtual_tools::PEER_RESOLVE);
    }
    tools.extend(virtual_descs);

    // Filter out signal primitives — the LLM should NOT see these
    tools.retain(|d| d.name != "signal.await" && d.name != "signal.emit");

    if tools.is_empty() {
        "No tools available.".into()
    } else {
        tools
            .iter()
            .map(|d| {
                let mut line = format!("- {}: {}", d.name, d.description);
                if let Some(schema) = &d.input_schema {
                    line.push_str(&format!("\n  params: {}", schema));
                }
                line
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

fn build_system_prompt(
    kernel: &KernelContext,
    tools: &str,
    introspection: &str,
) -> Result<String, ExoError> {
    // Conditionally include inner loop guidance (E8-S1)
    let inner_loop_guidance = if kernel.inner_loop_config.enabled {
        "\n## Interactive Tool Loop\n\n\
         You have access to an interactive tool loop. When your task requires multiple \
         sequential tool calls where each step depends on the result of the previous \
         one (e.g., reading a file, editing it, then verifying the change), set \
         `inner_loop_requested: true` in your response. This activates a rapid \
         tool-feedback cycle where you can call tools iteratively within this tick.\n\n\
         Set `inner_loop_requested: false` (or omit it) for single-shot actions that \
         don't need iterative feedback — routine actions, sending messages, writing \
         memory notes, etc.\n"
    } else {
        ""
    };

    let base = kernel.prompt_registry.resolve(
        "decide-system",
        &[
            ("vessel_id", &kernel.vessel_id.to_string()),
            ("mission", &kernel.mission),
            ("tools", tools),
        ],
    )?;
    Ok(format!("{base}{inner_loop_guidance}\n\n{introspection}"))
}

/// Generate the introspection tools section for the system prompt.
fn build_introspection_description() -> String {
    r#"---

## Internal State Queries (NOT tools — do NOT put these in the actions array)

Before making your decision, you can query your own internal state. This uses a
DIFFERENT format from tool actions. Respond with a JSON object of type "query":

```json
{"type": "query", "queries": [{"query": "tick_history", "limit": 10}]}
```

This is NOT a tool action. Do not use query names in the "actions" array.
Queries are resolved immediately and the results are returned to you in the
same conversation turn.

Available queries:
- tick_history(limit): Recent ticks with action counts, success rates, token usage
- tick_detail(tick_id): Full detail for a specific tick
- event_history(event_type?, limit): Events filtered by optional type
- trust_scores: Current trust levels for all known principals
- trust_history(principal_id, limit): Trust changes over time for one principal
- budget_status: Current budget dimensions with consumed/remaining
- thread_status: All threads with statuses and recent output summaries
- memory_search(topic?, tags?): Search long-term memory by topic or tags
- connector_details(name): Full descriptor for a named connector
- watch_list: Active watches

After receiving results, continue reasoning and provide your final decision.
When ready, respond with your decision JSON (the format with "reasoning", "actions", etc.).
You may issue up to 4 query rounds before your final decision."#
        .to_string()
}

/// Parse a single turn of the multi-turn Decide loop.
///
/// Attempts to parse as DecideTurn (tagged). On failure, falls back to
/// DecisionProtocol (untagged) for backward compatibility. On complete
/// failure, returns a no-action decision with the raw text as reasoning.
fn parse_decide_turn(response_text: &str) -> DecideTurn {
    let json_text = extract_json_from_code_fence(response_text).unwrap_or(response_text);

    // Attempt 1: parse as tagged DecideTurn
    if let Ok(turn) = serde_json::from_str::<DecideTurn>(json_text) {
        return turn;
    }

    // Attempt 2: parse as untagged DecisionProtocol (backward compat)
    if let Ok(protocol) = serde_json::from_str::<DecisionProtocol>(json_text) {
        return DecideTurn::Decide(protocol);
    }

    // Attempt 3: fallback no-action decision
    tracing::warn!("LLM returned unparseable response, treating as no-action tick");
    DecideTurn::Decide(DecisionProtocol {
        reasoning: response_text.to_string(),
        inner_loop_requested: false,
        reply: None,
        plan_update: None,
        working_memory_ops: None,
        vessel_mode_request: None,
        actions: vec![],
        memory_notes: vec![],
        watch_proposals: vec![],
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use exoskeleton_core::artifact::ArtifactKind;
    use exoskeleton_core::conversation::InMemoryConversationStore;
    use exoskeleton_core::llm::{LlmBackend, LlmResponse, StopReason};
    use exoskeleton_core::prompt::PromptRegistry;
    use exoskeleton_core::{ArtifactStore, LiveEvent};
    use exoskeleton_memory::{ApproximateTokenCounter, ContextCompiler};
    use exoskeleton_relationship::InMemoryRelationshipLedger;
    use exoskeleton_threads::{InMemoryThreadStore, ThreadRegistry};

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
            observatory_url: None,
            observatory_token: None,
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
        // Use legacy string format to verify backward-compatible deserialization
        r#"{
            "reasoning": "I need to write a file",
            "plan_update": "Write output",
            "working_context_update": "Writing file",
            "actions": [{"tool_name": "fs.write", "params": {"path": "/tmp/out.txt"}, "rationale": "Write output"}],
            "memory_notes": ["File written"]
        }"#
        .to_string()
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
        assert!(result.snapshot_delta.plan_update.is_some());
        assert!(result.snapshot_delta.working_memory_ops.is_some());
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
        assert!(result.snapshot_delta.working_memory_ops.is_none());
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
        let introspection = build_introspection_description();
        let prompt =
            build_system_prompt(&kernel_ctx, "- fs.write: Write a file", &introspection).unwrap();
        assert!(prompt.contains("Available tools"));
        assert!(prompt.contains("fs.write: Write a file"));
        assert!(prompt.contains("test mission"));
        assert!(prompt.contains("Internal State Queries"));
        assert!(prompt.contains("NOT tools"));
    }

    // E5S1-T18: System prompt contains introspection tool descriptions
    #[test]
    fn decide_tools_description_includes_introspection() {
        let kernel_ctx = {
            let dir = tempfile::tempdir().unwrap();
            test_kernel(dir.path())
        };
        let introspection = build_introspection_description();
        let prompt =
            build_system_prompt(&kernel_ctx, "- fs.write: Write a file", &introspection).unwrap();
        assert!(prompt.contains("Internal State Queries"));
        assert!(prompt.contains("tick_history"));
        assert!(prompt.contains("trust_scores"));
        assert!(prompt.contains("budget_status"));
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
            conversation_store: Arc::new(InMemoryConversationStore::new()),
            budget_tracker: Some(Arc::new(std::sync::Mutex::new(tracker))),
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
            observatory_url: None,
            observatory_token: None,
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
            let mut guard = tracker.lock().unwrap();
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
            let mut guard = tracker.lock().unwrap();
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
            let mut guard = tracker.lock().unwrap();
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
        exoskeleton_threads::register_builtin_threads(
            &kernel.thread_registry,
            &PromptRegistry::with_defaults(),
            None,
        )
        .unwrap();

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
        exoskeleton_threads::register_builtin_threads(
            &kernel.thread_registry,
            &PromptRegistry::with_defaults(),
            None,
        )
        .unwrap();

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

        exoskeleton_threads::register_builtin_threads(
            &kernel.thread_registry,
            &PromptRegistry::with_defaults(),
            None,
        )
        .unwrap();

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

        exoskeleton_threads::register_builtin_threads(
            &kernel.thread_registry,
            &PromptRegistry::with_defaults(),
            None,
        )
        .unwrap();

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

    // ── Multi-turn Decide tests (E5-S1) ──

    fn test_handler_with_sequence(
        responses: Vec<String>,
        artifact_store: Arc<dyn ArtifactStore>,
    ) -> CognitiveHandler {
        let llm_responses: Vec<LlmResponse> = responses
            .into_iter()
            .map(mock_response_with_content)
            .collect();
        let mock = Arc::new(crate::llm::mock::MockSequenceLlmBackend::new(llm_responses));
        CognitiveHandler::with_backends(Some(mock), None, LlmBackend::Local, artifact_store)
    }

    // E5S1-T19: parse_decide_turn parses query
    #[test]
    fn parse_decide_turn_query() {
        let json = r#"{"type":"query","queries":[{"query":"tick_history","limit":5}]}"#;
        let result = parse_decide_turn(json);
        match result {
            DecideTurn::Query { queries } => {
                assert_eq!(queries.len(), 1);
            }
            other => panic!("expected Query, got: {other:?}"),
        }
    }

    // E5S1-T20: parse_decide_turn parses decide
    #[test]
    fn parse_decide_turn_decide() {
        let json =
            r#"{"type":"decide","reasoning":"Based on analysis","actions":[],"memory_notes":[]}"#;
        let result = parse_decide_turn(json);
        match result {
            DecideTurn::Decide(protocol) => {
                assert_eq!(protocol.reasoning, "Based on analysis");
            }
            other => panic!("expected Decide, got: {other:?}"),
        }
    }

    // E5S1-T21: parse_decide_turn handles untagged fallback
    #[test]
    fn parse_decide_turn_untagged_fallback() {
        let json = r#"{"reasoning":"idle tick","actions":[],"memory_notes":[]}"#;
        let result = parse_decide_turn(json);
        match result {
            DecideTurn::Decide(protocol) => {
                assert_eq!(protocol.reasoning, "idle tick");
            }
            other => panic!("expected Decide (fallback), got: {other:?}"),
        }
    }

    // E5S1-T13: Query turn → results appended → second turn produces decision
    #[test]
    fn decide_multi_turn_introspection() {
        let dir = tempfile::tempdir().unwrap();
        let kernel = test_kernel(dir.path());
        let handler = test_handler_with_sequence(
            vec![
                // Turn 1: query
                r#"{"type":"query","queries":[{"query":"budget_status"}]}"#.into(),
                // Turn 2: decide
                r#"{"type":"decide","reasoning":"After reviewing budget...","actions":[],"memory_notes":[]}"#.into(),
            ],
            kernel.artifact_store.clone(),
        );
        let orientation = test_orientation();
        let cancel = CancellationToken::new();

        let result = decide(&handler, &kernel, &orientation, &cancel).unwrap();
        assert!(result.reasoning.contains("After reviewing"));
        assert_eq!(result.llm_call_record.turns, 2);
    }

    // E5S1-T17: DecisionProtocol without `type` field → single turn backward compat
    #[test]
    fn decide_single_turn_backward_compat() {
        let dir = tempfile::tempdir().unwrap();
        let kernel = test_kernel(dir.path());
        let response = mock_response_with_content(valid_decision_json());
        let handler = test_handler_with_mock(response, kernel.artifact_store.clone());
        let orientation = test_orientation();
        let token = CancellationToken::new();

        let result = decide(&handler, &kernel, &orientation, &token).unwrap();

        assert_eq!(result.reasoning, "I need to write a file");
        assert_eq!(result.llm_call_record.turns, 1);
    }

    // E5S1-T15: Terminates at max_turns, produces fallback decision
    #[test]
    fn decide_multi_turn_max_turns() {
        let dir = tempfile::tempdir().unwrap();
        let mut kernel = test_kernel(dir.path());
        kernel.max_decide_turns = 2; // Low limit

        let handler = test_handler_with_sequence(
            vec![
                // Keep querying, never deciding
                r#"{"type":"query","queries":[{"query":"budget_status"}]}"#.into(),
                r#"{"type":"query","queries":[{"query":"thread_status"}]}"#.into(),
            ],
            kernel.artifact_store.clone(),
        );
        let orientation = test_orientation();
        let cancel = CancellationToken::new();

        let result = decide(&handler, &kernel, &orientation, &cancel).unwrap();
        // Fallback decision has no actions
        assert!(result.actions.is_empty());
        assert_eq!(result.llm_call_record.turns, 2);
    }

    // E5S1-T16: Budget exhaustion mid-loop terminates gracefully with fallback
    #[test]
    fn decide_multi_turn_budget_exhaustion() {
        let dir = tempfile::tempdir().unwrap();
        // Budget of 100 local tokens — mock response costs 150 (100 in + 50 out),
        // so after turn 0 the budget is exhausted.
        let mut config = default_budget_config();
        config.local_token_budget = 100;
        let mut kernel = test_kernel_with_budget(dir.path(), config);
        kernel.max_decide_turns = 5;

        let handler = test_handler_with_sequence(
            vec![
                // Turn 0: query (consumes 150 tokens → exhausts budget)
                r#"{"type":"query","queries":[{"query":"budget_status"}]}"#.into(),
                // Turn 1 would be a decide, but budget check should break before call
                r#"{"type":"decide","reasoning":"Should not reach","actions":[],"memory_notes":[]}"#
                    .into(),
            ],
            kernel.artifact_store.clone(),
        );
        let orientation = test_orientation();
        let cancel = CancellationToken::new();

        let result = decide(&handler, &kernel, &orientation, &cancel).unwrap();
        // Fallback decision: no actions, only 1 LLM turn actually completed
        assert!(result.actions.is_empty());
        assert_eq!(result.llm_call_record.turns, 1);
    }

    // E5S1-T14: Action tools in decision → PlannedActions (not resolved inline)
    #[test]
    fn decide_multi_turn_action_deferred() {
        let dir = tempfile::tempdir().unwrap();
        let kernel = test_kernel(dir.path());
        let handler = test_handler_with_sequence(
            vec![
                r#"{"type":"decide","reasoning":"Need to write a file","actions":[{"tool_name":"fs.write","params":{"path":"/tmp/x"},"rationale":"write"}],"memory_notes":[]}"#.into(),
            ],
            kernel.artifact_store.clone(),
        );
        let orientation = test_orientation();
        let cancel = CancellationToken::new();

        let result = decide(&handler, &kernel, &orientation, &cancel).unwrap();
        assert_eq!(result.actions.len(), 1);
        assert_eq!(result.actions[0].tool_name, "fs.write");
    }

    // T31: Virtual tools appear in decide prompt; signal primitives excluded
    #[test]
    fn virtual_tools_in_decide_prompt() {
        // Case 1: observatory_url = None → agent.ask_user present, peer.resolve absent
        let dir = tempfile::tempdir().unwrap();
        let kernel = test_kernel(dir.path());
        assert!(kernel.observatory_url.is_none());

        let desc = build_tools_description(&kernel);
        assert!(
            desc.contains("agent.ask_user"),
            "expected agent.ask_user in tools description, got: {desc}"
        );
        assert!(
            !desc.contains("peer.resolve"),
            "peer.resolve should be excluded when observatory_url is None, got: {desc}"
        );
        assert!(
            !desc.contains("signal.await"),
            "signal.await should never appear in tools description, got: {desc}"
        );
        assert!(
            !desc.contains("signal.emit"),
            "signal.emit should never appear in tools description, got: {desc}"
        );

        // Case 2: observatory_url = Some → peer.resolve included
        let dir2 = tempfile::tempdir().unwrap();
        let mut kernel2 = test_kernel(dir2.path());
        kernel2.observatory_url = Some("http://observatory:8080".into());

        let desc2 = build_tools_description(&kernel2);
        assert!(
            desc2.contains("agent.ask_user"),
            "expected agent.ask_user in tools description, got: {desc2}"
        );
        assert!(
            desc2.contains("peer.resolve"),
            "expected peer.resolve when observatory_url is set, got: {desc2}"
        );
        assert!(
            !desc2.contains("signal.await"),
            "signal.await should never appear in tools description, got: {desc2}"
        );
        assert!(
            !desc2.contains("signal.emit"),
            "signal.emit should never appear in tools description, got: {desc2}"
        );
    }
}

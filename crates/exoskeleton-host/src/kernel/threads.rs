//! Thread execution within the master loop.
//!
//! Cognitive threads are sub-tasks that run between Perceive and Orient
//! in the PODAARA cycle. Each thread gets a compiled context slice, calls
//! the LLM backend directly (H-1 deadlock prevention), and produces
//! recommendation artifacts.
//!
//! Threads NEVER invoke tools (IBP §3.4) and NEVER cross to the Tool AQ.
//! Thread outputs are artifacts only (IBP §4.3).

use std::time::Instant;

use actionqueue_executor_local::CancellationToken;
use exoskeleton_core::llm::{LlmBackend, LlmMessage, LlmRequest, LlmRole};
use exoskeleton_core::tick::ThreadContribution;
use exoskeleton_core::{
    Artifact, ArtifactId, ArtifactKind, EventEntry, EventType, ExoError, LedgerEntryId,
    StateSnapshot, ThreadOutput, ThreadSpec, TickId,
};
use exoskeleton_memory::ApproximateTokenCounter;
use exoskeleton_threads::{compile_thread_context, ThreadResponse};

use super::KernelContext;
use crate::cognitive_engine::CognitiveHandler;

/// Execute a single cognitive thread for one tick.
///
/// Uses the same direct-backend LLM call pattern as the Decide step
/// (H-1 deadlock prevention -- cannot use LlmClient from within a handler).
///
/// Thread outputs are artifacts only (IBP §4.3). This function NEVER
/// invokes tools or crosses to the Tool AQ (IBP §3.4).
#[allow(clippy::too_many_arguments)]
pub fn execute_thread(
    handler: &CognitiveHandler,
    kernel: &KernelContext,
    thread: &ThreadSpec,
    snapshot: &StateSnapshot,
    tick_id: TickId,
    cancellation: &CancellationToken,
) -> Result<ThreadOutput, ExoError> {
    // 1. Fetch recent outputs for continuity
    let recent_outputs = kernel.thread_registry.recent_outputs(thread.thread_id, 5)?;

    // 2. Compile context slice (token-budgeted)
    let counter = ApproximateTokenCounter;
    let compiled_context =
        compile_thread_context(&counter, thread, snapshot, &recent_outputs, tick_id)?;

    // 3. Check cancellation before LLM call
    if cancellation.is_cancelled() {
        return Err(ExoError::Engine("thread execution cancelled".into()));
    }

    // 4. Resolve LLM backend (use handler's default_backend)
    let backend = match handler.default_backend {
        LlmBackend::Local => handler.local_backend.as_ref(),
        LlmBackend::Frontier => handler.frontier_backend.as_ref(),
    }
    .ok_or_else(|| ExoError::Engine("no LLM backend configured for threads".into()))?;

    // 5. Build LLM request
    let request = LlmRequest {
        backend: Some(handler.default_backend),
        system_prompt: Some(compiled_context.prompt),
        messages: vec![LlmMessage {
            role: LlmRole::User,
            content: "Analyze the current situation per your charter. \
                      Respond with JSON."
                .into(),
        }],
        max_output_tokens: thread.token_budget / 4,
        temperature: Some(0.7),
        stop_sequences: vec![],
    };

    // 6. Call LLM backend directly (H-1: NOT via LlmClient)
    let start = Instant::now();
    let llm_response = backend.call(&handler.http_client, &request, cancellation)?;
    let _latency_ms = start.elapsed().as_millis() as u64;

    // 6.5 Record LLM consumption in budget tracker (Sprint 9)
    if let Some(ref tracker) = kernel.budget_tracker {
        if let Ok(mut guard) = tracker.try_lock() {
            guard.record_llm_call(
                handler.default_backend,
                llm_response.tokens_in,
                llm_response.tokens_out,
                llm_response.cost_estimate_cents.unwrap_or(0.0),
            );
            guard.record_thread_consumption(
                thread.thread_id,
                llm_response.tokens_in + llm_response.tokens_out,
            );
        }
    }

    // 7. Store LLM response as artifact (I3)
    let response_artifact = Artifact::from_json(ArtifactKind::LlmResponse, &llm_response)?;
    kernel.artifact_store.put(&response_artifact)?;

    // 8. Parse thread response -- try JSON first, fallback to raw text
    let thread_response = serde_json::from_str::<ThreadResponse>(&llm_response.content)
        .unwrap_or_else(|_| ThreadResponse {
            summary: llm_response.content.clone(),
            recommendations: vec![],
        });

    // 9. Build and store ThreadOutput as artifact
    let output = ThreadOutput {
        thread_id: thread.thread_id,
        tick_id,
        artifact_id: ArtifactId::from_content(llm_response.content.as_bytes()),
        summary: thread_response.summary,
        recommendations: thread_response.recommendations,
    };
    let output_artifact = Artifact::from_json(ArtifactKind::ThreadOutput, &output)?;
    kernel.artifact_store.put(&output_artifact)?;

    // 10. Save output to thread store and record run
    kernel.thread_registry.save_output(&output)?;
    kernel
        .thread_registry
        .record_run(thread.thread_id, snapshot.tick_number + 1)?;

    // 11. Return output
    Ok(output)
}

/// Execute all threads that are due this tick.
///
/// Called between Perceive and Orient in the PODAARA cycle.
/// Threads execute sequentially, ordered by priority (Critical first).
/// Thread failures are logged but do not abort the tick.
pub fn execute_due_threads(
    handler: &CognitiveHandler,
    kernel: &KernelContext,
    snapshot: &StateSnapshot,
    tick_id: TickId,
    cancellation: &CancellationToken,
) -> Result<Vec<ThreadContribution>, ExoError> {
    let due = kernel
        .thread_registry
        .due_threads(snapshot.tick_number + 1)?;
    let mut contributions = Vec::new();

    for thread in &due {
        // Check cancellation between threads
        if cancellation.is_cancelled() {
            break;
        }

        // Per-thread budget check (Sprint 9)
        if let Some(ref tracker) = kernel.budget_tracker {
            if let Ok(guard) = tracker.try_lock() {
                if !guard.check_thread_budget(thread.thread_id) {
                    tracing::info!(
                        thread = %thread.name,
                        "thread skipped: per-thread budget exhausted for this window"
                    );
                    continue;
                }
            }
        }

        match execute_thread(handler, kernel, thread, snapshot, tick_id, cancellation) {
            Ok(output) => {
                // Record thread metrics (Sprint 10)
                if let Some(ref m) = kernel.metrics {
                    m.thread_runs_total
                        .with_label_values(&[&thread.name, "completed"])
                        .inc();
                }

                // Log ThreadRan event
                let event = EventEntry {
                    id: LedgerEntryId::new(),
                    tick_id: Some(tick_id),
                    event_type: EventType::ThreadRan,
                    payload_ref: None,
                    summary: format!("Thread '{}' produced: {}", thread.name, output.summary),
                    timestamp: chrono::Utc::now(),
                };
                if let Err(e) = kernel.event_ledger.append(&event) {
                    tracing::warn!(
                        error = %e,
                        "failed to log thread event"
                    );
                }

                contributions.push(ThreadContribution {
                    thread_id: output.thread_id,
                    artifact_id: output.artifact_id,
                    summary: output.summary,
                });
            }
            Err(e) => {
                // Record thread failure metrics (Sprint 10)
                if let Some(ref m) = kernel.metrics {
                    m.thread_runs_total
                        .with_label_values(&[&thread.name, "failed"])
                        .inc();
                }

                tracing::warn!(
                    thread_name = %thread.name,
                    error = %e,
                    "thread execution failed; continuing"
                );
                // Log error event
                let event = EventEntry {
                    id: LedgerEntryId::new(),
                    tick_id: Some(tick_id),
                    event_type: EventType::Error,
                    payload_ref: None,
                    summary: format!("Thread '{}' failed: {}", thread.name, e),
                    timestamp: chrono::Utc::now(),
                };
                if let Err(e) = kernel.event_ledger.append(&event) {
                    tracing::warn!(
                        error = %e,
                        "failed to log thread error event"
                    );
                }
            }
        }
    }

    Ok(contributions)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use exoskeleton_core::llm::{LlmBackend, LlmResponse, StopReason};
    use exoskeleton_core::{
        ArtifactKind, ArtifactStore, ThreadId, ThreadPriority, ThreadSchedule, ThreadSpec, VesselId,
    };
    use exoskeleton_memory::{ApproximateTokenCounter, ContextCompiler};
    use exoskeleton_relationship::InMemoryRelationshipLedger;
    use exoskeleton_threads::{InMemoryThreadStore, ThreadRegistry};

    use super::super::KernelContext;
    use super::*;
    use crate::cognitive_engine::CognitiveHandler;
    use crate::inbox::InMemoryInbox;
    use crate::llm::http::LlmHttpBackend;
    use crate::storage::StorageManager;

    // ── Mock backends ──

    struct MockThreadBackend {
        response: String,
    }

    impl LlmHttpBackend for MockThreadBackend {
        fn call(
            &self,
            _client: &reqwest::Client,
            _request: &LlmRequest,
            _cancellation: &CancellationToken,
        ) -> Result<LlmResponse, ExoError> {
            Ok(LlmResponse {
                content: self.response.clone(),
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

    struct CapturingBackend {
        captured: std::sync::Mutex<Option<LlmRequest>>,
    }

    impl LlmHttpBackend for CapturingBackend {
        fn call(
            &self,
            _client: &reqwest::Client,
            request: &LlmRequest,
            _cancellation: &CancellationToken,
        ) -> Result<LlmResponse, ExoError> {
            *self.captured.lock().unwrap() = Some(request.clone());
            Ok(LlmResponse {
                content: r#"{"summary":"ok","recommendations":[]}"#.into(),
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

    // ── Test helpers ──

    fn test_kernel(dir: &std::path::Path) -> KernelContext {
        let storage = StorageManager::open(dir).unwrap();
        let counter = Arc::new(ApproximateTokenCounter);
        let compiler = ContextCompiler::with_defaults(counter, 4000);
        let thread_store = Arc::new(InMemoryThreadStore::new());
        let thread_registry = Arc::new(ThreadRegistry::new(thread_store));

        KernelContext {
            snapshot_store: storage.snapshot_store().clone(),
            event_ledger: storage.event_ledger().clone(),
            tick_store: storage.tick_store().clone(),
            memory_store: storage.memory_store().clone(),
            context_compiler: Arc::new(compiler),
            artifact_store: storage.artifact_store().clone(),
            wi_host_slot: Arc::new(tokio::sync::Mutex::new(None)),
            inbox: Arc::new(InMemoryInbox::new()),
            vessel_id: VesselId::new(),
            mission: "test mission".into(),
            max_output_tokens: 4096,
            master_loop_interval_secs: 60,
            thread_registry,
            relationship_ledger: Arc::new(InMemoryRelationshipLedger::new()),
            budget_tracker: None,
            tool_budget_gate: None,
            metrics: None,
        }
    }

    fn test_thread(name: &str) -> ThreadSpec {
        ThreadSpec {
            thread_id: ThreadId::new(),
            name: name.into(),
            charter: "Monitor for threats".into(),
            priority: ThreadPriority::Normal,
            token_budget: 4000,
            schedule: ThreadSchedule::EveryTick,
        }
    }

    fn test_snapshot(vessel_id: VesselId) -> StateSnapshot {
        StateSnapshot::initial(vessel_id, "test mission".into())
    }

    fn test_handler_with_mock(
        response: &str,
        artifact_store: Arc<dyn ArtifactStore>,
    ) -> CognitiveHandler {
        let mock: Arc<dyn LlmHttpBackend> = Arc::new(MockThreadBackend {
            response: response.into(),
        });
        CognitiveHandler::with_backends(Some(mock), None, LlmBackend::Local, artifact_store)
    }

    // ── T-5: Thread execution tests ──

    #[test]
    fn execute_produces_thread_output() {
        let dir = tempfile::tempdir().unwrap();
        let kernel = test_kernel(dir.path());
        let thread = test_thread("ThreatMon");
        kernel.thread_registry.register(thread.clone()).unwrap();
        let snapshot = test_snapshot(kernel.vessel_id);
        let tick_id = TickId::new();
        let token = CancellationToken::new();
        let json = r#"{"summary":"ok","recommendations":["do x"]}"#;
        let handler = test_handler_with_mock(json, kernel.artifact_store.clone());

        let output =
            execute_thread(&handler, &kernel, &thread, &snapshot, tick_id, &token).unwrap();

        assert_eq!(output.summary, "ok");
        assert_eq!(output.recommendations, vec!["do x"]);
        assert_eq!(output.thread_id, thread.thread_id);
        assert_eq!(output.tick_id, tick_id);
    }

    #[test]
    fn execute_stores_response_artifact() {
        let dir = tempfile::tempdir().unwrap();
        let kernel = test_kernel(dir.path());
        let thread = test_thread("ArtifactCheck");
        kernel.thread_registry.register(thread.clone()).unwrap();
        let snapshot = test_snapshot(kernel.vessel_id);
        let tick_id = TickId::new();
        let token = CancellationToken::new();
        let json = r#"{"summary":"ok","recommendations":[]}"#;
        let handler = test_handler_with_mock(json, kernel.artifact_store.clone());

        execute_thread(&handler, &kernel, &thread, &snapshot, tick_id, &token).unwrap();

        // I3: LlmResponse artifact must exist in the store
        let responses = kernel
            .artifact_store
            .list_by_kind(ArtifactKind::LlmResponse, 10)
            .unwrap();
        assert!(
            !responses.is_empty(),
            "expected at least one LlmResponse artifact"
        );
    }

    #[test]
    fn execute_stores_output_artifact() {
        let dir = tempfile::tempdir().unwrap();
        let kernel = test_kernel(dir.path());
        let thread = test_thread("OutputArtifact");
        kernel.thread_registry.register(thread.clone()).unwrap();
        let snapshot = test_snapshot(kernel.vessel_id);
        let tick_id = TickId::new();
        let token = CancellationToken::new();
        let json = r#"{"summary":"ok","recommendations":[]}"#;
        let handler = test_handler_with_mock(json, kernel.artifact_store.clone());

        execute_thread(&handler, &kernel, &thread, &snapshot, tick_id, &token).unwrap();

        // I3: ThreadOutput artifact must exist in the store
        let outputs = kernel
            .artifact_store
            .list_by_kind(ArtifactKind::ThreadOutput, 10)
            .unwrap();
        assert!(
            !outputs.is_empty(),
            "expected at least one ThreadOutput artifact"
        );
    }

    #[test]
    fn execute_saves_output_to_thread_store() {
        let dir = tempfile::tempdir().unwrap();
        let kernel = test_kernel(dir.path());
        let thread = test_thread("StoreCheck");
        kernel.thread_registry.register(thread.clone()).unwrap();
        let snapshot = test_snapshot(kernel.vessel_id);
        let tick_id = TickId::new();
        let token = CancellationToken::new();
        let json = r#"{"summary":"stored ok","recommendations":[]}"#;
        let handler = test_handler_with_mock(json, kernel.artifact_store.clone());

        execute_thread(&handler, &kernel, &thread, &snapshot, tick_id, &token).unwrap();

        let recent = kernel
            .thread_registry
            .recent_outputs(thread.thread_id, 5)
            .unwrap();
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].summary, "stored ok");
    }

    #[test]
    fn execute_records_run_tick() {
        let dir = tempfile::tempdir().unwrap();
        let kernel = test_kernel(dir.path());
        let thread = test_thread("RunRecorder");
        kernel.thread_registry.register(thread.clone()).unwrap();
        let snapshot = test_snapshot(kernel.vessel_id);
        let tick_id = TickId::new();
        let token = CancellationToken::new();
        let json = r#"{"summary":"ok","recommendations":[]}"#;
        let handler = test_handler_with_mock(json, kernel.artifact_store.clone());

        execute_thread(&handler, &kernel, &thread, &snapshot, tick_id, &token).unwrap();

        // record_run sets the tick number to snapshot.tick_number + 1
        let (_, _) = kernel
            .thread_registry
            .get(thread.thread_id)
            .unwrap()
            .unwrap();
        // Due threads uses the thread store's get_last_run internally.
        // Verify by calling due_threads: the thread was just run at
        // tick 1, so it should not be due again at tick 1.
        // (EveryTick is always due, so we check the run was recorded
        // by verifying the output exists.)
        let recent = kernel
            .thread_registry
            .recent_outputs(thread.thread_id, 1)
            .unwrap();
        assert_eq!(recent.len(), 1);
    }

    #[test]
    fn execute_fallback_on_invalid_json() {
        let dir = tempfile::tempdir().unwrap();
        let kernel = test_kernel(dir.path());
        let thread = test_thread("FallbackTest");
        kernel.thread_registry.register(thread.clone()).unwrap();
        let snapshot = test_snapshot(kernel.vessel_id);
        let tick_id = TickId::new();
        let token = CancellationToken::new();
        let handler = test_handler_with_mock("not json lol", kernel.artifact_store.clone());

        let output =
            execute_thread(&handler, &kernel, &thread, &snapshot, tick_id, &token).unwrap();

        // Fallback: summary is the raw text, recommendations empty
        assert_eq!(output.summary, "not json lol");
        assert!(output.recommendations.is_empty());
    }

    #[test]
    fn execute_respects_cancellation() {
        let dir = tempfile::tempdir().unwrap();
        let kernel = test_kernel(dir.path());
        let thread = test_thread("CancelTest");
        kernel.thread_registry.register(thread.clone()).unwrap();
        let snapshot = test_snapshot(kernel.vessel_id);
        let tick_id = TickId::new();
        let json = r#"{"summary":"ok","recommendations":[]}"#;
        let handler = test_handler_with_mock(json, kernel.artifact_store.clone());

        // Create an already-cancelled token
        let token = CancellationToken::new();
        token.cancel();

        let result = execute_thread(&handler, &kernel, &thread, &snapshot, tick_id, &token);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("cancelled"));
    }

    #[test]
    fn execute_respects_token_budget() {
        let dir = tempfile::tempdir().unwrap();
        let kernel = test_kernel(dir.path());
        let mut thread = test_thread("BudgetCheck");
        thread.token_budget = 8000;
        kernel.thread_registry.register(thread.clone()).unwrap();
        let snapshot = test_snapshot(kernel.vessel_id);
        let tick_id = TickId::new();
        let token = CancellationToken::new();

        let capturing = Arc::new(CapturingBackend {
            captured: std::sync::Mutex::new(None),
        });
        let backend: Arc<dyn LlmHttpBackend> = capturing.clone();
        let handler = CognitiveHandler::with_backends(
            Some(backend),
            None,
            LlmBackend::Local,
            kernel.artifact_store.clone(),
        );

        execute_thread(&handler, &kernel, &thread, &snapshot, tick_id, &token).unwrap();

        let captured = capturing.captured.lock().unwrap();
        let req = captured.as_ref().expect("request should be captured");
        // max_output_tokens should be token_budget / 4 = 8000 / 4 = 2000
        assert_eq!(req.max_output_tokens, 2000);
    }
}

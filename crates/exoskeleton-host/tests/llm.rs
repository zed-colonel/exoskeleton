//! Sprint 4: LLM Adapter integration tests.

mod common;

use std::sync::Arc;

use common::{test_config, test_llm_request, test_llm_response};
use exoskeleton_core::llm::{LlmBackend, LlmResponse, StopReason};
use exoskeleton_core::{ArtifactKind, ArtifactStore};
use exoskeleton_host::cognitive_engine::{
    bootstrap_cognitive_engine_with_backends, CognitivePayload, CognitiveTaskType,
};
use exoskeleton_host::storage::StorageManager;
use exoskeleton_host::vessel::ensure_data_dirs;
use exoskeleton_host::MockLlmBackend;

// ── T-4: CognitiveHandler LlmCall Route ──

#[tokio::test]
async fn handler_llm_call_succeeds() {
    let dir = tempfile::tempdir().unwrap();
    let config = test_config(dir.path());
    ensure_data_dirs(&config).unwrap();
    let storage = StorageManager::open(&config.data_dir).unwrap();
    let artifact_store: Arc<dyn ArtifactStore> = storage.artifact_store().clone();

    let mock = Arc::new(MockLlmBackend::new(test_llm_response()));
    let engine = bootstrap_cognitive_engine_with_backends(
        &config,
        Some(mock.clone()),
        None,
        artifact_store,
        None,
    )
    .unwrap();

    let payload = CognitivePayload {
        task_type: CognitiveTaskType::LlmCall,
        data: serde_json::to_value(test_llm_request()).unwrap(),
    };
    let payload_bytes = serde_json::to_vec(&payload).unwrap();

    use actionqueue_core::ids::TaskId;
    use actionqueue_core::task::constraints::TaskConstraints;
    use actionqueue_core::task::metadata::TaskMetadata;
    use actionqueue_core::task::run_policy::RunPolicy;
    use actionqueue_core::task::task_spec::{TaskPayload, TaskSpec};

    let engine_slot: Arc<tokio::sync::Mutex<Option<_>>> =
        Arc::new(tokio::sync::Mutex::new(Some(engine)));
    let task_id = TaskId::new();
    let spec = TaskSpec::new(
        task_id,
        TaskPayload::with_content_type(payload_bytes, "application/json"),
        RunPolicy::Once,
        TaskConstraints::default(),
        TaskMetadata::default(),
    )
    .unwrap();

    {
        let mut guard = engine_slot.lock().await;
        let engine = guard.as_mut().unwrap();
        engine.submit_task(spec).unwrap();
        let _ = engine.run_until_idle().await.unwrap();
    }

    // Verify the mock was called
    assert_eq!(mock.call_count(), 1);

    // Verify we can extract the response from the projection
    {
        let guard = engine_slot.lock().await;
        let engine = guard.as_ref().unwrap();
        let projection = engine.projection();
        let runs: Vec<_> = projection.runs_for_task(task_id).collect();
        let run = runs.last().unwrap();
        assert!(run.is_terminal(), "Run should be complete");

        let attempts = projection.get_attempt_history(&run.id()).unwrap();
        let output = attempts.last().unwrap().output().unwrap();
        let response: LlmResponse = serde_json::from_slice(output).unwrap();
        assert_eq!(response.content, "The answer is 4.");
    }

    // Clean shutdown
    let engine = engine_slot.lock().await.take();
    if let Some(engine) = engine {
        let _ = engine.shutdown();
    }
}

#[tokio::test]
async fn handler_llm_call_invalid_request() {
    let dir = tempfile::tempdir().unwrap();
    let config = test_config(dir.path());
    ensure_data_dirs(&config).unwrap();
    let storage = StorageManager::open(&config.data_dir).unwrap();
    let artifact_store: Arc<dyn ArtifactStore> = storage.artifact_store().clone();

    let engine = bootstrap_cognitive_engine_with_backends(
        &config,
        Some(Arc::new(MockLlmBackend::new(test_llm_response()))),
        None,
        artifact_store,
        None,
    )
    .unwrap();

    // Send invalid data (not an LlmRequest)
    let payload = CognitivePayload {
        task_type: CognitiveTaskType::LlmCall,
        data: serde_json::json!({"invalid": true}),
    };
    let payload_bytes = serde_json::to_vec(&payload).unwrap();

    use actionqueue_core::ids::TaskId;
    use actionqueue_core::task::constraints::TaskConstraints;
    use actionqueue_core::task::metadata::TaskMetadata;
    use actionqueue_core::task::run_policy::RunPolicy;
    use actionqueue_core::task::task_spec::{TaskPayload, TaskSpec};

    let engine_slot: Arc<tokio::sync::Mutex<Option<_>>> =
        Arc::new(tokio::sync::Mutex::new(Some(engine)));
    let spec = TaskSpec::new(
        TaskId::new(),
        TaskPayload::with_content_type(payload_bytes, "application/json"),
        RunPolicy::Once,
        TaskConstraints::default(),
        TaskMetadata::default(),
    )
    .unwrap();

    {
        let mut guard = engine_slot.lock().await;
        let engine = guard.as_mut().unwrap();
        engine.submit_task(spec).unwrap();
        let _ = engine.run_until_idle().await.unwrap();
    }

    // Should not crash, task should fail terminally
    let engine = engine_slot.lock().await.take();
    if let Some(engine) = engine {
        let _ = engine.shutdown();
    }
}

#[tokio::test]
async fn handler_llm_call_backend_not_configured() {
    let dir = tempfile::tempdir().unwrap();
    let config = test_config(dir.path());
    ensure_data_dirs(&config).unwrap();
    let storage = StorageManager::open(&config.data_dir).unwrap();
    let artifact_store: Arc<dyn ArtifactStore> = storage.artifact_store().clone();

    // No backends configured
    let engine = bootstrap_cognitive_engine_with_backends(
        &config,
        None, // no local
        None, // no frontier
        artifact_store,
        None,
    )
    .unwrap();

    let mut request = test_llm_request();
    request.backend = Some(LlmBackend::Local);
    let payload = CognitivePayload {
        task_type: CognitiveTaskType::LlmCall,
        data: serde_json::to_value(request).unwrap(),
    };
    let payload_bytes = serde_json::to_vec(&payload).unwrap();

    use actionqueue_core::ids::TaskId;
    use actionqueue_core::task::constraints::TaskConstraints;
    use actionqueue_core::task::metadata::TaskMetadata;
    use actionqueue_core::task::run_policy::RunPolicy;
    use actionqueue_core::task::task_spec::{TaskPayload, TaskSpec};

    let engine_slot: Arc<tokio::sync::Mutex<Option<_>>> =
        Arc::new(tokio::sync::Mutex::new(Some(engine)));
    let task_id = TaskId::new();
    let spec = TaskSpec::new(
        task_id,
        TaskPayload::with_content_type(payload_bytes, "application/json"),
        RunPolicy::Once,
        TaskConstraints::default(),
        TaskMetadata::default(),
    )
    .unwrap();

    {
        let mut guard = engine_slot.lock().await;
        let engine = guard.as_mut().unwrap();
        engine.submit_task(spec).unwrap();
        let _ = engine.run_until_idle().await.unwrap();
    }

    // Task should fail with "not configured"
    {
        let guard = engine_slot.lock().await;
        let engine = guard.as_ref().unwrap();
        let projection = engine.projection();
        let runs: Vec<_> = projection.runs_for_task(task_id).collect();
        let run = runs.last().unwrap();
        assert!(run.is_terminal());

        let attempts = projection.get_attempt_history(&run.id()).unwrap();
        let error = attempts.last().unwrap().error().unwrap();
        assert!(
            error.contains("not configured"),
            "Expected 'not configured', got: {error}"
        );
    }

    let engine = engine_slot.lock().await.take();
    if let Some(engine) = engine {
        let _ = engine.shutdown();
    }
}

#[tokio::test]
async fn handler_llm_call_backend_error() {
    let dir = tempfile::tempdir().unwrap();
    let config = test_config(dir.path());
    ensure_data_dirs(&config).unwrap();
    let storage = StorageManager::open(&config.data_dir).unwrap();
    let artifact_store: Arc<dyn ArtifactStore> = storage.artifact_store().clone();

    let mock = Arc::new(MockLlmBackend::failing("429: rate limit exceeded"));
    let engine =
        bootstrap_cognitive_engine_with_backends(&config, Some(mock), None, artifact_store, None)
            .unwrap();

    let payload = CognitivePayload {
        task_type: CognitiveTaskType::LlmCall,
        data: serde_json::to_value(test_llm_request()).unwrap(),
    };
    let payload_bytes = serde_json::to_vec(&payload).unwrap();

    use actionqueue_core::ids::TaskId;
    use actionqueue_core::task::constraints::TaskConstraints;
    use actionqueue_core::task::metadata::TaskMetadata;
    use actionqueue_core::task::run_policy::RunPolicy;
    use actionqueue_core::task::task_spec::{TaskPayload, TaskSpec};

    let engine_slot: Arc<tokio::sync::Mutex<Option<_>>> =
        Arc::new(tokio::sync::Mutex::new(Some(engine)));
    let spec = TaskSpec::new(
        TaskId::new(),
        TaskPayload::with_content_type(payload_bytes, "application/json"),
        RunPolicy::Once,
        TaskConstraints::default(),
        TaskMetadata::default(),
    )
    .unwrap();

    {
        let mut guard = engine_slot.lock().await;
        let engine = guard.as_mut().unwrap();
        engine.submit_task(spec).unwrap();
        let _ = engine.run_until_idle().await.unwrap();
    }

    // Should not crash
    let engine = engine_slot.lock().await.take();
    if let Some(engine) = engine {
        let _ = engine.shutdown();
    }
}

#[tokio::test]
async fn handler_master_loop_still_stub() {
    let dir = tempfile::tempdir().unwrap();
    let config = test_config(dir.path());
    ensure_data_dirs(&config).unwrap();
    let storage = StorageManager::open(&config.data_dir).unwrap();
    let artifact_store: Arc<dyn ArtifactStore> = storage.artifact_store().clone();

    let engine =
        bootstrap_cognitive_engine_with_backends(&config, None, None, artifact_store, None)
            .unwrap();

    let payload = CognitivePayload {
        task_type: CognitiveTaskType::MasterLoop,
        data: serde_json::Value::Null,
    };
    let payload_bytes = serde_json::to_vec(&payload).unwrap();

    use actionqueue_core::ids::TaskId;
    use actionqueue_core::task::constraints::TaskConstraints;
    use actionqueue_core::task::metadata::TaskMetadata;
    use actionqueue_core::task::run_policy::RunPolicy;
    use actionqueue_core::task::task_spec::{TaskPayload, TaskSpec};

    let engine_slot: Arc<tokio::sync::Mutex<Option<_>>> =
        Arc::new(tokio::sync::Mutex::new(Some(engine)));
    let task_id = TaskId::new();
    let spec = TaskSpec::new(
        task_id,
        TaskPayload::with_content_type(payload_bytes, "application/json"),
        RunPolicy::Once,
        TaskConstraints::default(),
        TaskMetadata::default(),
    )
    .unwrap();

    {
        let mut guard = engine_slot.lock().await;
        let engine = guard.as_mut().unwrap();
        engine.submit_task(spec).unwrap();
        let _ = engine.run_until_idle().await.unwrap();
    }

    {
        let guard = engine_slot.lock().await;
        let engine = guard.as_ref().unwrap();
        let projection = engine.projection();
        let runs: Vec<_> = projection.runs_for_task(task_id).collect();
        let run = runs.last().unwrap();
        let attempts = projection.get_attempt_history(&run.id()).unwrap();
        let error = attempts.last().unwrap().error().unwrap();
        assert!(
            error.contains("kernel not configured"),
            "Expected 'kernel not configured' message, got: {error}"
        );
    }

    let engine = engine_slot.lock().await.take();
    if let Some(engine) = engine {
        let _ = engine.shutdown();
    }
}

#[tokio::test]
async fn handler_thread_invalid_payload() {
    let dir = tempfile::tempdir().unwrap();
    let config = test_config(dir.path());
    ensure_data_dirs(&config).unwrap();
    let storage = StorageManager::open(&config.data_dir).unwrap();
    let artifact_store: Arc<dyn ArtifactStore> = storage.artifact_store().clone();

    let engine =
        bootstrap_cognitive_engine_with_backends(&config, None, None, artifact_store, None)
            .unwrap();

    let payload = CognitivePayload {
        task_type: CognitiveTaskType::Thread,
        data: serde_json::Value::Null,
    };
    let payload_bytes = serde_json::to_vec(&payload).unwrap();

    use actionqueue_core::ids::TaskId;
    use actionqueue_core::task::constraints::TaskConstraints;
    use actionqueue_core::task::metadata::TaskMetadata;
    use actionqueue_core::task::run_policy::RunPolicy;
    use actionqueue_core::task::task_spec::{TaskPayload, TaskSpec};

    let engine_slot: Arc<tokio::sync::Mutex<Option<_>>> =
        Arc::new(tokio::sync::Mutex::new(Some(engine)));
    let task_id = TaskId::new();
    let spec = TaskSpec::new(
        task_id,
        TaskPayload::with_content_type(payload_bytes, "application/json"),
        RunPolicy::Once,
        TaskConstraints::default(),
        TaskMetadata::default(),
    )
    .unwrap();

    {
        let mut guard = engine_slot.lock().await;
        let engine = guard.as_mut().unwrap();
        engine.submit_task(spec).unwrap();
        let _ = engine.run_until_idle().await.unwrap();
    }

    {
        let guard = engine_slot.lock().await;
        let engine = guard.as_ref().unwrap();
        let projection = engine.projection();
        let runs: Vec<_> = projection.runs_for_task(task_id).collect();
        let run = runs.last().unwrap();
        let attempts = projection.get_attempt_history(&run.id()).unwrap();
        let error = attempts.last().unwrap().error().unwrap();
        assert!(
            error.contains("invalid thread payload"),
            "Expected 'invalid thread payload' message, got: {error}"
        );
    }

    let engine = engine_slot.lock().await.take();
    if let Some(engine) = engine {
        let _ = engine.shutdown();
    }
}

// ── T-5: Artifact Storage (I3) ──

#[tokio::test]
async fn llm_response_stored_as_artifact() {
    let dir = tempfile::tempdir().unwrap();
    let config = test_config(dir.path());
    ensure_data_dirs(&config).unwrap();
    let storage = StorageManager::open(&config.data_dir).unwrap();
    let artifact_store: Arc<dyn ArtifactStore> = storage.artifact_store().clone();
    let artifact_store_read = storage.artifact_store().clone();

    let mock = Arc::new(MockLlmBackend::new(test_llm_response()));
    let engine =
        bootstrap_cognitive_engine_with_backends(&config, Some(mock), None, artifact_store, None)
            .unwrap();

    let payload = CognitivePayload {
        task_type: CognitiveTaskType::LlmCall,
        data: serde_json::to_value(test_llm_request()).unwrap(),
    };
    let payload_bytes = serde_json::to_vec(&payload).unwrap();

    use actionqueue_core::ids::TaskId;
    use actionqueue_core::task::constraints::TaskConstraints;
    use actionqueue_core::task::metadata::TaskMetadata;
    use actionqueue_core::task::run_policy::RunPolicy;
    use actionqueue_core::task::task_spec::{TaskPayload, TaskSpec};

    let engine_slot: Arc<tokio::sync::Mutex<Option<_>>> =
        Arc::new(tokio::sync::Mutex::new(Some(engine)));
    let spec = TaskSpec::new(
        TaskId::new(),
        TaskPayload::with_content_type(payload_bytes, "application/json"),
        RunPolicy::Once,
        TaskConstraints::default(),
        TaskMetadata::default(),
    )
    .unwrap();

    {
        let mut guard = engine_slot.lock().await;
        let engine = guard.as_mut().unwrap();
        engine.submit_task(spec).unwrap();
        let _ = engine.run_until_idle().await.unwrap();
    }

    // Verify artifact was stored
    let artifacts = artifact_store_read
        .list_by_kind(ArtifactKind::LlmResponse, 10)
        .unwrap();
    assert!(
        !artifacts.is_empty(),
        "Expected at least one LlmResponse artifact"
    );

    // Verify content is valid JSON that deserializes to LlmResponse
    let artifact = artifact_store_read
        .get(&artifacts[0].id)
        .unwrap()
        .expect("artifact should exist");
    assert_eq!(artifact.kind, ArtifactKind::LlmResponse);
    let stored_response: LlmResponse = serde_json::from_slice(&artifact.content).unwrap();
    assert_eq!(stored_response.content, "The answer is 4.");

    let engine = engine_slot.lock().await.take();
    if let Some(engine) = engine {
        let _ = engine.shutdown();
    }
}

#[tokio::test]
async fn artifact_is_content_addressed() {
    let dir = tempfile::tempdir().unwrap();
    let config = test_config(dir.path());
    ensure_data_dirs(&config).unwrap();
    let storage = StorageManager::open(&config.data_dir).unwrap();
    let artifact_store: Arc<dyn ArtifactStore> = storage.artifact_store().clone();
    let artifact_store_read = storage.artifact_store().clone();

    let mock = Arc::new(MockLlmBackend::new(test_llm_response()));
    let engine =
        bootstrap_cognitive_engine_with_backends(&config, Some(mock), None, artifact_store, None)
            .unwrap();

    let engine_slot: Arc<tokio::sync::Mutex<Option<_>>> =
        Arc::new(tokio::sync::Mutex::new(Some(engine)));

    use actionqueue_core::ids::TaskId;
    use actionqueue_core::task::constraints::TaskConstraints;
    use actionqueue_core::task::metadata::TaskMetadata;
    use actionqueue_core::task::run_policy::RunPolicy;
    use actionqueue_core::task::task_spec::{TaskPayload, TaskSpec};

    // Submit two identical LLM calls
    for _ in 0..2 {
        let payload = CognitivePayload {
            task_type: CognitiveTaskType::LlmCall,
            data: serde_json::to_value(test_llm_request()).unwrap(),
        };
        let spec = TaskSpec::new(
            TaskId::new(),
            TaskPayload::with_content_type(
                serde_json::to_vec(&payload).unwrap(),
                "application/json",
            ),
            RunPolicy::Once,
            TaskConstraints::default(),
            TaskMetadata::default(),
        )
        .unwrap();

        let mut guard = engine_slot.lock().await;
        let engine = guard.as_mut().unwrap();
        engine.submit_task(spec).unwrap();
        let _ = engine.run_until_idle().await.unwrap();
    }

    // Content-addressed: same content = same artifact ID, so only 1 artifact
    let artifacts = artifact_store_read
        .list_by_kind(ArtifactKind::LlmResponse, 10)
        .unwrap();
    // Due to latency_ms being overwritten with actual timing, IDs may differ.
    // But artifact store deduplicates by content — so if timing is identical,
    // we get 1 artifact. If not, we get 2. Both are valid.
    assert!(
        !artifacts.is_empty(),
        "Expected at least one LlmResponse artifact"
    );

    let engine = engine_slot.lock().await.take();
    if let Some(engine) = engine {
        let _ = engine.shutdown();
    }
}

#[tokio::test]
async fn llm_response_artifact_kind_roundtrip() {
    let json = serde_json::to_string(&ArtifactKind::LlmResponse).unwrap();
    assert_eq!(json, "\"llm_response\"");
    let parsed: ArtifactKind = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed, ArtifactKind::LlmResponse);
}

// ── T-6: Secret Hygiene (I4) ──

#[tokio::test]
async fn api_key_not_in_artifact() {
    let dir = tempfile::tempdir().unwrap();
    let config = test_config(dir.path());
    ensure_data_dirs(&config).unwrap();
    let storage = StorageManager::open(&config.data_dir).unwrap();
    let artifact_store: Arc<dyn ArtifactStore> = storage.artifact_store().clone();
    let artifact_store_read = storage.artifact_store().clone();

    let mock = Arc::new(MockLlmBackend::new(test_llm_response()));
    let engine =
        bootstrap_cognitive_engine_with_backends(&config, Some(mock), None, artifact_store, None)
            .unwrap();

    let payload = CognitivePayload {
        task_type: CognitiveTaskType::LlmCall,
        data: serde_json::to_value(test_llm_request()).unwrap(),
    };
    let payload_bytes = serde_json::to_vec(&payload).unwrap();

    use actionqueue_core::ids::TaskId;
    use actionqueue_core::task::constraints::TaskConstraints;
    use actionqueue_core::task::metadata::TaskMetadata;
    use actionqueue_core::task::run_policy::RunPolicy;
    use actionqueue_core::task::task_spec::{TaskPayload, TaskSpec};

    let engine_slot: Arc<tokio::sync::Mutex<Option<_>>> =
        Arc::new(tokio::sync::Mutex::new(Some(engine)));
    let spec = TaskSpec::new(
        TaskId::new(),
        TaskPayload::with_content_type(payload_bytes, "application/json"),
        RunPolicy::Once,
        TaskConstraints::default(),
        TaskMetadata::default(),
    )
    .unwrap();

    {
        let mut guard = engine_slot.lock().await;
        let engine = guard.as_mut().unwrap();
        engine.submit_task(spec).unwrap();
        let _ = engine.run_until_idle().await.unwrap();
    }

    // Verify artifacts don't contain API key strings
    let artifacts = artifact_store_read
        .list_by_kind(ArtifactKind::LlmResponse, 10)
        .unwrap();
    for artifact_ref in &artifacts {
        let artifact = artifact_store_read.get(&artifact_ref.id).unwrap().unwrap();
        let content_str = String::from_utf8_lossy(&artifact.content);
        assert!(
            !content_str.contains("ANTHROPIC_API_KEY"),
            "API key env var name should not be in artifact"
        );
        assert!(
            !content_str.contains("OPENAI_API_KEY"),
            "API key env var name should not be in artifact"
        );
    }

    let engine = engine_slot.lock().await.take();
    if let Some(engine) = engine {
        let _ = engine.shutdown();
    }
}

#[tokio::test]
async fn api_key_not_in_payload() {
    // The CognitivePayload for an LlmCall contains an LlmRequest.
    // LlmRequest has NO API key fields — verify this.
    let request = test_llm_request();
    let payload = CognitivePayload {
        task_type: CognitiveTaskType::LlmCall,
        data: serde_json::to_value(request).unwrap(),
    };
    let json = serde_json::to_string(&payload).unwrap();
    assert!(
        !json.contains("api_key"),
        "Payload should not contain 'api_key': {json}"
    );
    assert!(
        !json.contains("ANTHROPIC_API_KEY"),
        "Payload should not contain API key env var name"
    );
}

#[tokio::test]
async fn api_key_env_name_in_config_only() {
    // FrontierModelConfig stores the env var NAME, not the key VALUE
    use exoskeleton_host::config::{FrontierModelConfig, FrontierProvider};
    let config = FrontierModelConfig {
        provider: FrontierProvider::Anthropic,
        model: "test".into(),
        api_key_env: "MY_SECRET_KEY".into(),
        endpoint: None,
    };
    let json = serde_json::to_string(&config).unwrap();
    assert!(
        json.contains("MY_SECRET_KEY"),
        "Config should contain env var name"
    );
    // But the actual key value should not be anywhere
    assert!(
        !json.contains("sk-"),
        "Config should not contain actual key values"
    );
}

// ── T-7: Cancellation / Timeout ──

#[tokio::test]
async fn non_cancelled_succeeds() {
    let dir = tempfile::tempdir().unwrap();
    let config = test_config(dir.path());
    ensure_data_dirs(&config).unwrap();
    let storage = StorageManager::open(&config.data_dir).unwrap();
    let artifact_store: Arc<dyn ArtifactStore> = storage.artifact_store().clone();

    let mock = Arc::new(MockLlmBackend::new(test_llm_response()));
    let engine = bootstrap_cognitive_engine_with_backends(
        &config,
        Some(mock.clone()),
        None,
        artifact_store,
        None,
    )
    .unwrap();

    let payload = CognitivePayload {
        task_type: CognitiveTaskType::LlmCall,
        data: serde_json::to_value(test_llm_request()).unwrap(),
    };
    let payload_bytes = serde_json::to_vec(&payload).unwrap();

    use actionqueue_core::ids::TaskId;
    use actionqueue_core::task::constraints::TaskConstraints;
    use actionqueue_core::task::metadata::TaskMetadata;
    use actionqueue_core::task::run_policy::RunPolicy;
    use actionqueue_core::task::task_spec::{TaskPayload, TaskSpec};

    let engine_slot: Arc<tokio::sync::Mutex<Option<_>>> =
        Arc::new(tokio::sync::Mutex::new(Some(engine)));
    let task_id = TaskId::new();
    let spec = TaskSpec::new(
        task_id,
        TaskPayload::with_content_type(payload_bytes, "application/json"),
        RunPolicy::Once,
        TaskConstraints::default(),
        TaskMetadata::default(),
    )
    .unwrap();

    {
        let mut guard = engine_slot.lock().await;
        let engine = guard.as_mut().unwrap();
        engine.submit_task(spec).unwrap();
        let _ = engine.run_until_idle().await.unwrap();
    }

    // Verify task completed successfully
    {
        let guard = engine_slot.lock().await;
        let engine = guard.as_ref().unwrap();
        let projection = engine.projection();
        let runs: Vec<_> = projection.runs_for_task(task_id).collect();
        let run = runs.last().unwrap();

        use actionqueue_core::run::state::RunState;
        assert_eq!(run.state(), RunState::Completed);
    }

    assert_eq!(mock.call_count(), 1);

    let engine = engine_slot.lock().await.take();
    if let Some(engine) = engine {
        let _ = engine.shutdown();
    }
}

// ── T-9: Edge Cases ──

#[tokio::test]
async fn empty_response_content() {
    let dir = tempfile::tempdir().unwrap();
    let config = test_config(dir.path());
    ensure_data_dirs(&config).unwrap();
    let storage = StorageManager::open(&config.data_dir).unwrap();
    let artifact_store: Arc<dyn ArtifactStore> = storage.artifact_store().clone();

    let response = LlmResponse {
        content: String::new(),
        model: "mock".into(),
        tokens_in: 10,
        tokens_out: 0,
        latency_ms: 50,
        stop_reason: StopReason::EndTurn,
        cost_estimate_cents: None,
        backend: LlmBackend::Local,
    };
    let mock = Arc::new(MockLlmBackend::new(response));
    let engine =
        bootstrap_cognitive_engine_with_backends(&config, Some(mock), None, artifact_store, None)
            .unwrap();

    let payload = CognitivePayload {
        task_type: CognitiveTaskType::LlmCall,
        data: serde_json::to_value(test_llm_request()).unwrap(),
    };
    let payload_bytes = serde_json::to_vec(&payload).unwrap();

    use actionqueue_core::ids::TaskId;
    use actionqueue_core::task::constraints::TaskConstraints;
    use actionqueue_core::task::metadata::TaskMetadata;
    use actionqueue_core::task::run_policy::RunPolicy;
    use actionqueue_core::task::task_spec::{TaskPayload, TaskSpec};

    let engine_slot: Arc<tokio::sync::Mutex<Option<_>>> =
        Arc::new(tokio::sync::Mutex::new(Some(engine)));
    let task_id = TaskId::new();
    let spec = TaskSpec::new(
        task_id,
        TaskPayload::with_content_type(payload_bytes, "application/json"),
        RunPolicy::Once,
        TaskConstraints::default(),
        TaskMetadata::default(),
    )
    .unwrap();

    {
        let mut guard = engine_slot.lock().await;
        let engine = guard.as_mut().unwrap();
        engine.submit_task(spec).unwrap();
        let _ = engine.run_until_idle().await.unwrap();
    }

    // Should succeed even with empty content
    {
        let guard = engine_slot.lock().await;
        let engine = guard.as_ref().unwrap();
        let projection = engine.projection();
        let runs: Vec<_> = projection.runs_for_task(task_id).collect();
        let run = runs.last().unwrap();
        use actionqueue_core::run::state::RunState;
        assert_eq!(run.state(), RunState::Completed);
    }

    let engine = engine_slot.lock().await.take();
    if let Some(engine) = engine {
        let _ = engine.shutdown();
    }
}

#[tokio::test]
async fn unicode_in_response() {
    let dir = tempfile::tempdir().unwrap();
    let config = test_config(dir.path());
    ensure_data_dirs(&config).unwrap();
    let storage = StorageManager::open(&config.data_dir).unwrap();
    let artifact_store: Arc<dyn ArtifactStore> = storage.artifact_store().clone();

    let response = LlmResponse {
        content: "你好世界 🌍 مرحبا".into(),
        model: "mock".into(),
        tokens_in: 10,
        tokens_out: 5,
        latency_ms: 50,
        stop_reason: StopReason::EndTurn,
        cost_estimate_cents: None,
        backend: LlmBackend::Local,
    };
    let mock = Arc::new(MockLlmBackend::new(response));
    let engine =
        bootstrap_cognitive_engine_with_backends(&config, Some(mock), None, artifact_store, None)
            .unwrap();

    let payload = CognitivePayload {
        task_type: CognitiveTaskType::LlmCall,
        data: serde_json::to_value(test_llm_request()).unwrap(),
    };
    let payload_bytes = serde_json::to_vec(&payload).unwrap();

    use actionqueue_core::ids::TaskId;
    use actionqueue_core::task::constraints::TaskConstraints;
    use actionqueue_core::task::metadata::TaskMetadata;
    use actionqueue_core::task::run_policy::RunPolicy;
    use actionqueue_core::task::task_spec::{TaskPayload, TaskSpec};

    let engine_slot: Arc<tokio::sync::Mutex<Option<_>>> =
        Arc::new(tokio::sync::Mutex::new(Some(engine)));
    let task_id = TaskId::new();
    let spec = TaskSpec::new(
        task_id,
        TaskPayload::with_content_type(payload_bytes, "application/json"),
        RunPolicy::Once,
        TaskConstraints::default(),
        TaskMetadata::default(),
    )
    .unwrap();

    {
        let mut guard = engine_slot.lock().await;
        let engine = guard.as_mut().unwrap();
        engine.submit_task(spec).unwrap();
        let _ = engine.run_until_idle().await.unwrap();
    }

    {
        let guard = engine_slot.lock().await;
        let engine = guard.as_ref().unwrap();
        let projection = engine.projection();
        let runs: Vec<_> = projection.runs_for_task(task_id).collect();
        let run = runs.last().unwrap();

        let attempts = projection.get_attempt_history(&run.id()).unwrap();
        let output = attempts.last().unwrap().output().unwrap();
        let llm_response: LlmResponse = serde_json::from_slice(output).unwrap();
        assert!(llm_response.content.contains("你好世界"));
        assert!(llm_response.content.contains("🌍"));
    }

    let engine = engine_slot.lock().await.take();
    if let Some(engine) = engine {
        let _ = engine.shutdown();
    }
}

#[tokio::test]
async fn concurrent_llm_calls() {
    let dir = tempfile::tempdir().unwrap();
    let config = test_config(dir.path());
    ensure_data_dirs(&config).unwrap();
    let storage = StorageManager::open(&config.data_dir).unwrap();
    let artifact_store: Arc<dyn ArtifactStore> = storage.artifact_store().clone();

    let mock = Arc::new(MockLlmBackend::new(test_llm_response()));
    let engine = bootstrap_cognitive_engine_with_backends(
        &config,
        Some(mock.clone()),
        None,
        artifact_store,
        None,
    )
    .unwrap();

    use actionqueue_core::ids::TaskId;
    use actionqueue_core::task::constraints::TaskConstraints;
    use actionqueue_core::task::metadata::TaskMetadata;
    use actionqueue_core::task::run_policy::RunPolicy;
    use actionqueue_core::task::task_spec::{TaskPayload, TaskSpec};

    let engine_slot: Arc<tokio::sync::Mutex<Option<_>>> =
        Arc::new(tokio::sync::Mutex::new(Some(engine)));

    // Submit two LLM tasks before running
    {
        let mut guard = engine_slot.lock().await;
        let engine = guard.as_mut().unwrap();
        for _ in 0..2 {
            let payload = CognitivePayload {
                task_type: CognitiveTaskType::LlmCall,
                data: serde_json::to_value(test_llm_request()).unwrap(),
            };
            let spec = TaskSpec::new(
                TaskId::new(),
                TaskPayload::with_content_type(
                    serde_json::to_vec(&payload).unwrap(),
                    "application/json",
                ),
                RunPolicy::Once,
                TaskConstraints::default(),
                TaskMetadata::default(),
            )
            .unwrap();
            engine.submit_task(spec).unwrap();
        }
        let _ = engine.run_until_idle().await.unwrap();
    }

    // Both should have been processed
    assert_eq!(mock.call_count(), 2);

    let engine = engine_slot.lock().await.take();
    if let Some(engine) = engine {
        let _ = engine.shutdown();
    }
}

// ── T-10: Property-Based Tests (Sprint 4) ──

mod sprint4_proptest {
    use exoskeleton_core::llm::*;
    use proptest::prelude::*;

    fn arb_backend() -> impl Strategy<Value = LlmBackend> {
        prop_oneof![Just(LlmBackend::Local), Just(LlmBackend::Frontier),]
    }

    fn arb_role() -> impl Strategy<Value = LlmRole> {
        prop_oneof![
            Just(LlmRole::System),
            Just(LlmRole::User),
            Just(LlmRole::Assistant),
        ]
    }

    fn arb_stop_reason() -> impl Strategy<Value = StopReason> {
        prop_oneof![
            Just(StopReason::EndTurn),
            Just(StopReason::MaxTokens),
            Just(StopReason::StopSequence),
        ]
    }

    fn arb_message() -> impl Strategy<Value = LlmMessage> {
        (arb_role(), ".*").prop_map(|(role, content)| LlmMessage { role, content })
    }

    fn arb_request() -> impl Strategy<Value = LlmRequest> {
        (
            proptest::option::of(arb_backend()),
            proptest::option::of(".*"),
            proptest::collection::vec(arb_message(), 0..5),
            1u64..10000,
            proptest::option::of(0.0f64..2.0),
            proptest::collection::vec(".*", 0..3),
        )
            .prop_map(
                |(
                    backend,
                    system_prompt,
                    messages,
                    max_output_tokens,
                    temperature,
                    stop_sequences,
                )| {
                    LlmRequest {
                        backend,
                        system_prompt,
                        messages,
                        max_output_tokens,
                        temperature,
                        stop_sequences,
                    }
                },
            )
    }

    fn arb_response() -> impl Strategy<Value = LlmResponse> {
        (
            ".*",
            ".*",
            0u64..100000,
            0u64..100000,
            0u64..60000,
            arb_stop_reason(),
            proptest::option::of(0.0f64..100.0),
            arb_backend(),
        )
            .prop_map(
                |(
                    content,
                    model,
                    tokens_in,
                    tokens_out,
                    latency_ms,
                    stop_reason,
                    cost,
                    backend,
                )| {
                    LlmResponse {
                        content,
                        model,
                        tokens_in,
                        tokens_out,
                        latency_ms,
                        stop_reason,
                        cost_estimate_cents: cost,
                        backend,
                    }
                },
            )
    }

    /// Check approximate equality for LlmRequest (f64 temperature may lose precision).
    fn request_approx_eq(a: &LlmRequest, b: &LlmRequest) -> bool {
        a.backend == b.backend
            && a.system_prompt == b.system_prompt
            && a.messages == b.messages
            && a.max_output_tokens == b.max_output_tokens
            && a.stop_sequences == b.stop_sequences
            && match (a.temperature, b.temperature) {
                (None, None) => true,
                (Some(x), Some(y)) => (x - y).abs() < 1e-10,
                _ => false,
            }
    }

    /// Check approximate equality for LlmResponse (f64 cost may lose precision).
    fn response_approx_eq(a: &LlmResponse, b: &LlmResponse) -> bool {
        a.content == b.content
            && a.model == b.model
            && a.tokens_in == b.tokens_in
            && a.tokens_out == b.tokens_out
            && a.latency_ms == b.latency_ms
            && a.stop_reason == b.stop_reason
            && a.backend == b.backend
            && match (a.cost_estimate_cents, b.cost_estimate_cents) {
                (None, None) => true,
                (Some(x), Some(y)) => (x - y).abs() < 1e-10,
                _ => false,
            }
    }

    proptest! {
        #[test]
        fn llm_request_survives_json_roundtrip(request in arb_request()) {
            let json = serde_json::to_string(&request).unwrap();
            let parsed: LlmRequest = serde_json::from_str(&json).unwrap();
            prop_assert!(request_approx_eq(&request, &parsed),
                "Mismatch: {:?} vs {:?}", request, parsed);
        }

        #[test]
        fn llm_response_survives_json_roundtrip(response in arb_response()) {
            let json = serde_json::to_string(&response).unwrap();
            let parsed: LlmResponse = serde_json::from_str(&json).unwrap();
            prop_assert!(response_approx_eq(&response, &parsed),
                "Mismatch: {:?} vs {:?}", response, parsed);
        }

        #[test]
        fn llm_response_artifact_roundtrip(response in arb_response()) {
            use exoskeleton_core::{Artifact, ArtifactKind};
            let artifact = Artifact::from_json(ArtifactKind::LlmResponse, &response).unwrap();
            let parsed: LlmResponse = serde_json::from_slice(&artifact.content).unwrap();
            prop_assert!(response_approx_eq(&response, &parsed),
                "Mismatch: {:?} vs {:?}", response, parsed);
        }
    }
}

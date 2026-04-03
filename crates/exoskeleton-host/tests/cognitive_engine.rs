//! Sprint 1: Cognitive Engine + Vessel Lifecycle integration tests.

mod common;

use exoskeleton_host::cognitive_engine::{CognitivePayload, CognitiveTaskType};
use exoskeleton_host::config::VesselConfig;
use exoskeleton_host::vessel::{ensure_data_dirs, Vessel};

// ── T-3: CognitiveHandler Routing ──

#[tokio::test]
async fn handler_routes_master_loop() {
    let dir = tempfile::tempdir().unwrap();
    let config = common::test_config(dir.path());
    let vessel = Vessel::start_with_registry(config, common::test_registry())
        .await
        .unwrap();

    // Submit a master_loop task to the Cognitive AQ
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

    let spec = TaskSpec::new(
        TaskId::new(),
        TaskPayload::with_content_type(payload_bytes, "application/json"),
        RunPolicy::Once,
        TaskConstraints::default(),
        TaskMetadata::default(),
    )
    .unwrap();

    // Submit and wait for processing
    {
        let mut guard = vessel.cognitive_engine_slot().lock().await;
        let engine = guard.as_mut().unwrap();
        engine.submit_task(spec).unwrap();
        let _ = engine.run_until_idle().await.unwrap();
    }

    vessel.shutdown().await.unwrap();
}

#[tokio::test]
async fn handler_routes_thread() {
    let dir = tempfile::tempdir().unwrap();
    let config = common::test_config(dir.path());
    let vessel = Vessel::start_with_registry(config, common::test_registry())
        .await
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

    let spec = TaskSpec::new(
        TaskId::new(),
        TaskPayload::with_content_type(payload_bytes, "application/json"),
        RunPolicy::Once,
        TaskConstraints::default(),
        TaskMetadata::default(),
    )
    .unwrap();

    {
        let mut guard = vessel.cognitive_engine_slot().lock().await;
        let engine = guard.as_mut().unwrap();
        engine.submit_task(spec).unwrap();
        let _ = engine.run_until_idle().await.unwrap();
    }

    vessel.shutdown().await.unwrap();
}

#[tokio::test]
async fn handler_routes_llm_call() {
    let dir = tempfile::tempdir().unwrap();
    let config = common::test_config(dir.path());
    let vessel = Vessel::start_with_registry(config, common::test_registry())
        .await
        .unwrap();

    let payload = CognitivePayload {
        task_type: CognitiveTaskType::LlmCall,
        data: serde_json::Value::Null,
    };
    let payload_bytes = serde_json::to_vec(&payload).unwrap();

    use actionqueue_core::ids::TaskId;
    use actionqueue_core::task::constraints::TaskConstraints;
    use actionqueue_core::task::metadata::TaskMetadata;
    use actionqueue_core::task::run_policy::RunPolicy;
    use actionqueue_core::task::task_spec::{TaskPayload, TaskSpec};

    let spec = TaskSpec::new(
        TaskId::new(),
        TaskPayload::with_content_type(payload_bytes, "application/json"),
        RunPolicy::Once,
        TaskConstraints::default(),
        TaskMetadata::default(),
    )
    .unwrap();

    {
        let mut guard = vessel.cognitive_engine_slot().lock().await;
        let engine = guard.as_mut().unwrap();
        engine.submit_task(spec).unwrap();
        let _ = engine.run_until_idle().await.unwrap();
    }

    vessel.shutdown().await.unwrap();
}

#[tokio::test]
async fn handler_rejects_invalid_payload() {
    let dir = tempfile::tempdir().unwrap();
    let config = common::test_config(dir.path());
    let vessel = Vessel::start_with_registry(config, common::test_registry())
        .await
        .unwrap();

    use actionqueue_core::ids::TaskId;
    use actionqueue_core::task::constraints::TaskConstraints;
    use actionqueue_core::task::metadata::TaskMetadata;
    use actionqueue_core::task::run_policy::RunPolicy;
    use actionqueue_core::task::task_spec::{TaskPayload, TaskSpec};

    let spec = TaskSpec::new(
        TaskId::new(),
        TaskPayload::new(b"not json".to_vec()),
        RunPolicy::Once,
        TaskConstraints::default(),
        TaskMetadata::default(),
    )
    .unwrap();

    {
        let mut guard = vessel.cognitive_engine_slot().lock().await;
        let engine = guard.as_mut().unwrap();
        engine.submit_task(spec).unwrap();
        let _ = engine.run_until_idle().await.unwrap();
    }

    vessel.shutdown().await.unwrap();
}

#[tokio::test]
async fn handler_rejects_unknown_task_type() {
    let dir = tempfile::tempdir().unwrap();
    let config = common::test_config(dir.path());
    let vessel = Vessel::start_with_registry(config, common::test_registry())
        .await
        .unwrap();

    use actionqueue_core::ids::TaskId;
    use actionqueue_core::task::constraints::TaskConstraints;
    use actionqueue_core::task::metadata::TaskMetadata;
    use actionqueue_core::task::run_policy::RunPolicy;
    use actionqueue_core::task::task_spec::{TaskPayload, TaskSpec};

    let payload_bytes = br#"{"task_type": "unknown"}"#.to_vec();
    let spec = TaskSpec::new(
        TaskId::new(),
        TaskPayload::with_content_type(payload_bytes, "application/json"),
        RunPolicy::Once,
        TaskConstraints::default(),
        TaskMetadata::default(),
    )
    .unwrap();

    {
        let mut guard = vessel.cognitive_engine_slot().lock().await;
        let engine = guard.as_mut().unwrap();
        engine.submit_task(spec).unwrap();
        let _ = engine.run_until_idle().await.unwrap();
    }

    vessel.shutdown().await.unwrap();
}

// ── T-4: Data Directory Structure ──

#[test]
fn ensure_data_dirs_creates_all() {
    let dir = tempfile::tempdir().unwrap();
    let config = VesselConfig {
        mission: "dir test".into(),
        data_dir: dir.path().to_path_buf(),
        ..Default::default()
    };
    ensure_data_dirs(&config).unwrap();

    assert!(dir.path().join("cognitive-aq").is_dir());
    assert!(dir.path().join("wi").is_dir());
    assert!(dir.path().join("wi").join("aq").is_dir());
    assert!(dir.path().join("exo").is_dir());
}

#[test]
fn ensure_data_dirs_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let config = VesselConfig {
        mission: "idempotent test".into(),
        data_dir: dir.path().to_path_buf(),
        ..Default::default()
    };
    ensure_data_dirs(&config).unwrap();
    ensure_data_dirs(&config).unwrap(); // Second call succeeds
}

#[test]
fn ensure_data_dirs_pre_existing_ok() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("cognitive-aq")).unwrap();
    std::fs::create_dir_all(dir.path().join("wi")).unwrap();

    let config = VesselConfig {
        mission: "pre-existing test".into(),
        data_dir: dir.path().to_path_buf(),
        ..Default::default()
    };
    ensure_data_dirs(&config).unwrap();
}

// ── T-5: Vessel Lifecycle ──

#[tokio::test]
async fn vessel_starts_and_shuts_down() {
    let dir = tempfile::tempdir().unwrap();
    let config = common::test_config(dir.path());
    let vessel = Vessel::start_with_registry(config, common::test_registry())
        .await
        .unwrap();
    vessel.shutdown().await.unwrap();
}

#[tokio::test]
async fn vessel_start_creates_wal_directories() {
    let dir = tempfile::tempdir().unwrap();
    let config = common::test_config(dir.path());
    let vessel = Vessel::start_with_registry(config, common::test_registry())
        .await
        .unwrap();

    // AQ bootstrap creates wal/ subdirectories
    assert!(dir.path().join("cognitive-aq").join("wal").is_dir());
    assert!(dir.path().join("wi").join("aq").join("wal").is_dir());

    vessel.shutdown().await.unwrap();
}

#[tokio::test]
async fn vessel_start_creates_context_db() {
    let dir = tempfile::tempdir().unwrap();
    let config = common::test_config(dir.path());
    let vessel = Vessel::start_with_registry(config, common::test_registry())
        .await
        .unwrap();

    assert!(dir.path().join("wi").join("context.db").exists());

    vessel.shutdown().await.unwrap();
}

#[tokio::test]
async fn vessel_config_accessible_after_start() {
    let dir = tempfile::tempdir().unwrap();
    let config = common::test_config(dir.path());
    let expected_mission = config.mission.clone();
    let vessel = Vessel::start_with_registry(config, common::test_registry())
        .await
        .unwrap();

    assert_eq!(vessel.config().mission, expected_mission);

    vessel.shutdown().await.unwrap();
}

#[tokio::test]
async fn vessel_id_accessible() {
    let dir = tempfile::tempdir().unwrap();
    let config = common::test_config(dir.path());
    let expected_id = config.vessel_id;
    let vessel = Vessel::start_with_registry(config, common::test_registry())
        .await
        .unwrap();

    assert_eq!(vessel.vessel_id(), expected_id);

    vessel.shutdown().await.unwrap();
}

#[tokio::test]
async fn vessel_start_rejects_invalid_config() {
    let dir = tempfile::tempdir().unwrap();
    let config = VesselConfig {
        data_dir: dir.path().to_path_buf(),
        mission: String::new(), // Invalid — empty mission
        ..Default::default()
    };
    let result = Vessel::start_with_registry(config, common::test_registry()).await;
    assert!(result.is_err());
}

// ── T-6: Capability Discovery ──

#[tokio::test]
async fn list_capabilities_returns_registered_connectors() {
    let dir = tempfile::tempdir().unwrap();
    let config = common::test_config(dir.path());
    let vessel = Vessel::start_with_registry(config, common::test_registry())
        .await
        .unwrap();

    let caps = vessel.list_capabilities();
    assert_eq!(caps.len(), 14);

    let names: Vec<&str> = caps.iter().map(|d| d.name.as_str()).collect();
    assert!(names.contains(&"delay"));
    assert!(names.contains(&"fs.read"));
    assert!(names.contains(&"fs.write"));
    assert!(names.contains(&"code.read"));
    assert!(names.contains(&"code.edit"));
    assert!(names.contains(&"code.write"));
    assert!(names.contains(&"code.grep"));
    assert!(names.contains(&"code.glob"));
    assert!(names.contains(&"code.ls"));
    assert!(names.contains(&"code.apply_patch"));
    assert!(names.contains(&"http.request"));
    assert!(names.contains(&"shell.exec"));
    assert!(names.contains(&"sandbox.exec"));
    assert!(names.contains(&"agent.ask_user"));

    vessel.shutdown().await.unwrap();
}

#[tokio::test]
async fn describe_known_connector() {
    let dir = tempfile::tempdir().unwrap();
    let config = common::test_config(dir.path());
    let vessel = Vessel::start_with_registry(config, common::test_registry())
        .await
        .unwrap();

    let desc = vessel.describe("delay");
    assert!(desc.is_some());
    assert_eq!(desc.unwrap().name, "delay");

    vessel.shutdown().await.unwrap();
}

#[tokio::test]
async fn describe_unknown_connector() {
    let dir = tempfile::tempdir().unwrap();
    let config = common::test_config(dir.path());
    let vessel = Vessel::start_with_registry(config, common::test_registry())
        .await
        .unwrap();

    assert!(vessel.describe("nonexistent").is_none());

    vessel.shutdown().await.unwrap();
}

#[tokio::test]
async fn capabilities_match_registry() {
    let dir = tempfile::tempdir().unwrap();
    let config = common::test_config(dir.path());
    let registry = common::test_registry();
    let base_count = registry.len();
    let vessel = Vessel::start_with_registry(config, registry).await.unwrap();

    // Vessel boot registers agent.ask_user (+1 over the base registry)
    assert_eq!(vessel.list_capabilities().len(), base_count + 1);

    vessel.shutdown().await.unwrap();
}

// ── T-7: Tool Invocation ──

#[tokio::test]
async fn invoke_delay_succeeds() {
    let dir = tempfile::tempdir().unwrap();
    let config = common::test_config(dir.path());
    let vessel = Vessel::start_with_registry(config, common::test_registry())
        .await
        .unwrap();

    let result = vessel
        .invoke_tool("delay", serde_json::json!({"duration_ms": 10}))
        .await;
    assert!(result.is_ok(), "invoke_tool failed: {:?}", result.err());

    vessel.shutdown().await.unwrap();
}

#[tokio::test]
async fn invoke_fs_write_then_read() {
    let dir = tempfile::tempdir().unwrap();
    let config = common::test_config(dir.path());
    let vessel = Vessel::start_with_registry(config, common::test_registry())
        .await
        .unwrap();

    let test_file = dir.path().join("test_output.txt");
    let test_content = "hello from exoskeleton";

    // Write
    let write_result = vessel
        .invoke_tool(
            "fs.write",
            serde_json::json!({
                "path": test_file.to_str().unwrap(),
                "content": test_content,
            }),
        )
        .await;
    assert!(
        write_result.is_ok(),
        "fs.write failed: {:?}",
        write_result.err()
    );

    // Read back
    let read_result = vessel
        .invoke_tool(
            "fs.read",
            serde_json::json!({
                "path": test_file.to_str().unwrap(),
            }),
        )
        .await;
    assert!(
        read_result.is_ok(),
        "fs.read failed: {:?}",
        read_result.err()
    );

    let output = read_result.unwrap();
    assert_eq!(output["content"].as_str().unwrap(), test_content);

    vessel.shutdown().await.unwrap();
}

#[tokio::test]
async fn invoke_unknown_tool_fails() {
    let dir = tempfile::tempdir().unwrap();
    let config = common::test_config(dir.path());
    let vessel = Vessel::start_with_registry(config, common::test_registry())
        .await
        .unwrap();

    let result = vessel
        .invoke_tool("nonexistent", serde_json::json!({}))
        .await;
    assert!(result.is_err());

    vessel.shutdown().await.unwrap();
}

// ── T-8: Restart / Recovery ──

#[tokio::test]
async fn restart_both_engines_recover() {
    let dir = tempfile::tempdir().unwrap();
    let config = common::test_config(dir.path());

    // First start + shutdown
    let vessel = Vessel::start_with_registry(config.clone(), common::test_registry())
        .await
        .unwrap();
    vessel.shutdown().await.unwrap();

    // Restart from same data_dir
    let vessel2 = Vessel::start_with_registry(config, common::test_registry())
        .await
        .unwrap();
    vessel2.shutdown().await.unwrap();
}

#[tokio::test]
async fn restart_cognitive_wal_survives() {
    let dir = tempfile::tempdir().unwrap();
    let config = common::test_config(dir.path());

    let vessel = Vessel::start_with_registry(config.clone(), common::test_registry())
        .await
        .unwrap();

    let wal_path = dir.path().join("cognitive-aq").join("wal");
    assert!(wal_path.is_dir(), "WAL dir should exist after first start");

    vessel.shutdown().await.unwrap();

    // Restart
    let vessel2 = Vessel::start_with_registry(config, common::test_registry())
        .await
        .unwrap();
    assert!(
        wal_path.is_dir(),
        "WAL dir should still exist after restart"
    );

    vessel2.shutdown().await.unwrap();
}

#[tokio::test]
async fn restart_wi_context_db_survives() {
    let dir = tempfile::tempdir().unwrap();
    let config = common::test_config(dir.path());

    let vessel = Vessel::start_with_registry(config.clone(), common::test_registry())
        .await
        .unwrap();
    vessel.shutdown().await.unwrap();

    let db_path = dir.path().join("wi").join("context.db");
    assert!(db_path.exists(), "context.db should survive shutdown");

    let vessel2 = Vessel::start_with_registry(config, common::test_registry())
        .await
        .unwrap();
    assert!(db_path.exists(), "context.db should survive restart");

    vessel2.shutdown().await.unwrap();
}

#[tokio::test]
async fn restart_capabilities_available() {
    let dir = tempfile::tempdir().unwrap();
    let config = common::test_config(dir.path());

    let vessel = Vessel::start_with_registry(config.clone(), common::test_registry())
        .await
        .unwrap();
    let caps_before = vessel.list_capabilities().len();
    vessel.shutdown().await.unwrap();

    let vessel2 = Vessel::start_with_registry(config, common::test_registry())
        .await
        .unwrap();
    let caps_after = vessel2.list_capabilities().len();
    assert_eq!(caps_before, caps_after);

    vessel2.shutdown().await.unwrap();
}

// ── T-9: I9 Physical Separation ──

#[tokio::test]
async fn wal_directories_are_separate() {
    let dir = tempfile::tempdir().unwrap();
    let config = common::test_config(dir.path());
    let vessel = Vessel::start_with_registry(config, common::test_registry())
        .await
        .unwrap();

    let cognitive_wal = dir.path().join("cognitive-aq").join("wal");
    let tool_wal = dir.path().join("wi").join("aq").join("wal");

    assert!(cognitive_wal.is_dir());
    assert!(tool_wal.is_dir());
    assert_ne!(
        cognitive_wal.canonicalize().unwrap(),
        tool_wal.canonicalize().unwrap(),
        "WAL directories must be physically separate (I9)"
    );

    vessel.shutdown().await.unwrap();
}

#[tokio::test]
async fn cognitive_dir_has_no_wi_files() {
    let dir = tempfile::tempdir().unwrap();
    let config = common::test_config(dir.path());
    let vessel = Vessel::start_with_registry(config, common::test_registry())
        .await
        .unwrap();

    let cognitive_dir = dir.path().join("cognitive-aq");
    // Should not contain context.db or any WI-specific files
    assert!(
        !cognitive_dir.join("context.db").exists(),
        "cognitive-aq/ must not contain WI context.db"
    );

    vessel.shutdown().await.unwrap();
}

#[tokio::test]
async fn wi_dir_has_no_cognitive_files() {
    let dir = tempfile::tempdir().unwrap();
    let config = common::test_config(dir.path());
    let vessel = Vessel::start_with_registry(config, common::test_registry())
        .await
        .unwrap();

    // The wi/ directory should not contain cognitive-aq files
    let wi_dir = dir.path().join("wi");
    assert!(
        !wi_dir.join("cognitive-aq").exists(),
        "wi/ must not contain cognitive-aq directory"
    );

    vessel.shutdown().await.unwrap();
}

// ── T48: vessel_has_agent_ask_user_connector ──
#[tokio::test]
async fn vessel_has_agent_ask_user_connector() {
    let dir = tempfile::tempdir().unwrap();
    let config = common::test_config(dir.path());
    let vessel = Vessel::start_with_registry(config, common::test_registry())
        .await
        .unwrap();

    let caps = vessel.list_capabilities();
    let names: Vec<&str> = caps.iter().map(|d| d.name.as_str()).collect();
    assert!(
        names.contains(&"agent.ask_user"),
        "fully booted vessel must include agent.ask_user connector, got: {names:?}"
    );

    // Verify it's described correctly
    let desc = vessel.describe("agent.ask_user");
    assert!(desc.is_some(), "agent.ask_user must be describable");
    let desc = desc.unwrap();
    assert!(desc.is_read_only, "agent.ask_user must be marked read_only");

    vessel.shutdown().await.unwrap();
}

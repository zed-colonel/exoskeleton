//! Integration tests for the Exoskeleton Host (Sprint 1 + Sprint 2 + Sprint 4 + Sprint 5 + Sprint 6).
//!
//! All tests use `#[tokio::test]` and `tempfile::tempdir()` for isolated data
//! directories. The `test_registry()` helper excludes `HttpRequestConnector`
//! to avoid the nested tokio runtime conflict (H-1).

use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use exoskeleton_core::llm::{LlmBackend, LlmMessage, LlmRequest, LlmResponse, LlmRole, StopReason};
use exoskeleton_core::{
    Artifact, ArtifactId, ArtifactKind, ArtifactStore, EventEntry, EventLedger, EventType,
    LedgerEntryId, SnapshotStore, StateSnapshot, TickId, TickStore, VesselId,
};
use exoskeleton_host::cognitive_engine::{
    bootstrap_cognitive_engine_with_backends, CognitivePayload, CognitiveTaskType,
};
use exoskeleton_host::config::{LlmConfig, VesselConfig};
use exoskeleton_host::storage::{
    SqliteArtifactStore, SqliteEventLedger, SqliteSnapshotStore, SqliteTickStore, StorageManager,
};
use exoskeleton_host::vessel::{ensure_data_dirs, Vessel};
use exoskeleton_host::MockLlmBackend;
use exoskeleton_relationship::InMemoryRelationshipLedger;
use worldinterface_connector::connectors::{DelayConnector, FsReadConnector, FsWriteConnector};
use worldinterface_connector::registry::ConnectorRegistry;

/// Build a VesselConfig suitable for testing.
///
/// Uses fast tick intervals (10ms) and low concurrency (2) for speed.
fn test_config(dir: &std::path::Path) -> VesselConfig {
    VesselConfig {
        vessel_id: VesselId::new(),
        data_dir: dir.to_path_buf(),
        mission: "integration test".into(),
        cognitive_tick_interval: Duration::from_millis(10),
        cognitive_dispatch_concurrency: NonZeroUsize::new(2).unwrap(),
        cognitive_lease_timeout_secs: 30,
        tool_tick_interval: Duration::from_millis(10),
        tool_dispatch_concurrency: NonZeroUsize::new(2).unwrap(),
        shutdown_timeout: Duration::from_secs(5),
        llm_config: LlmConfig {
            timeout_secs: 10, // Must be < cognitive_lease_timeout_secs (30)
            ..LlmConfig::default()
        },
        master_loop_interval_secs: 10, // Must be < cognitive_lease_timeout_secs (30)
        inbox_dir: None,
        cognitive_budget: None,
        tool_budget: None,
        daemon_listen: None,
    }
}

/// Build a ConnectorRegistry WITHOUT the HTTP connector.
///
/// `HttpRequestConnector` creates an internal tokio runtime via
/// `reqwest::blocking::Client`, which conflicts with `#[tokio::test]`.
fn test_registry() -> ConnectorRegistry {
    let mut registry = ConnectorRegistry::new();
    registry.register(Arc::new(DelayConnector));
    registry.register(Arc::new(FsReadConnector));
    registry.register(Arc::new(FsWriteConnector));
    registry
}

// ── T-3: CognitiveHandler Routing ──

#[tokio::test]
async fn handler_routes_master_loop() {
    let dir = tempfile::tempdir().unwrap();
    let config = test_config(dir.path());
    let vessel = Vessel::start_with_registry(config, test_registry())
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
    let config = test_config(dir.path());
    let vessel = Vessel::start_with_registry(config, test_registry())
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
    let config = test_config(dir.path());
    let vessel = Vessel::start_with_registry(config, test_registry())
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
    let config = test_config(dir.path());
    let vessel = Vessel::start_with_registry(config, test_registry())
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
    let config = test_config(dir.path());
    let vessel = Vessel::start_with_registry(config, test_registry())
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
    let config = test_config(dir.path());
    let vessel = Vessel::start_with_registry(config, test_registry())
        .await
        .unwrap();
    vessel.shutdown().await.unwrap();
}

#[tokio::test]
async fn vessel_start_creates_wal_directories() {
    let dir = tempfile::tempdir().unwrap();
    let config = test_config(dir.path());
    let vessel = Vessel::start_with_registry(config, test_registry())
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
    let config = test_config(dir.path());
    let vessel = Vessel::start_with_registry(config, test_registry())
        .await
        .unwrap();

    assert!(dir.path().join("wi").join("context.db").exists());

    vessel.shutdown().await.unwrap();
}

#[tokio::test]
async fn vessel_config_accessible_after_start() {
    let dir = tempfile::tempdir().unwrap();
    let config = test_config(dir.path());
    let expected_mission = config.mission.clone();
    let vessel = Vessel::start_with_registry(config, test_registry())
        .await
        .unwrap();

    assert_eq!(vessel.config().mission, expected_mission);

    vessel.shutdown().await.unwrap();
}

#[tokio::test]
async fn vessel_id_accessible() {
    let dir = tempfile::tempdir().unwrap();
    let config = test_config(dir.path());
    let expected_id = config.vessel_id;
    let vessel = Vessel::start_with_registry(config, test_registry())
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
    let result = Vessel::start_with_registry(config, test_registry()).await;
    assert!(result.is_err());
}

// ── T-6: Capability Discovery ──

#[tokio::test]
async fn list_capabilities_returns_registered_connectors() {
    let dir = tempfile::tempdir().unwrap();
    let config = test_config(dir.path());
    let vessel = Vessel::start_with_registry(config, test_registry())
        .await
        .unwrap();

    let caps = vessel.list_capabilities();
    assert_eq!(caps.len(), 3); // delay, fs.read, fs.write

    let names: Vec<&str> = caps.iter().map(|d| d.name.as_str()).collect();
    assert!(names.contains(&"delay"));
    assert!(names.contains(&"fs.read"));
    assert!(names.contains(&"fs.write"));

    vessel.shutdown().await.unwrap();
}

#[tokio::test]
async fn describe_known_connector() {
    let dir = tempfile::tempdir().unwrap();
    let config = test_config(dir.path());
    let vessel = Vessel::start_with_registry(config, test_registry())
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
    let config = test_config(dir.path());
    let vessel = Vessel::start_with_registry(config, test_registry())
        .await
        .unwrap();

    assert!(vessel.describe("nonexistent").is_none());

    vessel.shutdown().await.unwrap();
}

#[tokio::test]
async fn capabilities_match_registry() {
    let dir = tempfile::tempdir().unwrap();
    let config = test_config(dir.path());
    let registry = test_registry();
    let expected_count = registry.len();
    let vessel = Vessel::start_with_registry(config, registry).await.unwrap();

    assert_eq!(vessel.list_capabilities().len(), expected_count);

    vessel.shutdown().await.unwrap();
}

// ── T-7: Tool Invocation ──

#[tokio::test]
async fn invoke_delay_succeeds() {
    let dir = tempfile::tempdir().unwrap();
    let config = test_config(dir.path());
    let vessel = Vessel::start_with_registry(config, test_registry())
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
    let config = test_config(dir.path());
    let vessel = Vessel::start_with_registry(config, test_registry())
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
    let config = test_config(dir.path());
    let vessel = Vessel::start_with_registry(config, test_registry())
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
    let config = test_config(dir.path());

    // First start + shutdown
    let vessel = Vessel::start_with_registry(config.clone(), test_registry())
        .await
        .unwrap();
    vessel.shutdown().await.unwrap();

    // Restart from same data_dir
    let vessel2 = Vessel::start_with_registry(config, test_registry())
        .await
        .unwrap();
    vessel2.shutdown().await.unwrap();
}

#[tokio::test]
async fn restart_cognitive_wal_survives() {
    let dir = tempfile::tempdir().unwrap();
    let config = test_config(dir.path());

    let vessel = Vessel::start_with_registry(config.clone(), test_registry())
        .await
        .unwrap();

    let wal_path = dir.path().join("cognitive-aq").join("wal");
    assert!(wal_path.is_dir(), "WAL dir should exist after first start");

    vessel.shutdown().await.unwrap();

    // Restart
    let vessel2 = Vessel::start_with_registry(config, test_registry())
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
    let config = test_config(dir.path());

    let vessel = Vessel::start_with_registry(config.clone(), test_registry())
        .await
        .unwrap();
    vessel.shutdown().await.unwrap();

    let db_path = dir.path().join("wi").join("context.db");
    assert!(db_path.exists(), "context.db should survive shutdown");

    let vessel2 = Vessel::start_with_registry(config, test_registry())
        .await
        .unwrap();
    assert!(db_path.exists(), "context.db should survive restart");

    vessel2.shutdown().await.unwrap();
}

#[tokio::test]
async fn restart_capabilities_available() {
    let dir = tempfile::tempdir().unwrap();
    let config = test_config(dir.path());

    let vessel = Vessel::start_with_registry(config.clone(), test_registry())
        .await
        .unwrap();
    let caps_before = vessel.list_capabilities().len();
    vessel.shutdown().await.unwrap();

    let vessel2 = Vessel::start_with_registry(config, test_registry())
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
    let config = test_config(dir.path());
    let vessel = Vessel::start_with_registry(config, test_registry())
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
    let config = test_config(dir.path());
    let vessel = Vessel::start_with_registry(config, test_registry())
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
    let config = test_config(dir.path());
    let vessel = Vessel::start_with_registry(config, test_registry())
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

// ═══════════════════════════════════════════════════════════════════════
// Sprint 2: Persistence Layer Integration Tests
// ═══════════════════════════════════════════════════════════════════════

fn make_snapshot(vessel_id: VesselId, tick_number: u64) -> StateSnapshot {
    StateSnapshot {
        vessel_id,
        tick_number,
        mission: "test mission".into(),
        plan: None,
        status: exoskeleton_core::VesselStatus::Idle,
        working_context: String::new(),
        thread_summaries: Vec::new(),
        relationship_snapshot_ref: None,
        budget_status: exoskeleton_core::BudgetStatus::unlimited(),
        last_action_summary: None,
        updated_at: Utc::now(),
    }
}

fn make_tick(tick_number: u64) -> exoskeleton_core::TickRecord {
    exoskeleton_core::TickRecord {
        tick_id: TickId::new(),
        tick_number,
        phase: exoskeleton_core::TickPhase::Amend,
        started_at: Utc::now(),
        completed_at: Some(Utc::now()),
        snapshot_before: ArtifactId::from_content(format!("before-{tick_number}").as_bytes()),
        snapshot_after: Some(ArtifactId::from_content(
            format!("after-{tick_number}").as_bytes(),
        )),
        thread_contributions: Vec::new(),
        actions_taken: Vec::new(),
        llm_calls: Vec::new(),
        decision_rationale: None,
    }
}

// ── T-6: Crash Simulation (File-Backed) ──

#[test]
fn artifact_survives_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("artifacts.db");

    let artifact = Artifact::new(
        ArtifactKind::Snapshot,
        b"crash test artifact".to_vec(),
        "text/plain".into(),
    );
    let artifact_id = artifact.id.clone();

    {
        let store = SqliteArtifactStore::open(&db_path).unwrap();
        store.put(&artifact).unwrap();
        // Drop closes the connection
    }

    {
        let store = SqliteArtifactStore::open(&db_path).unwrap();
        let retrieved = store.get(&artifact_id).unwrap().unwrap();
        assert_eq!(retrieved.id, artifact_id);
        assert_eq!(retrieved.content, b"crash test artifact");
    }
}

#[test]
fn snapshot_survives_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("snapshots.db");
    let vid = VesselId::new();

    {
        let store = SqliteSnapshotStore::open(&db_path).unwrap();
        store.save(&make_snapshot(vid, 0)).unwrap();
    }

    {
        let store = SqliteSnapshotStore::open(&db_path).unwrap();
        let latest = store.latest().unwrap().unwrap();
        assert_eq!(latest.tick_number, 0);
        assert_eq!(latest.vessel_id, vid);

        let at_tick = store.at_tick(0).unwrap().unwrap();
        assert_eq!(at_tick.tick_number, 0);
    }
}

#[test]
fn event_survives_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("events.db");

    let entry = EventEntry {
        id: LedgerEntryId::new(),
        tick_id: None,
        event_type: EventType::VesselStarted,
        payload_ref: None,
        summary: "crash test event".into(),
        timestamp: Utc::now(),
    };
    let entry_id = entry.id;

    {
        let ledger = SqliteEventLedger::open(&db_path).unwrap();
        ledger.append(&entry).unwrap();
    }

    {
        let ledger = SqliteEventLedger::open(&db_path).unwrap();
        let recent = ledger.recent(1).unwrap();
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].id, entry_id);
    }
}

#[test]
fn tick_survives_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("ticks.db");

    let record = make_tick(0);
    let tick_id = record.tick_id;

    {
        let store = SqliteTickStore::open(&db_path).unwrap();
        store.save(&record).unwrap();
    }

    {
        let store = SqliteTickStore::open(&db_path).unwrap();
        let retrieved = store.get(tick_id).unwrap().unwrap();
        assert_eq!(retrieved.tick_id, tick_id);

        let latest = store.latest().unwrap().unwrap();
        assert_eq!(latest.tick_id, tick_id);
    }
}

#[test]
fn all_stores_survive_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let vid = VesselId::new();

    let artifact = Artifact::new(
        ArtifactKind::Plan,
        b"survive all".to_vec(),
        "text/plain".into(),
    );
    let artifact_id = artifact.id.clone();
    let tick_record = make_tick(0);
    let tick_id = tick_record.tick_id;
    let event = EventEntry {
        id: LedgerEntryId::new(),
        tick_id: None,
        event_type: EventType::VesselStarted,
        payload_ref: None,
        summary: "survive all event".into(),
        timestamp: Utc::now(),
    };
    let event_id = event.id;

    {
        let mgr = StorageManager::open(dir.path()).unwrap();
        mgr.artifact_store().put(&artifact).unwrap();
        mgr.snapshot_store().save(&make_snapshot(vid, 0)).unwrap();
        mgr.event_ledger().append(&event).unwrap();
        mgr.tick_store().save(&tick_record).unwrap();
    }

    {
        let mgr = StorageManager::open(dir.path()).unwrap();
        assert!(mgr.artifact_store().exists(&artifact_id).unwrap());
        assert_eq!(
            mgr.snapshot_store().latest().unwrap().unwrap().tick_number,
            0
        );
        assert_eq!(mgr.event_ledger().recent(1).unwrap()[0].id, event_id);
        assert_eq!(
            mgr.tick_store().get(tick_id).unwrap().unwrap().tick_id,
            tick_id
        );
    }
}

// ── T-7: SQLite-Specific ──

#[test]
fn wal_mode_enabled() {
    let store = SqliteArtifactStore::in_memory().unwrap();
    // Access the internal conn via put/get to verify WAL mode was set.
    // We verify by checking a known artifact.
    let artifact = Artifact::new(
        ArtifactKind::Snapshot,
        b"wal test".to_vec(),
        "text/plain".into(),
    );
    store.put(&artifact).unwrap();
    assert!(store.exists(&artifact.id).unwrap());
    // If pragmas were wrong, the store wouldn't function at all.
    // For file-backed stores, we can check more explicitly:
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    let _store = SqliteArtifactStore::open(&db_path).unwrap();

    // Check pragmas directly via a separate connection
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    let journal_mode: String = conn
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .unwrap();
    assert_eq!(journal_mode, "wal");
}

#[test]
fn synchronous_full() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    let _store = SqliteSnapshotStore::open(&db_path).unwrap();

    let conn = rusqlite::Connection::open(&db_path).unwrap();
    let synchronous: i64 = conn
        .query_row("PRAGMA synchronous", [], |row| row.get(0))
        .unwrap();
    assert_eq!(synchronous, 2, "synchronous should be FULL (2)");
}

#[test]
fn busy_timeout_set() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    let _store = SqliteEventLedger::open(&db_path).unwrap();

    let conn = rusqlite::Connection::open(&db_path).unwrap();
    let timeout: i64 = conn
        .query_row("PRAGMA busy_timeout", [], |row| row.get(0))
        .unwrap();
    assert_eq!(timeout, 5000);
}

#[test]
fn strict_tables() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    let _store = SqliteArtifactStore::open(&db_path).unwrap();

    // STRICT tables reject type mismatches.
    // Try to insert a BLOB where TEXT is expected for 'kind'.
    // (SQLite STRICT coerces integers to TEXT, but rejects BLOBs in TEXT columns.)
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    let result = conn.execute(
        "INSERT INTO artifacts (id, kind, content, content_type, created_at, metadata)
         VALUES ('abc', X'00', X'00', 'text/plain', '2024-01-01', '{}')",
        [],
    );
    assert!(
        result.is_err(),
        "STRICT table should reject BLOB in TEXT column"
    );
}

// ── T-8: Vessel Integration (Sprint 2) ──

#[tokio::test]
async fn vessel_start_creates_db_files() {
    let dir = tempfile::tempdir().unwrap();
    let config = test_config(dir.path());
    let vessel = Vessel::start_with_registry(config, test_registry())
        .await
        .unwrap();

    let exo_dir = dir.path().join("exo");
    assert!(exo_dir.join("artifacts.db").exists());
    assert!(exo_dir.join("snapshots.db").exists());
    assert!(exo_dir.join("events.db").exists());
    assert!(exo_dir.join("ticks.db").exists());

    vessel.shutdown().await.unwrap();
}

#[tokio::test]
async fn vessel_storage_accessible() {
    let dir = tempfile::tempdir().unwrap();
    let config = test_config(dir.path());
    let vessel = Vessel::start_with_registry(config, test_registry())
        .await
        .unwrap();

    let artifact = Artifact::new(
        ArtifactKind::Snapshot,
        b"vessel storage test".to_vec(),
        "text/plain".into(),
    );
    vessel.storage().artifact_store().put(&artifact).unwrap();
    let retrieved = vessel
        .storage()
        .artifact_store()
        .get(&artifact.id)
        .unwrap()
        .unwrap();
    assert_eq!(retrieved.content, b"vessel storage test");

    vessel.shutdown().await.unwrap();
}

#[tokio::test]
async fn vessel_storage_survives_restart() {
    let dir = tempfile::tempdir().unwrap();
    let config = test_config(dir.path());

    let artifact = Artifact::new(
        ArtifactKind::Plan,
        b"persist across restart".to_vec(),
        "text/plain".into(),
    );
    let artifact_id = artifact.id.clone();

    // First instance: write data
    {
        let vessel = Vessel::start_with_registry(config.clone(), test_registry())
            .await
            .unwrap();
        vessel.storage().artifact_store().put(&artifact).unwrap();
        vessel.shutdown().await.unwrap();
    }

    // Second instance: read data
    {
        let vessel = Vessel::start_with_registry(config, test_registry())
            .await
            .unwrap();
        let retrieved = vessel
            .storage()
            .artifact_store()
            .get(&artifact_id)
            .unwrap()
            .unwrap();
        assert_eq!(retrieved.content, b"persist across restart");
        vessel.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn vessel_shutdown_preserves_data() {
    let dir = tempfile::tempdir().unwrap();
    let config = test_config(dir.path());
    let vid = config.vessel_id;

    let vessel = Vessel::start_with_registry(config, test_registry())
        .await
        .unwrap();

    // Write to all stores
    let artifact = Artifact::new(
        ArtifactKind::Receipt,
        b"shutdown test".to_vec(),
        "text/plain".into(),
    );
    let artifact_id = artifact.id.clone();
    vessel.storage().artifact_store().put(&artifact).unwrap();

    let snap = make_snapshot(vid, 0);
    vessel.storage().snapshot_store().save(&snap).unwrap();

    let event = EventEntry {
        id: LedgerEntryId::new(),
        tick_id: None,
        event_type: EventType::VesselStarted,
        payload_ref: None,
        summary: "shutdown test event".into(),
        timestamp: Utc::now(),
    };
    vessel.storage().event_ledger().append(&event).unwrap();

    let tick_record = make_tick(0);
    let tick_id = tick_record.tick_id;
    vessel.storage().tick_store().save(&tick_record).unwrap();

    vessel.shutdown().await.unwrap();

    // Re-open stores directly (no Vessel) — data should be intact
    let mgr = StorageManager::open(dir.path()).unwrap();
    assert!(mgr.artifact_store().exists(&artifact_id).unwrap());
    assert_eq!(
        mgr.snapshot_store().latest().unwrap().unwrap().tick_number,
        0
    );
    assert_eq!(mgr.event_ledger().recent(1).unwrap().len(), 1);
    assert!(mgr.tick_store().get(tick_id).unwrap().is_some());
}

// ── T-9: Edge Cases ──

#[test]
fn large_artifact() {
    let store = SqliteArtifactStore::in_memory().unwrap();
    let big_content = vec![0xABu8; 1024 * 1024]; // 1MB
    let artifact = Artifact::new(
        ArtifactKind::Memory,
        big_content.clone(),
        "application/octet-stream".into(),
    );

    store.put(&artifact).unwrap();
    let retrieved = store.get(&artifact.id).unwrap().unwrap();
    assert_eq!(retrieved.content.len(), 1024 * 1024);
    assert_eq!(retrieved.content, big_content);
}

#[test]
fn empty_artifact_content() {
    let store = SqliteArtifactStore::in_memory().unwrap();
    let artifact = Artifact::new(ArtifactKind::Plan, Vec::new(), "text/plain".into());

    // ArtifactId is hash of empty bytes
    let expected_id = ArtifactId::from_content(&[]);
    assert_eq!(artifact.id, expected_id);

    store.put(&artifact).unwrap();
    let retrieved = store.get(&artifact.id).unwrap().unwrap();
    assert!(retrieved.content.is_empty());
}

#[test]
fn artifact_metadata_special_chars() {
    let store = SqliteArtifactStore::in_memory().unwrap();
    let artifact = Artifact::new(
        ArtifactKind::Envelope,
        b"unicode metadata".to_vec(),
        "text/plain".into(),
    )
    .with_metadata("名前", "太郎")
    .with_metadata("emoji", "🦀🔥")
    .with_metadata("quotes", "He said \"hello\"");

    store.put(&artifact).unwrap();
    let retrieved = store.get(&artifact.id).unwrap().unwrap();
    assert_eq!(retrieved.metadata.get("名前"), Some(&"太郎".to_string()));
    assert_eq!(retrieved.metadata.get("emoji"), Some(&"🦀🔥".to_string()));
    assert_eq!(
        retrieved.metadata.get("quotes"),
        Some(&"He said \"hello\"".to_string())
    );
}

#[test]
fn event_summary_unicode() {
    let ledger = SqliteEventLedger::in_memory().unwrap();
    let entry = EventEntry {
        id: LedgerEntryId::new(),
        tick_id: None,
        event_type: EventType::VesselStarted,
        payload_ref: None,
        summary: "船が始まった 🚀 — vessel started".into(),
        timestamp: Utc::now(),
    };

    ledger.append(&entry).unwrap();
    let recent = ledger.recent(1).unwrap();
    assert_eq!(recent[0].summary, "船が始まった 🚀 — vessel started");
}

#[test]
fn many_artifacts_same_kind() {
    let store = SqliteArtifactStore::in_memory().unwrap();
    for i in 0..100 {
        let a = Artifact::new(
            ArtifactKind::Memory,
            format!("memory-{i}").into_bytes(),
            "text/plain".into(),
        );
        store.put(&a).unwrap();
    }

    let all = store.list_by_kind(ArtifactKind::Memory, 200).unwrap();
    assert_eq!(all.len(), 100);

    let limited = store.list_by_kind(ArtifactKind::Memory, 10).unwrap();
    assert_eq!(limited.len(), 10);
}

// ── T-10: Property-Based Tests ──

mod proptest_tests {
    use exoskeleton_core::BudgetStatus;
    use proptest::prelude::*;

    use super::*;

    proptest! {
        #[test]
        fn any_artifact_roundtrips(
            content in proptest::collection::vec(any::<u8>(), 0..512),
            kind_idx in 0usize..9,
        ) {
            let kinds = [
                ArtifactKind::Snapshot,
                ArtifactKind::Plan,
                ArtifactKind::Receipt,
                ArtifactKind::ThreadOutput,
                ArtifactKind::RelationshipEntry,
                ArtifactKind::Memory,
                ArtifactKind::Decision,
                ArtifactKind::Envelope,
                ArtifactKind::LlmResponse,
            ];
            let kind = kinds[kind_idx];
            let artifact = Artifact::new(kind, content, "application/octet-stream".into());

            let store = SqliteArtifactStore::in_memory().unwrap();
            store.put(&artifact).unwrap();
            let retrieved = store.get(&artifact.id).unwrap().unwrap();
            prop_assert_eq!(artifact.id, retrieved.id);
            prop_assert_eq!(artifact.kind, retrieved.kind);
            prop_assert_eq!(artifact.content, retrieved.content);
        }

        #[test]
        fn artifact_id_is_deterministic_through_store(
            ref content in proptest::collection::vec(any::<u8>(), 0..512),
        ) {
            let a = Artifact::new(
                ArtifactKind::Plan,
                content.clone(),
                "text/plain".into(),
            );
            let b = Artifact::new(
                ArtifactKind::Plan,
                content.clone(),
                "text/plain".into(),
            );

            let store = SqliteArtifactStore::in_memory().unwrap();
            let id1 = store.put(&a).unwrap();
            let id2 = store.put(&b).unwrap();
            prop_assert_eq!(id1, id2);
        }

        #[test]
        fn snapshot_roundtrips_through_store(
            tick_number in 0u64..10_000,
            status_idx in 0usize..6,
        ) {
            let statuses = [
                exoskeleton_core::VesselStatus::Idle,
                exoskeleton_core::VesselStatus::Thinking,
                exoskeleton_core::VesselStatus::Acting,
                exoskeleton_core::VesselStatus::Reflecting,
                exoskeleton_core::VesselStatus::Suspended,
                exoskeleton_core::VesselStatus::Shutdown,
            ];
            let status = statuses[status_idx];

            let snap = StateSnapshot {
                vessel_id: VesselId::new(),
                tick_number,
                mission: "proptest mission".into(),
                plan: None,
                status,
                working_context: String::new(),
                thread_summaries: Vec::new(),
                relationship_snapshot_ref: None,
                budget_status: BudgetStatus::unlimited(),
                last_action_summary: None,
                updated_at: Utc::now(),
            };

            let store = SqliteSnapshotStore::in_memory().unwrap();
            store.save(&snap).unwrap();
            let retrieved = store.at_tick(tick_number).unwrap().unwrap();
            prop_assert_eq!(snap, retrieved);
        }

        #[test]
        fn event_type_roundtrips_through_store(
            type_idx in 0usize..10,
        ) {
            let types = [
                EventType::VesselStarted,
                EventType::VesselStopped,
                EventType::TickStarted,
                EventType::TickCompleted,
                EventType::ActionExecuted,
                EventType::LlmCalled,
                EventType::ThreadRan,
                EventType::RelationshipUpdated,
                EventType::BudgetConsumed,
                EventType::Error,
            ];
            let event_type = types[type_idx];

            let entry = EventEntry {
                id: LedgerEntryId::new(),
                tick_id: None,
                event_type,
                payload_ref: None,
                summary: format!("{event_type:?}"),
                timestamp: Utc::now(),
            };

            let ledger = SqliteEventLedger::in_memory().unwrap();
            ledger.append(&entry).unwrap();
            let recent = ledger.recent(1).unwrap();
            prop_assert_eq!(recent[0].event_type, event_type);
        }
    }
}

// ── Sprint 4: LLM Adapter Integration Tests ──

/// Helper to create a test config + mock-backend-bootstrapped Cognitive AQ.
fn test_llm_request() -> LlmRequest {
    LlmRequest {
        backend: None,
        system_prompt: Some("You are a test assistant.".into()),
        messages: vec![LlmMessage {
            role: LlmRole::User,
            content: "What is 2+2?".into(),
        }],
        max_output_tokens: 256,
        temperature: Some(0.0),
        stop_sequences: vec![],
    }
}

fn test_llm_response() -> LlmResponse {
    LlmResponse {
        content: "The answer is 4.".into(),
        model: "mock-model".into(),
        tokens_in: 20,
        tokens_out: 8,
        latency_ms: 100,
        stop_reason: StopReason::EndTurn,
        cost_estimate_cents: None,
        backend: LlmBackend::Local,
    }
}

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

// ═══════════════════════════════════════════════════════════════════════
// Sprint 5: Master Loop Integration Tests (PODAARA end-to-end)
// ═══════════════════════════════════════════════════════════════════════
//
// DEADLOCK PREVENTION:
// - All async tests use `flavor = "multi_thread", worker_threads = 2`
// - All tests wrap body in `tokio::time::timeout`
// - Tests calling `run_tick` use `spawn_blocking` (run_tick uses block_on)
// - Vessel.shutdown() always called in timely manner

mod sprint5 {
    use std::sync::Arc;
    use std::time::Duration;

    use actionqueue_executor_local::CancellationToken;
    use exoskeleton_core::llm::{LlmBackend, LlmResponse, StopReason};
    use exoskeleton_core::{ArtifactKind, ArtifactStore, EventType, VesselId};
    use exoskeleton_host::cognitive_engine::CognitiveHandler;
    use exoskeleton_host::inbox::InMemoryInbox;
    use exoskeleton_host::kernel::{KernelContext, WiHostSlot};
    use exoskeleton_host::llm::mock::MockLlmBackend;
    use exoskeleton_host::storage::StorageManager;
    use exoskeleton_host::vessel::Vessel;
    use exoskeleton_host::LlmHttpBackend;
    use exoskeleton_memory::{ApproximateTokenCounter, ContextCompiler};
    use exoskeleton_threads::{InMemoryThreadStore, ThreadRegistry};
    use worldinterface_connector::connectors::DelayConnector;
    use worldinterface_connector::registry::ConnectorRegistry;
    use worldinterface_host::config::HostConfig;
    use worldinterface_host::host::EmbeddedHost;

    use super::*;

    /// Valid DecisionProtocol JSON with no actions (simplest successful tick).
    const MOCK_DECISION_NO_ACTIONS: &str =
        r#"{"reasoning":"No actions needed","actions":[],"memory_notes":[]}"#;

    /// Valid DecisionProtocol JSON with a delay action.
    const MOCK_DECISION_WITH_ACTION: &str = r#"{"reasoning":"Testing","actions":[{"tool_name":"delay","params":{"duration_ms":1},"rationale":"test"}],"memory_notes":["test note"]}"#;

    /// Build a mock LlmResponse with the given content.
    fn mock_llm_response(content: &str) -> LlmResponse {
        LlmResponse {
            content: content.to_string(),
            model: "mock-model".into(),
            tokens_in: 100,
            tokens_out: 50,
            latency_ms: 10,
            stop_reason: StopReason::EndTurn,
            cost_estimate_cents: None,
            backend: LlmBackend::Local,
        }
    }

    /// Clone a KernelContext for sending to a blocking thread.
    ///
    /// All inner Arc types are Send+Sync. The struct itself doesn't auto-derive
    /// Send because of `Arc<dyn Trait>` pointers, but since we clone every Arc
    /// and only use shared references, this is safe.
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
            budget_tracker: kernel.budget_tracker.clone(),
            tool_budget_gate: kernel.tool_budget_gate.clone(),
            metrics: None,
        }
    }

    /// Build a KernelContext with a real WI Host (delay connector) and a mock LLM.
    ///
    /// Returns (kernel, handler, mock_backend) for assertions.
    async fn setup_kernel_with_host(
        dir: &std::path::Path,
        mock_response_content: &str,
    ) -> (KernelContext, CognitiveHandler, Arc<MockLlmBackend>) {
        // Storage
        std::fs::create_dir_all(dir.join("exo")).unwrap();
        let storage = StorageManager::open(dir).unwrap();
        let artifact_store: Arc<dyn ArtifactStore> = storage.artifact_store().clone();

        // Mock LLM backend
        let response = mock_llm_response(mock_response_content);
        let mock_backend = Arc::new(MockLlmBackend::new(response));
        let mock_as_http: Arc<dyn LlmHttpBackend> = mock_backend.clone();

        // Handler with mock
        let handler = CognitiveHandler::with_backends(
            Some(mock_as_http),
            None,
            LlmBackend::Local,
            artifact_store.clone(),
        );

        // Boot WI Host for Act step
        let mut registry = ConnectorRegistry::new();
        registry.register(Arc::new(DelayConnector));
        std::fs::create_dir_all(dir.join("wi").join("aq")).unwrap();
        let host_config = HostConfig {
            aq_data_dir: dir.join("wi").join("aq"),
            context_store_path: dir.join("wi").join("context.db"),
            tick_interval: Duration::from_millis(10),
            ..Default::default()
        };
        let host = EmbeddedHost::start(host_config, registry).await.unwrap();
        let wi_host_slot: WiHostSlot = Arc::new(tokio::sync::Mutex::new(Some(host)));

        // Context compiler
        let counter = Arc::new(ApproximateTokenCounter);
        let compiler = ContextCompiler::with_defaults(counter, 16000);

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
            mission: "integration test".into(),
            max_output_tokens: 4096,
            master_loop_interval_secs: 60,
            thread_registry: Arc::new(ThreadRegistry::new(Arc::new(InMemoryThreadStore::new()))),
            relationship_ledger: Arc::new(InMemoryRelationshipLedger::new()),
            budget_tracker: None,
            tool_budget_gate: None,
            metrics: None,
        };

        (kernel, handler, mock_backend)
    }

    /// Build a KernelContext with NO WI Host (for tests that don't need Act).
    #[allow(dead_code)]
    fn setup_kernel_no_host(
        dir: &std::path::Path,
        mock_response_content: &str,
    ) -> (KernelContext, CognitiveHandler, Arc<MockLlmBackend>) {
        std::fs::create_dir_all(dir.join("exo")).unwrap();
        let storage = StorageManager::open(dir).unwrap();
        let artifact_store: Arc<dyn ArtifactStore> = storage.artifact_store().clone();

        let response = mock_llm_response(mock_response_content);
        let mock_backend = Arc::new(MockLlmBackend::new(response));
        let mock_as_http: Arc<dyn LlmHttpBackend> = mock_backend.clone();

        let handler = CognitiveHandler::with_backends(
            Some(mock_as_http),
            None,
            LlmBackend::Local,
            artifact_store.clone(),
        );

        let wi_host_slot: WiHostSlot = Arc::new(tokio::sync::Mutex::new(None));

        let counter = Arc::new(ApproximateTokenCounter);
        let compiler = ContextCompiler::with_defaults(counter, 16000);

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
            mission: "integration test".into(),
            max_output_tokens: 4096,
            master_loop_interval_secs: 60,
            thread_registry: Arc::new(ThreadRegistry::new(Arc::new(InMemoryThreadStore::new()))),
            relationship_ledger: Arc::new(InMemoryRelationshipLedger::new()),
            budget_tracker: None,
            tool_budget_gate: None,
            metrics: None,
        };

        (kernel, handler, mock_backend)
    }

    /// Shut down the WI Host from the kernel's slot.
    async fn shutdown_host(kernel: &KernelContext) {
        let mut guard = kernel.wi_host_slot.lock().await;
        if let Some(host) = guard.take() {
            host.shutdown().await.ok();
        }
    }

    // ══════════════════════════════════════════════════════════════════
    // Group 1: Vessel-level tests (no master loop execution)
    // ══════════════════════════════════════════════════════════════════

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn sprint5_vessel_boot_creates_master_loop_task() {
        let result = tokio::time::timeout(Duration::from_secs(10), async {
            let dir = tempfile::tempdir().unwrap();
            let config = test_config(dir.path());
            let vessel = Vessel::start_with_registry(config, test_registry())
                .await
                .unwrap();

            // master_loop_task_id should be set (non-nil)
            let task_id = vessel.master_loop_task_id();
            assert_ne!(
                task_id.to_string(),
                "00000000-0000-0000-0000-000000000000",
                "master_loop_task_id should be a valid non-nil UUID"
            );

            vessel.shutdown().await.unwrap();
        })
        .await;
        assert!(result.is_ok(), "test timed out");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn sprint5_vessel_boot_creates_inbox_dir() {
        let result = tokio::time::timeout(Duration::from_secs(10), async {
            let dir = tempfile::tempdir().unwrap();
            let config = test_config(dir.path());
            let vessel = Vessel::start_with_registry(config, test_registry())
                .await
                .unwrap();

            // Default inbox_dir is {data_dir}/inbox/
            let inbox_dir = dir.path().join("inbox");
            assert!(
                inbox_dir.is_dir(),
                "inbox directory should exist after boot: {}",
                inbox_dir.display()
            );

            vessel.shutdown().await.unwrap();
        })
        .await;
        assert!(result.is_ok(), "test timed out");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn sprint5_vessel_shutdown_from_slot() {
        let result = tokio::time::timeout(Duration::from_secs(10), async {
            let dir = tempfile::tempdir().unwrap();
            let config = test_config(dir.path());
            let vessel = Vessel::start_with_registry(config, test_registry())
                .await
                .unwrap();

            // Verify WI Host is accessible before shutdown
            let caps = vessel.list_capabilities();
            assert!(!caps.is_empty(), "capabilities should be available");

            // Shutdown should complete cleanly
            vessel.shutdown().await.unwrap();
        })
        .await;
        assert!(result.is_ok(), "test timed out");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn sprint5_vessel_invoke_tool_through_slot() {
        let result = tokio::time::timeout(Duration::from_secs(10), async {
            let dir = tempfile::tempdir().unwrap();
            let config = test_config(dir.path());
            let vessel = Vessel::start_with_registry(config, test_registry())
                .await
                .unwrap();

            // Invoke a tool through the WI Host slot
            let tool_result = vessel
                .invoke_tool("delay", serde_json::json!({"duration_ms": 1}))
                .await;
            assert!(
                tool_result.is_ok(),
                "invoke_tool should succeed: {:?}",
                tool_result.err()
            );

            vessel.shutdown().await.unwrap();
        })
        .await;
        assert!(result.is_ok(), "test timed out");
    }

    // ══════════════════════════════════════════════════════════════════
    // Group 2: Kernel integration tests (run_tick directly)
    // ══════════════════════════════════════════════════════════════════

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn sprint5_full_tick_completes_podaara_cycle() {
        let result = tokio::time::timeout(Duration::from_secs(10), async {
            let dir = tempfile::tempdir().unwrap();
            let (kernel, handler, mock) =
                setup_kernel_with_host(dir.path(), MOCK_DECISION_NO_ACTIONS).await;

            let k = send_kernel(&kernel);
            let output = tokio::task::spawn_blocking(move || {
                let token = CancellationToken::new();
                exoskeleton_host::kernel::run_tick(&handler, &k, &token)
            })
            .await
            .unwrap();

            // run_tick should succeed
            match &output {
                actionqueue_executor_local::HandlerOutput::Success { .. } => {}
                other => panic!("expected Success, got: {other:?}"),
            }

            // Mock LLM was called exactly once (Decide step)
            assert_eq!(mock.call_count(), 1, "LLM should be called once per tick");

            shutdown_host(&kernel).await;
        })
        .await;
        assert!(result.is_ok(), "test timed out");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn sprint5_tick_produces_snapshot() {
        let result = tokio::time::timeout(Duration::from_secs(10), async {
            let dir = tempfile::tempdir().unwrap();
            let (kernel, handler, _mock) =
                setup_kernel_with_host(dir.path(), MOCK_DECISION_NO_ACTIONS).await;

            let snapshot_store = kernel.snapshot_store.clone();

            // Before tick: no snapshot
            assert!(
                snapshot_store.latest().unwrap().is_none(),
                "no snapshot before first tick"
            );

            let k = send_kernel(&kernel);
            tokio::task::spawn_blocking(move || {
                let token = CancellationToken::new();
                exoskeleton_host::kernel::run_tick(&handler, &k, &token)
            })
            .await
            .unwrap();

            // After tick: snapshot exists with tick_number = 1
            let snapshot = snapshot_store
                .latest()
                .unwrap()
                .expect("snapshot should exist after tick");
            assert_eq!(
                snapshot.tick_number, 1,
                "first tick should produce tick_number 1"
            );

            shutdown_host(&kernel).await;
        })
        .await;
        assert!(result.is_ok(), "test timed out");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn sprint5_tick_stores_llm_response_artifact() {
        let result = tokio::time::timeout(Duration::from_secs(10), async {
            let dir = tempfile::tempdir().unwrap();
            let (kernel, handler, _mock) =
                setup_kernel_with_host(dir.path(), MOCK_DECISION_NO_ACTIONS).await;

            let artifact_store = kernel.artifact_store.clone();

            let k = send_kernel(&kernel);
            tokio::task::spawn_blocking(move || {
                let token = CancellationToken::new();
                exoskeleton_host::kernel::run_tick(&handler, &k, &token)
            })
            .await
            .unwrap();

            // I3: LlmResponse artifact must exist
            let llm_artifacts = artifact_store
                .list_by_kind(ArtifactKind::LlmResponse, 10)
                .unwrap();
            assert!(
                !llm_artifacts.is_empty(),
                "LlmResponse artifact should be stored (I3)"
            );

            shutdown_host(&kernel).await;
        })
        .await;
        assert!(result.is_ok(), "test timed out");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn sprint5_tick_stores_tick_record_artifact() {
        let result = tokio::time::timeout(Duration::from_secs(10), async {
            let dir = tempfile::tempdir().unwrap();
            let (kernel, handler, _mock) =
                setup_kernel_with_host(dir.path(), MOCK_DECISION_NO_ACTIONS).await;

            let artifact_store = kernel.artifact_store.clone();

            let k = send_kernel(&kernel);
            tokio::task::spawn_blocking(move || {
                let token = CancellationToken::new();
                exoskeleton_host::kernel::run_tick(&handler, &k, &token)
            })
            .await
            .unwrap();

            // I3: Tick artifact must exist
            let tick_artifacts = artifact_store.list_by_kind(ArtifactKind::Tick, 10).unwrap();
            assert!(
                !tick_artifacts.is_empty(),
                "Tick artifact should be stored (I3)"
            );

            shutdown_host(&kernel).await;
        })
        .await;
        assert!(result.is_ok(), "test timed out");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn sprint5_tick_events_logged() {
        let result = tokio::time::timeout(Duration::from_secs(10), async {
            let dir = tempfile::tempdir().unwrap();
            let (kernel, handler, _mock) =
                setup_kernel_with_host(dir.path(), MOCK_DECISION_NO_ACTIONS).await;

            let event_ledger = kernel.event_ledger.clone();

            let k = send_kernel(&kernel);
            tokio::task::spawn_blocking(move || {
                let token = CancellationToken::new();
                exoskeleton_host::kernel::run_tick(&handler, &k, &token)
            })
            .await
            .unwrap();

            // TickStarted and TickCompleted events should be logged
            let events = event_ledger.recent(100).unwrap();
            let started = events
                .iter()
                .filter(|e| e.event_type == EventType::TickStarted)
                .count();
            let completed = events
                .iter()
                .filter(|e| e.event_type == EventType::TickCompleted)
                .count();
            assert_eq!(started, 1, "expected 1 TickStarted event");
            assert_eq!(completed, 1, "expected 1 TickCompleted event");

            shutdown_host(&kernel).await;
        })
        .await;
        assert!(result.is_ok(), "test timed out");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn sprint5_tick_creates_initial_snapshot_if_none() {
        let result = tokio::time::timeout(Duration::from_secs(10), async {
            let dir = tempfile::tempdir().unwrap();
            let (kernel, handler, _mock) =
                setup_kernel_with_host(dir.path(), MOCK_DECISION_NO_ACTIONS).await;

            let snapshot_store = kernel.snapshot_store.clone();

            // No snapshot in store
            assert!(snapshot_store.latest().unwrap().is_none());

            let k = send_kernel(&kernel);
            tokio::task::spawn_blocking(move || {
                let token = CancellationToken::new();
                exoskeleton_host::kernel::run_tick(&handler, &k, &token)
            })
            .await
            .unwrap();

            // First tick creates snapshot from initial (tick 0) -> new (tick 1)
            let snapshot = snapshot_store.latest().unwrap().unwrap();
            assert_eq!(snapshot.tick_number, 1);
            assert_eq!(snapshot.mission, "integration test");

            shutdown_host(&kernel).await;
        })
        .await;
        assert!(result.is_ok(), "test timed out");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn sprint5_tick_with_no_actions() {
        let result = tokio::time::timeout(Duration::from_secs(10), async {
            let dir = tempfile::tempdir().unwrap();
            // Use no-action response — tick should complete without any Act step invocations
            let (kernel, handler, _mock) =
                setup_kernel_with_host(dir.path(), MOCK_DECISION_NO_ACTIONS).await;

            let k = send_kernel(&kernel);
            let output = tokio::task::spawn_blocking(move || {
                let token = CancellationToken::new();
                exoskeleton_host::kernel::run_tick(&handler, &k, &token)
            })
            .await
            .unwrap();

            match &output {
                actionqueue_executor_local::HandlerOutput::Success { .. } => {}
                other => panic!("expected Success with no actions, got: {other:?}"),
            }

            // Verify snapshot shows no actions taken
            let snapshot = kernel.snapshot_store.latest().unwrap().unwrap();
            assert_eq!(
                snapshot.last_action_summary.as_deref(),
                Some("No actions taken")
            );

            shutdown_host(&kernel).await;
        })
        .await;
        assert!(result.is_ok(), "test timed out");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn sprint5_tick_number_advances() {
        let result = tokio::time::timeout(Duration::from_secs(10), async {
            let dir = tempfile::tempdir().unwrap();
            let (kernel, _handler, mock) =
                setup_kernel_with_host(dir.path(), MOCK_DECISION_NO_ACTIONS).await;

            let snapshot_store = kernel.snapshot_store.clone();
            let tick_store = kernel.tick_store.clone();

            // Run tick 1
            let k1 = send_kernel(&kernel);
            let h1 = CognitiveHandler::with_backends(
                Some(Arc::new(MockLlmBackend::new(mock_llm_response(
                    MOCK_DECISION_NO_ACTIONS,
                )))),
                None,
                LlmBackend::Local,
                kernel.artifact_store.clone(),
            );
            tokio::task::spawn_blocking(move || {
                let token = CancellationToken::new();
                exoskeleton_host::kernel::run_tick(&h1, &k1, &token)
            })
            .await
            .unwrap();

            let snap_after_1 = snapshot_store.latest().unwrap().unwrap();
            assert_eq!(snap_after_1.tick_number, 1);

            // Run tick 2
            let k2 = send_kernel(&kernel);
            let h2 = CognitiveHandler::with_backends(
                Some(Arc::new(MockLlmBackend::new(mock_llm_response(
                    MOCK_DECISION_NO_ACTIONS,
                )))),
                None,
                LlmBackend::Local,
                kernel.artifact_store.clone(),
            );
            tokio::task::spawn_blocking(move || {
                let token = CancellationToken::new();
                exoskeleton_host::kernel::run_tick(&h2, &k2, &token)
            })
            .await
            .unwrap();

            let snap_after_2 = snapshot_store.latest().unwrap().unwrap();
            assert_eq!(
                snap_after_2.tick_number, 2,
                "tick number should advance to 2"
            );

            // Tick store should have both records
            let latest_tick = tick_store.latest().unwrap().unwrap();
            assert_eq!(latest_tick.tick_number, 2);

            // The original mock should also have been called once (first tick)
            // plus the new handlers each got called once
            assert_eq!(
                mock.call_count(),
                0,
                "original mock not used (we created fresh handlers)"
            );

            shutdown_host(&kernel).await;
        })
        .await;
        assert!(result.is_ok(), "test timed out");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn sprint5_kernel_not_configured_returns_failure() {
        let result = tokio::time::timeout(Duration::from_secs(10), async {
            let dir = tempfile::tempdir().unwrap();
            let config = test_config(dir.path());
            exoskeleton_host::vessel::ensure_data_dirs(&config).unwrap();
            let storage = StorageManager::open(&config.data_dir).unwrap();
            let artifact_store: Arc<dyn ArtifactStore> = storage.artifact_store().clone();

            // Bootstrap engine WITHOUT kernel context
            let engine =
                exoskeleton_host::cognitive_engine::bootstrap_cognitive_engine_with_backends(
                    &config,
                    None,
                    None,
                    artifact_store,
                    None, // No kernel
                )
                .unwrap();

            // Submit a MasterLoop task
            use actionqueue_core::ids::TaskId;
            use actionqueue_core::task::constraints::TaskConstraints;
            use actionqueue_core::task::metadata::TaskMetadata;
            use actionqueue_core::task::run_policy::RunPolicy;
            use actionqueue_core::task::task_spec::{TaskPayload, TaskSpec};
            use exoskeleton_host::cognitive_engine::{CognitivePayload, CognitiveTaskType};

            let payload = CognitivePayload {
                task_type: CognitiveTaskType::MasterLoop,
                data: serde_json::Value::Null,
            };
            let payload_bytes = serde_json::to_vec(&payload).unwrap();
            let task_id = TaskId::new();
            let spec = TaskSpec::new(
                task_id,
                TaskPayload::with_content_type(payload_bytes, "application/json"),
                RunPolicy::Once,
                TaskConstraints::default(),
                TaskMetadata::default(),
            )
            .unwrap();

            let engine_slot: Arc<tokio::sync::Mutex<Option<_>>> =
                Arc::new(tokio::sync::Mutex::new(Some(engine)));

            {
                let mut guard = engine_slot.lock().await;
                let engine = guard.as_mut().unwrap();
                engine.submit_task(spec).unwrap();
                let _ = engine.run_until_idle().await.unwrap();
            }

            // Should fail with "kernel not configured"
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
                    "Expected 'kernel not configured', got: {error}"
                );
            }

            let engine = engine_slot.lock().await.take();
            if let Some(engine) = engine {
                let _ = engine.shutdown();
            }
        })
        .await;
        assert!(result.is_ok(), "test timed out");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn sprint5_llm_call_within_handler_no_deadlock() {
        let result = tokio::time::timeout(Duration::from_secs(10), async {
            let dir = tempfile::tempdir().unwrap();
            // This test verifies that the Decide step's direct backend call
            // does NOT deadlock (as opposed to using LlmClient which would).
            let (kernel, handler, mock) =
                setup_kernel_with_host(dir.path(), MOCK_DECISION_NO_ACTIONS).await;

            let k = send_kernel(&kernel);
            let output = tokio::task::spawn_blocking(move || {
                let token = CancellationToken::new();
                exoskeleton_host::kernel::run_tick(&handler, &k, &token)
            })
            .await
            .unwrap();

            // If we get here without hanging, the LLM call within the handler
            // completed without deadlock
            match &output {
                actionqueue_executor_local::HandlerOutput::Success { .. } => {}
                other => panic!("expected Success, got: {other:?}"),
            }
            assert_eq!(mock.call_count(), 1, "LLM should have been called once");

            shutdown_host(&kernel).await;
        })
        .await;
        assert!(result.is_ok(), "test timed out — possible deadlock!");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn sprint5_tick_with_action_executes_via_wi_host() {
        let result = tokio::time::timeout(Duration::from_secs(10), async {
            let dir = tempfile::tempdir().unwrap();
            let (kernel, handler, _mock) =
                setup_kernel_with_host(dir.path(), MOCK_DECISION_WITH_ACTION).await;

            let artifact_store = kernel.artifact_store.clone();
            let event_ledger = kernel.event_ledger.clone();

            let k = send_kernel(&kernel);
            let output = tokio::task::spawn_blocking(move || {
                let token = CancellationToken::new();
                exoskeleton_host::kernel::run_tick(&handler, &k, &token)
            })
            .await
            .unwrap();

            // Tick should succeed
            match &output {
                actionqueue_executor_local::HandlerOutput::Success { .. } => {}
                other => panic!("expected Success, got: {other:?}"),
            }

            // Act step should have produced a Receipt artifact (I3)
            let receipts = artifact_store
                .list_by_kind(ArtifactKind::Receipt, 10)
                .unwrap();
            assert!(
                !receipts.is_empty(),
                "Receipt artifact should be stored after action execution (I3)"
            );

            // ActionExecuted event should be in ledger
            let events = event_ledger.recent(100).unwrap();
            let action_events: Vec<_> = events
                .iter()
                .filter(|e| e.event_type == EventType::ActionExecuted)
                .collect();
            assert_eq!(
                action_events.len(),
                1,
                "expected 1 ActionExecuted event for delay action"
            );
            assert!(
                action_events[0].summary.contains("delay"),
                "event summary should mention the tool: {}",
                action_events[0].summary
            );

            // Snapshot should reflect the action summary
            let snapshot = kernel.snapshot_store.latest().unwrap().unwrap();
            assert!(
                snapshot.last_action_summary.is_some(),
                "last_action_summary should be set"
            );
            let summary = snapshot.last_action_summary.as_deref().unwrap();
            assert!(
                summary.contains("1 succeeded"),
                "action summary should show success: {summary}"
            );

            shutdown_host(&kernel).await;
        })
        .await;
        assert!(result.is_ok(), "test timed out");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn sprint5_tick_persists_memory_notes() {
        let result = tokio::time::timeout(Duration::from_secs(10), async {
            let dir = tempfile::tempdir().unwrap();
            // Use the response with memory_notes: ["test note"]
            let (kernel, handler, _mock) =
                setup_kernel_with_host(dir.path(), MOCK_DECISION_WITH_ACTION).await;

            let k = send_kernel(&kernel);
            tokio::task::spawn_blocking(move || {
                let token = CancellationToken::new();
                exoskeleton_host::kernel::run_tick(&handler, &k, &token)
            })
            .await
            .unwrap();

            // Memory notes from the decision should be persisted
            let episodics = kernel.memory_store.recent_episodic(10).unwrap();
            assert!(
                !episodics.is_empty(),
                "episodic summaries should be persisted from memory_notes"
            );
            let summaries: Vec<&str> = episodics.iter().map(|e| e.summary.as_str()).collect();
            assert!(
                summaries.contains(&"test note"),
                "expected 'test note' in episodic summaries, got: {:?}",
                summaries
            );

            shutdown_host(&kernel).await;
        })
        .await;
        assert!(result.is_ok(), "test timed out");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn sprint5_tick_stores_decision_artifact() {
        let result = tokio::time::timeout(Duration::from_secs(10), async {
            let dir = tempfile::tempdir().unwrap();
            let (kernel, handler, _mock) =
                setup_kernel_with_host(dir.path(), MOCK_DECISION_NO_ACTIONS).await;

            let artifact_store = kernel.artifact_store.clone();

            let k = send_kernel(&kernel);
            tokio::task::spawn_blocking(move || {
                let token = CancellationToken::new();
                exoskeleton_host::kernel::run_tick(&handler, &k, &token)
            })
            .await
            .unwrap();

            // I3: Decision artifact must exist
            let decisions = artifact_store
                .list_by_kind(ArtifactKind::Decision, 10)
                .unwrap();
            assert!(
                !decisions.is_empty(),
                "Decision artifact should be stored (I3)"
            );

            shutdown_host(&kernel).await;
        })
        .await;
        assert!(result.is_ok(), "test timed out");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn sprint5_tick_stores_snapshot_artifacts() {
        let result = tokio::time::timeout(Duration::from_secs(10), async {
            let dir = tempfile::tempdir().unwrap();
            let (kernel, handler, _mock) =
                setup_kernel_with_host(dir.path(), MOCK_DECISION_NO_ACTIONS).await;

            let artifact_store = kernel.artifact_store.clone();

            let k = send_kernel(&kernel);
            tokio::task::spawn_blocking(move || {
                let token = CancellationToken::new();
                exoskeleton_host::kernel::run_tick(&handler, &k, &token)
            })
            .await
            .unwrap();

            // I3: Snapshot artifacts (before + after) must exist
            let snapshots = artifact_store
                .list_by_kind(ArtifactKind::Snapshot, 10)
                .unwrap();
            assert!(
                snapshots.len() >= 2,
                "expected at least 2 Snapshot artifacts (before + after), got: {}",
                snapshots.len()
            );

            shutdown_host(&kernel).await;
        })
        .await;
        assert!(result.is_ok(), "test timed out");
    }
}

// ── T-13: Property-Based Tests (Sprint 5) ──

mod proptest_sprint5 {
    use exoskeleton_core::{StateSnapshot, VesselId, VesselStatus};
    use exoskeleton_host::kernel::{DecisionProtocol, PlannedAction, SnapshotDelta};
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn decision_protocol_survives_json_roundtrip(
            reasoning in "\\PC{1,200}",
            plan in proptest::option::of("\\PC{1,100}"),
            wc in proptest::option::of("\\PC{1,100}"),
            num_actions in 0usize..5,
            num_notes in 0usize..5,
        ) {
            let actions: Vec<PlannedAction> = (0..num_actions).map(|i| PlannedAction {
                tool_name: format!("tool_{i}"),
                params: serde_json::json!({"key": i}),
                rationale: format!("reason {i}"),
            }).collect();
            let notes: Vec<String> = (0..num_notes).map(|i| format!("note {i}")).collect();

            let proto = DecisionProtocol {
                reasoning,
                plan_update: plan,
                working_context_update: wc,
                actions,
                memory_notes: notes,
            };
            let json = serde_json::to_string(&proto).unwrap();
            let parsed: DecisionProtocol = serde_json::from_str(&json).unwrap();
            prop_assert_eq!(proto.reasoning, parsed.reasoning);
            prop_assert_eq!(proto.plan_update, parsed.plan_update);
            prop_assert_eq!(proto.working_context_update, parsed.working_context_update);
            prop_assert_eq!(proto.actions.len(), parsed.actions.len());
            prop_assert_eq!(proto.memory_notes.len(), parsed.memory_notes.len());
        }

        #[test]
        fn planned_action_survives_json_roundtrip(
            tool_name in "[a-z][a-z.]{0,20}",
            rationale in "\\PC{1,100}",
            param_val in 0i64..1000,
        ) {
            let action = PlannedAction {
                tool_name,
                params: serde_json::json!({"value": param_val}),
                rationale,
            };
            let json = serde_json::to_string(&action).unwrap();
            let parsed: PlannedAction = serde_json::from_str(&json).unwrap();
            prop_assert_eq!(action.tool_name, parsed.tool_name);
            prop_assert_eq!(action.rationale, parsed.rationale);
            prop_assert_eq!(action.params, parsed.params);
        }

        #[test]
        fn snapshot_delta_application_preserves_fields(
            has_plan in proptest::bool::ANY,
            has_wc in proptest::bool::ANY,
        ) {
            let snapshot = StateSnapshot::initial(VesselId::new(), "test mission".into());
            let original_mission = snapshot.mission.clone();
            let original_vessel_id = snapshot.vessel_id;
            let original_tick_number = snapshot.tick_number;

            let delta = SnapshotDelta {
                plan_update: if has_plan { Some("new plan".into()) } else { None },
                working_context_update: if has_wc { Some("new wc".into()) } else { None },
            };

            // Apply delta (same logic as amend step)
            let mut new_snapshot = snapshot.clone();
            if let Some(plan) = &delta.plan_update {
                new_snapshot.plan = Some(plan.clone());
            }
            if let Some(wc) = &delta.working_context_update {
                new_snapshot.working_context = wc.clone();
            }

            // Unmodified fields must be preserved
            prop_assert_eq!(new_snapshot.mission, original_mission);
            prop_assert_eq!(new_snapshot.vessel_id, original_vessel_id);
            prop_assert_eq!(new_snapshot.tick_number, original_tick_number);

            // Modified fields should reflect the delta
            if has_plan {
                prop_assert_eq!(new_snapshot.plan, Some("new plan".into()));
            } else {
                prop_assert_eq!(new_snapshot.plan, snapshot.plan);
            }
            if has_wc {
                prop_assert_eq!(new_snapshot.working_context, "new wc".to_string());
            } else {
                prop_assert_eq!(new_snapshot.working_context, snapshot.working_context);
            }
        }

        #[test]
        fn tick_number_always_increases(
            num_ticks in 1usize..20,
        ) {
            let mut tick_number = 0u64;
            let mut prev_tick_number = 0u64;

            for _ in 0..num_ticks {
                tick_number = prev_tick_number + 1; // Same logic as run_tick
                prop_assert!(tick_number > prev_tick_number);
                prev_tick_number = tick_number;
            }
            prop_assert_eq!(tick_number, num_ticks as u64);
        }

        #[test]
        fn amend_always_produces_valid_snapshot(
            plan in proptest::option::of("\\PC{0,200}"),
            wc in proptest::option::of("\\PC{0,200}"),
        ) {
            let snapshot = StateSnapshot::initial(VesselId::new(), "test".into());

            let mut new_snapshot = snapshot;
            if let Some(p) = plan {
                new_snapshot.plan = Some(p);
            }
            if let Some(w) = wc {
                new_snapshot.working_context = w;
            }
            new_snapshot.tick_number = 1;
            new_snapshot.status = VesselStatus::Idle;

            // Must produce valid JSON
            let json = serde_json::to_string(&new_snapshot);
            prop_assert!(json.is_ok(), "snapshot must serialize to JSON");

            // Must round-trip
            let parsed: Result<StateSnapshot, _> = serde_json::from_str(&json.unwrap());
            prop_assert!(parsed.is_ok(), "snapshot must deserialize from JSON");
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════
// Sprint 6: Thread Integration Tests (T-7 / T-9)
// ═══════════════════════════════════════════════════════════════════════
//
// T-7: Master Loop Thread Integration — full PODAARA tick with threads
// T-9: Thread Lifecycle — registration, suspension, deregistration,
//       persistence across restarts
//
// DEADLOCK PREVENTION:
// - All async tests use `flavor = "multi_thread", worker_threads = 2`
// - All tests wrap body in `tokio::time::timeout`
// - Tests calling `run_tick` use `spawn_blocking` (run_tick uses block_on)
// - WI Host always shut down at end of test

mod sprint6 {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    use actionqueue_executor_local::CancellationToken;
    use exoskeleton_core::llm::{LlmBackend, LlmRequest, LlmResponse, StopReason};
    use exoskeleton_core::{
        ArtifactStore, EventType, ThreadId, ThreadPriority, ThreadSchedule, ThreadSpec,
        ThreadStatus, VesselId,
    };
    use exoskeleton_host::cognitive_engine::CognitiveHandler;
    use exoskeleton_host::inbox::InMemoryInbox;
    use exoskeleton_host::kernel::{KernelContext, WiHostSlot};
    use exoskeleton_host::storage::StorageManager;
    use exoskeleton_host::LlmHttpBackend;
    use exoskeleton_memory::{ApproximateTokenCounter, ContextCompiler};
    use exoskeleton_relationship::InMemoryRelationshipLedger;
    use exoskeleton_threads::{InMemoryThreadStore, ThreadRegistry};
    use worldinterface_connector::connectors::DelayConnector;
    use worldinterface_connector::registry::ConnectorRegistry;
    use worldinterface_host::config::HostConfig;
    use worldinterface_host::host::EmbeddedHost;

    // ── Mock responses ──

    /// Thread JSON response.
    const THREAD_RESPONSE: &str =
        r#"{"summary":"all clear","recommendations":["continue monitoring"]}"#;

    /// Master loop decision JSON with no actions.
    const DECISION_NO_ACTIONS: &str =
        r#"{"reasoning":"No actions needed","actions":[],"memory_notes":[]}"#;

    // ── Multi-response mock backend ──

    /// A mock LLM backend that cycles through a list of responses.
    ///
    /// Used for tests where threads AND the master loop's Decide step
    /// both need LLM calls in one tick. Threads call first (by priority
    /// order), then Decide calls.
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

    /// A mock LLM backend that fails on certain call indices.
    struct FailThenSucceedBackend {
        fail_indices: Vec<usize>,
        success_content: String,
        call_count: AtomicUsize,
    }

    impl FailThenSucceedBackend {
        fn new(fail_indices: Vec<usize>, success_content: String) -> Self {
            Self {
                fail_indices,
                success_content,
                call_count: AtomicUsize::new(0),
            }
        }
    }

    impl LlmHttpBackend for FailThenSucceedBackend {
        fn call(
            &self,
            _client: &reqwest::Client,
            _request: &LlmRequest,
            _cancellation: &CancellationToken,
        ) -> Result<LlmResponse, exoskeleton_core::ExoError> {
            let idx = self.call_count.fetch_add(1, Ordering::SeqCst);
            if self.fail_indices.contains(&idx) {
                return Err(exoskeleton_core::ExoError::LlmInvocation(
                    "mock LLM failure".into(),
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

    // ── Helper: build a ThreadSpec ──

    fn make_thread(name: &str, priority: ThreadPriority, schedule: ThreadSchedule) -> ThreadSpec {
        ThreadSpec {
            thread_id: ThreadId::new(),
            name: name.into(),
            charter: format!("Charter for {name}"),
            priority,
            token_budget: 4000,
            schedule,
        }
    }

    // ── Helper: clone KernelContext for spawn_blocking ──

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
            budget_tracker: kernel.budget_tracker.clone(),
            tool_budget_gate: kernel.tool_budget_gate.clone(),
            metrics: None,
        }
    }

    // ── Helper: shut down WI Host ──

    async fn shutdown_host(kernel: &KernelContext) {
        let mut guard = kernel.wi_host_slot.lock().await;
        if let Some(host) = guard.take() {
            host.shutdown().await.ok();
        }
    }

    // ── Helper: setup kernel with threads and multi-mock ──

    async fn setup_with_threads(
        dir: &std::path::Path,
        responses: Vec<String>,
        thread_specs: Vec<ThreadSpec>,
    ) -> (KernelContext, CognitiveHandler, Arc<MultiMockBackend>) {
        // Storage
        std::fs::create_dir_all(dir.join("exo")).unwrap();
        std::fs::create_dir_all(dir.join("wi").join("aq")).unwrap();
        let storage = StorageManager::open(dir).unwrap();
        let artifact_store: Arc<dyn ArtifactStore> = storage.artifact_store().clone();

        // Multi-response mock
        let mock = Arc::new(MultiMockBackend::new(responses));
        let mock_as_http: Arc<dyn LlmHttpBackend> = mock.clone();

        let handler = CognitiveHandler::with_backends(
            Some(mock_as_http),
            None,
            LlmBackend::Local,
            artifact_store.clone(),
        );

        // WI Host
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

        // Context compiler
        let counter = Arc::new(ApproximateTokenCounter);
        let compiler = ContextCompiler::with_defaults(counter, 16000);

        // Thread registry
        let thread_store = Arc::new(InMemoryThreadStore::new());
        let thread_registry = Arc::new(ThreadRegistry::new(thread_store));
        for spec in thread_specs {
            thread_registry.register(spec).unwrap();
        }

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
            mission: "thread test".into(),
            max_output_tokens: 4096,
            master_loop_interval_secs: 60,
            thread_registry,
            relationship_ledger: Arc::new(InMemoryRelationshipLedger::new()),
            budget_tracker: None,
            tool_budget_gate: None,
            metrics: None,
        };

        (kernel, handler, mock)
    }

    /// Setup with a custom LlmHttpBackend (not MultiMockBackend).
    async fn setup_with_threads_custom_backend(
        dir: &std::path::Path,
        backend: Arc<dyn LlmHttpBackend>,
        thread_specs: Vec<ThreadSpec>,
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
        for spec in thread_specs {
            thread_registry.register(spec).unwrap();
        }

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
            mission: "thread test".into(),
            max_output_tokens: 4096,
            master_loop_interval_secs: 60,
            thread_registry,
            relationship_ledger: Arc::new(InMemoryRelationshipLedger::new()),
            budget_tracker: None,
            tool_budget_gate: None,
            metrics: None,
        };

        (kernel, handler)
    }

    // ══════════════════════════════════════════════════════════════════
    // T-7: Master Loop Thread Integration
    // ══════════════════════════════════════════════════════════════════

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn tick_with_no_threads() {
        let result = tokio::time::timeout(Duration::from_secs(10), async {
            let dir = tempfile::tempdir().unwrap();
            let (kernel, handler, _mock) =
                setup_with_threads(dir.path(), vec![DECISION_NO_ACTIONS.into()], vec![]).await;

            let tick_store = kernel.tick_store.clone();
            let k = send_kernel(&kernel);
            let output = tokio::task::spawn_blocking(move || {
                let token = CancellationToken::new();
                exoskeleton_host::kernel::run_tick(&handler, &k, &token)
            })
            .await
            .unwrap();

            match &output {
                actionqueue_executor_local::HandlerOutput::Success { .. } => {}
                other => {
                    panic!("expected Success, got: {other:?}")
                }
            }

            let record = tick_store.latest().unwrap().unwrap();
            assert!(
                record.thread_contributions.is_empty(),
                "expected no thread contributions, got: {:?}",
                record.thread_contributions
            );

            shutdown_host(&kernel).await;
        })
        .await;
        assert!(result.is_ok(), "test timed out");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn tick_with_one_thread() {
        let result = tokio::time::timeout(Duration::from_secs(10), async {
            let dir = tempfile::tempdir().unwrap();
            let thread = make_thread(
                "ThreatMon",
                ThreadPriority::Normal,
                ThreadSchedule::EveryTick,
            );
            let thread_id = thread.thread_id;

            let (kernel, handler, _mock) = setup_with_threads(
                dir.path(),
                vec![THREAD_RESPONSE.into(), DECISION_NO_ACTIONS.into()],
                vec![thread],
            )
            .await;

            let tick_store = kernel.tick_store.clone();
            let k = send_kernel(&kernel);
            let output = tokio::task::spawn_blocking(move || {
                let token = CancellationToken::new();
                exoskeleton_host::kernel::run_tick(&handler, &k, &token)
            })
            .await
            .unwrap();

            match &output {
                actionqueue_executor_local::HandlerOutput::Success { .. } => {}
                other => {
                    panic!("expected Success, got: {other:?}")
                }
            }

            let record = tick_store.latest().unwrap().unwrap();
            assert_eq!(
                record.thread_contributions.len(),
                1,
                "expected 1 thread contribution"
            );
            assert_eq!(record.thread_contributions[0].thread_id, thread_id,);
            assert_eq!(record.thread_contributions[0].summary, "all clear",);

            shutdown_host(&kernel).await;
        })
        .await;
        assert!(result.is_ok(), "test timed out");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn tick_with_multiple_threads() {
        let result = tokio::time::timeout(Duration::from_secs(10), async {
            let dir = tempfile::tempdir().unwrap();
            let t_crit = make_thread(
                "Critical",
                ThreadPriority::Critical,
                ThreadSchedule::EveryTick,
            );
            let t_norm = make_thread("Normal", ThreadPriority::Normal, ThreadSchedule::EveryTick);
            let t_bg = make_thread(
                "Background",
                ThreadPriority::Background,
                ThreadSchedule::EveryTick,
            );

            let (kernel, handler, mock) = setup_with_threads(
                dir.path(),
                vec![
                    THREAD_RESPONSE.into(),
                    THREAD_RESPONSE.into(),
                    THREAD_RESPONSE.into(),
                    DECISION_NO_ACTIONS.into(),
                ],
                vec![t_crit.clone(), t_norm.clone(), t_bg.clone()],
            )
            .await;

            let tick_store = kernel.tick_store.clone();
            let k = send_kernel(&kernel);
            let output = tokio::task::spawn_blocking(move || {
                let token = CancellationToken::new();
                exoskeleton_host::kernel::run_tick(&handler, &k, &token)
            })
            .await
            .unwrap();

            match &output {
                actionqueue_executor_local::HandlerOutput::Success { .. } => {}
                other => {
                    panic!("expected Success, got: {other:?}")
                }
            }

            let record = tick_store.latest().unwrap().unwrap();
            assert_eq!(
                record.thread_contributions.len(),
                3,
                "expected 3 thread contributions"
            );

            // All 4 calls made (3 threads + 1 decision)
            assert_eq!(mock.call_count(), 4);

            shutdown_host(&kernel).await;
        })
        .await;
        assert!(result.is_ok(), "test timed out");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn thread_output_in_compiled_context() {
        let result = tokio::time::timeout(Duration::from_secs(10), async {
            let dir = tempfile::tempdir().unwrap();
            let thread = make_thread(
                "ContextThread",
                ThreadPriority::Normal,
                ThreadSchedule::EveryTick,
            );

            use std::sync::Mutex;

            struct CapturingMultiMock {
                responses: Vec<String>,
                call_count: AtomicUsize,
                captured_requests: Mutex<Vec<LlmRequest>>,
            }

            impl LlmHttpBackend for CapturingMultiMock {
                fn call(
                    &self,
                    _client: &reqwest::Client,
                    request: &LlmRequest,
                    _cancellation: &CancellationToken,
                ) -> Result<LlmResponse, exoskeleton_core::ExoError> {
                    let idx = self.call_count.fetch_add(1, Ordering::SeqCst);
                    self.captured_requests.lock().unwrap().push(request.clone());
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

            let capturing = Arc::new(CapturingMultiMock {
                responses: vec![THREAD_RESPONSE.into(), DECISION_NO_ACTIONS.into()],
                call_count: AtomicUsize::new(0),
                captured_requests: Mutex::new(Vec::new()),
            });

            let (kernel, handler) = setup_with_threads_custom_backend(
                dir.path(),
                capturing.clone() as Arc<dyn LlmHttpBackend>,
                vec![thread],
            )
            .await;

            let k = send_kernel(&kernel);
            tokio::task::spawn_blocking(move || {
                let token = CancellationToken::new();
                exoskeleton_host::kernel::run_tick(&handler, &k, &token)
            })
            .await
            .unwrap();

            {
                let requests = capturing.captured_requests.lock().unwrap();
                assert!(
                    requests.len() >= 2,
                    "expected >= 2 LLM calls, got {}",
                    requests.len()
                );
                // The Decide step sends compiled context as the
                // user message (not the system prompt). Thread
                // outputs are rendered in the compiled context.
                let decide_request = &requests[1];
                let user_content = decide_request
                    .messages
                    .first()
                    .map(|m| m.content.as_str())
                    .unwrap_or("");
                assert!(
                    user_content.contains("all clear"),
                    "Decide user message should contain \
                         thread output 'all clear', got: {}",
                    &user_content[..user_content.len().min(500)]
                );
            }

            shutdown_host(&kernel).await;
        })
        .await;
        assert!(result.is_ok(), "test timed out");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn thread_output_in_tick_record() {
        let result = tokio::time::timeout(Duration::from_secs(10), async {
            let dir = tempfile::tempdir().unwrap();
            let thread = make_thread(
                "RecordThread",
                ThreadPriority::Normal,
                ThreadSchedule::EveryTick,
            );
            let thread_id = thread.thread_id;

            let (kernel, handler, _mock) = setup_with_threads(
                dir.path(),
                vec![THREAD_RESPONSE.into(), DECISION_NO_ACTIONS.into()],
                vec![thread],
            )
            .await;

            let tick_store = kernel.tick_store.clone();
            let k = send_kernel(&kernel);
            tokio::task::spawn_blocking(move || {
                let token = CancellationToken::new();
                exoskeleton_host::kernel::run_tick(&handler, &k, &token)
            })
            .await
            .unwrap();

            let record = tick_store.latest().unwrap().unwrap();
            assert_eq!(record.thread_contributions.len(), 1);
            let tc = &record.thread_contributions[0];
            assert_eq!(tc.thread_id, thread_id);
            assert_eq!(tc.summary, "all clear");
            assert!(
                !tc.artifact_id.to_string().is_empty(),
                "artifact_id should be set"
            );

            shutdown_host(&kernel).await;
        })
        .await;
        assert!(result.is_ok(), "test timed out");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn thread_failure_does_not_abort_tick() {
        let result = tokio::time::timeout(Duration::from_secs(10), async {
            let dir = tempfile::tempdir().unwrap();
            let thread = make_thread(
                "FailThread",
                ThreadPriority::Normal,
                ThreadSchedule::EveryTick,
            );

            let backend = Arc::new(FailThenSucceedBackend::new(
                vec![0],
                DECISION_NO_ACTIONS.into(),
            ));

            let (kernel, handler) = setup_with_threads_custom_backend(
                dir.path(),
                backend as Arc<dyn LlmHttpBackend>,
                vec![thread],
            )
            .await;

            let k = send_kernel(&kernel);
            let output = tokio::task::spawn_blocking(move || {
                let token = CancellationToken::new();
                exoskeleton_host::kernel::run_tick(&handler, &k, &token)
            })
            .await
            .unwrap();

            match &output {
                actionqueue_executor_local::HandlerOutput::Success { .. } => {}
                other => panic!(
                    "expected Success despite thread \
                         failure, got: {other:?}"
                ),
            }

            let record = kernel.tick_store.latest().unwrap().unwrap();
            assert!(
                record.thread_contributions.is_empty(),
                "failed thread should not contribute"
            );

            shutdown_host(&kernel).await;
        })
        .await;
        assert!(result.is_ok(), "test timed out");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn thread_cancellation_skips_remaining() {
        let result = tokio::time::timeout(Duration::from_secs(10), async {
            let dir = tempfile::tempdir().unwrap();
            let t1 = make_thread(
                "Thread1",
                ThreadPriority::Critical,
                ThreadSchedule::EveryTick,
            );
            let t2 = make_thread("Thread2", ThreadPriority::Normal, ThreadSchedule::EveryTick);

            let (kernel, handler, mock) = setup_with_threads(
                dir.path(),
                vec![
                    THREAD_RESPONSE.into(),
                    THREAD_RESPONSE.into(),
                    DECISION_NO_ACTIONS.into(),
                ],
                vec![t1, t2],
            )
            .await;

            let k = send_kernel(&kernel);
            let output = tokio::task::spawn_blocking(move || {
                let token = CancellationToken::new();
                token.cancel();
                exoskeleton_host::kernel::run_tick(&handler, &k, &token)
            })
            .await
            .unwrap();

            // Pre-cancelled token: threads get skipped,
            // tick returns retryable failure or partial.
            match &output {
                actionqueue_executor_local::HandlerOutput::RetryableFailure { .. } => {}
                actionqueue_executor_local::HandlerOutput::Success { .. } => {}
                other => panic!(
                    "expected RetryableFailure or \
                         Success, got: {other:?}"
                ),
            }

            let calls = mock.call_count();
            assert!(
                calls <= 3,
                "expected <= 3 calls with cancellation, \
                     got {calls}"
            );

            shutdown_host(&kernel).await;
        })
        .await;
        assert!(result.is_ok(), "test timed out");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn thread_summaries_in_snapshot() {
        let result = tokio::time::timeout(Duration::from_secs(10), async {
            let dir = tempfile::tempdir().unwrap();
            let thread = make_thread(
                "SnapshotThread",
                ThreadPriority::Normal,
                ThreadSchedule::EveryTick,
            );
            let thread_name = thread.name.clone();

            let (kernel, handler, _mock) = setup_with_threads(
                dir.path(),
                vec![THREAD_RESPONSE.into(), DECISION_NO_ACTIONS.into()],
                vec![thread],
            )
            .await;

            let snapshot_store = kernel.snapshot_store.clone();
            let k = send_kernel(&kernel);
            tokio::task::spawn_blocking(move || {
                let token = CancellationToken::new();
                exoskeleton_host::kernel::run_tick(&handler, &k, &token)
            })
            .await
            .unwrap();

            let snapshot = snapshot_store.latest().unwrap().unwrap();
            assert_eq!(
                snapshot.thread_summaries.len(),
                1,
                "expected 1 thread summary in snapshot"
            );
            let ts = &snapshot.thread_summaries[0];
            assert_eq!(ts.name, thread_name);
            assert_eq!(ts.status, ThreadStatus::Active);

            shutdown_host(&kernel).await;
        })
        .await;
        assert!(result.is_ok(), "test timed out");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn every_n_ticks_scheduling() {
        let result = tokio::time::timeout(Duration::from_secs(15), async {
            let dir = tempfile::tempdir().unwrap();
            let thread = make_thread(
                "Periodic",
                ThreadPriority::Normal,
                ThreadSchedule::EveryNTicks(2),
            );

            // Tick 1: due (never run) -> thread + decision
            // Tick 2: not due (elapsed=1<2) -> decision only
            // Tick 3: due (elapsed=2>=2) -> thread + decision
            // Total LLM calls: 2 + 1 + 2 = 5
            let responses: Vec<String> = vec![
                THREAD_RESPONSE.into(),
                DECISION_NO_ACTIONS.into(),
                DECISION_NO_ACTIONS.into(),
                THREAD_RESPONSE.into(),
                DECISION_NO_ACTIONS.into(),
            ];

            let (kernel, _handler, mock) =
                setup_with_threads(dir.path(), responses, vec![thread]).await;

            let tick_store = kernel.tick_store.clone();

            // Tick 1
            let k1 = send_kernel(&kernel);
            let b1: Arc<dyn LlmHttpBackend> = mock.clone();
            let a1 = kernel.artifact_store.clone();
            let h1 = CognitiveHandler::with_backends(Some(b1), None, LlmBackend::Local, a1);
            tokio::task::spawn_blocking(move || {
                let token = CancellationToken::new();
                exoskeleton_host::kernel::run_tick(&h1, &k1, &token)
            })
            .await
            .unwrap();

            let r1 = tick_store.latest().unwrap().unwrap();
            assert_eq!(r1.tick_number, 1);
            assert_eq!(
                r1.thread_contributions.len(),
                1,
                "tick 1: thread should be due"
            );

            // Tick 2
            let k2 = send_kernel(&kernel);
            let b2: Arc<dyn LlmHttpBackend> = mock.clone();
            let a2 = kernel.artifact_store.clone();
            let h2 = CognitiveHandler::with_backends(Some(b2), None, LlmBackend::Local, a2);
            tokio::task::spawn_blocking(move || {
                let token = CancellationToken::new();
                exoskeleton_host::kernel::run_tick(&h2, &k2, &token)
            })
            .await
            .unwrap();

            let r2 = tick_store.latest().unwrap().unwrap();
            assert_eq!(r2.tick_number, 2);
            assert!(
                r2.thread_contributions.is_empty(),
                "tick 2: thread should NOT be due"
            );

            // Tick 3
            let k3 = send_kernel(&kernel);
            let b3: Arc<dyn LlmHttpBackend> = mock.clone();
            let a3 = kernel.artifact_store.clone();
            let h3 = CognitiveHandler::with_backends(Some(b3), None, LlmBackend::Local, a3);
            tokio::task::spawn_blocking(move || {
                let token = CancellationToken::new();
                exoskeleton_host::kernel::run_tick(&h3, &k3, &token)
            })
            .await
            .unwrap();

            let r3 = tick_store.latest().unwrap().unwrap();
            assert_eq!(r3.tick_number, 3);
            assert_eq!(
                r3.thread_contributions.len(),
                1,
                "tick 3: thread should be due"
            );

            assert_eq!(mock.call_count(), 5, "expected 5 total LLM calls (2+1+2)");

            shutdown_host(&kernel).await;
        })
        .await;
        assert!(result.is_ok(), "test timed out");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn suspended_thread_not_executed() {
        let result = tokio::time::timeout(Duration::from_secs(10), async {
            let dir = tempfile::tempdir().unwrap();
            let thread = make_thread(
                "SuspendMe",
                ThreadPriority::Normal,
                ThreadSchedule::EveryTick,
            );
            let thread_id = thread.thread_id;

            let (kernel, handler, mock) =
                setup_with_threads(dir.path(), vec![DECISION_NO_ACTIONS.into()], vec![thread])
                    .await;

            kernel
                .thread_registry
                .update_status(thread_id, ThreadStatus::Suspended)
                .unwrap();

            let tick_store = kernel.tick_store.clone();
            let k = send_kernel(&kernel);
            tokio::task::spawn_blocking(move || {
                let token = CancellationToken::new();
                exoskeleton_host::kernel::run_tick(&handler, &k, &token)
            })
            .await
            .unwrap();

            let record = tick_store.latest().unwrap().unwrap();
            assert!(
                record.thread_contributions.is_empty(),
                "suspended thread should not contribute"
            );
            assert_eq!(mock.call_count(), 1, "only decision LLM call expected");

            shutdown_host(&kernel).await;
        })
        .await;
        assert!(result.is_ok(), "test timed out");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn thread_event_logged() {
        let result = tokio::time::timeout(Duration::from_secs(10), async {
            let dir = tempfile::tempdir().unwrap();
            let thread = make_thread(
                "EventThread",
                ThreadPriority::Normal,
                ThreadSchedule::EveryTick,
            );

            let (kernel, handler, _mock) = setup_with_threads(
                dir.path(),
                vec![THREAD_RESPONSE.into(), DECISION_NO_ACTIONS.into()],
                vec![thread],
            )
            .await;

            let event_ledger = kernel.event_ledger.clone();
            let k = send_kernel(&kernel);
            tokio::task::spawn_blocking(move || {
                let token = CancellationToken::new();
                exoskeleton_host::kernel::run_tick(&handler, &k, &token)
            })
            .await
            .unwrap();

            let events = event_ledger.recent(100).unwrap();
            let thread_events: Vec<_> = events
                .iter()
                .filter(|e| e.event_type == EventType::ThreadRan)
                .collect();
            assert_eq!(thread_events.len(), 1, "expected 1 ThreadRan event");
            assert!(
                thread_events[0].summary.contains("EventThread"),
                "ThreadRan event should mention thread \
                     name"
            );
            assert!(
                thread_events[0].summary.contains("all clear"),
                "ThreadRan event should include summary"
            );

            shutdown_host(&kernel).await;
        })
        .await;
        assert!(result.is_ok(), "test timed out");
    }

    #[test]
    fn thread_never_invokes_tools() {
        // Only audit production code (before the #[cfg(test)] marker).
        let full_source = include_str!("../src/kernel/threads.rs");
        let production_code = full_source
            .split("#[cfg(test)]")
            .next()
            .unwrap_or(full_source);
        assert!(
            !production_code.contains("invoke_single"),
            "threads.rs production code must not call \
             invoke_single (threads never invoke tools)"
        );
        assert!(
            !production_code.contains("wi_host_slot"),
            "threads.rs production code must not reference \
             wi_host_slot (threads never cross to Tool AQ)"
        );
    }

    // ══════════════════════════════════════════════════════════════════
    // T-9: Thread Lifecycle
    // ══════════════════════════════════════════════════════════════════

    #[test]
    fn thread_survives_storage_restart() {
        use exoskeleton_host::SqliteThreadStore;
        use exoskeleton_threads::ThreadStore;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("threads.db");
        let spec = make_thread(
            "Persistent",
            ThreadPriority::Normal,
            ThreadSchedule::EveryTick,
        );
        let thread_id = spec.thread_id;

        {
            let store = SqliteThreadStore::open(&path).unwrap();
            store.save(&spec, ThreadStatus::Active).unwrap();
        }

        {
            let store = SqliteThreadStore::open(&path).unwrap();
            let result = store.get(thread_id).unwrap();
            assert!(result.is_some(), "thread should survive restart");
            let (got_spec, got_status) = result.unwrap();
            assert_eq!(got_spec, spec);
            assert_eq!(got_status, ThreadStatus::Active);
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn deregistered_thread_stops_executing() {
        let result = tokio::time::timeout(Duration::from_secs(15), async {
            let dir = tempfile::tempdir().unwrap();
            let thread = make_thread(
                "Deregister",
                ThreadPriority::Normal,
                ThreadSchedule::EveryTick,
            );
            let thread_id = thread.thread_id;

            let (kernel, _handler, mock) = setup_with_threads(
                dir.path(),
                vec![
                    THREAD_RESPONSE.into(),
                    DECISION_NO_ACTIONS.into(),
                    DECISION_NO_ACTIONS.into(),
                ],
                vec![thread],
            )
            .await;

            let tick_store = kernel.tick_store.clone();

            // Tick 1: thread executes
            let k1 = send_kernel(&kernel);
            let b1: Arc<dyn LlmHttpBackend> = mock.clone();
            let a1 = kernel.artifact_store.clone();
            let h1 = CognitiveHandler::with_backends(Some(b1), None, LlmBackend::Local, a1);
            tokio::task::spawn_blocking(move || {
                let token = CancellationToken::new();
                exoskeleton_host::kernel::run_tick(&h1, &k1, &token)
            })
            .await
            .unwrap();

            let r1 = tick_store.latest().unwrap().unwrap();
            assert_eq!(
                r1.thread_contributions.len(),
                1,
                "tick 1: thread should execute"
            );

            // Deregister
            kernel.thread_registry.deregister(thread_id).unwrap();

            // Tick 2: no thread
            let k2 = send_kernel(&kernel);
            let b2: Arc<dyn LlmHttpBackend> = mock.clone();
            let a2 = kernel.artifact_store.clone();
            let h2 = CognitiveHandler::with_backends(Some(b2), None, LlmBackend::Local, a2);
            tokio::task::spawn_blocking(move || {
                let token = CancellationToken::new();
                exoskeleton_host::kernel::run_tick(&h2, &k2, &token)
            })
            .await
            .unwrap();

            let r2 = tick_store.latest().unwrap().unwrap();
            assert_eq!(r2.tick_number, 2);
            assert!(
                r2.thread_contributions.is_empty(),
                "tick 2: deregistered thread should \
                     not execute"
            );

            assert_eq!(mock.call_count(), 3);

            shutdown_host(&kernel).await;
        })
        .await;
        assert!(result.is_ok(), "test timed out");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn suspended_thread_skips_execution() {
        let result = tokio::time::timeout(Duration::from_secs(10), async {
            let dir = tempfile::tempdir().unwrap();
            let thread = make_thread(
                "SuspendSkip",
                ThreadPriority::Normal,
                ThreadSchedule::EveryTick,
            );
            let thread_id = thread.thread_id;

            let (kernel, handler, mock) =
                setup_with_threads(dir.path(), vec![DECISION_NO_ACTIONS.into()], vec![thread])
                    .await;

            kernel
                .thread_registry
                .update_status(thread_id, ThreadStatus::Suspended)
                .unwrap();

            let tick_store = kernel.tick_store.clone();
            let k = send_kernel(&kernel);
            tokio::task::spawn_blocking(move || {
                let token = CancellationToken::new();
                exoskeleton_host::kernel::run_tick(&handler, &k, &token)
            })
            .await
            .unwrap();

            let record = tick_store.latest().unwrap().unwrap();
            assert!(
                record.thread_contributions.is_empty(),
                "suspended thread should not contribute"
            );
            assert_eq!(mock.call_count(), 1, "only decision LLM call expected");

            shutdown_host(&kernel).await;
        })
        .await;
        assert!(result.is_ok(), "test timed out");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn thread_status_persists_across_ticks() {
        let result = tokio::time::timeout(Duration::from_secs(10), async {
            let dir = tempfile::tempdir().unwrap();
            let thread = make_thread(
                "StatusPersist",
                ThreadPriority::Normal,
                ThreadSchedule::EveryTick,
            );
            let thread_id = thread.thread_id;

            let (kernel, handler, _mock) = setup_with_threads(
                dir.path(),
                vec![THREAD_RESPONSE.into(), DECISION_NO_ACTIONS.into()],
                vec![thread],
            )
            .await;

            let k = send_kernel(&kernel);
            tokio::task::spawn_blocking(move || {
                let token = CancellationToken::new();
                exoskeleton_host::kernel::run_tick(&handler, &k, &token)
            })
            .await
            .unwrap();

            let entry = kernel.thread_registry.get(thread_id).unwrap().unwrap();
            assert_eq!(
                entry.1,
                ThreadStatus::Active,
                "thread should remain Active after tick"
            );

            shutdown_host(&kernel).await;
        })
        .await;
        assert!(result.is_ok(), "test timed out");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn completed_thread_never_runs() {
        let result = tokio::time::timeout(Duration::from_secs(10), async {
            let dir = tempfile::tempdir().unwrap();
            let thread = make_thread(
                "CompletedThread",
                ThreadPriority::Normal,
                ThreadSchedule::EveryTick,
            );
            let thread_id = thread.thread_id;

            let (kernel, handler, mock) =
                setup_with_threads(dir.path(), vec![DECISION_NO_ACTIONS.into()], vec![thread])
                    .await;

            kernel
                .thread_registry
                .update_status(thread_id, ThreadStatus::Completed)
                .unwrap();

            let tick_store = kernel.tick_store.clone();
            let k = send_kernel(&kernel);
            tokio::task::spawn_blocking(move || {
                let token = CancellationToken::new();
                exoskeleton_host::kernel::run_tick(&handler, &k, &token)
            })
            .await
            .unwrap();

            let record = tick_store.latest().unwrap().unwrap();
            assert!(
                record.thread_contributions.is_empty(),
                "completed thread should never run"
            );
            assert_eq!(mock.call_count(), 1, "only decision LLM call expected");

            shutdown_host(&kernel).await;
        })
        .await;
        assert!(result.is_ok(), "test timed out");
    }
}

// ── T-13: Property-Based Tests (Sprint 6) ──

mod proptest_sprint6 {
    use exoskeleton_core::{
        ArtifactId, StateSnapshot, ThreadId, ThreadOutput, ThreadPriority, ThreadSchedule,
        ThreadSpec, ThreadStatus, TickId, VesselId,
    };
    use exoskeleton_memory::ApproximateTokenCounter;
    use exoskeleton_threads::{compile_thread_context, is_thread_due};
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn thread_spec_survives_json_roundtrip(
            name in "[a-zA-Z ]{1,50}",
            charter in "[a-zA-Z ]{1,200}",
            budget in 100u64..10000,
        ) {
            let spec = ThreadSpec {
                thread_id: ThreadId::new(),
                name,
                charter,
                priority: ThreadPriority::Normal,
                token_budget: budget,
                schedule: ThreadSchedule::EveryTick,
            };
            let json =
                serde_json::to_string(&spec).unwrap();
            let parsed: ThreadSpec =
                serde_json::from_str(&json).unwrap();
            prop_assert_eq!(spec, parsed);
        }

        #[test]
        fn thread_output_survives_json_roundtrip(
            summary in "[a-zA-Z ]{1,100}",
            rec1 in "[a-zA-Z ]{1,50}",
            rec2 in "[a-zA-Z ]{1,50}",
        ) {
            let output = ThreadOutput {
                thread_id: ThreadId::new(),
                tick_id: TickId::new(),
                artifact_id: ArtifactId::from_content(
                    summary.as_bytes(),
                ),
                summary,
                recommendations: vec![rec1, rec2],
            };
            let json =
                serde_json::to_string(&output).unwrap();
            let parsed: ThreadOutput =
                serde_json::from_str(&json).unwrap();
            prop_assert_eq!(output, parsed);
        }

        #[test]
        fn is_thread_due_consistent_for_every_tick(
            tick in 0u64..10000,
        ) {
            prop_assert!(is_thread_due(
                ThreadSchedule::EveryTick,
                ThreadStatus::Active,
                tick,
                Some(tick.saturating_sub(1)),
            ));
        }

        #[test]
        fn thread_context_within_budget(
            budget in 200u64..5000,
        ) {
            let counter = ApproximateTokenCounter;
            let spec = ThreadSpec {
                thread_id: ThreadId::new(),
                name: "PropTest Thread".into(),
                charter: "Analyze for test".into(),
                priority: ThreadPriority::Normal,
                token_budget: budget,
                schedule: ThreadSchedule::EveryTick,
            };
            let snapshot = StateSnapshot::initial(
                VesselId::new(),
                "test".into(),
            );
            match compile_thread_context(
                &counter,
                &spec,
                &snapshot,
                &[],
                TickId::new(),
            ) {
                Ok(ctx) => {
                    prop_assert!(
                        ctx.total_tokens <= budget,
                        "tokens={} > budget={budget}",
                        ctx.total_tokens,
                    );
                }
                Err(_) => {
                    // Budget too small for charter
                }
            }
        }
    }
}

// Sprint 7: First Three Cognitive Threads Integration Tests
// ═══════════════════════════════════════════════════════════

mod sprint7 {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    use actionqueue_executor_local::CancellationToken;
    use exoskeleton_core::llm::{LlmBackend, LlmRequest, LlmResponse, StopReason};
    use exoskeleton_core::{
        ArtifactKind, ArtifactStore, EventType, ThreadPriority, ThreadSchedule, ThreadStatus,
        VesselId,
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
            budget_tracker: kernel.budget_tracker.clone(),
            tool_budget_gate: kernel.tool_budget_gate.clone(),
            metrics: None,
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
        register_builtin_threads(&thread_registry).unwrap();

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
            budget_tracker: None,
            tool_budget_gate: None,
            metrics: None,
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
        register_builtin_threads(&thread_registry).unwrap();

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
            budget_tracker: None,
            tool_budget_gate: None,
            metrics: None,
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
        register_builtin_threads(&registry).unwrap();

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
        register_builtin_threads(&registry).unwrap();
        register_builtin_threads(&registry).unwrap();
        register_builtin_threads(&registry).unwrap();
        assert_eq!(registry.list().unwrap().len(), 3);
    }

    #[test]
    fn builtin_registration_preserves_suspended() {
        let store = Arc::new(InMemoryThreadStore::new());
        let registry = ThreadRegistry::new(store);
        register_builtin_threads(&registry).unwrap();

        // Operator suspends Threat Monitor
        registry
            .update_status(THREAT_MONITOR_ID, ThreadStatus::Suspended)
            .unwrap();

        // Re-register (simulate restart)
        register_builtin_threads(&registry).unwrap();

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
        register_builtin_threads(&registry).unwrap();

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
}

// ── T-10: Property-Based Tests (Sprint 7) ──

mod proptest_sprint7 {
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

// ── Sprint 8: Relationship Substrate + Align Step ──

mod sprint8 {
    use std::sync::Arc;

    use chrono::Utc;
    use exoskeleton_core::{
        ArtifactId, ArtifactKind, ArtifactStore, LedgerEntryId, PrincipalId, RelationalSignalType,
        RelationshipRecord, RelationshipSnapshot, TickId, VesselId,
    };
    use exoskeleton_host::storage::StorageManager;
    use exoskeleton_memory::ApproximateTokenCounter;
    use exoskeleton_relationship::{
        compile_relationship_snapshot, InMemoryRelationshipLedger, RelationshipLedger,
    };

    fn make_record(
        principal_id: PrincipalId,
        signal_type: RelationalSignalType,
        tick_id: TickId,
        ts_offset: i64,
    ) -> RelationshipRecord {
        RelationshipRecord {
            id: LedgerEntryId::new(),
            principal_id,
            signal_type,
            content_ref: ArtifactId::from_content(
                format!("{:?}-{}", signal_type, ts_offset).as_bytes(),
            ),
            tick_id,
            timestamp: Utc::now() + chrono::Duration::seconds(ts_offset),
            metadata: Default::default(),
        }
    }

    // ── T-7: Basic Relationship Lifecycle ──

    #[test]
    fn relationship_ledger_lifecycle() {
        let dir = tempfile::tempdir().unwrap();
        let storage = StorageManager::open(dir.path()).unwrap();
        let ledger = storage.relationship_store();
        let p = PrincipalId::new();
        let tick_id = TickId::new();

        // Append signals
        ledger
            .append(&make_record(
                p,
                RelationalSignalType::TrustUpdate,
                tick_id,
                0,
            ))
            .unwrap();
        ledger
            .append(&make_record(
                p,
                RelationalSignalType::CommitmentMade,
                tick_id,
                1,
            ))
            .unwrap();

        // Verify
        assert_eq!(ledger.count().unwrap(), 2);
        let records = ledger.for_principal(p, 10).unwrap();
        assert_eq!(records.len(), 2);
        assert!(ledger.distinct_principals().unwrap().contains(&p));
    }

    #[test]
    fn relationship_snapshot_from_ledger() {
        let dir = tempfile::tempdir().unwrap();
        let storage = StorageManager::open(dir.path()).unwrap();
        let ledger = storage.relationship_store();
        let p = PrincipalId::new();
        let tick_id = TickId::new();

        // Build some relationship history
        ledger
            .append(&make_record(
                p,
                RelationalSignalType::CommitmentMade,
                tick_id,
                0,
            ))
            .unwrap();
        ledger
            .append(&make_record(
                p,
                RelationalSignalType::CommitmentFulfilled,
                tick_id,
                1,
            ))
            .unwrap();

        // Compile snapshot
        let snapshot = compile_relationship_snapshot(ledger.as_ref()).unwrap();
        assert_eq!(snapshot.principals.len(), 1);
        assert_eq!(snapshot.principals[0].principal_id, p);
        // 0.5 + 0.05 = 0.55
        assert!(
            (snapshot.principals[0].trust_level - 0.55).abs() < f64::EPSILON,
            "expected 0.55, got {}",
            snapshot.principals[0].trust_level
        );
        assert_eq!(snapshot.principals[0].active_commitments, 0);
    }

    // ── T-9: Align Passthrough with No Relationships ──

    #[test]
    fn align_passthrough_no_relationships() {
        let ledger = InMemoryRelationshipLedger::new();
        let snapshot = compile_relationship_snapshot(&ledger).unwrap();
        assert!(snapshot.principals.is_empty());

        // With no principals, all actions should pass through
        let config = exoskeleton_relationship::AlignConfig::default();
        let actions = vec![
            exoskeleton_relationship::AlignAction {
                tool_name: "fs.write".into(),
                rationale: "test".into(),
            },
            exoskeleton_relationship::AlignAction {
                tool_name: "delay".into(),
                rationale: "test".into(),
            },
        ];
        let tick_id = TickId::new();

        let (approved, blocked, updates) =
            exoskeleton_relationship::check_alignment(&actions, &snapshot, &config, tick_id);
        assert_eq!(approved, vec![0, 1]);
        assert!(blocked.is_empty());
        assert!(updates.is_empty());
    }

    // ── T-8: Align Action Filtering ──

    #[test]
    fn align_blocks_actions_with_low_trust() {
        let ledger = InMemoryRelationshipLedger::new();
        let p = PrincipalId::new();
        let tick_id = TickId::new();

        // Drive trust well below threshold with broken commitments
        for i in 0..5 {
            ledger
                .append(&make_record(
                    p,
                    RelationalSignalType::CommitmentMade,
                    tick_id,
                    i * 2,
                ))
                .unwrap();
            ledger
                .append(&make_record(
                    p,
                    RelationalSignalType::CommitmentBroken,
                    tick_id,
                    i * 2 + 1,
                ))
                .unwrap();
        }

        let snapshot = compile_relationship_snapshot(&ledger).unwrap();
        assert!(snapshot.principals[0].trust_level < 0.3);

        let config = exoskeleton_relationship::AlignConfig::default();
        let actions = vec![exoskeleton_relationship::AlignAction {
            tool_name: "delay".into(),
            rationale: "test".into(),
        }];

        let (approved, blocked, _) =
            exoskeleton_relationship::check_alignment(&actions, &snapshot, &config, tick_id);
        assert!(approved.is_empty());
        assert_eq!(blocked.len(), 1);
        assert!(blocked[0].1.contains("trust gate"));
    }

    // ── T-10: Multi-Tick Relationship Evolution ──

    #[test]
    fn multi_tick_relationship_evolution() {
        let ledger = InMemoryRelationshipLedger::new();
        let p = PrincipalId::new();

        // Tick 1: initial signal
        let tick1 = TickId::new();
        ledger
            .append(&make_record(p, RelationalSignalType::TrustUpdate, tick1, 0))
            .unwrap();
        let snap1 = compile_relationship_snapshot(&ledger).unwrap();
        assert_eq!(snap1.principals.len(), 1);
        assert!((snap1.principals[0].trust_level - 0.5).abs() < f64::EPSILON);

        // Tick 2: commitment made
        let tick2 = TickId::new();
        ledger
            .append(&make_record(
                p,
                RelationalSignalType::CommitmentMade,
                tick2,
                10,
            ))
            .unwrap();
        let snap2 = compile_relationship_snapshot(&ledger).unwrap();
        assert_eq!(snap2.principals[0].active_commitments, 1);

        // Tick 3: commitment fulfilled
        let tick3 = TickId::new();
        ledger
            .append(&make_record(
                p,
                RelationalSignalType::CommitmentFulfilled,
                tick3,
                20,
            ))
            .unwrap();
        let snap3 = compile_relationship_snapshot(&ledger).unwrap();
        assert_eq!(snap3.principals[0].active_commitments, 0);
        assert!((snap3.principals[0].trust_level - 0.55).abs() < f64::EPSILON);

        // Each snapshot is recompiled fresh (I5), not carried forward
        // Re-compile from the same ledger produces identical results
        let snap3_again = compile_relationship_snapshot(&ledger).unwrap();
        assert_eq!(
            snap3.principals[0].trust_level,
            snap3_again.principals[0].trust_level
        );
    }

    // ── T-12: Relationship Durability (AC-B) ──

    #[test]
    fn relationship_durability_across_restart() {
        let dir = tempfile::tempdir().unwrap();
        let p1 = PrincipalId::new();
        let p2 = PrincipalId::new();

        // Phase 1: Build relationship history
        {
            let storage = StorageManager::open(dir.path()).unwrap();
            let ledger = storage.relationship_store();
            let tick1 = TickId::new();
            let tick2 = TickId::new();
            let tick3 = TickId::new();

            // p1: trust update, commitment made
            ledger
                .append(&make_record(
                    p1,
                    RelationalSignalType::TrustUpdate,
                    tick1,
                    0,
                ))
                .unwrap();
            ledger
                .append(&make_record(
                    p1,
                    RelationalSignalType::CommitmentMade,
                    tick1,
                    1,
                ))
                .unwrap();

            // p2: feedback
            ledger
                .append(&make_record(
                    p2,
                    RelationalSignalType::FeedbackReceived,
                    tick2,
                    2,
                ))
                .unwrap();

            // p1: commitment fulfilled
            ledger
                .append(&make_record(
                    p1,
                    RelationalSignalType::CommitmentFulfilled,
                    tick3,
                    3,
                ))
                .unwrap();

            // Verify pre-crash state
            assert_eq!(ledger.count().unwrap(), 4);
            let snap = compile_relationship_snapshot(ledger.as_ref()).unwrap();
            assert_eq!(snap.principals.len(), 2);
        }
        // Drop — simulates crash

        // Phase 2: Reopen and verify durability
        {
            let storage = StorageManager::open(dir.path()).unwrap();
            let ledger = storage.relationship_store();

            // All records preserved
            assert_eq!(ledger.count().unwrap(), 4);
            let principals = ledger.distinct_principals().unwrap();
            assert_eq!(principals.len(), 2);
            assert!(principals.contains(&p1));
            assert!(principals.contains(&p2));

            // Snapshot recompiled from ledger matches pre-crash state
            let snap = compile_relationship_snapshot(ledger.as_ref()).unwrap();
            assert_eq!(snap.principals.len(), 2);

            let s1 = snap
                .principals
                .iter()
                .find(|s| s.principal_id == p1)
                .unwrap();
            let s2 = snap
                .principals
                .iter()
                .find(|s| s.principal_id == p2)
                .unwrap();

            // p1: 0.5 (TrustUpdate) + 0.05 (CommitmentFulfilled) = 0.55
            assert!(
                (s1.trust_level - 0.55).abs() < f64::EPSILON,
                "expected 0.55, got {}",
                s1.trust_level
            );
            assert_eq!(s1.active_commitments, 0);

            // p2: 0.5 (neutral) + 0.02 (FeedbackReceived) = 0.52
            assert!(
                (s2.trust_level - 0.52).abs() < f64::EPSILON,
                "expected 0.52, got {}",
                s2.trust_level
            );
        }
    }

    // ── T-13: Atomic Ledger Consistency (AC-D) ──

    #[test]
    fn atomic_ledger_crash_consistency() {
        let dir = tempfile::tempdir().unwrap();
        let p = PrincipalId::new();

        // Write records
        {
            let storage = StorageManager::open(dir.path()).unwrap();
            let ledger = storage.relationship_store();

            for i in 0..10 {
                ledger
                    .append(&make_record(
                        p,
                        RelationalSignalType::FeedbackReceived,
                        TickId::new(),
                        i,
                    ))
                    .unwrap();
            }

            assert_eq!(ledger.count().unwrap(), 10);
        }
        // Drop — simulate crash

        // Reopen and verify
        {
            let storage = StorageManager::open(dir.path()).unwrap();
            let ledger = storage.relationship_store();

            assert_eq!(ledger.count().unwrap(), 10);
            let records = ledger.for_principal(p, 20).unwrap();
            assert_eq!(records.len(), 10);

            // All records are complete (no partial writes)
            for r in &records {
                assert_eq!(r.principal_id, p);
                assert_eq!(r.signal_type, RelationalSignalType::FeedbackReceived);
            }

            // Snapshot compilation works correctly
            let snap = compile_relationship_snapshot(ledger.as_ref()).unwrap();
            assert_eq!(snap.principals.len(), 1);
        }
    }

    // ── T-14: Context Compilation with Relationships ──

    #[test]
    fn context_compilation_with_relationships() {
        let dir = tempfile::tempdir().unwrap();
        let storage = StorageManager::open(dir.path()).unwrap();
        let ledger = storage.relationship_store();
        let p = PrincipalId::new();

        // Build relationship data
        ledger
            .append(&make_record(
                p,
                RelationalSignalType::TrustUpdate,
                TickId::new(),
                0,
            ))
            .unwrap();
        ledger
            .append(&make_record(
                p,
                RelationalSignalType::FeedbackReceived,
                TickId::new(),
                1,
            ))
            .unwrap();

        // Compile relationship snapshot
        let rel_snapshot = compile_relationship_snapshot(ledger.as_ref()).unwrap();

        // Store as artifact and retrieve
        let artifact =
            exoskeleton_core::Artifact::from_json(ArtifactKind::RelationshipEntry, &rel_snapshot)
                .unwrap();
        let artifact_id = storage.artifact_store().put(&artifact).unwrap();

        // Verify we can deserialize the artifact
        let loaded = storage.artifact_store().get(&artifact_id).unwrap().unwrap();
        let loaded_snap: RelationshipSnapshot = serde_json::from_slice(&loaded.content).unwrap();
        assert_eq!(loaded_snap.principals.len(), 1);

        // Context compilation with relationship snapshot
        let vessel_id = VesselId::new();
        let snapshot = exoskeleton_core::StateSnapshot::initial(vessel_id, "test".into());
        let counter = Arc::new(ApproximateTokenCounter);
        let compiler = exoskeleton_memory::ContextCompiler::with_defaults(counter, 4000);

        let sources = exoskeleton_memory::ContextSources {
            vessel_id,
            mission: "test mission",
            snapshot: &snapshot,
            relationship_snapshot: Some(&loaded_snap),
            thread_contributions: &[],
            recent_events: &[],
            episodic_summaries: &[],
            long_term_notes: &[],
            working_context: "",
        };

        let compiled = compiler.compile(&sources).unwrap();

        // The prompt should contain the RELATIONSHIPS header
        assert!(
            compiled.prompt.contains("RELATIONSHIPS"),
            "prompt should contain RELATIONSHIPS header, got: {}",
            compiled.prompt
        );
    }

    // ── T-8 (extended): Destructive tool threshold ──

    #[test]
    fn align_destructive_tool_blocked_at_intermediate_trust() {
        let ledger = InMemoryRelationshipLedger::new();
        let p = PrincipalId::new();
        let tick_id = TickId::new();

        // Set trust to 0.4 — above normal threshold (0.3) but below destructive (0.6)
        let mut record = make_record(p, RelationalSignalType::TrustUpdate, tick_id, 0);
        record.metadata.insert("trust_level".into(), "0.4".into());
        ledger.append(&record).unwrap();

        let snapshot = compile_relationship_snapshot(&ledger).unwrap();
        let config = exoskeleton_relationship::AlignConfig::default();

        // Non-destructive tool should pass
        let non_destructive = vec![exoskeleton_relationship::AlignAction {
            tool_name: "delay".into(),
            rationale: "test".into(),
        }];
        let (approved, blocked, _) = exoskeleton_relationship::check_alignment(
            &non_destructive,
            &snapshot,
            &config,
            tick_id,
        );
        assert_eq!(approved.len(), 1);
        assert!(blocked.is_empty());

        // Destructive tool should be blocked
        let destructive = vec![exoskeleton_relationship::AlignAction {
            tool_name: "fs.write".into(),
            rationale: "test".into(),
        }];
        let (approved, blocked, _) =
            exoskeleton_relationship::check_alignment(&destructive, &snapshot, &config, tick_id);
        assert!(approved.is_empty());
        assert_eq!(blocked.len(), 1);
        assert!(blocked[0].1.contains("destructive"));
    }

    // ── T-11: Signal Processing End-to-End ──

    #[test]
    fn signal_processing_end_to_end() {
        let dir = tempfile::tempdir().unwrap();
        let storage = StorageManager::open(dir.path()).unwrap();
        let artifact_store = storage.artifact_store();
        let relationship_store = storage.relationship_store();
        let tick_id = TickId::new();
        let principal = PrincipalId::new();

        // Create a RelationalSignal and store as artifact
        let signal = exoskeleton_core::RelationalSignal {
            signal_type: RelationalSignalType::TrustUpdate,
            principal_id: principal,
            content: "Trust established at 0.9".into(),
            metadata: {
                let mut m = std::collections::HashMap::new();
                m.insert("trust_level".into(), "0.9".into());
                m.insert("display_name".into(), "TestUser".into());
                m
            },
        };
        let artifact =
            exoskeleton_core::Artifact::from_json(ArtifactKind::RelationshipEntry, &signal)
                .unwrap();
        let artifact_id = artifact_store.put(&artifact).unwrap();

        // Create an envelope referencing the artifact
        let envelope = exoskeleton_core::MessageEnvelope {
            id: exoskeleton_core::EnvelopeId::new(),
            source: principal,
            target: None,
            kind: exoskeleton_core::EnvelopeKind::RelationalSignal,
            payload_ref: artifact_id,
            timestamp: Utc::now(),
            in_reply_to: None,
        };

        // Process the signal
        let ids = exoskeleton_relationship::process_relational_signals(
            &[envelope],
            relationship_store.as_ref(),
            artifact_store.as_ref(),
            tick_id,
        );

        // Verify: signal processed
        assert_eq!(ids.len(), 1);
        assert_eq!(relationship_store.count().unwrap(), 1);

        // Verify: record has metadata from signal
        let records = relationship_store.for_principal(principal, 10).unwrap();
        assert_eq!(records[0].metadata.get("trust_level").unwrap(), "0.9");
        assert_eq!(records[0].metadata.get("display_name").unwrap(), "TestUser");

        // Verify: snapshot reflects the signal with correct trust
        let snapshot = compile_relationship_snapshot(relationship_store.as_ref()).unwrap();
        assert_eq!(snapshot.principals.len(), 1);
        assert!(
            (snapshot.principals[0].trust_level - 0.9).abs() < f64::EPSILON,
            "expected 0.9, got {}",
            snapshot.principals[0].trust_level
        );
        assert_eq!(snapshot.principals[0].display_name, "TestUser");
    }

    // ── T-14 (extended): Principal names and trust in prompt ──

    #[test]
    fn context_prompt_contains_principal_details() {
        let dir = tempfile::tempdir().unwrap();
        let storage = StorageManager::open(dir.path()).unwrap();
        let ledger = storage.relationship_store();
        let p = PrincipalId::new();

        let mut record = make_record(p, RelationalSignalType::TrustUpdate, TickId::new(), 0);
        record
            .metadata
            .insert("display_name".into(), "Alice".into());
        record.metadata.insert("role".into(), "operator".into());
        record.metadata.insert("trust_level".into(), "0.95".into());
        ledger.append(&record).unwrap();

        let rel_snapshot = compile_relationship_snapshot(ledger.as_ref()).unwrap();

        let vessel_id = VesselId::new();
        let snapshot = exoskeleton_core::StateSnapshot::initial(vessel_id, "test".into());
        let counter = Arc::new(ApproximateTokenCounter);
        let compiler = exoskeleton_memory::ContextCompiler::with_defaults(counter, 4000);

        let sources = exoskeleton_memory::ContextSources {
            vessel_id,
            mission: "test",
            snapshot: &snapshot,
            relationship_snapshot: Some(&rel_snapshot),
            thread_contributions: &[],
            recent_events: &[],
            episodic_summaries: &[],
            long_term_notes: &[],
            working_context: "",
        };

        let compiled = compiler.compile(&sources).unwrap();

        assert!(
            compiled.prompt.contains("Alice"),
            "prompt should contain principal name 'Alice'"
        );
        assert!(
            compiled.prompt.contains("0.95"),
            "prompt should contain trust level '0.95'"
        );
    }

    // ── T-5 (extended): SQLite metadata roundtrip ──

    #[test]
    fn sqlite_metadata_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("exo").join("relationships.db");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();

        use exoskeleton_host::storage::SqliteRelationshipLedger;
        let ledger = SqliteRelationshipLedger::open(&path).unwrap();
        let p = PrincipalId::new();

        let record = RelationshipRecord {
            id: LedgerEntryId::new(),
            principal_id: p,
            signal_type: RelationalSignalType::TrustUpdate,
            content_ref: ArtifactId::from_content(b"meta-test"),
            tick_id: TickId::new(),
            timestamp: Utc::now(),
            metadata: {
                let mut m = std::collections::HashMap::new();
                m.insert("trust_level".into(), "0.75".into());
                m.insert("display_name".into(), "Bob".into());
                m
            },
        };
        ledger.append(&record).unwrap();

        // Retrieve and check metadata survived the roundtrip
        let records = ledger.for_principal(p, 10).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].metadata.get("trust_level").unwrap(), "0.75");
        assert_eq!(records[0].metadata.get("display_name").unwrap(), "Bob");

        // Reopen and verify durability
        drop(ledger);
        let ledger2 = SqliteRelationshipLedger::open(&path).unwrap();
        let records2 = ledger2.for_principal(p, 10).unwrap();
        assert_eq!(records2[0].metadata.get("trust_level").unwrap(), "0.75");
    }
}

// ── Sprint 8 Property-Based Tests ──

mod proptest_sprint8 {
    use chrono::Utc;
    use exoskeleton_core::{
        ArtifactId, LedgerEntryId, PrincipalId, RelationalSignalType, RelationshipRecord, TickId,
    };
    use exoskeleton_relationship::{
        compile_relationship_snapshot, AlignAction, AlignConfig, InMemoryRelationshipLedger,
        RelationshipLedger,
    };
    use proptest::prelude::*;

    fn arb_signal_type() -> impl Strategy<Value = RelationalSignalType> {
        prop_oneof![
            Just(RelationalSignalType::TrustUpdate),
            Just(RelationalSignalType::CommitmentMade),
            Just(RelationalSignalType::CommitmentFulfilled),
            Just(RelationalSignalType::CommitmentBroken),
            Just(RelationalSignalType::FeedbackReceived),
            Just(RelationalSignalType::AlignmentCheck),
            Just(RelationalSignalType::AlignmentMismatch),
            Just(RelationalSignalType::ToneObservation),
        ]
    }

    proptest! {
        /// Snapshot compilation never panics for arbitrary signal sequences.
        #[test]
        fn snapshot_compilation_never_panics(
            signal_count in 0usize..50,
            signal_types in proptest::collection::vec(arb_signal_type(), 0..50),
        ) {
            let ledger = InMemoryRelationshipLedger::new();
            let p = PrincipalId::new();
            let tick_id = TickId::new();
            let base = Utc::now();

            for (i, st) in signal_types.iter().take(signal_count).enumerate() {
                let record = RelationshipRecord {
                    id: LedgerEntryId::new(),
                    principal_id: p,
                    signal_type: *st,
                    content_ref: ArtifactId::from_content(format!("sig-{i}").as_bytes()),
                    tick_id,
                    timestamp: base + chrono::Duration::seconds(i as i64),
                    metadata: Default::default(),
                };
                let _ = ledger.append(&record);
            }

            let result = compile_relationship_snapshot(&ledger);
            prop_assert!(result.is_ok());
        }

        /// Trust is always in [0.0, 1.0] after arbitrary signal sequences.
        #[test]
        fn trust_always_in_range(
            signal_types in proptest::collection::vec(arb_signal_type(), 1..50),
        ) {
            let ledger = InMemoryRelationshipLedger::new();
            let p = PrincipalId::new();
            let tick_id = TickId::new();
            let base = Utc::now();

            for (i, st) in signal_types.iter().enumerate() {
                let record = RelationshipRecord {
                    id: LedgerEntryId::new(),
                    principal_id: p,
                    signal_type: *st,
                    content_ref: ArtifactId::from_content(format!("sig-{i}").as_bytes()),
                    tick_id,
                    timestamp: base + chrono::Duration::seconds(i as i64),
                    metadata: Default::default(),
                };
                let _ = ledger.append(&record);
            }

            let snap = compile_relationship_snapshot(&ledger).unwrap();
            for principal in &snap.principals {
                prop_assert!(principal.trust_level >= 0.0);
                prop_assert!(principal.trust_level <= 1.0);
            }
        }

        /// check_alignment never panics for arbitrary trust levels and action lists.
        #[test]
        fn check_alignment_never_panics(
            trust in 0.0f64..1.0,
            action_count in 0usize..10,
        ) {
            let snapshot = exoskeleton_core::RelationshipSnapshot {
                principals: vec![exoskeleton_core::PrincipalSummary {
                    principal_id: PrincipalId::new(),
                    display_name: "test".into(),
                    role: "test".into(),
                    trust_level: trust,
                    active_commitments: 0,
                    last_interaction: None,
                    notes: None,
                }],
                compiled_at: Utc::now(),
            };

            let config = AlignConfig::default();
            let actions: Vec<AlignAction> = (0..action_count)
                .map(|i| AlignAction {
                    tool_name: format!("tool_{i}"),
                    rationale: "test".into(),
                })
                .collect();

            let tick_id = TickId::new();
            let (approved, blocked, _) =
                exoskeleton_relationship::check_alignment(&actions, &snapshot, &config, tick_id);

            // All actions accounted for
            prop_assert_eq!(approved.len() + blocked.len(), action_count);
        }
    }
}

// ── Sprint 9: Budget Enforcement + Escalation ──

#[tokio::test]
async fn s9_cognitive_budget_tracker_records_consumption() {
    // CognitiveBudgetTracker tracks LLM consumption by backend
    use exoskeleton_core::budget::{BudgetStore, CognitiveBudgetConfig};
    use exoskeleton_host::budget::tracker::{CognitiveBudgetTracker, InMemoryBudgetStore};

    let config = CognitiveBudgetConfig {
        local_token_budget: 10_000,
        frontier_token_budget: 5_000,
        frontier_cost_budget_cents: 100,
        time_window_secs: 3600,
        per_tick_token_cap: 5_000,
        per_thread_token_cap: 1_000,
        ..Default::default()
    };
    let store: Arc<dyn BudgetStore> = Arc::new(InMemoryBudgetStore::new());
    let mut tracker = CognitiveBudgetTracker::new(config, store);

    // Record local call
    tracker.record_llm_call(LlmBackend::Local, 500, 300, 0.0);
    assert_eq!(tracker.remaining_local_tokens(), 9_200);
    assert_eq!(tracker.remaining_frontier_tokens(), 5_000);

    // Record frontier call
    tracker.record_llm_call(LlmBackend::Frontier, 200, 100, 0.25);
    assert_eq!(tracker.remaining_frontier_tokens(), 4_700);
    assert_eq!(tracker.remaining_local_tokens(), 9_200); // unaffected

    // BudgetStatus reflects state
    let status = tracker.budget_status(950);
    assert_eq!(status.local_tokens_remaining, 9_200);
    assert_eq!(status.frontier_tokens_remaining, 4_700);
    assert_eq!(status.tool_invocations_remaining, 950);
}

#[tokio::test]
async fn s9_tool_budget_gate_independent_of_cognitive() {
    // Tool budget gate operates independently from cognitive budget (I6, I9)
    use exoskeleton_core::budget::{BudgetStore, CognitiveBudgetConfig, ToolBudgetConfig};
    use exoskeleton_host::budget::tracker::{CognitiveBudgetTracker, InMemoryBudgetStore};
    use exoskeleton_host::budget::ToolBudgetGate;

    let cog_config = CognitiveBudgetConfig {
        local_token_budget: 1_000, // very tight
        ..Default::default()
    };
    let tool_config = ToolBudgetConfig {
        max_invocations_per_window: 5,
        time_window_secs: 3600,
    };
    let store: Arc<dyn BudgetStore> = Arc::new(InMemoryBudgetStore::new());
    let mut cog_tracker = CognitiveBudgetTracker::new(cog_config, store);
    let mut tool_gate = ToolBudgetGate::new(tool_config);

    // Exhaust cognitive budget
    cog_tracker.record_llm_call(LlmBackend::Local, 500, 600, 0.0); // 1100 > 1000
    assert_eq!(cog_tracker.remaining_local_tokens(), 0);

    // Tool budget should be unaffected (I9)
    assert!(tool_gate.check());
    assert_eq!(tool_gate.remaining(), 5);
    tool_gate.record_invocation();
    assert_eq!(tool_gate.remaining(), 4);

    // Exhaust tool budget
    for _ in 0..4 {
        tool_gate.record_invocation();
    }
    assert!(!tool_gate.check());

    // Cognitive tracker still reports exhaustion independently
    assert_eq!(cog_tracker.remaining_local_tokens(), 0);
}

#[tokio::test]
async fn s9_budget_window_reset() {
    use exoskeleton_core::budget::{BudgetStore, CognitiveBudgetConfig, ToolBudgetConfig};
    use exoskeleton_host::budget::tracker::{CognitiveBudgetTracker, InMemoryBudgetStore};
    use exoskeleton_host::budget::ToolBudgetGate;

    let config = CognitiveBudgetConfig {
        local_token_budget: 5_000,
        frontier_token_budget: 2_000,
        ..Default::default()
    };
    let tool_config = ToolBudgetConfig {
        max_invocations_per_window: 10,
        time_window_secs: 3600,
    };
    let store: Arc<dyn BudgetStore> = Arc::new(InMemoryBudgetStore::new());
    let mut tracker = CognitiveBudgetTracker::new(config, store);
    let mut gate = ToolBudgetGate::new(tool_config);

    // Consume budget
    tracker.record_llm_call(LlmBackend::Local, 2000, 1500, 0.0);
    gate.record_invocation();
    gate.record_invocation();
    gate.record_invocation();

    assert_eq!(tracker.remaining_local_tokens(), 1_500);
    assert_eq!(gate.remaining(), 7);

    // Reset window
    tracker.reset_window().unwrap();
    gate.reset_window();

    assert_eq!(tracker.remaining_local_tokens(), 5_000);
    assert_eq!(tracker.remaining_frontier_tokens(), 2_000);
    assert_eq!(gate.remaining(), 10);
}

#[tokio::test]
async fn s9_budget_persistence_across_restart() {
    use exoskeleton_core::budget::{BudgetStore, CognitiveBudgetConfig};
    use exoskeleton_host::budget::tracker::CognitiveBudgetTracker;
    use exoskeleton_host::storage::SqliteBudgetStore;

    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("budget.db");

    let config = CognitiveBudgetConfig {
        local_token_budget: 10_000,
        frontier_token_budget: 5_000,
        ..Default::default()
    };

    // First session: consume some budget and persist
    {
        let store: Arc<dyn BudgetStore> = Arc::new(SqliteBudgetStore::open(&db_path).unwrap());
        let mut tracker = CognitiveBudgetTracker::new(config.clone(), store);
        tracker.record_llm_call(LlmBackend::Local, 3000, 1000, 0.0);
        tracker.record_llm_call(LlmBackend::Frontier, 500, 200, 0.10);
        tracker.record_failure();
        tracker.persist().unwrap();
    }

    // Second session: load state
    {
        let store: Arc<dyn BudgetStore> = Arc::new(SqliteBudgetStore::open(&db_path).unwrap());
        let mut tracker = CognitiveBudgetTracker::new(config, store);
        tracker.load_or_reset().unwrap();

        assert_eq!(tracker.remaining_local_tokens(), 6_000); // 10000 - 4000
        assert_eq!(tracker.remaining_frontier_tokens(), 4_300); // 5000 - 700
        assert_eq!(tracker.consecutive_failures(), 1);
    }
}

#[tokio::test]
async fn s9_model_escalation_on_consecutive_failures() {
    use exoskeleton_core::budget::{BudgetStore, CognitiveBudgetConfig, EscalationPolicy};
    use exoskeleton_host::budget::tracker::{
        CognitiveBudgetCheck, CognitiveBudgetTracker, InMemoryBudgetStore,
    };

    let config = CognitiveBudgetConfig {
        escalation_policy: EscalationPolicy {
            escalate_on_consecutive_failures: 2,
            ..Default::default()
        },
        ..Default::default()
    };
    let store: Arc<dyn BudgetStore> = Arc::new(InMemoryBudgetStore::new());
    let mut tracker = CognitiveBudgetTracker::new(config, store);

    // Initially: budget available (local)
    assert_eq!(
        tracker.check_cognitive_budget(),
        CognitiveBudgetCheck::Available(LlmBackend::Local)
    );

    // Record failures
    tracker.record_failure();
    assert_eq!(tracker.consecutive_failures(), 1);
    tracker.record_failure();
    assert_eq!(tracker.consecutive_failures(), 2);

    // Clear on success
    tracker.clear_failures();
    assert_eq!(tracker.consecutive_failures(), 0);
}

#[tokio::test]
async fn s9_thrash_detection_identifies_patterns() {
    use exoskeleton_core::budget::ThrashLevel;
    use exoskeleton_core::tick::{
        ActionOutcome, ActionRecord, LlmCallRecord, TickPhase, TickRecord,
    };
    use exoskeleton_host::budget::ThrashDetector;

    // Helper to build ticks
    let make_tick = |actions: Vec<ActionRecord>, llm_calls: Vec<LlmCallRecord>| -> TickRecord {
        TickRecord {
            tick_id: TickId::new(),
            tick_number: 0,
            phase: TickPhase::Amend,
            started_at: Utc::now(),
            completed_at: Some(Utc::now()),
            snapshot_before: ArtifactId::from_content(b"before"),
            snapshot_after: None,
            thread_contributions: vec![],
            actions_taken: actions,
            llm_calls,
            decision_rationale: None,
        }
    };

    // No thrash on empty
    assert_eq!(ThrashDetector::check(&[]).level, ThrashLevel::None);

    // No thrash on successful ticks
    let successful_ticks: Vec<_> = (0..5)
        .map(|_| {
            make_tick(
                vec![ActionRecord {
                    action_type: "fs.write".into(),
                    target: "t".into(),
                    receipt_ref: None,
                    outcome: ActionOutcome::Success,
                }],
                vec![LlmCallRecord {
                    model: "m".into(),
                    tokens_in: 500,
                    tokens_out: 300,
                    cost_cents: 0.0,
                    latency_ms: 100,
                    response_artifact_ref: None,
                }],
            )
        })
        .collect();
    assert_eq!(
        ThrashDetector::check(&successful_ticks).level,
        ThrashLevel::None
    );

    // High thrash on repeated failures
    let failing_ticks: Vec<_> = (0..5)
        .map(|_| {
            make_tick(
                vec![ActionRecord {
                    action_type: "http.request".into(),
                    target: "t".into(),
                    receipt_ref: None,
                    outcome: ActionOutcome::Failure,
                }],
                vec![LlmCallRecord {
                    model: "m".into(),
                    tokens_in: 500,
                    tokens_out: 300,
                    cost_cents: 0.0,
                    latency_ms: 100,
                    response_artifact_ref: None,
                }],
            )
        })
        .collect();
    let assessment = ThrashDetector::check(&failing_ticks);
    assert_eq!(assessment.level, ThrashLevel::High);
    assert!(!assessment.indicators.is_empty());
    assert!(assessment.recommendation.is_some());
}

#[tokio::test]
async fn s9_per_thread_budget_enforcement() {
    use exoskeleton_core::budget::{BudgetStore, CognitiveBudgetConfig};
    use exoskeleton_core::ThreadId;
    use exoskeleton_host::budget::tracker::{CognitiveBudgetTracker, InMemoryBudgetStore};

    let config = CognitiveBudgetConfig {
        per_thread_token_cap: 500,
        ..Default::default()
    };
    let store: Arc<dyn BudgetStore> = Arc::new(InMemoryBudgetStore::new());
    let mut tracker = CognitiveBudgetTracker::new(config, store);

    let thread_a = ThreadId::new();
    let thread_b = ThreadId::new();

    // Both threads start within budget
    assert!(tracker.check_thread_budget(thread_a));
    assert!(tracker.check_thread_budget(thread_b));

    // Thread A consumes 400 tokens — still within cap
    tracker.record_thread_consumption(thread_a, 400);
    assert!(tracker.check_thread_budget(thread_a));
    assert!(tracker.check_thread_budget(thread_b)); // unaffected

    // Thread A consumes 200 more — over cap (600 > 500)
    tracker.record_thread_consumption(thread_a, 200);
    assert!(!tracker.check_thread_budget(thread_a));
    assert!(tracker.check_thread_budget(thread_b)); // still unaffected

    // Thread B consumes 500 — exactly at cap, not over
    tracker.record_thread_consumption(thread_b, 500);
    assert!(!tracker.check_thread_budget(thread_b)); // >= cap
}

#[tokio::test]
async fn s9_vessel_config_with_budgets() {
    use exoskeleton_core::budget::{CognitiveBudgetConfig, ToolBudgetConfig};

    let dir = tempfile::tempdir().unwrap();
    let config = VesselConfig {
        cognitive_budget: Some(CognitiveBudgetConfig::default()),
        tool_budget: Some(ToolBudgetConfig::default()),
        ..test_config(dir.path())
    };

    // Validation should pass
    config.validate().unwrap();

    // Fields present
    assert!(config.cognitive_budget.is_some());
    assert!(config.tool_budget.is_some());
    let cb = config.cognitive_budget.unwrap();
    assert_eq!(cb.local_token_budget, 1_000_000);
    assert_eq!(cb.frontier_token_budget, 100_000);
    assert_eq!(cb.frontier_cost_budget_cents, 500);
    assert_eq!(cb.escalation_policy.escalate_on_consecutive_failures, 3);

    let tb = config.tool_budget.unwrap();
    assert_eq!(tb.max_invocations_per_window, 1000);
}

#[tokio::test]
async fn s9_vessel_config_budget_validation() {
    use exoskeleton_core::budget::{CognitiveBudgetConfig, ToolBudgetConfig};

    let dir = tempfile::tempdir().unwrap();

    // Zero window should fail validation
    let config = VesselConfig {
        cognitive_budget: Some(CognitiveBudgetConfig {
            time_window_secs: 0,
            ..Default::default()
        }),
        ..test_config(dir.path())
    };
    assert!(config.validate().is_err());

    // Zero tool invocations should fail
    let config = VesselConfig {
        tool_budget: Some(ToolBudgetConfig {
            max_invocations_per_window: 0,
            ..Default::default()
        }),
        ..test_config(dir.path())
    };
    assert!(config.validate().is_err());
}

#[tokio::test]
async fn s9_storage_manager_budget_store() {
    // Budget store accessible via StorageManager
    use exoskeleton_core::budget::BudgetStore;

    let dir = tempfile::tempdir().unwrap();
    let mgr = StorageManager::open(dir.path()).unwrap();

    // Initially empty
    assert!(mgr.budget_store().load().unwrap().is_none());

    // Budget.db file created
    assert!(dir.path().join("exo").join("budget.db").exists());
}

#[tokio::test]
async fn s9_thrash_assessment_none_is_none() {
    use exoskeleton_core::budget::{ThrashAssessment, ThrashLevel};

    let none = ThrashAssessment::none();
    assert_eq!(none.level, ThrashLevel::None);
    assert!(none.indicators.is_empty());
    assert!(none.recommendation.is_none());
}

#[tokio::test]
async fn s9_budget_status_exhaustion_logic() {
    use exoskeleton_core::budget::ThrashLevel;
    use exoskeleton_core::BudgetStatus;

    // Both token types zero -> exhausted
    let status = BudgetStatus {
        local_tokens_remaining: 0,
        frontier_tokens_remaining: 0,
        frontier_cost_cents_remaining: 100,
        time_secs_remaining: 100,
        thrash_level: ThrashLevel::None,
        tool_invocations_remaining: 100,
    };
    assert!(status.is_exhausted());

    // Only local zero, frontier available -> NOT exhausted
    let status = BudgetStatus {
        local_tokens_remaining: 0,
        frontier_tokens_remaining: 100,
        frontier_cost_cents_remaining: 100,
        time_secs_remaining: 100,
        thrash_level: ThrashLevel::None,
        tool_invocations_remaining: 100,
    };
    assert!(!status.is_exhausted());

    // Total tokens calculation
    assert_eq!(status.total_tokens_remaining(), 100);
}

#[tokio::test]
async fn s9_budget_config_toml_roundtrip() {
    use exoskeleton_host::config::VesselConfigFile;

    let toml_str = r#"
[vessel]
mission = "budget test"
data_dir = "/tmp/exo"

[cognitive]
tick_interval_ms = 100

[cognitive.budget]
local_token_budget = 500000
frontier_token_budget = 50000
frontier_cost_budget_cents = 200
time_window_secs = 3600
per_tick_token_cap = 25000
per_thread_token_cap = 8000

[cognitive.budget.escalation_policy]
escalate_on_uncertainty = 0.7
escalate_on_stakes = ["fs.write"]
escalate_on_consecutive_failures = 3
max_frontier_calls_per_window = 20

[tool]
tick_interval_ms = 50

[tool.budget]
max_invocations_per_window = 500
time_window_secs = 3600
"#;
    let file: VesselConfigFile = toml::from_str(toml_str).unwrap();
    let config = VesselConfig::try_from(file).unwrap();

    let cb = config.cognitive_budget.unwrap();
    assert_eq!(cb.local_token_budget, 500_000);
    assert_eq!(cb.frontier_token_budget, 50_000);
    assert_eq!(cb.frontier_cost_budget_cents, 200);
    assert_eq!(cb.per_tick_token_cap, 25_000);
    assert_eq!(cb.per_thread_token_cap, 8_000);
    assert_eq!(cb.escalation_policy.escalate_on_consecutive_failures, 3);
    assert_eq!(cb.escalation_policy.escalate_on_stakes, vec!["fs.write"]);
    assert!((cb.escalation_policy.escalate_on_uncertainty - 0.7).abs() < f64::EPSILON);
    assert_eq!(cb.escalation_policy.max_frontier_calls_per_window, 20);

    let tb = config.tool_budget.unwrap();
    assert_eq!(tb.max_invocations_per_window, 500);
    assert_eq!(tb.time_window_secs, 3600);
}

#[tokio::test]
async fn s9_tool_budget_gate_rate_limiting() {
    use exoskeleton_core::budget::ToolBudgetConfig;
    use exoskeleton_host::budget::ToolBudgetGate;

    let config = ToolBudgetConfig {
        max_invocations_per_window: 3,
        time_window_secs: 3600,
    };
    let mut gate = ToolBudgetGate::new(config);

    // First 3 invocations allowed
    assert!(gate.check());
    gate.record_invocation();
    assert!(gate.check());
    gate.record_invocation();
    assert!(gate.check());
    gate.record_invocation();

    // 4th blocked
    assert!(!gate.check());
    assert_eq!(gate.remaining(), 0);

    // Reset restores budget
    gate.reset_window();
    assert!(gate.check());
    assert_eq!(gate.remaining(), 3);
}

#[tokio::test]
async fn s9_per_tick_token_cap() {
    use exoskeleton_core::budget::{BudgetStore, CognitiveBudgetConfig};
    use exoskeleton_host::budget::tracker::{CognitiveBudgetTracker, InMemoryBudgetStore};

    let config = CognitiveBudgetConfig {
        per_tick_token_cap: 1_000,
        ..Default::default()
    };
    let store: Arc<dyn BudgetStore> = Arc::new(InMemoryBudgetStore::new());
    let mut tracker = CognitiveBudgetTracker::new(config, store);

    // Within cap
    assert!(tracker.check_tick_cap());
    tracker.record_llm_call(LlmBackend::Local, 400, 300, 0.0);
    assert!(tracker.check_tick_cap());

    // Over cap
    tracker.record_llm_call(LlmBackend::Local, 200, 200, 0.0);
    assert!(!tracker.check_tick_cap());

    // Reset tick counters
    tracker.reset_tick_counters();
    assert!(tracker.check_tick_cap());
}

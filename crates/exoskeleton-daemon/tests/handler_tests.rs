//! Unit tests for daemon HTTP handlers (T-6).
//!
//! Uses tower::ServiceExt::oneshot to test the axum router directly
//! without starting a TCP server.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use exoskeleton_core::id::derive_external_principal_id;
use exoskeleton_core::inbox::Inbox;
use exoskeleton_core::{
    ArtifactId, ArtifactStore, CapabilityRequestPayload, ConversationStore, EnvelopeKind,
    EventEntry, EventLedger, EventType, InMemoryWatchStore, LedgerEntryId, SnapshotStore,
    StateSnapshot, TickStore, VesselId, VesselMode,
};
use exoskeleton_daemon::routes::build_router;
use exoskeleton_daemon::state::AppState;
use exoskeleton_host::inspect::VesselInspector;
use exoskeleton_host::metrics::ExoMetrics;
use exoskeleton_host::storage::StorageManager;
use exoskeleton_host::InMemoryInbox;
use exoskeleton_relationship::InMemoryRelationshipLedger;
use exoskeleton_threads::{InMemoryThreadStore, ThreadRegistry};
use http_body_util::BodyExt;
use tower::ServiceExt;

fn test_app_state(dir: &std::path::Path) -> Arc<AppState> {
    let storage = StorageManager::open(dir).unwrap();
    let thread_store = Arc::new(InMemoryThreadStore::new());
    let thread_registry = Arc::new(ThreadRegistry::new(thread_store.clone()));
    let relationship_ledger: Arc<dyn exoskeleton_relationship::RelationshipLedger> =
        Arc::new(InMemoryRelationshipLedger::new());
    let wi_host_slot = Arc::new(tokio::sync::Mutex::new(None));
    let inbox: Arc<dyn Inbox> = Arc::new(InMemoryInbox::new());
    let config = exoskeleton_host::VesselConfig {
        mission: "test".into(),
        ..Default::default()
    };

    let inspector = VesselInspector::new(
        storage,
        thread_registry.clone(),
        relationship_ledger,
        None,
        None,
        wi_host_slot,
        config,
    );
    let metrics = Arc::new(ExoMetrics::new().unwrap());

    Arc::new(AppState {
        inspector,
        metrics,
        inbox,
        vessel_id: VesselId::new(),
        event_tx: tokio::sync::broadcast::channel(16).0,
        cors_origins: vec![],
        acknowledged_events: Arc::new(dashmap::DashSet::new()),
        webhook_secrets: std::collections::HashMap::new(),
        charter_proposal_statuses: Arc::new(dashmap::DashMap::new()),
        watch_store: Arc::new(InMemoryWatchStore::new()),
        thread_registry,
        wi_host_slot: Arc::new(tokio::sync::Mutex::new(None)),
        connectors_dir: None,
        align_config: None,
        cognitive_engine: Arc::new(tokio::sync::Mutex::new(None)),
        vessel_mode: Arc::new(std::sync::Mutex::new(VesselMode::Normal)),
        questions_dir: dir.join("questions"),
        answers_dir: dir.join("answers"),
    })
}

fn test_app_state_with_cors(dir: &std::path::Path, origins: Vec<&str>) -> Arc<AppState> {
    let storage = StorageManager::open(dir).unwrap();
    let thread_store = Arc::new(InMemoryThreadStore::new());
    let thread_registry = Arc::new(ThreadRegistry::new(thread_store.clone()));
    let relationship_ledger: Arc<dyn exoskeleton_relationship::RelationshipLedger> =
        Arc::new(InMemoryRelationshipLedger::new());
    let wi_host_slot = Arc::new(tokio::sync::Mutex::new(None));
    let inbox: Arc<dyn Inbox> = Arc::new(InMemoryInbox::new());
    let config = exoskeleton_host::VesselConfig {
        mission: "test".into(),
        ..Default::default()
    };

    let inspector = VesselInspector::new(
        storage,
        thread_registry.clone(),
        relationship_ledger,
        None,
        None,
        wi_host_slot,
        config,
    );
    let metrics = Arc::new(ExoMetrics::new().unwrap());
    let cors_origins = origins.into_iter().map(|o| o.parse().unwrap()).collect();

    Arc::new(AppState {
        inspector,
        metrics,
        inbox,
        vessel_id: VesselId::new(),
        event_tx: tokio::sync::broadcast::channel(16).0,
        cors_origins,
        acknowledged_events: Arc::new(dashmap::DashSet::new()),
        webhook_secrets: std::collections::HashMap::new(),
        charter_proposal_statuses: Arc::new(dashmap::DashMap::new()),
        watch_store: Arc::new(InMemoryWatchStore::new()),
        thread_registry,
        wi_host_slot: Arc::new(tokio::sync::Mutex::new(None)),
        connectors_dir: None,
        align_config: None,
        cognitive_engine: Arc::new(tokio::sync::Mutex::new(None)),
        vessel_mode: Arc::new(std::sync::Mutex::new(VesselMode::Normal)),
        questions_dir: dir.join("questions"),
        answers_dir: dir.join("answers"),
    })
}

async fn body_string(body: Body) -> String {
    let bytes = body.collect().await.unwrap().to_bytes();
    String::from_utf8(bytes.to_vec()).unwrap()
}

#[tokio::test]
async fn healthz_returns_200() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_app_state(dir.path());
    let app = build_router(state);

    let resp = app
        .oneshot(Request::get("/healthz").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn ready_returns_503_when_engines_unavailable() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_app_state(dir.path());
    let app = build_router(state);

    let resp = app
        .oneshot(Request::get("/ready").body(Body::empty()).unwrap())
        .await
        .unwrap();

    // No engines populated → 503
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn metrics_returns_prometheus_format() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_app_state(dir.path());
    // Touch a metric so there's output
    state
        .metrics
        .ticks_total
        .with_label_values(&["completed"])
        .inc();

    let app = build_router(state);
    let resp = app
        .oneshot(Request::get("/metrics").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    assert!(
        body.contains("exo_ticks_total"),
        "metrics should contain exo_ticks_total"
    );
}

#[tokio::test]
async fn get_status_returns_204_when_empty() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_app_state(dir.path());
    let app = build_router(state);

    let resp = app
        .oneshot(Request::get("/api/v1/status").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn get_status_returns_snapshot_when_present() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_app_state(dir.path());

    // Use shared storage so handler reads from the same SQLite connection.
    let snap = StateSnapshot::initial(VesselId::new(), "test mission".into());
    state
        .inspector
        .storage()
        .snapshot_store()
        .save(&snap)
        .unwrap();

    let app = build_router(state);
    let resp = app
        .oneshot(Request::get("/api/v1/status").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["mission"], "test mission");
}

#[tokio::test]
async fn get_ticks_respects_limit() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_app_state(dir.path());
    let app = build_router(state);

    let resp = app
        .oneshot(
            Request::get("/api/v1/ticks?limit=5")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    let json: Vec<serde_json::Value> = serde_json::from_str(&body).unwrap();
    // No ticks stored yet, should be empty
    assert!(json.is_empty());
}

#[tokio::test]
async fn get_tick_nonexistent_returns_404() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_app_state(dir.path());
    let app = build_router(state);

    let resp = app
        .oneshot(
            Request::get("/api/v1/ticks/550e8400-e29b-41d4-a716-446655440000")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn get_threads_returns_empty_array() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_app_state(dir.path());
    let app = build_router(state);

    let resp = app
        .oneshot(Request::get("/api/v1/threads").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    let json: Vec<serde_json::Value> = serde_json::from_str(&body).unwrap();
    assert!(json.is_empty());
}

#[tokio::test]
async fn post_inbox_creates_envelope() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_app_state(dir.path());
    let app = build_router(state);

    let body = serde_json::json!({
        "source": "550e8400-e29b-41d4-a716-446655440000",
        "content": "Hello from test"
    });

    let resp = app
        .oneshot(
            Request::post("/api/v1/inbox")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_string(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::CREATED);
    let body = body_string(resp.into_body()).await;
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(json["envelope_id"].is_string());
}

#[tokio::test]
async fn post_inbox_invalid_body_returns_error() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_app_state(dir.path());
    let app = build_router(state);

    let resp = app
        .oneshot(
            Request::post("/api/v1/inbox")
                .header("content-type", "application/json")
                .body(Body::from("{\"bad\":\"json\"}"))
                .unwrap(),
        )
        .await
        .unwrap();

    // axum returns 422 Unprocessable Entity for JSON parse failures
    assert!(resp.status().is_client_error());
}

#[tokio::test]
async fn get_events_returns_events() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_app_state(dir.path());

    // Use shared storage so handler reads from the same SQLite connection.
    let event = EventEntry {
        id: LedgerEntryId::new(),
        tick_id: None,
        event_type: EventType::VesselStarted,
        payload_ref: None,
        summary: "test event".into(),
        timestamp: chrono::Utc::now(),
    };
    state
        .inspector
        .storage()
        .event_ledger()
        .append(&event)
        .unwrap();

    let app = build_router(state);
    let resp = app
        .oneshot(
            Request::get("/api/v1/events?limit=10")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    let json: Vec<serde_json::Value> = serde_json::from_str(&body).unwrap();
    assert_eq!(json.len(), 1);
}

#[tokio::test]
async fn get_engines_returns_status() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_app_state(dir.path());
    let app = build_router(state);

    let resp = app
        .oneshot(Request::get("/api/v1/engines").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(json["cognitive"].is_object());
    assert!(json["tool"].is_object());
}

// ── D2: New endpoint handler tests ──

#[tokio::test]
async fn get_artifact_returns_404_for_missing() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_app_state(dir.path());
    let app = build_router(state);

    let resp = app
        .oneshot(
            Request::get("/api/v1/artifacts/deadbeef1234567890abcdef1234567890abcdef1234567890abcdef12345678")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn get_memory_returns_both_types_by_default() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_app_state(dir.path());
    let app = build_router(state);

    let resp = app
        .oneshot(
            Request::get("/api/v1/memory?limit=10")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    // Both fields should be present (even if empty arrays)
    assert!(json["episodic"].is_array());
    assert!(json["long_term"].is_array());
}

#[tokio::test]
async fn get_memory_filters_by_type_episodic() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_app_state(dir.path());
    let app = build_router(state);

    let resp = app
        .oneshot(
            Request::get("/api/v1/memory?type=episodic&limit=10")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(json["episodic"].is_array());
    // long_term should be omitted
    assert!(json.get("long_term").is_none());
}

#[tokio::test]
async fn get_memory_filters_by_type_long_term() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_app_state(dir.path());
    let app = build_router(state);

    let resp = app
        .oneshot(
            Request::get("/api/v1/memory?type=long_term&limit=10")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(json["long_term"].is_array());
    // episodic should be omitted
    assert!(json.get("episodic").is_none());
}

#[tokio::test]
async fn get_snapshots_returns_empty_initially() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_app_state(dir.path());
    let app = build_router(state);

    let resp = app
        .oneshot(
            Request::get("/api/v1/snapshots?limit=10")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    let json: Vec<serde_json::Value> = serde_json::from_str(&body).unwrap();
    assert!(json.is_empty());
}

#[tokio::test]
async fn get_snapshots_returns_stored_snapshots() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_app_state(dir.path());

    // Use shared storage so handler reads from the same SQLite connection.
    let mut snap = StateSnapshot::initial(VesselId::new(), "test".into());
    snap.tick_number = 1;
    state
        .inspector
        .storage()
        .snapshot_store()
        .save(&snap)
        .unwrap();
    snap.tick_number = 2;
    state
        .inspector
        .storage()
        .snapshot_store()
        .save(&snap)
        .unwrap();

    let app = build_router(state);
    let resp = app
        .oneshot(
            Request::get("/api/v1/snapshots?limit=10")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    let json: Vec<serde_json::Value> = serde_json::from_str(&body).unwrap();
    assert_eq!(json.len(), 2);
}

#[tokio::test]
async fn get_snapshot_at_tick_returns_404_for_missing() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_app_state(dir.path());
    let app = build_router(state);

    let resp = app
        .oneshot(
            Request::get("/api/v1/snapshots/at/99999")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn get_snapshot_at_tick_returns_200_for_existing() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_app_state(dir.path());

    // Use shared storage so handler reads from the same SQLite connection.
    let mut snap = StateSnapshot::initial(VesselId::new(), "snapshot-at test".into());
    snap.tick_number = 42;
    state
        .inspector
        .storage()
        .snapshot_store()
        .save(&snap)
        .unwrap();

    let app = build_router(state);
    let resp = app
        .oneshot(
            Request::get("/api/v1/snapshots/at/42")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["tick_number"], 42);
}

#[tokio::test]
async fn get_inbox_history_returns_empty() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_app_state(dir.path());
    let app = build_router(state);

    let resp = app
        .oneshot(
            Request::get("/api/v1/inbox/history?limit=10")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    let json: Vec<serde_json::Value> = serde_json::from_str(&body).unwrap();
    assert!(json.is_empty());
}

#[tokio::test]
async fn get_config_returns_sanitized_config() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_app_state(dir.path());
    let app = build_router(state);

    let resp = app
        .oneshot(Request::get("/api/v1/config").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["mission"], "test");
    assert!(json["vessel_id"].is_string());
    assert!(json["llm_default_backend"].is_string());
}

#[tokio::test]
async fn get_config_does_not_leak_api_key_values() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_app_state(dir.path());
    let app = build_router(state);

    let resp = app
        .oneshot(Request::get("/api/v1/config").body(Body::empty()).unwrap())
        .await
        .unwrap();

    let body = body_string(resp.into_body()).await;
    // The response must never contain actual API key values.
    // It may contain the env var NAME (e.g., "ANTHROPIC_API_KEY") but never a key value.
    // Since our test config has no frontier config, just verify no "sk-" prefixed strings.
    assert!(
        !body.contains("sk-"),
        "response must not contain API key values"
    );
}

// ── T-19: get_artifact returns 200 for existing artifact ──

#[tokio::test]
async fn get_artifact_returns_200_for_existing() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_app_state(dir.path());

    // Use shared storage so handler reads from the same SQLite connection.
    let artifact = exoskeleton_core::Artifact::new(
        exoskeleton_core::ArtifactKind::Receipt,
        b"hello world".to_vec(),
        "text/plain".to_string(),
    );
    let artifact_id = state
        .inspector
        .storage()
        .artifact_store()
        .put(&artifact)
        .unwrap();

    let app = build_router(state);
    let url = format!("/api/v1/artifacts/{artifact_id}");
    let resp = app
        .oneshot(Request::get(&url).body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["content_type"], "text/plain");
    assert_eq!(json["content"], "hello world");
}

// ── T-17: ArtifactResponse encodes JSON content as UTF-8 ──

#[tokio::test]
async fn get_artifact_encodes_json_as_utf8() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_app_state(dir.path());

    // Use shared storage so handler reads from the same SQLite connection.
    let json_content = r#"{"key":"value"}"#;
    let artifact = exoskeleton_core::Artifact::new(
        exoskeleton_core::ArtifactKind::Receipt,
        json_content.as_bytes().to_vec(),
        "application/json".to_string(),
    );
    let artifact_id = state
        .inspector
        .storage()
        .artifact_store()
        .put(&artifact)
        .unwrap();

    let app = build_router(state);
    let url = format!("/api/v1/artifacts/{artifact_id}");
    let resp = app
        .oneshot(Request::get(&url).body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["content"], json_content);
    assert_eq!(json["content_type"], "application/json");
}

// ── T-18: ArtifactResponse encodes binary content as Base64 ──

#[tokio::test]
async fn get_artifact_encodes_binary_as_base64() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_app_state(dir.path());

    // Use shared storage so handler reads from the same SQLite connection.
    let binary_content: Vec<u8> = vec![0x00, 0x01, 0xFF, 0xFE, 0x89, 0x50, 0x4E, 0x47];
    let artifact = exoskeleton_core::Artifact::new(
        exoskeleton_core::ArtifactKind::Receipt,
        binary_content.clone(),
        "application/octet-stream".to_string(),
    );
    let artifact_id = state
        .inspector
        .storage()
        .artifact_store()
        .put(&artifact)
        .unwrap();

    let app = build_router(state);
    let url = format!("/api/v1/artifacts/{artifact_id}");
    let resp = app
        .oneshot(Request::get(&url).body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    let content_str = json["content"].as_str().unwrap();
    use base64::Engine;
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(content_str)
        .expect("content should be valid base64");
    assert_eq!(decoded, binary_content);
}

// ── OA-T1: POST /api/v1/inbox stores content artifact retrievable by payload_ref ──

#[tokio::test]
async fn post_inbox_stores_content_artifact() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_app_state(dir.path());
    let app = build_router(state.clone());

    let body = serde_json::json!({
        "source": "550e8400-e29b-41d4-a716-446655440000",
        "content": "Hello from artifact test"
    });

    let resp = app
        .oneshot(
            Request::post("/api/v1/inbox")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_string(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::CREATED);

    let expected_id = ArtifactId::from_content(b"Hello from artifact test");
    let artifact = state
        .inspector
        .storage()
        .artifact_store()
        .get(&expected_id)
        .unwrap();
    assert!(artifact.is_some(), "content artifact should be stored");
    let artifact = artifact.unwrap();
    assert_eq!(
        String::from_utf8_lossy(&artifact.content),
        "Hello from artifact test"
    );
}

// ── OA-T2: POST /api/v1/inbox with duplicate content reuses artifact ──

#[tokio::test]
async fn post_inbox_duplicate_content_reuses_artifact() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_app_state(dir.path());

    let body = serde_json::json!({
        "source": "550e8400-e29b-41d4-a716-446655440000",
        "content": "Duplicate message"
    });
    let json_str = serde_json::to_string(&body).unwrap();

    let app1 = build_router(state.clone());
    let resp1 = app1
        .oneshot(
            Request::post("/api/v1/inbox")
                .header("content-type", "application/json")
                .body(Body::from(json_str.clone()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp1.status(), StatusCode::CREATED);

    let app2 = build_router(state.clone());
    let resp2 = app2
        .oneshot(
            Request::post("/api/v1/inbox")
                .header("content-type", "application/json")
                .body(Body::from(json_str))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp2.status(), StatusCode::CREATED);

    let body1: serde_json::Value =
        serde_json::from_str(&body_string(resp1.into_body()).await).unwrap();
    let body2: serde_json::Value =
        serde_json::from_str(&body_string(resp2.into_body()).await).unwrap();
    assert_ne!(body1["envelope_id"], body2["envelope_id"]);

    let artifact_id = ArtifactId::from_content(b"Duplicate message");
    let artifact = state
        .inspector
        .storage()
        .artifact_store()
        .get(&artifact_id)
        .unwrap();
    assert!(artifact.is_some(), "single artifact should exist for both");
}

// ── OA-T16: Artifact response kind field is snake_case ──

#[tokio::test]
async fn get_artifact_kind_is_snake_case() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_app_state(dir.path());

    // Use shared storage so handler reads from the same SQLite connection.
    let artifact = exoskeleton_core::Artifact::new(
        exoskeleton_core::ArtifactKind::ThreadOutput,
        b"thread output data".to_vec(),
        "text/plain".to_string(),
    );
    let artifact_id = state
        .inspector
        .storage()
        .artifact_store()
        .put(&artifact)
        .unwrap();

    let app = build_router(state);
    let url = format!("/api/v1/artifacts/{artifact_id}");
    let resp = app
        .oneshot(Request::get(&url).body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        json["kind"], "thread_output",
        "kind should be snake_case, got: {}",
        json["kind"]
    );
}

// ── OA-T28: GET /api/v1/conversations/:id/messages returns resolved messages ──

#[tokio::test]
async fn get_conversation_messages_returns_resolved() {
    use exoskeleton_core::conversation::Conversation;
    use exoskeleton_core::{Artifact, ArtifactKind, EnvelopeId, PrincipalId};

    let dir = tempfile::tempdir().unwrap();
    let state = test_app_state(dir.path());

    let user = PrincipalId::new();
    let content = Artifact::new(
        ArtifactKind::Envelope,
        b"Hi there".to_vec(),
        "text/plain".into(),
    );
    let payload_id = state
        .inspector
        .storage()
        .artifact_store()
        .put(&content)
        .unwrap();
    let conv =
        Conversation::from_first_message(user, EnvelopeId::new(), payload_id, chrono::Utc::now());
    state
        .inspector
        .storage()
        .conversation_store()
        .save(&conv)
        .unwrap();

    let app = build_router(state);
    let url = format!("/api/v1/conversations/{}/messages", conv.id);
    let resp = app
        .oneshot(Request::get(&url).body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    let msgs: Vec<serde_json::Value> = serde_json::from_str(&body).unwrap();
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0]["content"], "Hi there");
}

// ── OA-T29: GET /api/v1/conversations/:id/messages with unknown ID returns 404 ──

#[tokio::test]
async fn get_conversation_messages_returns_404_for_unknown() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_app_state(dir.path());
    let app = build_router(state);

    let resp = app
        .oneshot(
            Request::get("/api/v1/conversations/550e8400-e29b-41d4-a716-446655440000/messages")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// ── U1: CORS tests ──

#[tokio::test]
async fn cors_allows_configured_origin() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_app_state_with_cors(dir.path(), vec!["http://localhost:5173"]);
    let app = build_router(state);

    let resp = app
        .oneshot(
            Request::get("/api/v1/status")
                .header("origin", "http://localhost:5173")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    let acao = resp
        .headers()
        .get("access-control-allow-origin")
        .expect("should have ACAO header");
    assert_eq!(acao, "http://localhost:5173");
}

#[tokio::test]
async fn cors_rejects_unconfigured_origin() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_app_state_with_cors(dir.path(), vec!["http://localhost:5173"]);
    let app = build_router(state);

    let resp = app
        .oneshot(
            Request::get("/api/v1/status")
                .header("origin", "http://evil.example.com")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert!(
        resp.headers().get("access-control-allow-origin").is_none(),
        "should not have ACAO header for unconfigured origin"
    );
}

#[tokio::test]
async fn cors_allows_get_and_post_methods() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_app_state_with_cors(dir.path(), vec!["http://localhost:5173"]);
    let app = build_router(state);

    let resp = app
        .oneshot(
            Request::options("/api/v1/status")
                .header("origin", "http://localhost:5173")
                .header("access-control-request-method", "GET")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    let methods = resp
        .headers()
        .get("access-control-allow-methods")
        .expect("should have allow-methods header")
        .to_str()
        .unwrap()
        .to_string();
    assert!(methods.contains("GET"), "should allow GET");
    assert!(methods.contains("POST"), "should allow POST");
}

#[tokio::test]
async fn cors_includes_max_age_header() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_app_state_with_cors(dir.path(), vec!["http://localhost:5173"]);
    let app = build_router(state);

    let resp = app
        .oneshot(
            Request::options("/api/v1/status")
                .header("origin", "http://localhost:5173")
                .header("access-control-request-method", "GET")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    let max_age = resp
        .headers()
        .get("access-control-max-age")
        .expect("should have max-age header")
        .to_str()
        .unwrap();
    assert_eq!(max_age, "3600");
}

// ── E3-S2: Context Compiler Visualization endpoint tests ──

/// E3-T20: GET /api/v1/ticks/{id}/context returns context breakdown for a tick.
#[tokio::test]
async fn get_tick_context_returns_breakdown() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_app_state(dir.path());

    // Use shared storage so handler reads from the same SQLite connection.
    let compiled_context = exoskeleton_memory::compiler::CompiledContext {
        prompt: "test prompt".into(),
        total_tokens: 1847,
        budget: 4000,
        sections: vec![
            exoskeleton_memory::compiler::SectionResult {
                name: "system".into(),
                allocated: 600,
                used: 320,
                truncated: false,
            },
            exoskeleton_memory::compiler::SectionResult {
                name: "episodic_memory".into(),
                allocated: 520,
                used: 520,
                truncated: true,
            },
        ],
        truncated_sections: vec!["episodic_memory".into()],
    };

    let artifact = exoskeleton_core::Artifact::from_json(
        exoskeleton_core::ArtifactKind::ContextBreakdown,
        &compiled_context,
    )
    .unwrap();
    let storage = state.inspector.storage();
    let artifact_id = storage.artifact_store().put(&artifact).unwrap();

    let tick_record = exoskeleton_core::TickRecord {
        tick_id: exoskeleton_core::TickId::new(),
        tick_number: 1,
        phase: exoskeleton_core::TickPhase::Amend,
        started_at: chrono::Utc::now(),
        completed_at: Some(chrono::Utc::now()),
        snapshot_before: exoskeleton_core::ArtifactId::from_content(b"before-1"),
        snapshot_after: Some(exoskeleton_core::ArtifactId::from_content(b"after-1")),
        thread_contributions: Vec::new(),
        actions_taken: Vec::new(),
        llm_calls: Vec::new(),
        decision_rationale: None,
        context_breakdown_ref: Some(artifact_id),
    };
    let tick_id = tick_record.tick_id;
    storage.tick_store().save(&tick_record).unwrap();

    let app = build_router(state);
    let url = format!("/api/v1/ticks/{tick_id}/context");
    let resp = app
        .oneshot(Request::get(&url).body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["total_tokens"], 1847);
    assert_eq!(json["budget"], 4000);
    assert!(json["sections"].is_array());
    assert_eq!(json["sections"].as_array().unwrap().len(), 2);
    assert_eq!(json["sections"][0]["name"], "system");
    assert_eq!(json["truncated_sections"][0], "episodic_memory");
}

/// E3-T20b: GET /api/v1/ticks/{id}/context returns 404 for nonexistent tick.
#[tokio::test]
async fn get_tick_context_returns_404_for_missing() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_app_state(dir.path());
    let app = build_router(state);

    let resp = app
        .oneshot(
            Request::get("/api/v1/ticks/550e8400-e29b-41d4-a716-446655440000/context")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

/// E3-T20c: GET /api/v1/ticks/{id}/context returns error for invalid UUID.
#[tokio::test]
async fn get_tick_context_returns_error_for_invalid_id() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_app_state(dir.path());
    let app = build_router(state);

    let resp = app
        .oneshot(
            Request::get("/api/v1/ticks/not-a-uuid/context")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    // axum returns 400 when the Path extractor fails to parse the UUID.
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "invalid UUID should produce 400, got {}",
        resp.status()
    );
}

#[tokio::test]
async fn router_includes_all_d2_routes() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_app_state(dir.path());

    // Test each D2 route returns non-404 (meaning the route exists)
    let routes = vec![
        "/api/v1/memory",
        "/api/v1/snapshots",
        "/api/v1/inbox/history",
        "/api/v1/config",
    ];

    for route in routes {
        let app = build_router(state.clone());
        let resp = app
            .oneshot(Request::get(route).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_ne!(
            resp.status(),
            StatusCode::NOT_FOUND,
            "route {route} should exist"
        );
    }
}

/// Verify all parameterized routes accept dynamic path segments and reach
/// handlers (not the SPA fallback or axum's default 404). Uses a
/// well-formed but nonexistent UUID so handlers produce 404 from their
/// own lookup logic — distinguishable from route-miss only if positive
/// tests (above) also pass.
#[tokio::test]
async fn parameterized_routes_reach_handlers() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_app_state(dir.path());

    // A valid UUID that doesn't exist in any store.
    let fake_uuid = "00000000-0000-0000-0000-000000000099";
    // A valid-looking artifact hash that doesn't exist.
    let fake_hash = "0000000000000000000000000000000000000000000000000000000000000099";

    let ticks = format!("/api/v1/ticks/{fake_uuid}");
    let ticks_ctx = format!("/api/v1/ticks/{fake_uuid}/context");
    let rels = format!("/api/v1/relationships/{fake_uuid}");
    let artifacts = format!("/api/v1/artifacts/{fake_hash}");
    let convos = format!("/api/v1/conversations/{fake_uuid}");
    let convo_msgs = format!("/api/v1/conversations/{fake_uuid}/messages");

    let parameterized_routes: Vec<(&str, &str)> = vec![
        ("GET /api/v1/ticks/:id", &ticks),
        ("GET /api/v1/ticks/:id/context", &ticks_ctx),
        ("GET /api/v1/relationships/:principal_id", &rels),
        ("GET /api/v1/artifacts/:id", &artifacts),
        (
            "GET /api/v1/snapshots/at/:tick",
            "/api/v1/snapshots/at/99999",
        ),
        ("GET /api/v1/conversations/:id", &convos),
        ("GET /api/v1/conversations/:id/messages", &convo_msgs),
    ];

    for (name, url) in &parameterized_routes {
        let app = build_router(state.clone());
        let resp = app
            .oneshot(Request::get(*url).body(Body::empty()).unwrap())
            .await
            .unwrap();

        // Handlers return 200 (empty result), 400, or 404 for missing data.
        // A route-miss also produces 404 — but the positive companion tests
        // above catch that case. This test verifies the route pattern accepts
        // dynamic segments without crashing.
        assert!(
            !resp.status().is_server_error(),
            "route {name} at {url} should not produce server error (got {})",
            resp.status()
        );
    }
}

// ── E4S4-T6: acknowledge_endpoint_marks_event ──
#[tokio::test]
async fn acknowledge_endpoint_marks_event() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_app_state(dir.path());

    // Create a CapabilityRequest event
    let event_id = LedgerEntryId::new();
    let event = EventEntry {
        id: event_id,
        tick_id: None,
        event_type: EventType::CapabilityRequest,
        payload_ref: None,
        summary: "test cap request".into(),
        timestamp: chrono::Utc::now(),
    };
    state
        .inspector
        .storage()
        .event_ledger()
        .append(&event)
        .unwrap();

    let app = build_router(state.clone());
    let resp = app
        .oneshot(
            Request::post(format!("/api/v1/events/{event_id}/acknowledge"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    assert!(state.acknowledged_events.contains(&event_id));
}

// ── E4S4-T7: acknowledge_endpoint_not_found ──
#[tokio::test]
async fn acknowledge_endpoint_not_found() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_app_state(dir.path());
    let app = build_router(state);

    let fake_id = LedgerEntryId::new();
    let resp = app
        .oneshot(
            Request::post(format!("/api/v1/events/{fake_id}/acknowledge"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// ── E4S4-T8: capability_requests_endpoint_returns_events ──
#[tokio::test]
async fn capability_requests_endpoint_returns_events() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_app_state(dir.path());

    // Create a CapabilityRequest event with payload
    let payload = CapabilityRequestPayload {
        capability: "discord".into(),
        reason: "trust too low".into(),
        context: "need to send alert".into(),
        acknowledged: false,
    };
    let payload_json = serde_json::to_vec(&payload).unwrap();
    let artifact = exoskeleton_core::Artifact::new(
        exoskeleton_core::ArtifactKind::Event,
        payload_json,
        "application/json".into(),
    );
    let artifact_id = state
        .inspector
        .storage()
        .artifact_store()
        .put(&artifact)
        .unwrap();

    let event = EventEntry {
        id: LedgerEntryId::new(),
        tick_id: None,
        event_type: EventType::CapabilityRequest,
        payload_ref: Some(artifact_id),
        summary: "cap request discord".into(),
        timestamp: chrono::Utc::now(),
    };
    state
        .inspector
        .storage()
        .event_ledger()
        .append(&event)
        .unwrap();

    let app = build_router(state);
    let resp = app
        .oneshot(
            Request::get("/api/v1/capability-requests")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    let json: Vec<serde_json::Value> = serde_json::from_str(&body).unwrap();
    assert_eq!(json.len(), 1);
    assert_eq!(json[0]["capability"], "discord");
    assert_eq!(json[0]["acknowledged"], false);
}

// ── E4S4-T9: generic_webhook_injects_message ──
#[tokio::test]
async fn generic_webhook_injects_message() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_app_state(dir.path());

    let app = build_router(state.clone());
    let resp = app
        .oneshot(
            Request::post("/api/v1/webhooks/generic")
                .header("content-type", "application/json")
                .header("x-webhook-source", "monitoring-alerts")
                .body(Body::from(r#"{"alert": "CPU high"}"#))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::CREATED);

    let envelopes = state.inbox.receive().unwrap();
    assert_eq!(envelopes.len(), 1);
    assert_eq!(envelopes[0].kind, EnvelopeKind::HumanMessage);
}

// ── E4S4-T10: generic_webhook_hmac_valid ──
#[tokio::test]
async fn generic_webhook_hmac_valid() {
    let dir = tempfile::tempdir().unwrap();
    let storage = StorageManager::open(dir.path()).unwrap();
    let thread_store = Arc::new(InMemoryThreadStore::new());
    let thread_registry = Arc::new(ThreadRegistry::new(thread_store.clone()));
    let relationship_ledger: Arc<dyn exoskeleton_relationship::RelationshipLedger> =
        Arc::new(InMemoryRelationshipLedger::new());
    let wi_host_slot = Arc::new(tokio::sync::Mutex::new(None));
    let inbox: Arc<dyn exoskeleton_core::inbox::Inbox> =
        Arc::new(exoskeleton_host::InMemoryInbox::new());
    let config = exoskeleton_host::VesselConfig {
        mission: "test".into(),
        ..Default::default()
    };
    let inspector = VesselInspector::new(
        storage,
        thread_registry.clone(),
        relationship_ledger,
        None,
        None,
        wi_host_slot,
        config,
    );
    let metrics = Arc::new(ExoMetrics::new().unwrap());

    let secret = b"mysecret";
    let body_bytes = b"{\"event\": \"deploy\"}";

    // Compute HMAC
    use hmac::Mac;
    let mut mac = hmac::Hmac::<sha2::Sha256>::new_from_slice(secret).unwrap();
    mac.update(body_bytes);
    let signature = hex::encode(mac.finalize().into_bytes());

    let mut secrets = std::collections::HashMap::new();
    secrets.insert("ci-deploy".to_string(), secret.to_vec());

    let state = Arc::new(AppState {
        inspector,
        metrics,
        inbox,
        vessel_id: VesselId::new(),
        event_tx: tokio::sync::broadcast::channel(16).0,
        cors_origins: vec![],
        acknowledged_events: Arc::new(dashmap::DashSet::new()),
        webhook_secrets: secrets,
        charter_proposal_statuses: Arc::new(dashmap::DashMap::new()),
        watch_store: Arc::new(InMemoryWatchStore::new()),
        thread_registry,
        wi_host_slot: Arc::new(tokio::sync::Mutex::new(None)),
        connectors_dir: None,
        align_config: None,
        cognitive_engine: Arc::new(tokio::sync::Mutex::new(None)),
        vessel_mode: Arc::new(std::sync::Mutex::new(VesselMode::Normal)),
        questions_dir: dir.path().join("questions"),
        answers_dir: dir.path().join("answers"),
    });

    let app = build_router(state);
    let resp = app
        .oneshot(
            Request::post("/api/v1/webhooks/generic")
                .header("content-type", "application/json")
                .header("x-webhook-source", "ci-deploy")
                .header("x-webhook-signature", format!("sha256={signature}"))
                .body(Body::from(&body_bytes[..]))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::CREATED);
}

// ── E4S4-T11: generic_webhook_hmac_invalid ──
#[tokio::test]
async fn generic_webhook_hmac_invalid() {
    let dir = tempfile::tempdir().unwrap();
    let storage = StorageManager::open(dir.path()).unwrap();
    let thread_store = Arc::new(InMemoryThreadStore::new());
    let thread_registry = Arc::new(ThreadRegistry::new(thread_store.clone()));
    let relationship_ledger: Arc<dyn exoskeleton_relationship::RelationshipLedger> =
        Arc::new(InMemoryRelationshipLedger::new());
    let wi_host_slot = Arc::new(tokio::sync::Mutex::new(None));
    let inbox: Arc<dyn exoskeleton_core::inbox::Inbox> =
        Arc::new(exoskeleton_host::InMemoryInbox::new());
    let config = exoskeleton_host::VesselConfig {
        mission: "test".into(),
        ..Default::default()
    };
    let inspector = VesselInspector::new(
        storage,
        thread_registry.clone(),
        relationship_ledger,
        None,
        None,
        wi_host_slot,
        config,
    );
    let metrics = Arc::new(ExoMetrics::new().unwrap());

    let mut secrets = std::collections::HashMap::new();
    secrets.insert("ci-deploy".to_string(), b"mysecret".to_vec());

    let state = Arc::new(AppState {
        inspector,
        metrics,
        inbox,
        vessel_id: VesselId::new(),
        event_tx: tokio::sync::broadcast::channel(16).0,
        cors_origins: vec![],
        acknowledged_events: Arc::new(dashmap::DashSet::new()),
        webhook_secrets: secrets,
        charter_proposal_statuses: Arc::new(dashmap::DashMap::new()),
        watch_store: Arc::new(InMemoryWatchStore::new()),
        thread_registry,
        wi_host_slot: Arc::new(tokio::sync::Mutex::new(None)),
        connectors_dir: None,
        align_config: None,
        cognitive_engine: Arc::new(tokio::sync::Mutex::new(None)),
        vessel_mode: Arc::new(std::sync::Mutex::new(VesselMode::Normal)),
        questions_dir: dir.path().join("questions"),
        answers_dir: dir.path().join("answers"),
    });

    let app = build_router(state);
    let resp = app
        .oneshot(
            Request::post("/api/v1/webhooks/generic")
                .header("content-type", "application/json")
                .header("x-webhook-source", "ci-deploy")
                .header("x-webhook-signature", "sha256=badhex")
                .body(Body::from(r#"{"event": "deploy"}"#))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

// ── E4S4-T12: generic_webhook_no_hmac_when_no_secret ──
#[tokio::test]
async fn generic_webhook_no_hmac_when_no_secret() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_app_state(dir.path());
    let app = build_router(state);

    let resp = app
        .oneshot(
            Request::post("/api/v1/webhooks/generic")
                .header("content-type", "application/json")
                .header("x-webhook-source", "unsecured-source")
                .body(Body::from(r#"{"data": "test"}"#))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::CREATED);
}

// ── E4S4-T13: generic_webhook_derives_principal ──
#[tokio::test]
async fn generic_webhook_derives_principal() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_app_state(dir.path());
    let app = build_router(state.clone());

    let resp = app
        .oneshot(
            Request::post("/api/v1/webhooks/generic")
                .header("content-type", "application/json")
                .header("x-webhook-source", "monitoring-alerts")
                .body(Body::from(r#"{"alert": "test"}"#))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::CREATED);

    let envelopes = state.inbox.receive().unwrap();
    assert_eq!(envelopes.len(), 1);
    let expected = derive_external_principal_id("webhook:monitoring-alerts");
    assert_eq!(envelopes[0].source, expected);
}

// ── E4S4-T14: generic_webhook_missing_source ──
#[tokio::test]
async fn generic_webhook_missing_source() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_app_state(dir.path());
    let app = build_router(state);

    let resp = app
        .oneshot(
            Request::post("/api/v1/webhooks/generic")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"data": "test"}"#))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

// ══════════════════════════════════════════════════════════════════════════════
// Sprint E5-S3: Connector Hot-Loading Endpoint Tests
// ══════════════════════════════════════════════════════════════════════════════

// ── E5S3-T10: daemon_load_connector_endpoint ──
#[tokio::test]
async fn daemon_load_connector_endpoint() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_app_state(dir.path());
    let app = build_router(state);

    // WI host slot is None, so this should return 503 (Service Unavailable)
    let body = serde_json::json!({"name": "test.connector"});
    let resp = app
        .oneshot(
            Request::post("/api/v1/connectors/load")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_string(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    let text = body_string(resp.into_body()).await;
    assert!(
        text.contains("WI host not started") || text.contains("connectors directory"),
        "expected WI host or connectors_dir error, got: {text}"
    );
}

// ── E5S3-T11: daemon_unload_connector_endpoint ──
#[tokio::test]
async fn daemon_unload_connector_endpoint() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_app_state(dir.path());
    let app = build_router(state);

    // WI host slot is None, so DELETE should return 503
    let resp = app
        .oneshot(
            Request::delete("/api/v1/connectors/nonexistent")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
}

// ── E5S3-T12: daemon_list_connectors_endpoint ──
#[tokio::test]
async fn daemon_list_connectors_endpoint() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_app_state(dir.path());
    let app = build_router(state);

    // With no WI host, should return an empty JSON array
    let resp = app
        .oneshot(
            Request::get("/api/v1/connectors")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    let json: Vec<serde_json::Value> = serde_json::from_str(&body).unwrap();
    assert!(
        json.is_empty(),
        "expected empty array when no WI host, got: {json:?}"
    );
}

// ── E5S3-T13: daemon_rescan_endpoint ──
#[tokio::test]
async fn daemon_rescan_endpoint() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_app_state(dir.path());
    let app = build_router(state);

    // WI host slot is None, so rescan should return 503
    let resp = app
        .oneshot(
            Request::post("/api/v1/connectors/rescan")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
}

// ── E5S3-T14: capability_approval_triggers_load ──
#[tokio::test]
async fn capability_approval_triggers_load() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_app_state(dir.path());

    // Create a CapabilityRequest event with payload containing a capability name
    let payload = CapabilityRequestPayload {
        capability: "test.connector".into(),
        reason: "need access".into(),
        context: "integration test".into(),
        acknowledged: false,
    };
    let payload_json = serde_json::to_vec(&payload).unwrap();
    let artifact = exoskeleton_core::Artifact::new(
        exoskeleton_core::ArtifactKind::Event,
        payload_json,
        "application/json".into(),
    );
    let artifact_id = state
        .inspector
        .storage()
        .artifact_store()
        .put(&artifact)
        .unwrap();

    let event_id = LedgerEntryId::new();
    let event = EventEntry {
        id: event_id,
        tick_id: None,
        event_type: EventType::CapabilityRequest,
        payload_ref: Some(artifact_id),
        summary: "cap request test.connector".into(),
        timestamp: chrono::Utc::now(),
    };
    state
        .inspector
        .storage()
        .event_ledger()
        .append(&event)
        .unwrap();

    let app = build_router(state.clone());
    let action_body = serde_json::json!({"action": "load_connector"});
    let resp = app
        .oneshot(
            Request::post(format!("/api/v1/events/{event_id}/acknowledge"))
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_string(&action_body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    // No connectors_dir configured, so it should fail with 503
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    // But the event should still have been acknowledged
    assert!(state.acknowledged_events.contains(&event_id));
}

// ── E5S3-T15: connector_loaded_event_emitted ──
#[tokio::test]
async fn connector_loaded_event_emitted() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_app_state(dir.path());

    // Subscribe to broadcast channel before emitting
    let mut rx = state.event_tx.subscribe();

    // Simulate a ConnectorLoaded event by replicating what emit_connector_event does:
    // write to event ledger + broadcast
    let descriptor = worldinterface_core::descriptor::Descriptor {
        name: "test.loaded".into(),
        display_name: "Test Loaded".into(),
        description: "A test connector".into(),
        category: worldinterface_core::descriptor::ConnectorCategory::Custom("test".into()),
        input_schema: None,
        output_schema: None,
        idempotent: true,
        side_effects: false,
        is_read_only: true,
        is_mutating: false,
        is_concurrency_safe: true,
        requires_read_before_write: false,
    };
    let payload = serde_json::to_vec(&descriptor).unwrap();
    let artifact = exoskeleton_core::Artifact::new(
        exoskeleton_core::ArtifactKind::Event,
        payload,
        "application/json".into(),
    );
    let payload_ref = state
        .inspector
        .storage()
        .artifact_store()
        .put(&artifact)
        .ok();
    let event = EventEntry {
        id: LedgerEntryId::new(),
        tick_id: None,
        event_type: EventType::ConnectorLoaded,
        payload_ref,
        summary: "Connector 'test.loaded' loaded".into(),
        timestamp: chrono::Utc::now(),
    };
    state
        .inspector
        .storage()
        .event_ledger()
        .append(&event)
        .unwrap();

    let live_event = exoskeleton_core::LiveEvent {
        event_type: EventType::ConnectorLoaded,
        summary: "Connector 'test.loaded' loaded".into(),
        ..exoskeleton_core::LiveEvent::new(None)
    };
    let _ = state.event_tx.send(live_event);

    // Verify the event is in the ledger
    let events = state.inspector.recent_events(10).unwrap();
    let loaded_events: Vec<_> = events
        .iter()
        .filter(|e| e.event_type == EventType::ConnectorLoaded)
        .collect();
    assert_eq!(loaded_events.len(), 1);
    assert!(loaded_events[0].summary.contains("test.loaded"));

    // Verify the broadcast was received
    let received = rx.try_recv().unwrap();
    assert_eq!(received.event_type, EventType::ConnectorLoaded);
    assert!(received.summary.contains("test.loaded"));
}

// ── E5S3-T16: decide_sees_new_connector ──
#[tokio::test]
async fn decide_sees_new_connector() {
    use worldinterface_connector::registry::RegistryError;
    use worldinterface_connector::traits::Connector;
    use worldinterface_connector::ConnectorRegistry;
    use worldinterface_core::descriptor::{ConnectorCategory, Descriptor};

    struct StubConnector {
        name: String,
    }
    impl Connector for StubConnector {
        fn describe(&self) -> Descriptor {
            Descriptor {
                name: self.name.clone(),
                display_name: self.name.clone(),
                description: "stub".into(),
                category: ConnectorCategory::Custom("test".into()),
                input_schema: None,
                output_schema: None,
                idempotent: true,
                side_effects: false,
                is_read_only: true,
                is_mutating: false,
                is_concurrency_safe: true,
                requires_read_before_write: false,
            }
        }
        fn invoke(
            &self,
            _ctx: &worldinterface_connector::InvocationContext,
            _params: &serde_json::Value,
        ) -> Result<serde_json::Value, worldinterface_connector::ConnectorError> {
            Ok(serde_json::Value::Null)
        }
    }

    let registry = ConnectorRegistry::new();
    assert!(registry.list_capabilities().is_empty());

    // Register via register_runtime (the hot-load path)
    let connector = Arc::new(StubConnector {
        name: "hot.loaded".into(),
    });
    registry.register_runtime(connector).unwrap();

    // Verify it appears in list_capabilities
    let caps = registry.list_capabilities();
    assert_eq!(caps.len(), 1);
    assert_eq!(caps[0].name, "hot.loaded");

    // Duplicate should fail
    let dup = Arc::new(StubConnector {
        name: "hot.loaded".into(),
    });
    let err = registry.register_runtime(dup).unwrap_err();
    assert!(matches!(err, RegistryError::DuplicateConnector(_)));
}

// ── T41: plan_approve_transitions_to_executing ──
#[tokio::test]
async fn plan_approve_transitions_to_executing() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_app_state(dir.path());

    // Set mode to Planning
    *state.vessel_mode.lock().unwrap() = VesselMode::Planning;

    // Store a PlanDraft artifact so the approve endpoint can find it
    let draft = exoskeleton_core::Artifact::from_json(
        exoskeleton_core::ArtifactKind::PlanDraft,
        &serde_json::json!({"objective": "test plan", "tasks": []}),
    )
    .unwrap();
    let draft_id = state
        .inspector
        .storage()
        .artifact_store()
        .put(&draft)
        .unwrap();

    let app = build_router(state.clone());
    let body = serde_json::json!({"plan_draft_id": draft_id});
    let resp = app
        .oneshot(
            Request::post("/api/v1/plan/approve")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        *state.vessel_mode.lock().unwrap(),
        VesselMode::Executing,
        "mode should transition to Executing after plan approval"
    );
}

// ── T42: plan_approve_without_planning_mode_400 ──
#[tokio::test]
async fn plan_approve_without_planning_mode_400() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_app_state(dir.path());

    // Mode is Normal (not Planning)
    assert_eq!(*state.vessel_mode.lock().unwrap(), VesselMode::Normal);

    let app = build_router(state);
    let body = serde_json::json!({
        "plan_draft_id": "abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890"
    });
    let resp = app
        .oneshot(
            Request::post("/api/v1/plan/approve")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

// ── T43: plan_approve_missing_draft_404 ──
#[tokio::test]
async fn plan_approve_missing_draft_404() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_app_state(dir.path());

    // Set mode to Planning
    *state.vessel_mode.lock().unwrap() = VesselMode::Planning;

    let app = build_router(state);
    // Use a valid content-addressed hash that doesn't exist in the store
    let body = serde_json::json!({
        "plan_draft_id": "0000000000000000000000000000000000000000000000000000000000000000"
    });
    let resp = app
        .oneshot(
            Request::post("/api/v1/plan/approve")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// ── T44: plan_cancel_from_planning_transitions_to_normal ──
#[tokio::test]
async fn plan_cancel_from_planning_transitions_to_normal() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_app_state(dir.path());

    *state.vessel_mode.lock().unwrap() = VesselMode::Planning;

    let app = build_router(state.clone());
    let resp = app
        .oneshot(
            Request::post("/api/v1/plan/cancel")
                .header("content-type", "application/json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        *state.vessel_mode.lock().unwrap(),
        VesselMode::Normal,
        "mode should return to Normal after cancel from Planning"
    );
}

// ── T44a: plan_cancel_from_executing_transitions_to_normal ──
#[tokio::test]
async fn plan_cancel_from_executing_transitions_to_normal() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_app_state(dir.path());

    *state.vessel_mode.lock().unwrap() = VesselMode::Executing;

    let app = build_router(state.clone());
    let resp = app
        .oneshot(
            Request::post("/api/v1/plan/cancel")
                .header("content-type", "application/json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        *state.vessel_mode.lock().unwrap(),
        VesselMode::Normal,
        "mode should return to Normal after cancel from Executing"
    );
}

// ── T45: plan_status_returns_current_mode ──
#[tokio::test]
async fn plan_status_returns_current_mode() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_app_state(dir.path());

    // Default mode is Normal
    let app = build_router(state.clone());
    let resp = app
        .oneshot(
            Request::get("/api/v1/plan/status")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = http_body_util::BodyExt::collect(resp.into_body())
        .await
        .unwrap()
        .to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["mode"], "normal");

    // Switch to Planning
    *state.vessel_mode.lock().unwrap() = VesselMode::Planning;
    let app = build_router(state.clone());
    let resp = app
        .oneshot(
            Request::get("/api/v1/plan/status")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = http_body_util::BodyExt::collect(resp.into_body())
        .await
        .unwrap()
        .to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["mode"], "planning");
}

// ── T46: question_answer_writes_file ──
#[tokio::test]
async fn question_answer_writes_file() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_app_state(dir.path());

    // Create questions and answers directories
    let questions_dir = dir.path().join("questions");
    let answers_dir = dir.path().join("answers");
    std::fs::create_dir_all(&questions_dir).unwrap();
    std::fs::create_dir_all(&answers_dir).unwrap();

    // Write a question file
    let question_id = uuid::Uuid::new_v4().to_string();
    let question_path = questions_dir.join(format!("{question_id}.json"));
    std::fs::write(
        &question_path,
        serde_json::to_vec_pretty(&serde_json::json!({
            "question": "What branch?",
            "choices": ["main", "develop"]
        }))
        .unwrap(),
    )
    .unwrap();

    let app = build_router(state);
    let body = serde_json::json!({"answer": "main", "source": "operator"});
    let resp = app
        .oneshot(
            Request::post(format!("/api/v1/questions/{question_id}/answer"))
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);

    // Verify answer file was written
    let answer_path = answers_dir.join(format!("{question_id}.json"));
    assert!(answer_path.exists(), "answer file must be written");
    let answer: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&answer_path).unwrap()).unwrap();
    assert_eq!(answer["answer"], "main");
    assert_eq!(answer["answered_by"], "operator");
}

// ── T47: question_answer_nonexistent_404 ──
#[tokio::test]
async fn question_answer_nonexistent_404() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_app_state(dir.path());

    // Create questions directory but don't put any question file
    std::fs::create_dir_all(dir.path().join("questions")).unwrap();
    std::fs::create_dir_all(dir.path().join("answers")).unwrap();

    let question_id = uuid::Uuid::new_v4().to_string();
    let app = build_router(state);
    let body = serde_json::json!({"answer": "test", "source": "operator"});
    let resp = app
        .oneshot(
            Request::post(format!("/api/v1/questions/{question_id}/answer"))
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

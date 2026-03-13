//! Unit tests for daemon HTTP handlers (T-6).
//!
//! Uses tower::ServiceExt::oneshot to test the axum router directly
//! without starting a TCP server.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use exoskeleton_core::inbox::Inbox;
use exoskeleton_core::{
    EventEntry, EventLedger, EventType, LedgerEntryId, SnapshotStore, StateSnapshot, VesselId,
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
    let thread_registry = Arc::new(ThreadRegistry::new(thread_store));
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
        thread_registry,
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

    // Save a snapshot
    let snap = StateSnapshot::initial(VesselId::new(), "test mission".into());
    // Access storage through a fresh StorageManager (since inspector holds its own)
    let storage = StorageManager::open(dir.path()).unwrap();
    storage.snapshot_store().save(&snap).unwrap();

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

    // Append an event directly to the store
    let storage = StorageManager::open(dir.path()).unwrap();
    let event = EventEntry {
        id: LedgerEntryId::new(),
        tick_id: None,
        event_type: EventType::VesselStarted,
        payload_ref: None,
        summary: "test event".into(),
        timestamp: chrono::Utc::now(),
    };
    storage.event_ledger().append(&event).unwrap();

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

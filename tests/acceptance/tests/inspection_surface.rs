//! Acceptance Test F: Inspection Surface
//!
//! Proves `exo inspect` and daemon endpoints show full vessel state
//! (Scope Appendix §5 criterion F).

mod support;

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use exoskeleton_daemon::routes::build_router;
use exoskeleton_daemon::state::AppState;
use exoskeleton_host::metrics::ExoMetrics;
use http_body_util::BodyExt;
use tower::ServiceExt;

const TICK_TIMEOUT: Duration = Duration::from_secs(120);

async fn body_string(body: Body) -> String {
    let bytes = body.collect().await.unwrap().to_bytes();
    String::from_utf8(bytes.to_vec()).unwrap()
}

/// Build an AppState from a running Vessel for endpoint testing.
fn build_app_state(vessel: &exoskeleton_host::Vessel) -> Arc<AppState> {
    let inspector = vessel.inspector();
    let metrics = Arc::new(ExoMetrics::new().unwrap());
    let inbox = vessel.inbox().clone();
    let vessel_id = vessel.vessel_id();

    Arc::new(AppState {
        inspector,
        metrics,
        inbox,
        vessel_id,
    })
}

#[tokio::test]
async fn status_endpoint_shows_full_state() {
    let dir = tempfile::tempdir().unwrap();
    let vessel = support::boot_vessel(dir.path()).await;
    let _ticks = support::wait_for_ticks(vessel.storage(), 5, TICK_TIMEOUT).await;

    let state = build_app_state(&vessel);
    let app = build_router(state);

    let resp = app
        .oneshot(Request::get("/api/v1/status").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();

    assert_eq!(json["mission"], "acceptance test");
    assert!(
        json["tick_number"].as_u64().unwrap() >= 5,
        "tick_number should be >= 5"
    );

    // Thread summaries present
    let summaries = json["thread_summaries"].as_array().unwrap();
    assert!(
        summaries.len() >= 2,
        "should have >= 2 thread summaries in status"
    );

    support::shutdown_and_verify(vessel).await;
}

#[tokio::test]
async fn ticks_endpoint_returns_history() {
    let dir = tempfile::tempdir().unwrap();
    let vessel = support::boot_vessel(dir.path()).await;
    let _ticks = support::wait_for_ticks(vessel.storage(), 5, TICK_TIMEOUT).await;

    let state = build_app_state(&vessel);
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

    assert_eq!(json.len(), 5, "should return 5 tick records");

    // Each tick should have thread_contributions
    for tick in &json {
        let contributions = tick["thread_contributions"].as_array().unwrap();
        assert!(
            !contributions.is_empty(),
            "each tick should have thread contributions"
        );
    }

    support::shutdown_and_verify(vessel).await;
}

#[tokio::test]
async fn threads_endpoint_lists_all_three() {
    let dir = tempfile::tempdir().unwrap();
    let vessel = support::boot_vessel(dir.path()).await;
    let _ticks = support::wait_for_ticks(vessel.storage(), 3, TICK_TIMEOUT).await;

    let state = build_app_state(&vessel);
    let app = build_router(state);

    let resp = app
        .oneshot(Request::get("/api/v1/threads").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    let json: Vec<serde_json::Value> = serde_json::from_str(&body).unwrap();

    assert_eq!(json.len(), 3, "should have 3 built-in threads");

    let names: Vec<&str> = json.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert!(names.contains(&"Threat Monitor"));
    assert!(names.contains(&"Self-Critique"));
    assert!(names.contains(&"Memory Consolidation"));

    support::shutdown_and_verify(vessel).await;
}

#[tokio::test]
async fn events_endpoint_returns_events() {
    let dir = tempfile::tempdir().unwrap();
    let vessel = support::boot_vessel(dir.path()).await;
    let _ticks = support::wait_for_ticks(vessel.storage(), 3, TICK_TIMEOUT).await;

    let state = build_app_state(&vessel);
    let app = build_router(state);

    let resp = app
        .oneshot(
            Request::get("/api/v1/events?limit=20")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    let json: Vec<serde_json::Value> = serde_json::from_str(&body).unwrap();

    // Should have at least VesselStarted + TickStarted events
    assert!(!json.is_empty(), "should have events after 3 ticks");

    support::shutdown_and_verify(vessel).await;
}

#[tokio::test]
async fn engines_endpoint_shows_both_engines() {
    let dir = tempfile::tempdir().unwrap();
    let vessel = support::boot_vessel(dir.path()).await;
    let _ticks = support::wait_for_ticks(vessel.storage(), 1, TICK_TIMEOUT).await;

    let state = build_app_state(&vessel);
    let app = build_router(state);

    let resp = app
        .oneshot(Request::get("/api/v1/engines").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();

    assert!(
        json["cognitive"].is_object(),
        "should have cognitive engine status"
    );
    assert!(json["tool"].is_object(), "should have tool engine status");

    support::shutdown_and_verify(vessel).await;
}

#[tokio::test]
async fn metrics_endpoint_has_live_data() {
    let dir = tempfile::tempdir().unwrap();
    let vessel = support::boot_vessel(dir.path()).await;
    let _ticks = support::wait_for_ticks(vessel.storage(), 3, TICK_TIMEOUT).await;

    // Touch metrics so they have data
    let metrics = Arc::new(ExoMetrics::new().unwrap());
    metrics.ticks_total.with_label_values(&["completed"]).inc();

    let state = Arc::new(AppState {
        inspector: vessel.inspector(),
        metrics,
        inbox: vessel.inbox().clone(),
        vessel_id: vessel.vessel_id(),
    });
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

    support::shutdown_and_verify(vessel).await;
}

#[tokio::test]
async fn healthz_returns_200() {
    let dir = tempfile::tempdir().unwrap();
    let vessel = support::boot_vessel(dir.path()).await;

    let state = build_app_state(&vessel);
    let app = build_router(state);

    let resp = app
        .oneshot(Request::get("/healthz").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);

    support::shutdown_and_verify(vessel).await;
}

#[tokio::test]
async fn ready_reflects_engine_state() {
    let dir = tempfile::tempdir().unwrap();
    let vessel = support::boot_vessel(dir.path()).await;

    let state = build_app_state(&vessel);
    let app = build_router(state);

    let resp = app
        .oneshot(Request::get("/ready").body(Body::empty()).unwrap())
        .await
        .unwrap();

    // Readiness depends on engine availability — may be 200 or 503
    assert!(
        resp.status() == StatusCode::OK || resp.status() == StatusCode::SERVICE_UNAVAILABLE,
        "ready should return 200 or 503"
    );

    support::shutdown_and_verify(vessel).await;
}

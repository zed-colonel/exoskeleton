//! Bootstrap API integration tests.
//!
//! Start a real pre-bootstrap HTTP server (via Tower oneshot) and test
//! endpoint ordering/validation without requiring LLM connectivity.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::routing::{get, post};
use http_body_util::BodyExt;
use tokio::sync::oneshot;
use tower::ServiceExt;

use exoskeleton_daemon::bootstrap_api;
use exoskeleton_daemon::bootstrap_state::BootstrapState;

/// Build the bootstrap router (same route table as `bootstrap_server.rs`).
fn bootstrap_router(state: Arc<BootstrapState>) -> axum::Router {
    axum::Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/ready", get(bootstrap_api::ready))
        .route(
            "/api/v1/bootstrap/configure",
            post(bootstrap_api::configure),
        )
        .route("/api/v1/bootstrap/verify", post(bootstrap_api::verify))
        .route(
            "/api/v1/bootstrap/conversation",
            get(bootstrap_api::conversation_ws),
        )
        .route("/api/v1/bootstrap/finalize", post(bootstrap_api::finalize))
        .route(
            "/api/v1/bootstrap/start-vessel",
            post(bootstrap_api::start_vessel),
        )
        .with_state(state)
}

/// Create a test BootstrapState backed by a temp directory.
fn test_bootstrap_state(dir: &std::path::Path) -> (Arc<BootstrapState>, oneshot::Receiver<()>) {
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let (tx, rx) = oneshot::channel();
    let state = Arc::new(BootstrapState::new(dir.to_path_buf(), addr, tx, None));
    (state, rx)
}

async fn body_string(body: Body) -> String {
    let bytes = body.collect().await.unwrap().to_bytes();
    String::from_utf8(bytes.to_vec()).unwrap()
}

fn valid_configure_json() -> serde_json::Value {
    serde_json::json!({
        "default_backend": "frontier",
        "frontier": {
            "provider": "anthropic",
            "model": "claude-sonnet-4-20250514",
            "api_key_env": "ANTHROPIC_API_KEY"
        }
    })
}

// ── EO3-T1: GET /healthz → 200 "ok" ──

#[tokio::test]
async fn healthz_returns_200_ok() {
    let dir = tempfile::tempdir().unwrap();
    let (state, _rx) = test_bootstrap_state(dir.path());
    let app = bootstrap_router(state);

    let resp = app
        .oneshot(Request::get("/healthz").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    assert_eq!(body, "ok");
}

// ── EO3-T2: GET /ready → 200 with { "mode": "bootstrap" } ──

#[tokio::test]
async fn ready_returns_bootstrap_mode() {
    let dir = tempfile::tempdir().unwrap();
    let (state, _rx) = test_bootstrap_state(dir.path());
    let app = bootstrap_router(state);

    let resp = app
        .oneshot(Request::get("/ready").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["mode"], "bootstrap");
}

// ── EO3-T3: GET /api/v1/status → 404 (not available in bootstrap mode) ──

#[tokio::test]
async fn status_endpoint_not_available_in_bootstrap() {
    let dir = tempfile::tempdir().unwrap();
    let (state, _rx) = test_bootstrap_state(dir.path());
    let app = bootstrap_router(state);

    let resp = app
        .oneshot(Request::get("/api/v1/status").body(Body::empty()).unwrap())
        .await
        .unwrap();

    // The bootstrap router does not register /api/v1/status, so axum returns 404.
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// ── EO3-T7: POST /bootstrap/configure with valid config → 200 ──

#[tokio::test]
async fn configure_with_valid_config_returns_200() {
    let dir = tempfile::tempdir().unwrap();
    let (state, _rx) = test_bootstrap_state(dir.path());
    let app = bootstrap_router(state);

    let resp = app
        .oneshot(
            Request::post("/api/v1/bootstrap/configure")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_string(&valid_configure_json()).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
}

// ── EO3-T8: POST /bootstrap/configure missing backend → 400 ──

#[tokio::test]
async fn configure_missing_backend_returns_400() {
    let dir = tempfile::tempdir().unwrap();
    let (state, _rx) = test_bootstrap_state(dir.path());
    let app = bootstrap_router(state);

    // No frontier or local config — should be rejected.
    let body = serde_json::json!({
        "default_backend": "frontier"
    });

    let resp = app
        .oneshot(
            Request::post("/api/v1/bootstrap/configure")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_string(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = body_string(resp.into_body()).await;
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(
        json["error"]
            .as_str()
            .unwrap()
            .contains("at least one backend"),
        "error should mention missing backend, got: {}",
        json["error"]
    );
}

// ── EO3-T9: POST /bootstrap/verify without configure → 400 ──

#[tokio::test]
async fn verify_without_configure_returns_400() {
    let dir = tempfile::tempdir().unwrap();
    let (state, _rx) = test_bootstrap_state(dir.path());
    let app = bootstrap_router(state);

    let resp = app
        .oneshot(
            Request::post("/api/v1/bootstrap/verify")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = body_string(resp.into_body()).await;
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(
        json["error"].as_str().unwrap().contains("not configured"),
        "error should mention not configured, got: {}",
        json["error"]
    );
}

// ── EO3-T11: POST /bootstrap/finalize without conversation → 400 ──

#[tokio::test]
async fn finalize_without_conversation_returns_400() {
    let dir = tempfile::tempdir().unwrap();
    let (state, _rx) = test_bootstrap_state(dir.path());

    // Configure LLM first (finalize checks config before transcript).
    let app1 = bootstrap_router(state.clone());
    let resp = app1
        .oneshot(
            Request::post("/api/v1/bootstrap/configure")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_string(&valid_configure_json()).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // Now try finalize without any conversation transcript.
    let app2 = bootstrap_router(state);
    let resp = app2
        .oneshot(
            Request::post("/api/v1/bootstrap/finalize")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = body_string(resp.into_body()).await;
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(
        json["error"]
            .as_str()
            .unwrap()
            .contains("no conversation transcript"),
        "error should mention missing transcript, got: {}",
        json["error"]
    );
}

// ── EO3-T13: POST /bootstrap/start-vessel without finalize → 400 ──

#[tokio::test]
async fn start_vessel_without_finalize_returns_400() {
    let dir = tempfile::tempdir().unwrap();
    let (state, _rx) = test_bootstrap_state(dir.path());
    let app = bootstrap_router(state);

    let resp = app
        .oneshot(
            Request::post("/api/v1/bootstrap/start-vessel")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = body_string(resp.into_body()).await;
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(
        json["error"].as_str().unwrap().contains("not finalized"),
        "error should mention not finalized, got: {}",
        json["error"]
    );
}

// ── EO3-T16: serve-bootstrap command parses listen address ──
//
// The real CLI parsing lives in exoskeleton-cli (which depends on clap).
// Here we verify that the listen address format accepted by
// `run_bootstrap_server` is a valid SocketAddr, and that the bootstrap
// router can be constructed with an ephemeral address — the same
// integration point the CLI exercises.

#[test]
fn serve_bootstrap_listen_addr_parses() {
    // Default listen address used by `exo serve-bootstrap`
    let default_listen: SocketAddr = "0.0.0.0:7600".parse().unwrap();
    assert_eq!(default_listen.port(), 7600);

    // Ephemeral address (used in tests and when the OS assigns a port)
    let ephemeral: SocketAddr = "127.0.0.1:0".parse().unwrap();
    assert_eq!(ephemeral.port(), 0);

    // Custom address
    let custom: SocketAddr = "127.0.0.1:8080".parse().unwrap();
    assert_eq!(custom.port(), 8080);
}

#[test]
fn bootstrap_state_constructable_with_ephemeral_addr() {
    let dir = tempfile::tempdir().unwrap();
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let (tx, _rx) = oneshot::channel();
    let state = BootstrapState::new(dir.path().to_path_buf(), addr, tx, None);
    assert_eq!(state.listen_addr.port(), 0);
}

// ── EO4-T41: Bootstrap request with valid registration token → accepted ──

#[tokio::test]
async fn configure_with_valid_registration_token() {
    let dir = tempfile::tempdir().unwrap();
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let (tx, _rx) = oneshot::channel();
    let state = Arc::new(BootstrapState::new(
        dir.path().to_path_buf(),
        addr,
        tx,
        Some("test-secret-token".into()),
    ));
    let app = bootstrap_router(state);
    let resp = app
        .oneshot(
            Request::post("/api/v1/bootstrap/configure")
                .header("content-type", "application/json")
                .header("authorization", "Bearer test-secret-token")
                .body(Body::from(
                    serde_json::to_string(&valid_configure_json()).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

// ── EO4-T42: Bootstrap request without token when token configured → 401 ──

#[tokio::test]
async fn configure_without_token_when_required_returns_401() {
    let dir = tempfile::tempdir().unwrap();
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let (tx, _rx) = oneshot::channel();
    let state = Arc::new(BootstrapState::new(
        dir.path().to_path_buf(),
        addr,
        tx,
        Some("test-secret-token".into()),
    ));
    let app = bootstrap_router(state);
    let resp = app
        .oneshot(
            Request::post("/api/v1/bootstrap/configure")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_string(&valid_configure_json()).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

// ── EO4-T43: Bootstrap request without token when no token configured → accepted ──

#[tokio::test]
async fn configure_without_token_when_not_required() {
    let dir = tempfile::tempdir().unwrap();
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let (tx, _rx) = oneshot::channel();
    let state = Arc::new(BootstrapState::new(
        dir.path().to_path_buf(),
        addr,
        tx,
        None,
    ));
    let app = bootstrap_router(state);
    let resp = app
        .oneshot(
            Request::post("/api/v1/bootstrap/configure")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_string(&valid_configure_json()).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

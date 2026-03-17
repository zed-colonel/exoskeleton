//! Router construction for the Exoskeleton HTTP daemon.

use std::sync::Arc;
use std::time::Duration;

use axum::http::{header, Method};
use axum::routing::{get, post};
use axum::Router;
use tower_http::cors::CorsLayer;

#[cfg(feature = "embedded-observatory")]
use crate::embedded;
use crate::handlers;
use crate::state::AppState;

/// Build the axum router with all routes wired to handlers.
pub fn build_router(state: Arc<AppState>) -> Router {
    let cors = if state.cors_origins.is_empty() {
        CorsLayer::new()
    } else {
        CorsLayer::new()
            .allow_origin(state.cors_origins.clone())
            .allow_methods([Method::GET, Method::POST, Method::OPTIONS])
            .allow_headers([header::CONTENT_TYPE, header::ACCEPT])
            .max_age(Duration::from_secs(3600))
    };

    let router = Router::new()
        // API v1 endpoints
        .route("/api/v1/status", get(handlers::get_status))
        .route("/api/v1/ticks", get(handlers::get_ticks))
        .route("/api/v1/ticks/{id}", get(handlers::get_tick_by_id))
        .route(
            "/api/v1/ticks/{id}/context",
            get(handlers::get_tick_context),
        )
        .route("/api/v1/threads", get(handlers::get_threads))
        .route("/api/v1/relationships", get(handlers::get_relationships))
        .route(
            "/api/v1/relationships/{principal_id}",
            get(handlers::get_relationship_by_principal),
        )
        .route("/api/v1/budget", get(handlers::get_budget))
        .route("/api/v1/events", get(handlers::get_events))
        .route("/api/v1/capabilities", get(handlers::get_capabilities))
        .route("/api/v1/engines", get(handlers::get_engines))
        .route("/api/v1/inbox", post(handlers::post_inbox))
        // Epoch 0: Charter hot-reload
        .route(
            "/api/v1/charters/reload",
            post(handlers::post_reload_charters),
        )
        // D2: New endpoints
        .route("/api/v1/ws", get(handlers::ws_handler))
        .route("/api/v1/artifacts/{id}", get(handlers::get_artifact))
        .route("/api/v1/memory", get(handlers::get_memory))
        .route("/api/v1/snapshots", get(handlers::get_snapshots))
        .route(
            "/api/v1/snapshots/at/{tick}",
            get(handlers::get_snapshot_at_tick),
        )
        .route(
            "/api/v1/snapshots/at/{tick}/fork",
            post(handlers::post_fork_snapshot),
        )
        .route("/api/v1/inbox/history", get(handlers::get_inbox_history))
        .route("/api/v1/config", get(handlers::get_config))
        // Operational endpoints
        .route("/healthz", get(handlers::healthz))
        .route("/ready", get(handlers::readyz))
        .route("/metrics", get(handlers::metrics))
        .with_state(state);

    // Embedded Observatory: serve SPA from compiled-in assets.
    #[cfg(feature = "embedded-observatory")]
    let router = router
        .route("/config.js", get(embedded::serve_config_js))
        .fallback(embedded::serve_embedded_safe);

    router.layer(cors)
}

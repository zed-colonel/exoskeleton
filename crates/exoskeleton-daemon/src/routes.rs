//! Router construction for the Exoskeleton HTTP daemon.

use std::sync::Arc;

use axum::routing::{get, post};
use axum::Router;

use crate::handlers;
use crate::state::AppState;

/// Build the axum router with all routes wired to handlers.
pub fn build_router(state: Arc<AppState>) -> Router {
    Router::new()
        // API v1 endpoints
        .route("/api/v1/status", get(handlers::get_status))
        .route("/api/v1/ticks", get(handlers::get_ticks))
        .route("/api/v1/ticks/{id}", get(handlers::get_tick_by_id))
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
        // Operational endpoints
        .route("/healthz", get(handlers::healthz))
        .route("/ready", get(handlers::readyz))
        .route("/metrics", get(handlers::metrics))
        .with_state(state)
}

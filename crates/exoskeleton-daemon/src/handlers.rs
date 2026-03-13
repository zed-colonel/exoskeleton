//! HTTP request handlers for the Exoskeleton daemon.
//!
//! All GET handlers are read-only queries against the
//! [`VesselInspector`](exoskeleton_host::inspect::VesselInspector).
//! The single POST handler (`post_inbox`) writes a message envelope to the inbox.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use chrono::Utc;
use exoskeleton_core::{
    ArtifactId, EnvelopeId, EnvelopeKind, MessageEnvelope, PrincipalId, TickId,
};
use serde::{Deserialize, Serialize};

use crate::state::AppState;

// ── Query parameter types ──

#[derive(Debug, Deserialize)]
pub struct LimitQuery {
    pub limit: Option<usize>,
}

// ── Request/response types ──

/// Request body for POST /api/v1/inbox.
#[derive(Debug, Deserialize)]
pub struct InboxSubmitRequest {
    pub source: PrincipalId,
    pub content: String,
    pub in_reply_to: Option<EnvelopeId>,
}

/// Response body for POST /api/v1/inbox.
#[derive(Debug, Serialize)]
pub struct InboxSubmitResponse {
    pub envelope_id: EnvelopeId,
}

/// Simplified capability info (avoids requiring wi-core dependency in daemon).
#[derive(Debug, Serialize)]
pub struct CapabilityInfo {
    pub name: String,
    pub description: String,
}

// ── Handlers ──

/// GET /api/v1/status -> StateSnapshot (or 204 No Content)
pub async fn get_status(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    match state.inspector.snapshot() {
        Ok(Some(snap)) => Json(snap).into_response(),
        Ok(None) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// GET /api/v1/ticks with ?limit=N (default 20, max 1000).
pub async fn get_ticks(
    State(state): State<Arc<AppState>>,
    Query(query): Query<LimitQuery>,
) -> impl IntoResponse {
    let limit = query.limit.unwrap_or(20).min(1000);
    match state.inspector.tick_history(limit) {
        Ok(ticks) => Json(ticks).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// GET /api/v1/ticks/:id -> TickRecord or 404
pub async fn get_tick_by_id(
    State(state): State<Arc<AppState>>,
    Path(id_str): Path<String>,
) -> impl IntoResponse {
    let tick_id: TickId = match id_str.parse() {
        Ok(id) => id,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                "invalid tick ID (expected UUID)".to_string(),
            )
                .into_response();
        }
    };
    match state.inspector.tick_detail(tick_id) {
        Ok(Some(record)) => Json(record).into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// GET /api/v1/threads — list all registered threads with status.
pub async fn get_threads(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    match state.inspector.thread_status() {
        Ok(threads) => Json(threads).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// GET /api/v1/relationships -> RelationshipSnapshot
pub async fn get_relationships(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    match state.inspector.relationship_snapshot() {
        Ok(snapshot) => Json(snapshot).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// GET /api/v1/relationships/:principal_id with ?limit=N (default 50).
pub async fn get_relationship_by_principal(
    State(state): State<Arc<AppState>>,
    Path(id_str): Path<String>,
    Query(query): Query<LimitQuery>,
) -> impl IntoResponse {
    let principal_id: PrincipalId = match id_str.parse() {
        Ok(id) => id,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                "invalid principal ID (expected UUID)".to_string(),
            )
                .into_response();
        }
    };
    let limit = query.limit.unwrap_or(50).min(1000);
    match state.inspector.relationship_history(principal_id, limit) {
        Ok(records) => Json(records).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// GET /api/v1/budget -> InspectionBudgetStatus
pub async fn get_budget(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    match state.inspector.budget_status().await {
        Ok(status) => Json(status).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// GET /api/v1/events with ?limit=N (default 50).
pub async fn get_events(
    State(state): State<Arc<AppState>>,
    Query(query): Query<LimitQuery>,
) -> impl IntoResponse {
    let limit = query.limit.unwrap_or(50).min(1000);
    match state.inspector.recent_events(limit) {
        Ok(events) => Json(events).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// GET /api/v1/capabilities — list available WI connectors.
pub async fn get_capabilities(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let descriptors = state.inspector.capabilities();
    let infos: Vec<CapabilityInfo> = descriptors
        .into_iter()
        .map(|d| CapabilityInfo {
            name: d.name,
            description: d.description,
        })
        .collect();
    Json(infos).into_response()
}

/// GET /api/v1/engines -> EngineStatus
pub async fn get_engines(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let status = state.inspector.engine_status().await;
    Json(status).into_response()
}

/// POST /api/v1/inbox -> 201 Created with EnvelopeId
pub async fn post_inbox(
    State(state): State<Arc<AppState>>,
    Json(req): Json<InboxSubmitRequest>,
) -> impl IntoResponse {
    let envelope_id = EnvelopeId::new();
    let payload_ref = ArtifactId::from_content(req.content.as_bytes());

    let envelope = MessageEnvelope {
        id: envelope_id,
        source: req.source,
        target: None,
        kind: EnvelopeKind::HumanMessage,
        payload_ref,
        timestamp: Utc::now(),
        in_reply_to: req.in_reply_to,
    };

    match state.inbox.submit(&envelope) {
        Ok(()) => (
            StatusCode::CREATED,
            Json(InboxSubmitResponse { envelope_id }),
        )
            .into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// GET /healthz -> 200 OK always
pub async fn healthz() -> impl IntoResponse {
    StatusCode::OK
}

/// GET /ready -> 200 if both engines available, 503 otherwise
pub async fn readyz(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let status = state.inspector.engine_status().await;
    if status.cognitive.available && status.tool.available {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    }
}

/// GET /metrics -> Prometheus text format
pub async fn metrics(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    match state.metrics.render() {
        Ok(text) => (
            StatusCode::OK,
            [("content-type", "text/plain; version=0.0.4; charset=utf-8")],
            text,
        )
            .into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

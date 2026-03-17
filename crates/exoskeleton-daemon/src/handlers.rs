//! HTTP request handlers for the Exoskeleton daemon.
//!
//! All GET handlers are read-only queries against the
//! [`VesselInspector`](exoskeleton_host::inspect::VesselInspector).
//! The single POST handler (`post_inbox`) writes a message envelope to the inbox.

use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use chrono::Utc;
use exoskeleton_core::{
    ArtifactId, EnvelopeId, EnvelopeKind, LlmBackend, MessageEnvelope, PrincipalId, TickId,
};
use exoskeleton_host::config::LocalApiFormat;
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

// ── D2: New handlers ──

/// GET /api/v1/ws -> WebSocket upgrade for real-time event streaming.
pub async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    ws.on_upgrade(|socket| handle_ws_connection(socket, state))
}

async fn handle_ws_connection(mut socket: WebSocket, state: Arc<AppState>) {
    let mut rx = state.event_tx.subscribe();

    loop {
        tokio::select! {
            event = rx.recv() => {
                match event {
                    Ok(live_event) => {
                        let json = serde_json::to_string(&live_event).unwrap_or_default();
                        if socket.send(Message::Text(json)).await.is_err() {
                            break; // Client disconnected
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        let warning = serde_json::json!({
                            "type": "warning",
                            "message": format!("dropped {n} events (slow consumer)"),
                        });
                        let _ = socket.send(Message::Text(warning.to_string())).await;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        break; // Channel closed (vessel shutting down)
                    }
                }
            }
            msg = socket.recv() => {
                match msg {
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(Message::Ping(data))) => {
                        let _ = socket.send(Message::Pong(data)).await;
                    }
                    _ => {} // Ignore other client messages for now
                }
            }
        }
    }
}

/// GET /api/v1/artifacts/:id -> Artifact content (200) or 404.
pub async fn get_artifact(
    State(state): State<Arc<AppState>>,
    Path(id_str): Path<String>,
) -> impl IntoResponse {
    let artifact_id: ArtifactId = match id_str.parse() {
        Ok(id) => id,
        Err(_) => return StatusCode::BAD_REQUEST.into_response(),
    };
    match state.inspector.artifact(&artifact_id) {
        Ok(Some(artifact)) => {
            let is_text = artifact.content_type.starts_with("text/")
                || artifact.content_type == "application/json";
            let content = if is_text {
                String::from_utf8_lossy(&artifact.content).into_owned()
            } else {
                use base64::Engine;
                base64::engine::general_purpose::STANDARD.encode(&artifact.content)
            };
            let resp = ArtifactResponse {
                id: artifact.id.to_string(),
                kind: format!("{:?}", artifact.kind),
                content_type: artifact.content_type.clone(),
                content,
                created_at: artifact.created_at,
            };
            Json(resp).into_response()
        }
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

#[derive(Serialize)]
struct ArtifactResponse {
    id: String,
    kind: String,
    content_type: String,
    content: String,
    created_at: chrono::DateTime<Utc>,
}

/// GET /api/v1/memory with ?type=episodic|long_term&limit=N.
pub async fn get_memory(
    State(state): State<Arc<AppState>>,
    Query(query): Query<MemoryQuery>,
) -> impl IntoResponse {
    let limit = query.limit.unwrap_or(50).min(1000);
    let mut response = MemoryResponse {
        episodic: None,
        long_term: None,
    };

    let want_episodic = query.memory_type.as_deref() != Some("long_term");
    let want_long_term = query.memory_type.as_deref() != Some("episodic");

    if want_episodic {
        match state.inspector.memory_episodic(limit) {
            Ok(items) => response.episodic = Some(items),
            Err(e) => {
                return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
            }
        }
    }
    if want_long_term {
        match state.inspector.memory_long_term(limit) {
            Ok(items) => response.long_term = Some(items),
            Err(e) => {
                return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
            }
        }
    }

    Json(response).into_response()
}

#[derive(Deserialize)]
pub struct MemoryQuery {
    #[serde(rename = "type")]
    memory_type: Option<String>,
    limit: Option<usize>,
}

#[derive(Serialize, Deserialize, ts_rs::TS)]
pub struct MemoryResponse {
    #[serde(skip_serializing_if = "Option::is_none")]
    episodic: Option<Vec<exoskeleton_core::EpisodicSummary>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    long_term: Option<Vec<exoskeleton_core::LongTermNote>>,
}

/// GET /api/v1/snapshots with ?limit=N (default 20, max 1000).
pub async fn get_snapshots(
    State(state): State<Arc<AppState>>,
    Query(query): Query<LimitQuery>,
) -> impl IntoResponse {
    let limit = query.limit.unwrap_or(20).min(1000);
    match state.inspector.snapshot_history(limit) {
        Ok(snapshots) => Json(snapshots).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// GET /api/v1/snapshots/at/:tick -> StateSnapshot or 404.
pub async fn get_snapshot_at_tick(
    State(state): State<Arc<AppState>>,
    Path(tick): Path<u64>,
) -> impl IntoResponse {
    match state.inspector.snapshot_at_tick(tick) {
        Ok(Some(snapshot)) => Json(snapshot).into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// GET /api/v1/inbox/history with ?limit=N (default 50).
pub async fn get_inbox_history(
    State(state): State<Arc<AppState>>,
    Query(query): Query<LimitQuery>,
) -> impl IntoResponse {
    let limit = query.limit.unwrap_or(50).min(1000);
    match state.inspector.inbox_history(limit) {
        Ok(entries) => Json(entries).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// GET /api/v1/ticks/:id/context -> CompiledContext or 404
pub async fn get_tick_context(
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
    match state.inspector.context_breakdown(tick_id) {
        Ok(Some(breakdown)) => Json(breakdown).into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// POST /api/v1/charters/reload -> Reload thread charters from disk.
pub async fn post_reload_charters(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    match state.inspector.reload_charters() {
        Ok(updated) => {
            let resp = serde_json::json!({
                "updated": updated,
                "message": format!("{updated} charter(s) reloaded from disk"),
            });
            Json(resp).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// GET /api/v1/config -> Sanitized vessel configuration (I4: no API key values).
pub async fn get_config(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let config = state.inspector.config();

    let llm_local = config
        .llm_config
        .local
        .as_ref()
        .map(|lc| SanitizedLocalConfig {
            endpoint: lc.endpoint.clone(),
            model: lc.model.clone(),
            api_format: match lc.api_format {
                LocalApiFormat::OpenAICompat => "openai_compat".into(),
                LocalApiFormat::Ollama => "ollama".into(),
            },
        });

    let llm_frontier = config
        .llm_config
        .frontier
        .as_ref()
        .map(|fc| SanitizedFrontierConfig {
            provider: format!("{:?}", fc.provider).to_lowercase(),
            model: fc.model.clone(),
            api_key_env: fc.api_key_env.clone(),
        });

    let sanitized = SanitizedConfig {
        vessel_id: config.vessel_id,
        mission: config.mission.clone(),
        data_dir: config.data_dir.to_string_lossy().into_owned(),
        master_loop_interval_secs: config.master_loop_interval_secs,
        cognitive_tick_interval_ms: config.cognitive_tick_interval.as_millis() as u64,
        cognitive_dispatch_concurrency: config.cognitive_dispatch_concurrency.get(),
        tool_tick_interval_ms: config.tool_tick_interval.as_millis() as u64,
        tool_dispatch_concurrency: config.tool_dispatch_concurrency.get(),
        llm_default_backend: match config.llm_config.default_backend {
            LlmBackend::Local => "local".into(),
            LlmBackend::Frontier => "frontier".into(),
        },
        llm_max_output_tokens: config.llm_config.max_output_tokens,
        llm_timeout_secs: config.llm_config.timeout_secs,
        llm_local,
        llm_frontier,
        cognitive_budget: config.cognitive_budget.clone(),
        tool_budget: config.tool_budget.clone(),
        daemon_listen: config.daemon_listen.map(|a| a.to_string()),
    };

    Json(sanitized).into_response()
}

#[derive(Serialize, Deserialize, ts_rs::TS)]
pub struct SanitizedConfig {
    vessel_id: exoskeleton_core::VesselId,
    mission: String,
    data_dir: String,
    master_loop_interval_secs: u64,
    cognitive_tick_interval_ms: u64,
    cognitive_dispatch_concurrency: usize,
    tool_tick_interval_ms: u64,
    tool_dispatch_concurrency: usize,
    llm_default_backend: String,
    llm_max_output_tokens: u64,
    llm_timeout_secs: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    llm_local: Option<SanitizedLocalConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    llm_frontier: Option<SanitizedFrontierConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cognitive_budget: Option<exoskeleton_core::CognitiveBudgetConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_budget: Option<exoskeleton_core::ToolBudgetConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    daemon_listen: Option<String>,
}

#[derive(Serialize, Deserialize, ts_rs::TS)]
pub struct SanitizedLocalConfig {
    endpoint: String,
    model: String,
    api_format: String,
}

#[derive(Serialize, Deserialize, ts_rs::TS)]
pub struct SanitizedFrontierConfig {
    provider: String,
    model: String,
    /// Name of the env var (NOT the key value).
    api_key_env: String,
}

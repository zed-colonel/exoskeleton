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
use exoskeleton_core::id::derive_external_principal_id;
use exoskeleton_core::{
    Artifact, ArtifactId, ArtifactKind, ArtifactStore, CapabilityRequestPayload, ConversationId,
    EnvelopeId, EnvelopeKind, EventType, ExoError, LedgerEntryId, LlmBackend, MessageEnvelope,
    PrincipalId, TickId,
};
use exoskeleton_host::config::LocalApiFormat;
use hmac::Mac;
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

    // Store the message content as an artifact (I3: replayable).
    // Content-addressed: duplicate messages share a single artifact.
    let content_artifact = Artifact::new(
        ArtifactKind::Envelope,
        req.content.as_bytes().to_vec(),
        "text/plain".into(),
    );
    let payload_ref = match state
        .inspector
        .storage()
        .artifact_store()
        .put(&content_artifact)
    {
        Ok(id) => id,
        Err(e) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
        }
    };

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
            let kind_str = serde_json::to_value(artifact.kind)
                .ok()
                .and_then(|v| v.as_str().map(String::from))
                .unwrap_or_else(|| format!("{:?}", artifact.kind));
            let resp = ArtifactResponse {
                id: artifact.id.to_string(),
                kind: kind_str,
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

/// GET /api/v1/conversations?limit=N (default 20, max 100).
pub async fn get_conversations(
    State(state): State<Arc<AppState>>,
    Query(query): Query<LimitQuery>,
) -> impl IntoResponse {
    let limit = query.limit.unwrap_or(20).min(100);
    match state.inspector.conversations(limit) {
        Ok(conversations) => Json(conversations).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// GET /api/v1/conversations/:id
pub async fn get_conversation_by_id(
    State(state): State<Arc<AppState>>,
    Path(id_str): Path<String>,
) -> impl IntoResponse {
    let conv_id: ConversationId = match id_str.parse() {
        Ok(id) => id,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                "invalid conversation ID (expected UUID)".to_string(),
            )
                .into_response();
        }
    };
    match state.inspector.conversation(conv_id) {
        Ok(Some(conv)) => Json(conv).into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// GET /api/v1/conversations/:id/messages -> `Vec<ConversationMessageWithContent>` or 404.
pub async fn get_conversation_messages(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(params): Query<LimitQuery>,
) -> impl IntoResponse {
    let conv_id: ConversationId = match id.parse() {
        Ok(id) => id,
        Err(_) => return StatusCode::BAD_REQUEST.into_response(),
    };
    let limit = params.limit.unwrap_or(200).min(1000);

    match state.inspector.conversation_messages(conv_id, limit) {
        Ok(Some(messages)) => Json(messages).into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
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

// ── E4-S4: Capability Escalation + Webhooks ──

/// Response for GET /api/v1/capability-requests.
#[derive(Debug, Serialize)]
pub struct CapabilityRequestResponse {
    pub event_id: LedgerEntryId,
    pub capability: String,
    pub reason: String,
    pub context: String,
    pub acknowledged: bool,
    pub summary: String,
    pub timestamp: chrono::DateTime<Utc>,
}

/// POST /api/v1/events/:id/acknowledge -> 200 OK or 404
pub async fn post_acknowledge_event(
    State(state): State<Arc<AppState>>,
    Path(id_str): Path<String>,
) -> impl IntoResponse {
    let event_id: LedgerEntryId = match id_str.parse() {
        Ok(id) => id,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                "invalid event ID (expected UUID)".to_string(),
            )
                .into_response();
        }
    };

    // Verify the event exists and is a CapabilityRequest
    match state.inspector.recent_events(1000) {
        Ok(events) => {
            let found = events
                .iter()
                .any(|e| e.id == event_id && e.event_type == EventType::CapabilityRequest);
            if !found {
                return StatusCode::NOT_FOUND.into_response();
            }
        }
        Err(e) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
        }
    }

    state.acknowledged_events.insert(event_id);
    StatusCode::OK.into_response()
}

/// GET /api/v1/capability-requests -> list of capability requests with ack status
pub async fn get_capability_requests(
    State(state): State<Arc<AppState>>,
    Query(query): Query<LimitQuery>,
) -> impl IntoResponse {
    let limit = query.limit.unwrap_or(100).min(1000);
    match state.inspector.recent_events(limit) {
        Ok(events) => {
            let requests: Vec<CapabilityRequestResponse> = events
                .iter()
                .filter(|e| e.event_type == EventType::CapabilityRequest)
                .map(|e| {
                    let (capability, reason, context) = e
                        .payload_ref
                        .as_ref()
                        .and_then(|ref_id| state.inspector.artifact(ref_id).ok().flatten())
                        .and_then(|artifact| {
                            serde_json::from_slice::<CapabilityRequestPayload>(&artifact.content)
                                .ok()
                        })
                        .map(|p| (p.capability, p.reason, p.context))
                        .unwrap_or_else(|| ("unknown".into(), "unknown".into(), "unknown".into()));

                    CapabilityRequestResponse {
                        event_id: e.id,
                        capability,
                        reason,
                        context,
                        acknowledged: state.acknowledged_events.contains(&e.id),
                        summary: e.summary.clone(),
                        timestamp: e.timestamp,
                    }
                })
                .collect();
            Json(requests).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// POST /api/v1/webhooks/generic -> 201 Created or 401/400
pub async fn post_webhook_generic(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> impl IntoResponse {
    // 1. Extract X-Webhook-Source (required)
    let source = match headers
        .get("x-webhook-source")
        .and_then(|v| v.to_str().ok())
    {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                "missing or invalid X-Webhook-Source header".to_string(),
            )
                .into_response();
        }
    };

    // 2. Verify HMAC signature if a secret is configured for this source
    if let Some(secret) = state.webhook_secrets.get(&source) {
        let signature_header = headers
            .get("x-webhook-signature")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");

        if !verify_hmac_sha256(secret, &body, signature_header) {
            return (
                StatusCode::UNAUTHORIZED,
                "invalid webhook signature".to_string(),
            )
                .into_response();
        }
    }

    // 3. Validate body is valid JSON
    if serde_json::from_slice::<serde_json::Value>(&body).is_err() {
        return (
            StatusCode::BAD_REQUEST,
            "request body must be valid JSON".to_string(),
        )
            .into_response();
    }

    // 4. Derive PrincipalId from source
    let identity = format!("webhook:{source}");
    let principal_id = derive_external_principal_id(&identity);

    // 5. Store content as artifact
    let artifact = Artifact::new(
        ArtifactKind::Envelope,
        body.to_vec(),
        "application/json".into(),
    );
    let payload_ref = match state.inspector.storage().artifact_store().put(&artifact) {
        Ok(id) => id,
        Err(e) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
        }
    };

    // 6. Create MessageEnvelope
    let envelope_id = EnvelopeId::new();
    let envelope = MessageEnvelope {
        id: envelope_id,
        source: principal_id,
        target: None,
        kind: EnvelopeKind::HumanMessage,
        payload_ref,
        timestamp: Utc::now(),
        in_reply_to: None,
    };

    // 7. Write to inbox
    match state.inbox.submit(&envelope) {
        Ok(()) => (
            StatusCode::CREATED,
            Json(serde_json::json!({
                "envelope_id": envelope_id,
                "principal_id": principal_id,
            })),
        )
            .into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// Verify an HMAC-SHA256 signature.
///
/// Expected format: "sha256=<hex-encoded HMAC>"
fn verify_hmac_sha256(secret: &[u8], body: &[u8], signature_header: &str) -> bool {
    let expected_hex = match signature_header.strip_prefix("sha256=") {
        Some(hex) => hex,
        None => return false,
    };

    let expected_bytes = match hex::decode(expected_hex) {
        Ok(b) => b,
        Err(_) => return false,
    };

    let mut mac = match hmac::Hmac::<sha2::Sha256>::new_from_slice(secret) {
        Ok(m) => m,
        Err(_) => return false,
    };
    hmac::Mac::update(&mut mac, body);
    hmac::Mac::verify_slice(mac, &expected_bytes).is_ok()
}

// ── E3-S3: Snapshot Fork ──

/// Request body for POST /api/v1/snapshots/at/{tick}/fork.
#[derive(Debug, Deserialize)]
pub struct ForkRequest {
    /// Target data directory for the forked vessel. Must not already exist.
    pub data_dir: String,
    /// Optional mission override. If None, copies the source vessel's mission.
    pub mission: Option<String>,
}

/// Response body for POST /api/v1/snapshots/at/{tick}/fork.
#[derive(Debug, Serialize, Deserialize, ts_rs::TS)]
pub struct ForkResponse {
    /// The new vessel's unique identity.
    pub vessel_id: exoskeleton_core::VesselId,
    /// Path to the generated vessel.toml configuration file.
    pub config_path: String,
    /// Path to the new vessel's data directory.
    pub data_dir: String,
    /// The source tick number that was forked from.
    pub forked_from_tick: u64,
    /// The source vessel's ID (for provenance tracking).
    pub source_vessel_id: exoskeleton_core::VesselId,
}

/// POST /api/v1/snapshots/at/{tick}/fork -> ForkResponse or error.
pub async fn post_fork_snapshot(
    State(state): State<Arc<AppState>>,
    Path(tick): Path<u64>,
    Json(req): Json<ForkRequest>,
) -> impl IntoResponse {
    let target_dir = std::path::PathBuf::from(&req.data_dir);

    match state
        .inspector
        .fork_from_snapshot(tick, &target_dir, req.mission.as_deref())
    {
        Ok(result) => (
            StatusCode::CREATED,
            Json(ForkResponse {
                vessel_id: result.vessel_id,
                config_path: result.config_path.to_string_lossy().into_owned(),
                data_dir: result.data_dir.to_string_lossy().into_owned(),
                forked_from_tick: result.forked_from_tick,
                source_vessel_id: result.source_vessel_id,
            }),
        )
            .into_response(),
        Err(ExoError::NotFound(msg)) => (StatusCode::NOT_FOUND, msg).into_response(),
        Err(ExoError::Config(msg)) if msg.contains("already exists") => {
            (StatusCode::CONFLICT, msg).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

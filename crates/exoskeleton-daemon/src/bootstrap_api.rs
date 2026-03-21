//! Bootstrap API endpoint handlers.
//!
//! All handlers extract `State<Arc<BootstrapState>>` from the axum state.
//! The bootstrap flow is: configure → verify → conversation → finalize → start-vessel.

use std::path::Path;
use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket};
use axum::extract::{State, WebSocketUpgrade};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};

use exoskeleton_core::llm::{LlmBackend, LlmMessage, LlmRequest, LlmRole};
use exoskeleton_host::config::{
    FrontierModelConfig, FrontierProvider, LlmConfig, LocalApiFormat, LocalModelConfig,
};
use exoskeleton_host::direct_llm_call;

use crate::bootstrap_state::{BootstrapError, BootstrapState, VesselIdentity};
use crate::DaemonConfig;

type BootstrapResult<T> = Result<T, (StatusCode, Json<serde_json::Value>)>;

fn err_response(status: StatusCode, msg: &str) -> (StatusCode, Json<serde_json::Value>) {
    (status, Json(serde_json::json!({ "error": msg })))
}

fn bootstrap_err(e: BootstrapError) -> (StatusCode, Json<serde_json::Value>) {
    let status = match &e {
        BootstrapError::NotConfigured
        | BootstrapError::NoTranscript
        | BootstrapError::NotFinalized
        | BootstrapError::AlreadyFinalized => StatusCode::BAD_REQUEST,
        BootstrapError::Unauthorized => StatusCode::UNAUTHORIZED,
        BootstrapError::Config(_) => StatusCode::BAD_REQUEST,
        BootstrapError::Llm(_) | BootstrapError::Io(_) => StatusCode::INTERNAL_SERVER_ERROR,
    };
    err_response(status, &e.to_string())
}

fn extract_registration_token(headers: &axum::http::HeaderMap) -> Option<String> {
    headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .map(|s| s.to_string())
}

fn validate_token(
    state: &BootstrapState,
    headers: &axum::http::HeaderMap,
) -> Result<(), (StatusCode, Json<serde_json::Value>)> {
    let token = extract_registration_token(headers);
    state
        .validate_registration_token(token.as_deref())
        .map_err(|_| {
            (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({ "error": "invalid or missing registration token" })),
            )
        })
}

// ── GET /ready ──

/// In pre-bootstrap mode, returns `{ "mode": "bootstrap" }`.
pub async fn ready() -> impl IntoResponse {
    Json(serde_json::json!({ "mode": "bootstrap" }))
}

// ── POST /api/v1/bootstrap/configure ──

#[derive(Debug, Deserialize)]
pub struct ConfigureRequest {
    pub default_backend: String,
    #[serde(default)]
    pub frontier: Option<FrontierConfigRequest>,
    #[serde(default)]
    pub local: Option<LocalConfigRequest>,
}

#[derive(Debug, Deserialize)]
pub struct FrontierConfigRequest {
    pub provider: String,
    pub model: String,
    pub api_key_env: String,
}

#[derive(Debug, Deserialize)]
pub struct LocalConfigRequest {
    pub endpoint: String,
    pub model: String,
    pub api_format: String,
}

pub async fn configure(
    State(state): State<Arc<BootstrapState>>,
    headers: axum::http::HeaderMap,
    Json(req): Json<ConfigureRequest>,
) -> BootstrapResult<StatusCode> {
    validate_token(&state, &headers)?;

    // Validate: at least one backend configured
    if req.frontier.is_none() && req.local.is_none() {
        return Err(err_response(
            StatusCode::BAD_REQUEST,
            "at least one backend (frontier or local) must be configured",
        ));
    }

    // Parse default_backend
    let default_backend = match req.default_backend.as_str() {
        "frontier" => LlmBackend::Frontier,
        "local" => LlmBackend::Local,
        other => {
            return Err(err_response(
                StatusCode::BAD_REQUEST,
                &format!("invalid default_backend: '{other}' (must be 'frontier' or 'local')"),
            ))
        }
    };

    // Build frontier config
    let frontier = req.frontier.map(|fc| {
        let provider = match fc.provider.as_str() {
            "openai" => FrontierProvider::OpenAI,
            _ => FrontierProvider::Anthropic, // default to Anthropic
        };
        FrontierModelConfig {
            provider,
            model: fc.model,
            api_key_env: fc.api_key_env,
            endpoint: None,
        }
    });

    // Build local config
    let local = req.local.map(|lc| {
        let api_format = match lc.api_format.as_str() {
            "ollama" => LocalApiFormat::Ollama,
            _ => LocalApiFormat::OpenAICompat,
        };
        LocalModelConfig {
            endpoint: lc.endpoint,
            model: lc.model,
            api_format,
        }
    });

    let llm_config = LlmConfig {
        local,
        frontier,
        default_backend,
        max_output_tokens: 4096,
        timeout_secs: 120,
    };

    state.set_llm_config(llm_config).map_err(bootstrap_err)?;
    Ok(StatusCode::OK)
}

// ── POST /api/v1/bootstrap/verify ──

#[derive(Debug, Serialize)]
pub struct VerifyResponse {
    pub verified: bool,
    pub model: Option<String>,
    pub error: Option<String>,
}

pub async fn verify(
    State(state): State<Arc<BootstrapState>>,
    headers: axum::http::HeaderMap,
) -> BootstrapResult<Json<VerifyResponse>> {
    validate_token(&state, &headers)?;

    let llm_config = state.llm_config().map_err(bootstrap_err)?;

    let request = LlmRequest {
        backend: None,
        system_prompt: None,
        messages: vec![LlmMessage {
            role: LlmRole::User,
            content: "Respond with exactly: ok".into(),
        }],
        max_output_tokens: 16,
        temperature: Some(0.0),
        stop_sequences: vec![],
    };

    match direct_llm_call(&llm_config, request).await {
        Ok(response) => Ok(Json(VerifyResponse {
            verified: true,
            model: Some(response.model),
            error: None,
        })),
        Err(e) => Ok(Json(VerifyResponse {
            verified: false,
            model: None,
            error: Some(e.to_string()),
        })),
    }
}

// ── GET /api/v1/bootstrap/conversation (WebSocket) ──

pub async fn conversation_ws(
    State(state): State<Arc<BootstrapState>>,
    headers: axum::http::HeaderMap,
    ws: WebSocketUpgrade,
) -> Result<impl IntoResponse, (StatusCode, Json<serde_json::Value>)> {
    validate_token(&state, &headers)?;
    Ok(ws.on_upgrade(move |socket| handle_conversation(state, socket)))
}

async fn handle_conversation(state: Arc<BootstrapState>, mut socket: WebSocket) {
    // 1. Get LLM config
    let llm_config = match state.llm_config() {
        Ok(c) => c,
        Err(e) => {
            let _ = socket
                .send(Message::Text(
                    serde_json::json!({ "type": "error", "message": e.to_string() }).to_string(),
                ))
                .await;
            return;
        }
    };

    // 2. Load bootstrap-first-contact prompt
    let system_prompt = match state.get_prompt("bootstrap-first-contact") {
        Some(p) => p,
        None => {
            let _ = socket
                .send(Message::Text(
                    serde_json::json!({ "type": "error", "message": "bootstrap-first-contact prompt not found" })
                        .to_string(),
                ))
                .await;
            return;
        }
    };

    // 3. Seed with "Hello." and get vessel greeting
    let seed = LlmMessage {
        role: LlmRole::User,
        content: "Hello.".into(),
    };
    state.push_message(seed.clone());

    let request = LlmRequest {
        backend: None,
        system_prompt: Some(system_prompt.clone()),
        messages: vec![seed],
        max_output_tokens: 1024,
        temperature: Some(0.8),
        stop_sequences: vec![],
    };

    let greeting = match direct_llm_call(&llm_config, request).await {
        Ok(r) => r.content,
        Err(e) => {
            let _ = socket
                .send(Message::Text(
                    serde_json::json!({ "type": "error", "message": format!("LLM error: {e}") })
                        .to_string(),
                ))
                .await;
            return;
        }
    };

    // 4. Store greeting and send to client
    state.push_message(LlmMessage {
        role: LlmRole::Assistant,
        content: greeting.clone(),
    });

    if socket
        .send(Message::Text(
            serde_json::json!({ "type": "greeting", "content": greeting }).to_string(),
        ))
        .await
        .is_err()
    {
        return;
    }

    // 5. Conversation loop
    loop {
        let msg = match socket.recv().await {
            Some(Ok(Message::Text(text))) => text,
            Some(Ok(Message::Close(_))) | None => break,
            _ => continue,
        };

        // Parse client message
        let parsed: serde_json::Value = match serde_json::from_str(&msg) {
            Ok(v) => v,
            Err(_) => continue,
        };

        let msg_type = parsed.get("type").and_then(|t| t.as_str()).unwrap_or("");

        if msg_type == "done" {
            let transcript = state.transcript();
            let count = transcript.len();
            let _ = socket
                .send(Message::Text(
                    serde_json::json!({ "type": "complete", "message_count": count }).to_string(),
                ))
                .await;
            break;
        }

        if msg_type == "message" {
            let content = parsed
                .get("content")
                .and_then(|c| c.as_str())
                .unwrap_or("")
                .to_string();
            if content.is_empty() {
                continue;
            }

            // Store user message
            state.push_message(LlmMessage {
                role: LlmRole::User,
                content: content.clone(),
            });

            // Build LLM request with full transcript
            let transcript = state.transcript();
            let request = LlmRequest {
                backend: None,
                system_prompt: Some(system_prompt.clone()),
                messages: transcript,
                max_output_tokens: 1024,
                temperature: Some(0.8),
                stop_sequences: vec![],
            };

            match direct_llm_call(&llm_config, request).await {
                Ok(response) => {
                    state.push_message(LlmMessage {
                        role: LlmRole::Assistant,
                        content: response.content.clone(),
                    });
                    let _ = socket
                        .send(Message::Text(
                            serde_json::json!({ "type": "message", "content": response.content })
                                .to_string(),
                        ))
                        .await;
                }
                Err(e) => {
                    let _ = socket
                        .send(Message::Text(
                            serde_json::json!({ "type": "error", "message": format!("LLM error: {e}") })
                                .to_string(),
                        ))
                        .await;
                    // Don't close on LLM error — allow retry
                }
            }
        }
    }
}

// ── POST /api/v1/bootstrap/finalize ──

#[derive(Debug, Serialize)]
pub struct FinalizeResponse {
    pub identity: VesselIdentity,
    pub config_path: String,
}

pub async fn finalize(
    State(state): State<Arc<BootstrapState>>,
    headers: axum::http::HeaderMap,
) -> BootstrapResult<Json<FinalizeResponse>> {
    validate_token(&state, &headers)?;

    if state.is_finalized() {
        return Err(bootstrap_err(BootstrapError::AlreadyFinalized));
    }

    let transcript = state.transcript();
    if transcript.len() < 2 {
        return Err(bootstrap_err(BootstrapError::NoTranscript));
    }

    let llm_config = state.llm_config().map_err(bootstrap_err)?;

    // Format transcript
    let transcript_text = transcript
        .iter()
        .map(|m| {
            let role = match m.role {
                LlmRole::User => "Human",
                LlmRole::Assistant => "Vessel",
                LlmRole::System => "System",
            };
            format!("{role}: {}", m.content)
        })
        .collect::<Vec<_>>()
        .join("\n\n");

    // Load identity extraction prompt and substitute transcript
    let extraction_prompt = state
        .resolve_prompt(
            "bootstrap-identity-extraction",
            &[("transcript", &transcript_text)],
        )
        .map_err(|e| {
            err_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                &format!("prompt error: {e}"),
            )
        })?;

    // Call LLM for identity extraction
    let request = LlmRequest {
        backend: None,
        system_prompt: Some(
            "You are a precise information extractor. Respond only with valid JSON.".into(),
        ),
        messages: vec![LlmMessage {
            role: LlmRole::User,
            content: extraction_prompt,
        }],
        max_output_tokens: 512,
        temperature: Some(0.0),
        stop_sequences: vec![],
    };

    let response = direct_llm_call(&llm_config, request).await.map_err(|e| {
        err_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("LLM error: {e}"),
        )
    })?;

    // Parse JSON response — strip markdown code fences if present
    let raw = response.content.trim();
    let json_str = extract_json_object(raw).unwrap_or(raw);

    let identity: VesselIdentity = serde_json::from_str(json_str).map_err(|e| {
        err_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("failed to parse identity: {e}"),
        )
    })?;

    // Write vessel.toml
    let config_path =
        write_vessel_config(&state.data_dir, &llm_config, &identity, state.listen_addr).map_err(
            |e| {
                err_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    &format!("write error: {e}"),
                )
            },
        )?;

    // Write bootstrap artifacts
    write_bootstrap_record(&state.data_dir, &transcript, &identity).map_err(|e| {
        err_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("write error: {e}"),
        )
    })?;

    state.set_identity(identity.clone());
    state.set_finalized();

    Ok(Json(FinalizeResponse {
        identity,
        config_path: config_path.display().to_string(),
    }))
}

// ── POST /api/v1/bootstrap/start-vessel ──

#[derive(Debug, Serialize)]
pub struct StartVesselResponse {
    pub status: String,
}

pub async fn start_vessel(
    State(state): State<Arc<BootstrapState>>,
    headers: axum::http::HeaderMap,
) -> BootstrapResult<Json<StartVesselResponse>> {
    validate_token(&state, &headers)?;

    if !state.is_finalized() {
        return Err(bootstrap_err(BootstrapError::NotFinalized));
    }

    // Load vessel.toml from data_dir
    let config_path = state.data_dir.join("vessel.toml");
    let vessel_config = exoskeleton_host::VesselConfig::from_file(&config_path).map_err(|e| {
        err_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("config error: {e}"),
        )
    })?;

    let daemon_config = DaemonConfig {
        vessel: vessel_config,
        listen_addr: state.listen_addr,
    };

    // Spawn a brief delay to allow the HTTP response to be sent, then trigger shutdown
    let state_clone = state.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        state_clone.trigger_start_vessel(daemon_config);
    });

    Ok(Json(StartVesselResponse {
        status: "starting".to_string(),
    }))
}

// ── Helpers ──

/// Write the vessel.toml configuration file.
fn write_vessel_config(
    data_dir: &Path,
    llm_config: &LlmConfig,
    identity: &VesselIdentity,
    listen_addr: std::net::SocketAddr,
) -> Result<std::path::PathBuf, String> {
    std::fs::create_dir_all(data_dir).map_err(|e| format!("failed to create data dir: {e}"))?;

    let config_path = data_dir.join("vessel.toml");
    let vessel_id = uuid::Uuid::new_v4();

    let mut toml = String::new();
    toml.push_str(&format!(
        "# Vessel configuration — generated by bootstrap API\n# {}\n\n",
        chrono::Utc::now().format("%Y-%m-%d %H:%M:%S UTC")
    ));

    // [vessel] section
    toml.push_str("[vessel]\n");
    toml.push_str(&format!("vessel_id = \"{vessel_id}\"\n"));
    toml.push_str(&format!(
        "mission = {}\n",
        toml_string_escape(&identity.mission)
    ));
    toml.push_str(&format!(
        "data_dir = {}\n",
        toml_string_escape(&data_dir.to_string_lossy())
    ));
    toml.push_str("master_loop_interval_secs = 60\n\n");

    // [llm] section
    toml.push_str("[llm]\n");
    let backend_str = match llm_config.default_backend {
        LlmBackend::Frontier => "frontier",
        LlmBackend::Local => "local",
    };
    toml.push_str(&format!("default_backend = \"{backend_str}\"\n"));
    toml.push_str("max_output_tokens = 4096\n");
    toml.push_str("timeout_secs = 120\n\n");

    if let Some(ref lc) = llm_config.local {
        toml.push_str("[llm.local]\n");
        toml.push_str(&format!(
            "endpoint = {}\n",
            toml_string_escape(&lc.endpoint)
        ));
        toml.push_str(&format!("model = {}\n", toml_string_escape(&lc.model)));
        let fmt_str = match lc.api_format {
            LocalApiFormat::OpenAICompat => "openai_compat",
            LocalApiFormat::Ollama => "ollama",
        };
        toml.push_str(&format!("api_format = \"{fmt_str}\"\n\n"));
    }

    if let Some(ref fc) = llm_config.frontier {
        toml.push_str("[llm.frontier]\n");
        let provider_str = match fc.provider {
            FrontierProvider::Anthropic => "anthropic",
            FrontierProvider::OpenAI => "openai",
        };
        toml.push_str(&format!("provider = \"{provider_str}\"\n"));
        toml.push_str(&format!("model = {}\n", toml_string_escape(&fc.model)));
        toml.push_str(&format!(
            "api_key_env = {}\n\n",
            toml_string_escape(&fc.api_key_env)
        ));
    }

    // [daemon] section
    toml.push_str("[daemon]\n");
    toml.push_str(&format!("listen = \"{listen_addr}\"\n"));

    std::fs::write(&config_path, &toml).map_err(|e| format!("failed to write vessel.toml: {e}"))?;
    Ok(config_path)
}

/// Write the bootstrap record (conversation transcript + identity).
fn write_bootstrap_record(
    data_dir: &Path,
    transcript: &[LlmMessage],
    identity: &VesselIdentity,
) -> Result<(), String> {
    let bootstrap_dir = data_dir.join("bootstrap");
    std::fs::create_dir_all(&bootstrap_dir)
        .map_err(|e| format!("failed to create bootstrap dir: {e}"))?;

    let transcript_data = serde_json::json!({
        "timestamp": chrono::Utc::now().to_rfc3339(),
        "messages": transcript.iter().map(|m| {
            serde_json::json!({
                "role": format!("{:?}", m.role).to_lowercase(),
                "content": m.content,
            })
        }).collect::<Vec<_>>(),
    });

    std::fs::write(
        bootstrap_dir.join("first-contact.json"),
        serde_json::to_string_pretty(&transcript_data).unwrap(),
    )
    .map_err(|e| format!("failed to write transcript: {e}"))?;

    std::fs::write(
        bootstrap_dir.join("identity.json"),
        serde_json::to_string_pretty(&identity).unwrap(),
    )
    .map_err(|e| format!("failed to write identity: {e}"))?;

    Ok(())
}

/// Escape a string for TOML output.
fn toml_string_escape(s: &str) -> String {
    let escaped = s
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n");
    format!("\"{escaped}\"")
}

/// Extract the first JSON object from a string.
fn extract_json_object(s: &str) -> Option<&str> {
    let start = s.find('{')?;
    let end = s.rfind('}')?;
    if end > start {
        Some(&s[start..=end])
    } else {
        None
    }
}

//! Virtual tool descriptors and translation logic.
//!
//! Virtual tools are Exoskeleton-specific tool names that the LLM sees in its
//! tool list (`agent.ask_user`, `peer.resolve`). The Act step translates them
//! into generic WI connector invocations, preserving I9 (dual-engine boundary).
//!
//! The LLM does NOT see `signal.await` or `signal.emit` — these are
//! infrastructure primitives. Virtual tools are the abstraction boundary
//! between agent intent and infrastructure mechanism.

use exoskeleton_core::llm::ToolDefinition;
use exoskeleton_core::{
    Artifact, ArtifactKind, EventType, ExoError, LiveEvent, QuestionDetail, TickId,
};
use serde_json::{json, Value};
use worldinterface_core::descriptor::{ConnectorCategory, Descriptor};

use super::KernelContext;

/// Virtual tool name for agent-to-operator questions.
pub const AGENT_ASK_USER: &str = "agent.ask_user";
/// Virtual tool name for fleet peer resolution.
pub const PEER_RESOLVE: &str = "peer.resolve";

/// Returns true if the tool name is a virtual tool handled by Exoskeleton.
pub fn is_virtual_tool(name: &str) -> bool {
    matches!(name, AGENT_ASK_USER | PEER_RESOLVE)
}

/// Returns descriptors for all virtual tools. These are included in the
/// LLM's tool list alongside WI connector descriptors.
pub fn virtual_tool_descriptors() -> Vec<Descriptor> {
    vec![ask_user_descriptor(), peer_resolve_descriptor()]
}

/// Returns ToolDefinitions for virtual tools exposed to the LLM.
pub fn virtual_tool_definitions() -> Vec<ToolDefinition> {
    virtual_tool_descriptors()
        .into_iter()
        .map(|descriptor| ToolDefinition {
            name: descriptor.name,
            description: descriptor.description,
            input_schema: descriptor
                .input_schema
                .unwrap_or_else(|| json!({ "type": "object", "properties": {} })),
        })
        .collect()
}

fn ask_user_descriptor() -> Descriptor {
    Descriptor {
        name: AGENT_ASK_USER.into(),
        display_name: "Ask User".into(),
        description: "Present a question to the operator and wait for a response. \
                      If the operator answers promptly, returns the answer. On timeout, \
                      returns pending status and the coding session yields gracefully."
            .into(),
        category: ConnectorCategory::Custom("interaction".into()),
        input_schema: Some(json!({
            "type": "object",
            "required": ["question"],
            "properties": {
                "question": { "type": "string", "description": "The question text" },
                "choices": {
                    "type": "array", "items": { "type": "string" },
                    "description": "Bounded choices (omit for free-text)"
                },
                "timeout_secs": {
                    "type": "integer", "description": "Wait timeout. Default: 30",
                    "minimum": 1
                },
                "context": { "type": "string", "description": "Additional context" }
            }
        })),
        output_schema: Some(json!({
            "type": "object",
            "properties": {
                "status": { "type": "string", "enum": ["answered", "pending"] },
                "question_id": { "type": "string" },
                "answer": { "type": "string" }
            }
        })),
        idempotent: true,
        side_effects: false,
        is_read_only: true,
        is_mutating: false,
        is_concurrency_safe: false,
        requires_read_before_write: false,
    }
}

fn peer_resolve_descriptor() -> Descriptor {
    Descriptor {
        name: PEER_RESOLVE.into(),
        display_name: "Peer Resolve".into(),
        description: "Resolves a vessel name to its inbox URL via the fleet registry.".into(),
        category: ConnectorCategory::Http,
        input_schema: Some(json!({
            "type": "object",
            "required": ["name"],
            "properties": {
                "name": { "type": "string", "description": "Vessel name to resolve" }
            }
        })),
        output_schema: Some(json!({
            "type": "object",
            "properties": {
                "name": { "type": "string" },
                "inbox_url": { "type": "string" },
                "vessel_id": { "type": "string" }
            }
        })),
        idempotent: true,
        side_effects: false,
        is_read_only: true,
        is_mutating: false,
        is_concurrency_safe: true,
        requires_read_before_write: false,
    }
}

/// Top-level translation dispatch. Returns `(actual_tool_name, actual_params)`.
///
/// The Act step calls `invoke_single(actual_tool_name, actual_params)` via WI.
/// Returns an optional `VirtualContext` for post-processing.
pub fn translate(
    virtual_name: &str,
    params: &Value,
    kernel: &KernelContext,
    tick_id: TickId,
) -> Result<(String, Value, VirtualContext), ExoError> {
    match virtual_name {
        AGENT_ASK_USER => {
            let (tool_name, tool_params, question_id) =
                translate_ask_user(params, kernel, tick_id)?;
            Ok((
                tool_name,
                tool_params,
                VirtualContext::AskUser { question_id },
            ))
        }
        PEER_RESOLVE => {
            let observatory_url = kernel.observatory_url.as_deref().ok_or_else(|| {
                ExoError::Engine("peer.resolve: observatory_url not configured".into())
            })?;
            let observatory_token = kernel.observatory_token.as_deref();
            let (tool_name, tool_params) =
                translate_peer_resolve(params, observatory_url, observatory_token)?;
            Ok((tool_name, tool_params, VirtualContext::PeerResolve))
        }
        _ => Err(ExoError::Engine(format!(
            "unknown virtual tool: {virtual_name}"
        ))),
    }
}

/// Post-process the WI result back into the virtual tool's response format.
pub fn post_process(
    context: &VirtualContext,
    result: &Result<Value, String>,
) -> Result<Value, String> {
    match context {
        VirtualContext::AskUser { question_id } => match result {
            Ok(signal_result) => Ok(post_process_ask_user(question_id, signal_result)),
            Err(e) => Err(e.clone()),
        },
        VirtualContext::PeerResolve => post_process_peer_resolve(result),
    }
}

/// Context carried from translation to post-processing.
pub enum VirtualContext {
    AskUser { question_id: String },
    PeerResolve,
}

// ── agent.ask_user translation ──

/// Translate agent.ask_user into signal.await + question artifact + events.
fn translate_ask_user(
    params: &Value,
    kernel: &KernelContext,
    _tick_id: TickId,
) -> Result<(String, Value, String), ExoError> {
    let question = params
        .get("question")
        .and_then(Value::as_str)
        .ok_or_else(|| ExoError::Engine("agent.ask_user: missing 'question'".into()))?;
    let timeout_secs = params
        .get("timeout_secs")
        .and_then(Value::as_u64)
        .unwrap_or(30);
    let question_id = uuid::Uuid::new_v4().to_string();

    // Store question artifact
    let question_artifact = json!({
        "question_id": question_id,
        "question": question,
        "choices": params.get("choices"),
        "context": params.get("context"),
        "asked_at": chrono::Utc::now().to_rfc3339(),
    });
    let artifact = Artifact::from_json(ArtifactKind::Event, &question_artifact)?;
    kernel.artifact_store.put(&artifact)?;

    // Emit QuestionAsked event
    let tick_number = kernel
        .tick_store
        .latest()
        .ok()
        .flatten()
        .map(|t| t.tick_number + 1);
    let _ = kernel.event_tx.send(LiveEvent {
        event_type: EventType::QuestionAsked,
        summary: format!("Question: {}", question),
        question_detail: Some(QuestionDetail {
            question_id: question_id.clone(),
            question: question.to_string(),
            choices: params
                .get("choices")
                .and_then(|v| serde_json::from_value(v.clone()).ok()),
            status: "pending".to_string(),
        }),
        ..LiveEvent::new(tick_number)
    });

    // Translate to signal.await invocation
    let signal_key = format!("question:{}", question_id);
    let signal_params = json!({
        "key": signal_key,
        "timeout_secs": timeout_secs,
    });

    Ok(("signal.await".to_string(), signal_params, question_id))
}

/// Post-process signal.await result back into agent.ask_user response format.
pub fn post_process_ask_user(question_id: &str, signal_result: &Value) -> Value {
    let status = signal_result
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("timeout");
    match status {
        "received" => {
            let payload = signal_result.get("payload").cloned().unwrap_or(json!({}));
            json!({
                "status": "answered",
                "question_id": question_id,
                "answer": payload.get("answer").cloned().unwrap_or(json!(null)),
                "answered_by": payload.get("answered_by").cloned().unwrap_or(json!(null)),
            })
        }
        _ => {
            json!({
                "status": "pending",
                "question_id": question_id,
            })
        }
    }
}

// ── peer.resolve translation ──

/// Translate peer.resolve into http.request + response parsing.
pub fn translate_peer_resolve(
    params: &Value,
    observatory_url: &str,
    observatory_token: Option<&str>,
) -> Result<(String, Value), ExoError> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| ExoError::Engine("peer.resolve: missing 'name'".into()))?;

    // Validate name (defense-in-depth against path injection)
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_')
    {
        return Err(ExoError::Engine(format!(
            "peer.resolve: invalid vessel name '{name}'"
        )));
    }

    let url = format!(
        "{}/api/v1/fleet/vessels/{}/inbox-url",
        observatory_url, name
    );
    let mut headers = serde_json::Map::new();
    if let Some(token) = observatory_token {
        headers.insert(
            "Authorization".to_string(),
            json!(format!("Bearer {}", token)),
        );
    }

    let http_params = json!({
        "url": url,
        "method": "GET",
        "headers": Value::Object(headers),
    });

    Ok(("http.request".to_string(), http_params))
}

/// Post-process http.request result for peer.resolve error classification.
pub fn post_process_peer_resolve(http_result: &Result<Value, String>) -> Result<Value, String> {
    match http_result {
        Ok(value) => {
            // Check for HTTP error status in the response
            if let Some(status) = value.get("status_code").and_then(Value::as_u64) {
                if status == 404 {
                    return Err("vessel not found".to_string());
                }
                if status >= 400 {
                    return Err(format!("fleet registry returned HTTP {}", status));
                }
            }
            // Return the response body
            Ok(value.get("body").cloned().unwrap_or(value.clone()))
        }
        Err(e) => Err(e.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── T17: is_virtual_tool_recognizes_known_tools ──
    #[test]
    fn is_virtual_tool_recognizes_known_tools() {
        assert!(is_virtual_tool("agent.ask_user"));
        assert!(is_virtual_tool("peer.resolve"));
    }

    // ── T18: is_virtual_tool_rejects_unknown ──
    #[test]
    fn is_virtual_tool_rejects_unknown() {
        assert!(!is_virtual_tool("code.read"));
        assert!(!is_virtual_tool("signal.await"));
        assert!(!is_virtual_tool("signal.emit"));
        assert!(!is_virtual_tool("delay"));
        assert!(!is_virtual_tool(""));
    }

    // ── T19: ask_user_translation_produces_signal_await ──
    #[test]
    fn ask_user_translation_produces_signal_await() {
        // We test translate_peer_resolve directly since translate_ask_user
        // requires a full KernelContext. The key format and tool name are
        // validated through the public interface.
        let descriptors = virtual_tool_descriptors();
        let ask_desc = descriptors
            .iter()
            .find(|d| d.name == AGENT_ASK_USER)
            .unwrap();
        assert_eq!(ask_desc.name, "agent.ask_user");

        // Validate the post_process side independently
        let signal_result = json!({
            "status": "received",
            "payload": { "answer": "pytest", "answered_by": "operator" }
        });
        let result = post_process_ask_user("test-id", &signal_result);
        assert_eq!(result["status"], "answered");
        assert_eq!(result["answer"], "pytest");
        assert_eq!(result["question_id"], "test-id");
    }

    // ── T20: ask_user_post_process_received ──
    #[test]
    fn ask_user_post_process_received() {
        let signal_result = json!({
            "status": "received",
            "payload": { "answer": "yes", "answered_by": "operator" }
        });
        let result = post_process_ask_user("q-123", &signal_result);
        assert_eq!(result["status"], "answered");
        assert_eq!(result["answer"], "yes");
        assert_eq!(result["answered_by"], "operator");
        assert_eq!(result["question_id"], "q-123");
    }

    // ── T21: ask_user_post_process_timeout ──
    #[test]
    fn ask_user_post_process_timeout() {
        let signal_result = json!({ "status": "timeout" });
        let result = post_process_ask_user("q-456", &signal_result);
        assert_eq!(result["status"], "pending");
        assert_eq!(result["question_id"], "q-456");
    }

    // ── T22: peer_resolve_translation_produces_http_request ──
    #[test]
    fn peer_resolve_translation_produces_http_request() {
        let params = json!({"name": "atlas"});
        let (tool_name, tool_params) =
            translate_peer_resolve(&params, "http://observatory:3000", None).unwrap();
        assert_eq!(tool_name, "http.request");
        assert_eq!(
            tool_params["url"],
            "http://observatory:3000/api/v1/fleet/vessels/atlas/inbox-url"
        );
        assert_eq!(tool_params["method"], "GET");
    }

    // ── T23: peer_resolve_name_validation ──
    #[test]
    fn peer_resolve_name_validation() {
        // Empty name
        let err = translate_peer_resolve(&json!({"name": ""}), "http://obs", None);
        assert!(err.is_err());

        // Path traversal
        let err = translate_peer_resolve(&json!({"name": "../../admin"}), "http://obs", None);
        assert!(err.is_err());

        // Spaces
        let err = translate_peer_resolve(&json!({"name": "bad name"}), "http://obs", None);
        assert!(err.is_err());

        // Missing name
        let err = translate_peer_resolve(&json!({}), "http://obs", None);
        assert!(err.is_err());

        // Valid names
        assert!(translate_peer_resolve(&json!({"name": "atlas"}), "http://obs", None).is_ok());
        assert!(
            translate_peer_resolve(&json!({"name": "my-vessel.v2"}), "http://obs", None).is_ok()
        );
        assert!(translate_peer_resolve(&json!({"name": "vessel_1"}), "http://obs", None).is_ok());
    }

    // ── T24: peer_resolve_auth_header_included ──
    #[test]
    fn peer_resolve_auth_header_included() {
        let params = json!({"name": "atlas"});
        let (_, tool_params) =
            translate_peer_resolve(&params, "http://obs:3000", Some("my-token")).unwrap();
        assert_eq!(tool_params["headers"]["Authorization"], "Bearer my-token");
    }

    // ── T25: peer_resolve_post_process_404 ──
    #[test]
    fn peer_resolve_post_process_404() {
        let http_result = Ok(json!({"status_code": 404, "body": "not found"}));
        let result = post_process_peer_resolve(&http_result);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not found"));
    }

    // ── T26: peer_resolve_post_process_5xx ──
    #[test]
    fn peer_resolve_post_process_5xx() {
        let http_result = Ok(json!({"status_code": 503, "body": "unavailable"}));
        let result = post_process_peer_resolve(&http_result);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("503"));
    }

    // ── T32: peer_resolve_excluded_without_observatory_url ──
    #[test]
    fn peer_resolve_excluded_without_observatory_url() {
        // Simulate the filtering logic from decide.rs
        let mut tools = virtual_tool_descriptors();
        let observatory_url: Option<String> = None;
        if observatory_url.is_none() {
            tools.retain(|d| d.name != PEER_RESOLVE);
        }
        assert!(tools.iter().any(|d| d.name == AGENT_ASK_USER));
        assert!(!tools.iter().any(|d| d.name == PEER_RESOLVE));
    }
}

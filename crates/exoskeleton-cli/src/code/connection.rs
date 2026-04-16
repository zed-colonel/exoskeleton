//! Connection layer for `exo code` — WebSocket streaming and REST integration.
//!
//! Handles URL normalization, WebSocket URL derivation, initial vessel status
//! fetch, conversation history loading, and task submission.

use std::time::Duration;

use exoskeleton_core::derive_external_principal_id;
use serde_json::Value;

use crate::client::{CliError, DaemonClient};

use super::app::{App, ConnectionStatus, StatusState};
use super::widgets::conversation::{Block, NoteSeverity, ToolOutcome};

/// The principal ID used for all `exo code` CLI interactions.
pub fn cli_principal_id() -> String {
    derive_external_principal_id("exo-code-cli").to_string()
}

/// Normalize a daemon address to a base URL with scheme.
pub fn normalize_base_url(addr: &str) -> String {
    let trimmed = addr.trim_end_matches('/');
    if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        trimmed.to_string()
    } else {
        format!("http://{trimmed}")
    }
}

/// Derive a WebSocket URL from an HTTP base URL.
pub fn websocket_url(base_url: &str) -> String {
    if let Some(rest) = base_url.strip_prefix("https://") {
        format!("wss://{rest}/api/v1/ws")
    } else if let Some(rest) = base_url.strip_prefix("http://") {
        format!("ws://{rest}/api/v1/ws")
    } else {
        format!("ws://{}/api/v1/ws", base_url.trim_end_matches('/'))
    }
}

/// Fetch vessel status and populate the App's status state.
///
/// Returns the vessel_id string if a snapshot is already available.
pub async fn fetch_vessel_status(
    client: &DaemonClient,
    app: &mut App,
) -> Result<Option<String>, CliError> {
    let Some(status) = client.status().await? else {
        app.connection = ConnectionStatus::Connected;
        app.status = StatusState {
            vessel_name: "Starting vessel...".into(),
            vessel_mode: "booting".into(),
            token_summary: String::new(),
            step_summary: "Waiting for first completed tick".into(),
            exec_summary: String::new(),
        };
        app.vessel_id = "pending".into();
        return Ok(None);
    };

    let vessel_id = status
        .get("vessel_id")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();

    let vessel_name = status
        .get("mission")
        .and_then(|v| v.as_str())
        .unwrap_or(&vessel_id)
        .to_string();

    let vessel_mode = status
        .get("vessel_mode")
        .and_then(|v| v.as_str())
        .unwrap_or("normal")
        .to_string();

    app.status = StatusState {
        vessel_name,
        vessel_mode,
        token_summary: String::new(),
        step_summary: String::new(),
        exec_summary: String::new(),
    };
    app.connection = ConnectionStatus::Connected;
    app.vessel_id = vessel_id.clone();

    Ok(Some(vessel_id))
}

/// Load recent conversation history into the App.
///
/// Fetches the most recent conversation and its last N messages,
/// converting them to Block items in the conversation state.
#[allow(dead_code)]
pub async fn load_conversation_history(
    client: &DaemonClient,
    app: &mut App,
    message_limit: usize,
) -> Result<Option<String>, CliError> {
    load_conversation_history_for(client, app, message_limit, None).await
}

/// Load recent conversation history, optionally preferring a specific conversation ID.
pub async fn load_conversation_history_for(
    client: &DaemonClient,
    app: &mut App,
    message_limit: usize,
    preferred_conversation_id: Option<&str>,
) -> Result<Option<String>, CliError> {
    if let Some(preferred) = preferred_conversation_id {
        if let Ok(messages) = client.conversation_messages(preferred, message_limit).await {
            for msg in &messages {
                if let Some(block) = history_message_to_block(msg) {
                    app.conversation.add_block(block);
                }
            }
            app.current_conversation_id = preferred.to_string();
            return Ok(Some(preferred.to_string()));
        }
    }

    let conversations = client.conversations(1).await?;
    let conversation_id = match conversations.first() {
        Some(conv) => match conv.get("id").and_then(|v| v.as_str()) {
            Some(id) => id.to_string(),
            None => return Ok(None),
        },
        None => return Ok(None),
    };

    let messages = client
        .conversation_messages(&conversation_id, message_limit)
        .await?;

    for msg in &messages {
        if let Some(block) = history_message_to_block(msg) {
            app.conversation.add_block(block);
        }
    }

    app.current_conversation_id = conversation_id.clone();

    Ok(Some(conversation_id))
}

/// Convert a historical conversation message (JSON) to a Block.
fn history_message_to_block(msg: &Value) -> Option<Block> {
    if let Some(role) = msg.get("role").and_then(|v| v.as_str()) {
        let content = msg.get("content").and_then(|v| v.as_str())?;
        return match role {
            "user" => Some(Block::UserMessage {
                text: content.to_string(),
                timestamp: parse_timestamp(msg),
            }),
            "assistant" => Some(Block::AgentText {
                text: content.to_string(),
                is_streaming: false,
            }),
            "tool" => {
                let tool_name = msg
                    .get("tool_name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("tool")
                    .to_string();
                Some(Block::ToolCall {
                    tool_name,
                    args_summary: content.chars().take(80).collect(),
                    outcome: outcome_from_str(
                        msg.get("outcome")
                            .and_then(|v| v.as_str())
                            .unwrap_or("success"),
                    ),
                    collapsed: true,
                    token_cost: None,
                })
            }
            "system" => Some(Block::SystemNote {
                text: content.to_string(),
                severity: NoteSeverity::Info,
            }),
            _ => None,
        };
    }

    let content = msg.get("content").and_then(|v| v.as_str())?;
    let is_vessel_reply = msg
        .get("is_vessel_reply")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    if is_vessel_reply {
        Some(Block::AgentText {
            text: content.to_string(),
            is_streaming: false,
        })
    } else {
        Some(Block::UserMessage {
            text: content.to_string(),
            timestamp: parse_timestamp(msg),
        })
    }
}

fn parse_timestamp(msg: &Value) -> chrono::DateTime<chrono::Utc> {
    msg.get("timestamp")
        .and_then(|v| v.as_str())
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&chrono::Utc))
        .unwrap_or_else(chrono::Utc::now)
}

fn outcome_from_str(outcome: &str) -> ToolOutcome {
    match outcome {
        "success" => ToolOutcome::Success,
        "pending" => ToolOutcome::Pending,
        "denied" | "policy_denied" => ToolOutcome::PolicyDenied,
        other => ToolOutcome::Error(other.to_string()),
    }
}

/// Submit a coding task via the inbox.
pub async fn send_message(client: &DaemonClient, content: &str) -> Result<(), CliError> {
    let principal = cli_principal_id();
    client.send_message(&principal, content).await?;
    Ok(())
}

/// Reconnect delays for exponential backoff.
///
/// Returns the sequence of delays to use: 1s, 2s, 4s, 8s, 16s, 30s, 30s, ...
/// Capped at 30 seconds.
pub fn backoff_delays(max_retries: u32) -> Vec<Duration> {
    let mut delays = Vec::with_capacity(max_retries as usize);
    let mut delay = Duration::from_secs(1);
    let max_delay = Duration::from_secs(30);
    for _ in 0..max_retries {
        delays.push(delay);
        delay = (delay * 2).min(max_delay);
    }
    delays
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── TUI-T12: normalize_base_url_strips_trailing_slash ──

    #[test]
    fn normalize_base_url_strips_trailing_slash() {
        assert_eq!(
            normalize_base_url("http://localhost:7600/"),
            "http://localhost:7600"
        );
        assert_eq!(
            normalize_base_url("http://localhost:7600"),
            "http://localhost:7600"
        );
    }

    #[test]
    fn normalize_base_url_adds_scheme() {
        assert_eq!(
            normalize_base_url("localhost:7600"),
            "http://localhost:7600"
        );
    }

    #[test]
    fn normalize_base_url_preserves_https() {
        assert_eq!(
            normalize_base_url("https://example.com:8080/"),
            "https://example.com:8080"
        );
    }

    // ── TUI-T13: websocket_url_from_http ──

    #[test]
    fn websocket_url_from_http() {
        assert_eq!(
            websocket_url("http://localhost:7600"),
            "ws://localhost:7600/api/v1/ws"
        );
    }

    // ── TUI-T14: websocket_url_from_https ──

    #[test]
    fn websocket_url_from_https() {
        assert_eq!(
            websocket_url("https://example.com"),
            "wss://example.com/api/v1/ws"
        );
    }

    #[test]
    fn websocket_url_no_scheme() {
        assert_eq!(
            websocket_url("localhost:7600"),
            "ws://localhost:7600/api/v1/ws"
        );
    }

    #[test]
    fn cli_principal_id_is_deterministic() {
        let id1 = cli_principal_id();
        let id2 = cli_principal_id();
        assert_eq!(id1, id2);
        assert!(!id1.is_empty());
    }

    #[test]
    fn reconnect_backoff_delays_double() {
        let delays = backoff_delays(4);
        assert_eq!(delays.len(), 4);
        assert_eq!(delays[0], Duration::from_secs(1));
        assert_eq!(delays[1], Duration::from_secs(2));
        assert_eq!(delays[2], Duration::from_secs(4));
        assert_eq!(delays[3], Duration::from_secs(8));
    }

    #[test]
    fn reconnect_backoff_caps_at_30s() {
        let delays = backoff_delays(10);
        for delay in &delays {
            assert!(
                *delay <= Duration::from_secs(30),
                "delay should not exceed 30s, got: {delay:?}"
            );
        }
        assert_eq!(delays[5], Duration::from_secs(30));
        assert_eq!(delays[6], Duration::from_secs(30));
    }

    #[test]
    fn reconnect_backoff_returns_first_success() {
        let delays = backoff_delays(1);
        assert_eq!(delays.len(), 1);
        assert_eq!(delays[0], Duration::from_secs(1));
    }

    #[test]
    fn history_message_to_block_user() {
        let msg = serde_json::json!({
            "role": "user",
            "content": "Add a test",
            "timestamp": "2026-04-03T12:00:00Z"
        });
        let block = history_message_to_block(&msg);
        assert!(block.is_some());
        match block.unwrap() {
            Block::UserMessage { text, .. } => assert_eq!(text, "Add a test"),
            other => panic!("expected UserMessage, got {other:?}"),
        }
    }

    #[test]
    fn history_message_to_block_assistant() {
        let msg = serde_json::json!({
            "role": "assistant",
            "content": "I'll add a test for you."
        });
        let block = history_message_to_block(&msg);
        assert!(block.is_some());
        match block.unwrap() {
            Block::AgentText { text, .. } => assert_eq!(text, "I'll add a test for you."),
            other => panic!("expected AgentText, got {other:?}"),
        }
    }

    #[test]
    fn history_message_to_block_daemon_shape_user() {
        let msg = serde_json::json!({
            "content": "Add a test",
            "timestamp": "2026-04-03T12:00:00Z",
            "is_vessel_reply": false
        });
        let block = history_message_to_block(&msg);
        assert!(matches!(block, Some(Block::UserMessage { .. })));
    }

    #[test]
    fn history_message_to_block_daemon_shape_assistant() {
        let msg = serde_json::json!({
            "content": "I'll add a test for you.",
            "timestamp": "2026-04-03T12:00:00Z",
            "is_vessel_reply": true
        });
        let block = history_message_to_block(&msg);
        assert!(matches!(block, Some(Block::AgentText { .. })));
    }

    #[test]
    fn history_message_to_block_unknown_role() {
        let msg = serde_json::json!({
            "role": "narrator",
            "content": "something"
        });
        let block = history_message_to_block(&msg);
        assert!(block.is_none());
    }
}

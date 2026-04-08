//! LLM domain types for inference requests and responses.
//!
//! Pure domain vocabulary for LLM inference. These types have no I/O
//! dependencies and are used by both `exoskeleton-host` (which makes the
//! actual calls) and `exoskeleton-threads` (Sprint 6, which calls LLM for
//! thread execution). Placing them in `exoskeleton-core` keeps the dependency
//! DAG clean.

use serde::{Deserialize, Serialize};

fn is_false(v: &bool) -> bool {
    !*v
}

/// Which LLM backend to use for a given request.
///
/// The vessel operates with two classes of model:
/// - **Local:** fast, cheap, runs on-premises (Ollama, vLLM, LM Studio)
/// - **Frontier:** powerful, expensive, remote (Anthropic Claude, OpenAI GPT)
///
/// Sprint 9 adds escalation logic (local → frontier) based on uncertainty,
/// stakes, and consecutive failures. Sprint 4 lets the caller choose explicitly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LlmBackend {
    /// Local model (Ollama, vLLM, LM Studio, etc.)
    Local,
    /// Frontier model (Anthropic, OpenAI, etc.)
    Frontier,
}

/// Role of a message in the LLM conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LlmRole {
    /// System instruction (sets behavior/persona).
    System,
    /// User message (the prompt / compiled context).
    User,
    /// Assistant response (model output, for multi-turn).
    Assistant,
}

/// A content block in an LLM message or response.
///
/// Provider-agnostic representation of structured content. HTTP backends
/// translate to/from provider-specific wire formats.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    /// Plain text content.
    Text { text: String },
    /// A tool invocation requested by the model.
    ToolUse {
        /// API-issued call ID (opaque, returned verbatim in ToolResult).
        id: String,
        /// Tool name (e.g., "code.read", "update_plan").
        name: String,
        /// Tool input parameters as JSON.
        input: serde_json::Value,
    },
    /// Result of a tool invocation, sent back to the model.
    ToolResult {
        /// Must match the `id` from the corresponding ToolUse block.
        tool_use_id: String,
        /// Tool output (text or JSON string).
        content: String,
        /// Whether this result represents an error.
        #[serde(default, skip_serializing_if = "is_false")]
        is_error: bool,
    },
}

/// A tool definition sent to the LLM.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolDefinition {
    /// Tool name (e.g., "code.read", "introspect_tick_history").
    pub name: String,
    /// Human-readable description of what the tool does.
    pub description: String,
    /// JSON Schema describing the expected input parameters.
    pub input_schema: serde_json::Value,
}

/// One message in an LLM conversation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LlmMessage {
    /// Role of the message sender.
    pub role: LlmRole,
    /// Structured message content.
    pub content: Vec<ContentBlock>,
}

impl LlmMessage {
    /// Create a message containing a single text block.
    pub fn text(role: LlmRole, content: impl Into<String>) -> Self {
        Self {
            role,
            content: vec![ContentBlock::Text {
                text: content.into(),
            }],
        }
    }
}

/// Request to invoke an LLM.
///
/// This is the payload serialized into the `CognitivePayload.data` field
/// for `CognitiveTaskType::LlmCall` tasks. The CognitiveHandler deserializes
/// this, makes the HTTP call, and returns an `LlmResponse`.
///
/// The `backend` field indicates which model class to use. If `None`, the
/// handler uses the `LlmConfig.default_backend`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LlmRequest {
    /// Optional override: which backend to use. If `None`, uses config default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend: Option<LlmBackend>,
    /// System prompt (injected as a system message or parameter, depending on API).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    /// Conversation messages.
    pub messages: Vec<LlmMessage>,
    /// Maximum tokens to generate.
    pub max_output_tokens: u64,
    /// Sampling temperature (0.0 = deterministic, 1.0+ = creative).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    /// Stop sequences: the model stops generating when any of these appear.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stop_sequences: Vec<String>,
    /// Whether to request server-sent event streaming from the backend.
    /// When true, the backend emits incremental text deltas via callback.
    /// Default: false (complete response returned at once).
    #[serde(default, skip_serializing_if = "is_false")]
    pub stream: bool,
    /// Structured tool definitions exposed to the model.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<ToolDefinition>,
}

/// Why the LLM stopped generating.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    /// Model reached a natural end of response.
    EndTurn,
    /// Model hit the max_output_tokens limit.
    MaxTokens,
    /// Model encountered a stop sequence.
    StopSequence,
    /// Model wants to invoke one or more tools.
    ToolUse,
}

/// Response from an LLM invocation.
///
/// The handler constructs this from the raw API response, stores it as an
/// artifact (I3), and returns it as the handler output. The `LlmCallRecord`
/// in the TickRecord (Sprint 5) references the artifact by ID.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LlmResponse {
    /// Generated content blocks.
    pub content_blocks: Vec<ContentBlock>,
    /// Which model produced this response (e.g., "llama3.2:latest", "claude-sonnet-4-20250514").
    pub model: String,
    /// Input tokens consumed (prompt + system message).
    pub tokens_in: u64,
    /// Output tokens generated.
    pub tokens_out: u64,
    /// Wall-clock latency in milliseconds.
    pub latency_ms: u64,
    /// Why the model stopped generating.
    pub stop_reason: StopReason,
    /// Estimated cost in hundredths of a cent. `None` for local models (zero cost).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_estimate_cents: Option<f64>,
    /// Which backend was used.
    pub backend: LlmBackend,
}

impl LlmResponse {
    /// Concatenate all text content blocks.
    pub fn text(&self) -> String {
        self.content_blocks
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }

    /// Extract all ToolUse blocks.
    pub fn tool_use_blocks(&self) -> Vec<&ContentBlock> {
        self.content_blocks
            .iter()
            .filter(|block| matches!(block, ContentBlock::ToolUse { .. }))
            .collect()
    }

    /// Whether the response contains any ToolUse blocks.
    pub fn has_tool_use(&self) -> bool {
        self.content_blocks
            .iter()
            .any(|block| matches!(block, ContentBlock::ToolUse { .. }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── T-1: LLM Domain Types ──

    #[test]
    fn llm_backend_roundtrip() {
        for backend in [LlmBackend::Local, LlmBackend::Frontier] {
            let json = serde_json::to_string(&backend).unwrap();
            let parsed: LlmBackend = serde_json::from_str(&json).unwrap();
            assert_eq!(backend, parsed);
        }
    }

    #[test]
    fn llm_backend_snake_case() {
        assert_eq!(
            serde_json::to_string(&LlmBackend::Local).unwrap(),
            "\"local\""
        );
        assert_eq!(
            serde_json::to_string(&LlmBackend::Frontier).unwrap(),
            "\"frontier\""
        );
    }

    #[test]
    fn llm_role_roundtrip() {
        for role in [LlmRole::System, LlmRole::User, LlmRole::Assistant] {
            let json = serde_json::to_string(&role).unwrap();
            let parsed: LlmRole = serde_json::from_str(&json).unwrap();
            assert_eq!(role, parsed);
        }
    }

    #[test]
    fn content_block_text_roundtrip() {
        let block = ContentBlock::Text {
            text: "Hello, world!".into(),
        };
        let json = serde_json::to_string(&block).unwrap();
        assert!(json.contains("\"type\":\"text\""));
        let parsed: ContentBlock = serde_json::from_str(&json).unwrap();
        assert_eq!(block, parsed);
    }

    #[test]
    fn content_block_tool_use_roundtrip() {
        let block = ContentBlock::ToolUse {
            id: "call_123".into(),
            name: "code.read".into(),
            input: serde_json::json!({"file_path": "src/lib.rs"}),
        };
        let json = serde_json::to_string(&block).unwrap();
        assert!(json.contains("\"type\":\"tool_use\""));
        let parsed: ContentBlock = serde_json::from_str(&json).unwrap();
        assert_eq!(block, parsed);
    }

    #[test]
    fn content_block_tool_result_roundtrip() {
        let block = ContentBlock::ToolResult {
            tool_use_id: "call_123".into(),
            content: "file contents here".into(),
            is_error: false,
        };
        let json = serde_json::to_string(&block).unwrap();
        assert!(json.contains("\"type\":\"tool_result\""));
        let parsed: ContentBlock = serde_json::from_str(&json).unwrap();
        assert_eq!(block, parsed);
    }

    #[test]
    fn content_block_tool_result_error() {
        let block = ContentBlock::ToolResult {
            tool_use_id: "call_456".into(),
            content: "file not found".into(),
            is_error: true,
        };
        let json = serde_json::to_string(&block).unwrap();
        assert!(json.contains("\"is_error\":true"));
        let parsed: ContentBlock = serde_json::from_str(&json).unwrap();
        assert_eq!(block, parsed);
    }

    #[test]
    fn tool_definition_roundtrip() {
        let tool = ToolDefinition {
            name: "code.read".into(),
            description: "Read a file".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "file_path": {"type": "string"}
                },
                "required": ["file_path"]
            }),
        };
        let json = serde_json::to_string(&tool).unwrap();
        let parsed: ToolDefinition = serde_json::from_str(&json).unwrap();
        assert_eq!(tool.name, parsed.name);
        assert_eq!(tool.description, parsed.description);
    }

    #[test]
    fn llm_message_roundtrip() {
        let msg = LlmMessage::text(LlmRole::User, "Hello, world!");
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: LlmMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, parsed);
    }

    #[test]
    fn llm_message_text_convenience() {
        let msg = LlmMessage::text(LlmRole::User, "Hello");
        assert_eq!(msg.role, LlmRole::User);
        assert_eq!(msg.content.len(), 1);
        match &msg.content[0] {
            ContentBlock::Text { text } => assert_eq!(text, "Hello"),
            other => panic!("expected Text, got: {other:?}"),
        }
    }

    #[test]
    fn llm_message_mixed_content() {
        let msg = LlmMessage {
            role: LlmRole::User,
            content: vec![
                ContentBlock::Text {
                    text: "Here are the results:".into(),
                },
                ContentBlock::ToolResult {
                    tool_use_id: "call_1".into(),
                    content: "file contents".into(),
                    is_error: false,
                },
            ],
        };
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: LlmMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, parsed);
    }

    #[test]
    fn llm_request_roundtrip() {
        let request = LlmRequest {
            backend: Some(LlmBackend::Frontier),
            system_prompt: Some("You are a helpful assistant.".into()),
            messages: vec![LlmMessage::text(LlmRole::User, "What is 2+2?")],
            max_output_tokens: 1024,
            temperature: Some(0.7),
            stop_sequences: vec!["</answer>".into()],
            stream: false,
            tools: vec![ToolDefinition {
                name: "code.read".into(),
                description: "Read a file".into(),
                input_schema: serde_json::json!({"type": "object"}),
            }],
        };
        let json = serde_json::to_string(&request).unwrap();
        let parsed: LlmRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(request, parsed);
    }

    #[test]
    fn llm_request_minimal() {
        let request = LlmRequest {
            backend: None,
            system_prompt: None,
            messages: vec![LlmMessage::text(LlmRole::User, "Hi")],
            max_output_tokens: 256,
            temperature: None,
            stop_sequences: vec![],
            stream: false,
            tools: vec![],
        };
        let json = serde_json::to_string(&request).unwrap();
        // Optional fields should be absent
        assert!(!json.contains("backend"));
        assert!(!json.contains("system_prompt"));
        assert!(!json.contains("temperature"));
        assert!(!json.contains("stop_sequences"));
        assert!(!json.contains("stream"));
        assert!(!json.contains("tools"));
        let parsed: LlmRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(request, parsed);
    }

    #[test]
    fn llm_request_stream_field_default_false() {
        let json = r#"{"messages":[{"role":"user","content":[{"type":"text","text":"Hi"}]}],"max_output_tokens":100}"#;
        let parsed: LlmRequest = serde_json::from_str(json).unwrap();
        assert!(!parsed.stream);
        assert!(parsed.tools.is_empty());
    }

    #[test]
    fn llm_request_stream_true_serializes() {
        let request = LlmRequest {
            backend: None,
            system_prompt: None,
            messages: vec![LlmMessage::text(LlmRole::User, "Hi")],
            max_output_tokens: 100,
            temperature: None,
            stop_sequences: vec![],
            stream: true,
            tools: vec![],
        };
        let json = serde_json::to_string(&request).unwrap();
        assert!(
            json.contains("\"stream\":true"),
            "stream: true should be serialized"
        );
        let parsed: LlmRequest = serde_json::from_str(&json).unwrap();
        assert!(parsed.stream);
    }

    #[test]
    fn llm_request_stream_false_omitted_in_json() {
        let request = LlmRequest {
            backend: None,
            system_prompt: None,
            messages: vec![LlmMessage::text(LlmRole::User, "Hi")],
            max_output_tokens: 100,
            temperature: None,
            stop_sequences: vec![],
            stream: false,
            tools: vec![],
        };
        let json = serde_json::to_string(&request).unwrap();
        assert!(
            !json.contains("stream"),
            "stream: false should be omitted, got: {json}"
        );
    }

    #[test]
    fn llm_response_roundtrip() {
        let response = LlmResponse {
            content_blocks: vec![ContentBlock::Text {
                text: "The answer is 4.".into(),
            }],
            model: "claude-sonnet-4-20250514".into(),
            tokens_in: 50,
            tokens_out: 10,
            latency_ms: 1200,
            stop_reason: StopReason::EndTurn,
            cost_estimate_cents: Some(0.015),
            backend: LlmBackend::Frontier,
        };
        let json = serde_json::to_string(&response).unwrap();
        let parsed: LlmResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(response, parsed);
    }

    #[test]
    fn llm_response_no_cost() {
        let response = LlmResponse {
            content_blocks: vec![ContentBlock::Text {
                text: "Local model response.".into(),
            }],
            model: "llama3.2:latest".into(),
            tokens_in: 30,
            tokens_out: 20,
            latency_ms: 500,
            stop_reason: StopReason::EndTurn,
            cost_estimate_cents: None,
            backend: LlmBackend::Local,
        };
        let json = serde_json::to_string(&response).unwrap();
        assert!(!json.contains("cost_estimate_cents"));
        let parsed: LlmResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(response, parsed);
    }

    #[test]
    fn llm_response_text_concatenates_text_blocks() {
        let response = LlmResponse {
            content_blocks: vec![
                ContentBlock::Text {
                    text: "Hello".into(),
                },
                ContentBlock::ToolUse {
                    id: "call_1".into(),
                    name: "code.read".into(),
                    input: serde_json::json!({"path": "src/lib.rs"}),
                },
                ContentBlock::Text {
                    text: " world".into(),
                },
            ],
            model: "test-model".into(),
            tokens_in: 1,
            tokens_out: 2,
            latency_ms: 3,
            stop_reason: StopReason::ToolUse,
            cost_estimate_cents: None,
            backend: LlmBackend::Local,
        };
        assert_eq!(response.text(), "Hello world");
    }

    #[test]
    fn llm_response_tool_use_helpers_work() {
        let response = LlmResponse {
            content_blocks: vec![
                ContentBlock::Text {
                    text: "Thinking".into(),
                },
                ContentBlock::ToolUse {
                    id: "call_1".into(),
                    name: "code.read".into(),
                    input: serde_json::json!({"path": "src/lib.rs"}),
                },
            ],
            model: "test-model".into(),
            tokens_in: 1,
            tokens_out: 2,
            latency_ms: 3,
            stop_reason: StopReason::ToolUse,
            cost_estimate_cents: None,
            backend: LlmBackend::Local,
        };
        assert!(response.has_tool_use());
        assert_eq!(response.tool_use_blocks().len(), 1);
    }

    #[test]
    fn stop_reason_roundtrip() {
        for reason in [
            StopReason::EndTurn,
            StopReason::MaxTokens,
            StopReason::StopSequence,
            StopReason::ToolUse,
        ] {
            let json = serde_json::to_string(&reason).unwrap();
            let parsed: StopReason = serde_json::from_str(&json).unwrap();
            assert_eq!(reason, parsed);
        }
    }

    #[test]
    fn stop_reason_snake_case() {
        assert_eq!(
            serde_json::to_string(&StopReason::EndTurn).unwrap(),
            "\"end_turn\""
        );
        assert_eq!(
            serde_json::to_string(&StopReason::MaxTokens).unwrap(),
            "\"max_tokens\""
        );
        assert_eq!(
            serde_json::to_string(&StopReason::StopSequence).unwrap(),
            "\"stop_sequence\""
        );
        assert_eq!(
            serde_json::to_string(&StopReason::ToolUse).unwrap(),
            "\"tool_use\""
        );
    }
}

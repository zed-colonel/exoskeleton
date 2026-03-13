//! LLM domain types for inference requests and responses.
//!
//! Pure domain vocabulary for LLM inference. These types have no I/O
//! dependencies and are used by both `exoskeleton-host` (which makes the
//! actual calls) and `exoskeleton-threads` (Sprint 6, which calls LLM for
//! thread execution). Placing them in `exoskeleton-core` keeps the dependency
//! DAG clean.

use serde::{Deserialize, Serialize};

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

/// One message in an LLM conversation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LlmMessage {
    /// Role of the message sender.
    pub role: LlmRole,
    /// Text content of the message.
    pub content: String,
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
}

/// Response from an LLM invocation.
///
/// The handler constructs this from the raw API response, stores it as an
/// artifact (I3), and returns it as the handler output. The `LlmCallRecord`
/// in the TickRecord (Sprint 5) references the artifact by ID.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LlmResponse {
    /// Generated text content.
    pub content: String,
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
    fn llm_message_roundtrip() {
        let msg = LlmMessage {
            role: LlmRole::User,
            content: "Hello, world!".into(),
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
            messages: vec![LlmMessage {
                role: LlmRole::User,
                content: "What is 2+2?".into(),
            }],
            max_output_tokens: 1024,
            temperature: Some(0.7),
            stop_sequences: vec!["</answer>".into()],
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
            messages: vec![LlmMessage {
                role: LlmRole::User,
                content: "Hi".into(),
            }],
            max_output_tokens: 256,
            temperature: None,
            stop_sequences: vec![],
        };
        let json = serde_json::to_string(&request).unwrap();
        // Optional fields should be absent
        assert!(!json.contains("backend"));
        assert!(!json.contains("system_prompt"));
        assert!(!json.contains("temperature"));
        assert!(!json.contains("stop_sequences"));
        let parsed: LlmRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(request, parsed);
    }

    #[test]
    fn llm_response_roundtrip() {
        let response = LlmResponse {
            content: "The answer is 4.".into(),
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
            content: "Local model response.".into(),
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
    fn stop_reason_roundtrip() {
        for reason in [
            StopReason::EndTurn,
            StopReason::MaxTokens,
            StopReason::StopSequence,
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
    }
}

//! Mock LLM backend for testing.
//!
//! Returns configurable canned responses. Tracks call count for assertions.
//! Used by handler tests and integration tests to avoid actual HTTP calls.

use std::sync::atomic::{AtomicU64, Ordering};

use actionqueue_executor_local::CancellationToken;
use exoskeleton_core::llm::{ContentBlock, LlmResponse};
use exoskeleton_core::ExoError;

use super::http::LlmHttpBackend;

/// Mock LLM backend for testing.
///
/// Returns a configurable canned response. Tracks call count for assertions.
pub struct MockLlmBackend {
    response: LlmResponse,
    call_count: AtomicU64,
    should_fail: bool,
    failure_message: String,
}

impl std::fmt::Debug for MockLlmBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MockLlmBackend")
            .field("call_count", &self.call_count.load(Ordering::SeqCst))
            .field("should_fail", &self.should_fail)
            .finish()
    }
}

impl MockLlmBackend {
    /// Create a mock that returns the given response.
    pub fn new(response: LlmResponse) -> Self {
        Self {
            response,
            call_count: AtomicU64::new(0),
            should_fail: false,
            failure_message: String::new(),
        }
    }

    /// Create a mock that always fails with the given message.
    pub fn failing(message: impl Into<String>) -> Self {
        Self {
            response: default_mock_response(),
            call_count: AtomicU64::new(0),
            should_fail: true,
            failure_message: message.into(),
        }
    }

    /// Number of times `call()` has been invoked.
    pub fn call_count(&self) -> u64 {
        self.call_count.load(Ordering::SeqCst)
    }
}

impl LlmHttpBackend for MockLlmBackend {
    fn call(
        &self,
        _client: &reqwest::Client,
        _request: &exoskeleton_core::llm::LlmRequest,
        _cancellation: &CancellationToken,
    ) -> Result<LlmResponse, ExoError> {
        self.call_count.fetch_add(1, Ordering::SeqCst);
        if self.should_fail {
            Err(ExoError::LlmInvocation(self.failure_message.clone()))
        } else {
            Ok(self.response.clone())
        }
    }

    fn call_streaming(
        &self,
        client: &reqwest::Client,
        request: &exoskeleton_core::llm::LlmRequest,
        cancellation: &CancellationToken,
        on_delta: &dyn Fn(&str),
    ) -> Result<LlmResponse, ExoError> {
        let _ = on_delta;
        self.call(client, request, cancellation)
    }
}

/// Create a default mock response for testing.
pub fn default_mock_response() -> LlmResponse {
    use exoskeleton_core::llm::{LlmBackend, StopReason};

    LlmResponse {
        content_blocks: vec![ContentBlock::Text {
            text: "Mock LLM response.".into(),
        }],
        model: "mock-model".into(),
        tokens_in: 10,
        tokens_out: 5,
        latency_ms: 100,
        stop_reason: StopReason::EndTurn,
        cost_estimate_cents: None,
        backend: LlmBackend::Local,
    }
}

/// Create a mock response with a single tool_use block.
pub fn mock_tool_use_response(
    call_id: &str,
    tool_name: &str,
    input: serde_json::Value,
) -> LlmResponse {
    use exoskeleton_core::llm::{LlmBackend, StopReason};

    LlmResponse {
        content_blocks: vec![ContentBlock::ToolUse {
            id: call_id.into(),
            name: tool_name.into(),
            input,
        }],
        model: "mock-model".into(),
        tokens_in: 10,
        tokens_out: 5,
        latency_ms: 100,
        stop_reason: StopReason::ToolUse,
        cost_estimate_cents: None,
        backend: LlmBackend::Local,
    }
}

/// Create a mock response with text and tool_use blocks.
pub fn mock_text_and_tool_use_response(
    text: &str,
    call_id: &str,
    tool_name: &str,
    input: serde_json::Value,
) -> LlmResponse {
    use exoskeleton_core::llm::{LlmBackend, StopReason};

    LlmResponse {
        content_blocks: vec![
            ContentBlock::Text { text: text.into() },
            ContentBlock::ToolUse {
                id: call_id.into(),
                name: tool_name.into(),
                input,
            },
        ],
        model: "mock-model".into(),
        tokens_in: 10,
        tokens_out: 5,
        latency_ms: 100,
        stop_reason: StopReason::ToolUse,
        cost_estimate_cents: None,
        backend: LlmBackend::Local,
    }
}

/// Create a mock text-only response (end turn, no tool calls).
pub fn mock_end_turn_response(text: &str) -> LlmResponse {
    use exoskeleton_core::llm::{LlmBackend, StopReason};

    LlmResponse {
        content_blocks: vec![ContentBlock::Text { text: text.into() }],
        model: "mock-model".into(),
        tokens_in: 10,
        tokens_out: 5,
        latency_ms: 100,
        stop_reason: StopReason::EndTurn,
        cost_estimate_cents: None,
        backend: LlmBackend::Local,
    }
}

/// Mock LLM backend that returns a sequence of responses.
///
/// Returns the next response in the sequence on each call. Wraps around if
/// calls exceed the sequence length.
pub struct MockSequenceLlmBackend {
    responses: Vec<LlmResponse>,
    call_count: AtomicU64,
}

impl MockSequenceLlmBackend {
    /// Create a mock that returns responses in sequence.
    pub fn new(responses: Vec<LlmResponse>) -> Self {
        assert!(!responses.is_empty(), "responses must not be empty");
        Self {
            responses,
            call_count: AtomicU64::new(0),
        }
    }

    /// Number of times `call()` has been invoked.
    pub fn call_count(&self) -> u64 {
        self.call_count.load(Ordering::SeqCst)
    }
}

impl LlmHttpBackend for MockSequenceLlmBackend {
    fn call(
        &self,
        _client: &reqwest::Client,
        _request: &exoskeleton_core::llm::LlmRequest,
        _cancellation: &CancellationToken,
    ) -> Result<LlmResponse, ExoError> {
        let idx = self.call_count.fetch_add(1, Ordering::SeqCst) as usize;
        let response = &self.responses[idx % self.responses.len()];
        Ok(response.clone())
    }

    fn call_streaming(
        &self,
        client: &reqwest::Client,
        request: &exoskeleton_core::llm::LlmRequest,
        cancellation: &CancellationToken,
        on_delta: &dyn Fn(&str),
    ) -> Result<LlmResponse, ExoError> {
        let _ = on_delta;
        self.call(client, request, cancellation)
    }
}

#[cfg(test)]
mod tests {
    use exoskeleton_core::llm::{
        ContentBlock, LlmBackend, LlmMessage, LlmRequest, LlmRole, StopReason,
    };

    use super::*;

    fn test_request() -> LlmRequest {
        LlmRequest {
            backend: None,
            system_prompt: None,
            messages: vec![LlmMessage::text(LlmRole::User, "Hi")],
            max_output_tokens: 100,
            temperature: None,
            stop_sequences: vec![],
            stream: false,
            tools: vec![],
        }
    }

    fn test_cancellation_token() -> CancellationToken {
        CancellationToken::new()
    }

    #[test]
    fn mock_backend_returns_canned_response() {
        let response = LlmResponse {
            content_blocks: vec![ContentBlock::Text {
                text: "Custom response".into(),
            }],
            model: "test-model".into(),
            tokens_in: 20,
            tokens_out: 10,
            latency_ms: 50,
            stop_reason: StopReason::EndTurn,
            cost_estimate_cents: None,
            backend: LlmBackend::Local,
        };
        let mock = MockLlmBackend::new(response.clone());
        let client = reqwest::Client::new();
        let token = test_cancellation_token();
        let result = mock.call(&client, &test_request(), &token).unwrap();
        assert_eq!(result.text(), "Custom response");
        assert_eq!(result.model, "test-model");
    }

    #[test]
    fn mock_backend_tracks_call_count() {
        let mock = MockLlmBackend::new(default_mock_response());
        let client = reqwest::Client::new();
        let token = test_cancellation_token();
        assert_eq!(mock.call_count(), 0);
        mock.call(&client, &test_request(), &token).unwrap();
        assert_eq!(mock.call_count(), 1);
        mock.call(&client, &test_request(), &token).unwrap();
        assert_eq!(mock.call_count(), 2);
    }

    #[test]
    fn mock_tool_use_response_has_tool_use() {
        let resp = mock_tool_use_response(
            "call_1",
            "code.read",
            serde_json::json!({"file_path": "src/lib.rs"}),
        );
        assert!(resp.has_tool_use());
        assert_eq!(resp.stop_reason, StopReason::ToolUse);
        assert_eq!(resp.tool_use_blocks().len(), 1);
    }

    #[test]
    fn mock_end_turn_no_tool_use() {
        let resp = mock_end_turn_response("Done.");
        assert!(!resp.has_tool_use());
        assert_eq!(resp.stop_reason, StopReason::EndTurn);
        assert_eq!(resp.text(), "Done.");
    }

    #[test]
    fn mock_text_and_tool_use_has_both() {
        let resp = mock_text_and_tool_use_response(
            "Let me read that.",
            "call_1",
            "code.read",
            serde_json::json!({}),
        );
        assert!(resp.has_tool_use());
        assert_eq!(resp.text(), "Let me read that.");
        assert_eq!(resp.tool_use_blocks().len(), 1);
    }

    #[test]
    fn mock_backend_can_fail() {
        let mock = MockLlmBackend::failing("test error 429");
        let client = reqwest::Client::new();
        let token = test_cancellation_token();
        let result = mock.call(&client, &test_request(), &token);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("test error 429"));
        assert_eq!(mock.call_count(), 1);
    }
}

//! HTTP backends for LLM API calls.
//!
//! Each backend handles the wire protocol for a specific LLM API:
//! request serialization, HTTP call, response parsing, and error classification.

use std::fmt;
use std::time::Instant;

use actionqueue_executor_local::CancellationToken;
use exoskeleton_core::llm::{
    ContentBlock, LlmBackend, LlmRequest, LlmResponse, LlmRole, StopReason, ToolDefinition,
};
use exoskeleton_core::ExoError;
use serde::{Deserialize, Serialize};

/// Trait for LLM HTTP backends.
///
/// Each implementation handles the wire protocol for a specific LLM API:
/// request serialization, HTTP call, response parsing, and error classification.
///
/// Implementations must be `Send + Sync` (shared across handler invocations).
pub trait LlmHttpBackend: Send + Sync {
    /// Send a request to the LLM API and return the response.
    ///
    /// # Arguments
    /// - `client`: shared reqwest HTTP client (connection pooling)
    /// - `request`: the LLM request to send
    /// - `cancellation`: cooperative cancellation token (poll `is_cancelled()`)
    ///
    /// # Errors
    /// - `ExoError::LlmInvocation` for all LLM-related failures
    fn call(
        &self,
        client: &reqwest::Client,
        request: &LlmRequest,
        cancellation: &CancellationToken,
    ) -> Result<LlmResponse, ExoError>;

    /// Send a streaming request, invoking `on_delta` for each text chunk.
    ///
    /// The default implementation falls back to `call()` (non-streaming).
    fn call_streaming(
        &self,
        client: &reqwest::Client,
        request: &LlmRequest,
        cancellation: &CancellationToken,
        on_delta: &dyn Fn(&str),
    ) -> Result<LlmResponse, ExoError> {
        let _ = on_delta;
        self.call(client, request, cancellation)
    }
}

// ── Sync HTTP helper ──

/// Execute an async future from the sync handler context.
///
/// The handler runs on a blocking thread managed by the AQ dispatch loop.
/// We use `Handle::current().block_on()` to bridge async reqwest calls.
fn block_on<F: std::future::Future>(fut: F) -> F::Output {
    tokio::runtime::Handle::current().block_on(fut)
}

fn is_false(v: &bool) -> bool {
    !*v
}

fn effective_stream(request: &LlmRequest) -> bool {
    request.stream && request.tools.is_empty()
}

fn encode_anthropic_tool_name(name: &str) -> String {
    let mut encoded = String::from("tool_");
    for byte in name.as_bytes() {
        use std::fmt::Write as _;
        let _ = write!(&mut encoded, "{byte:02x}");
    }
    encoded
}

fn decode_anthropic_tool_name(name: &str) -> String {
    let Some(hex) = name.strip_prefix("tool_") else {
        return name.to_string();
    };
    if hex.len() % 2 != 0 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return name.to_string();
    }

    let mut bytes = Vec::with_capacity(hex.len() / 2);
    for i in (0..hex.len()).step_by(2) {
        let pair = &hex[i..i + 2];
        let Ok(byte) = u8::from_str_radix(pair, 16) else {
            return name.to_string();
        };
        bytes.push(byte);
    }

    String::from_utf8(bytes).unwrap_or_else(|_| name.to_string())
}

fn request_text_len(request: &LlmRequest) -> usize {
    request.messages.iter().map(message_text_len).sum::<usize>()
        + request.system_prompt.as_deref().map_or(0, str::len)
}

fn message_text_len(message: &exoskeleton_core::llm::LlmMessage) -> usize {
    message
        .content
        .iter()
        .map(|block| match block {
            ContentBlock::Text { text } => text.len(),
            ContentBlock::ToolResult { content, .. } => content.len(),
            ContentBlock::ToolUse { input, .. } => input.to_string().len(),
        })
        .sum()
}

// ── SSE Streaming Helpers ──

/// Parsed event from an OpenAI-compatible SSE stream.
#[derive(Debug, Clone, PartialEq)]
enum OpenAiSseEvent {
    Delta(String),
    Final {
        finish_reason: Option<String>,
        prompt_tokens: Option<u64>,
        completion_tokens: Option<u64>,
    },
    Done,
    Skip,
}

/// Parsed event from an Anthropic SSE stream.
#[derive(Debug, Clone, PartialEq)]
enum AnthropicSseEvent {
    MessageStart {
        model: Option<String>,
        input_tokens: Option<u64>,
    },
    TextDelta(String),
    MessageDelta {
        stop_reason: Option<String>,
        output_tokens: Option<u64>,
    },
    Done,
    Skip,
}

#[derive(Debug, Deserialize)]
struct OpenAiSseChunk {
    choices: Vec<OpenAiSseChoice>,
    #[serde(default)]
    usage: Option<OpenAiUsage>,
}

#[derive(Debug, Deserialize)]
struct OpenAiSseChoice {
    #[serde(default)]
    delta: OpenAiSseDelta,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct OpenAiSseDelta {
    #[serde(default)]
    content: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AnthropicSseContentDelta {
    delta: AnthropicSseTextDelta,
}

#[derive(Debug, Deserialize)]
struct AnthropicSseTextDelta {
    #[serde(default)]
    text: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AnthropicSseMessageDelta {
    delta: AnthropicSseMessageDeltaInner,
    #[serde(default)]
    usage: Option<AnthropicSseOutputUsage>,
}

#[derive(Debug, Deserialize)]
struct AnthropicSseMessageDeltaInner {
    #[serde(default)]
    stop_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AnthropicSseOutputUsage {
    output_tokens: u64,
}

#[derive(Debug, Deserialize)]
struct AnthropicSseMessageStart {
    message: AnthropicSseMessageStartInner,
}

#[derive(Debug, Deserialize)]
struct AnthropicSseMessageStartInner {
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    usage: Option<AnthropicSseStartUsage>,
}

#[derive(Debug, Deserialize)]
struct AnthropicSseStartUsage {
    input_tokens: u64,
}

fn parse_openai_sse_line(line: &str) -> OpenAiSseEvent {
    let line = line.trim();
    if line.is_empty() || line.starts_with(':') {
        return OpenAiSseEvent::Skip;
    }
    let data = match line.strip_prefix("data:") {
        Some(d) => d.trim(),
        None => return OpenAiSseEvent::Skip,
    };
    if data == "[DONE]" {
        return OpenAiSseEvent::Done;
    }
    let chunk: OpenAiSseChunk = match serde_json::from_str(data) {
        Ok(c) => c,
        Err(_) => return OpenAiSseEvent::Skip,
    };
    let choice = match chunk.choices.first() {
        Some(c) => c,
        None => return OpenAiSseEvent::Skip,
    };
    if let Some(ref content) = choice.delta.content {
        if !content.is_empty() {
            return OpenAiSseEvent::Delta(content.clone());
        }
    }
    if choice.finish_reason.is_some() || chunk.usage.is_some() {
        let (prompt_tokens, completion_tokens) = chunk.usage.map_or((None, None), |u| {
            (Some(u.prompt_tokens), Some(u.completion_tokens))
        });
        return OpenAiSseEvent::Final {
            finish_reason: choice.finish_reason.clone(),
            prompt_tokens,
            completion_tokens,
        };
    }
    OpenAiSseEvent::Skip
}

fn parse_anthropic_sse_event(event_type: &str, data: &str) -> AnthropicSseEvent {
    match event_type {
        "content_block_delta" => match serde_json::from_str::<AnthropicSseContentDelta>(data) {
            Ok(parsed) => match parsed.delta.text {
                Some(text) if !text.is_empty() => AnthropicSseEvent::TextDelta(text),
                _ => AnthropicSseEvent::Skip,
            },
            Err(_) => AnthropicSseEvent::Skip,
        },
        "message_delta" => match serde_json::from_str::<AnthropicSseMessageDelta>(data) {
            Ok(parsed) => AnthropicSseEvent::MessageDelta {
                stop_reason: parsed.delta.stop_reason,
                output_tokens: parsed.usage.map(|u| u.output_tokens),
            },
            Err(_) => AnthropicSseEvent::Skip,
        },
        "message_start" => match serde_json::from_str::<AnthropicSseMessageStart>(data) {
            Ok(parsed) => AnthropicSseEvent::MessageStart {
                model: parsed.message.model,
                input_tokens: parsed.message.usage.map(|u| u.input_tokens),
            },
            Err(_) => AnthropicSseEvent::Skip,
        },
        "message_stop" => AnthropicSseEvent::Done,
        _ => AnthropicSseEvent::Skip,
    }
}

// ── OpenAI-Compatible Backend ──

/// HTTP backend for OpenAI-compatible Chat Completions API.
///
/// Works with: OpenAI, Ollama (/v1/chat/completions), vLLM, LM Studio,
/// llama.cpp server, and any other server implementing the OpenAI format.
///
/// Endpoint: `{base_url}/v1/chat/completions`
pub struct OpenAiCompatBackend {
    base_url: String,
    model: String,
    api_key: Option<String>,
}

impl fmt::Debug for OpenAiCompatBackend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OpenAiCompatBackend")
            .field("base_url", &self.base_url)
            .field("model", &self.model)
            .field("api_key", &"[REDACTED]")
            .finish()
    }
}

impl OpenAiCompatBackend {
    /// Create a new OpenAI-compatible backend.
    pub fn new(base_url: String, model: String, api_key: Option<String>) -> Self {
        Self {
            base_url,
            model,
            api_key,
        }
    }

    fn build_request_body(&self, request: &LlmRequest) -> OpenAiRequest {
        let mut messages = Vec::new();

        // System prompt becomes a system role message
        if let Some(ref system) = request.system_prompt {
            messages.push(OpenAiMessage {
                role: "system".into(),
                content: Some(system.clone()),
                tool_calls: None,
                tool_call_id: None,
            });
        }

        // User/assistant/tool messages
        for msg in &request.messages {
            append_openai_messages(&mut messages, msg);
        }

        OpenAiRequest {
            model: self.model.clone(),
            messages,
            max_completion_tokens: request.max_output_tokens,
            temperature: request.temperature,
            stop: if request.stop_sequences.is_empty() {
                None
            } else {
                Some(request.stop_sequences.clone())
            },
            stream: effective_stream(request),
            tools: if request.tools.is_empty() {
                None
            } else {
                Some(
                    request
                        .tools
                        .iter()
                        .map(|tool| OpenAiToolDefinition {
                            r#type: "function".into(),
                            function: OpenAiFunctionDefinition {
                                name: tool.name.clone(),
                                description: tool.description.clone(),
                                parameters: tool.input_schema.clone(),
                            },
                        })
                        .collect(),
                )
            },
        }
    }
}

#[derive(Debug, Serialize)]
struct OpenAiRequest {
    model: String,
    messages: Vec<OpenAiMessage>,
    /// Newer OpenAI models (o1, o3, gpt-4.1+) require `max_completion_tokens`
    /// instead of the legacy `max_tokens`. Using the new name is backwards-
    /// compatible with older models that accept both.
    max_completion_tokens: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stop: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "is_false")]
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<OpenAiToolDefinition>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct OpenAiMessage {
    role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<OpenAiToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct OpenAiToolDefinition {
    r#type: String,
    function: OpenAiFunctionDefinition,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct OpenAiFunctionDefinition {
    name: String,
    description: String,
    parameters: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct OpenAiToolCall {
    /// Call ID. Optional because some backends (Ollama native) may omit it.
    /// When absent, a synthetic ID is generated during response parsing.
    #[serde(default)]
    id: Option<String>,
    r#type: String,
    function: OpenAiFunctionCall,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct OpenAiFunctionCall {
    name: String,
    arguments: String,
}

#[derive(Debug, Deserialize)]
struct OpenAiResponse {
    choices: Vec<OpenAiChoice>,
    #[serde(default)]
    usage: Option<OpenAiUsage>,
    model: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OpenAiChoice {
    message: OpenAiMessage,
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OpenAiUsage {
    prompt_tokens: u64,
    completion_tokens: u64,
}

fn append_openai_messages(
    messages: &mut Vec<OpenAiMessage>,
    msg: &exoskeleton_core::llm::LlmMessage,
) {
    let role = match msg.role {
        LlmRole::System => "system",
        LlmRole::User => "user",
        LlmRole::Assistant => "assistant",
    };

    let text = msg
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<String>();

    let tool_calls = msg
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::ToolUse { id, name, input } => Some(OpenAiToolCall {
                id: Some(id.clone()),
                r#type: "function".into(),
                function: OpenAiFunctionCall {
                    name: name.clone(),
                    arguments: serde_json::to_string(input).unwrap_or_else(|_| "{}".into()),
                },
            }),
            _ => None,
        })
        .collect::<Vec<_>>();

    if !text.is_empty() || tool_calls.is_empty() {
        messages.push(OpenAiMessage {
            role: role.into(),
            content: if text.is_empty() { None } else { Some(text) },
            tool_calls: if tool_calls.is_empty() {
                None
            } else {
                Some(tool_calls)
            },
            tool_call_id: None,
        });
    } else if !tool_calls.is_empty() {
        messages.push(OpenAiMessage {
            role: role.into(),
            content: None,
            tool_calls: Some(tool_calls),
            tool_call_id: None,
        });
    }

    for block in &msg.content {
        if let ContentBlock::ToolResult {
            tool_use_id,
            content,
            ..
        } = block
        {
            messages.push(OpenAiMessage {
                role: "tool".into(),
                content: Some(content.clone()),
                tool_calls: None,
                tool_call_id: Some(tool_use_id.clone()),
            });
        }
    }
}

fn parse_openai_response_blocks(message: OpenAiMessage) -> Result<Vec<ContentBlock>, ExoError> {
    let mut blocks = Vec::new();
    if let Some(content) = message.content {
        if !content.is_empty() {
            blocks.push(ContentBlock::Text { text: content });
        }
    }
    if let Some(tool_calls) = message.tool_calls {
        for (i, call) in tool_calls.into_iter().enumerate() {
            let input = serde_json::from_str(&call.function.arguments).map_err(|e| {
                ExoError::LlmInvocation(format!(
                    "malformed tool call arguments for '{}': {e}",
                    call.function.name
                ))
            })?;
            // Use the API-provided call ID, or generate a synthetic one
            // for backends (e.g., some Ollama versions) that omit it.
            let id = call
                .id
                .unwrap_or_else(|| format!("synthetic_call_{i}"));
            blocks.push(ContentBlock::ToolUse {
                id,
                name: call.function.name,
                input,
            });
        }
    }
    Ok(blocks)
}

impl LlmHttpBackend for OpenAiCompatBackend {
    fn call(
        &self,
        client: &reqwest::Client,
        request: &LlmRequest,
        cancellation: &CancellationToken,
    ) -> Result<LlmResponse, ExoError> {
        if cancellation.is_cancelled() {
            return Err(ExoError::LlmInvocation("cancelled before HTTP call".into()));
        }

        let body = self.build_request_body(request);
        let url = format!(
            "{}/v1/chat/completions",
            self.base_url.trim_end_matches('/')
        );

        let start = Instant::now();

        let mut req = client.post(&url).json(&body);
        if let Some(ref key) = self.api_key {
            req = req.bearer_auth(key);
        }

        let response = block_on(async { req.send().await }).map_err(|e| {
            if e.is_timeout() {
                ExoError::LlmInvocation(format!("timeout: {e}"))
            } else if e.is_connect() {
                ExoError::LlmInvocation(format!("connection refused: {e}"))
            } else {
                ExoError::LlmInvocation(format!("HTTP error: {e}"))
            }
        })?;

        let latency_ms = start.elapsed().as_millis() as u64;
        let status = response.status();

        if !status.is_success() {
            let body_text = block_on(response.text()).unwrap_or_default();
            return Err(ExoError::LlmInvocation(format!(
                "{}: {}",
                status.as_u16(),
                body_text
            )));
        }

        let parsed: OpenAiResponse = block_on(response.json())
            .map_err(|e| ExoError::LlmInvocation(format!("malformed response body: {e}")))?;

        let choice = parsed
            .choices
            .into_iter()
            .next()
            .ok_or_else(|| ExoError::LlmInvocation("empty choices array".into()))?;
        let content_blocks = parse_openai_response_blocks(choice.message.clone())?;

        let stop_reason = match choice.finish_reason.as_deref() {
            Some("stop") => StopReason::EndTurn,
            Some("length") => StopReason::MaxTokens,
            Some("stop_sequence") => StopReason::StopSequence,
            Some("tool_calls") => StopReason::ToolUse,
            _ => StopReason::EndTurn,
        };

        let (tokens_in, tokens_out) = match parsed.usage {
            Some(usage) => (usage.prompt_tokens, usage.completion_tokens),
            None => {
                // Estimate using chars/4 heuristic
                let in_chars = request_text_len(request);
                let out_chars: usize = content_blocks
                    .iter()
                    .map(|block| match block {
                        ContentBlock::Text { text } => text.len(),
                        ContentBlock::ToolUse { input, .. } => input.to_string().len(),
                        ContentBlock::ToolResult { content, .. } => content.len(),
                    })
                    .sum();
                ((in_chars / 4) as u64, (out_chars / 4) as u64)
            }
        };

        // Determine cost (local models have no cost)
        let cost_estimate_cents = self
            .api_key
            .as_ref()
            .and_then(|_| estimate_openai_cost(&self.model, tokens_in, tokens_out));

        Ok(LlmResponse {
            content_blocks,
            model: parsed.model.unwrap_or_else(|| self.model.clone()),
            tokens_in,
            tokens_out,
            latency_ms,
            stop_reason,
            cost_estimate_cents,
            backend: if self.api_key.is_some() {
                LlmBackend::Frontier
            } else {
                LlmBackend::Local
            },
        })
    }

    fn call_streaming(
        &self,
        client: &reqwest::Client,
        request: &LlmRequest,
        cancellation: &CancellationToken,
        on_delta: &dyn Fn(&str),
    ) -> Result<LlmResponse, ExoError> {
        if !request.stream {
            return self.call(client, request, cancellation);
        }
        if !request.tools.is_empty() {
            return self.call(client, request, cancellation);
        }
        if cancellation.is_cancelled() {
            return Err(ExoError::LlmInvocation("cancelled before HTTP call".into()));
        }

        let body = self.build_request_body(request);
        let url = format!(
            "{}/v1/chat/completions",
            self.base_url.trim_end_matches('/')
        );

        let mut req = client.post(&url).json(&body);
        if let Some(ref key) = self.api_key {
            req = req.bearer_auth(key);
        }

        let response = block_on(async { req.send().await }).map_err(|e| {
            if e.is_timeout() {
                ExoError::LlmInvocation(format!("timeout: {e}"))
            } else if e.is_connect() {
                ExoError::LlmInvocation(format!("connection refused: {e}"))
            } else {
                ExoError::LlmInvocation(format!("HTTP error: {e}"))
            }
        })?;

        let status = response.status();
        if !status.is_success() {
            let body_text = block_on(response.text()).unwrap_or_default();
            return Err(ExoError::LlmInvocation(format!(
                "{}: {}",
                status.as_u16(),
                body_text
            )));
        }

        let mut content_buffer = String::new();
        let mut finish_reason: Option<String> = None;
        let mut tokens_in = 0;
        let mut tokens_out = 0;

        block_on(async {
            use futures_util::StreamExt;

            let mut stream = response.bytes_stream();
            let mut line_buffer = String::new();

            while let Some(chunk_result) = stream.next().await {
                if cancellation.is_cancelled() {
                    return Err(ExoError::LlmInvocation("cancelled during streaming".into()));
                }

                let chunk = chunk_result
                    .map_err(|e| ExoError::LlmInvocation(format!("stream read error: {e}")))?;
                let text = String::from_utf8_lossy(&chunk);
                line_buffer.push_str(&text);

                let mut stream_done = false;
                while let Some(newline_pos) = line_buffer.find('\n') {
                    let line = line_buffer[..newline_pos].to_string();
                    line_buffer = line_buffer[newline_pos + 1..].to_string();

                    match parse_openai_sse_line(&line) {
                        OpenAiSseEvent::Delta(text) => {
                            content_buffer.push_str(&text);
                            on_delta(&text);
                        }
                        OpenAiSseEvent::Final {
                            finish_reason: fr,
                            prompt_tokens,
                            completion_tokens,
                        } => {
                            finish_reason = fr;
                            if let Some(t) = prompt_tokens {
                                tokens_in = t;
                            }
                            if let Some(t) = completion_tokens {
                                tokens_out = t;
                            }
                        }
                        OpenAiSseEvent::Done => {
                            stream_done = true;
                            break;
                        }
                        OpenAiSseEvent::Skip => {}
                    }
                }

                if stream_done {
                    break;
                }
            }
            Ok(())
        })?;

        let stop_reason = match finish_reason.as_deref() {
            Some("length") => StopReason::MaxTokens,
            Some("stop_sequence") => StopReason::StopSequence,
            Some("tool_calls") => StopReason::ToolUse,
            _ => StopReason::EndTurn,
        };

        if tokens_in == 0 && tokens_out == 0 {
            let in_chars = request_text_len(request);
            tokens_in = (in_chars / 4) as u64;
            tokens_out = (content_buffer.len() / 4) as u64;
        }

        let cost_estimate_cents = self
            .api_key
            .as_ref()
            .and_then(|_| estimate_openai_cost(&self.model, tokens_in, tokens_out));

        Ok(LlmResponse {
            content_blocks: vec![ContentBlock::Text {
                text: content_buffer,
            }],
            model: self.model.clone(),
            tokens_in,
            tokens_out,
            latency_ms: 0,
            stop_reason,
            cost_estimate_cents,
            backend: if self.api_key.is_some() {
                LlmBackend::Frontier
            } else {
                LlmBackend::Local
            },
        })
    }
}

// ── Anthropic Backend ──

/// HTTP backend for Anthropic Messages API.
///
/// Endpoint: `{base_url}/v1/messages` (default: `https://api.anthropic.com`)
///
/// Requires headers:
/// - `x-api-key: {key}`
/// - `anthropic-version: 2023-06-01`
pub struct AnthropicBackend {
    base_url: String,
    model: String,
    api_key: String,
}

impl fmt::Debug for AnthropicBackend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AnthropicBackend")
            .field("base_url", &self.base_url)
            .field("model", &self.model)
            .field("api_key", &"[REDACTED]")
            .finish()
    }
}

impl AnthropicBackend {
    /// Create a new Anthropic backend.
    pub fn new(base_url: String, model: String, api_key: String) -> Self {
        Self {
            base_url,
            model,
            api_key,
        }
    }

    fn build_request_body(&self, request: &LlmRequest) -> AnthropicRequest {
        let messages: Vec<AnthropicMessage> = request
            .messages
            .iter()
            .map(|msg| {
                let role = match msg.role {
                    LlmRole::System => "user", // Anthropic doesn't have system role in messages
                    LlmRole::User => "user",
                    LlmRole::Assistant => "assistant",
                };
                AnthropicMessage {
                    role: role.into(),
                    content: anthropic_content_from_blocks(&msg.content),
                }
            })
            .collect();

        AnthropicRequest {
            model: self.model.clone(),
            max_tokens: request.max_output_tokens,
            system: request.system_prompt.clone(),
            messages,
            temperature: request.temperature,
            stop_sequences: if request.stop_sequences.is_empty() {
                None
            } else {
                Some(request.stop_sequences.clone())
            },
            stream: effective_stream(request),
            tools: if request.tools.is_empty() {
                None
            } else {
                Some(
                    request
                        .tools
                        .iter()
                        .map(|tool| AnthropicToolDefinition {
                            name: encode_anthropic_tool_name(&tool.name),
                            description: tool.description.clone(),
                            input_schema: tool.input_schema.clone(),
                        })
                        .collect(),
                )
            },
        }
    }
}

#[derive(Debug, Serialize)]
struct AnthropicRequest {
    model: String,
    max_tokens: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<String>,
    messages: Vec<AnthropicMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stop_sequences: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "is_false")]
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<AnthropicToolDefinition>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AnthropicMessage {
    role: String,
    content: AnthropicContent,
}

#[derive(Debug, Clone, Serialize)]
struct AnthropicToolDefinition {
    name: String,
    description: String,
    input_schema: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
enum AnthropicContent {
    Text(String),
    Blocks(Vec<AnthropicContentBlock>),
}

#[derive(Debug, Deserialize)]
struct AnthropicResponse {
    content: Vec<AnthropicContentBlock>,
    model: String,
    stop_reason: Option<String>,
    usage: AnthropicUsage,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum AnthropicContentBlock {
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    ToolResult {
        tool_use_id: String,
        content: String,
        #[serde(default, skip_serializing_if = "is_false")]
        is_error: bool,
    },
}

fn anthropic_content_from_blocks(blocks: &[ContentBlock]) -> AnthropicContent {
    let all_text = blocks
        .iter()
        .all(|block| matches!(block, ContentBlock::Text { .. }));
    if all_text {
        let text = blocks
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<String>();
        AnthropicContent::Text(text)
    } else {
        AnthropicContent::Blocks(
            blocks
                .iter()
                .map(|block| match block {
                    ContentBlock::Text { text } => {
                        AnthropicContentBlock::Text { text: text.clone() }
                    }
                    ContentBlock::ToolUse { id, name, input } => AnthropicContentBlock::ToolUse {
                        id: id.clone(),
                        name: encode_anthropic_tool_name(name),
                        input: input.clone(),
                    },
                    ContentBlock::ToolResult {
                        tool_use_id,
                        content,
                        is_error,
                    } => AnthropicContentBlock::ToolResult {
                        tool_use_id: tool_use_id.clone(),
                        content: content.clone(),
                        is_error: *is_error,
                    },
                })
                .collect(),
        )
    }
}

fn parse_anthropic_response_blocks(blocks: Vec<AnthropicContentBlock>) -> Vec<ContentBlock> {
    blocks
        .into_iter()
        .map(|block| match block {
            AnthropicContentBlock::Text { text } => ContentBlock::Text { text },
            AnthropicContentBlock::ToolUse { id, name, input } => {
                ContentBlock::ToolUse {
                    id,
                    name: decode_anthropic_tool_name(&name),
                    input,
                }
            }
            AnthropicContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
            },
        })
        .collect()
}

#[derive(Debug, Deserialize)]
struct AnthropicUsage {
    input_tokens: u64,
    output_tokens: u64,
}

impl LlmHttpBackend for AnthropicBackend {
    fn call(
        &self,
        client: &reqwest::Client,
        request: &LlmRequest,
        cancellation: &CancellationToken,
    ) -> Result<LlmResponse, ExoError> {
        if cancellation.is_cancelled() {
            return Err(ExoError::LlmInvocation("cancelled before HTTP call".into()));
        }

        let body = self.build_request_body(request);
        let url = format!("{}/v1/messages", self.base_url.trim_end_matches('/'));

        let start = Instant::now();

        let response = block_on(async {
            client
                .post(&url)
                .header("x-api-key", &self.api_key)
                .header("anthropic-version", "2023-06-01")
                .header("content-type", "application/json")
                .json(&body)
                .send()
                .await
        })
        .map_err(|e| {
            if e.is_timeout() {
                ExoError::LlmInvocation(format!("timeout: {e}"))
            } else if e.is_connect() {
                ExoError::LlmInvocation(format!("connection refused: {e}"))
            } else {
                ExoError::LlmInvocation(format!("HTTP error: {e}"))
            }
        })?;

        let latency_ms = start.elapsed().as_millis() as u64;
        let status = response.status();

        if !status.is_success() {
            let body_text = block_on(response.text()).unwrap_or_default();
            return Err(ExoError::LlmInvocation(format!(
                "{}: {}",
                status.as_u16(),
                body_text
            )));
        }

        let response_text = block_on(response.text())
            .map_err(|e| ExoError::LlmInvocation(format!("failed to read response body: {e}")))?;
        let parsed: AnthropicResponse = serde_json::from_str(&response_text).map_err(|e| {
            let snippet: String = response_text.chars().take(600).collect();
            ExoError::LlmInvocation(format!(
                "malformed response body: {e}; body={snippet}"
            ))
        })?;

        let content_blocks = parse_anthropic_response_blocks(parsed.content);

        let stop_reason = match parsed.stop_reason.as_deref() {
            Some("end_turn") => StopReason::EndTurn,
            Some("max_tokens") => StopReason::MaxTokens,
            Some("stop_sequence") => StopReason::StopSequence,
            Some("tool_use") => StopReason::ToolUse,
            _ => StopReason::EndTurn,
        };

        let cost_estimate_cents = estimate_anthropic_cost(
            &parsed.model,
            parsed.usage.input_tokens,
            parsed.usage.output_tokens,
        );

        Ok(LlmResponse {
            content_blocks,
            model: parsed.model,
            tokens_in: parsed.usage.input_tokens,
            tokens_out: parsed.usage.output_tokens,
            latency_ms,
            stop_reason,
            cost_estimate_cents: Some(cost_estimate_cents),
            backend: LlmBackend::Frontier,
        })
    }

    fn call_streaming(
        &self,
        client: &reqwest::Client,
        request: &LlmRequest,
        cancellation: &CancellationToken,
        on_delta: &dyn Fn(&str),
    ) -> Result<LlmResponse, ExoError> {
        if !request.stream {
            return self.call(client, request, cancellation);
        }
        if !request.tools.is_empty() {
            return self.call(client, request, cancellation);
        }
        if cancellation.is_cancelled() {
            return Err(ExoError::LlmInvocation("cancelled before HTTP call".into()));
        }

        let body = self.build_request_body(request);
        let url = format!("{}/v1/messages", self.base_url.trim_end_matches('/'));

        let response = block_on(async {
            client
                .post(&url)
                .header("x-api-key", &self.api_key)
                .header("anthropic-version", "2023-06-01")
                .header("content-type", "application/json")
                .json(&body)
                .send()
                .await
        })
        .map_err(|e| {
            if e.is_timeout() {
                ExoError::LlmInvocation(format!("timeout: {e}"))
            } else if e.is_connect() {
                ExoError::LlmInvocation(format!("connection refused: {e}"))
            } else {
                ExoError::LlmInvocation(format!("HTTP error: {e}"))
            }
        })?;

        let status = response.status();
        if !status.is_success() {
            let body_text = block_on(response.text()).unwrap_or_default();
            return Err(ExoError::LlmInvocation(format!(
                "{}: {}",
                status.as_u16(),
                body_text
            )));
        }

        let mut content_buffer = String::new();
        let mut stop_reason_str: Option<String> = None;
        let mut input_tokens = 0;
        let mut output_tokens = 0;
        let mut model_name: Option<String> = None;

        block_on(async {
            use futures_util::StreamExt;

            let mut stream = response.bytes_stream();
            let mut line_buffer = String::new();
            let mut current_event_type = String::new();

            while let Some(chunk_result) = stream.next().await {
                if cancellation.is_cancelled() {
                    return Err(ExoError::LlmInvocation("cancelled during streaming".into()));
                }

                let chunk = chunk_result
                    .map_err(|e| ExoError::LlmInvocation(format!("stream read error: {e}")))?;
                let text = String::from_utf8_lossy(&chunk);
                line_buffer.push_str(&text);

                let mut stream_done = false;
                while let Some(newline_pos) = line_buffer.find('\n') {
                    let line = line_buffer[..newline_pos].trim().to_string();
                    line_buffer = line_buffer[newline_pos + 1..].to_string();

                    if line.is_empty() {
                        continue;
                    }
                    if let Some(event_type) = line.strip_prefix("event: ") {
                        current_event_type = event_type.trim().to_string();
                        continue;
                    }
                    if let Some(data) = line.strip_prefix("data: ") {
                        match parse_anthropic_sse_event(&current_event_type, data.trim()) {
                            AnthropicSseEvent::MessageStart {
                                model,
                                input_tokens: tokens,
                            } => {
                                model_name = model;
                                if let Some(t) = tokens {
                                    input_tokens = t;
                                }
                            }
                            AnthropicSseEvent::TextDelta(text) => {
                                content_buffer.push_str(&text);
                                on_delta(&text);
                            }
                            AnthropicSseEvent::MessageDelta {
                                stop_reason,
                                output_tokens: tokens,
                            } => {
                                stop_reason_str = stop_reason;
                                if let Some(t) = tokens {
                                    output_tokens = t;
                                }
                            }
                            AnthropicSseEvent::Done => {
                                stream_done = true;
                                break;
                            }
                            AnthropicSseEvent::Skip => {}
                        }
                        current_event_type.clear();
                    }
                }

                if stream_done {
                    break;
                }
            }
            Ok(())
        })?;

        let stop_reason = match stop_reason_str.as_deref() {
            Some("max_tokens") => StopReason::MaxTokens,
            Some("stop_sequence") => StopReason::StopSequence,
            Some("tool_use") => StopReason::ToolUse,
            _ => StopReason::EndTurn,
        };

        let resolved_model = model_name.unwrap_or_else(|| self.model.clone());
        let cost_estimate_cents =
            estimate_anthropic_cost(&resolved_model, input_tokens, output_tokens);

        Ok(LlmResponse {
            content_blocks: vec![ContentBlock::Text {
                text: content_buffer,
            }],
            model: resolved_model,
            tokens_in: input_tokens,
            tokens_out: output_tokens,
            latency_ms: 0,
            stop_reason,
            cost_estimate_cents: Some(cost_estimate_cents),
            backend: LlmBackend::Frontier,
        })
    }
}

// ── Ollama Native Backend ──

/// HTTP backend for Ollama's native /api/chat endpoint.
///
/// Endpoint: `{base_url}/api/chat`
pub struct OllamaNativeBackend {
    base_url: String,
    model: String,
}

impl fmt::Debug for OllamaNativeBackend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OllamaNativeBackend")
            .field("base_url", &self.base_url)
            .field("model", &self.model)
            .finish()
    }
}

impl OllamaNativeBackend {
    /// Create a new Ollama native backend.
    pub fn new(base_url: String, model: String) -> Self {
        Self { base_url, model }
    }

    fn build_request_body(&self, request: &LlmRequest) -> OllamaRequest {
        let mut messages = Vec::new();

        if let Some(ref system) = request.system_prompt {
            messages.push(OllamaMessage {
                role: "system".into(),
                content: Some(system.clone()),
                tool_calls: None,
                tool_call_id: None,
            });
        }

        for msg in &request.messages {
            append_ollama_messages(&mut messages, msg);
        }

        let options = if request.temperature.is_some() || !request.stop_sequences.is_empty() {
            Some(OllamaOptions {
                temperature: request.temperature,
                stop: if request.stop_sequences.is_empty() {
                    None
                } else {
                    Some(request.stop_sequences.clone())
                },
            })
        } else {
            None
        };

        OllamaRequest {
            model: self.model.clone(),
            messages,
            stream: false,
            options,
            tools: if request.tools.is_empty() {
                None
            } else {
                Some(
                    request
                        .tools
                        .iter()
                        .map(tool_to_openai_definition)
                        .collect(),
                )
            },
        }
    }
}

#[derive(Debug, Serialize)]
struct OllamaRequest {
    model: String,
    messages: Vec<OllamaMessage>,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    options: Option<OllamaOptions>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<OpenAiToolDefinition>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct OllamaMessage {
    role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<OpenAiToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
}

#[derive(Debug, Serialize)]
struct OllamaOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stop: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct OllamaResponse {
    message: OllamaMessage,
    model: String,
    #[serde(default)]
    eval_count: Option<u64>,
    #[serde(default)]
    prompt_eval_count: Option<u64>,
}

fn tool_to_openai_definition(tool: &ToolDefinition) -> OpenAiToolDefinition {
    OpenAiToolDefinition {
        r#type: "function".into(),
        function: OpenAiFunctionDefinition {
            name: tool.name.clone(),
            description: tool.description.clone(),
            parameters: tool.input_schema.clone(),
        },
    }
}

fn append_ollama_messages(
    messages: &mut Vec<OllamaMessage>,
    msg: &exoskeleton_core::llm::LlmMessage,
) {
    let role = match msg.role {
        LlmRole::System => "system",
        LlmRole::User => "user",
        LlmRole::Assistant => "assistant",
    };

    let text = msg
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<String>();

    let tool_calls = msg
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::ToolUse { id, name, input } => Some(OpenAiToolCall {
                id: Some(id.clone()),
                r#type: "function".into(),
                function: OpenAiFunctionCall {
                    name: name.clone(),
                    arguments: serde_json::to_string(input).unwrap_or_else(|_| "{}".into()),
                },
            }),
            _ => None,
        })
        .collect::<Vec<_>>();

    if !text.is_empty() || tool_calls.is_empty() {
        messages.push(OllamaMessage {
            role: role.into(),
            content: if text.is_empty() { None } else { Some(text) },
            tool_calls: if tool_calls.is_empty() {
                None
            } else {
                Some(tool_calls)
            },
            tool_call_id: None,
        });
    } else if !tool_calls.is_empty() {
        messages.push(OllamaMessage {
            role: role.into(),
            content: None,
            tool_calls: Some(tool_calls),
            tool_call_id: None,
        });
    }

    for block in &msg.content {
        if let ContentBlock::ToolResult {
            tool_use_id,
            content,
            ..
        } = block
        {
            messages.push(OllamaMessage {
                role: "tool".into(),
                content: Some(content.clone()),
                tool_calls: None,
                tool_call_id: Some(tool_use_id.clone()),
            });
        }
    }
}

impl LlmHttpBackend for OllamaNativeBackend {
    fn call(
        &self,
        client: &reqwest::Client,
        request: &LlmRequest,
        cancellation: &CancellationToken,
    ) -> Result<LlmResponse, ExoError> {
        if cancellation.is_cancelled() {
            return Err(ExoError::LlmInvocation("cancelled before HTTP call".into()));
        }

        let body = self.build_request_body(request);

        let url = format!("{}/api/chat", self.base_url.trim_end_matches('/'));
        let start = Instant::now();

        let response =
            block_on(async { client.post(&url).json(&body).send().await }).map_err(|e| {
                if e.is_timeout() {
                    ExoError::LlmInvocation(format!("timeout: {e}"))
                } else if e.is_connect() {
                    ExoError::LlmInvocation(format!("connection refused: {e}"))
                } else {
                    ExoError::LlmInvocation(format!("HTTP error: {e}"))
                }
            })?;

        let latency_ms = start.elapsed().as_millis() as u64;
        let status = response.status();

        if !status.is_success() {
            let body_text = block_on(response.text()).unwrap_or_default();
            return Err(ExoError::LlmInvocation(format!(
                "{}: {}",
                status.as_u16(),
                body_text
            )));
        }

        let parsed: OllamaResponse = block_on(response.json())
            .map_err(|e| ExoError::LlmInvocation(format!("malformed response body: {e}")))?;
        let content_blocks = parse_openai_response_blocks(OpenAiMessage {
            role: parsed.message.role.clone(),
            content: parsed.message.content.clone(),
            tool_calls: parsed.message.tool_calls.clone(),
            tool_call_id: parsed.message.tool_call_id.clone(),
        })?;

        let tokens_in = parsed.prompt_eval_count.unwrap_or({
            let in_chars = request_text_len(request);
            (in_chars / 4) as u64
        });
        let tokens_out = parsed.eval_count.unwrap_or(
            (content_blocks
                .iter()
                .map(|block| match block {
                    ContentBlock::Text { text } => text.len(),
                    ContentBlock::ToolUse { input, .. } => input.to_string().len(),
                    ContentBlock::ToolResult { content, .. } => content.len(),
                })
                .sum::<usize>()
                / 4) as u64,
        );

        Ok(LlmResponse {
            content_blocks,
            model: parsed.model,
            tokens_in,
            tokens_out,
            latency_ms,
            stop_reason: if parsed.message.tool_calls.is_some() {
                StopReason::ToolUse
            } else {
                StopReason::EndTurn
            },
            cost_estimate_cents: None, // Local models have no cost
            backend: LlmBackend::Local,
        })
    }
}

// ── Error classification ──

use actionqueue_executor_local::HandlerOutput;

/// Classify an LLM invocation error as retryable or terminal.
pub fn classify_llm_error(error: ExoError) -> HandlerOutput {
    let msg = error.to_string();
    if is_retryable_error(&msg) {
        HandlerOutput::retryable_failure(msg)
    } else {
        HandlerOutput::terminal_failure(msg)
    }
}

fn is_retryable_error(msg: &str) -> bool {
    msg.contains("timeout")
        || msg.contains("connection refused")
        || msg.contains("429")
        || msg.contains("500")
        || msg.contains("502")
        || msg.contains("503")
        || msg.contains("504")
        || msg.contains("cancelled")
}

// ── Cost estimation ──

/// Estimate cost for OpenAI-compatible models (cents).
fn estimate_openai_cost(model: &str, tokens_in: u64, tokens_out: u64) -> Option<f64> {
    // Prices in dollars per million tokens: (input, output)
    let (price_in, price_out) = if model.contains("gpt-4o-mini") {
        (0.15, 0.60)
    } else if model.contains("gpt-4o") {
        (2.50, 10.00)
    } else if model.contains("gpt-4") {
        (30.00, 60.00)
    } else {
        return None;
    };
    // Convert to cents: (dollars/million) * tokens / 1_000_000 * 100 cents/dollar
    let cost = (price_in * tokens_in as f64 + price_out * tokens_out as f64) / 1_000_000.0 * 100.0;
    Some(cost)
}

/// Estimate cost for Anthropic models (cents).
fn estimate_anthropic_cost(model: &str, tokens_in: u64, tokens_out: u64) -> f64 {
    // Prices in dollars per million tokens: (input, output)
    let (price_in, price_out) = if model.contains("opus") {
        (15.00, 75.00)
    } else if model.contains("sonnet") {
        (3.00, 15.00)
    } else if model.contains("haiku") {
        (0.25, 1.25)
    } else {
        (3.00, 15.00) // Default to sonnet pricing for unknown models
    };
    (price_in * tokens_in as f64 + price_out * tokens_out as f64) / 1_000_000.0 * 100.0
}

#[cfg(test)]
mod tests {
    use exoskeleton_core::llm::{ContentBlock, LlmMessage};

    use super::*;
    use crate::llm::mock::MockLlmBackend;

    // ── T-3: HTTP Client Layer ──

    #[test]
    fn parse_openai_sse_delta_extracts_content() {
        let line = r#"data: {"choices":[{"delta":{"content":"Hello"},"index":0}]}"#;
        let delta = parse_openai_sse_line(line);
        assert_eq!(delta, OpenAiSseEvent::Delta("Hello".into()));
    }

    #[test]
    fn parse_openai_sse_done_signal() {
        let delta = parse_openai_sse_line("data: [DONE]");
        assert_eq!(delta, OpenAiSseEvent::Done);
    }

    #[test]
    fn parse_openai_sse_with_usage() {
        let line = r#"data: {"choices":[{"delta":{},"finish_reason":"stop","index":0}],"usage":{"prompt_tokens":10,"completion_tokens":5}}"#;
        let delta = parse_openai_sse_line(line);
        match delta {
            OpenAiSseEvent::Final {
                finish_reason,
                prompt_tokens,
                completion_tokens,
            } => {
                assert_eq!(finish_reason.as_deref(), Some("stop"));
                assert_eq!(prompt_tokens, Some(10));
                assert_eq!(completion_tokens, Some(5));
            }
            other => panic!("expected Final, got {other:?}"),
        }
    }

    #[test]
    fn parse_openai_sse_empty_delta_ignored() {
        let line = r#"data: {"choices":[{"delta":{},"index":0}]}"#;
        let delta = parse_openai_sse_line(line);
        assert_eq!(delta, OpenAiSseEvent::Skip);
    }

    #[test]
    fn parse_anthropic_sse_content_block_delta() {
        let event_type = "content_block_delta";
        let data = r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello"}}"#;
        let delta = parse_anthropic_sse_event(event_type, data);
        assert_eq!(delta, AnthropicSseEvent::TextDelta("Hello".into()));
    }

    #[test]
    fn parse_anthropic_sse_message_delta_stop_reason() {
        let event_type = "message_delta";
        let data = r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":5}}"#;
        let delta = parse_anthropic_sse_event(event_type, data);
        match delta {
            AnthropicSseEvent::MessageDelta {
                stop_reason,
                output_tokens,
            } => {
                assert_eq!(stop_reason.as_deref(), Some("end_turn"));
                assert_eq!(output_tokens, Some(5));
            }
            other => panic!("expected MessageDelta, got {other:?}"),
        }
    }

    #[test]
    fn parse_anthropic_sse_message_stop() {
        let delta = parse_anthropic_sse_event("message_stop", r#"{"type":"message_stop"}"#);
        assert_eq!(delta, AnthropicSseEvent::Done);
    }

    #[test]
    fn parse_anthropic_sse_message_start_extracts_input_tokens() {
        let event_type = "message_start";
        let data = r#"{"type":"message_start","message":{"model":"claude-sonnet-4-20250514","usage":{"input_tokens":25,"output_tokens":0}}}"#;
        let delta = parse_anthropic_sse_event(event_type, data);
        match delta {
            AnthropicSseEvent::MessageStart {
                model,
                input_tokens,
            } => {
                assert_eq!(model.as_deref(), Some("claude-sonnet-4-20250514"));
                assert_eq!(input_tokens, Some(25));
            }
            other => panic!("expected MessageStart, got {other:?}"),
        }
    }

    #[test]
    fn call_streaming_default_falls_back_to_call() {
        let backend = OllamaNativeBackend::new("http://localhost:11434".into(), "test".into());
        let _: &dyn LlmHttpBackend = &backend;

        use exoskeleton_core::llm::{LlmBackend, StopReason};

        let response = LlmResponse {
            content_blocks: vec![ContentBlock::Text {
                text: "streaming fallback".into(),
            }],
            model: "mock".into(),
            tokens_in: 10,
            tokens_out: 5,
            latency_ms: 50,
            stop_reason: StopReason::EndTurn,
            cost_estimate_cents: None,
            backend: LlmBackend::Local,
        };
        let mock = MockLlmBackend::new(response);
        let client = reqwest::Client::new();
        let token = CancellationToken::new();
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

        let deltas = std::sync::Mutex::new(Vec::new());
        let result = mock.call_streaming(&client, &request, &token, &|chunk: &str| {
            deltas.lock().unwrap().push(chunk.to_string());
        });
        assert!(result.is_ok());
        assert_eq!(result.unwrap().text(), "streaming fallback");
        assert!(deltas.lock().unwrap().is_empty());
    }

    #[test]
    fn openai_compat_request_format() {
        let backend = OpenAiCompatBackend::new(
            "http://localhost:11434".into(),
            "llama3.2:latest".into(),
            None,
        );
        let request = LlmRequest {
            backend: None,
            system_prompt: Some("You are helpful.".into()),
            messages: vec![LlmMessage::text(LlmRole::User, "Hello")],
            max_output_tokens: 1024,
            temperature: Some(0.7),
            stop_sequences: vec!["</answer>".into()],
            stream: false,
            tools: vec![],
        };
        let body = backend.build_request_body(&request);
        assert_eq!(body.model, "llama3.2:latest");
        assert_eq!(body.messages.len(), 2); // system + user
        assert_eq!(body.messages[0].role, "system");
        assert_eq!(
            body.messages[0].content.as_deref(),
            Some("You are helpful.")
        );
        assert_eq!(body.messages[1].role, "user");
        assert_eq!(body.messages[1].content.as_deref(), Some("Hello"));
        assert_eq!(body.max_completion_tokens, 1024);
        assert_eq!(body.temperature, Some(0.7));
        assert_eq!(body.stop, Some(vec!["</answer>".into()]));
    }

    #[test]
    fn openai_compat_response_parsing() {
        let json = r#"{
            "choices": [{
                "message": {"role": "assistant", "content": "Hello there!"},
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 10, "completion_tokens": 5},
            "model": "llama3.2:latest"
        }"#;
        let parsed: OpenAiResponse = serde_json::from_str(json).unwrap();
        assert_eq!(
            parsed.choices[0].message.content.as_deref(),
            Some("Hello there!")
        );
        assert_eq!(parsed.choices[0].finish_reason.as_deref(), Some("stop"));
        let usage = parsed.usage.unwrap();
        assert_eq!(usage.prompt_tokens, 10);
        assert_eq!(usage.completion_tokens, 5);
    }

    #[test]
    fn openai_compat_system_message_injection() {
        let backend =
            OpenAiCompatBackend::new("http://localhost:11434".into(), "test".into(), None);
        let request = LlmRequest {
            backend: None,
            system_prompt: Some("Be concise.".into()),
            messages: vec![LlmMessage::text(LlmRole::User, "Hi")],
            max_output_tokens: 100,
            temperature: None,
            stop_sequences: vec![],
            stream: false,
            tools: vec![],
        };
        let body = backend.build_request_body(&request);
        // System prompt becomes the first message with role "system"
        assert_eq!(body.messages[0].role, "system");
        assert_eq!(body.messages[0].content.as_deref(), Some("Be concise."));
    }

    #[test]
    fn anthropic_request_format() {
        let backend = AnthropicBackend::new(
            "https://api.anthropic.com".into(),
            "claude-sonnet-4-20250514".into(),
            "test-key".into(),
        );
        let request = LlmRequest {
            backend: None,
            system_prompt: Some("You are helpful.".into()),
            messages: vec![LlmMessage::text(LlmRole::User, "Hello")],
            max_output_tokens: 2048,
            temperature: Some(0.5),
            stop_sequences: vec!["STOP".into()],
            stream: false,
            tools: vec![],
        };
        let body = backend.build_request_body(&request);
        assert_eq!(body.model, "claude-sonnet-4-20250514");
        // System prompt is a top-level field, not a message
        assert_eq!(body.system, Some("You are helpful.".into()));
        assert_eq!(body.messages.len(), 1); // Only user message
        assert_eq!(body.messages[0].role, "user");
        assert_eq!(body.max_tokens, 2048);
        assert_eq!(body.stop_sequences, Some(vec!["STOP".into()]));
    }

    #[test]
    fn anthropic_response_parsing() {
        let json = r#"{
            "content": [{"type": "text", "text": "I'm Claude."}],
            "model": "claude-sonnet-4-20250514",
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 20, "output_tokens": 8}
        }"#;
        let parsed: AnthropicResponse = serde_json::from_str(json).unwrap();
        assert!(matches!(
            parsed.content[0],
            AnthropicContentBlock::Text { ref text } if text == "I'm Claude."
        ));
        assert_eq!(parsed.model, "claude-sonnet-4-20250514");
        assert_eq!(parsed.stop_reason.as_deref(), Some("end_turn"));
        assert_eq!(parsed.usage.input_tokens, 20);
        assert_eq!(parsed.usage.output_tokens, 8);
    }

    #[test]
    fn anthropic_system_as_top_level() {
        let backend = AnthropicBackend::new(
            "https://api.anthropic.com".into(),
            "test".into(),
            "key".into(),
        );
        let request = LlmRequest {
            backend: None,
            system_prompt: Some("System instruction".into()),
            messages: vec![LlmMessage::text(LlmRole::User, "Hi")],
            max_output_tokens: 100,
            temperature: None,
            stop_sequences: vec![],
            stream: false,
            tools: vec![],
        };
        let body = backend.build_request_body(&request);
        // System should be a top-level field
        assert_eq!(body.system, Some("System instruction".into()));
        // Messages should NOT contain a system message
        assert!(body.messages.iter().all(|m| m.role != "system"));
    }

    #[test]
    fn anthropic_headers() {
        // Verify header values are set by the backend (unit-level check)
        let _backend = AnthropicBackend::new(
            "https://api.anthropic.com".into(),
            "claude-sonnet-4-20250514".into(),
            "sk-test-key".into(),
        );
        // Headers are set in the call() method — this test verifies the Debug
        // impl doesn't leak the API key
        let debug = format!("{:?}", _backend);
        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains("sk-test-key"));
    }

    #[test]
    fn ollama_native_request_format() {
        let _backend =
            OllamaNativeBackend::new("http://localhost:11434".into(), "llama3.2:latest".into());
        let request = LlmRequest {
            backend: None,
            system_prompt: Some("Be helpful.".into()),
            messages: vec![LlmMessage::text(LlmRole::User, "Hello")],
            max_output_tokens: 1024,
            temperature: Some(0.8),
            stop_sequences: vec!["END".into()],
            stream: false,
            tools: vec![],
        };
        let body = _backend.build_request_body(&request);
        let json = serde_json::to_value(&body).unwrap();
        assert_eq!(json["model"], "llama3.2:latest");
        assert_eq!(json["stream"], false);
        assert_eq!(json["messages"][0]["role"], "system");
        assert!(json["options"]["temperature"].is_number());
    }

    #[test]
    fn ollama_native_response_parsing() {
        let json = r#"{
            "message": {"role": "assistant", "content": "Hello from Ollama!"},
            "model": "llama3.2:latest",
            "eval_count": 15,
            "prompt_eval_count": 25
        }"#;
        let parsed: OllamaResponse = serde_json::from_str(json).unwrap();
        assert_eq!(
            parsed.message.content.as_deref(),
            Some("Hello from Ollama!")
        );
        assert_eq!(parsed.model, "llama3.2:latest");
        assert_eq!(parsed.eval_count, Some(15));
        assert_eq!(parsed.prompt_eval_count, Some(25));
    }

    #[test]
    fn http_429_is_retryable() {
        assert!(is_retryable_error("429: rate limit exceeded"));
        assert!(is_retryable_error("HTTP 429 Too Many Requests"));
    }

    #[test]
    fn http_401_is_terminal() {
        assert!(!is_retryable_error("401: unauthorized"));
        assert!(!is_retryable_error("403: forbidden"));
        assert!(!is_retryable_error("400: bad request"));
    }

    #[test]
    fn http_5xx_is_retryable() {
        assert!(is_retryable_error("500: internal server error"));
        assert!(is_retryable_error("502: bad gateway"));
        assert!(is_retryable_error("503: service unavailable"));
        assert!(is_retryable_error("504: gateway timeout"));
    }

    #[test]
    fn timeout_is_retryable() {
        assert!(is_retryable_error("timeout: operation timed out"));
        assert!(is_retryable_error("connection refused: could not connect"));
    }

    #[test]
    fn openai_debug_redacts_api_key() {
        let backend = OpenAiCompatBackend::new(
            "http://api.openai.com".into(),
            "gpt-4o".into(),
            Some("sk-secret123".into()),
        );
        let debug = format!("{:?}", backend);
        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains("sk-secret123"));
    }

    // ── Wire-format parsing tests (tool_use) ──

    #[test]
    fn parse_anthropic_text_content_block() {
        let json = r#"{"type": "text", "text": "I'll read the file."}"#;
        let block: AnthropicContentBlock = serde_json::from_str(json).unwrap();
        match block {
            AnthropicContentBlock::Text { text } => assert_eq!(text, "I'll read the file."),
            other => panic!("expected Text, got: {other:?}"),
        }
    }

    #[test]
    fn parse_anthropic_tool_use_content_block() {
        let json = r#"{
            "type": "tool_use",
            "id": "toolu_01ABC",
            "name": "code.read",
            "input": {"file_path": "src/lib.rs"}
        }"#;
        let block: AnthropicContentBlock = serde_json::from_str(json).unwrap();
        match block {
            AnthropicContentBlock::ToolUse { id, name, input } => {
                assert_eq!(id, "toolu_01ABC");
                assert_eq!(name, "code.read");
                assert_eq!(input["file_path"], "src/lib.rs");
            }
            other => panic!("expected ToolUse, got: {other:?}"),
        }
    }

    #[test]
    fn parse_anthropic_tool_result_content_block() {
        let json = r#"{
            "type": "tool_result",
            "tool_use_id": "toolu_01ABC",
            "content": "file contents here",
            "is_error": false
        }"#;
        let block: AnthropicContentBlock = serde_json::from_str(json).unwrap();
        match block {
            AnthropicContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => {
                assert_eq!(tool_use_id, "toolu_01ABC");
                assert_eq!(content, "file contents here");
                assert!(!is_error);
            }
            other => panic!("expected ToolResult, got: {other:?}"),
        }
    }

    #[test]
    fn anthropic_tool_definition_serialization() {
        let tool_def = AnthropicToolDefinition {
            name: encode_anthropic_tool_name("code.read"),
            description: "Read a file".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {"file_path": {"type": "string"}},
                "required": ["file_path"]
            }),
        };
        let json = serde_json::to_string(&tool_def).unwrap();
        assert!(json.contains("\"name\":\"tool_636f64652e72656164\""));
        assert!(json.contains("\"input_schema\""));
        assert!(json.contains("\"description\":\"Read a file\""));
    }

    #[test]
    fn anthropic_tool_name_roundtrip_preserves_canonical_name() {
        let encoded = encode_anthropic_tool_name("agent.ask_user");
        assert_eq!(encoded, "tool_6167656e742e61736b5f75736572");
        assert_eq!(decode_anthropic_tool_name(&encoded), "agent.ask_user");
        assert_eq!(decode_anthropic_tool_name("update_plan"), "update_plan");
    }

    #[test]
    fn parse_anthropic_response_blocks_decodes_tool_names() {
        let blocks = parse_anthropic_response_blocks(vec![AnthropicContentBlock::ToolUse {
            id: "toolu_01ABC".into(),
            name: encode_anthropic_tool_name("code.read"),
            input: serde_json::json!({"file_path": "src/lib.rs"}),
        }]);
        assert!(matches!(
            &blocks[0],
            ContentBlock::ToolUse { id, name, input }
                if id == "toolu_01ABC" && name == "code.read" && input["file_path"] == "src/lib.rs"
        ));
    }

    #[test]
    fn parse_openai_tool_call_with_id() {
        let json = r#"{
            "id": "call_abc123",
            "type": "function",
            "function": {
                "name": "code.read",
                "arguments": "{\"file_path\":\"src/lib.rs\"}"
            }
        }"#;
        let call: OpenAiToolCall = serde_json::from_str(json).unwrap();
        assert_eq!(call.id, Some("call_abc123".into()));
        assert_eq!(call.function.name, "code.read");
    }

    #[test]
    fn parse_openai_tool_call_without_id() {
        let json = r#"{
            "type": "function",
            "function": {
                "name": "code.read",
                "arguments": "{\"file_path\":\"src/lib.rs\"}"
            }
        }"#;
        let call: OpenAiToolCall = serde_json::from_str(json).unwrap();
        assert_eq!(call.id, None);
        assert_eq!(call.function.name, "code.read");
    }

    #[test]
    fn parse_openai_response_blocks_with_tool_calls() {
        let msg = OpenAiMessage {
            role: "assistant".into(),
            content: Some("I'll read the file.".into()),
            tool_calls: Some(vec![OpenAiToolCall {
                id: Some("call_1".into()),
                r#type: "function".into(),
                function: OpenAiFunctionCall {
                    name: "code.read".into(),
                    arguments: r#"{"file_path":"src/lib.rs"}"#.into(),
                },
            }]),
            tool_call_id: None,
        };
        let blocks = parse_openai_response_blocks(msg).unwrap();
        assert_eq!(blocks.len(), 2);
        assert!(matches!(&blocks[0], ContentBlock::Text { text } if text == "I'll read the file."));
        assert!(matches!(&blocks[1], ContentBlock::ToolUse { id, name, .. } if id == "call_1" && name == "code.read"));
    }

    #[test]
    fn parse_openai_response_blocks_synthetic_id() {
        let msg = OpenAiMessage {
            role: "assistant".into(),
            content: None,
            tool_calls: Some(vec![OpenAiToolCall {
                id: None,
                r#type: "function".into(),
                function: OpenAiFunctionCall {
                    name: "code.read".into(),
                    arguments: r#"{"file_path":"test"}"#.into(),
                },
            }]),
            tool_call_id: None,
        };
        let blocks = parse_openai_response_blocks(msg).unwrap();
        assert_eq!(blocks.len(), 1);
        assert!(
            matches!(&blocks[0], ContentBlock::ToolUse { id, .. } if id == "synthetic_call_0")
        );
    }

    #[test]
    fn openai_tool_definition_serialization() {
        let tool_def = OpenAiToolDefinition {
            r#type: "function".into(),
            function: OpenAiFunctionDefinition {
                name: "code.read".into(),
                description: "Read a file".into(),
                parameters: serde_json::json!({"type": "object"}),
            },
        };
        let json = serde_json::to_string(&tool_def).unwrap();
        assert!(json.contains("\"type\":\"function\""));
        assert!(json.contains("\"name\":\"code.read\""));
        assert!(json.contains("\"parameters\""));
    }

    #[test]
    fn anthropic_request_disables_streaming_when_tools_present() {
        let backend = AnthropicBackend::new(
            "https://api.anthropic.com".into(),
            "claude-sonnet-4-20250514".into(),
            "test-key".into(),
        );
        let request = LlmRequest {
            backend: None,
            system_prompt: Some("test".into()),
            messages: vec![exoskeleton_core::llm::LlmMessage::text(LlmRole::User, "hello")],
            max_output_tokens: 256,
            temperature: None,
            stop_sequences: vec![],
            stream: true,
            tools: vec![ToolDefinition {
                name: "code.read".into(),
                description: "Read a file".into(),
                input_schema: serde_json::json!({"type": "object"}),
            }],
        };

        let body = backend.build_request_body(&request);
        assert!(!body.stream);
    }

    #[test]
    fn openai_request_disables_streaming_when_tools_present() {
        let backend =
            OpenAiCompatBackend::new("https://api.openai.com".into(), "gpt-4o".into(), None);
        let request = LlmRequest {
            backend: None,
            system_prompt: Some("test".into()),
            messages: vec![exoskeleton_core::llm::LlmMessage::text(LlmRole::User, "hello")],
            max_output_tokens: 256,
            temperature: None,
            stop_sequences: vec![],
            stream: true,
            tools: vec![ToolDefinition {
                name: "code.read".into(),
                description: "Read a file".into(),
                input_schema: serde_json::json!({"type": "object"}),
            }],
        };

        let body = backend.build_request_body(&request);
        assert!(!body.stream);
    }
}

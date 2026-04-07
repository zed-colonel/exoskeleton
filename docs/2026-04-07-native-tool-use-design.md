# Native Tool Use Design

> **Pre-E11** — Replace free-form JSON parsing with native tool_use/function_calling
> APIs for reliable LLM-to-agent communication.

**Goal:** Eliminate the fragile free-form JSON parsing in the Decide/DecideLite steps
by adopting native tool_use (Anthropic Messages API) and function_calling
(OpenAI-compatible APIs). The LLM communicates actions, metadata updates, and
queries exclusively through structured tool_use blocks guaranteed by the API.

**Architecture:** Provider-agnostic `ContentBlock` types in `exoskeleton-core`.
HTTP backends translate to/from provider-specific wire formats. Two scoped tool
sets — Decide (full: WI connectors + virtual + cognitive + introspection) and
DecideLite (inner loop: WI connectors + cognitive only). All agent metadata
(plans, memory, watches, replies, mode transitions) expressed as cognitive tools
processed within the kernel. Free-form JSON parsing deleted entirely.

**Tech Stack:** Rust, Anthropic Messages API (`tool_use` content blocks), OpenAI
Chat Completions API (`tool_calls`), existing exoskeleton-core/host/memory crates.

---

## 1. Core LLM Types

All types in `exoskeleton-core/src/llm.rs`. Provider-agnostic representations
that backends translate to/from wire formats.

### 1.1 New Types

```rust
/// A tool definition sent to the LLM.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

/// A content block in an LLM message or response.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
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
        #[serde(default)]
        is_error: bool,
    },
}
```

### 1.2 Modified Types

```rust
pub struct LlmMessage {
    pub role: LlmRole,
    pub content: Vec<ContentBlock>,  // was: String
}

pub struct LlmRequest {
    pub backend: Option<LlmBackend>,
    pub system_prompt: Option<String>,
    pub messages: Vec<LlmMessage>,
    pub max_output_tokens: u64,
    pub temperature: Option<f64>,
    pub stop_sequences: Vec<String>,
    pub stream: bool,
    pub tools: Vec<ToolDefinition>,  // new
}

pub struct LlmResponse {
    pub content_blocks: Vec<ContentBlock>,  // was: content: String
    pub model: String,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub latency_ms: u64,
    pub stop_reason: StopReason,
    pub cost_estimate_cents: Option<f64>,
    pub backend: LlmBackend,
}

pub enum StopReason {
    EndTurn,
    MaxTokens,
    StopSequence,
    ToolUse,  // new
}
```

### 1.3 Convenience Methods

```rust
impl LlmMessage {
    /// Create a message with a single text block.
    pub fn text(role: LlmRole, content: impl Into<String>) -> Self;
}

impl LlmResponse {
    /// Concatenate all text content blocks.
    pub fn text(&self) -> String;

    /// Extract all ToolUse blocks.
    pub fn tool_use_blocks(&self) -> Vec<&ContentBlock>;

    /// Whether the response contains any ToolUse blocks.
    pub fn has_tool_use(&self) -> bool;
}
```

---

## 2. HTTP Backend Translation

Each backend translates between core `ContentBlock` types and provider-specific
wire formats. The `LlmHttpBackend` trait accepts `LlmRequest` (now with `tools`
and content blocks) and returns `LlmResponse` (now with `content_blocks`).

### 2.1 Anthropic Backend (`/v1/messages`)

**Request:**
- `LlmRequest.tools` maps to Anthropic `tools` array:
  `[{"name": "...", "description": "...", "input_schema": {...}}]`
- `LlmMessage` with single `Text` block maps to simple string content:
  `{"role": "user", "content": "text"}`
- `LlmMessage` with mixed blocks maps to array content:
  `{"role": "user", "content": [{"type": "text", ...}, {"type": "tool_result", ...}]}`
- `ContentBlock::ToolResult` maps to:
  `{"type": "tool_result", "tool_use_id": "...", "content": "...", "is_error": bool}`

**Response:**
- `AnthropicContentBlock` enum expands to handle both text and tool_use:
  - `{"type": "text", "text": "..."}` maps to `ContentBlock::Text`
  - `{"type": "tool_use", "id": "...", "name": "...", "input": {...}}`
    maps to `ContentBlock::ToolUse`
- `stop_reason: "tool_use"` maps to `StopReason::ToolUse`

### 2.2 OpenAI-Compatible Backend (`/v1/chat/completions`)

Covers OpenAI, Ollama, vLLM, LM Studio.

**Request:**
- `LlmRequest.tools` maps to OpenAI `tools` array:
  `[{"type": "function", "function": {"name": "...", "description": "...", "parameters": {...}}}]`
- `ContentBlock::ToolResult` maps to a separate message:
  `{"role": "tool", "tool_call_id": "...", "content": "..."}`
- Assistant messages with `ToolUse` blocks map to:
  `{"role": "assistant", "tool_calls": [{"id": "...", "type": "function", "function": {"name": "...", "arguments": "..."}}]}`

**Response:**
- `message.content` (text) maps to `ContentBlock::Text`
- `message.tool_calls` array maps to `ContentBlock::ToolUse` per entry
  (with `id`, `function.name`, JSON-parsed `function.arguments`)
- `finish_reason: "tool_calls"` maps to `StopReason::ToolUse`

### 2.3 Ollama Native Backend (`/api/chat`)

Same tool_calls format as OpenAI-compatible. Ollama supports function calling
for capable models (Llama 3.1+, Qwen 2.5, Mistral). If a model doesn't support
tools, the response contains only text blocks — the Decide step sees no tool_use
and the tick produces no actions (correct degraded behavior, fails loudly).

---

## 3. Tool Categories

Three categories of tools, processed differently by the kernel.

### 3.1 WI Connector Tools

Registered in WorldInterface `ConnectorRegistry`. Dispatched through the Tool AQ
via the Act step (I9 boundary crossing). Available in both Decide and DecideLite.

Examples: `code.read`, `code.edit`, `code.write`, `code.grep`, `code.glob`,
`code.ls`, `code.apply_patch`, `code.git_diff`, `shell.exec`, `sandbox.exec`,
`fs.read`, `fs.write`.

Virtual tools (`agent.ask_user`, `peer.resolve`) are included in this category
since they're translated to WI calls at the Act step.

### 3.2 Cognitive Tools

Processed entirely within the Decide/DecideLite step. Never cross the I9
boundary. Never registered in ConnectorRegistry.

| Tool Name | Description | Available In |
|-----------|-------------|-------------|
| `update_plan` | Replace or patch the current plan | Decide, DecideLite |
| `set_working_memory` | Set/delete working memory entries | Decide, DecideLite |
| `save_memory_note` | Store an episodic memory observation | Decide, DecideLite |
| `propose_watch` | Propose a persistent watch | Decide, DecideLite |
| `request_vessel_mode` | Request mode transition | Decide, DecideLite |
| `reply_to_user` | Send a reply to the user | Decide, DecideLite |

When the LLM calls a cognitive tool, the Decide step:
1. Extracts the input parameters
2. Accumulates into the `DecisionResult` fields (plan_update, working_memory_ops, etc.)
3. Returns an acknowledgment `tool_result` (`"ok"`, `is_error: false`)
4. Continues the multi-turn loop

### 3.3 Introspection Tools

Query vessel internal state. Available only in the Decide step (not DecideLite).
Processed via the existing `IntrospectionService`.

| Tool Name | Description |
|-----------|-------------|
| `introspect_tick_history` | Recent ticks with action counts |
| `introspect_budget_status` | Remaining budget dimensions |
| `introspect_thread_status` | All threads with recent outputs |
| `introspect_memory_search` | Search episodic memory |
| `introspect_trust_scores` | Trust levels per principal |
| `introspect_connector_details` | Tool descriptor lookup |
| `introspect_watch_list` | Active watches |
| `introspect_event_history` | Recent events |

When the LLM calls an introspection tool, the Decide step:
1. Executes the query via `IntrospectionService`
2. Returns the result as `tool_result` content (JSON-serialized)
3. Continues the multi-turn loop

---

## 4. Tool Set Builders

Two focused builder functions in the kernel. No composable abstractions.

### 4.1 `build_decide_tools()`

Returns `Vec<ToolDefinition>` containing:
1. WI connector tools — from `kernel.wi_host_slot.list_capabilities()`
2. Virtual tool definitions — `agent.ask_user`, `peer.resolve` (if observatory configured)
3. Cognitive tools — all 6 definitions
4. Introspection tools — all 8 definitions
5. Filters out infrastructure primitives (`signal.await`, `signal.emit`)

### 4.2 `build_inner_loop_tools()`

Returns `Vec<ToolDefinition>` containing:
1. WI connector tools — same source as Decide
2. Cognitive tools — all 6 definitions
3. No virtual tools — `agent.ask_user` and `peer.resolve` require cross-tick
   resolution (signal.await) which the inner loop cannot perform
4. No introspection tools
5. Same infrastructure filtering

Tool definitions are constructed from `ConnectorDescriptor` (name, description,
input_schema) which WI connectors already provide via `describe()`. Cognitive and
introspection tool definitions are hardcoded structs (they don't change at runtime).

---

## 5. Decide Step Rewrite

Replaces the current JSON-parsing multi-turn loop with a tool_use-driven loop.

### 5.1 Flow

```
build_decide_tools()         →  Vec<ToolDefinition>
build_system_prompt()        →  String (behavioral guidance only)

Loop (max_decide_turns):
  1. Build LlmRequest with tools + messages
  2. handler_direct_llm_call()
  3. Process response content_blocks:
     - Text        → accumulate into reasoning
     - ToolUse     → categorize by tool name:
         introspection → execute, return tool_result, continue
         cognitive     → accumulate into DecisionResult, return tool_result, continue
         WI/virtual    → accumulate into actions list
  4. Append assistant message + tool_results to conversation history
  5. Check stop_reason:
     - ToolUse  → continue loop
     - EndTurn  → break
  6. Return DecisionResult
```

### 5.2 DecisionResult Construction

| DecisionResult field | Source |
|---------------------|--------|
| `reasoning` | Concatenated `Text` content blocks |
| `reply` | `reply_to_user` cognitive tool input |
| `actions` | WI/virtual `ToolUse` blocks (with `call_id` preserved) |
| `plan_update` | `update_plan` cognitive tool input |
| `working_memory_ops` | `set_working_memory` cognitive tool input |
| `memory_notes` | `save_memory_note` cognitive tool inputs (accumulated) |
| `watch_proposals` | `propose_watch` cognitive tool inputs (accumulated) |
| `vessel_mode_request` | `request_vessel_mode` cognitive tool input |
| `inner_loop_requested` | Inferred: `true` if actions list is non-empty |

### 5.3 Deleted Code

- `DecisionProtocol` struct
- `DecideTurn` enum
- `parse_decide_turn()` function
- `extract_json_from_code_fence()` function
- `build_tools_description()` function (replaced by `build_decide_tools()`)
- JSON response format instructions in system prompts

---

## 6. Inner Loop (DecideLite) Adaptation

### 6.1 Flow

Same categorization logic as Decide, but with the inner loop tool set
(no introspection tools).

Each iteration:
1. Build messages with tool results from previous Act step execution as
   `ContentBlock::ToolResult` blocks (matched by `call_id`)
2. `handler_direct_llm_call()` with inner loop tools
3. Process response — cognitive tools consumed inline, WI tools become actions
4. Completion: `EndTurn` with no WI tool calls = `AgentComplete`

### 6.2 Context Windowing

Instead of summarizing text, trim older message pairs (assistant tool_use +
user tool_result) from the front of the conversation history, keeping only
the most recent N turns. Cleaner than text truncation.

### 6.3 Doom-Loop Detection

Unchanged — tracks consecutive identical tool calls (name + input hash).
Correction message becomes a `ContentBlock::Text` injected into the user
message alongside tool results.

### 6.4 Completion Signals

- `stop_reason == EndTurn`, no WI tool_use blocks → `AgentComplete`
- `stop_reason == EndTurn`, only cognitive tool_use blocks → `AgentComplete`
- `stop_reason == ToolUse`, WI tool calls present → execute via Act, continue
- Budget exhaustion → `StepLimit`/`TokenLimit`/`Timeout` as before

---

## 7. Act Step Changes

### 7.1 PlannedAction

Adds `call_id: String` field — the API-issued tool_use ID, passed through
verbatim for tool_result matching. Not converted to UUID (it's an opaque
token from the API provider).

### 7.2 ActionExecution

Adds `tool_result: ContentBlock` field — a `ContentBlock::ToolResult`
constructed from the execution outcome:
- Success: `content` = JSON tool output (truncated to `MAX_TOOL_RESULT_CHARS = 4000`), `is_error = false`
- Failure: `content` = error message, `is_error = true`
- Policy denied: `content` = denial reason, `is_error = true`

The inner loop feeds these `tool_result` blocks back to the LLM on the
next DecideLite call.

---

## 8. Prompt System Changes

### 8.1 Removed from System Prompt

- `{{tools}}` section (tool list as text) — now in API `tools` parameter
- JSON response format specification
- Introspection query format description
- `DecisionProtocol` schema example

### 8.2 Retained in System Prompt

- Vessel identity (ID, mission)
- Behavioral guidance (PODAARA model, trust gates, relationship awareness)
- Coding guidelines (read before edit, verify changes, 5-step workflow)
- Mode context (Planning/Executing/Normal)
- Inner loop hint ("call code/shell tools to activate the iterative coding loop")

### 8.3 Files Affected

- `prompts/decide-system.md` — significant simplification
- `prompts/inner-loop-system.md` — same simplification
- `prompts/coding-system.md` — mostly unchanged (behavioral, not format)

---

## 9. Scope & Boundaries

### 9.1 In Scope

- `ContentBlock`, `ToolDefinition`, `StopReason::ToolUse` in exoskeleton-core
- `LlmMessage`, `LlmRequest`, `LlmResponse` migration to content blocks
- `AnthropicBackend` tool_use send/receive
- `OpenAiCompatBackend` tool_calls send/receive
- `OllamaNativeBackend` tool_calls send/receive
- 6 cognitive tool definitions
- 8 introspection tool definitions
- `build_decide_tools()` and `build_inner_loop_tools()`
- Decide step rewrite (tool_use-driven multi-turn loop)
- DecideLite adaptation (tool_use conversation threading)
- Act step `call_id` pass-through and `tool_result` construction
- Tool result truncation
- `PlannedAction.call_id` field
- `ActionExecution.tool_result` field
- Delete: `DecisionProtocol`, `DecideTurn`, `parse_decide_turn`,
  `extract_json_from_code_fence`, `build_tools_description`
- Prompt simplification
- `MockLlmBackend` / `MockSequenceLlmBackend` updated for content blocks
- All tests updated
- Benchmark re-run to validate

### 9.2 Out of Scope

- Parallel tool calls (multiple tool_use blocks executed concurrently) — E11
- Streaming tool_use parsing (incremental `input_json_delta`) — future
- Structured outputs / constrained decoding for tool inputs — future
- MCP tool integration — future
- Changes to Reflect step (evaluates outcomes, doesn't use tools)
- Changes to Amend step (persistence, unchanged)

### 9.3 Sprint Decomposition

- **Sprint 1: Core Types + HTTP Backends** — ContentBlock, ToolDefinition,
  StopReason::ToolUse, LlmMessage/Request/Response migration, all three
  backend translations, MockLlmBackend update. Exit: builds, all tests pass.
- **Sprint 2: Kernel Rewrite** — Cognitive tools, introspection tools, tool
  builders, Decide rewrite, DecideLite adaptation, Act step call_id/tool_result,
  delete legacy parsing, prompt simplification. Exit: builds, tests pass,
  benchmark runs and produces actions.
- **Sprint 3: Validation & Baseline** — Run full benchmark suite, fix
  behavioral issues, tune prompts, record baseline. Exit: initial benchmark
  baseline recorded.

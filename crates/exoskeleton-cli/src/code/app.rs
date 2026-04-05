//! Application state, Message enum, pure update() function, and SideEffect enum.
//!
//! Follows the Elm architecture: `update()` is a pure function that takes the
//! current `App` state and a `Message`, returning side effects to execute.
//! The event loop calls `update()` and then executes the side effects.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use exoskeleton_core::{EventType, LiveEvent};

use crate::code::render::diff::{DiffFileSummary, DiffSummaryData};

use super::widgets::approval::{
    confirmation_options, plan_approval_options, tool_approval_options, ApprovalAction,
    ApprovalContext, ApprovalKind, ApprovalOption, ApprovalState,
};
use super::widgets::conversation::{Block, ConversationState, NoteSeverity, ToolOutcome};

/// The top-level UI mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UiMode {
    /// Standard conversation view — input area active.
    Normal,
    /// Approval overlay is active — replaces input area.
    Approval(ApprovalContext),
}

/// Connection status to the vessel daemon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionStatus {
    /// Establishing initial connection.
    Connecting,
    /// Connected and streaming events.
    Connected,
    /// WebSocket disconnected.
    Disconnected,
}

/// Status bar information about the vessel.
#[derive(Debug, Clone)]
pub struct StatusState {
    /// Vessel display name (from status endpoint).
    pub vessel_name: String,
    /// Current vessel mode (Normal, Planning, Executing).
    pub vessel_mode: String,
    /// Token budget summary (e.g., "4,231 tok").
    pub token_summary: String,
    /// Step summary (e.g., "step 3/25").
    pub step_summary: String,
}

impl Default for StatusState {
    fn default() -> Self {
        Self {
            vessel_name: String::new(),
            vessel_mode: "normal".into(),
            token_summary: String::new(),
            step_summary: String::new(),
        }
    }
}

/// Activity indicator state.
#[derive(Debug, Default)]
pub struct ActivityState {
    /// Current activity label (e.g., "Thinking...", "Running code.edit...").
    pub label: String,
    /// Spinner animation phase (indexes into SPINNER_FRAMES).
    pub spinner_phase: usize,
    /// Whether the agent is currently active (spinner should animate).
    pub is_active: bool,
    /// Whether LLM text is currently streaming.
    pub is_streaming: bool,
    /// Number of streaming text chunks received in the current response.
    pub streaming_tokens: u64,
}

/// Thread information displayed in the debug banner.
#[derive(Debug, Clone, Default)]
pub struct DebugThreadInfo {
    /// Thread display name.
    pub name: String,
    /// Current state label: "idle", "active", "suspended", or "contributed: \"...\""
    pub state: String,
    /// Whether this thread recently contributed an insight (highlight in banner).
    pub contributed: bool,
}

/// Budget information displayed in the debug banner.
#[derive(Debug, Clone, Default)]
pub struct DebugBudgetInfo {
    /// Token budget usage as a percentage (0-100).
    pub token_percent_used: u8,
    /// Token budget remaining (formatted string like "4,231/50,000").
    pub token_label: String,
    /// Step budget usage as a percentage (0-100).
    pub step_percent_used: u8,
    /// Step budget remaining (formatted string like "3/25").
    pub step_label: String,
}

/// Policy rule information displayed in the debug banner.
#[derive(Debug, Clone)]
pub struct DebugPolicyInfo {
    /// Tool pattern (e.g., "shell.exec", "code.*").
    pub tool_pattern: String,
    /// Policy outcome (e.g., "ask", "allow", "deny").
    pub outcome: String,
}

/// Plan summary information displayed in the debug banner.
#[derive(Debug, Clone)]
pub struct DebugPlanSummary {
    /// Number of completed tasks.
    pub completed: usize,
    /// Total number of tasks.
    pub total: usize,
    /// Compact progress string.
    pub progress_icons: String,
    /// Plan objective (truncated).
    pub objective: String,
}

/// State for the F1 debug banner.
#[derive(Debug, Clone, Default)]
pub struct DebugState {
    /// Whether the debug banner is visible (toggled by F1).
    pub visible: bool,
    /// Current tick number.
    pub tick_number: u64,
    /// Whether the inner loop is currently active.
    pub inner_loop_active: bool,
    /// Step count summary (e.g., "3/25 steps").
    pub step_count: String,
    /// Token count summary (e.g., "4,231/50,000 tok").
    pub token_count: String,
    /// Thread state information.
    pub threads: Vec<DebugThreadInfo>,
    /// Budget progress information.
    pub budget: DebugBudgetInfo,
    /// Active policy rules.
    pub policies: Vec<DebugPolicyInfo>,
    /// Plan summary (if a plan exists).
    pub plan_summary: Option<DebugPlanSummary>,
    /// Initial token budget captured from the first snapshot.
    pub initial_token_budget: Option<u64>,
    /// Initial step limit captured from the first inner-loop step.
    pub initial_step_limit: Option<u64>,
}

/// Braille spinner animation frames.
pub const SPINNER_FRAMES: &[char] = &[
    '\u{280B}', '\u{2819}', '\u{2839}', '\u{2838}', '\u{283C}', '\u{2834}', '\u{2826}', '\u{2827}',
    '\u{2807}', '\u{280F}',
];

/// Top-level application state for the `exo code` TUI.
#[derive(Debug)]
pub struct App {
    /// Conversation blocks and scroll state.
    pub conversation: ConversationState,
    /// Status bar data.
    pub status: StatusState,
    /// Current text input contents (managed by tui-textarea in the view layer).
    pub input_text: String,
    /// Activity indicator state.
    pub activity: ActivityState,
    /// Connection status.
    pub connection: ConnectionStatus,
    /// Current UI mode.
    pub mode: UiMode,
    /// Mutable state for the active approval overlay (if any).
    pub approval: Option<ApprovalState>,
    /// Terminal dimensions.
    pub terminal_width: u16,
    pub terminal_height: u16,
    /// F1 debug banner state.
    pub debug: DebugState,
    /// Vessel ID (populated after initial status fetch).
    pub vessel_id: String,
    /// Active conversation ID for session persistence.
    pub current_conversation_id: String,
    /// Whether the TUI should exit.
    pub should_quit: bool,
}

impl App {
    /// Create a new App with default state.
    pub fn new() -> Self {
        Self {
            conversation: ConversationState::new(),
            status: StatusState::default(),
            input_text: String::new(),
            activity: ActivityState::default(),
            connection: ConnectionStatus::Connecting,
            mode: UiMode::Normal,
            approval: None,
            terminal_width: 80,
            terminal_height: 24,
            debug: DebugState::default(),
            vessel_id: String::new(),
            current_conversation_id: String::new(),
            should_quit: false,
        }
    }
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

/// Messages that drive state transitions.
#[allow(clippy::enum_variant_names, clippy::large_enum_variant)]
#[derive(Debug)]
pub enum Message {
    /// A keyboard event.
    Key(KeyEvent),
    /// Pasted text (bracketed paste mode).
    Paste(String),
    /// Terminal resize.
    Resize(u16, u16),
    /// A LiveEvent received from the WebSocket.
    WsEvent(LiveEvent),
    /// WebSocket connection was lost.
    WsDisconnected,
    /// WebSocket connection was re-established.
    WsReconnected,
    /// A reconnection attempt result.
    ReconnectAttempt(Result<(), String>),
    /// Spinner animation tick (100ms interval).
    SpinnerTick,
    /// Incremental text delta from an LLM streaming response.
    TextDelta(String),
    /// Result of submitting a message via the inbox.
    MessageSent(Result<(), String>),
    /// Result of fetching an artifact.
    ArtifactFetched {
        artifact_id: String,
        result: Result<serde_json::Value, String>,
    },
    /// An approval action was completed (overlay resolved).
    #[allow(dead_code)]
    ApprovalResolved(ApprovalAction),
    /// Plan content was fetched for a plan approval overlay.
    PlanContentFetched {
        plan_draft_id: String,
        result: Result<String, String>,
    },
    /// Current plan status was fetched to discover the active draft ID.
    PlanStatusFetched(Result<Option<String>, String>),
    /// Session was saved successfully.
    SessionSaved(Result<(), String>),
}

/// Side effects produced by update() for the event loop to execute.
#[derive(Debug)]
pub enum SideEffect {
    /// Send a message to the vessel via POST /api/v1/inbox.
    SendMessage(String),
    /// Attempt to reconnect the WebSocket.
    #[allow(dead_code)]
    Reconnect,
    /// Reconnect WebSocket with exponential backoff.
    ReconnectWithBackoff,
    /// Fetch an artifact by ID via GET /api/v1/artifacts/{id}.
    #[allow(dead_code)]
    FetchArtifact(String),
    /// Submit an answer to a question via POST /api/v1/questions/{id}/answer.
    SubmitAnswer { question_id: String, answer: String },
    /// Approve a plan via POST /api/v1/plan/approve.
    ApprovePlan { plan_draft_id: String },
    /// Cancel/deny a plan via POST /api/v1/plan/cancel.
    DenyPlan,
    /// Fetch plan draft content via GET /api/v1/artifacts/{id}.
    FetchPlanDraft(String),
    /// Fetch current plan status to discover the active draft.
    FetchPlanStatus,
    /// Save session state to disk before exiting.
    SaveSession,
    /// Send a cancellation message to the vessel inbox.
    SendCancellation,
    /// Exit the TUI.
    Quit,
}

/// Pure update function: takes current state + message, returns side effects.
///
/// This function MUST NOT perform any I/O. All I/O is expressed as SideEffect
/// values that the event loop executes after update returns.
pub fn update(app: &mut App, msg: Message) -> Vec<SideEffect> {
    let mut effects = Vec::new();

    match msg {
        Message::Key(key_event) => {
            handle_key(app, key_event, &mut effects);
        }
        Message::Paste(text) => {
            app.input_text.push_str(&text);
        }
        Message::Resize(w, h) => {
            app.terminal_width = w;
            app.terminal_height = h;
        }
        Message::WsEvent(event) => {
            let ws_effects = handle_ws_event(app, event);
            effects.extend(ws_effects);
        }
        Message::WsDisconnected => {
            app.connection = ConnectionStatus::Disconnected;
            app.activity.label = "Reconnecting...".into();
            app.activity.is_active = true;
            effects.push(SideEffect::ReconnectWithBackoff);
        }
        Message::WsReconnected => {
            app.connection = ConnectionStatus::Connected;
            app.activity.label = String::new();
            app.activity.is_active = false;
        }
        Message::ReconnectAttempt(result) => match result {
            Ok(()) => {
                app.connection = ConnectionStatus::Connected;
                app.activity.label = String::new();
                app.activity.is_active = false;
            }
            Err(err) => {
                app.activity.label = format!("Reconnecting... ({err})");
                app.activity.is_active = true;
            }
        },
        Message::SpinnerTick => {
            if app.activity.is_active {
                app.activity.spinner_phase =
                    (app.activity.spinner_phase + 1) % SPINNER_FRAMES.len();
            }
        }
        Message::TextDelta(delta) => {
            append_text_delta(app, &delta);
        }
        Message::ApprovalResolved(_action) => {}
        Message::MessageSent(result) => {
            if let Err(err) = result {
                app.conversation.add_block(Block::SystemNote {
                    text: format!("Failed to send message: {err}"),
                    severity: NoteSeverity::Warning,
                });
            }
        }
        Message::ArtifactFetched {
            artifact_id,
            result,
        } => {
            handle_artifact_fetched(app, &artifact_id, result);
        }
        Message::PlanContentFetched {
            plan_draft_id,
            result,
        } => {
            handle_plan_content_fetched(app, &plan_draft_id, result);
        }
        Message::PlanStatusFetched(result) => {
            handle_plan_status_fetched(app, result, &mut effects);
        }
        Message::SessionSaved(result) => {
            if let Err(err) = result {
                tracing::warn!("failed to save session: {err}");
            }
        }
    }

    effects
}

/// Handle keyboard input.
fn handle_key(app: &mut App, key: KeyEvent, effects: &mut Vec<SideEffect>) {
    match &app.mode {
        UiMode::Normal => handle_key_normal(app, key, effects),
        UiMode::Approval(_) => handle_key_approval(app, key, effects),
    }
}

/// Handle keyboard input in Normal mode.
fn handle_key_normal(app: &mut App, key: KeyEvent, effects: &mut Vec<SideEffect>) {
    match (key.code, key.modifiers) {
        (KeyCode::Char('d'), KeyModifiers::CONTROL) => {
            if app.input_text.is_empty() {
                app.should_quit = true;
                effects.push(SideEffect::SaveSession);
                effects.push(SideEffect::Quit);
            }
        }
        (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
            if app.activity.is_active {
                let context = super::widgets::approval::ApprovalContext {
                    question_id: String::new(),
                    title: "Interrupt agent?".into(),
                    description: "The agent is currently working. Interrupt and exit?".into(),
                    options: confirmation_options(),
                    kind: super::widgets::approval::ApprovalKind::Confirmation,
                    plan_content: None,
                    plan_draft_id: None,
                };
                app.mode = UiMode::Approval(context);
                app.approval = Some(super::widgets::approval::ApprovalState::new());
            } else {
                app.should_quit = true;
                effects.push(SideEffect::SaveSession);
                effects.push(SideEffect::Quit);
            }
        }
        (KeyCode::F(1), _) => {
            app.debug.visible = !app.debug.visible;
        }
        (KeyCode::Enter, modifiers) if !modifiers.contains(KeyModifiers::SHIFT) => {
            let text = app.input_text.trim().to_string();
            if !text.is_empty() {
                app.conversation.add_block(Block::UserMessage {
                    text: text.clone(),
                    timestamp: chrono::Utc::now(),
                });
                app.input_text.clear();
                effects.push(SideEffect::SendMessage(text));
            }
        }
        (KeyCode::Enter, modifiers) if modifiers.contains(KeyModifiers::SHIFT) => {
            app.input_text.push('\n');
        }
        (KeyCode::PageUp, _) => {
            app.conversation.scroll_up(10);
        }
        (KeyCode::PageDown, _) => {
            app.conversation.scroll_down(10);
        }
        (KeyCode::End, _) => {
            app.conversation.jump_to_bottom();
        }
        (KeyCode::Char(c), modifiers) => {
            if modifiers.is_empty() || modifiers == KeyModifiers::SHIFT {
                app.input_text.push(c);
            }
        }
        (KeyCode::Backspace, _) => {
            app.input_text.pop();
        }
        _ => {}
    }
}

/// Handle keyboard input in Approval mode.
fn handle_key_approval(app: &mut App, key: KeyEvent, effects: &mut Vec<SideEffect>) {
    let context = match &app.mode {
        UiMode::Approval(ctx) => ctx.clone(),
        UiMode::Normal => return,
    };
    let is_text_mode = match app.approval.as_ref() {
        Some(state) => state.is_text_mode,
        None => return,
    };

    if is_text_mode {
        match key.code {
            KeyCode::Enter => {
                let (value, appended) = {
                    let state = app.approval.as_ref().expect("approval state exists");
                    let value = if context.options.is_empty() {
                        state.text_input.clone()
                    } else {
                        context.options[state.selected_index].value.clone()
                    };
                    let appended = if !state.text_input.is_empty() && !context.options.is_empty() {
                        Some(state.text_input.clone())
                    } else {
                        None
                    };
                    (value, appended)
                };
                resolve_approval(
                    app,
                    effects,
                    ApprovalAction::Selected {
                        value,
                        appended_text: appended,
                    },
                );
            }
            KeyCode::Esc => {
                if context.options.is_empty() {
                    resolve_approval(app, effects, ApprovalAction::Cancelled);
                } else if let Some(s) = app.approval.as_mut() {
                    s.is_text_mode = false;
                }
            }
            KeyCode::Tab => {
                if !context.options.is_empty() {
                    if let Some(s) = app.approval.as_mut() {
                        s.is_text_mode = false;
                    }
                }
            }
            KeyCode::Backspace => {
                if let Some(state) = app.approval.as_mut() {
                    state.text_input.pop();
                }
            }
            KeyCode::Char(c) => {
                if let Some(state) = app.approval.as_mut() {
                    state.text_input.push(c);
                }
            }
            _ => {}
        }
        return;
    }

    match key.code {
        KeyCode::Up => {
            if let Some(state) = app.approval.as_mut() {
                state.select_prev(context.options.len());
            }
        }
        KeyCode::Down => {
            if let Some(state) = app.approval.as_mut() {
                state.select_next(context.options.len());
            }
        }
        KeyCode::Enter => {
            if !context.options.is_empty() {
                let selected_index = app
                    .approval
                    .as_ref()
                    .map(|state| state.selected_index)
                    .unwrap_or(0);
                let selected = &context.options[selected_index];
                resolve_approval(
                    app,
                    effects,
                    ApprovalAction::Selected {
                        value: selected.value.clone(),
                        appended_text: None,
                    },
                );
            }
        }
        KeyCode::Esc => {
            resolve_approval(app, effects, ApprovalAction::Cancelled);
        }
        KeyCode::Tab => {
            if let Some(s) = app.approval.as_mut() {
                s.is_text_mode = true;
            }
        }
        KeyCode::PageUp => {
            if let Some(s) = app.approval.as_mut() {
                s.plan_scroll = s.plan_scroll.saturating_sub(1);
            }
        }
        KeyCode::PageDown => {
            if let Some(s) = app.approval.as_mut() {
                s.plan_scroll = s.plan_scroll.saturating_add(1);
            }
        }
        KeyCode::Char(c) => {
            if let Some(option) = context.options.iter().find(|o| o.hotkey == Some(c)) {
                resolve_approval(
                    app,
                    effects,
                    ApprovalAction::Selected {
                        value: option.value.clone(),
                        appended_text: None,
                    },
                );
            }
        }
        _ => {}
    }
}

/// Resolve an approval action: transition back to Normal mode and emit side effects.
fn resolve_approval(app: &mut App, effects: &mut Vec<SideEffect>, action: ApprovalAction) {
    let context = match &app.mode {
        UiMode::Approval(ctx) => ctx.clone(),
        UiMode::Normal => return,
    };

    match (&context.kind, &action) {
        (
            ApprovalKind::PlanApproval,
            ApprovalAction::Selected {
                value,
                appended_text,
            },
        ) => {
            if value == "approve" {
                if let Some(ref draft_id) = context.plan_draft_id {
                    effects.push(SideEffect::ApprovePlan {
                        plan_draft_id: draft_id.clone(),
                    });
                }
            } else if value == "deny" {
                effects.push(SideEffect::DenyPlan);
            } else if value == "edit" {
                let feedback = appended_text.clone().unwrap_or_default();
                if !feedback.is_empty() {
                    effects.push(SideEffect::SendMessage(format!(
                        "Plan feedback: {feedback}"
                    )));
                }
                effects.push(SideEffect::DenyPlan);
            }
        }
        (ApprovalKind::Confirmation, ApprovalAction::Selected { value, .. }) => {
            if value == "yes" {
                app.should_quit = true;
                effects.push(SideEffect::SendCancellation);
                effects.push(SideEffect::SaveSession);
                effects.push(SideEffect::Quit);
            }
        }
        (
            _,
            ApprovalAction::Selected {
                value,
                appended_text,
            },
        ) => {
            let answer = if let Some(ref text) = appended_text {
                format!("{value}: {text}")
            } else {
                value.clone()
            };
            effects.push(SideEffect::SubmitAnswer {
                question_id: context.question_id.clone(),
                answer,
            });
        }
        (ApprovalKind::PlanApproval, ApprovalAction::Cancelled) => {
            effects.push(SideEffect::DenyPlan);
        }
        (ApprovalKind::Confirmation, ApprovalAction::Cancelled) => {}
        (_, ApprovalAction::Cancelled) => {
            effects.push(SideEffect::SubmitAnswer {
                question_id: context.question_id.clone(),
                answer: "deny".into(),
            });
        }
    }

    app.mode = UiMode::Normal;
    app.approval = None;
}

fn append_text_delta(app: &mut App, delta: &str) {
    let should_create_new = match app.conversation.blocks().last() {
        Some(Block::AgentText { is_streaming, .. }) => !is_streaming,
        _ => true,
    };

    if should_create_new {
        app.conversation.add_block(Block::AgentText {
            text: delta.to_string(),
            is_streaming: true,
        });
    } else if let Some(Block::AgentText { text, .. }) = app.conversation.blocks_mut().last_mut() {
        text.push_str(delta);
        app.conversation.notify_content_changed();
    }

    app.activity.is_streaming = true;
    app.activity.streaming_tokens += 1;
    app.activity.label = format!("Streaming ({} tokens)", app.activity.streaming_tokens);
    app.activity.is_active = true;
}

fn finalize_streaming_block(app: &mut App) {
    for block in app.conversation.blocks_mut().iter_mut().rev() {
        if let Block::AgentText { is_streaming, .. } = block {
            if *is_streaming {
                *is_streaming = false;
                break;
            }
        }
    }
    app.activity.is_streaming = false;
    app.activity.streaming_tokens = 0;
}

/// Handle a WebSocket LiveEvent.
fn handle_ws_event(app: &mut App, event: LiveEvent) -> Vec<SideEffect> {
    let mut effects = Vec::new();

    match event.event_type {
        EventType::InnerLoopStarted => {
            app.activity.label = "Thinking...".into();
            app.activity.is_active = true;
            app.activity.spinner_phase = 0;
            app.activity.is_streaming = false;
            app.activity.streaming_tokens = 0;
            app.debug.inner_loop_active = true;
        }
        EventType::LlmTextDelta => {
            if let Some(ref delta) = event.text_delta {
                append_text_delta(app, delta);
            }
        }
        EventType::InnerLoopStep => {
            if let Some(ref detail) = event.inner_loop_detail {
                finalize_streaming_block(app);
                let tool_name = detail.tool_name.clone().unwrap_or_default();
                let outcome =
                    tool_outcome_from_str(detail.tool_outcome.as_deref().unwrap_or_default());

                app.conversation.add_block(Block::ToolCall {
                    tool_name: tool_name.clone(),
                    args_summary: format!("step {}/{}", detail.step_number, detail.max_steps),
                    outcome,
                    collapsed: true,
                    token_cost: Some(detail.tokens_this_step),
                });

                app.status.step_summary =
                    format!("step {}/{}", detail.step_number, detail.max_steps);
                app.status.token_summary = format_token_count(detail.tokens_total);
                app.activity.label = format!("Running {tool_name}...");
                app.activity.is_active = true;

                app.debug.step_count = format!("{}/{} steps", detail.step_number, detail.max_steps);
                app.debug.token_count = format!(
                    "{}/{} tok",
                    detail.tokens_total,
                    app.debug
                        .initial_token_budget
                        .unwrap_or(detail.tokens_total)
                );

                if app.debug.initial_step_limit.is_none() {
                    app.debug.initial_step_limit = Some(detail.max_steps as u64);
                }

                if let Some(max_steps) = app.debug.initial_step_limit {
                    if max_steps > 0 {
                        let used = detail.step_number as u64;
                        app.debug.budget.step_percent_used =
                            ((used * 100) / max_steps).min(100) as u8;
                        app.debug.budget.step_label =
                            format!("{}/{}", detail.step_number, max_steps);
                    }
                }
                if let Some(max_tokens) = app.debug.initial_token_budget {
                    if max_tokens > 0 {
                        let used = detail.tokens_total;
                        app.debug.budget.token_percent_used =
                            ((used * 100) / max_tokens).min(100) as u8;
                        app.debug.budget.token_label =
                            format!("{}/{}", detail.tokens_total, max_tokens);
                    }
                }
            }
        }
        EventType::InnerLoopCompleted => {
            if let Some(ref detail) = event.inner_loop_detail {
                let reason = detail
                    .completion_reason
                    .clone()
                    .unwrap_or_else(|| "unknown".into());
                app.conversation.add_block(Block::SystemNote {
                    text: format!(
                        "Completed: {} ({} steps, {} tokens)",
                        reason, detail.step_number, detail.tokens_total,
                    ),
                    severity: NoteSeverity::Info,
                });
                app.status.token_summary = format_token_count(detail.tokens_total);
            }

            if let Some(ref diff) = event.diff_summary {
                let summary = DiffSummaryData {
                    files_modified: diff.files_modified,
                    lines_added: diff.lines_added,
                    lines_removed: diff.lines_removed,
                    net_delta: diff.net_delta,
                    files: diff
                        .files
                        .iter()
                        .map(|file| DiffFileSummary {
                            path: file.path.clone(),
                            lines_added: file.lines_added,
                            lines_removed: file.lines_removed,
                            operation: format!("{:?}", file.operation).to_lowercase(),
                        })
                        .collect(),
                };
                app.conversation.add_block(Block::Diff {
                    summary,
                    full_text: None,
                });
            }

            finalize_streaming_block(app);
            app.activity.label = String::new();
            app.activity.is_active = false;
            app.debug.inner_loop_active = false;
        }
        EventType::ActionExecuted => {
            let summary = &event.summary;
            app.conversation.add_block(Block::ToolCall {
                tool_name: extract_tool_name(summary),
                args_summary: truncate_str(summary, 80),
                outcome: ToolOutcome::Success,
                collapsed: true,
                token_cost: None,
            });
        }
        EventType::TickStarted => {
            if let Some(tick) = event.tick_number {
                app.debug.tick_number = tick;
            }
        }
        EventType::TickCompleted => {
            app.conversation.add_block(Block::SystemNote {
                text: event.summary.clone(),
                severity: NoteSeverity::Info,
            });

            if let Some(ref snapshot) = event.snapshot {
                app.status.vessel_mode = format!("{:?}", snapshot.vessel_mode).to_lowercase();
                app.status.token_summary =
                    format_token_count(snapshot.budget_status.total_tokens_remaining());

                if let Some(ref plan) = snapshot.plan {
                    app.conversation
                        .add_block(Block::PlanSummary { plan: plan.clone() });
                }

                app.debug.tick_number = snapshot.tick_number;
                app.debug.threads = snapshot
                    .thread_summaries
                    .iter()
                    .map(|ts| {
                        let state = match ts.status {
                            exoskeleton_core::thread::ThreadStatus::Active => {
                                if let Some(ref summary) = ts.last_output_summary {
                                    format!(
                                        "contributed: \"{}\"",
                                        if summary.len() > 40 {
                                            format!("{}...", &summary[..37])
                                        } else {
                                            summary.clone()
                                        }
                                    )
                                } else {
                                    "active".to_string()
                                }
                            }
                            exoskeleton_core::thread::ThreadStatus::Suspended => {
                                "suspended".to_string()
                            }
                            _ => "idle".to_string(),
                        };
                        let contributed = ts.last_output_summary.is_some()
                            && ts.status == exoskeleton_core::thread::ThreadStatus::Active;
                        DebugThreadInfo {
                            name: ts.name.clone(),
                            state,
                            contributed,
                        }
                    })
                    .collect();

                let total_tokens = snapshot.budget_status.total_tokens_remaining();
                if app.debug.initial_token_budget.is_none() && total_tokens < u64::MAX {
                    app.debug.initial_token_budget = Some(total_tokens);
                }

                if let Some(ref plan) = snapshot.plan {
                    let completed = plan
                        .tasks
                        .iter()
                        .filter(|t| t.status == exoskeleton_core::plan::PlanTaskStatus::Completed)
                        .count();
                    let total = plan.tasks.len();
                    let progress_icons: String = plan
                        .tasks
                        .iter()
                        .map(|t| match t.status {
                            exoskeleton_core::plan::PlanTaskStatus::Completed => '✓',
                            exoskeleton_core::plan::PlanTaskStatus::InProgress => '▸',
                            exoskeleton_core::plan::PlanTaskStatus::Pending => '○',
                            exoskeleton_core::plan::PlanTaskStatus::Failed => '✗',
                            exoskeleton_core::plan::PlanTaskStatus::Blocked => '⛔',
                            exoskeleton_core::plan::PlanTaskStatus::Skipped => '‒',
                        })
                        .collect();
                    let objective = if plan.objective.len() > 50 {
                        format!("{}...", &plan.objective[..47])
                    } else {
                        plan.objective.clone()
                    };
                    app.debug.plan_summary = Some(DebugPlanSummary {
                        completed,
                        total,
                        progress_icons,
                        objective,
                    });
                } else {
                    app.debug.plan_summary = None;
                }

                if !snapshot.status.is_terminal() {
                    app.activity.label = "Governance cycle...".into();
                    app.activity.is_active = true;
                }
            }
        }
        EventType::QuestionAsked => {
            if let Some(ref detail) = event.question_detail {
                let options: Vec<ApprovalOption> = match &detail.choices {
                    Some(choices) => choices
                        .iter()
                        .enumerate()
                        .map(|(i, c)| ApprovalOption {
                            label: c.clone(),
                            hotkey: None,
                            value: format!("{i}"),
                        })
                        .collect(),
                    None => Vec::new(),
                };

                let context = ApprovalContext {
                    question_id: detail.question_id.clone(),
                    title: "Agent question".into(),
                    description: detail.question.clone(),
                    options: options.clone(),
                    kind: ApprovalKind::Question,
                    plan_content: None,
                    plan_draft_id: None,
                };

                let state = if options.is_empty() {
                    ApprovalState::new_text_mode()
                } else {
                    ApprovalState::new()
                };

                app.mode = UiMode::Approval(context);
                app.approval = Some(state);
            }
        }
        EventType::PolicyApprovalRequired => {
            if let Some(ref detail) = event.policy_detail {
                if let Some(ref q_detail) = event.question_detail {
                    let context = ApprovalContext {
                        question_id: q_detail.question_id.clone(),
                        title: "Tool requires approval".into(),
                        description: format!("{}\n{}", detail.tool_name, detail.rule),
                        options: tool_approval_options(&detail.tool_name),
                        kind: ApprovalKind::ToolApproval,
                        plan_content: None,
                        plan_draft_id: None,
                    };
                    app.mode = UiMode::Approval(context);
                    app.approval = Some(ApprovalState::new());
                } else {
                    app.conversation.add_block(Block::ToolCall {
                        tool_name: detail.tool_name.clone(),
                        args_summary: detail.rule.clone(),
                        outcome: ToolOutcome::PolicyDenied,
                        collapsed: true,
                        token_cost: None,
                    });
                }
            }
        }
        EventType::PlanModeTransition => {
            if let Some(ref detail) = event.plan_mode_detail {
                app.conversation.add_block(Block::SystemNote {
                    text: format!("Mode: {} -> {}", detail.from, detail.to),
                    severity: NoteSeverity::Info,
                });
                app.status.vessel_mode = detail.to.clone();

                if detail.to == "planning" {
                    if let Some(ref draft_id) = detail.plan_draft_id {
                        effects.push(SideEffect::FetchPlanDraft(draft_id.clone()));
                    } else {
                        effects.push(SideEffect::FetchPlanStatus);
                    }
                }
            }
        }
        EventType::PolicyApprovalGranted => {
            if let Some(ref detail) = event.policy_detail {
                let outcome = if event.summary.contains("allow") {
                    "allow"
                } else if event.summary.contains("deny") {
                    "deny"
                } else {
                    "ask"
                };
                let existing = app
                    .debug
                    .policies
                    .iter_mut()
                    .find(|p| p.tool_pattern == detail.tool_name);
                if let Some(policy) = existing {
                    policy.outcome = outcome.to_string();
                } else {
                    app.debug.policies.push(DebugPolicyInfo {
                        tool_pattern: detail.tool_name.clone(),
                        outcome: outcome.to_string(),
                    });
                }
            }
            if !event.summary.is_empty() {
                app.conversation.add_block(Block::SystemNote {
                    text: event.summary.clone(),
                    severity: NoteSeverity::Info,
                });
            }
        }
        _ => {
            if !event.summary.is_empty() {
                app.conversation.add_block(Block::SystemNote {
                    text: event.summary.clone(),
                    severity: NoteSeverity::Info,
                });
            }
        }
    }

    effects
}

/// Handle a fetched artifact (currently used for CodeDiff full text).
fn handle_artifact_fetched(
    app: &mut App,
    _artifact_id: &str,
    result: Result<serde_json::Value, String>,
) {
    match result {
        Ok(value) => {
            let diff_text = value
                .get("content")
                .or_else(|| value.get("diff_text"))
                .and_then(|v| v.as_str())
                .map(String::from);

            if let Some(text) = diff_text {
                for block in app.conversation.blocks_mut().iter_mut().rev() {
                    if let Block::Diff { full_text, .. } = block {
                        if full_text.is_none() {
                            *full_text = Some(text);
                            break;
                        }
                    }
                }
            }
        }
        Err(err) => {
            tracing::warn!("failed to fetch artifact: {err}");
        }
    }
}

/// Handle fetched plan draft content — triggers plan approval overlay.
fn handle_plan_content_fetched(app: &mut App, plan_draft_id: &str, result: Result<String, String>) {
    match result {
        Ok(content) => {
            let context = ApprovalContext {
                question_id: String::new(),
                title: "Plan approval required".into(),
                description: String::new(),
                options: plan_approval_options(),
                kind: ApprovalKind::PlanApproval,
                plan_content: Some(content),
                plan_draft_id: Some(plan_draft_id.to_string()),
            };
            app.mode = UiMode::Approval(context);
            app.approval = Some(ApprovalState::new());
        }
        Err(err) => {
            app.conversation.add_block(Block::SystemNote {
                text: format!("Failed to fetch plan draft: {err}"),
                severity: NoteSeverity::Warning,
            });
        }
    }
}

/// Handle fetched plan status — discover active draft ID when transition events omit it.
fn handle_plan_status_fetched(
    app: &mut App,
    result: Result<Option<String>, String>,
    effects: &mut Vec<SideEffect>,
) {
    match result {
        Ok(Some(plan_draft_id)) => effects.push(SideEffect::FetchPlanDraft(plan_draft_id)),
        Ok(None) => {
            app.conversation.add_block(Block::SystemNote {
                text: "Planning mode entered, but no plan draft is available yet".into(),
                severity: NoteSeverity::Warning,
            });
        }
        Err(err) => {
            app.conversation.add_block(Block::SystemNote {
                text: format!("Failed to fetch plan status: {err}"),
                severity: NoteSeverity::Warning,
            });
        }
    }
}

fn tool_outcome_from_str(outcome: &str) -> ToolOutcome {
    match outcome {
        "success" => ToolOutcome::Success,
        "pending" => ToolOutcome::Pending,
        "denied" | "policy_denied" => ToolOutcome::PolicyDenied,
        other => ToolOutcome::Error(other.to_string()),
    }
}

/// Format a token count for display (e.g., 4231 -> "4,231 tok").
fn format_token_count(tokens: u64) -> String {
    if tokens >= 1_000_000 {
        format!("{:.1}M tok", tokens as f64 / 1_000_000.0)
    } else if tokens >= 1_000 {
        format!("{:.1}k tok", tokens as f64 / 1_000.0)
    } else {
        format!("{tokens} tok")
    }
}

/// Extract tool name from an ActionExecuted summary string.
///
/// Summaries are typically in the form "tool_name: description" or similar.
fn extract_tool_name(summary: &str) -> String {
    if let Some(colon_pos) = summary.find(':') {
        let candidate = summary[..colon_pos].trim();
        if candidate.contains('.') || candidate.contains('_') {
            return candidate.to_string();
        }
    }
    "action".into()
}

/// Truncate a string to max_len, adding "..." if truncated.
fn truncate_str(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        format!("{}...", &s[..max_len.saturating_sub(3)])
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use exoskeleton_core::{
        CodeDiffOperation, DiffSummary, EventType, FileDiffEntry, InnerLoopStepDetail, LiveEvent,
        PolicyDetail,
    };

    use crate::code::widgets::approval::{ApprovalKind, ApprovalState};

    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn key_with_mods(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    // ── TUI-T1: update_key_enter_sends_message ──

    #[test]
    fn update_key_enter_sends_message() {
        let mut app = App::new();
        app.input_text = "Add a test".into();

        let effects = update(&mut app, Message::Key(key(KeyCode::Enter)));

        assert_eq!(effects.len(), 1);
        match &effects[0] {
            SideEffect::SendMessage(text) => assert_eq!(text, "Add a test"),
            other => panic!("expected SendMessage, got {other:?}"),
        }
        assert!(app.input_text.is_empty());
        assert_eq!(app.conversation.len(), 1);
    }

    // ── TUI-T2: update_key_enter_empty_input_no_effect ──

    #[test]
    fn update_key_enter_empty_input_no_effect() {
        let mut app = App::new();
        app.input_text = String::new();

        let effects = update(&mut app, Message::Key(key(KeyCode::Enter)));

        assert!(effects.is_empty());
        assert_eq!(app.conversation.len(), 0);
    }

    // ── TUI-T3: update_ws_event_inner_loop_step_adds_tool_call_block ──

    #[test]
    fn update_ws_event_inner_loop_step_adds_tool_call_block() {
        let mut app = App::new();
        let event = LiveEvent {
            event_type: EventType::InnerLoopStep,
            summary: "Step 1/5: code.read (success)".into(),
            inner_loop_detail: Some(InnerLoopStepDetail {
                step_number: 1,
                max_steps: 5,
                tool_name: Some("code.read".into()),
                tool_outcome: Some("success".into()),
                tokens_this_step: 500,
                tokens_total: 500,
                completion_reason: None,
            }),
            ..LiveEvent::new(Some(1))
        };

        let effects = update(&mut app, Message::WsEvent(event));

        assert!(effects.is_empty());
        assert_eq!(app.conversation.len(), 1);
        match &app.conversation.blocks()[0] {
            Block::ToolCall {
                tool_name,
                outcome,
                token_cost,
                ..
            } => {
                assert_eq!(tool_name, "code.read");
                assert_eq!(*outcome, ToolOutcome::Success);
                assert_eq!(*token_cost, Some(500));
            }
            other => panic!("expected ToolCall block, got {other:?}"),
        }
    }

    // ── TUI-T4: update_ws_event_tick_completed_adds_system_note ──

    #[test]
    fn update_ws_event_tick_completed_adds_system_note() {
        let mut app = App::new();
        let event = LiveEvent {
            event_type: EventType::TickCompleted,
            summary: "Tick 7 completed".into(),
            ..LiveEvent::new(Some(7))
        };

        let effects = update(&mut app, Message::WsEvent(event));

        assert!(effects.is_empty());
        assert_eq!(app.conversation.len(), 1);
        match &app.conversation.blocks()[0] {
            Block::SystemNote { text, severity } => {
                assert!(text.contains("Tick 7"));
                assert_eq!(*severity, NoteSeverity::Info);
            }
            other => panic!("expected SystemNote block, got {other:?}"),
        }
    }

    // ── TUI-T5: update_ws_disconnected_sets_connection_state ──

    #[test]
    fn update_ws_disconnected_sets_connection_state() {
        let mut app = App::new();
        app.connection = ConnectionStatus::Connected;

        let effects = update(&mut app, Message::WsDisconnected);

        assert_eq!(app.connection, ConnectionStatus::Disconnected);
        assert!(app.activity.label.contains("Reconnecting"));
        assert!(app.activity.is_active);
        assert!(effects
            .iter()
            .any(|e| matches!(e, SideEffect::ReconnectWithBackoff)));
    }

    // ── TUI-T6: update_resize_updates_dimensions ──

    #[test]
    fn update_resize_updates_dimensions() {
        let mut app = App::new();
        assert_eq!(app.terminal_width, 80);
        assert_eq!(app.terminal_height, 24);

        let effects = update(&mut app, Message::Resize(120, 40));

        assert!(effects.is_empty());
        assert_eq!(app.terminal_width, 120);
        assert_eq!(app.terminal_height, 40);
    }

    // ── TUI-T15: update_spinner_tick_advances_phase ──

    #[test]
    fn update_spinner_tick_advances_phase() {
        let mut app = App::new();
        app.activity.is_active = true;
        app.activity.spinner_phase = 0;

        update(&mut app, Message::SpinnerTick);
        assert_eq!(app.activity.spinner_phase, 1);

        update(&mut app, Message::SpinnerTick);
        assert_eq!(app.activity.spinner_phase, 2);

        app.activity.spinner_phase = SPINNER_FRAMES.len() - 1;
        update(&mut app, Message::SpinnerTick);
        assert_eq!(app.activity.spinner_phase, 0);
    }

    // ── TUI-T16: initial_app_state_is_normal_mode ──

    #[test]
    fn initial_app_state_is_normal_mode() {
        let app = App::new();
        assert_eq!(app.mode, UiMode::Normal);
        assert!(app.input_text.is_empty());
        assert_eq!(app.connection, ConnectionStatus::Connecting);
        assert!(!app.should_quit);
        assert!(app.conversation.is_empty());
        assert!(!app.debug.visible);
    }

    #[test]
    fn text_delta_creates_new_agent_text_if_none_exists() {
        let mut app = App::new();
        assert!(app.conversation.is_empty());

        update(&mut app, Message::TextDelta("Hello".into()));

        assert_eq!(app.conversation.len(), 1);
        match &app.conversation.blocks()[0] {
            Block::AgentText { text, is_streaming } => {
                assert_eq!(text, "Hello");
                assert!(*is_streaming);
            }
            other => panic!("expected AgentText, got {other:?}"),
        }
    }

    #[test]
    fn text_delta_appends_to_existing_streaming_block() {
        let mut app = App::new();
        app.conversation.add_block(Block::AgentText {
            text: "Hello".into(),
            is_streaming: true,
        });

        update(&mut app, Message::TextDelta(" world".into()));

        assert_eq!(app.conversation.len(), 1);
        match &app.conversation.blocks()[0] {
            Block::AgentText { text, is_streaming } => {
                assert_eq!(text, "Hello world");
                assert!(*is_streaming);
            }
            other => panic!("expected AgentText, got {other:?}"),
        }
    }

    #[test]
    fn text_delta_does_not_append_to_non_streaming_block() {
        let mut app = App::new();
        app.conversation.add_block(Block::AgentText {
            text: "Previous response".into(),
            is_streaming: false,
        });

        update(&mut app, Message::TextDelta("New text".into()));

        assert_eq!(app.conversation.len(), 2);
        match &app.conversation.blocks()[1] {
            Block::AgentText { text, is_streaming } => {
                assert_eq!(text, "New text");
                assert!(*is_streaming);
            }
            other => panic!("expected new AgentText, got {other:?}"),
        }
    }

    // ── TUI-T19: update_ws_event_inner_loop_started_sets_activity ──

    #[test]
    fn update_ws_event_inner_loop_started_sets_activity() {
        let mut app = App::new();
        let event = LiveEvent {
            event_type: EventType::InnerLoopStarted,
            summary: "Inner loop started".into(),
            ..LiveEvent::new(Some(1))
        };

        update(&mut app, Message::WsEvent(event));

        assert_eq!(app.activity.label, "Thinking...");
        assert!(app.activity.is_active);
    }

    #[test]
    fn inner_loop_started_clears_streaming_state() {
        let mut app = App::new();
        app.activity.is_streaming = true;
        app.activity.streaming_tokens = 42;

        let event = LiveEvent {
            event_type: EventType::InnerLoopStarted,
            summary: "Inner loop started".into(),
            ..LiveEvent::new(Some(1))
        };
        update(&mut app, Message::WsEvent(event));

        assert!(!app.activity.is_streaming);
        assert_eq!(app.activity.streaming_tokens, 0);
    }

    // ── TUI-T20: update_ws_event_inner_loop_completed_sets_idle ──

    #[test]
    fn update_ws_event_inner_loop_completed_sets_idle() {
        let mut app = App::new();
        app.activity.is_active = true;
        app.activity.label = "Running code.edit...".into();

        let event = LiveEvent {
            event_type: EventType::InnerLoopCompleted,
            summary: "Inner loop completed".into(),
            inner_loop_detail: Some(InnerLoopStepDetail {
                step_number: 5,
                max_steps: 25,
                tool_name: None,
                tool_outcome: None,
                tokens_this_step: 0,
                tokens_total: 4500,
                completion_reason: Some("agent_complete".into()),
            }),
            ..LiveEvent::new(Some(1))
        };

        update(&mut app, Message::WsEvent(event));

        assert!(!app.activity.is_active);
        assert!(app.activity.label.is_empty());
    }

    #[test]
    fn inner_loop_completed_finalizes_streaming_block() {
        let mut app = App::new();
        app.conversation.add_block(Block::AgentText {
            text: "streaming content".into(),
            is_streaming: true,
        });
        app.activity.is_streaming = true;
        app.activity.streaming_tokens = 10;

        let event = LiveEvent {
            event_type: EventType::InnerLoopCompleted,
            summary: "Completed".into(),
            inner_loop_detail: Some(InnerLoopStepDetail {
                step_number: 3,
                max_steps: 25,
                tool_name: None,
                tool_outcome: None,
                tokens_this_step: 0,
                tokens_total: 2000,
                completion_reason: Some("agent_complete".into()),
            }),
            ..LiveEvent::new(Some(1))
        };
        update(&mut app, Message::WsEvent(event));

        let last_agent = app
            .conversation
            .blocks()
            .iter()
            .rev()
            .find(|b| matches!(b, Block::AgentText { .. }));
        match last_agent {
            Some(Block::AgentText { is_streaming, .. }) => assert!(!is_streaming),
            other => panic!("expected finalized AgentText, got {other:?}"),
        }
        assert!(!app.activity.is_streaming);
        assert_eq!(app.activity.streaming_tokens, 0);
    }

    #[test]
    fn ctrl_c_while_idle_quits_directly() {
        let mut app = App::new();
        app.activity.is_active = false;
        let effects = update(
            &mut app,
            Message::Key(key_with_mods(KeyCode::Char('c'), KeyModifiers::CONTROL)),
        );
        assert!(app.should_quit);
        assert!(effects.iter().any(|e| matches!(e, SideEffect::SaveSession)));
        assert!(effects.iter().any(|e| matches!(e, SideEffect::Quit)));
    }

    #[test]
    fn ctrl_d_on_empty_input_quits() {
        let mut app = App::new();
        let effects = update(
            &mut app,
            Message::Key(key_with_mods(KeyCode::Char('d'), KeyModifiers::CONTROL)),
        );
        assert!(app.should_quit);
        assert!(effects.iter().any(|e| matches!(e, SideEffect::SaveSession)));
        assert!(effects.iter().any(|e| matches!(e, SideEffect::Quit)));
    }

    #[test]
    fn ctrl_d_on_nonempty_input_does_not_quit() {
        let mut app = App::new();
        app.input_text = "hello".into();
        let effects = update(
            &mut app,
            Message::Key(key_with_mods(KeyCode::Char('d'), KeyModifiers::CONTROL)),
        );
        assert!(!app.should_quit);
        assert!(effects.is_empty());
    }

    #[test]
    fn f1_toggles_debug_visibility() {
        let mut app = App::new();
        assert!(!app.debug.visible);

        update(&mut app, Message::Key(key(KeyCode::F(1))));
        assert!(app.debug.visible);

        update(&mut app, Message::Key(key(KeyCode::F(1))));
        assert!(!app.debug.visible);
    }

    #[test]
    fn ctrl_c_during_active_task_shows_confirmation() {
        let mut app = App::new();
        app.activity.is_active = true;
        app.activity.label = "Thinking...".into();

        let effects = update(
            &mut app,
            Message::Key(key_with_mods(KeyCode::Char('c'), KeyModifiers::CONTROL)),
        );

        assert!(!app.should_quit);
        assert!(effects.is_empty());
        match &app.mode {
            UiMode::Approval(ctx) => {
                assert_eq!(ctx.kind, ApprovalKind::Confirmation);
                assert!(ctx.title.contains("Interrupt"));
            }
            other => panic!("expected Approval(Confirmation), got {other:?}"),
        }
    }

    #[test]
    fn shift_enter_inserts_newline() {
        let mut app = App::new();
        app.input_text = "line1".into();
        update(
            &mut app,
            Message::Key(key_with_mods(KeyCode::Enter, KeyModifiers::SHIFT)),
        );
        assert_eq!(app.input_text, "line1\n");
    }

    #[test]
    fn backspace_removes_last_char() {
        let mut app = App::new();
        app.input_text = "hello".into();
        update(&mut app, Message::Key(key(KeyCode::Backspace)));
        assert_eq!(app.input_text, "hell");
    }

    #[test]
    fn spinner_tick_does_not_advance_when_inactive() {
        let mut app = App::new();
        app.activity.is_active = false;
        app.activity.spinner_phase = 3;

        update(&mut app, Message::SpinnerTick);
        assert_eq!(app.activity.spinner_phase, 3);
    }

    #[test]
    fn message_sent_error_adds_warning() {
        let mut app = App::new();
        update(
            &mut app,
            Message::MessageSent(Err("connection refused".into())),
        );
        assert_eq!(app.conversation.len(), 1);
        match &app.conversation.blocks()[0] {
            Block::SystemNote { severity, .. } => {
                assert_eq!(*severity, NoteSeverity::Warning);
            }
            other => panic!("expected SystemNote, got {other:?}"),
        }
    }

    #[test]
    fn message_sent_ok_no_block() {
        let mut app = App::new();
        update(&mut app, Message::MessageSent(Ok(())));
        assert_eq!(app.conversation.len(), 0);
    }

    #[test]
    fn format_token_count_small() {
        assert_eq!(format_token_count(42), "42 tok");
    }

    #[test]
    fn format_token_count_thousands() {
        assert_eq!(format_token_count(4231), "4.2k tok");
    }

    #[test]
    fn format_token_count_millions() {
        assert_eq!(format_token_count(1_500_000), "1.5M tok");
    }

    #[test]
    fn extract_tool_name_colon_format() {
        assert_eq!(extract_tool_name("code.edit: src/main.rs"), "code.edit");
    }

    #[test]
    fn extract_tool_name_no_colon() {
        assert_eq!(extract_tool_name("some random summary"), "action");
    }

    #[test]
    fn truncate_str_short() {
        assert_eq!(truncate_str("short", 10), "short");
    }

    #[test]
    fn truncate_str_long() {
        assert_eq!(truncate_str("abcdefghij", 7), "abcd...");
    }

    #[test]
    fn ws_reconnected_clears_activity() {
        let mut app = App::new();
        app.connection = ConnectionStatus::Disconnected;
        app.activity.label = "Disconnected".into();

        update(&mut app, Message::WsReconnected);

        assert_eq!(app.connection, ConnectionStatus::Connected);
        assert!(app.activity.label.is_empty());
    }

    #[test]
    fn reconnect_attempt_success_restores_connected() {
        let mut app = App::new();
        app.connection = ConnectionStatus::Disconnected;
        app.activity.is_active = true;

        update(&mut app, Message::ReconnectAttempt(Ok(())));

        assert_eq!(app.connection, ConnectionStatus::Connected);
        assert!(!app.activity.is_active);
    }

    #[test]
    fn reconnect_attempt_failure_keeps_reconnecting() {
        let mut app = App::new();
        app.connection = ConnectionStatus::Disconnected;

        update(
            &mut app,
            Message::ReconnectAttempt(Err("connection refused".into())),
        );

        assert!(app.activity.label.contains("Reconnecting"));
        assert!(app.activity.is_active);
    }

    #[test]
    fn plan_mode_transition_updates_status() {
        let mut app = App::new();
        let event = LiveEvent {
            event_type: EventType::PlanModeTransition,
            plan_mode_detail: Some(exoskeleton_core::PlanModeDetail {
                from: "normal".into(),
                to: "planning".into(),
                plan_draft_id: None,
            }),
            ..LiveEvent::new(Some(1))
        };

        let effects = update(&mut app, Message::WsEvent(event));

        assert_eq!(app.status.vessel_mode, "planning");
        assert_eq!(app.conversation.len(), 1);
        assert!(matches!(effects.as_slice(), [SideEffect::FetchPlanStatus]));
    }

    #[test]
    fn page_up_scrolls_conversation() {
        let mut app = App::new();
        app.conversation.set_rendered_dimensions(200, 20);

        update(&mut app, Message::Key(key(KeyCode::PageUp)));

        assert!(!app.conversation.auto_scroll());
    }

    #[test]
    fn end_key_jumps_to_bottom() {
        let mut app = App::new();
        app.conversation.set_rendered_dimensions(200, 20);
        app.conversation.scroll_up(50);

        update(&mut app, Message::Key(key(KeyCode::End)));

        assert!(app.conversation.auto_scroll());
    }

    // ── T26: handle_inner_loop_completed_with_diff_creates_diff_block ──

    #[test]
    fn handle_inner_loop_completed_with_diff_creates_diff_block() {
        let mut app = App::new();
        let event = LiveEvent {
            event_type: EventType::InnerLoopCompleted,
            summary: "Inner loop completed".into(),
            inner_loop_detail: Some(InnerLoopStepDetail {
                step_number: 3,
                max_steps: 25,
                tool_name: None,
                tool_outcome: None,
                tokens_this_step: 0,
                tokens_total: 2000,
                completion_reason: Some("agent_complete".into()),
            }),
            diff_summary: Some(DiffSummary {
                files_modified: 1,
                lines_added: 5,
                lines_removed: 2,
                net_delta: 3,
                files: vec![FileDiffEntry {
                    path: "src/main.rs".into(),
                    lines_added: 5,
                    lines_removed: 2,
                    operation: CodeDiffOperation::Edit,
                }],
            }),
            ..LiveEvent::new(Some(1))
        };

        let _effects = update(&mut app, Message::WsEvent(event));

        let has_diff = app
            .conversation
            .blocks()
            .iter()
            .any(|block| matches!(block, Block::Diff { .. }));
        assert!(has_diff, "should have a Diff block from DiffSummary");

        let diff_block = app
            .conversation
            .blocks()
            .iter()
            .find(|block| matches!(block, Block::Diff { .. }));
        match diff_block {
            Some(Block::Diff { summary, full_text }) => {
                assert_eq!(summary.files_modified, 1);
                assert_eq!(summary.lines_added, 5);
                assert_eq!(summary.lines_removed, 2);
                assert!(full_text.is_none(), "full_text should be None initially");
                assert_eq!(summary.files.len(), 1);
                assert_eq!(summary.files[0].path, "src/main.rs");
            }
            _ => panic!("expected Diff block"),
        }
    }

    // ── T27: fetch_artifact_side_effect_infrastructure ──

    #[test]
    fn fetch_artifact_side_effect_infrastructure() {
        let effect = SideEffect::FetchArtifact("test-artifact-id".into());
        match effect {
            SideEffect::FetchArtifact(id) => assert_eq!(id, "test-artifact-id"),
            _ => panic!("expected FetchArtifact"),
        }

        let msg = Message::ArtifactFetched {
            artifact_id: "test-id".into(),
            result: Ok(serde_json::json!({"content": "test"})),
        };
        match msg {
            Message::ArtifactFetched {
                artifact_id,
                result,
            } => {
                assert_eq!(artifact_id, "test-id");
                assert!(result.is_ok());
            }
            _ => panic!("expected ArtifactFetched"),
        }

        let mut app = App::new();
        let _effects = update(
            &mut app,
            Message::ArtifactFetched {
                artifact_id: "nonexistent".into(),
                result: Err("not found".into()),
            },
        );
    }

    // ── T22: question_asked_event_enters_approval_mode ──

    #[test]
    fn question_asked_event_enters_approval_mode() {
        let mut app = App::new();
        let event = LiveEvent {
            event_type: EventType::QuestionAsked,
            summary: "Agent question".into(),
            question_detail: Some(exoskeleton_core::QuestionDetail {
                question_id: "q-100".into(),
                question: "Which database?".into(),
                choices: Some(vec!["PostgreSQL".into(), "SQLite".into()]),
                status: "pending".into(),
            }),
            ..LiveEvent::new(Some(1))
        };

        update(&mut app, Message::WsEvent(event));

        match &app.mode {
            UiMode::Approval(ctx) => {
                assert_eq!(ctx.kind, ApprovalKind::Question);
                assert_eq!(ctx.question_id, "q-100");
                assert_eq!(ctx.options.len(), 2);
            }
            other => panic!("expected Approval mode, got {other:?}"),
        }
        assert!(app.approval.is_some());
        assert!(!app.approval.as_ref().expect("approval exists").is_text_mode);
    }

    // ── T23: approval_y_produces_submit_effect ──

    #[test]
    fn approval_y_produces_submit_effect() {
        let mut app = App::new();
        use crate::code::widgets::approval::{tool_approval_options, ApprovalContext};

        app.mode = UiMode::Approval(ApprovalContext {
            question_id: "q-200".into(),
            title: "Tool requires approval".into(),
            description: "shell.exec".into(),
            options: tool_approval_options("shell.exec"),
            kind: ApprovalKind::ToolApproval,
            plan_content: None,
            plan_draft_id: None,
        });
        app.approval = Some(ApprovalState::new());

        let effects = update(&mut app, Message::Key(key(KeyCode::Char('y'))));

        assert!(
            effects
                .iter()
                .any(|e| matches!(e, SideEffect::SubmitAnswer { .. })),
            "should produce SubmitAnswer side effect, got: {effects:?}"
        );
        assert_eq!(app.mode, UiMode::Normal);
        assert!(app.approval.is_none());
    }

    // ── T24: approval_returns_to_normal_mode ──

    #[test]
    fn approval_returns_to_normal_mode() {
        let mut app = App::new();
        use crate::code::widgets::approval::{tool_approval_options, ApprovalContext};

        app.mode = UiMode::Approval(ApprovalContext {
            question_id: "q-300".into(),
            title: "Approval".into(),
            description: "test".into(),
            options: tool_approval_options("test"),
            kind: ApprovalKind::ToolApproval,
            plan_content: None,
            plan_draft_id: None,
        });
        app.approval = Some(ApprovalState::new());

        let _effects = update(&mut app, Message::Key(key(KeyCode::Esc)));

        assert_eq!(app.mode, UiMode::Normal);
        assert!(app.approval.is_none());
    }

    // ── T25: tick_completed_with_plan_creates_plan_summary ──

    #[test]
    fn tick_completed_with_plan_creates_plan_summary() {
        use std::collections::HashMap;

        use exoskeleton_core::{
            id::{PlanTaskId, VesselId},
            plan::{Plan, PlanTask, PlanTaskStatus},
            StateSnapshot,
        };

        let mut app = App::new();

        let plan = Plan {
            objective: "Test objective".into(),
            tasks: vec![
                PlanTask {
                    id: PlanTaskId::new(),
                    description: "Task A".into(),
                    status: PlanTaskStatus::Completed,
                    depends_on: Vec::new(),
                    tool_hint: None,
                    metadata: HashMap::new(),
                },
                PlanTask {
                    id: PlanTaskId::new(),
                    description: "Task B".into(),
                    status: PlanTaskStatus::Pending,
                    depends_on: Vec::new(),
                    tool_hint: None,
                    metadata: HashMap::new(),
                },
            ],
            updated_at: chrono::Utc::now(),
        };

        let mut snapshot = StateSnapshot::initial(VesselId::new(), "mission".into());
        snapshot.plan = Some(plan);

        let event = LiveEvent {
            event_type: EventType::TickCompleted,
            summary: "Tick 5 completed".into(),
            snapshot: Some(snapshot),
            ..LiveEvent::new(Some(5))
        };

        update(&mut app, Message::WsEvent(event));

        let has_plan = app
            .conversation
            .blocks()
            .iter()
            .any(|b| matches!(b, Block::PlanSummary { .. }));
        assert!(has_plan, "should have a PlanSummary block");
    }

    #[test]
    fn debug_state_populated_from_snapshot() {
        use std::collections::HashMap;

        use exoskeleton_core::{
            id::{PlanTaskId, VesselId},
            plan::{Plan, PlanTask, PlanTaskStatus},
            thread::ThreadStatus,
            StateSnapshot, ThreadSummary,
        };

        let mut app = App::new();

        let mut snapshot = StateSnapshot::initial(VesselId::new(), "test".into());
        snapshot.tick_number = 42;
        snapshot.thread_summaries = vec![ThreadSummary {
            thread_id: exoskeleton_core::id::ThreadId::new(),
            name: "meta-cognition".into(),
            status: ThreadStatus::Active,
            last_output_summary: Some("consider edge cases".into()),
            token_budget_remaining: 5000,
        }];
        snapshot.plan = Some(Plan {
            objective: "Implement feature".into(),
            tasks: vec![
                PlanTask {
                    id: PlanTaskId::new(),
                    description: "Task A".into(),
                    status: PlanTaskStatus::Completed,
                    depends_on: Vec::new(),
                    tool_hint: None,
                    metadata: HashMap::new(),
                },
                PlanTask {
                    id: PlanTaskId::new(),
                    description: "Task B".into(),
                    status: PlanTaskStatus::Pending,
                    depends_on: Vec::new(),
                    tool_hint: None,
                    metadata: HashMap::new(),
                },
            ],
            updated_at: chrono::Utc::now(),
        });

        let event = LiveEvent {
            event_type: EventType::TickCompleted,
            summary: "Tick 42 completed".into(),
            snapshot: Some(snapshot),
            ..LiveEvent::new(Some(42))
        };

        update(&mut app, Message::WsEvent(event));

        assert_eq!(app.debug.tick_number, 42);
        assert_eq!(app.debug.threads.len(), 1);
        assert_eq!(app.debug.threads[0].name, "meta-cognition");
        assert!(app.debug.threads[0].contributed);
        assert!(app.debug.threads[0].state.contains("contributed"));

        let plan = app.debug.plan_summary.as_ref().expect("plan should exist");
        assert_eq!(plan.completed, 1);
        assert_eq!(plan.total, 2);
        assert!(plan.objective.contains("Implement feature"));
    }

    #[test]
    fn inner_loop_step_populates_debug_state() {
        let mut app = App::new();
        app.debug.initial_token_budget = Some(50_000);

        let event = LiveEvent {
            event_type: EventType::InnerLoopStep,
            summary: "Step 3/25".into(),
            inner_loop_detail: Some(InnerLoopStepDetail {
                step_number: 3,
                max_steps: 25,
                tool_name: Some("code.read".into()),
                tool_outcome: Some("success".into()),
                tokens_this_step: 200,
                tokens_total: 4231,
                completion_reason: None,
            }),
            ..LiveEvent::new(Some(1))
        };

        update(&mut app, Message::WsEvent(event));

        assert_eq!(app.debug.step_count, "3/25 steps");
        assert!(app.debug.token_count.contains("4231"));
        assert_eq!(app.debug.budget.step_percent_used, 12);
    }

    #[test]
    fn inner_loop_started_sets_debug_active() {
        let mut app = App::new();
        let event = LiveEvent {
            event_type: EventType::InnerLoopStarted,
            summary: "Inner loop started".into(),
            ..LiveEvent::new(Some(1))
        };

        update(&mut app, Message::WsEvent(event));

        assert!(app.debug.inner_loop_active);
    }

    #[test]
    fn inner_loop_completed_clears_debug_active() {
        let mut app = App::new();
        app.debug.inner_loop_active = true;

        let event = LiveEvent {
            event_type: EventType::InnerLoopCompleted,
            summary: "Inner loop completed".into(),
            inner_loop_detail: Some(InnerLoopStepDetail {
                step_number: 5,
                max_steps: 25,
                tool_name: None,
                tool_outcome: None,
                tokens_this_step: 0,
                tokens_total: 5000,
                completion_reason: Some("agent_complete".into()),
            }),
            ..LiveEvent::new(Some(1))
        };

        update(&mut app, Message::WsEvent(event));

        assert!(!app.debug.inner_loop_active);
    }

    #[test]
    fn confirmation_yes_quits_and_cancels() {
        use crate::code::widgets::approval::{
            confirmation_options, ApprovalContext, ApprovalState,
        };

        let mut app = App::new();
        app.mode = UiMode::Approval(ApprovalContext {
            question_id: String::new(),
            title: "Interrupt agent?".into(),
            description: "test".into(),
            options: confirmation_options(),
            kind: ApprovalKind::Confirmation,
            plan_content: None,
            plan_draft_id: None,
        });
        app.approval = Some(ApprovalState::new());

        let effects = update(&mut app, Message::Key(key(KeyCode::Char('y'))));

        assert!(app.should_quit);
        assert!(effects
            .iter()
            .any(|e| matches!(e, SideEffect::SendCancellation)));
        assert!(effects.iter().any(|e| matches!(e, SideEffect::SaveSession)));
    }

    #[test]
    fn confirmation_no_returns_to_normal() {
        use crate::code::widgets::approval::{
            confirmation_options, ApprovalContext, ApprovalState,
        };

        let mut app = App::new();
        app.mode = UiMode::Approval(ApprovalContext {
            question_id: String::new(),
            title: "Interrupt agent?".into(),
            description: "test".into(),
            options: confirmation_options(),
            kind: ApprovalKind::Confirmation,
            plan_content: None,
            plan_draft_id: None,
        });
        let mut state = ApprovalState::new();
        state.selected_index = 1;
        app.approval = Some(state);

        let effects = update(&mut app, Message::Key(key(KeyCode::Enter)));

        assert!(!app.should_quit);
        assert_eq!(app.mode, UiMode::Normal);
        assert!(effects.is_empty() || !effects.iter().any(|e| matches!(e, SideEffect::Quit)));
    }

    #[test]
    fn tick_started_updates_debug_tick() {
        let mut app = App::new();
        let event = LiveEvent {
            event_type: EventType::TickStarted,
            summary: "Tick 10 started".into(),
            ..LiveEvent::new(Some(10))
        };

        update(&mut app, Message::WsEvent(event));

        assert_eq!(app.debug.tick_number, 10);
    }

    // ── T26: plan_mode_transition_with_draft_triggers_fetch ──

    #[test]
    fn plan_mode_transition_with_draft_triggers_fetch() {
        let mut app = App::new();
        let event = LiveEvent {
            event_type: EventType::PlanModeTransition,
            plan_mode_detail: Some(exoskeleton_core::PlanModeDetail {
                from: "normal".into(),
                to: "planning".into(),
                plan_draft_id: Some("draft-abc-123".into()),
            }),
            ..LiveEvent::new(Some(1))
        };

        let effects = update(&mut app, Message::WsEvent(event));

        assert!(
            effects
                .iter()
                .any(|e| matches!(e, SideEffect::FetchPlanDraft(id) if id == "draft-abc-123")),
            "should produce FetchPlanDraft side effect, got: {effects:?}"
        );
    }

    #[test]
    fn policy_auto_deny_creates_tool_call_with_policy_denied() {
        let mut app = App::new();
        let event = LiveEvent {
            event_type: EventType::PolicyApprovalRequired,
            summary: "Policy denied shell.exec".into(),
            policy_detail: Some(PolicyDetail {
                tool_name: "shell.exec".into(),
                rule: "deny: *".into(),
            }),
            question_detail: None,
            ..LiveEvent::new(Some(1))
        };
        update(&mut app, Message::WsEvent(event));
        assert_eq!(app.conversation.len(), 1);
        match &app.conversation.blocks()[0] {
            Block::ToolCall { tool_name, outcome, .. } => {
                assert_eq!(tool_name, "shell.exec");
                assert_eq!(*outcome, ToolOutcome::PolicyDenied);
            }
            other => panic!("expected ToolCall with PolicyDenied, got {other:?}"),
        }
    }

    #[test]
    fn tick_completed_sets_governance_activity_when_active() {
        use exoskeleton_core::id::VesselId;

        let mut app = App::new();
        let mut snapshot =
            exoskeleton_core::StateSnapshot::initial(VesselId::new(), "test".into());
        snapshot.tick_number = 5;

        let event = LiveEvent {
            event_type: EventType::TickCompleted,
            summary: "Tick 5 completed".into(),
            snapshot: Some(snapshot),
            ..LiveEvent::new(Some(5))
        };

        update(&mut app, Message::WsEvent(event));

        assert!(app.activity.is_active);
        assert!(
            app.activity.label.contains("Governance"),
            "should show governance label, got: {}",
            app.activity.label
        );
    }

    #[test]
    fn tick_completed_no_activity_when_suspended() {
        use exoskeleton_core::id::VesselId;
        use exoskeleton_core::snapshot::VesselStatus;

        let mut app = App::new();
        let mut snapshot =
            exoskeleton_core::StateSnapshot::initial(VesselId::new(), "test".into());
        snapshot.tick_number = 5;
        snapshot.status = VesselStatus::Suspended;

        let event = LiveEvent {
            event_type: EventType::TickCompleted,
            summary: "Tick 5 completed".into(),
            snapshot: Some(snapshot),
            ..LiveEvent::new(Some(5))
        };

        update(&mut app, Message::WsEvent(event));

        assert!(!app.activity.is_active);
    }
}

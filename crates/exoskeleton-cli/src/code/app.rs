//! Application state, Message enum, pure update() function, and SideEffect enum.
//!
//! Follows the Elm architecture: `update()` is a pure function that takes the
//! current `App` state and a `Message`, returning side effects to execute.
//! The event loop calls `update()` and then executes the side effects.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use exoskeleton_core::{EventType, LiveEvent};

use crate::code::render::diff::{DiffFileSummary, DiffSummaryData};

use super::widgets::conversation::{Block, ConversationState, NoteSeverity, ToolOutcome};

/// The top-level UI mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UiMode {
    /// Standard conversation view — input area active.
    Normal,
    // Approval(ApprovalContext) — added in S3
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
    /// Terminal dimensions.
    pub terminal_width: u16,
    pub terminal_height: u16,
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
            terminal_width: 80,
            terminal_height: 24,
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
    /// Spinner animation tick (100ms interval).
    SpinnerTick,
    /// Result of submitting a message via the inbox.
    MessageSent(Result<(), String>),
    /// Result of fetching an artifact.
    ArtifactFetched {
        artifact_id: String,
        result: Result<serde_json::Value, String>,
    },
}

/// Side effects produced by update() for the event loop to execute.
#[derive(Debug)]
pub enum SideEffect {
    /// Send a message to the vessel via POST /api/v1/inbox.
    SendMessage(String),
    /// Attempt to reconnect the WebSocket.
    #[allow(dead_code)]
    Reconnect,
    /// Fetch an artifact by ID via GET /api/v1/artifacts/{id}.
    #[allow(dead_code)]
    FetchArtifact(String),
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
            app.activity.label = "Disconnected".into();
            app.activity.is_active = false;
        }
        Message::WsReconnected => {
            app.connection = ConnectionStatus::Connected;
            app.activity.label = String::new();
            app.activity.is_active = false;
        }
        Message::SpinnerTick => {
            if app.activity.is_active {
                app.activity.spinner_phase =
                    (app.activity.spinner_phase + 1) % SPINNER_FRAMES.len();
            }
        }
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
    }

    effects
}

/// Handle keyboard input.
fn handle_key(app: &mut App, key: KeyEvent, effects: &mut Vec<SideEffect>) {
    match app.mode {
        UiMode::Normal => handle_key_normal(app, key, effects),
    }
}

/// Handle keyboard input in Normal mode.
fn handle_key_normal(app: &mut App, key: KeyEvent, effects: &mut Vec<SideEffect>) {
    match (key.code, key.modifiers) {
        (KeyCode::Char('d'), KeyModifiers::CONTROL) => {
            if app.input_text.is_empty() {
                app.should_quit = true;
                effects.push(SideEffect::Quit);
            }
        }
        (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
            app.should_quit = true;
            effects.push(SideEffect::Quit);
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

/// Handle a WebSocket LiveEvent.
fn handle_ws_event(app: &mut App, event: LiveEvent) -> Vec<SideEffect> {
    let effects = Vec::new();

    match event.event_type {
        EventType::InnerLoopStarted => {
            app.activity.label = "Thinking...".into();
            app.activity.is_active = true;
            app.activity.spinner_phase = 0;
        }
        EventType::InnerLoopStep => {
            if let Some(ref detail) = event.inner_loop_detail {
                let tool_name = detail.tool_name.clone().unwrap_or_default();
                let outcome =
                    tool_outcome_from_str(detail.tool_outcome.as_deref().unwrap_or_default());

                app.conversation.add_block(Block::ToolCall {
                    tool_name: tool_name.clone(),
                    args_summary: format!("step {}/{}", detail.step_number, detail.max_steps),
                    outcome,
                    collapsed: true,
                });

                app.status.step_summary =
                    format!("step {}/{}", detail.step_number, detail.max_steps);
                app.status.token_summary = format_token_count(detail.tokens_total);
                app.activity.label = format!("Running {tool_name}...");
                app.activity.is_active = true;
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

            app.activity.label = String::new();
            app.activity.is_active = false;
        }
        EventType::ActionExecuted => {
            let summary = &event.summary;
            app.conversation.add_block(Block::ToolCall {
                tool_name: extract_tool_name(summary),
                args_summary: truncate_str(summary, 80),
                outcome: ToolOutcome::Success,
                collapsed: true,
            });
        }
        EventType::TickStarted => {}
        EventType::TickCompleted => {
            app.conversation.add_block(Block::SystemNote {
                text: event.summary.clone(),
                severity: NoteSeverity::Info,
            });

            if let Some(ref snapshot) = event.snapshot {
                app.status.vessel_mode = format!("{:?}", snapshot.vessel_mode).to_lowercase();
                app.status.token_summary =
                    format_token_count(snapshot.budget_status.total_tokens_remaining());
            }
        }
        EventType::QuestionAsked => {
            if let Some(ref detail) = event.question_detail {
                let mut text = format!("Question: {}", detail.question);
                if let Some(ref choices) = detail.choices {
                    for (i, choice) in choices.iter().enumerate() {
                        text.push_str(&format!("\n  {}. {}", i + 1, choice));
                    }
                }
                text.push_str("\n  (Interactive input available in S3)");
                app.conversation.add_block(Block::SystemNote {
                    text,
                    severity: NoteSeverity::Warning,
                });
            }
        }
        EventType::PolicyApprovalRequired => {
            if let Some(ref detail) = event.policy_detail {
                app.conversation.add_block(Block::SystemNote {
                    text: format!(
                        "Policy: {} requires approval ({})",
                        detail.tool_name, detail.rule
                    ),
                    severity: NoteSeverity::Warning,
                });
            }
        }
        EventType::PlanModeTransition => {
            if let Some(ref detail) = event.plan_mode_detail {
                app.conversation.add_block(Block::SystemNote {
                    text: format!("Mode: {} -> {}", detail.from, detail.to),
                    severity: NoteSeverity::Info,
                });
                app.status.vessel_mode = detail.to.clone();
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

fn tool_outcome_from_str(outcome: &str) -> ToolOutcome {
    match outcome {
        "success" => ToolOutcome::Success,
        "pending" => ToolOutcome::Pending,
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
    };

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
                tool_name, outcome, ..
            } => {
                assert_eq!(tool_name, "code.read");
                assert_eq!(*outcome, ToolOutcome::Success);
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

        assert!(effects.is_empty());
        assert_eq!(app.connection, ConnectionStatus::Disconnected);
        assert_eq!(app.activity.label, "Disconnected");
        assert!(!app.activity.is_active);
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
    fn ctrl_c_quits() {
        let mut app = App::new();
        let effects = update(
            &mut app,
            Message::Key(key_with_mods(KeyCode::Char('c'), KeyModifiers::CONTROL)),
        );
        assert!(app.should_quit);
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

        update(&mut app, Message::WsEvent(event));

        assert_eq!(app.status.vessel_mode, "planning");
        assert_eq!(app.conversation.len(), 1);
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
}

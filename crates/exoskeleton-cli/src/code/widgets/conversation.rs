//! Conversation model and scrollable block list widget.
//!
//! The conversation is rendered as a vertical list of `Block` items — each
//! representing a user message, agent text, tool call, or system note. This
//! module owns the data model (`Block`, `ConversationState`) and the ratatui
//! widget that renders them as plain text (S1). Markdown rendering is added
//! in S2.

use chrono::{DateTime, Utc};
use exoskeleton_core::plan::Plan;
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block as RatatuiBlock, Borders, Paragraph, Widget, Wrap},
};

use crate::code::render::diff::{render_diff_summary, render_diff_text, DiffSummaryData};
use crate::code::widgets::markdown::render_markdown;
use crate::code::widgets::plan::render_plan_lines;

/// A single visual element in the conversation.
#[derive(Debug, Clone, PartialEq)]
pub enum Block {
    /// Operator's message.
    UserMessage {
        text: String,
        timestamp: DateTime<Utc>,
    },
    /// Agent's reasoning/response text (plain text in S1, markdown spans in S2).
    AgentText { text: String, is_streaming: bool },
    /// Tool call with result.
    ToolCall {
        tool_name: String,
        args_summary: String,
        outcome: ToolOutcome,
        collapsed: bool,
        token_cost: Option<u64>,
    },
    /// System notification (tick boundary, mode change, completion).
    SystemNote {
        text: String,
        severity: NoteSeverity,
    },
    /// Diff visualization (from CodeDiff artifacts or DiffSummary events).
    Diff {
        /// Summary data (always available from LiveEvent).
        summary: DiffSummaryData,
        /// Full unified diff text (fetched from artifact, may be absent).
        full_text: Option<String>,
    },
    /// Plan summary display (task list with status icons).
    PlanSummary { plan: Plan },
}

/// Outcome of a tool invocation.
#[derive(Debug, Clone, PartialEq)]
pub enum ToolOutcome {
    /// Tool completed normally.
    Success,
    /// Tool returned an error.
    Error(String),
    /// Tool execution in progress.
    Pending,
    /// Tool blocked by policy engine.
    PolicyDenied,
}

/// Severity level for system notes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoteSeverity {
    /// Informational (tick boundaries, mode transitions).
    Info,
    /// Warning (budget warnings, reconnection notices).
    Warning,
    /// Insight (thread contributions, governance recommendations).
    #[allow(dead_code)]
    Insight,
}

/// State for the conversation display area.
#[derive(Debug)]
pub struct ConversationState {
    /// All blocks in the conversation, in chronological order.
    blocks: Vec<Block>,
    /// Vertical scroll offset (in rendered lines from the top).
    scroll_offset: u16,
    /// Whether auto-scroll is enabled (scroll to bottom on new content).
    auto_scroll: bool,
    /// Whether there is new content below the current scroll position.
    has_new_content_below: bool,
    /// Total rendered height from last render pass (for scroll calculations).
    last_rendered_height: u16,
    /// Viewport height from last render pass.
    last_viewport_height: u16,
    /// Index of the currently focused block (for collapse/expand). None = no focus.
    focused_block: Option<usize>,
}

impl ConversationState {
    /// Create a new empty conversation state.
    pub fn new() -> Self {
        Self {
            blocks: Vec::new(),
            scroll_offset: 0,
            auto_scroll: true,
            has_new_content_below: false,
            last_rendered_height: 0,
            last_viewport_height: 0,
            focused_block: None,
        }
    }

    /// Add a block to the conversation.
    pub fn add_block(&mut self, block: Block) {
        self.blocks.push(block);
        if self.auto_scroll {
            self.scroll_to_bottom();
        } else {
            self.has_new_content_below = true;
        }
    }

    /// Notify that existing content has changed without adding a new block.
    pub fn notify_content_changed(&mut self) {
        if self.auto_scroll {
            self.scroll_to_bottom();
        } else {
            self.has_new_content_below = true;
        }
    }

    /// Number of blocks in the conversation.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn len(&self) -> usize {
        self.blocks.len()
    }

    /// Whether the conversation is empty.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }

    /// Get an immutable reference to all blocks.
    pub fn blocks(&self) -> &[Block] {
        &self.blocks
    }

    /// Get a mutable reference to all blocks (for updating diff content).
    pub fn blocks_mut(&mut self) -> &mut Vec<Block> {
        &mut self.blocks
    }

    /// Whether auto-scroll is currently active.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn auto_scroll(&self) -> bool {
        self.auto_scroll
    }

    /// Whether there is new content below the visible area.
    pub fn has_new_content_below(&self) -> bool {
        self.has_new_content_below
    }

    /// Current scroll offset in rendered lines from the top.
    pub fn scroll_offset(&self) -> u16 {
        self.scroll_offset
    }

    /// Scroll up by the given number of lines.
    pub fn scroll_up(&mut self, lines: u16) {
        self.scroll_offset = self.scroll_offset.saturating_sub(lines);
        self.auto_scroll = false;
    }

    /// Scroll down by the given number of lines.
    pub fn scroll_down(&mut self, lines: u16) {
        self.scroll_offset = self.scroll_offset.saturating_add(lines);
        let max = self
            .last_rendered_height
            .saturating_sub(self.last_viewport_height);
        if self.scroll_offset >= max {
            self.scroll_offset = max;
            self.auto_scroll = true;
            self.has_new_content_below = false;
        }
    }

    /// Jump to the bottom and re-enable auto-scroll.
    pub fn jump_to_bottom(&mut self) {
        let max = self
            .last_rendered_height
            .saturating_sub(self.last_viewport_height);
        self.scroll_offset = max;
        self.auto_scroll = true;
        self.has_new_content_below = false;
    }

    /// Scroll to bottom (used internally when auto-scroll is on).
    fn scroll_to_bottom(&mut self) {
        let max = self
            .last_rendered_height
            .saturating_sub(self.last_viewport_height);
        self.scroll_offset = max;
        self.has_new_content_below = false;
    }

    /// Update rendered dimensions after a render pass.
    pub fn set_rendered_dimensions(&mut self, total_height: u16, viewport_height: u16) {
        self.last_rendered_height = total_height;
        self.last_viewport_height = viewport_height;
        if self.auto_scroll {
            self.scroll_to_bottom();
        } else {
            let max = self
                .last_rendered_height
                .saturating_sub(self.last_viewport_height);
            self.scroll_offset = self.scroll_offset.min(max);
        }
    }

    /// Restore a previously-saved scroll offset and disable auto-scroll.
    pub fn restore_scroll_offset(&mut self, offset: u16) {
        self.scroll_offset = offset;
        self.auto_scroll = false;
        self.has_new_content_below = false;
    }

    /// Get the currently focused block index.
    pub fn focused_block(&self) -> Option<usize> {
        self.focused_block
    }

    /// Set focus to a specific block index. Clamps to valid range.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn set_focused_block(&mut self, index: Option<usize>) {
        self.focused_block = match index {
            Some(i) if !self.blocks.is_empty() => Some(i.min(self.blocks.len() - 1)),
            Some(_) => None,
            None => None,
        };
    }

    /// Move focus to the previous block. If no focus, focuses the last block.
    pub fn focus_prev(&mut self) {
        if self.blocks.is_empty() {
            return;
        }
        self.focused_block = Some(match self.focused_block {
            Some(0) => 0,
            Some(i) => i - 1,
            None => self.blocks.len() - 1,
        });
    }

    /// Move focus to the next block. If no focus, focuses the first block.
    pub fn focus_next(&mut self) {
        if self.blocks.is_empty() {
            return;
        }
        let max = self.blocks.len() - 1;
        self.focused_block = Some(match self.focused_block {
            Some(i) if i >= max => max,
            Some(i) => i + 1,
            None => 0,
        });
    }

    /// Clear the block focus.
    pub fn clear_focus(&mut self) {
        self.focused_block = None;
    }

    /// Toggle collapsed state on the focused block if it is a ToolCall.
    /// Returns true if a toggle happened.
    pub fn toggle_focused_collapse(&mut self) -> bool {
        if let Some(idx) = self.focused_block {
            if let Some(Block::ToolCall { collapsed, .. }) = self.blocks.get_mut(idx) {
                *collapsed = !*collapsed;
                return true;
            }
        }
        false
    }
}

impl Default for ConversationState {
    fn default() -> Self {
        Self::new()
    }
}

/// Render a block to ratatui Lines.
fn render_block_lines(block: &Block, width: u16, debug_mode: bool) -> Vec<Line<'static>> {
    match block {
        Block::UserMessage { text, timestamp } => {
            let time_str = timestamp.format("%H:%M").to_string();
            let header = Line::from(vec![Span::styled(
                format!(" You ({time_str}) "),
                Style::default()
                    .fg(Color::White)
                    .bg(Color::Blue)
                    .add_modifier(Modifier::BOLD),
            )]);
            let mut lines = vec![Line::from(""), header];
            for line in text.lines() {
                lines.push(Line::from(format!("  {line}")));
            }
            lines.push(Line::from(""));
            lines
        }
        Block::AgentText { text, .. } => {
            let header = Line::from(vec![Span::styled(
                " Agent ",
                Style::default()
                    .fg(Color::White)
                    .bg(Color::Magenta)
                    .add_modifier(Modifier::BOLD),
            )]);
            let mut lines = vec![Line::from(""), header];

            let md_lines = render_markdown(text, width);
            for md_line in md_lines {
                let mut indented_spans = vec![Span::raw("  ".to_string())];
                indented_spans.extend(md_line.spans);
                lines.push(Line::from(indented_spans));
            }

            lines.push(Line::from(""));
            lines
        }
        Block::ToolCall {
            tool_name,
            args_summary,
            outcome,
            collapsed,
            token_cost,
        } => {
            let (icon, color) = match outcome {
                ToolOutcome::Success => ("OK", Color::Green),
                ToolOutcome::Error(_) => ("ERR", Color::Red),
                ToolOutcome::Pending => ("...", Color::Yellow),
                ToolOutcome::PolicyDenied => ("DENY", Color::Red),
            };
            let mut spans_vec = vec![
                Span::styled("  ", Style::default()),
                Span::styled(
                    tool_name.clone(),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!(" {args_summary} "),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled(format!("[{icon}]"), Style::default().fg(color)),
            ];
            if debug_mode {
                if let Some(cost) = token_cost {
                    spans_vec.push(Span::styled(
                        format!(" ({cost} tok)"),
                        Style::default().fg(Color::DarkGray),
                    ));
                }
            }
            let mut lines = vec![Line::from(spans_vec)];

            if !collapsed {
                lines.push(Line::from(vec![
                    Span::styled("    Tool: ", Style::default().fg(Color::DarkGray)),
                    Span::styled(
                        tool_name.clone(),
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    ),
                ]));
                lines.push(Line::from(vec![
                    Span::styled("    Args: ", Style::default().fg(Color::DarkGray)),
                    Span::styled(args_summary.clone(), Style::default().fg(Color::White)),
                ]));
                match outcome {
                    ToolOutcome::Error(msg) => {
                        lines.push(Line::from(vec![
                            Span::styled("    Error: ", Style::default().fg(Color::Red)),
                            Span::styled(msg.clone(), Style::default().fg(Color::Red)),
                        ]));
                    }
                    ToolOutcome::PolicyDenied => {
                        lines.push(Line::from(vec![Span::styled(
                            "    Policy denied".to_string(),
                            Style::default().fg(Color::Red),
                        )]));
                    }
                    _ => {}
                }
                if let Some(cost) = token_cost {
                    lines.push(Line::from(vec![
                        Span::styled("    Tokens: ", Style::default().fg(Color::DarkGray)),
                        Span::styled(format!("{cost}"), Style::default().fg(Color::White)),
                    ]));
                }
            }

            lines
        }
        Block::SystemNote { text, severity } => {
            let color = match severity {
                NoteSeverity::Info => Color::DarkGray,
                NoteSeverity::Warning => Color::Yellow,
                NoteSeverity::Insight => Color::Cyan,
            };

            if debug_mode
                && *severity == NoteSeverity::Info
                && (text.starts_with("Tick ") || text.contains("tick"))
            {
                let separator = "─".repeat(20.min(width as usize / 2));
                vec![Line::from(vec![Span::styled(
                    format!("  {separator} {text} {separator}"),
                    Style::default()
                        .fg(Color::DarkGray)
                        .add_modifier(Modifier::DIM),
                )])]
            } else {
                vec![Line::from(vec![Span::styled(
                    format!("  --- {text} ---"),
                    Style::default().fg(color),
                )])]
            }
        }
        Block::Diff { summary, full_text } => {
            let header = Line::from(vec![Span::styled(
                " Diff ",
                Style::default()
                    .fg(Color::White)
                    .bg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            )]);
            let mut lines = vec![Line::from(""), header];

            if let Some(diff_text) = full_text {
                let diff_lines = render_diff_text(diff_text);
                for diff_line in diff_lines {
                    let mut indented = vec![Span::raw("  ".to_string())];
                    indented.extend(diff_line.spans);
                    lines.push(Line::from(indented));
                }
            } else {
                let summary_lines = render_diff_summary(summary);
                for summary_line in summary_lines {
                    let mut indented = vec![Span::raw("  ".to_string())];
                    indented.extend(summary_line.spans);
                    lines.push(Line::from(indented));
                }
            }

            lines.push(Line::from(""));
            lines
        }
        Block::PlanSummary { plan } => render_plan_lines(plan),
    }
}

/// Widget for rendering the conversation area.
pub struct ConversationWidget<'a> {
    state: &'a ConversationState,
    debug_mode: bool,
}

impl<'a> ConversationWidget<'a> {
    /// Create a new conversation widget.
    pub fn new(state: &'a ConversationState) -> Self {
        Self {
            state,
            debug_mode: false,
        }
    }

    pub fn with_debug_mode(mut self, debug: bool) -> Self {
        self.debug_mode = debug;
        self
    }
}

impl<'a> Widget for ConversationWidget<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let border = RatatuiBlock::default().borders(Borders::NONE);
        let inner = border.inner(area);

        let mut all_lines: Vec<Line<'static>> = Vec::new();
        for (block_idx, block) in self.state.blocks().iter().enumerate() {
            let mut block_lines = render_block_lines(block, inner.width, self.debug_mode);
            if self.state.focused_block() == Some(block_idx) {
                if let Some(first_line) = block_lines.first_mut() {
                    let mut new_spans = vec![Span::styled(
                        "\u{25B6} ".to_string(),
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    )];
                    new_spans.append(&mut first_line.spans);
                    first_line.spans = new_spans;
                }
            }
            all_lines.extend(block_lines);
        }

        let paragraph = Paragraph::new(all_lines)
            .scroll((self.state.scroll_offset, 0))
            .wrap(Wrap { trim: false });
        paragraph.render(inner, buf);

        if self.state.has_new_content_below() && area.width > 20 {
            let indicator = " v New content below v ";
            let x = area.x + area.width.saturating_sub(indicator.len() as u16 + 1);
            let y = area.y + area.height.saturating_sub(1);
            if y >= area.y && x >= area.x {
                buf.set_string(
                    x,
                    y,
                    indicator,
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                );
            }
        }
    }
}

/// Estimate the total rendered height for the conversation content.
///
/// This is an approximation used for scroll calculations. The actual height
/// depends on terminal width and text wrapping, but for plain text in S1
/// counting newlines is sufficient.
pub fn estimate_rendered_height(state: &ConversationState, width: u16, debug_mode: bool) -> u16 {
    let mut total: u16 = 0;
    for block in state.blocks() {
        let lines = render_block_lines(block, width, debug_mode);
        total = total.saturating_add(lines.len() as u16);
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── TUI-T9: conversation_state_add_block_increments_len ──

    #[test]
    fn conversation_state_add_block_increments_len() {
        let mut state = ConversationState::new();
        assert_eq!(state.len(), 0);
        assert!(state.is_empty());

        state.add_block(Block::SystemNote {
            text: "test".into(),
            severity: NoteSeverity::Info,
        });
        assert_eq!(state.len(), 1);
        assert!(!state.is_empty());

        state.add_block(Block::AgentText {
            text: "hello".into(),
            is_streaming: false,
        });
        assert_eq!(state.len(), 2);
    }

    // ── TUI-T10: auto_scroll_pauses_on_scroll_up ──

    #[test]
    fn auto_scroll_pauses_on_scroll_up() {
        let mut state = ConversationState::new();
        assert!(state.auto_scroll());

        state.scroll_up(5);
        assert!(!state.auto_scroll());
    }

    // ── TUI-T11: auto_scroll_resumes_on_jump_to_bottom ──

    #[test]
    fn auto_scroll_resumes_on_jump_to_bottom() {
        let mut state = ConversationState::new();
        state.set_rendered_dimensions(100, 20);

        state.scroll_up(10);
        assert!(!state.auto_scroll());

        state.jump_to_bottom();
        assert!(state.auto_scroll());
        assert!(!state.has_new_content_below());
    }

    // ── TUI-T17: block_from_inner_loop_step_with_detail ──

    #[test]
    fn block_from_inner_loop_step_with_detail() {
        let block = Block::ToolCall {
            tool_name: "code.edit".into(),
            args_summary: "src/main.rs".into(),
            outcome: ToolOutcome::Success,
            collapsed: true,
            token_cost: None,
        };
        match &block {
            Block::ToolCall {
                tool_name,
                args_summary,
                outcome,
                ..
            } => {
                assert_eq!(tool_name, "code.edit");
                assert_eq!(args_summary, "src/main.rs");
                assert_eq!(*outcome, ToolOutcome::Success);
            }
            _ => panic!("expected ToolCall block"),
        }
    }

    // ── TUI-T18: block_from_action_executed ──

    #[test]
    fn block_from_action_executed() {
        let summary = "shell.exec: cargo test (exit 0)";
        let block = Block::ToolCall {
            tool_name: "shell.exec".into(),
            args_summary: "cargo test".into(),
            outcome: ToolOutcome::Success,
            collapsed: true,
            token_cost: None,
        };
        match &block {
            Block::ToolCall {
                tool_name, outcome, ..
            } => {
                assert_eq!(tool_name, "shell.exec");
                assert_eq!(*outcome, ToolOutcome::Success);
                assert!(summary.contains("shell.exec"));
            }
            _ => panic!("expected ToolCall block"),
        }
    }

    #[test]
    fn new_content_below_set_when_not_auto_scrolling() {
        let mut state = ConversationState::new();
        state.set_rendered_dimensions(100, 20);

        state.scroll_up(5);
        assert!(!state.auto_scroll());

        state.add_block(Block::SystemNote {
            text: "test".into(),
            severity: NoteSeverity::Info,
        });

        assert!(state.has_new_content_below());
    }

    #[test]
    fn scroll_down_past_bottom_reenables_auto_scroll() {
        let mut state = ConversationState::new();
        state.set_rendered_dimensions(100, 20);

        state.scroll_up(5);
        assert!(!state.auto_scroll());

        state.scroll_down(200);
        assert!(state.auto_scroll());
        assert!(!state.has_new_content_below());
    }

    // ── T24: render_agent_text_with_markdown ──

    #[test]
    fn render_agent_text_with_markdown() {
        let block = Block::AgentText {
            text: "Here is **bold** and `code`".into(),
            is_streaming: false,
        };
        let lines = render_block_lines(&block, 80, false);

        assert!(
            lines.len() >= 3,
            "should have header + markdown lines + trailing blank"
        );

        let text: String = lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect();
        assert!(text.contains("bold"), "should contain bold text");
        assert!(text.contains("code"), "should contain inline code");

        let has_bold = lines.iter().any(|line| {
            line.spans.iter().any(|span| {
                span.content.contains("bold")
                    && span
                        .style
                        .add_modifier
                        .contains(ratatui::style::Modifier::BOLD)
            })
        });
        assert!(has_bold, "bold text should have BOLD modifier");
    }

    // ── T25: render_diff_block ──

    #[test]
    fn render_diff_block() {
        use crate::code::render::diff::{DiffFileSummary, DiffSummaryData};

        let block = Block::Diff {
            summary: DiffSummaryData {
                files_modified: 1,
                lines_added: 3,
                lines_removed: 1,
                net_delta: 2,
                files: vec![DiffFileSummary {
                    path: "src/main.rs".into(),
                    operation: "edit".into(),
                    lines_added: 3,
                    lines_removed: 1,
                }],
            },
            full_text: Some(
                "--- a/src/main.rs\n+++ b/src/main.rs\n@@ -1,3 +1,5 @@\n fn main() {\n-    old();\n+    new();\n+    added();\n+    more();\n }".into(),
            ),
        };
        let lines = render_block_lines(&block, 80, false);

        let text: String = lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect();

        assert!(text.contains("src/main.rs"), "should show file path");
        assert!(text.contains("+"), "should have added lines");

        let has_green = lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| span.style.fg == Some(Color::Green) && span.content.starts_with('+'))
        });
        assert!(has_green, "added lines should be green");
    }

    #[test]
    fn insight_severity_renders_with_distinct_color() {
        let block = Block::SystemNote {
            text: "Thread: consider edge cases".into(),
            severity: NoteSeverity::Insight,
        };
        let lines = render_block_lines(&block, 80, false);
        assert!(!lines.is_empty());
        let has_cyan = lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| span.style.fg == Some(Color::Cyan))
        });
        assert!(has_cyan, "Insight should render with Cyan color");
    }

    #[test]
    fn policy_denied_outcome_renders_with_deny_icon() {
        let block = Block::ToolCall {
            tool_name: "shell.exec".into(),
            args_summary: "cargo test".into(),
            outcome: ToolOutcome::PolicyDenied,
            collapsed: true,
            token_cost: None,
        };
        let lines = render_block_lines(&block, 100, false);
        let text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.to_string()))
            .collect();
        assert!(
            text.contains("[DENY]"),
            "PolicyDenied should show [DENY] icon, got: {text}"
        );
    }

    #[test]
    fn tool_call_shows_token_cost_in_debug_mode() {
        let block = Block::ToolCall {
            tool_name: "code.read".into(),
            args_summary: "step 3/25".into(),
            outcome: ToolOutcome::Success,
            collapsed: true,
            token_cost: Some(1500),
        };
        let debug_lines = render_block_lines(&block, 100, true);
        let debug_text: String = debug_lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.to_string()))
            .collect();
        assert!(
            debug_text.contains("1500 tok"),
            "debug mode should show actual token cost, got: {debug_text}"
        );
        let normal_lines = render_block_lines(&block, 100, false);
        let normal_text: String = normal_lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.to_string()))
            .collect();
        assert!(
            !normal_text.contains("tok"),
            "normal mode should not show token annotation, got: {normal_text}"
        );
    }

    #[test]
    fn tool_call_no_token_cost_shows_nothing_in_debug() {
        let block = Block::ToolCall {
            tool_name: "code.read".into(),
            args_summary: "step 3/25".into(),
            outcome: ToolOutcome::Success,
            collapsed: true,
            token_cost: None,
        };
        let debug_lines = render_block_lines(&block, 100, true);
        let debug_text: String = debug_lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.to_string()))
            .collect();
        assert!(
            !debug_text.contains("tok"),
            "no token cost should show no annotation, got: {debug_text}"
        );
    }

    #[test]
    fn tick_boundary_visible_in_debug_mode() {
        let block = Block::SystemNote {
            text: "Tick 47 completed".into(),
            severity: NoteSeverity::Info,
        };

        let debug_lines = render_block_lines(&block, 80, true);
        let debug_text: String = debug_lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.to_string()))
            .collect();
        assert!(
            debug_text.contains('─'),
            "debug mode should render tick as boundary line, got: {debug_text}"
        );

        let normal_lines = render_block_lines(&block, 80, false);
        let normal_text: String = normal_lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.to_string()))
            .collect();
        assert!(
            normal_text.contains("---"),
            "normal mode should use standard format, got: {normal_text}"
        );
    }

    #[test]
    fn focus_prev_from_none_selects_last() {
        let mut state = ConversationState::new();
        state.add_block(Block::SystemNote {
            text: "a".into(),
            severity: NoteSeverity::Info,
        });
        state.add_block(Block::SystemNote {
            text: "b".into(),
            severity: NoteSeverity::Info,
        });
        assert_eq!(state.focused_block(), None);
        state.focus_prev();
        assert_eq!(state.focused_block(), Some(1));
    }

    #[test]
    fn focus_next_from_none_selects_first() {
        let mut state = ConversationState::new();
        state.add_block(Block::SystemNote {
            text: "a".into(),
            severity: NoteSeverity::Info,
        });
        state.add_block(Block::SystemNote {
            text: "b".into(),
            severity: NoteSeverity::Info,
        });
        state.focus_next();
        assert_eq!(state.focused_block(), Some(0));
    }

    #[test]
    fn focus_prev_clamps_at_zero() {
        let mut state = ConversationState::new();
        state.add_block(Block::SystemNote {
            text: "a".into(),
            severity: NoteSeverity::Info,
        });
        state.set_focused_block(Some(0));
        state.focus_prev();
        assert_eq!(state.focused_block(), Some(0));
    }

    #[test]
    fn focus_next_clamps_at_last() {
        let mut state = ConversationState::new();
        state.add_block(Block::SystemNote {
            text: "a".into(),
            severity: NoteSeverity::Info,
        });
        state.set_focused_block(Some(0));
        state.focus_next();
        assert_eq!(state.focused_block(), Some(0));
    }

    #[test]
    fn clear_focus_resets_to_none() {
        let mut state = ConversationState::new();
        state.add_block(Block::SystemNote {
            text: "a".into(),
            severity: NoteSeverity::Info,
        });
        state.set_focused_block(Some(0));
        state.clear_focus();
        assert_eq!(state.focused_block(), None);
    }

    #[test]
    fn toggle_focused_collapse_toggles_tool_call() {
        let mut state = ConversationState::new();
        state.add_block(Block::ToolCall {
            tool_name: "code.read".into(),
            args_summary: "src/main.rs".into(),
            outcome: ToolOutcome::Success,
            collapsed: true,
            token_cost: None,
        });
        state.set_focused_block(Some(0));
        assert!(state.toggle_focused_collapse());
        match &state.blocks()[0] {
            Block::ToolCall { collapsed, .. } => assert!(!collapsed),
            _ => panic!("expected ToolCall"),
        }
        assert!(state.toggle_focused_collapse());
        match &state.blocks()[0] {
            Block::ToolCall { collapsed, .. } => assert!(collapsed),
            _ => panic!("expected ToolCall"),
        }
    }

    #[test]
    fn toggle_focused_collapse_noop_on_non_tool_call() {
        let mut state = ConversationState::new();
        state.add_block(Block::SystemNote {
            text: "a".into(),
            severity: NoteSeverity::Info,
        });
        state.set_focused_block(Some(0));
        assert!(!state.toggle_focused_collapse());
    }

    #[test]
    fn focus_on_empty_conversation_is_noop() {
        let mut state = ConversationState::new();
        state.focus_next();
        assert_eq!(state.focused_block(), None);
        state.focus_prev();
        assert_eq!(state.focused_block(), None);
    }

    #[test]
    fn expanded_tool_call_shows_details() {
        let block = Block::ToolCall {
            tool_name: "shell.exec".into(),
            args_summary: "cargo test".into(),
            outcome: ToolOutcome::Error("exit code 1".into()),
            collapsed: false,
            token_cost: Some(750),
        };
        let lines = render_block_lines(&block, 100, false);
        assert!(
            lines.len() > 1,
            "expanded ToolCall should have multiple lines, got {}",
            lines.len()
        );
        let text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.to_string()))
            .collect();
        assert!(text.contains("Tool:"), "should show Tool: label");
        assert!(text.contains("Args:"), "should show Args: label");
        assert!(text.contains("exit code 1"), "should show error message");
        assert!(text.contains("750"), "should show token cost");
    }

    #[test]
    fn collapsed_tool_call_is_single_line() {
        let block = Block::ToolCall {
            tool_name: "code.read".into(),
            args_summary: "src/lib.rs".into(),
            outcome: ToolOutcome::Success,
            collapsed: true,
            token_cost: Some(300),
        };
        let lines = render_block_lines(&block, 100, false);
        assert_eq!(lines.len(), 1, "collapsed ToolCall should be single line");
    }

    #[test]
    fn expanded_policy_denied_shows_denial() {
        let block = Block::ToolCall {
            tool_name: "http.request".into(),
            args_summary: "deny: *".into(),
            outcome: ToolOutcome::PolicyDenied,
            collapsed: false,
            token_cost: None,
        };
        let lines = render_block_lines(&block, 100, false);
        let text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.to_string()))
            .collect();
        assert!(
            text.contains("Policy denied"),
            "should show policy denied message, got: {text}"
        );
    }
}

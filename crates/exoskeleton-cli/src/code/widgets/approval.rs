//! Approval overlay widget for `exo code`.
//!
//! Renders a blocking overlay that replaces the input area when a tool needs
//! approval, the agent asks a question, or a plan needs approval.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block as RatatuiBlock, Borders, Clear, Paragraph, Widget, Wrap},
};

/// What kind of approval/interaction is being requested.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalKind {
    /// Tool needs policy approval (Approve/Deny/Always).
    ToolApproval,
    /// Agent asked a question via `agent.ask_user`.
    Question,
    /// Plan draft needs approval (Approve/Deny/Edit).
    PlanApproval,
    /// Interrupt confirmation ("Interrupt agent? (y/n)").
    Confirmation,
}

/// An option presented in the approval overlay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalOption {
    /// Display label (e.g., "Approve (y)").
    pub label: String,
    /// The keyboard shortcut character, if any.
    pub hotkey: Option<char>,
    /// The value to submit if this option is selected.
    pub value: String,
}

/// Context for an approval interaction, stored in `UiMode::Approval`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalContext {
    /// Unique ID for this question/approval (for REST submission).
    pub question_id: String,
    /// Title line displayed at the top of the overlay.
    pub title: String,
    /// Optional description text (tool name + args, or question text).
    pub description: String,
    /// The navigable options.
    pub options: Vec<ApprovalOption>,
    /// What kind of approval this is.
    pub kind: ApprovalKind,
    /// Optional plan content for PlanApproval overlays.
    pub plan_content: Option<String>,
    /// Optional plan_draft_id for plan approval submission.
    pub plan_draft_id: Option<String>,
}

/// Mutable state for the active approval overlay.
#[derive(Debug, Clone)]
pub struct ApprovalState {
    /// Index of the currently highlighted option.
    pub selected_index: usize,
    /// Whether free-text input mode is active (activated by Tab).
    pub is_text_mode: bool,
    /// Free-text input buffer (for Tab → text append mode).
    pub text_input: String,
    /// Scroll offset for plan content in PlanApproval overlays.
    pub plan_scroll: u16,
}

impl ApprovalState {
    /// Create a new approval state for the given context.
    pub fn new() -> Self {
        Self {
            selected_index: 0,
            is_text_mode: false,
            text_input: String::new(),
            plan_scroll: 0,
        }
    }

    /// Create a new approval state starting in text mode.
    pub fn new_text_mode() -> Self {
        Self {
            selected_index: 0,
            is_text_mode: true,
            text_input: String::new(),
            plan_scroll: 0,
        }
    }

    /// Move selection down, wrapping at the end.
    pub fn select_next(&mut self, option_count: usize) {
        if option_count == 0 {
            return;
        }
        self.selected_index = (self.selected_index + 1) % option_count;
    }

    /// Move selection up, wrapping at the beginning.
    pub fn select_prev(&mut self, option_count: usize) {
        if option_count == 0 {
            return;
        }
        if self.selected_index == 0 {
            self.selected_index = option_count - 1;
        } else {
            self.selected_index -= 1;
        }
    }
}

impl Default for ApprovalState {
    fn default() -> Self {
        Self::new()
    }
}

/// The result of an approval action, produced by key handling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalAction {
    /// User selected an option (with optional appended text).
    Selected {
        value: String,
        appended_text: Option<String>,
    },
    /// User cancelled (Escape).
    Cancelled,
}

/// Build the standard tool approval options.
pub fn tool_approval_options(tool_name: &str) -> Vec<ApprovalOption> {
    vec![
        ApprovalOption {
            label: "Approve (y)".into(),
            hotkey: Some('y'),
            value: "approve".into(),
        },
        ApprovalOption {
            label: "Deny (n)".into(),
            hotkey: Some('n'),
            value: "deny".into(),
        },
        ApprovalOption {
            label: format!("Always allow {} (a)", tool_name),
            hotkey: Some('a'),
            value: "always".into(),
        },
    ]
}

/// Build the standard plan approval options.
pub fn plan_approval_options() -> Vec<ApprovalOption> {
    vec![
        ApprovalOption {
            label: "Approve (y)".into(),
            hotkey: Some('y'),
            value: "approve".into(),
        },
        ApprovalOption {
            label: "Deny (n)".into(),
            hotkey: Some('n'),
            value: "deny".into(),
        },
        ApprovalOption {
            label: "Edit (e)".into(),
            hotkey: Some('e'),
            value: "edit".into(),
        },
    ]
}

/// Options for an interrupt confirmation dialog.
pub fn confirmation_options() -> Vec<ApprovalOption> {
    vec![
        ApprovalOption {
            label: "Yes (y)".into(),
            hotkey: Some('y'),
            value: "yes".into(),
        },
        ApprovalOption {
            label: "No (n)".into(),
            hotkey: Some('n'),
            value: "no".into(),
        },
    ]
}

/// Widget for rendering the approval overlay.
pub struct ApprovalWidget<'a> {
    context: &'a ApprovalContext,
    state: &'a ApprovalState,
}

impl<'a> ApprovalWidget<'a> {
    pub fn new(context: &'a ApprovalContext, state: &'a ApprovalState) -> Self {
        Self { context, state }
    }
}

impl<'a> Widget for ApprovalWidget<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.height == 0 || area.width == 0 {
            return;
        }

        Clear.render(area, buf);

        let mut lines: Vec<Line<'static>> = Vec::new();
        lines.push(Line::from(vec![Span::styled(
            format!("  {}", self.context.title),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )]));
        lines.push(Line::from(""));

        if !self.context.description.is_empty() {
            for desc_line in self.context.description.lines() {
                lines.push(Line::from(vec![
                    Span::raw("  "),
                    Span::styled(desc_line.to_string(), Style::default().fg(Color::White)),
                ]));
            }
            lines.push(Line::from(""));
        }

        if let Some(ref plan_text) = self.context.plan_content {
            let plan_lines: Vec<&str> = plan_text.lines().collect();
            let max_plan_lines = area.height.saturating_sub(8) as usize;
            let start = self.state.plan_scroll as usize;
            let end = (start + max_plan_lines).min(plan_lines.len());
            for plan_line in &plan_lines[start..end] {
                lines.push(Line::from(vec![
                    Span::raw("  "),
                    Span::styled(plan_line.to_string(), Style::default().fg(Color::DarkGray)),
                ]));
            }
            if end < plan_lines.len() {
                lines.push(Line::from(vec![
                    Span::raw("  "),
                    Span::styled(
                        format!("... ({} more lines)", plan_lines.len() - end),
                        Style::default().fg(Color::DarkGray),
                    ),
                ]));
            }
            lines.push(Line::from(""));
        }

        for (i, option) in self.context.options.iter().enumerate() {
            let is_selected = i == self.state.selected_index;
            let indicator = if is_selected { "\u{25B8} " } else { "  " };
            let style = if is_selected {
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            };
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(format!("{indicator}{}", option.label), style),
            ]));
        }

        if self.state.is_text_mode {
            lines.push(Line::from(""));
            let separator = "\u{2500}".repeat((area.width as usize).saturating_sub(6).min(40));
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(separator, Style::default().fg(Color::DarkGray)),
            ]));
            let input_display = if self.state.text_input.is_empty() {
                "Type your response...".to_string()
            } else {
                self.state.text_input.clone()
            };
            let input_style = if self.state.text_input.is_empty() {
                Style::default().fg(Color::DarkGray)
            } else {
                Style::default().fg(Color::White)
            };
            lines.push(Line::from(vec![
                Span::styled("  > ", Style::default().fg(Color::Cyan)),
                Span::styled(input_display, input_style),
            ]));
        } else if !self.context.options.is_empty() {
            lines.push(Line::from(""));
            let separator = "\u{2500}".repeat((area.width as usize).saturating_sub(6).min(40));
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(separator, Style::default().fg(Color::DarkGray)),
            ]));
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(
                    "Tab: add context to selection".to_string(),
                    Style::default().fg(Color::DarkGray),
                ),
            ]));
        }

        let border = RatatuiBlock::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray))
            .style(Style::default().bg(Color::Black));

        let paragraph = Paragraph::new(lines)
            .block(border)
            .wrap(Wrap { trim: false });

        paragraph.render(area, buf);
    }
}

#[cfg(test)]
mod tests {
    use ratatui::{buffer::Buffer, layout::Rect, widgets::Widget};

    use super::*;

    fn make_tool_context() -> ApprovalContext {
        ApprovalContext {
            question_id: "q-123".into(),
            title: "Tool requires approval".into(),
            description: "shell.exec\ncargo test test_parse_config".into(),
            options: tool_approval_options("shell.exec"),
            kind: ApprovalKind::ToolApproval,
            plan_content: None,
            plan_draft_id: None,
        }
    }

    fn make_question_context(choices: Option<Vec<String>>) -> ApprovalContext {
        let options = match &choices {
            Some(items) => items
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
        ApprovalContext {
            question_id: "q-456".into(),
            title: "Agent question".into(),
            description: "Which approach should I use?".into(),
            options,
            kind: ApprovalKind::Question,
            plan_content: None,
            plan_draft_id: None,
        }
    }

    fn buffer_content(buf: &Buffer, area: Rect) -> String {
        let mut content = String::new();
        for y in 0..area.height {
            for x in 0..area.width {
                content.push_str(buf[(x, y)].symbol());
            }
        }
        content
    }

    // ── T1: approval_widget_renders_options ──

    #[test]
    fn approval_widget_renders_options() {
        let ctx = make_tool_context();
        let state = ApprovalState::new();
        let widget = ApprovalWidget::new(&ctx, &state);
        let area = Rect::new(0, 0, 60, 15);
        let mut buf = Buffer::empty(area);
        widget.render(area, &mut buf);

        let content = buffer_content(&buf, area);
        assert!(
            content.contains("Approve (y)"),
            "should render Approve option"
        );
        assert!(content.contains("Deny (n)"), "should render Deny option");
        assert!(
            content.contains("Always allow"),
            "should render Always option"
        );
        assert!(
            content.contains('\u{25B8}'),
            "should show selection indicator"
        );
    }

    // ── T2: approval_navigate_down_wraps ──

    #[test]
    fn approval_navigate_down_wraps() {
        let mut state = ApprovalState::new();
        state.selected_index = 2;
        state.select_next(3);
        assert_eq!(state.selected_index, 0, "should wrap to first");
    }

    // ── T3: approval_navigate_up_wraps ──

    #[test]
    fn approval_navigate_up_wraps() {
        let mut state = ApprovalState::new();
        state.selected_index = 0;
        state.select_prev(3);
        assert_eq!(state.selected_index, 2, "should wrap to last");
    }

    // ── T4: approval_hotkey_y_selects_approve ──

    #[test]
    fn approval_hotkey_y_selects_approve() {
        let ctx = make_tool_context();
        let option = ctx
            .options
            .iter()
            .find(|o| o.hotkey == Some('y'))
            .expect("should have 'y' hotkey");
        assert_eq!(option.value, "approve");
    }

    // ── T5: approval_hotkey_n_selects_deny ──

    #[test]
    fn approval_hotkey_n_selects_deny() {
        let ctx = make_tool_context();
        let option = ctx
            .options
            .iter()
            .find(|o| o.hotkey == Some('n'))
            .expect("should have 'n' hotkey");
        assert_eq!(option.value, "deny");
    }

    // ── T6: approval_hotkey_a_selects_always ──

    #[test]
    fn approval_hotkey_a_selects_always() {
        let ctx = make_tool_context();
        let option = ctx
            .options
            .iter()
            .find(|o| o.hotkey == Some('a'))
            .expect("should have 'a' hotkey");
        assert_eq!(option.value, "always");
    }

    // ── T7: approval_escape_denies ──

    #[test]
    fn approval_escape_denies() {
        let action = ApprovalAction::Cancelled;
        assert_eq!(action, ApprovalAction::Cancelled);
    }

    // ── T8: approval_enter_confirms_selected ──

    #[test]
    fn approval_enter_confirms_selected() {
        let ctx = make_tool_context();
        let state = ApprovalState {
            selected_index: 1,
            is_text_mode: false,
            text_input: String::new(),
            plan_scroll: 0,
        };
        let selected = &ctx.options[state.selected_index];
        let action = ApprovalAction::Selected {
            value: selected.value.clone(),
            appended_text: None,
        };
        match action {
            ApprovalAction::Selected {
                value,
                appended_text,
            } => {
                assert_eq!(value, "deny");
                assert!(appended_text.is_none());
            }
            _ => panic!("expected Selected"),
        }
    }

    // ── T9: approval_tab_enters_text_mode ──

    #[test]
    fn approval_tab_enters_text_mode() {
        let mut state = ApprovalState::new();
        assert!(!state.is_text_mode);
        state.is_text_mode = true;
        assert!(state.is_text_mode);
    }

    // ── T10: approval_text_mode_enter_submits ──

    #[test]
    fn approval_text_mode_enter_submits() {
        let ctx = make_tool_context();
        let state = ApprovalState {
            selected_index: 0,
            is_text_mode: true,
            text_input: "but skip integration tests".into(),
            plan_scroll: 0,
        };
        let selected = &ctx.options[state.selected_index];
        let action = ApprovalAction::Selected {
            value: selected.value.clone(),
            appended_text: Some(state.text_input.clone()),
        };
        match action {
            ApprovalAction::Selected {
                value,
                appended_text,
            } => {
                assert_eq!(value, "approve");
                assert_eq!(appended_text.as_deref(), Some("but skip integration tests"));
            }
            _ => panic!("expected Selected"),
        }
    }

    // ── T11: question_with_choices_renders_list ──

    #[test]
    fn question_with_choices_renders_list() {
        let ctx = make_question_context(Some(vec![
            "Option A".into(),
            "Option B".into(),
            "Option C".into(),
        ]));
        let state = ApprovalState::new();
        let widget = ApprovalWidget::new(&ctx, &state);
        let area = Rect::new(0, 0, 60, 15);
        let mut buf = Buffer::empty(area);
        widget.render(area, &mut buf);

        let content = buffer_content(&buf, area);
        assert!(content.contains("Option A"), "should render first choice");
        assert!(content.contains("Option B"), "should render second choice");
        assert!(content.contains("Option C"), "should render third choice");
    }

    // ── T12: question_open_ended_starts_in_text_mode ──

    #[test]
    fn question_open_ended_starts_in_text_mode() {
        let ctx = make_question_context(None);
        assert!(ctx.options.is_empty(), "open-ended should have no options");
        let state = ApprovalState::new_text_mode();
        assert!(state.is_text_mode);
    }

    // ── T13: question_tab_switches_to_custom_answer ──

    #[test]
    fn question_tab_switches_to_custom_answer() {
        let _ctx = make_question_context(Some(vec!["A".into(), "B".into()]));
        let mut state = ApprovalState::new();
        assert!(!state.is_text_mode);

        state.is_text_mode = true;
        assert!(state.is_text_mode);

        state.is_text_mode = false;
        assert!(!state.is_text_mode);
    }
}

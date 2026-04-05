//! Debug banner widget for `exo code`.
//!
//! Renders a 4-line info banner between the status bar and conversation area
//! when toggled by F1. Shows tick number, thread states, visual budget
//! progress bars, and active policy rules.
//!
//! Graceful degradation:
//! - Width >= 100: full banner (bars + thread details + policy)
//! - Width 60-99: numeric budgets, truncated thread names
//! - Width < 60: 2-line minimal banner (tick + budget only)

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Widget,
};

use crate::code::app::{DebugBudgetInfo, DebugPlanSummary, DebugState};

/// Widget that renders the F1 debug banner.
///
/// Not rendered when `DebugState.visible` is false.
pub struct DebugBannerWidget<'a> {
    state: &'a DebugState,
    width: u16,
}

impl<'a> DebugBannerWidget<'a> {
    /// Create a new debug banner widget.
    pub fn new(state: &'a DebugState, width: u16) -> Self {
        Self { state, width }
    }

    /// How many lines this banner needs for the current width.
    pub fn height(state: &DebugState, width: u16) -> u16 {
        if !state.visible {
            return 0;
        }
        let base = if width < 60 { 2 } else { 4 };
        let plan_line = if state.plan_summary.is_some() && width >= 60 {
            1
        } else {
            0
        };
        base + plan_line
    }
}

/// Render the tick info line.
///
/// Format: `TICK #47 | inner-loop active | 3/25 steps | 4,231/50,000 tok`
fn render_tick_line(state: &DebugState) -> Line<'static> {
    let loop_status = if state.inner_loop_active {
        "inner-loop active"
    } else {
        "idle"
    };

    let mut spans = vec![
        Span::styled(
            format!("  TICK #{}", state.tick_number),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(" │ {loop_status}"),
            Style::default().fg(Color::DarkGray),
        ),
    ];

    if !state.step_count.is_empty() {
        spans.push(Span::styled(
            format!(" │ {}", state.step_count),
            Style::default().fg(Color::DarkGray),
        ));
    }

    if !state.token_count.is_empty() {
        spans.push(Span::styled(
            format!(" │ {}", state.token_count),
            Style::default().fg(Color::DarkGray),
        ));
    }

    Line::from(spans)
}

/// Render the thread info line.
///
/// Format: `Threads: meta-cognition (idle) | creative-synthesis (contributed: "insight")`
fn render_thread_line(state: &DebugState, max_width: u16) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = vec![Span::styled(
        "  Threads: ".to_string(),
        Style::default().fg(Color::DarkGray),
    )];

    if state.threads.is_empty() {
        spans.push(Span::styled(
            "none".to_string(),
            Style::default().fg(Color::DarkGray),
        ));
        return Line::from(spans);
    }

    let mut remaining_width = max_width.saturating_sub(11) as usize;

    for (i, thread) in state.threads.iter().enumerate() {
        if i > 0 {
            if remaining_width < 5 {
                break;
            }
            spans.push(Span::styled(
                " │ ".to_string(),
                Style::default().fg(Color::DarkGray),
            ));
            remaining_width = remaining_width.saturating_sub(3);
        }

        let truncated_name = if max_width < 100 {
            truncate_string(&thread.name, 12)
        } else {
            thread.name.clone()
        };

        let state_str = if max_width < 100 {
            truncate_string(&thread.state, 10)
        } else {
            thread.state.clone()
        };

        let entry = format!("{truncated_name} ({state_str})");
        let entry_len = entry.len();

        if entry_len > remaining_width {
            break;
        }

        let color = if thread.contributed {
            Color::Yellow
        } else {
            Color::DarkGray
        };
        spans.push(Span::styled(entry, Style::default().fg(color)));
        remaining_width = remaining_width.saturating_sub(entry_len);
    }

    Line::from(spans)
}

/// Render the budget line with visual progress bars.
///
/// Full format: `Budget: [bars] N% tokens  [bars] N% steps`
/// Narrow format: `Budget: N% tokens | N% steps`
fn render_budget_line(budget: &DebugBudgetInfo, width: u16) -> Line<'static> {
    if width >= 100 {
        let token_bar = render_bar(budget.token_percent_used, 20);
        let step_bar = render_bar(budget.step_percent_used, 20);

        Line::from(vec![
            Span::styled(
                "  Budget: ".to_string(),
                Style::default().fg(Color::DarkGray),
            ),
            Span::styled(token_bar, Style::default().fg(Color::Green)),
            Span::styled(
                format!(" {}% tokens", budget.token_percent_used),
                Style::default().fg(Color::DarkGray),
            ),
            Span::styled("  ".to_string(), Style::default()),
            Span::styled(step_bar, Style::default().fg(Color::Blue)),
            Span::styled(
                format!(" {}% steps", budget.step_percent_used),
                Style::default().fg(Color::DarkGray),
            ),
        ])
    } else {
        Line::from(vec![
            Span::styled(
                "  Budget: ".to_string(),
                Style::default().fg(Color::DarkGray),
            ),
            Span::styled(
                format!(
                    "{}% tokens │ {}% steps",
                    budget.token_percent_used, budget.step_percent_used
                ),
                Style::default().fg(Color::DarkGray),
            ),
        ])
    }
}

/// Render the policy line.
///
/// Format: `Policy: shell.exec=ask | code.*=allow | http.*=deny`
fn render_policy_line(state: &DebugState) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = vec![Span::styled(
        "  Policy: ".to_string(),
        Style::default().fg(Color::DarkGray),
    )];

    if state.policies.is_empty() {
        spans.push(Span::styled(
            "default".to_string(),
            Style::default().fg(Color::DarkGray),
        ));
        return Line::from(spans);
    }

    for (i, policy) in state.policies.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(
                " │ ".to_string(),
                Style::default().fg(Color::DarkGray),
            ));
        }

        let color = match policy.outcome.as_str() {
            "allow" => Color::Green,
            "deny" => Color::Red,
            "ask" => Color::Yellow,
            _ => Color::DarkGray,
        };

        spans.push(Span::styled(
            format!("{}={}", policy.tool_pattern, policy.outcome),
            Style::default().fg(color),
        ));
    }

    Line::from(spans)
}

/// Render the plan summary line.
///
/// Format: `Plan: 2/5 tasks [icons] | "objective"`
fn render_plan_line(plan: &DebugPlanSummary) -> Line<'static> {
    Line::from(vec![
        Span::styled("  Plan: ".to_string(), Style::default().fg(Color::DarkGray)),
        Span::styled(
            format!("{}/{} tasks", plan.completed, plan.total),
            Style::default().fg(Color::Cyan),
        ),
        Span::styled(
            format!(" {} ", plan.progress_icons),
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled("│ ".to_string(), Style::default().fg(Color::DarkGray)),
        Span::styled(
            format!("\"{}\"", truncate_string(&plan.objective, 40)),
            Style::default().fg(Color::DarkGray),
        ),
    ])
}

/// Render a visual progress bar using Unicode block characters.
fn render_bar(percent_used: u8, bar_width: usize) -> String {
    let filled = (percent_used as usize * bar_width) / 100;
    let empty = bar_width.saturating_sub(filled);
    let mut bar = String::with_capacity(bar_width);
    for _ in 0..filled {
        bar.push('\u{2588}');
    }
    for _ in 0..empty {
        bar.push('\u{2591}');
    }
    bar
}

/// Truncate a string to max_len, adding "..." if needed.
fn truncate_string(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else if max_len <= 3 {
        s.chars().take(max_len).collect()
    } else {
        format!("{}...", &s[..max_len.saturating_sub(3)])
    }
}

impl<'a> Widget for DebugBannerWidget<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if !self.state.visible || area.height == 0 {
            return;
        }

        let width = self.width.max(area.width);
        let mut y = area.y;

        let tick_line = render_tick_line(self.state);
        buf.set_line(area.x, y, &tick_line, area.width);
        y += 1;

        if width < 60 {
            if y < area.y + area.height {
                let budget_line = render_budget_line(&self.state.budget, width);
                buf.set_line(area.x, y, &budget_line, area.width);
            }
            return;
        }

        if y < area.y + area.height {
            let thread_line = render_thread_line(self.state, width);
            buf.set_line(area.x, y, &thread_line, area.width);
            y += 1;
        }

        if y < area.y + area.height {
            let budget_line = render_budget_line(&self.state.budget, width);
            buf.set_line(area.x, y, &budget_line, area.width);
            y += 1;
        }

        if y < area.y + area.height {
            let policy_line = render_policy_line(self.state);
            buf.set_line(area.x, y, &policy_line, area.width);
            y += 1;
        }

        if let Some(ref plan) = self.state.plan_summary {
            if y < area.y + area.height {
                let plan_line = render_plan_line(plan);
                buf.set_line(area.x, y, &plan_line, area.width);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use ratatui::{buffer::Buffer, layout::Rect, widgets::Widget};

    use crate::code::app::{
        DebugBudgetInfo, DebugPlanSummary, DebugPolicyInfo, DebugState, DebugThreadInfo,
    };

    use super::{render_bar, render_budget_line, render_plan_line, DebugBannerWidget};

    fn make_debug_state() -> DebugState {
        DebugState {
            visible: true,
            tick_number: 47,
            inner_loop_active: true,
            step_count: "3/25 steps".into(),
            token_count: "4,231/50,000 tok".into(),
            threads: vec![
                DebugThreadInfo {
                    name: "meta-cognition".into(),
                    state: "idle".into(),
                    contributed: false,
                },
                DebugThreadInfo {
                    name: "creative-synthesis".into(),
                    state: "idle".into(),
                    contributed: false,
                },
            ],
            budget: DebugBudgetInfo {
                token_percent_used: 8,
                token_label: "4,231/50,000".into(),
                step_percent_used: 12,
                step_label: "3/25".into(),
            },
            policies: vec![
                DebugPolicyInfo {
                    tool_pattern: "shell.exec".into(),
                    outcome: "ask".into(),
                },
                DebugPolicyInfo {
                    tool_pattern: "code.*".into(),
                    outcome: "allow".into(),
                },
                DebugPolicyInfo {
                    tool_pattern: "http.*".into(),
                    outcome: "deny".into(),
                },
            ],
            plan_summary: None,
            initial_token_budget: Some(50_000),
            initial_step_limit: Some(25),
        }
    }

    // ── T1: debug_banner_renders_tick_line ──

    #[test]
    fn debug_banner_renders_tick_line() {
        let state = make_debug_state();
        let widget = DebugBannerWidget::new(&state, 120);
        let area = Rect::new(0, 0, 120, 5);
        let mut buf = Buffer::empty(area);
        widget.render(area, &mut buf);

        let first_line: String = (0..120).map(|x| buf[(x, 0)].symbol().to_string()).collect();
        assert!(
            first_line.contains("TICK #47"),
            "tick line should contain tick number, got: {first_line}"
        );
        assert!(
            first_line.contains("3/25 steps"),
            "tick line should contain step count, got: {first_line}"
        );
        assert!(
            first_line.contains("4,231/50,000 tok"),
            "tick line should contain token count, got: {first_line}"
        );
    }

    // ── T2: debug_banner_renders_thread_line ──

    #[test]
    fn debug_banner_renders_thread_line() {
        let state = make_debug_state();
        let widget = DebugBannerWidget::new(&state, 120);
        let area = Rect::new(0, 0, 120, 5);
        let mut buf = Buffer::empty(area);
        widget.render(area, &mut buf);

        let thread_line: String = (0..120).map(|x| buf[(x, 1)].symbol().to_string()).collect();
        assert!(
            thread_line.contains("meta-cognition"),
            "thread line should contain thread name, got: {thread_line}"
        );
        assert!(
            thread_line.contains("creative-synthesis"),
            "thread line should contain second thread name, got: {thread_line}"
        );
        assert!(
            thread_line.contains("idle"),
            "thread line should show state, got: {thread_line}"
        );
    }

    // ── T3: debug_banner_renders_budget_bars ──

    #[test]
    fn debug_banner_renders_budget_bars() {
        let state = make_debug_state();
        let widget = DebugBannerWidget::new(&state, 120);
        let area = Rect::new(0, 0, 120, 5);
        let mut buf = Buffer::empty(area);
        widget.render(area, &mut buf);

        let budget_line: String = (0..120).map(|x| buf[(x, 2)].symbol().to_string()).collect();
        assert!(
            budget_line.contains("Budget:"),
            "budget line should start with Budget:, got: {budget_line}"
        );
        assert!(
            budget_line.contains("8% tokens"),
            "budget line should show token percentage, got: {budget_line}"
        );
        assert!(
            budget_line.contains("12% steps"),
            "budget line should show step percentage, got: {budget_line}"
        );
        assert!(
            budget_line.contains('\u{2588}') || budget_line.contains('\u{2591}'),
            "budget line should contain bar characters, got: {budget_line}"
        );
    }

    // ── T4: debug_banner_renders_policy_line ──

    #[test]
    fn debug_banner_renders_policy_line() {
        let state = make_debug_state();
        let widget = DebugBannerWidget::new(&state, 120);
        let area = Rect::new(0, 0, 120, 5);
        let mut buf = Buffer::empty(area);
        widget.render(area, &mut buf);

        let policy_line: String = (0..120).map(|x| buf[(x, 3)].symbol().to_string()).collect();
        assert!(
            policy_line.contains("shell.exec=ask"),
            "policy line should show ask policy, got: {policy_line}"
        );
        assert!(
            policy_line.contains("code.*=allow"),
            "policy line should show allow policy, got: {policy_line}"
        );
        assert!(
            policy_line.contains("http.*=deny"),
            "policy line should show deny policy, got: {policy_line}"
        );
    }

    // ── T5: debug_banner_narrow_terminal_numeric_only ──

    #[test]
    fn debug_banner_narrow_terminal_numeric_only() {
        let state = make_debug_state();
        let widget = DebugBannerWidget::new(&state, 80);
        let area = Rect::new(0, 0, 80, 5);
        let mut buf = Buffer::empty(area);
        widget.render(area, &mut buf);

        let budget_line: String = (0..80).map(|x| buf[(x, 2)].symbol().to_string()).collect();
        assert!(
            budget_line.contains("8% tokens"),
            "narrow budget should show percentage, got: {budget_line}"
        );
        let bar = render_bar(8, 20);
        assert!(
            !budget_line.contains(&bar),
            "narrow budget should not show full bar, got: {budget_line}"
        );

        let thread_line: String = (0..80).map(|x| buf[(x, 1)].symbol().to_string()).collect();
        assert!(
            thread_line.contains("Threads:"),
            "should still show thread header, got: {thread_line}"
        );
    }

    // ── T6: debug_banner_very_narrow_two_lines_only ──

    #[test]
    fn debug_banner_very_narrow_two_lines_only() {
        let state = make_debug_state();
        assert_eq!(DebugBannerWidget::height(&state, 50), 2);

        let widget = DebugBannerWidget::new(&state, 50);
        let area = Rect::new(0, 0, 50, 2);
        let mut buf = Buffer::empty(area);
        widget.render(area, &mut buf);

        let first_line: String = (0..50).map(|x| buf[(x, 0)].symbol().to_string()).collect();
        assert!(
            first_line.contains("TICK #47"),
            "first line should be tick, got: {first_line}"
        );

        let second_line: String = (0..50).map(|x| buf[(x, 1)].symbol().to_string()).collect();
        assert!(
            second_line.contains("Budget:"),
            "second line should be budget, got: {second_line}"
        );
    }

    // ── T7: debug_banner_with_plan_summary ──

    #[test]
    fn debug_banner_with_plan_summary() {
        let mut state = make_debug_state();
        state.plan_summary = Some(DebugPlanSummary {
            completed: 2,
            total: 5,
            progress_icons: "✓✓▸○○".into(),
            objective: "Implement feature X".into(),
        });

        assert_eq!(DebugBannerWidget::height(&state, 120), 5);

        let widget = DebugBannerWidget::new(&state, 120);
        let area = Rect::new(0, 0, 120, 5);
        let mut buf = Buffer::empty(area);
        widget.render(area, &mut buf);

        let plan_line: String = (0..120).map(|x| buf[(x, 4)].symbol().to_string()).collect();
        assert!(
            plan_line.contains("2/5 tasks"),
            "plan line should show progress, got: {plan_line}"
        );
        assert!(
            plan_line.contains("Implement feature X"),
            "plan line should show objective, got: {plan_line}"
        );
    }

    // ── T8: debug_banner_thread_insight_highlighted ──

    #[test]
    fn debug_banner_thread_insight_highlighted() {
        let mut state = make_debug_state();
        state.threads[0] = DebugThreadInfo {
            name: "meta-cognition".into(),
            state: "contributed: \"consider edge case\"".into(),
            contributed: true,
        };

        let widget = DebugBannerWidget::new(&state, 120);
        let area = Rect::new(0, 0, 120, 5);
        let mut buf = Buffer::empty(area);
        widget.render(area, &mut buf);

        let thread_line: String = (0..120).map(|x| buf[(x, 1)].symbol().to_string()).collect();
        assert!(
            thread_line.contains("contributed"),
            "contributed thread should show insight, got: {thread_line}"
        );
    }

    #[test]
    fn debug_banner_hidden_when_not_visible() {
        let state = DebugState {
            visible: false,
            ..Default::default()
        };

        assert_eq!(DebugBannerWidget::height(&state, 120), 0);
    }

    #[test]
    fn render_bar_zero_percent() {
        let bar = render_bar(0, 10);
        assert_eq!(bar.chars().count(), 10);
        assert!(bar.chars().all(|c| c == '\u{2591}'));
    }

    #[test]
    fn render_bar_hundred_percent() {
        let bar = render_bar(100, 10);
        assert_eq!(bar.chars().count(), 10);
        assert!(bar.chars().all(|c| c == '\u{2588}'));
    }

    #[test]
    fn render_bar_fifty_percent() {
        let bar = render_bar(50, 10);
        assert_eq!(bar.chars().count(), 10);
        let filled: usize = bar.chars().filter(|&c| c == '\u{2588}').count();
        assert_eq!(filled, 5);
    }

    #[test]
    fn budget_line_numeric_below_100() {
        let budget = DebugBudgetInfo {
            token_percent_used: 42,
            token_label: "21,000/50,000".into(),
            step_percent_used: 60,
            step_label: "15/25".into(),
        };
        let line = render_budget_line(&budget, 80);
        let text: String = line.spans.iter().map(|s| s.content.to_string()).collect();
        assert!(text.contains("42% tokens"));
        assert!(text.contains("60% steps"));
        assert!(!text.contains('\u{2588}'));
    }

    #[test]
    fn plan_line_renders_icons() {
        let plan = DebugPlanSummary {
            completed: 3,
            total: 5,
            progress_icons: "✓✓✓○○".into(),
            objective: "Test objective".into(),
        };
        let line = render_plan_line(&plan);
        let text: String = line.spans.iter().map(|s| s.content.to_string()).collect();
        assert!(text.contains("3/5 tasks"));
        assert!(text.contains("Test objective"));
    }

    #[test]
    fn debug_banner_no_threads() {
        let mut state = make_debug_state();
        state.threads.clear();

        let widget = DebugBannerWidget::new(&state, 120);
        let area = Rect::new(0, 0, 120, 5);
        let mut buf = Buffer::empty(area);
        widget.render(area, &mut buf);

        let thread_line: String = (0..120).map(|x| buf[(x, 1)].symbol().to_string()).collect();
        assert!(
            thread_line.contains("none"),
            "empty threads should show 'none', got: {thread_line}"
        );
    }

    #[test]
    fn debug_banner_no_policies() {
        let mut state = make_debug_state();
        state.policies.clear();

        let widget = DebugBannerWidget::new(&state, 120);
        let area = Rect::new(0, 0, 120, 5);
        let mut buf = Buffer::empty(area);
        widget.render(area, &mut buf);

        let policy_line: String = (0..120).map(|x| buf[(x, 3)].symbol().to_string()).collect();
        assert!(
            policy_line.contains("default"),
            "empty policies should show 'default', got: {policy_line}"
        );
    }
}

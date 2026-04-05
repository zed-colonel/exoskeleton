//! Plan/task list display widget for `exo code`.
//!
//! Renders a vessel's structured Plan as a visual checklist with 6-state
//! status icons and a progress counter. Used by `Block::PlanSummary` in
//! the conversation view.

use exoskeleton_core::plan::{Plan, PlanTask, PlanTaskStatus};
use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

/// View model for a single plan task, ready for rendering.
#[derive(Debug, Clone, PartialEq)]
pub struct PlanTaskView {
    /// Status icon character.
    pub icon: char,
    /// Color for the status icon.
    pub icon_color: Color,
    /// Task description text.
    pub description: String,
    /// Whether this task's text should be dimmed (completed/skipped).
    pub dimmed: bool,
}

impl PlanTaskView {
    /// Convert a `PlanTask` into a renderable view.
    pub fn from_plan_task(task: &PlanTask) -> Self {
        let (icon, icon_color, dimmed) = match task.status {
            PlanTaskStatus::Completed => ('\u{2713}', Color::Green, false),
            PlanTaskStatus::InProgress => ('\u{25B8}', Color::Blue, false),
            PlanTaskStatus::Pending => ('\u{25CB}', Color::DarkGray, false),
            PlanTaskStatus::Failed => ('\u{2717}', Color::Red, false),
            PlanTaskStatus::Blocked => ('\u{2298}', Color::Yellow, false),
            PlanTaskStatus::Skipped => ('\u{2212}', Color::DarkGray, true),
        };
        Self {
            icon,
            icon_color,
            description: task.description.clone(),
            dimmed,
        }
    }
}

/// Render a complete Plan as ratatui Lines for display in the conversation.
pub fn render_plan_lines(plan: &Plan) -> Vec<Line<'static>> {
    let mut lines = Vec::new();

    lines.push(Line::from(""));
    lines.push(Line::from(vec![Span::styled(
        " Plan ",
        Style::default()
            .fg(Color::White)
            .bg(Color::Blue)
            .add_modifier(Modifier::BOLD),
    )]));

    if !plan.objective.is_empty() {
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(
                plan.objective.clone(),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
        ]));
    }

    lines.push(Line::from(""));

    if plan.tasks.is_empty() {
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(
                "No tasks defined".to_string(),
                Style::default().fg(Color::DarkGray),
            ),
        ]));
    } else {
        for task in &plan.tasks {
            let view = PlanTaskView::from_plan_task(task);
            let text_style = if view.dimmed {
                Style::default().fg(Color::DarkGray)
            } else {
                Style::default().fg(Color::White)
            };

            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(
                    format!("{} ", view.icon),
                    Style::default().fg(view.icon_color),
                ),
                Span::styled(view.description, text_style),
            ]));
        }

        let completed = plan
            .tasks
            .iter()
            .filter(|t| t.status == PlanTaskStatus::Completed)
            .count();
        let total = plan.tasks.len();

        lines.push(Line::from(""));
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(
                format!("Progress: {completed}/{total} tasks"),
                Style::default().fg(Color::DarkGray),
            ),
        ]));
    }

    lines.push(Line::from(""));
    lines
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use chrono::Utc;
    use exoskeleton_core::{
        id::PlanTaskId,
        plan::{Plan, PlanTask, PlanTaskStatus},
    };

    use super::*;

    fn make_task(desc: &str, status: PlanTaskStatus) -> PlanTask {
        PlanTask {
            id: PlanTaskId::new(),
            description: desc.into(),
            status,
            depends_on: Vec::new(),
            tool_hint: None,
            metadata: HashMap::new(),
        }
    }

    // ── T14: plan_summary_renders_status_icons ──

    #[test]
    fn plan_summary_renders_status_icons() {
        let plan = Plan {
            objective: "Test plan".into(),
            tasks: vec![
                make_task("Done task", PlanTaskStatus::Completed),
                make_task("Active task", PlanTaskStatus::InProgress),
                make_task("Waiting task", PlanTaskStatus::Pending),
                make_task("Broken task", PlanTaskStatus::Failed),
                make_task("Stuck task", PlanTaskStatus::Blocked),
                make_task("Omitted task", PlanTaskStatus::Skipped),
            ],
            updated_at: Utc::now(),
        };
        let lines = render_plan_lines(&plan);

        let all_text: String = lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect();

        assert!(
            all_text.contains('\u{2713}'),
            "should have checkmark (Completed)"
        );
        assert!(
            all_text.contains('\u{25B8}'),
            "should have triangle (InProgress)"
        );
        assert!(
            all_text.contains('\u{25CB}'),
            "should have circle (Pending)"
        );
        assert!(all_text.contains('\u{2717}'), "should have cross (Failed)");
        assert!(
            all_text.contains('\u{2298}'),
            "should have no-entry (Blocked)"
        );
        assert!(all_text.contains('\u{2212}'), "should have dash (Skipped)");
    }

    // ── T15: plan_summary_shows_progress_count ──

    #[test]
    fn plan_summary_shows_progress_count() {
        let plan = Plan {
            objective: "Build something".into(),
            tasks: vec![
                make_task("First", PlanTaskStatus::Completed),
                make_task("Second", PlanTaskStatus::Completed),
                make_task("Third", PlanTaskStatus::InProgress),
                make_task("Fourth", PlanTaskStatus::Pending),
                make_task("Fifth", PlanTaskStatus::Pending),
            ],
            updated_at: Utc::now(),
        };
        let lines = render_plan_lines(&plan);

        let all_text: String = lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect();

        assert!(
            all_text.contains("Progress: 2/5 tasks"),
            "should show progress counter, got: {all_text}"
        );
    }

    // ── T16: plan_summary_empty_plan ──

    #[test]
    fn plan_summary_empty_plan() {
        let plan = Plan {
            objective: "Empty plan".into(),
            tasks: Vec::new(),
            updated_at: Utc::now(),
        };
        let lines = render_plan_lines(&plan);

        let all_text: String = lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect();

        assert!(
            all_text.contains("No tasks defined"),
            "should show empty message, got: {all_text}"
        );
    }

    // ── T17: plan_task_view_from_plan_task ──

    #[test]
    fn plan_task_view_from_plan_task() {
        let task = make_task("Test task", PlanTaskStatus::Completed);
        let view = PlanTaskView::from_plan_task(&task);

        assert_eq!(view.icon, '\u{2713}');
        assert_eq!(view.icon_color, ratatui::style::Color::Green);
        assert_eq!(view.description, "Test task");
        assert!(!view.dimmed);

        let skipped_task = make_task("Skipped", PlanTaskStatus::Skipped);
        let skipped_view = PlanTaskView::from_plan_task(&skipped_task);
        assert!(skipped_view.dimmed);
        assert_eq!(skipped_view.icon, '\u{2212}');
        assert_eq!(skipped_view.icon_color, ratatui::style::Color::DarkGray);
    }
}

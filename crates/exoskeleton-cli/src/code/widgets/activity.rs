//! Activity indicator widget for `exo code`.
//!
//! Renders a single line above the input area showing what the agent is
//! currently doing. Uses a braille spinner animation at 100ms intervals
//! (driven by `SpinnerTick` messages in the event loop).

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
    widgets::Widget,
};

use crate::code::app::{ActivityState, SPINNER_FRAMES};

/// Widget that displays the current activity state as a single line.
pub struct ActivityWidget<'a> {
    activity: &'a ActivityState,
}

impl<'a> ActivityWidget<'a> {
    /// Create a new activity widget from the current activity state.
    pub fn new(activity: &'a ActivityState) -> Self {
        Self { activity }
    }

    /// Whether this widget should be visible (has content to display).
    pub fn is_visible(activity: &ActivityState) -> bool {
        activity.is_active && !activity.label.is_empty()
    }
}

impl<'a> Widget for ActivityWidget<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.height == 0 {
            return;
        }

        if !self.activity.is_active || self.activity.label.is_empty() {
            return;
        }

        let spinner_char = SPINNER_FRAMES
            .get(self.activity.spinner_phase)
            .copied()
            .unwrap_or(' ');

        let line = Line::from(vec![
            Span::styled(
                format!("  {spinner_char} "),
                Style::default().fg(Color::Cyan),
            ),
            Span::styled(
                self.activity.label.clone(),
                Style::default().fg(Color::DarkGray),
            ),
        ]);

        buf.set_line(area.x, area.y, &line, area.width);
    }
}

#[cfg(test)]
mod tests {
    use ratatui::{buffer::Buffer, layout::Rect, widgets::Widget};

    use crate::code::app::{ActivityState, SPINNER_FRAMES};

    use super::ActivityWidget;

    // ── T18: activity_thinking_shows_spinner ──

    #[test]
    fn activity_thinking_shows_spinner() {
        let activity = ActivityState {
            label: "Thinking...".into(),
            spinner_phase: 0,
            is_active: true,
        };
        let widget = ActivityWidget::new(&activity);
        let area = Rect::new(0, 0, 40, 1);
        let mut buf = Buffer::empty(area);
        widget.render(area, &mut buf);

        let content: String = (0..40).map(|x| buf[(x, 0)].symbol().to_string()).collect();
        let expected_spinner = SPINNER_FRAMES[0].to_string();
        assert!(
            content.contains(&expected_spinner),
            "should contain spinner char, got: {content:?}"
        );
        assert!(
            content.contains("Thinking..."),
            "should contain label, got: {content:?}"
        );
    }

    // ── T19: activity_running_tool_shows_name ──

    #[test]
    fn activity_running_tool_shows_name() {
        let activity = ActivityState {
            label: "Running code.edit...".into(),
            spinner_phase: 3,
            is_active: true,
        };
        let widget = ActivityWidget::new(&activity);
        let area = Rect::new(0, 0, 60, 1);
        let mut buf = Buffer::empty(area);
        widget.render(area, &mut buf);

        let content: String = (0..60).map(|x| buf[(x, 0)].symbol().to_string()).collect();
        assert!(
            content.contains("Running code.edit..."),
            "should contain tool name, got: {content:?}"
        );
    }

    // ── T20: activity_idle_hidden ──

    #[test]
    fn activity_idle_hidden() {
        let activity = ActivityState {
            label: String::new(),
            spinner_phase: 0,
            is_active: false,
        };
        assert!(
            !ActivityWidget::is_visible(&activity),
            "idle activity should not be visible"
        );

        let widget = ActivityWidget::new(&activity);
        let area = Rect::new(0, 0, 40, 1);
        let mut buf = Buffer::empty(area);
        widget.render(area, &mut buf);

        let content: String = (0..40).map(|x| buf[(x, 0)].symbol().to_string()).collect();
        assert_eq!(content.trim(), "", "idle widget should render nothing");
    }

    // ── T21: activity_spinner_frame_cycles ──

    #[test]
    fn activity_spinner_frame_cycles() {
        for (phase, expected_char) in SPINNER_FRAMES.iter().enumerate() {
            let activity = ActivityState {
                label: "test".into(),
                spinner_phase: phase,
                is_active: true,
            };
            let widget = ActivityWidget::new(&activity);
            let area = Rect::new(0, 0, 40, 1);
            let mut buf = Buffer::empty(area);
            widget.render(area, &mut buf);

            let content: String = (0..40).map(|x| buf[(x, 0)].symbol().to_string()).collect();
            assert!(
                content.contains(&expected_char.to_string()),
                "phase {phase} should show char {expected_char}, got: {content:?}"
            );
        }
    }
}

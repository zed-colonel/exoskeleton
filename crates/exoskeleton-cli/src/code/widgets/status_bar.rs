//! Top status bar widget showing vessel name, mode, and budget summary.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Widget,
};

use crate::code::app::{ActivityState, ConnectionStatus, StatusState, SPINNER_FRAMES};

/// Widget for the top status bar.
pub struct StatusBarWidget<'a> {
    status: &'a StatusState,
    connection: &'a ConnectionStatus,
    activity: &'a ActivityState,
}

impl<'a> StatusBarWidget<'a> {
    pub fn new(
        status: &'a StatusState,
        connection: &'a ConnectionStatus,
        activity: &'a ActivityState,
    ) -> Self {
        Self {
            status,
            connection,
            activity,
        }
    }
}

impl<'a> Widget for StatusBarWidget<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.height == 0 {
            return;
        }

        let connection_indicator = match self.connection {
            ConnectionStatus::Connected => Span::styled(
                " CONNECTED ",
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ),
            ConnectionStatus::Connecting => Span::styled(
                " CONNECTING ",
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            ConnectionStatus::Disconnected => Span::styled(
                " DISCONNECTED ",
                Style::default()
                    .fg(Color::White)
                    .bg(Color::Red)
                    .add_modifier(Modifier::BOLD),
            ),
        };

        let vessel_name = if self.status.vessel_name.is_empty() {
            "exo code".to_string()
        } else {
            self.status.vessel_name.clone()
        };

        let mode_color = match self.status.vessel_mode.as_str() {
            "normal" => Color::Cyan,
            "planning" => Color::Magenta,
            "executing" => Color::Green,
            _ => Color::White,
        };

        let mut spans = vec![
            Span::styled(
                " exo code ",
                Style::default()
                    .fg(Color::White)
                    .bg(Color::DarkGray)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" "),
            connection_indicator,
            Span::raw(" "),
            Span::styled(vessel_name, Style::default().fg(Color::White)),
            Span::styled(" | ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                self.status.vessel_mode.clone(),
                Style::default().fg(mode_color),
            ),
        ];

        if !self.status.token_summary.is_empty() {
            spans.push(Span::styled(" | ", Style::default().fg(Color::DarkGray)));
            spans.push(Span::styled(
                self.status.token_summary.clone(),
                Style::default().fg(Color::Yellow),
            ));
        }

        if !self.status.step_summary.is_empty() {
            spans.push(Span::styled(" | ", Style::default().fg(Color::DarkGray)));
            spans.push(Span::styled(
                self.status.step_summary.clone(),
                Style::default().fg(Color::White),
            ));
        }

        let line = Line::from(spans);

        let bar_style = Style::default().bg(Color::DarkGray);
        for x in area.x..area.x + area.width {
            buf[(x, area.y)].set_style(bar_style);
        }

        buf.set_line(area.x, area.y, &line, area.width);

        if area.height > 1 && self.activity.is_active && !self.activity.label.is_empty() {
            let spinner = SPINNER_FRAMES
                .get(self.activity.spinner_phase)
                .copied()
                .unwrap_or(' ');
            let activity_line = Line::from(vec![
                Span::styled(format!(" {spinner} "), Style::default().fg(Color::Cyan)),
                Span::styled(
                    self.activity.label.clone(),
                    Style::default().fg(Color::DarkGray),
                ),
            ]);
            buf.set_line(area.x, area.y + 1, &activity_line, area.width);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_bar_renders_without_panic() {
        let status = StatusState {
            vessel_name: "test-vessel".into(),
            vessel_mode: "normal".into(),
            token_summary: "4.2k tok".into(),
            step_summary: "step 3/25".into(),
        };
        let connection = ConnectionStatus::Connected;
        let activity = ActivityState {
            label: "Thinking...".into(),
            spinner_phase: 0,
            is_active: true,
        };
        let widget = StatusBarWidget::new(&status, &connection, &activity);
        let area = Rect::new(0, 0, 100, 2);
        let mut buf = Buffer::empty(area);
        widget.render(area, &mut buf);

        let content: String = (0..100).map(|x| buf[(x, 0)].symbol().to_string()).collect();
        assert!(content.contains("exo code"));
    }

    #[test]
    fn status_bar_zero_height_no_panic() {
        let status = StatusState::default();
        let connection = ConnectionStatus::Connecting;
        let activity = ActivityState::default();
        let widget = StatusBarWidget::new(&status, &connection, &activity);
        let area = Rect::new(0, 0, 100, 0);
        let mut buf = Buffer::empty(area);
        widget.render(area, &mut buf);
    }
}

//! Text input widget for `exo code`.
//!
//! Wraps tui-textarea to provide a multi-line input area with Enter-to-send
//! and Shift+Enter-for-newline behavior. The textarea widget handles cursor
//! movement, selection, and paste natively.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Widget,
};

/// Widget for rendering the input area.
///
/// In S1, this is a simple text display showing the current input contents.
/// The actual tui-textarea integration is deferred behind the same widget
/// boundary so the input model can expand without changing the layout code.
pub struct InputWidget<'a> {
    text: &'a str,
    is_connected: bool,
}

impl<'a> InputWidget<'a> {
    pub fn new(text: &'a str, is_connected: bool) -> Self {
        Self { text, is_connected }
    }
}

impl<'a> Widget for InputWidget<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.height == 0 {
            return;
        }

        let prompt = if self.is_connected {
            Span::styled(
                " > ",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            Span::styled(" > ", Style::default().fg(Color::DarkGray))
        };

        let content = if self.text.is_empty() {
            Span::styled(
                "Type a message... (Enter to send, Shift+Enter for newline)",
                Style::default().fg(Color::DarkGray),
            )
        } else {
            Span::styled(self.text.to_string(), Style::default().fg(Color::White))
        };

        let line = Line::from(vec![prompt, content]);

        let separator = "─".repeat(area.width as usize);
        if area.height > 1 {
            buf.set_string(
                area.x,
                area.y,
                &separator,
                Style::default().fg(Color::DarkGray),
            );
            buf.set_line(area.x, area.y + 1, &line, area.width);
        } else {
            buf.set_line(area.x, area.y, &line, area.width);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_widget_renders_without_panic() {
        let widget = InputWidget::new("hello world", true);
        let area = Rect::new(0, 0, 80, 2);
        let mut buf = Buffer::empty(area);
        widget.render(area, &mut buf);

        let content: String = (0..80).map(|x| buf[(x, 1)].symbol().to_string()).collect();
        assert!(content.contains("hello world"));
    }

    #[test]
    fn input_widget_empty_shows_placeholder() {
        let widget = InputWidget::new("", true);
        let area = Rect::new(0, 0, 80, 2);
        let mut buf = Buffer::empty(area);
        widget.render(area, &mut buf);

        let content: String = (0..80).map(|x| buf[(x, 1)].symbol().to_string()).collect();
        assert!(content.contains("Type a message"));
    }

    #[test]
    fn input_widget_zero_height_no_panic() {
        let widget = InputWidget::new("test", true);
        let area = Rect::new(0, 0, 80, 0);
        let mut buf = Buffer::empty(area);
        widget.render(area, &mut buf);
    }
}

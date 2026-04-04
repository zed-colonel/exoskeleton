//! Top-level view layout for `exo code`.
//!
//! Divides the terminal into three vertical regions:
//! 1. Status bar (top, 1-2 lines)
//! 2. Conversation area (middle, fills remaining space)
//! 3. Input area (bottom, 2-3 lines)

use ratatui::{
    layout::{Constraint, Direction, Layout},
    Frame,
};

use super::app::App;
use super::widgets::conversation::{estimate_rendered_height, ConversationWidget};
use super::widgets::input::InputWidget;
use super::widgets::status_bar::StatusBarWidget;

/// Render the complete TUI layout to a frame.
pub fn view(app: &mut App, frame: &mut Frame) {
    let area = frame.area();

    let status_height = if app.activity.is_active { 2 } else { 1 };
    let input_height: u16 = 2;

    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(status_height),
            Constraint::Min(3),
            Constraint::Length(input_height),
        ])
        .split(area);

    let status_area = layout[0];
    let conversation_area = layout[1];
    let input_area = layout[2];

    let rendered_height = estimate_rendered_height(&app.conversation, conversation_area.width);
    app.conversation
        .set_rendered_dimensions(rendered_height, conversation_area.height);

    let status_bar = StatusBarWidget::new(&app.status, &app.connection, &app.activity);
    frame.render_widget(status_bar, status_area);

    let conversation = ConversationWidget::new(&app.conversation);
    frame.render_widget(conversation, conversation_area);

    let is_connected = app.connection == super::app::ConnectionStatus::Connected;
    let input = InputWidget::new(&app.input_text, is_connected);
    frame.render_widget(input, input_area);
}

#[cfg(test)]
mod tests {
    use ratatui::{backend::TestBackend, Terminal};

    use crate::code::app::App;

    use super::view;

    #[test]
    fn view_renders_without_panic() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = App::new();
        app.status.vessel_name = "test-vessel".into();

        terminal
            .draw(|frame| {
                view(&mut app, frame);
            })
            .unwrap();

        let buffer = terminal.backend().buffer().clone();
        let content: String = (0..80)
            .map(|x| buffer[(x, 0)].symbol().to_string())
            .collect();
        assert!(content.contains("exo code"));
    }

    #[test]
    fn view_renders_with_blocks() {
        use crate::code::widgets::conversation::{Block, NoteSeverity};

        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = App::new();
        app.conversation.add_block(Block::SystemNote {
            text: "Connected to vessel".into(),
            severity: NoteSeverity::Info,
        });
        app.conversation.add_block(Block::UserMessage {
            text: "Add a test".into(),
            timestamp: chrono::Utc::now(),
        });

        terminal
            .draw(|frame| {
                view(&mut app, frame);
            })
            .unwrap();
    }
}

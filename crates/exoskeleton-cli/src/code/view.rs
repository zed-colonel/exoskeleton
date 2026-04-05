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

use super::app::{App, UiMode};
use super::widgets::activity::ActivityWidget;
use super::widgets::approval::ApprovalWidget;
use super::widgets::conversation::{estimate_rendered_height, ConversationWidget};
use super::widgets::debug_banner::DebugBannerWidget;
use super::widgets::input::InputWidget;
use super::widgets::status_bar::StatusBarWidget;

/// Render the complete TUI layout to a frame.
pub fn view(app: &mut App, frame: &mut Frame) {
    let area = frame.area();

    let status_height: u16 = 1;
    let debug_height: u16 = DebugBannerWidget::height(&app.debug, area.width);
    let activity_height: u16 = if ActivityWidget::is_visible(&app.activity) {
        1
    } else {
        0
    };

    let bottom_height: u16 = match &app.mode {
        UiMode::Normal => 2,
        UiMode::Approval(ctx) => {
            let base = 6 + ctx.options.len() as u16;
            let plan_lines = ctx
                .plan_content
                .as_ref()
                .map(|c| c.lines().count().min(10) as u16)
                .unwrap_or(0);
            (base + plan_lines).min(
                area.height.saturating_sub(
                    status_height
                        .saturating_add(debug_height)
                        .saturating_add(activity_height)
                        .saturating_add(3),
                ),
            )
        }
    };

    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(status_height),
            Constraint::Length(debug_height),
            Constraint::Min(3),
            Constraint::Length(activity_height),
            Constraint::Length(bottom_height),
        ])
        .split(area);

    let status_area = layout[0];
    let debug_area = layout[1];
    let conversation_area = layout[2];
    let activity_area = layout[3];
    let bottom_area = layout[4];

    let rendered_height = estimate_rendered_height(
        &app.conversation,
        conversation_area.width,
        app.debug.visible,
    );
    app.conversation
        .set_rendered_dimensions(rendered_height, conversation_area.height);

    let status_bar = StatusBarWidget::new(&app.status, &app.connection, &app.activity);
    frame.render_widget(status_bar, status_area);

    if debug_height > 0 {
        let debug_banner = DebugBannerWidget::new(&app.debug, area.width);
        frame.render_widget(debug_banner, debug_area);
    }

    let conversation =
        ConversationWidget::new(&app.conversation).with_debug_mode(app.debug.visible);
    frame.render_widget(conversation, conversation_area);

    if activity_height > 0 {
        let activity = ActivityWidget::new(&app.activity);
        frame.render_widget(activity, activity_area);
    }

    match &app.mode {
        UiMode::Normal => {
            let is_connected = app.connection == super::app::ConnectionStatus::Connected;
            let input = InputWidget::new(&app.input_text, is_connected);
            frame.render_widget(input, bottom_area);
        }
        UiMode::Approval(context) => {
            if let Some(ref state) = app.approval {
                let approval = ApprovalWidget::new(context, state);
                frame.render_widget(approval, bottom_area);
            }
        }
    }
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

    #[test]
    fn view_renders_with_approval_overlay() {
        use crate::code::app::UiMode;
        use crate::code::widgets::approval::{
            tool_approval_options, ApprovalContext, ApprovalKind, ApprovalState,
        };

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = App::new();
        app.mode = UiMode::Approval(ApprovalContext {
            question_id: "q-1".into(),
            title: "Tool requires approval".into(),
            description: "shell.exec cargo test".into(),
            options: tool_approval_options("shell.exec"),
            kind: ApprovalKind::ToolApproval,
            plan_content: None,
            plan_draft_id: None,
        });
        app.approval = Some(ApprovalState::new());

        terminal
            .draw(|frame| {
                view(&mut app, frame);
            })
            .unwrap();
    }

    #[test]
    fn view_renders_with_activity_indicator() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = App::new();
        app.activity.is_active = true;
        app.activity.label = "Thinking...".into();
        app.activity.spinner_phase = 2;

        terminal
            .draw(|frame| {
                view(&mut app, frame);
            })
            .unwrap();
    }

    #[test]
    fn view_renders_with_debug_banner() {
        let backend = TestBackend::new(120, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = App::new();
        app.debug.visible = true;
        app.debug.tick_number = 42;
        app.debug.step_count = "3/25 steps".into();
        app.debug.token_count = "4,231/50,000 tok".into();

        terminal
            .draw(|frame| {
                view(&mut app, frame);
            })
            .unwrap();

        let buffer = terminal.backend().buffer().clone();
        let debug_line: String = (0..120)
            .map(|x| buffer[(x, 1)].symbol().to_string())
            .collect();
        assert!(
            debug_line.contains("TICK #42"),
            "debug banner should show tick, got: {debug_line}"
        );
    }

    #[test]
    fn view_debug_banner_hidden_when_toggled_off() {
        let backend = TestBackend::new(120, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = App::new();
        app.debug.visible = false;
        app.debug.tick_number = 42;

        terminal
            .draw(|frame| {
                view(&mut app, frame);
            })
            .unwrap();

        let buffer = terminal.backend().buffer().clone();
        let line1: String = (0..120)
            .map(|x| buffer[(x, 1)].symbol().to_string())
            .collect();
        assert!(
            !line1.contains("TICK #42"),
            "debug banner should be hidden, but line 1 got: {line1}"
        );
    }
}

//! Message conversion from raw events to TUI Messages.
//!
//! Converts crossterm key events, WebSocket LiveEvents, and timer ticks into
//! the unified `Message` enum consumed by `update()`.

use crossterm::event::Event as CrosstermEvent;
use exoskeleton_core::LiveEvent;

use super::app::Message;

/// Convert a crossterm event into a TUI Message.
pub fn from_crossterm_event(event: CrosstermEvent) -> Option<Message> {
    match event {
        CrosstermEvent::Key(key_event) => Some(Message::Key(key_event)),
        CrosstermEvent::Paste(text) => Some(Message::Paste(text)),
        CrosstermEvent::Resize(w, h) => Some(Message::Resize(w, h)),
        _ => None,
    }
}

/// Convert a WebSocket text message into a TUI Message.
///
/// Attempts to parse the text as a `LiveEvent`. If parsing fails, the
/// message is silently dropped (the daemon may send non-LiveEvent JSON
/// such as broadcast lag warnings).
pub fn from_ws_text(text: &str) -> Option<Message> {
    match serde_json::from_str::<LiveEvent>(text) {
        Ok(event) => Some(Message::WsEvent(event)),
        Err(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use exoskeleton_core::EventType;

    use super::*;

    // ── TUI-T7: live_event_to_message_inner_loop_step ──

    #[test]
    fn live_event_to_message_inner_loop_step() {
        let event = LiveEvent {
            event_type: EventType::InnerLoopStep,
            summary: "Step 1/5: code.read (success)".into(),
            inner_loop_detail: Some(exoskeleton_core::InnerLoopStepDetail {
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
        let json = serde_json::to_string(&event).unwrap();
        let msg = from_ws_text(&json);
        assert!(msg.is_some());
        match msg.unwrap() {
            Message::WsEvent(e) => {
                assert_eq!(e.event_type, EventType::InnerLoopStep);
                assert!(e.inner_loop_detail.is_some());
            }
            other => panic!("expected WsEvent, got {other:?}"),
        }
    }

    // ── TUI-T8: live_event_to_message_unknown_type_still_wraps ──

    #[test]
    fn live_event_to_message_unknown_type_still_wraps() {
        let event = LiveEvent {
            event_type: EventType::VesselForked,
            summary: "Vessel forked".into(),
            ..LiveEvent::new(None)
        };
        let json = serde_json::to_string(&event).unwrap();
        let msg = from_ws_text(&json);
        assert!(msg.is_some());
        match msg.unwrap() {
            Message::WsEvent(e) => {
                assert_eq!(e.event_type, EventType::VesselForked);
            }
            other => panic!("expected WsEvent, got {other:?}"),
        }
    }

    #[test]
    fn from_crossterm_key_event_converts() {
        let key = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        let ct_event = CrosstermEvent::Key(key);
        let msg = from_crossterm_event(ct_event);
        assert!(matches!(msg, Some(Message::Key(_))));
    }

    #[test]
    fn from_crossterm_resize_converts() {
        let ct_event = CrosstermEvent::Resize(120, 40);
        let msg = from_crossterm_event(ct_event);
        assert!(matches!(msg, Some(Message::Resize(120, 40))));
    }

    #[test]
    fn from_crossterm_paste_converts() {
        let ct_event = CrosstermEvent::Paste("hello world".into());
        let msg = from_crossterm_event(ct_event);
        match msg {
            Some(Message::Paste(text)) => assert_eq!(text, "hello world"),
            other => panic!("expected Paste, got {other:?}"),
        }
    }

    #[test]
    fn from_ws_text_invalid_json_returns_none() {
        assert!(from_ws_text("not json").is_none());
    }

    #[test]
    fn from_ws_text_non_live_event_json_returns_none() {
        assert!(from_ws_text(r#"{"type":"broadcast_lag","behind":5}"#).is_none());
    }
}

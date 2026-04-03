//! `exo code` — live coding session client (E9-S2, W-78).
//!
//! Connects to a vessel's WebSocket, sends a coding task, and streams
//! inner-loop events in real time with ANSI-colored terminal output.

use std::io::Write;

use crossterm::execute;
use crossterm::style::{Color, Print, ResetColor, SetForegroundColor};
use exoskeleton_core::{derive_external_principal_id, DiffSummary, EventType, LiveEvent};
use futures_util::StreamExt;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;

use crate::client::CliError;

/// Run a live coding session against a vessel.
pub async fn run_code_session(daemon_addr: &str, task: &str) -> Result<(), CliError> {
    let base_url = normalize_base_url(daemon_addr);
    let ws_url = websocket_url(&base_url);
    let http_client = reqwest::Client::new();
    let mut stdout = std::io::stdout();

    let status_url = format!("{base_url}/api/v1/status");
    let status: serde_json::Value = http_client
        .get(&status_url)
        .send()
        .await
        .map_err(|e| CliError::Connection(e.to_string()))?
        .json()
        .await
        .map_err(|e| CliError::Other(format!("failed to parse status response: {e}")))?;

    execute!(
        stdout,
        SetForegroundColor(Color::DarkCyan),
        Print(format!(
            "Connected to vessel {}\n",
            status
                .get("vessel_id")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
        )),
        ResetColor,
    )
    .map_err(|e| CliError::Other(e.to_string()))?;

    let (ws_stream, _) = connect_async(&ws_url)
        .await
        .map_err(|e| CliError::Connection(e.to_string()))?;
    let (_, mut read) = ws_stream.split();

    let inbox_url = format!("{base_url}/api/v1/inbox");
    let body = serde_json::json!({
        "content": task,
        "source": derive_external_principal_id("exo-code-cli"),
    });
    let response = http_client
        .post(&inbox_url)
        .json(&body)
        .send()
        .await
        .map_err(|e| CliError::Connection(e.to_string()))?;
    if !response.status().is_success() {
        let status = response.status().as_u16();
        let body = response
            .text()
            .await
            .unwrap_or_else(|_| String::from("<unreadable>"));
        return Err(CliError::DaemonError { status, body });
    }

    execute!(
        stdout,
        SetForegroundColor(Color::White),
        Print(format!("Task submitted: {task}\n")),
        ResetColor,
        Print(format!("{}\n", "─".repeat(60))),
    )
    .map_err(|e| CliError::Other(e.to_string()))?;

    let mut exit_after_tick = false;

    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                execute!(
                    stdout,
                    SetForegroundColor(Color::DarkYellow),
                    Print("\nInterrupted.\n"),
                    ResetColor,
                ).map_err(|e| CliError::Other(e.to_string()))?;
                break;
            }
            message = read.next() => {
                match message {
                    Some(Ok(Message::Text(text))) => {
                        if let Ok(event) = serde_json::from_str::<LiveEvent>(text.as_ref()) {
                            render_event(&mut stdout, &event)
                                .map_err(|e| CliError::Other(e.to_string()))?;

                            if event.event_type == EventType::InnerLoopCompleted {
                                let reason = event
                                    .inner_loop_detail
                                    .as_ref()
                                    .and_then(|detail| detail.completion_reason.as_deref());
                                if matches!(reason, Some("agent_complete") | Some("cancelled")) {
                                    exit_after_tick = true;
                                }
                            }

                            if exit_after_tick && event.event_type == EventType::TickCompleted {
                                break;
                            }
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => {
                        execute!(
                            stdout,
                            SetForegroundColor(Color::DarkYellow),
                            Print("\nConnection closed.\n"),
                            ResetColor,
                        ).map_err(|e| CliError::Other(e.to_string()))?;
                        break;
                    }
                    Some(Err(e)) => {
                        return Err(CliError::Other(format!("websocket error: {e}")));
                    }
                    _ => {}
                }
            }
        }
    }

    Ok(())
}

fn normalize_base_url(daemon_addr: &str) -> String {
    if daemon_addr.starts_with("http://") || daemon_addr.starts_with("https://") {
        daemon_addr.trim_end_matches('/').to_string()
    } else {
        format!("http://{}", daemon_addr.trim_end_matches('/'))
    }
}

fn websocket_url(base_url: &str) -> String {
    if let Some(rest) = base_url.strip_prefix("https://") {
        format!("wss://{rest}/api/v1/ws")
    } else if let Some(rest) = base_url.strip_prefix("http://") {
        format!("ws://{rest}/api/v1/ws")
    } else {
        format!("ws://{}/api/v1/ws", base_url.trim_end_matches('/'))
    }
}

/// Render a single LiveEvent to a writer.
fn render_event(out: &mut impl Write, event: &LiveEvent) -> Result<(), Box<dyn std::error::Error>> {
    match event.event_type {
        EventType::TickStarted => {
            let tick = event.tick_number.unwrap_or(0);
            execute!(
                out,
                SetForegroundColor(Color::DarkGrey),
                Print(format!("── Tick {tick} {}\n", "─".repeat(50))),
                ResetColor,
            )?;
        }
        EventType::InnerLoopStarted => {
            execute!(
                out,
                SetForegroundColor(Color::Blue),
                Print(format!("▶ {}\n", event.summary)),
                ResetColor,
            )?;
        }
        EventType::InnerLoopStep => {
            if let Some(ref detail) = event.inner_loop_detail {
                let tool_name = detail.tool_name.as_deref().unwrap_or("?");
                let outcome = detail.tool_outcome.as_deref().unwrap_or("?");
                let color = if outcome == "success" {
                    Color::Green
                } else {
                    Color::Red
                };
                execute!(
                    out,
                    SetForegroundColor(Color::DarkGrey),
                    Print(format!("  [{}/{}] ", detail.step_number, detail.max_steps)),
                    SetForegroundColor(color),
                    Print(tool_name),
                    SetForegroundColor(Color::DarkGrey),
                    Print(format!(" ({outcome}) [{} tokens]\n", detail.tokens_total)),
                    ResetColor,
                )?;
            } else {
                execute!(
                    out,
                    SetForegroundColor(Color::DarkGrey),
                    Print(format!("  {}\n", event.summary)),
                    ResetColor,
                )?;
            }
        }
        EventType::InnerLoopCompleted => {
            if let Some(ref detail) = event.inner_loop_detail {
                execute!(
                    out,
                    SetForegroundColor(Color::Blue),
                    Print(format!(
                        "■ Completed: {} ({} steps, {} tokens)\n",
                        detail.completion_reason.as_deref().unwrap_or("unknown"),
                        detail.step_number,
                        detail.tokens_total,
                    )),
                    ResetColor,
                )?;
            }
            if let Some(ref diff) = event.diff_summary {
                render_diff_summary(out, diff)?;
            }
        }
        EventType::ActionExecuted => {
            execute!(
                out,
                SetForegroundColor(Color::DarkGrey),
                Print(format!("  → {}\n", truncate_summary(&event.summary, 100))),
                ResetColor,
            )?;
        }
        EventType::QuestionAsked => {
            if let Some(ref detail) = event.question_detail {
                execute!(
                    out,
                    SetForegroundColor(Color::Yellow),
                    Print(format!("\n❓ {}\n", detail.question)),
                    ResetColor,
                )?;
                if let Some(ref choices) = detail.choices {
                    for (index, choice) in choices.iter().enumerate() {
                        execute!(
                            out,
                            SetForegroundColor(Color::Yellow),
                            Print(format!("  {}. {}\n", index + 1, choice)),
                            ResetColor,
                        )?;
                    }
                }
                execute!(
                    out,
                    SetForegroundColor(Color::DarkYellow),
                    Print("  (Interactive input available in E9-S3)\n"),
                    ResetColor,
                )?;
            }
        }
        EventType::PlanModeTransition => {
            if let Some(ref detail) = event.plan_mode_detail {
                execute!(
                    out,
                    SetForegroundColor(Color::Magenta),
                    Print(format!("⚙ Mode: {} → {}\n", detail.from, detail.to)),
                    ResetColor,
                )?;
            }
        }
        EventType::PolicyApprovalRequired => {
            if let Some(ref detail) = event.policy_detail {
                execute!(
                    out,
                    SetForegroundColor(Color::DarkYellow),
                    Print(format!(
                        "🔒 Policy: {} requires approval ({})\n",
                        detail.tool_name, detail.rule
                    )),
                    ResetColor,
                )?;
            }
        }
        EventType::TickCompleted => {
            execute!(
                out,
                SetForegroundColor(Color::DarkGrey),
                Print(format!("── {}\n", event.summary)),
                ResetColor,
            )?;
        }
        _ => {}
    }

    out.flush()?;
    Ok(())
}

fn render_diff_summary(
    out: &mut impl Write,
    diff: &DiffSummary,
) -> Result<(), Box<dyn std::error::Error>> {
    execute!(
        out,
        SetForegroundColor(Color::Cyan),
        Print(format!("  📁 {} file(s) changed: ", diff.files_modified)),
        SetForegroundColor(Color::Green),
        Print(format!("+{}", diff.lines_added)),
        SetForegroundColor(Color::DarkGrey),
        Print("/"),
        SetForegroundColor(Color::Red),
        Print(format!("-{}", diff.lines_removed)),
        SetForegroundColor(Color::DarkGrey),
        Print(format!(" (net: {})\n", diff.net_delta)),
        ResetColor,
    )?;
    Ok(())
}

fn truncate_summary(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        format!("{}...", &s[..max_len.saturating_sub(3)])
    }
}

#[cfg(test)]
mod tests {
    use exoskeleton_core::{
        EventType, InnerLoopStepDetail, LiveEvent, PlanModeDetail, QuestionDetail,
    };

    use super::*;

    #[test]
    fn truncate_summary_short_string() {
        assert_eq!(truncate_summary("short", 10), "short");
    }

    #[test]
    fn truncate_summary_long_string() {
        assert_eq!(truncate_summary("abcdefghij", 7), "abcd...");
    }

    #[test]
    fn render_event_inner_loop_started() {
        let mut out = Vec::new();
        let event = LiveEvent {
            event_type: EventType::InnerLoopStarted,
            summary: "Inner loop started".into(),
            ..LiveEvent::new(Some(7))
        };

        render_event(&mut out, &event).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("▶ Inner loop started"));
    }

    #[test]
    fn render_event_inner_loop_step_success() {
        let mut out = Vec::new();
        let event = LiveEvent {
            event_type: EventType::InnerLoopStep,
            inner_loop_detail: Some(InnerLoopStepDetail {
                step_number: 1,
                max_steps: 5,
                tool_name: Some("code.read".into()),
                tool_outcome: Some("success".into()),
                tokens_this_step: 12,
                tokens_total: 30,
                completion_reason: None,
            }),
            ..LiveEvent::new(Some(7))
        };

        render_event(&mut out, &event).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("[1/5]"));
        assert!(text.contains("code.read"));
        assert!(text.contains("success"));
        assert!(text.contains("\u{1b}["));
    }

    #[test]
    fn render_event_inner_loop_completed_with_diff() {
        let mut out = Vec::new();
        let event = LiveEvent {
            event_type: EventType::InnerLoopCompleted,
            inner_loop_detail: Some(InnerLoopStepDetail {
                step_number: 3,
                max_steps: 5,
                tool_name: None,
                tool_outcome: None,
                tokens_this_step: 0,
                tokens_total: 88,
                completion_reason: Some("agent_complete".into()),
            }),
            diff_summary: Some(DiffSummary {
                files_modified: 2,
                lines_added: 7,
                lines_removed: 3,
                net_delta: 4,
                files: vec![],
            }),
            ..LiveEvent::new(Some(7))
        };

        render_event(&mut out, &event).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("Completed: agent_complete"));
        assert!(text.contains("2 file(s) changed"));
        assert!(text.contains("+7"));
        assert!(text.contains("-3"));
    }

    #[test]
    fn render_event_tick_boundaries() {
        let mut out = Vec::new();
        let started = LiveEvent {
            event_type: EventType::TickStarted,
            ..LiveEvent::new(Some(7))
        };
        let completed = LiveEvent {
            event_type: EventType::TickCompleted,
            summary: "Tick 7 completed".into(),
            ..LiveEvent::new(Some(7))
        };

        render_event(&mut out, &started).unwrap();
        render_event(&mut out, &completed).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("Tick 7"));
        assert!(text.contains("Tick 7 completed"));
    }

    #[test]
    fn render_event_question_asked() {
        let mut out = Vec::new();
        let event = LiveEvent {
            event_type: EventType::QuestionAsked,
            question_detail: Some(QuestionDetail {
                question_id: "q1".into(),
                question: "Choose a path".into(),
                choices: Some(vec!["A".into(), "B".into()]),
                status: "pending".into(),
            }),
            ..LiveEvent::new(Some(7))
        };

        render_event(&mut out, &event).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("Choose a path"));
        assert!(text.contains("1. A"));
        assert!(text.contains("2. B"));
    }

    #[test]
    fn render_event_mode_transition() {
        let mut out = Vec::new();
        let event = LiveEvent {
            event_type: EventType::PlanModeTransition,
            plan_mode_detail: Some(PlanModeDetail {
                from: "planning".into(),
                to: "executing".into(),
                plan_draft_id: None,
            }),
            ..LiveEvent::new(Some(7))
        };

        render_event(&mut out, &event).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("planning"));
        assert!(text.contains("executing"));
    }
}

//! `exo code` — fullscreen TUI coding session client (TUI-S1).
//!
//! Replaces the E9-S2 foundation client with a ratatui-based TUI using the
//! Elm architecture: pure `update()` function driven by a `tokio::select!`
//! event loop.

pub mod app;
pub mod connection;
pub mod messages;
pub mod render;
pub mod view;
pub mod widgets;

use std::io;
use std::time::Duration;

use crossterm::{
    event::{DisableMouseCapture, EnableMouseCapture, EventStream},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use futures_util::{stream::SplitStream, StreamExt};
use ratatui::{backend::CrosstermBackend, Terminal};
use tokio::net::TcpStream;
use tokio_tungstenite::{
    connect_async,
    tungstenite::{Error as WsError, Message as WsMessage},
    MaybeTlsStream, WebSocketStream,
};
use tracing::warn;

use crate::client::{CliError, DaemonClient};

use self::app::{App, Message, SideEffect};
use self::connection::{
    cli_principal_id, fetch_vessel_status, load_conversation_history, normalize_base_url,
    websocket_url,
};
use self::messages::{from_crossterm_event, from_ws_text};

type WsRead = SplitStream<WebSocketStream<MaybeTlsStream<TcpStream>>>;
type WsReadItem = Option<Result<WsMessage, WsError>>;

/// Run a live coding session with a fullscreen TUI.
///
/// This is the main entry point called by `main.rs` when the user runs
/// `exo code <task>`. It sets up the terminal, connects to the vessel,
/// and runs the Elm event loop until the user quits.
pub async fn run_code_session(daemon_addr: &str, task: &str) -> Result<(), CliError> {
    let base_url = normalize_base_url(daemon_addr);
    let client = DaemonClient::new(&base_url);
    let mut app = App::new();

    let _vessel_id = fetch_vessel_status(&client, &mut app).await.map_err(|e| {
        CliError::Connection(format!(
            "cannot connect to vessel at {}: {e}",
            client.base_url()
        ))
    })?;

    if let Err(e) = load_conversation_history(&client, &mut app, 50).await {
        warn!("failed to load conversation history: {e}");
    }

    enable_raw_mode().map_err(|e| CliError::Other(format!("failed to enable raw mode: {e}")))?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)
        .map_err(|e| CliError::Other(format!("failed to enter alternate screen: {e}")))?;

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)
        .map_err(|e| CliError::Other(format!("failed to create terminal: {e}")))?;

    let session_result: Result<(), CliError> = async {
        let ws_url = websocket_url(client.base_url());
        let ws_result = connect_async(&ws_url).await;
        let mut ws_read: Option<WsRead> = match ws_result {
            Ok((ws_stream, _)) => {
                let (_, read) = ws_stream.split();
                Some(read)
            }
            Err(e) => {
                warn!("websocket connection failed: {e}");
                app.connection = app::ConnectionStatus::Disconnected;
                None
            }
        };

        if !task.is_empty() {
            let principal = cli_principal_id();
            match client.send_message(&principal, task).await {
                Ok(_) => {
                    app.conversation
                        .add_block(widgets::conversation::Block::UserMessage {
                            text: task.to_string(),
                            timestamp: chrono::Utc::now(),
                        });
                }
                Err(e) => {
                    app.conversation
                        .add_block(widgets::conversation::Block::SystemNote {
                            text: format!("Failed to submit task: {e}"),
                            severity: widgets::conversation::NoteSeverity::Warning,
                        });
                }
            }
        }

        let mut crossterm_events = EventStream::new();
        let mut spinner_interval = tokio::time::interval(Duration::from_millis(100));
        spinner_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        terminal
            .draw(|frame| view::view(&mut app, frame))
            .map_err(|e| CliError::Other(format!("render error: {e}")))?;

        loop {
            let msg: Option<Message> = tokio::select! {
                event = crossterm_events.next() => {
                    match event {
                        Some(Ok(ct_event)) => from_crossterm_event(ct_event),
                        Some(Err(_)) => None,
                        None => Some(Message::WsDisconnected),
                    }
                }
                ws_msg = async {
                    match ws_read.as_mut() {
                        Some(reader) => reader.next().await,
                        None => std::future::pending::<WsReadItem>().await,
                    }
                } => {
                    match ws_msg {
                        Some(Ok(WsMessage::Text(text))) => from_ws_text(text.as_ref()),
                        Some(Ok(WsMessage::Close(_))) | None => Some(Message::WsDisconnected),
                        Some(Err(e)) => {
                            warn!("websocket error: {e}");
                            Some(Message::WsDisconnected)
                        }
                        _ => None,
                    }
                }
                _ = spinner_interval.tick() => {
                    Some(Message::SpinnerTick)
                }
            };

            if let Some(msg) = msg {
                let effects = app::update(&mut app, msg);

                for effect in effects {
                    match effect {
                        SideEffect::SendMessage(text) => {
                            match connection::send_message(&client, &text).await {
                                Ok(()) => {
                                    let _ = app::update(&mut app, Message::MessageSent(Ok(())));
                                }
                                Err(e) => {
                                    let _ = app::update(
                                        &mut app,
                                        Message::MessageSent(Err(e.to_string())),
                                    );
                                }
                            }
                        }
                        SideEffect::Reconnect => match connect_async(&ws_url).await {
                            Ok((ws_stream, _)) => {
                                let (_, read) = ws_stream.split();
                                ws_read = Some(read);
                                let _ = app::update(&mut app, Message::WsReconnected);
                            }
                            Err(e) => {
                                warn!("reconnection failed: {e}");
                            }
                        },
                        SideEffect::FetchArtifact(artifact_id) => {
                            let fetch_result = client.artifact(&artifact_id).await;
                            let msg_result = match fetch_result {
                                Ok(Some(value)) => Ok(value),
                                Ok(None) => Err("artifact not found".into()),
                                Err(e) => Err(e.to_string()),
                            };
                            let _ = app::update(
                                &mut app,
                                Message::ArtifactFetched {
                                    artifact_id,
                                    result: msg_result,
                                },
                            );
                        }
                        SideEffect::SubmitAnswer {
                            question_id,
                            answer,
                        } => {
                            let source = connection::cli_principal_id();
                            if let Err(e) =
                                client.answer_question(&question_id, &answer, &source).await
                            {
                                let _ = app::update(
                                    &mut app,
                                    Message::MessageSent(Err(format!(
                                        "Failed to submit answer: {e}"
                                    ))),
                                );
                            }
                        }
                        SideEffect::ApprovePlan { plan_draft_id } => {
                            if let Err(e) = client.approve_plan(&plan_draft_id).await {
                                let _ = app::update(
                                    &mut app,
                                    Message::MessageSent(Err(format!(
                                        "Failed to approve plan: {e}"
                                    ))),
                                );
                            }
                        }
                        SideEffect::DenyPlan => {
                            if let Err(e) = client.cancel_plan().await {
                                let _ = app::update(
                                    &mut app,
                                    Message::MessageSent(Err(format!(
                                        "Failed to cancel plan: {e}"
                                    ))),
                                );
                            }
                        }
                        SideEffect::FetchPlanDraft(draft_id) => {
                            let fetch_result = client.artifact(&draft_id).await;
                            let content_result = match fetch_result {
                                Ok(Some(value)) => Ok(value
                                    .get("content")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or_default()
                                    .to_string()),
                                Ok(None) => Err("plan draft not found".into()),
                                Err(e) => Err(e.to_string()),
                            };
                            let _ = app::update(
                                &mut app,
                                Message::PlanContentFetched {
                                    plan_draft_id: draft_id,
                                    result: content_result,
                                },
                            );
                        }
                        SideEffect::FetchPlanStatus => {
                            let status_result = match client.plan_status().await {
                                Ok(Some(value)) => Ok(value
                                    .get("plan_draft_id")
                                    .and_then(|v| v.as_str())
                                    .map(|s| s.to_string())),
                                Ok(None) => Ok(None),
                                Err(e) => Err(e.to_string()),
                            };
                            let _ =
                                app::update(&mut app, Message::PlanStatusFetched(status_result));
                        }
                        SideEffect::Quit => {
                            break;
                        }
                    }
                }
            }

            if app.should_quit {
                break;
            }

            terminal
                .draw(|frame| view::view(&mut app, frame))
                .map_err(|e| CliError::Other(format!("render error: {e}")))?;
        }

        Ok(())
    }
    .await;

    let restore_result = (|| -> Result<(), CliError> {
        disable_raw_mode()
            .map_err(|e| CliError::Other(format!("failed to disable raw mode: {e}")))?;
        execute!(
            terminal.backend_mut(),
            LeaveAlternateScreen,
            DisableMouseCapture,
        )
        .map_err(|e| CliError::Other(format!("failed to leave alternate screen: {e}")))?;
        terminal
            .show_cursor()
            .map_err(|e| CliError::Other(format!("failed to show cursor: {e}")))?;
        Ok(())
    })();

    match (session_result, restore_result) {
        (Err(err), _) => Err(err),
        (Ok(()), Err(err)) => Err(err),
        (Ok(()), Ok(())) => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::connection::{normalize_base_url, websocket_url};

    #[test]
    fn normalize_and_ws_url_round_trip() {
        let base = normalize_base_url("localhost:7600/");
        assert_eq!(base, "http://localhost:7600");
        let ws = websocket_url(&base);
        assert_eq!(ws, "ws://localhost:7600/api/v1/ws");
    }

    #[test]
    fn normalize_and_ws_url_https() {
        let base = normalize_base_url("https://vessel.example.com:8443");
        assert_eq!(base, "https://vessel.example.com:8443");
        let ws = websocket_url(&base);
        assert_eq!(ws, "wss://vessel.example.com:8443/api/v1/ws");
    }
}

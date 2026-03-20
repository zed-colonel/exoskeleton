//! Pre-bootstrap mode HTTP server.
//!
//! Serves only bootstrap + operational endpoints. Returns a `DaemonConfig`
//! when start-vessel is called (signaling transition to full mode),
//! or `None` if the server was shut down without completing bootstrap.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use axum::routing::{get, post};
use exoskeleton_core::ExoError;
use tokio::sync::oneshot;

use crate::bootstrap_api;
use crate::bootstrap_state::BootstrapState;
use crate::DaemonConfig;

/// Run the daemon in pre-bootstrap mode.
///
/// Serves only bootstrap + operational endpoints. Returns a `DaemonConfig`
/// when start-vessel is called (signaling transition to full mode),
/// or `None` if the server was shut down without completing bootstrap.
pub async fn run_bootstrap_server(
    data_dir: PathBuf,
    listen_addr: SocketAddr,
) -> Result<Option<DaemonConfig>, ExoError> {
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let state = Arc::new(BootstrapState::new(data_dir, listen_addr, shutdown_tx));

    let router = axum::Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/ready", get(bootstrap_api::ready))
        .route(
            "/api/v1/bootstrap/configure",
            post(bootstrap_api::configure),
        )
        .route("/api/v1/bootstrap/verify", post(bootstrap_api::verify))
        .route(
            "/api/v1/bootstrap/conversation",
            get(bootstrap_api::conversation_ws),
        )
        .route("/api/v1/bootstrap/finalize", post(bootstrap_api::finalize))
        .route(
            "/api/v1/bootstrap/start-vessel",
            post(bootstrap_api::start_vessel),
        )
        .with_state(state.clone());

    let listener = tokio::net::TcpListener::bind(listen_addr)
        .await
        .map_err(|e| ExoError::Config(format!("failed to bind {listen_addr}: {e}")))?;

    tracing::info!(%listen_addr, "daemon listening (pre-bootstrap mode)");

    axum::serve(listener, router)
        .with_graceful_shutdown(async {
            shutdown_rx.await.ok();
        })
        .await
        .map_err(|e| ExoError::Config(format!("HTTP server error: {e}")))?;

    // After shutdown, check if transition to full mode was requested
    Ok(state.take_vessel_config())
}

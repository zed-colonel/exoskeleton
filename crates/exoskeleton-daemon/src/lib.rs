//! HTTP daemon for remote inspection of a running Exoskeleton vessel.
//!
//! The daemon exposes read-only REST endpoints for inspecting vessel state
//! (ticks, threads, relationships, budget, events, capabilities, engine status)
//! plus a single write endpoint for inbox message submission.
//!
//! Operational endpoints: `/healthz`, `/ready`, `/metrics` (Prometheus).

pub mod handlers;
pub mod routes;
pub mod state;

use std::net::SocketAddr;
use std::sync::Arc;

use exoskeleton_core::ExoError;
use exoskeleton_host::config::VesselConfig;
use exoskeleton_host::metrics::ExoMetrics;
use exoskeleton_host::vessel::Vessel;
pub use state::AppState;

/// Configuration for the HTTP daemon.
#[derive(Debug, Clone)]
pub struct DaemonConfig {
    /// Vessel configuration (boot parameters for the underlying runtime).
    pub vessel: VesselConfig,
    /// Socket address to bind the HTTP server to.
    pub listen_addr: SocketAddr,
}

/// The Exoskeleton HTTP daemon.
///
/// Wraps a running [`Vessel`] and serves an HTTP API for inspection and
/// inbox submission. Created via [`ExoDaemon::start`], then run with
/// [`ExoDaemon::run_until_shutdown`].
pub struct ExoDaemon {
    vessel: Vessel,
    listen_addr: SocketAddr,
    metrics: Arc<ExoMetrics>,
}

impl ExoDaemon {
    /// Boot the vessel and prepare the daemon for serving.
    ///
    /// This starts the Vessel (both engines) but does NOT start the HTTP
    /// server. Call [`ExoDaemon::run_until_shutdown`] to begin serving.
    pub async fn start(config: DaemonConfig) -> Result<Self, ExoError> {
        let metrics = Arc::new(ExoMetrics::new()?);
        let vessel = Vessel::start(config.vessel).await?;

        Ok(Self {
            vessel,
            listen_addr: config.listen_addr,
            metrics,
        })
    }

    /// Serve the HTTP API until Ctrl+C, then shut down the vessel gracefully.
    pub async fn run_until_shutdown(self) -> Result<(), ExoError> {
        let inspector = self.vessel.inspector();
        let inbox = self.vessel.inbox().clone();
        let vessel_id = self.vessel.vessel_id();

        let app_state = Arc::new(AppState {
            inspector,
            metrics: self.metrics,
            inbox,
            vessel_id,
        });

        let router = routes::build_router(app_state);

        let listener = tokio::net::TcpListener::bind(self.listen_addr)
            .await
            .map_err(|e| ExoError::Config(format!("failed to bind {}: {e}", self.listen_addr)))?;

        tracing::info!(
            listen_addr = %self.listen_addr,
            vessel_id = %vessel_id,
            "daemon listening"
        );

        axum::serve(listener, router)
            .with_graceful_shutdown(shutdown_signal())
            .await
            .map_err(|e| ExoError::Config(format!("HTTP server error: {e}")))?;

        tracing::info!("HTTP server stopped, shutting down vessel");
        self.vessel.shutdown().await?;

        Ok(())
    }
}

/// Wait for Ctrl+C (SIGINT) to signal graceful shutdown.
async fn shutdown_signal() {
    tokio::signal::ctrl_c()
        .await
        .expect("failed to install Ctrl+C handler");
    tracing::info!("received Ctrl+C, initiating graceful shutdown");
}

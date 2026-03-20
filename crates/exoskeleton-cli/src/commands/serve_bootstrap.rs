//! `exo serve-bootstrap` subcommand.
//!
//! Starts the daemon in pre-bootstrap mode. Serves bootstrap API endpoints
//! without starting cognitive engines. After bootstrap completes, transitions
//! to full daemon mode.

use exoskeleton_daemon::bootstrap_server;
use exoskeleton_daemon::ExoDaemon;

use crate::client::CliError;

pub async fn run_serve_bootstrap(
    data_dir: String,
    listen: String,
    log_level: String,
) -> Result<(), CliError> {
    // Initialize tracing
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(&log_level));
    tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .with_writer(std::io::stderr)
        .init();

    // Parse data_dir and listen_addr
    let data_dir = std::path::PathBuf::from(&data_dir);
    let listen_addr: std::net::SocketAddr = listen
        .parse()
        .map_err(|e| CliError::Config(format!("invalid listen address '{listen}': {e}")))?;

    // Create data_dir if it doesn't exist
    std::fs::create_dir_all(&data_dir)
        .map_err(|e| CliError::Config(format!("failed to create data dir: {e}")))?;

    // Run bootstrap server
    let result = bootstrap_server::run_bootstrap_server(data_dir, listen_addr)
        .await
        .map_err(|e| CliError::Other(e.to_string()))?;

    // If start-vessel was called, transition to full mode
    if let Some(daemon_config) = result {
        tracing::info!("Bootstrap complete — starting full daemon");
        let daemon = ExoDaemon::start(daemon_config)
            .await
            .map_err(|e| CliError::Other(e.to_string()))?;
        daemon
            .run_until_shutdown()
            .await
            .map_err(|e| CliError::Other(e.to_string()))?;
    } else {
        tracing::info!("Bootstrap server shut down without starting vessel");
    }

    Ok(())
}

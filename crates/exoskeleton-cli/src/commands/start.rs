//! `exo start` — boot a Vessel and HTTP daemon in the foreground.

use std::net::SocketAddr;
use std::path::PathBuf;

use exoskeleton_daemon::{DaemonConfig, ExoDaemon};
use exoskeleton_host::VesselConfig;

use crate::client::CliError;

/// Boot the Vessel and daemon, running until Ctrl+C.
pub async fn run_start(
    config_path: Option<String>,
    data_dir: Option<String>,
    mission: Option<String>,
    listen: Option<String>,
    log_level: String,
) -> Result<(), CliError> {
    // Initialize tracing
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(&log_level));
    tracing_subscriber::fmt().with_env_filter(env_filter).init();

    // Load or build VesselConfig
    let vessel_config = if let Some(ref path) = config_path {
        VesselConfig::from_file(&PathBuf::from(path))
            .map_err(|e| CliError::Config(format!("failed to load config from {path}: {e}")))?
    } else {
        // Build a default config from CLI args
        let mut config = VesselConfig::default();
        if let Some(ref dir) = data_dir {
            config.data_dir = PathBuf::from(dir);
        }
        if let Some(ref m) = mission {
            config.mission = m.clone();
        }
        if config.mission.is_empty() {
            return Err(CliError::Config(
                "mission is required: use --config or --mission".into(),
            ));
        }
        config
    };

    // Determine listen address
    let listen_addr: SocketAddr = if let Some(ref addr_str) = listen {
        addr_str
            .parse()
            .map_err(|e| CliError::Config(format!("invalid listen address: {e}")))?
    } else {
        vessel_config
            .daemon_listen
            .unwrap_or_else(|| "127.0.0.1:7600".parse().unwrap())
    };

    let daemon_config = DaemonConfig {
        vessel: vessel_config,
        listen_addr,
    };

    tracing::info!(
        listen = %listen_addr,
        "starting vessel and daemon"
    );

    let daemon = ExoDaemon::start(daemon_config)
        .await
        .map_err(|e| CliError::Other(format!("daemon start failed: {e}")))?;

    daemon
        .run_until_shutdown()
        .await
        .map_err(|e| CliError::Other(format!("daemon error: {e}")))?;

    Ok(())
}

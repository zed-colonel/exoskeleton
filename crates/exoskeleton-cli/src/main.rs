#![forbid(unsafe_code)]
//! CLI binary for Exoskeleton (`exo`).
//!
//! Provides two modes of operation:
//! - `exo start` — boots a Vessel and HTTP daemon in-process (foreground)
//! - All other commands — HTTP client queries against a running daemon

mod bootstrap;
mod client;
mod code;
mod commands;
mod format;

use clap::{Parser, Subcommand};

use crate::client::{CliError, DaemonClient};

/// Exoskeleton CLI -- persistent, governable AI runtime.
#[derive(Parser)]
#[command(name = "exo", about = "Exoskeleton CLI", version)]
struct Cli {
    /// Daemon address for client commands.
    #[arg(long, env = "EXO_DAEMON_ADDR", default_value = "http://127.0.0.1:7600")]
    addr: String,

    /// Output raw JSON instead of human-readable format.
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Bootstrap a new Vessel: configure LLM, run first-contact conversation.
    Bootstrap {
        /// Override the data directory (skips the wizard prompt for this field).
        /// Useful for Docker workflows where the data dir is always /data.
        #[arg(long)]
        data_dir: Option<String>,

        /// Log level (trace, debug, info, warn, error).
        #[arg(long, default_value = "warn")]
        log_level: String,
    },

    /// Start the daemon in pre-bootstrap mode.
    /// Serves bootstrap API endpoints without starting cognitive engines.
    /// After bootstrap completes, transitions to full daemon mode.
    #[command(name = "serve-bootstrap")]
    ServeBootstrap {
        /// Data directory for vessel state.
        #[arg(long, default_value = "/data")]
        data_dir: String,

        /// Listen address.
        #[arg(long, default_value = "0.0.0.0:7600")]
        listen: String,

        /// Log level (trace, debug, info, warn, error).
        #[arg(long, default_value = "info")]
        log_level: String,
    },

    /// Start the Vessel and HTTP daemon (foreground).
    Start {
        /// Path to vessel.toml configuration file.
        #[arg(long)]
        config: Option<String>,

        /// Override the data directory.
        #[arg(long)]
        data_dir: Option<String>,

        /// Override the vessel mission.
        #[arg(long)]
        mission: Option<String>,

        /// Daemon listen address (host:port).
        #[arg(long)]
        listen: Option<String>,

        /// Log level (trace, debug, info, warn, error).
        #[arg(long, default_value = "info")]
        log_level: String,
    },

    /// Show current state snapshot.
    Inspect {
        #[command(subcommand)]
        subcommand: Option<InspectSubcommand>,
    },

    /// Manage cognitive threads.
    Thread {
        #[command(subcommand)]
        subcommand: ThreadSubcommand,
    },

    /// Manage relationships.
    Relationship {
        #[command(subcommand)]
        subcommand: RelationshipSubcommand,
    },

    /// Show budget status (cognitive + tool).
    Budget,

    /// Show recent events from the event ledger.
    Events {
        /// Maximum number of events to show.
        #[arg(long, default_value = "50")]
        limit: usize,
    },

    /// Show dual engine health and status.
    Engines,

    /// Send a message to the vessel.
    Send {
        /// Message content.
        message: String,

        /// Source principal ID (UUID). Generates a new one if omitted.
        #[arg(long)]
        source: Option<String>,
    },

    /// Start a live coding session with a vessel.
    Code {
        /// The coding task to submit.
        task: Vec<String>,
    },

    /// Fetch and display an artifact by ID.
    Artifact {
        /// Artifact ID (hex string).
        id: String,
    },

    /// View memory (episodic summaries and long-term notes).
    Memory {
        /// Memory type filter: "episodic" or "long_term".
        #[arg(long)]
        r#type: Option<String>,

        /// Maximum number of items to show.
        #[arg(long, default_value = "50")]
        limit: usize,
    },

    /// View snapshot history or a specific snapshot.
    Snapshots {
        #[command(subcommand)]
        subcommand: Option<SnapshotSubcommand>,

        /// Maximum number of snapshots to show (for list mode).
        #[arg(long, default_value = "20")]
        limit: usize,
    },

    /// View inbox message history.
    InboxHistory {
        /// Maximum number of entries to show.
        #[arg(long, default_value = "50")]
        limit: usize,
    },

    /// View sanitized vessel configuration.
    Config,

    /// Reload thread charters from prompt files on disk.
    ReloadCharters,

    /// Fork a new vessel from a historical snapshot.
    Fork {
        /// Tick number to fork from.
        tick: u64,
        /// Target data directory for the forked vessel.
        #[arg(long)]
        data_dir: String,
        /// Optional mission override.
        #[arg(long)]
        mission: Option<String>,
    },

    /// Initialize a coding-optimized vessel configuration in the current directory.
    Init {
        /// Output path for vessel.toml (default: .exo/vessel.toml).
        #[arg(long)]
        output: Option<String>,

        /// Override the vessel mission.
        #[arg(long)]
        mission: Option<String>,
    },

    /// Run coding benchmarks against a task suite.
    Bench {
        /// Path to a single task spec TOML file.
        #[arg(long)]
        task: Option<String>,

        /// Path to a directory of task spec TOML files.
        #[arg(long)]
        suite: Option<String>,

        /// Vessel config for live runs (required for agent execution).
        #[arg(long)]
        config: Option<String>,

        /// Record results with this label (e.g., "baseline-v1").
        #[arg(long)]
        record: Option<String>,

        /// Compare results against a previously recorded baseline.
        #[arg(long)]
        compare: Option<String>,

        /// Directory for result storage (default: benchmarks/results/).
        #[arg(long)]
        results_dir: Option<String>,

        /// Dry-run: validate harness without booting the agent.
        #[arg(long)]
        dry_run: bool,

        /// Verbose: stream per-step output during execution.
        #[arg(long)]
        verbose: bool,

        /// LLM provider override (e.g., "anthropic", "openai", "ollama").
        /// Overrides the config file's frontier/local settings.
        #[arg(long)]
        provider: Option<String>,

        /// Model name override (e.g., "claude-sonnet-4-20250514", "llama3.2:latest").
        #[arg(long)]
        model: Option<String>,

        /// Environment variable name for the API key (e.g., "ANTHROPIC_PLATFORM_API_KEY").
        #[arg(long)]
        api_key_env: Option<String>,

        /// Local model endpoint URL (e.g., "http://localhost:11434").
        /// Implies --provider=ollama if --provider is not set.
        #[arg(long)]
        local_endpoint: Option<String>,
    },
}

#[derive(Subcommand)]
enum InspectSubcommand {
    /// Show detail for a specific tick.
    Tick {
        /// Tick ID (UUID).
        id: String,
    },
    /// Show recent tick history.
    Ticks {
        /// Maximum number of ticks to show.
        #[arg(long, default_value = "20")]
        limit: usize,
    },
}

#[derive(Subcommand)]
enum ThreadSubcommand {
    /// List all registered threads.
    List,
    /// Show detail for a specific thread.
    Inspect {
        /// Thread ID (UUID or prefix).
        id: String,
    },
}

#[derive(Subcommand)]
enum RelationshipSubcommand {
    /// Show the current relationship snapshot.
    Show,
    /// Show relationship history for a principal.
    History {
        /// Principal ID (UUID).
        principal_id: String,

        /// Maximum number of records to show.
        #[arg(long, default_value = "50")]
        limit: usize,
    },
}

#[derive(Subcommand)]
enum SnapshotSubcommand {
    /// View snapshot at a specific tick number.
    At {
        /// Tick number.
        tick: u64,
    },
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    let result = match cli.command {
        Commands::Bootstrap {
            data_dir,
            log_level,
        } => bootstrap::run_bootstrap(data_dir, log_level).await,

        Commands::ServeBootstrap {
            data_dir,
            listen,
            log_level,
        } => commands::serve_bootstrap::run_serve_bootstrap(data_dir, listen, log_level).await,

        Commands::Start {
            config,
            data_dir,
            mission,
            listen,
            log_level,
        } => commands::start::run_start(config, data_dir, mission, listen, log_level).await,

        Commands::Init { output, mission } => commands::init::run_init(output, mission).await,

        Commands::Bench {
            task,
            suite,
            config,
            record,
            compare,
            results_dir,
            dry_run,
            verbose,
            provider,
            model,
            api_key_env,
            local_endpoint,
        } => {
            commands::bench::run_bench(
                task,
                suite,
                config,
                record,
                compare,
                results_dir,
                dry_run,
                verbose,
                provider,
                model,
                api_key_env,
                local_endpoint,
            )
            .await
        }

        // All remaining commands are client commands that talk to a running daemon.
        _ => run_client_command(&cli).await,
    };

    if let Err(e) = result {
        match &e {
            CliError::Connection(_) => {
                eprintln!("error: cannot connect to daemon at {}", cli.addr);
                eprintln!(
                    "hint: is the vessel running? start it with: exo start --config vessel.toml"
                );
            }
            _ => {
                eprintln!("error: {e}");
            }
        }
        std::process::exit(1);
    }
}

/// Dispatch client commands (everything except `start`).
async fn run_client_command(cli: &Cli) -> Result<(), CliError> {
    let client = DaemonClient::new(&cli.addr);

    match &cli.command {
        Commands::Inspect { subcommand } => match subcommand {
            Some(InspectSubcommand::Tick { id }) => {
                commands::inspect::run_inspect_tick(&client, id, cli.json).await
            }
            Some(InspectSubcommand::Ticks { limit }) => {
                commands::inspect::run_inspect_ticks(&client, *limit, cli.json).await
            }
            None => commands::inspect::run_inspect(&client, cli.json).await,
        },

        Commands::Thread { subcommand } => match subcommand {
            ThreadSubcommand::List => commands::thread::run_thread_list(&client, cli.json).await,
            ThreadSubcommand::Inspect { id } => {
                commands::thread::run_thread_inspect(&client, id, cli.json).await
            }
        },

        Commands::Relationship { subcommand } => match subcommand {
            RelationshipSubcommand::Show => {
                commands::relationship::run_relationship_show(&client, cli.json).await
            }
            RelationshipSubcommand::History {
                principal_id,
                limit,
            } => {
                commands::relationship::run_relationship_history(
                    &client,
                    principal_id,
                    *limit,
                    cli.json,
                )
                .await
            }
        },

        Commands::Budget => commands::budget::run_budget(&client, cli.json).await,

        Commands::Events { limit } => commands::events::run_events(&client, *limit, cli.json).await,

        Commands::Engines => commands::engines::run_engines(&client, cli.json).await,

        Commands::Send { message, source } => {
            let actual_source = match source {
                Some(ref s) => s.clone(),
                None => uuid::Uuid::new_v4().to_string(),
            };

            commands::send::run_send(&client, &actual_source, message, cli.json).await
        }

        Commands::Code { task } => {
            let task_text = task.join(" ");
            code::run_code_session(&cli.addr, &task_text).await
        }

        Commands::Artifact { id } => commands::artifact::run_artifact(&client, id, cli.json).await,

        Commands::Memory { r#type, limit } => {
            commands::memory::run_memory(&client, r#type.as_deref(), *limit, cli.json).await
        }

        Commands::Snapshots { subcommand, limit } => match subcommand {
            Some(SnapshotSubcommand::At { tick }) => {
                commands::snapshot::run_snapshot_at(&client, *tick, cli.json).await
            }
            None => commands::snapshot::run_snapshots(&client, *limit, cli.json).await,
        },

        Commands::InboxHistory { limit } => {
            commands::inbox_history::run_inbox_history(&client, *limit, cli.json).await
        }

        Commands::Config => commands::config::run_config(&client, cli.json).await,

        Commands::ReloadCharters => {
            commands::reload_charters::run_reload_charters(&client, cli.json).await
        }

        Commands::Fork {
            tick,
            data_dir,
            mission,
        } => commands::fork::run_fork(client, *tick, data_dir, mission.as_deref(), cli.json).await,

        Commands::Bootstrap { .. } => unreachable!("bootstrap is handled in main()"),
        Commands::ServeBootstrap { .. } => {
            unreachable!("serve-bootstrap is handled in main()")
        }
        Commands::Start { .. } => unreachable!("start is handled in main()"),
        Commands::Init { .. } => unreachable!("init is handled in main()"),
        Commands::Bench { .. } => unreachable!("bench is handled in main()"),
    }
}

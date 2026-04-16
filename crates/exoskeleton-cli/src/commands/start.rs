//! `exo start` — boot a Vessel and HTTP daemon in the foreground.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use exoskeleton_daemon::{DaemonConfig, ExoDaemon};
use exoskeleton_host::config::{
    CodingDefaults, FrontierModelConfig, FrontierProvider, LocalApiFormat, LocalModelConfig,
    ThreadsSection,
};
use exoskeleton_host::VesselConfig;

use crate::client::CliError;

#[derive(Debug, Clone, Default)]
pub(crate) struct LlmOverrideOptions {
    pub provider: Option<String>,
    pub model: Option<String>,
    pub api_key_env: Option<String>,
    pub local_endpoint: Option<String>,
}

pub(crate) fn init_tracing(log_level: &str) -> Result<(), CliError> {
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(log_level));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .with_writer(std::io::stderr)
        .try_init();
    Ok(())
}

pub(crate) fn load_vessel_config(
    config_path: Option<String>,
    data_dir: Option<String>,
    mission: Option<String>,
) -> Result<VesselConfig, CliError> {
    let vessel_config = if let Some(ref path) = config_path {
        VesselConfig::from_file(&PathBuf::from(path))
            .map_err(|e| CliError::Config(format!("failed to load config from {path}: {e}")))?
    } else {
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

    Ok(vessel_config)
}

pub(crate) fn load_local_code_vessel_config(
    config_path: Option<String>,
    data_dir: Option<String>,
    mission: Option<String>,
    workspace_root: Option<String>,
) -> Result<VesselConfig, CliError> {
    let mut vessel_config = if let Some(ref path) = config_path {
        VesselConfig::from_file(&PathBuf::from(path))
            .map_err(|e| CliError::Config(format!("failed to load config from {path}: {e}")))?
    } else {
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

        let inferred_workspace = workspace_root
            .as_ref()
            .map(PathBuf::from)
            .or_else(|| {
                std::env::current_dir()
                    .ok()
                    .map(|cwd| detect_git_root(&cwd).unwrap_or(cwd))
            })
            .map(|path| path.to_string_lossy().into_owned());

        config.coding_thread = CodingDefaults::coding_thread_config(inferred_workspace);
        config.tool_policy = CodingDefaults::tool_policy_config();
        config
    };

    if let Some(ref dir) = data_dir {
        vessel_config.data_dir = PathBuf::from(dir);
    }
    if let Some(ref m) = mission {
        vessel_config.mission = m.clone();
    }
    if let Some(root) = workspace_root {
        vessel_config.coding_thread.workspace_root = Some(root);
    }
    vessel_config.coding_thread.return_to_idle_after_completion = true;

    Ok(vessel_config)
}

pub(crate) fn apply_local_code_operator_waiting_posture(config: &mut VesselConfig) {
    let standby_suffix = "Remain ready to receive coding instructions from the operator. Stay idle until a concrete operator request arrives. Do not use exploratory code.*, fs.*, or shell.exec tools before the operator gives a task.";
    if !config.mission.contains(standby_suffix) {
        if config.mission.trim().is_empty() {
            config.mission = standby_suffix.to_string();
        } else {
            config.mission = format!("{}\n\n{}", config.mission.trim(), standby_suffix);
        }
    }

    let threads = config.threads.get_or_insert_with(ThreadsSection::default);
    threads.initiative_enabled = Some(false);
    threads.initiative_schedule = Some("on_demand".into());
}

/// Apply CLI flag overrides to the vessel's LLM configuration.
///
/// Supports three patterns:
/// 1. Frontier override: `--provider anthropic --model claude-sonnet-4-20250514 --api-key-env MY_KEY`
/// 2. Local override: `--local-endpoint http://localhost:11434 --model llama3.2:latest`
/// 3. Mix: `--provider openai --model gpt-4o --api-key-env OPENAI_API_KEY` (uses OpenAI-compat)
///
/// When `--local-endpoint` is provided without `--provider`, defaults to "ollama".
pub(crate) fn apply_llm_overrides(
    mut config: VesselConfig,
    overrides: &LlmOverrideOptions,
) -> Result<VesselConfig, CliError> {
    use exoskeleton_core::llm::LlmBackend;

    let provider = overrides.provider.as_deref();
    let model = overrides.model.as_deref();
    let api_key_env = overrides.api_key_env.as_deref();
    let local_endpoint = overrides.local_endpoint.as_deref();

    if let Some(endpoint) = local_endpoint {
        let provider_str = provider.unwrap_or("ollama");
        let api_format = match provider_str {
            "ollama" => LocalApiFormat::Ollama,
            _ => LocalApiFormat::OpenAICompat,
        };
        config.llm_config.local = Some(LocalModelConfig {
            endpoint: endpoint.into(),
            model: model.unwrap_or("llama3.2:latest").into(),
            api_format,
        });
        config.llm_config.default_backend = LlmBackend::Local;
        eprintln!(
            "  LLM override: local {} @ {} ({})",
            config.llm_config.local.as_ref().unwrap().model,
            endpoint,
            provider_str,
        );
        return Ok(config);
    }

    if let Some(provider_str) = provider {
        let frontier_provider = match provider_str {
            "anthropic" => FrontierProvider::Anthropic,
            "openai" => FrontierProvider::OpenAI,
            "gemini" => FrontierProvider::Gemini,
            "grok" => FrontierProvider::Grok,
            "openrouter" => FrontierProvider::OpenRouter,
            "deepseek" => FrontierProvider::DeepSeek,
            other => {
                return Err(CliError::Other(format!(
                    "unknown provider '{other}'. Supported: anthropic, openai, gemini, grok, openrouter, deepseek"
                )));
            }
        };

        let model_name = model.unwrap_or(default_model_for_provider(frontier_provider));
        let key_env = api_key_env.unwrap_or(default_api_key_env_for_provider(frontier_provider));

        config.llm_config.frontier = Some(FrontierModelConfig {
            provider: frontier_provider,
            model: model_name.into(),
            api_key_env: key_env.into(),
            endpoint: None,
        });
        config.llm_config.default_backend = LlmBackend::Frontier;
        eprintln!(
            "  LLM override: {} {} (key from ${})",
            provider_str, model_name, key_env,
        );
        return Ok(config);
    }

    if let Some(model_name) = model {
        match config.llm_config.default_backend {
            LlmBackend::Frontier => {
                if let Some(ref mut f) = config.llm_config.frontier {
                    eprintln!("  LLM override: model {} (frontier)", model_name);
                    f.model = model_name.into();
                }
            }
            LlmBackend::Local => {
                if let Some(ref mut l) = config.llm_config.local {
                    eprintln!("  LLM override: model {} (local)", model_name);
                    l.model = model_name.into();
                }
            }
        }
    }

    if let Some(key_env) = api_key_env {
        if let Some(ref mut f) = config.llm_config.frontier {
            f.api_key_env = key_env.into();
        }
    }

    Ok(config)
}

fn default_model_for_provider(provider: FrontierProvider) -> &'static str {
    match provider {
        FrontierProvider::Anthropic => "claude-sonnet-4-20250514",
        FrontierProvider::OpenAI => "gpt-4o",
        FrontierProvider::Gemini => "gemini-2.5-flash",
        FrontierProvider::Grok => "grok-3-mini",
        FrontierProvider::OpenRouter => "anthropic/claude-sonnet-4-20250514",
        FrontierProvider::DeepSeek => "deepseek-chat",
    }
}

fn default_api_key_env_for_provider(provider: FrontierProvider) -> &'static str {
    match provider {
        FrontierProvider::Anthropic => "ANTHROPIC_PLATFORM_API_KEY",
        FrontierProvider::OpenAI => "OPENAI_API_KEY",
        FrontierProvider::Gemini => "GEMINI_API_KEY",
        FrontierProvider::Grok => "GROK_API_KEY",
        FrontierProvider::OpenRouter => "OPENROUTER_API_KEY",
        FrontierProvider::DeepSeek => "DEEPSEEK_API_KEY",
    }
}

fn detect_git_root(start: &Path) -> Option<PathBuf> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(start)
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let root = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!root.is_empty()).then(|| PathBuf::from(root))
}

/// Boot the Vessel and daemon, running until Ctrl+C.
pub async fn run_start(
    config_path: Option<String>,
    data_dir: Option<String>,
    mission: Option<String>,
    listen: Option<String>,
    log_level: String,
) -> Result<(), CliError> {
    init_tracing(&log_level)?;

    let vessel_config = load_vessel_config(config_path, data_dir, mission)?;

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

#[cfg(test)]
mod tests {
    use exoskeleton_core::llm::LlmBackend;
    use exoskeleton_host::config::{FrontierProvider, LocalApiFormat};

    use super::{
        apply_llm_overrides, apply_local_code_operator_waiting_posture,
        load_local_code_vessel_config, load_vessel_config, LlmOverrideOptions,
    };

    #[test]
    fn load_vessel_config_requires_mission_without_config_file() {
        let err = load_vessel_config(None, None, None).unwrap_err();
        assert!(err.to_string().contains("mission is required"));
    }

    #[test]
    fn load_vessel_config_applies_cli_overrides() {
        let config = load_vessel_config(
            None,
            Some("/tmp/exo-local-code-test".into()),
            Some("local tui mission".into()),
        )
        .unwrap();

        assert_eq!(
            config.data_dir,
            std::path::PathBuf::from("/tmp/exo-local-code-test")
        );
        assert_eq!(config.mission, "local tui mission");
    }

    #[test]
    fn llm_overrides_configure_frontier_backend() {
        let config = apply_llm_overrides(
            load_vessel_config(None, None, Some("mission".into())).unwrap(),
            &LlmOverrideOptions {
                provider: Some("anthropic".into()),
                model: Some("claude-sonnet-4-20250514".into()),
                api_key_env: Some("MY_ANTHROPIC_KEY".into()),
                local_endpoint: None,
            },
        )
        .unwrap();

        assert!(matches!(
            config.llm_config.default_backend,
            LlmBackend::Frontier
        ));
        let frontier = config.llm_config.frontier.expect("frontier config");
        assert!(matches!(frontier.provider, FrontierProvider::Anthropic));
        assert_eq!(frontier.model, "claude-sonnet-4-20250514");
        assert_eq!(frontier.api_key_env, "MY_ANTHROPIC_KEY");
    }

    #[test]
    fn llm_overrides_configure_local_backend() {
        let config = apply_llm_overrides(
            load_vessel_config(None, None, Some("mission".into())).unwrap(),
            &LlmOverrideOptions {
                provider: None,
                model: Some("llama3.2:latest".into()),
                api_key_env: None,
                local_endpoint: Some("http://localhost:11434".into()),
            },
        )
        .unwrap();

        assert!(matches!(
            config.llm_config.default_backend,
            LlmBackend::Local
        ));
        let local = config.llm_config.local.expect("local config");
        assert_eq!(local.endpoint, "http://localhost:11434");
        assert_eq!(local.model, "llama3.2:latest");
        assert!(matches!(local.api_format, LocalApiFormat::Ollama));
    }

    #[test]
    fn local_code_config_uses_coding_defaults_without_config_file() {
        let config = load_local_code_vessel_config(
            None,
            Some("/tmp/exo-local-code-test".into()),
            Some("local tui mission".into()),
            Some("/tmp/workspace".into()),
        )
        .unwrap();

        assert!(config.coding_thread.enabled);
        assert_eq!(
            config.coding_thread.workspace_root.as_deref(),
            Some("/tmp/workspace")
        );
        assert!(config.coding_thread.return_to_idle_after_completion);
        assert_eq!(
            config
                .tool_policy
                .rules
                .get("shell.exec")
                .map(|rule| format!("{rule:?}")),
            Some("Ask".into())
        );
    }

    #[test]
    fn local_code_config_overrides_workspace_root_from_config_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("vessel.toml");
        std::fs::write(
            &path,
            r#"
[vessel]
mission = "config mission"
data_dir = "/tmp/exo"

[llm]

[coding_thread]
enabled = true
workspace_root = "/tmp/original"
"#,
        )
        .unwrap();

        let config = load_local_code_vessel_config(
            Some(path.to_string_lossy().into_owned()),
            None,
            None,
            Some("/tmp/override".into()),
        )
        .unwrap();

        assert_eq!(
            config.coding_thread.workspace_root.as_deref(),
            Some("/tmp/override")
        );
        assert!(config.coding_thread.return_to_idle_after_completion);
    }

    #[test]
    fn local_code_operator_waiting_posture_disables_initiative_and_updates_mission() {
        let mut config =
            load_vessel_config(None, None, Some("Local coding session".into())).unwrap();

        apply_local_code_operator_waiting_posture(&mut config);

        assert!(config
            .mission
            .contains("Remain ready to receive coding instructions from the operator"));
        let threads = config.threads.expect("threads section should be present");
        assert_eq!(threads.initiative_enabled, Some(false));
        assert_eq!(threads.initiative_schedule.as_deref(), Some("on_demand"));
    }
}

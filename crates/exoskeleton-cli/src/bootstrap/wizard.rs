//! Interactive configuration wizard for vessel bootstrap.

use std::path::PathBuf;

use dialoguer::{Confirm, Input, Select};
use exoskeleton_host::config::{FrontierProvider, LocalApiFormat};

use crate::client::CliError;

/// Result of the configuration wizard.
pub struct WizardResult {
    pub data_dir: PathBuf,
    pub local_config: Option<LocalConfig>,
    pub frontier_config: Option<FrontierConfig>,
    pub listen_port: u16,
}

/// Local LLM backend configuration from wizard.
pub struct LocalConfig {
    pub endpoint: String,
    pub model: String,
    pub api_format: LocalApiFormat,
}

/// Frontier LLM backend configuration from wizard.
pub struct FrontierConfig {
    pub provider: FrontierProvider,
    pub model: String,
    pub api_key_env: String,
}

/// Run the interactive configuration wizard.
///
/// If `data_dir_override` is `Some`, that value is used without prompting.
pub fn run_wizard(data_dir_override: Option<String>) -> Result<WizardResult, CliError> {
    super::print_step(1, "Configuration");
    println!();

    // Data directory
    let data_dir = if let Some(dir) = data_dir_override {
        let path = PathBuf::from(shellexpand_tilde(&dir));
        println!("  Data directory: {}", path.display());
        path
    } else {
        let default_data = dirs_default_data();
        let data_dir_str: String = Input::new()
            .with_prompt("  Data directory")
            .default(default_data.to_string_lossy().to_string())
            .interact_text()
            .map_err(|e| CliError::Other(format!("input error: {e}")))?;
        PathBuf::from(shellexpand_tilde(&data_dir_str))
    };

    println!();

    // LLM backend selection
    let backend_options = vec![
        "Frontier only (Anthropic, OpenAI)",
        "Local only (Ollama, vLLM, LM Studio)",
        "Both (local + frontier)",
    ];
    let backend_choice = Select::new()
        .with_prompt("  LLM backend")
        .items(&backend_options)
        .default(0)
        .interact()
        .map_err(|e| CliError::Other(format!("selection error: {e}")))?;

    let (local_config, frontier_config) = match backend_choice {
        0 => (None, Some(configure_frontier()?)),
        1 => (Some(configure_local()?), None),
        2 => (Some(configure_local()?), Some(configure_frontier()?)),
        _ => unreachable!(),
    };

    println!();

    // Daemon listen port
    let listen_port: u16 = Input::new()
        .with_prompt("  Daemon port")
        .default(7600)
        .interact_text()
        .map_err(|e| CliError::Other(format!("input error: {e}")))?;

    Ok(WizardResult {
        data_dir,
        local_config,
        frontier_config,
        listen_port,
    })
}

fn configure_frontier() -> Result<FrontierConfig, CliError> {
    println!();

    let provider_options = vec!["Anthropic", "OpenAI"];
    let provider_choice = Select::new()
        .with_prompt("  Provider")
        .items(&provider_options)
        .default(0)
        .interact()
        .map_err(|e| CliError::Other(format!("selection error: {e}")))?;

    let provider = match provider_choice {
        0 => FrontierProvider::Anthropic,
        1 => FrontierProvider::OpenAI,
        _ => unreachable!(),
    };

    let default_model = match provider {
        FrontierProvider::Anthropic => "claude-sonnet-4-20250514",
        FrontierProvider::OpenAI => "gpt-4o",
    };
    let model: String = Input::new()
        .with_prompt("  Model")
        .default(default_model.into())
        .interact_text()
        .map_err(|e| CliError::Other(format!("input error: {e}")))?;

    let default_env = match provider {
        FrontierProvider::Anthropic => "ANTHROPIC_API_KEY",
        FrontierProvider::OpenAI => "OPENAI_API_KEY",
    };
    let api_key_env: String = Input::new()
        .with_prompt("  API key env var")
        .default(default_env.into())
        .interact_text()
        .map_err(|e| CliError::Other(format!("input error: {e}")))?;

    // Check if the env var is set
    if std::env::var(&api_key_env).is_err() {
        println!();
        println!("  WARNING: ${api_key_env} is not set in the current environment.");
        let proceed = Confirm::new()
            .with_prompt("  Continue anyway?")
            .default(false)
            .interact()
            .map_err(|e| CliError::Other(format!("confirm error: {e}")))?;
        if !proceed {
            return Err(CliError::Config(format!(
                "Set ${api_key_env} and re-run bootstrap"
            )));
        }
    }

    Ok(FrontierConfig {
        provider,
        model,
        api_key_env,
    })
}

fn configure_local() -> Result<LocalConfig, CliError> {
    println!();

    let endpoint: String = Input::new()
        .with_prompt("  Local endpoint")
        .default("http://localhost:11434".into())
        .interact_text()
        .map_err(|e| CliError::Other(format!("input error: {e}")))?;

    let model: String = Input::new()
        .with_prompt("  Local model")
        .default("llama3.2:latest".into())
        .interact_text()
        .map_err(|e| CliError::Other(format!("input error: {e}")))?;

    let format_options = vec![
        "OpenAI-compatible (/v1/chat/completions)",
        "Ollama native (/api/chat)",
    ];
    let format_choice = Select::new()
        .with_prompt("  API format")
        .items(&format_options)
        .default(0)
        .interact()
        .map_err(|e| CliError::Other(format!("selection error: {e}")))?;

    let api_format = match format_choice {
        0 => LocalApiFormat::OpenAICompat,
        1 => LocalApiFormat::Ollama,
        _ => unreachable!(),
    };

    Ok(LocalConfig {
        endpoint,
        model,
        api_format,
    })
}

/// Default data directory: ~/.exo/vessels/<random-short-id>
fn dirs_default_data() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    PathBuf::from(home)
        .join(".exo")
        .join("vessels")
        .join("default")
}

/// Expand ~ to home directory.
fn shellexpand_tilde(s: &str) -> String {
    if let Some(rest) = s.strip_prefix("~/") {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
        format!("{home}/{rest}")
    } else if s == "~" {
        std::env::var("HOME").unwrap_or_else(|_| "/tmp".into())
    } else {
        s.to_string()
    }
}

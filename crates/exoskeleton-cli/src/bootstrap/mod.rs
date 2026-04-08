//! Vessel bootstrap: interactive configuration wizard and first-contact protocol.
//!
//! The bootstrap process brings a new Vessel into existence:
//! 1. **Wizard** — interactive prompts to configure LLM backend, data dir, etc.
//! 2. **Verification** — connectivity check against the configured backend
//! 3. **First Contact** — a multi-turn conversation where the agent and user
//!    establish identity, purpose, and relationship
//! 4. **Extraction** — an LLM call to distill name/mission from the conversation
//! 5. **Persistence** — write vessel.toml, bootstrap record, and origin artifacts

pub mod first_contact;
pub mod wizard;

use std::io::Write;
use std::path::{Path, PathBuf};

use exoskeleton_core::llm::{LlmBackend, LlmMessage, LlmRequest, LlmRole};
use exoskeleton_core::prompt::PromptRegistry;
use exoskeleton_host::config::{
    FrontierModelConfig, FrontierProvider, LlmConfig, LocalApiFormat, LocalModelConfig,
};
use exoskeleton_host::{direct_llm_call, prompt_loader};

use self::first_contact::FirstContactResult;
use self::wizard::WizardResult;
use crate::client::CliError;

/// Run the full bootstrap process.
///
/// If `data_dir_override` is provided, the wizard will use it instead of
/// prompting. This is useful for Docker workflows where the data dir is
/// always `/data`.
pub async fn run_bootstrap(
    data_dir_override: Option<String>,
    log_level: String,
) -> Result<(), CliError> {
    // Initialize tracing
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(&log_level));
    tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .with_writer(std::io::stderr)
        .init();

    print_banner();

    // Step 1: Configuration wizard
    let wizard_result = wizard::run_wizard(data_dir_override)?;

    // Step 2: Build LLM config and verify connectivity
    let llm_config = build_llm_config(&wizard_result);
    println!();
    print_step(2, "Verifying LLM Connectivity");
    verify_connectivity(&llm_config).await?;

    // Step 2.5: Load prompt registry (Epoch 0)
    let prompts = load_bootstrap_prompts(Some(&wizard_result.data_dir));

    // Step 3: First contact conversation
    println!();
    print_step(3, "First Contact");
    println!("  The vessel is ready. Let's bring it to life.\n");
    let contact_result = first_contact::run_first_contact(&llm_config, &prompts).await?;

    // Step 4: Extract identity from conversation
    println!();
    print_step(4, "Establishing Identity");
    let identity = extract_identity(&llm_config, &contact_result, &prompts).await?;

    println!("  Name:    {}", identity.vessel_name);
    println!("  Mission: {}", identity.mission);
    if let Some(ref user_name) = identity.user_name {
        println!("  User:    {}", user_name);
    }

    // Step 5: Write configuration
    println!();
    print_step(5, "Writing Configuration");
    let config_path = write_vessel_config(&wizard_result, &identity)?;
    write_bootstrap_record(&wizard_result.data_dir, &contact_result, &identity)?;

    // Final summary
    println!();
    println!("  {}", "=".repeat(60));
    println!("  Bootstrap complete.");
    println!();
    println!("  Config:  {}", config_path.display());
    println!("  Data:    {}", wizard_result.data_dir.display());
    println!();
    println!("  To start this vessel:");
    println!("    exo start --config {}", config_path.display());
    println!();
    println!("  Or with Docker:");
    println!(
        "    docker run -v {}:/data -p {}:7600 exoskeleton:latest",
        wizard_result.data_dir.display(),
        wizard_result.listen_port
    );
    println!();

    Ok(())
}

/// Load prompt registry for bootstrap.
///
/// Bootstrap runs before the vessel exists. Uses tier-2 (project-level) and
/// tier-3 (compiled-in) loading. If a data_dir is available, also checks tier-1.
fn load_bootstrap_prompts(data_dir: Option<&Path>) -> PromptRegistry {
    let mut registry = PromptRegistry::with_defaults();
    if let Some(dir) = data_dir {
        prompt_loader::load_prompt_overrides(&mut registry, dir);
    } else {
        prompt_loader::load_project_prompt_overrides(&mut registry);
    }
    registry
}

/// Build an LlmConfig from the wizard results.
fn build_llm_config(wizard: &WizardResult) -> LlmConfig {
    let local = wizard.local_config.as_ref().map(|lc| LocalModelConfig {
        endpoint: lc.endpoint.clone(),
        model: lc.model.clone(),
        api_format: lc.api_format,
    });

    let frontier = wizard
        .frontier_config
        .as_ref()
        .map(|fc| FrontierModelConfig {
            provider: fc.provider,
            model: fc.model.clone(),
            api_key_env: fc.api_key_env.clone(),
            endpoint: None,
        });

    let default_backend = if frontier.is_some() {
        LlmBackend::Frontier
    } else {
        LlmBackend::Local
    };

    LlmConfig {
        local,
        frontier,
        default_backend,
        max_output_tokens: 4096,
        timeout_secs: 120,
    }
}

/// Verify connectivity to the configured LLM backend.
async fn verify_connectivity(llm_config: &LlmConfig) -> Result<(), CliError> {
    let request = LlmRequest {
        backend: None,
        system_prompt: None,
        messages: vec![LlmMessage::text(LlmRole::User, "Respond with exactly: ok")],
        max_output_tokens: 16,
        temperature: Some(0.0),
        stop_sequences: vec![],
        stream: false,
        tools: vec![],
    };

    print!("  Connecting to LLM backend... ");
    std::io::stdout().flush().ok();

    match direct_llm_call(llm_config, request).await {
        Ok(response) => {
            println!("connected ({})", response.model);
            Ok(())
        }
        Err(e) => {
            println!("FAILED");
            Err(CliError::Config(format!(
                "LLM connectivity check failed: {e}\n\
                 hint: verify your backend is running and API keys are set"
            )))
        }
    }
}

/// Extract vessel identity from the first-contact conversation transcript.
async fn extract_identity(
    llm_config: &LlmConfig,
    contact: &FirstContactResult,
    prompts: &PromptRegistry,
) -> Result<VesselIdentity, CliError> {
    print!("  Analyzing conversation... ");
    std::io::stdout().flush().ok();

    let transcript = contact
        .messages
        .iter()
        .map(|m| {
            let role = match m.role {
                LlmRole::User => "Human",
                LlmRole::Assistant => "Vessel",
                LlmRole::System => "System",
            };
            format!(
                "{role}: {}",
                m.content
                    .iter()
                    .filter_map(|block| match block {
                        exoskeleton_core::llm::ContentBlock::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<String>()
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n");

    let extraction_prompt = prompts
        .resolve(
            "bootstrap-identity-extraction",
            &[("transcript", &transcript)],
        )
        .map_err(|e| CliError::Other(format!("prompt resolution failed: {e}")))?;

    let request = LlmRequest {
        backend: None,
        system_prompt: Some(
            "You are a precise information extractor. Respond only with valid JSON.".into(),
        ),
        messages: vec![LlmMessage::text(LlmRole::User, extraction_prompt)],
        max_output_tokens: 512,
        temperature: Some(0.0),
        stop_sequences: vec![],
        stream: false,
        tools: vec![],
    };

    let response = direct_llm_call(llm_config, request)
        .await
        .map_err(|e| CliError::Other(format!("identity extraction failed: {e}")))?;

    // Parse the JSON response — strip markdown code fences if present
    let response_text = response.text();
    let raw = response_text.trim();
    let json_str = extract_json_object(raw).unwrap_or(raw);

    let identity: VesselIdentity = serde_json::from_str(json_str).map_err(|e| {
        CliError::Other(format!(
            "failed to parse identity JSON: {e}\nraw response: {}",
            response_text
        ))
    })?;

    println!("done");
    Ok(identity)
}

/// Write the vessel.toml configuration file.
fn write_vessel_config(
    wizard: &WizardResult,
    identity: &VesselIdentity,
) -> Result<PathBuf, CliError> {
    // Ensure data directory exists
    std::fs::create_dir_all(&wizard.data_dir)
        .map_err(|e| CliError::Config(format!("failed to create data dir: {e}")))?;

    let config_path = wizard.data_dir.join("vessel.toml");
    let vessel_id = uuid::Uuid::new_v4();

    let mut toml = String::new();
    toml.push_str(&format!(
        "# Vessel configuration — generated by `exo bootstrap`\n\
         # {}\n\n",
        chrono::Utc::now().format("%Y-%m-%d %H:%M:%S UTC")
    ));

    // [vessel] section
    toml.push_str("[vessel]\n");
    toml.push_str(&format!("vessel_id = \"{vessel_id}\"\n"));
    toml.push_str(&format!(
        "mission = {}\n",
        toml_string_escape(&identity.mission)
    ));
    toml.push_str(&format!(
        "data_dir = {}\n",
        toml_string_escape(&wizard.data_dir.to_string_lossy())
    ));
    toml.push_str("master_loop_interval_secs = 60\n");
    toml.push('\n');

    // [llm] section
    toml.push_str("[llm]\n");
    if wizard.frontier_config.is_some() {
        toml.push_str("default_backend = \"frontier\"\n");
    } else {
        toml.push_str("default_backend = \"local\"\n");
    }
    toml.push_str("max_output_tokens = 4096\n");
    toml.push_str("timeout_secs = 120\n");
    toml.push('\n');

    // [llm.local] section
    if let Some(ref lc) = wizard.local_config {
        toml.push_str("[llm.local]\n");
        toml.push_str(&format!(
            "endpoint = {}\n",
            toml_string_escape(&lc.endpoint)
        ));
        toml.push_str(&format!("model = {}\n", toml_string_escape(&lc.model)));
        let fmt_str = match lc.api_format {
            LocalApiFormat::OpenAICompat => "openai_compat",
            LocalApiFormat::Ollama => "ollama",
        };
        toml.push_str(&format!("api_format = \"{fmt_str}\"\n"));
        toml.push('\n');
    }

    // [llm.frontier] section
    if let Some(ref fc) = wizard.frontier_config {
        toml.push_str("[llm.frontier]\n");
        let provider_str = match fc.provider {
            FrontierProvider::Anthropic => "anthropic",
            FrontierProvider::OpenAI => "openai",
            FrontierProvider::Gemini => "gemini",
            FrontierProvider::Grok => "grok",
            FrontierProvider::OpenRouter => "openrouter",
            FrontierProvider::DeepSeek => "deepseek",
        };
        toml.push_str(&format!("provider = \"{provider_str}\"\n"));
        toml.push_str(&format!("model = {}\n", toml_string_escape(&fc.model)));
        toml.push_str(&format!(
            "api_key_env = {}\n",
            toml_string_escape(&fc.api_key_env)
        ));
        toml.push('\n');
    }

    // [daemon] section
    toml.push_str("[daemon]\n");
    toml.push_str(&format!("listen = \"0.0.0.0:{}\"\n", wizard.listen_port));

    std::fs::write(&config_path, &toml)
        .map_err(|e| CliError::Config(format!("failed to write vessel.toml: {e}")))?;

    println!("  Written: {}", config_path.display());
    Ok(config_path)
}

/// Write the bootstrap record (conversation transcript + identity).
fn write_bootstrap_record(
    data_dir: &Path,
    contact: &FirstContactResult,
    identity: &VesselIdentity,
) -> Result<(), CliError> {
    let bootstrap_dir = data_dir.join("bootstrap");
    std::fs::create_dir_all(&bootstrap_dir)
        .map_err(|e| CliError::Config(format!("failed to create bootstrap dir: {e}")))?;

    // Write the conversation transcript
    let transcript_path = bootstrap_dir.join("first-contact.json");
    let transcript = serde_json::json!({
        "timestamp": chrono::Utc::now().to_rfc3339(),
        "messages": contact.messages.iter().map(|m| {
            serde_json::json!({
                "role": format!("{:?}", m.role).to_lowercase(),
                "content": m.content,
            })
        }).collect::<Vec<_>>(),
        "total_tokens_in": contact.total_tokens_in,
        "total_tokens_out": contact.total_tokens_out,
    });
    std::fs::write(
        &transcript_path,
        serde_json::to_string_pretty(&transcript).unwrap(),
    )
    .map_err(|e| CliError::Config(format!("failed to write transcript: {e}")))?;

    // Write the extracted identity
    let identity_path = bootstrap_dir.join("identity.json");
    std::fs::write(
        &identity_path,
        serde_json::to_string_pretty(&identity).unwrap(),
    )
    .map_err(|e| CliError::Config(format!("failed to write identity: {e}")))?;

    println!("  Written: {}", transcript_path.display());
    println!("  Written: {}", identity_path.display());
    Ok(())
}

/// Escape a string for TOML output.
fn toml_string_escape(s: &str) -> String {
    // Use basic TOML string with escape sequences
    let escaped = s
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\t', "\\t");
    format!("\"{escaped}\"")
}

fn print_banner() {
    println!();
    println!("  ╔══════════════════════════════════════════════════════════╗");
    println!("  ║           Exoskeleton — Vessel Bootstrap                ║");
    println!("  ╚══════════════════════════════════════════════════════════╝");
    println!();
    println!("  This wizard will configure and initialize a new Vessel.");
    println!();
}

fn print_step(n: u32, title: &str) {
    println!("  Step {n}: {title}");
}

/// Extract the first JSON object from a string that may contain markdown
/// code fences or other surrounding text.
///
/// Handles common LLM response patterns:
/// - ```json\n{...}\n```
/// - ```\n{...}\n```
/// - Preamble text {..} trailing text
fn extract_json_object(s: &str) -> Option<&str> {
    // Find the first '{' and the last matching '}'
    let start = s.find('{')?;
    let end = s.rfind('}')?;
    if end > start {
        Some(&s[start..=end])
    } else {
        None
    }
}

/// Extracted vessel identity from the first-contact conversation.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct VesselIdentity {
    pub vessel_name: String,
    pub mission: String,
    pub user_name: Option<String>,
    pub user_summary: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_json_from_markdown_fences() {
        let input = "```json\n{\"vessel_name\": \"Atlas\"}\n```";
        assert_eq!(
            extract_json_object(input),
            Some("{\"vessel_name\": \"Atlas\"}")
        );
    }

    #[test]
    fn extract_json_bare() {
        let input = "{\"vessel_name\": \"Atlas\"}";
        assert_eq!(
            extract_json_object(input),
            Some("{\"vessel_name\": \"Atlas\"}")
        );
    }

    #[test]
    fn extract_json_with_preamble() {
        let input = "Here is the result:\n{\"vessel_name\": \"Atlas\"}\n";
        assert_eq!(
            extract_json_object(input),
            Some("{\"vessel_name\": \"Atlas\"}")
        );
    }

    #[test]
    fn extract_json_no_object() {
        assert_eq!(extract_json_object("no json here"), None);
    }
}

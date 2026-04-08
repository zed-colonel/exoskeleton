//! `exo bench` — benchmark harness CLI (E10-S3, W-104/W-105).
//!
//! Runs a suite of coding tasks headlessly and reports metrics.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use chrono::Utc;
use exoskeleton_host::benchmark::{
    format_comparison, format_report, format_step_verbose, load_suite, load_task_spec,
    prepare_workspace, run_verification, ContextUtilization, HeadlessRunner, SuiteResult,
    TaskResult, TokenMetrics,
};

use exoskeleton_host::config::{FrontierModelConfig, FrontierProvider, LocalApiFormat, LocalModelConfig};
use exoskeleton_host::VesselConfig;

use crate::client::CliError;

/// Run the `exo bench` command.
#[allow(clippy::too_many_arguments)]
pub async fn run_bench(
    task_path: Option<String>,
    suite_path: Option<String>,
    config_path: Option<String>,
    record: Option<String>,
    compare: Option<String>,
    results_dir: Option<String>,
    dry_run: bool,
    verbose: bool,
    provider: Option<String>,
    model: Option<String>,
    api_key_env: Option<String>,
    local_endpoint: Option<String>,
) -> Result<(), CliError> {
    let results_dir = results_dir
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("benchmarks/results"));

    let specs = if let Some(suite) = suite_path {
        load_suite(Path::new(&suite)).map_err(CliError::Other)?
    } else if let Some(task) = task_path {
        let path = PathBuf::from(task);
        let spec = load_task_spec(&path).map_err(CliError::Other)?;
        vec![(path, spec)]
    } else {
        return Err(CliError::Other(
            "specify --task <file.toml> or --suite <dir/>".into(),
        ));
    };

    let live_mode = config_path.is_some() && !dry_run;

    let base_config = if live_mode {
        let config_file = config_path.as_ref().expect("checked above");
        let toml_str = std::fs::read_to_string(config_file)
            .map_err(|e| CliError::Other(format!("cannot read config {config_file}: {e}")))?;
        let vessel_config_file: exoskeleton_host::VesselConfigFile = toml::from_str(&toml_str)
            .map_err(|e| CliError::Other(format!("invalid config: {e}")))?;
        let vessel_config = exoskeleton_host::VesselConfig::try_from(vessel_config_file)
            .map_err(|e| CliError::Other(format!("config validation failed: {e}")))?;
        Some(vessel_config)
    } else {
        None
    };

    // Apply CLI overrides to LLM config
    let base_config = base_config.map(|config| {
        apply_llm_overrides(
            config,
            provider.as_deref(),
            model.as_deref(),
            api_key_env.as_deref(),
            local_endpoint.as_deref(),
        )
    }).transpose()?;

    if live_mode {
        eprintln!(
            "Live mode: {} task(s), config: {}",
            specs.len(),
            config_path.as_deref().unwrap_or("?")
        );
    } else {
        eprintln!(
            "Dry-run mode: {} task(s) (no agent, harness validation only)",
            specs.len()
        );
    }

    let run_id = uuid::Uuid::new_v4().to_string()[..8].to_string();
    let mut task_results = Vec::new();

    for (_path, spec) in &specs {
        eprintln!("\n--- {} ---", spec.task.name);

        let result = if let Some(ref config) = base_config {
            HeadlessRunner::run_task(spec, config)
                .await
                .map_err(CliError::Other)?
        } else {
            run_benchmark_task_dry(spec).await?
        };

        let status = if result.passed { "PASS" } else { "FAIL" };
        eprintln!(
            "  {} ({} steps, {:.1}s, {} tok, {})",
            status,
            result.steps_taken,
            result.wall_time_secs,
            result.tokens.total,
            result.completion_reason
        );

        if verbose {
            for step in &result.step_trace {
                eprintln!("{}", format_step_verbose(step));
            }
        }

        task_results.push(result);
    }

    let suite_result = SuiteResult {
        run_id: run_id.clone(),
        timestamp: Utc::now(),
        tasks: task_results,
    };

    eprintln!("\n{}", format_report(&suite_result));

    if let Some(label) = record {
        std::fs::create_dir_all(&results_dir)
            .map_err(|e| CliError::Other(format!("cannot create results dir: {e}")))?;
        let record_path = results_dir.join(format!("{label}.json"));
        let json = serde_json::to_string_pretty(&suite_result)
            .map_err(|e| CliError::Other(format!("failed to serialize results: {e}")))?;
        std::fs::write(&record_path, json).map_err(|e| {
            CliError::Other(format!("failed to write {}: {e}", record_path.display()))
        })?;
        eprintln!("Recorded to {}", record_path.display());
    }

    if let Some(label) = compare {
        let baseline_path = results_dir.join(format!("{label}.json"));
        let baseline_json = std::fs::read_to_string(&baseline_path).map_err(|e| {
            CliError::Other(format!(
                "cannot read baseline {}: {e}",
                baseline_path.display()
            ))
        })?;
        let baseline: SuiteResult = serde_json::from_str(&baseline_json)
            .map_err(|e| CliError::Other(format!("invalid baseline JSON: {e}")))?;
        eprintln!("{}", format_comparison(&baseline, &suite_result));

        if suite_result.pass_rate() < baseline.pass_rate() {
            return Err(CliError::Other(format!(
                "REGRESSION: pass rate dropped from {:.0}% to {:.0}%",
                baseline.pass_rate() * 100.0,
                suite_result.pass_rate() * 100.0,
            )));
        }
    }

    Ok(())
}

/// Apply CLI flag overrides to the vessel's LLM configuration.
///
/// Supports three patterns:
/// 1. Frontier override: `--provider anthropic --model claude-sonnet-4-20250514 --api-key-env MY_KEY`
/// 2. Local override: `--local-endpoint http://localhost:11434 --model llama3.2:latest`
/// 3. Mix: `--provider openai --model gpt-4o --api-key-env OPENAI_API_KEY` (uses OpenAI-compat)
///
/// When `--local-endpoint` is provided without `--provider`, defaults to "ollama".
fn apply_llm_overrides(
    mut config: VesselConfig,
    provider: Option<&str>,
    model: Option<&str>,
    api_key_env: Option<&str>,
    local_endpoint: Option<&str>,
) -> Result<VesselConfig, CliError> {
    use exoskeleton_core::llm::LlmBackend;

    // If local-endpoint is provided, configure a local backend
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

    // If provider is specified (without local-endpoint), configure a frontier backend
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

    // If only --model is specified, update the current default backend's model
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

    // If only --api-key-env is specified, update the frontier config's key env
    if let Some(key_env) = api_key_env {
        if let Some(ref mut f) = config.llm_config.frontier {
            f.api_key_env = key_env.into();
        }
    }

    Ok(config)
}

/// Default model for a given frontier provider.
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

/// Default API key environment variable for a given provider.
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

/// Dry-run mode: validate harness without agent execution.
async fn run_benchmark_task_dry(
    spec: &exoskeleton_host::benchmark::TaskSpec,
) -> Result<exoskeleton_host::benchmark::TaskResult, CliError> {
    let start = std::time::Instant::now();

    let repo_path = PathBuf::from(&spec.task.repo_path);
    if !repo_path.exists() {
        return Err(CliError::Other(format!(
            "repo path does not exist: {}",
            repo_path.display()
        )));
    }

    let temp_dir = prepare_workspace(&repo_path)
        .map_err(|e| CliError::Other(format!("failed to prepare workspace: {e}")))?;

    let (passed, actual_exit_code) = run_verification(
        &spec.verify.command,
        spec.verify.expected_exit_code,
        temp_dir.path(),
    )
    .map_err(|e| CliError::Other(format!("verification failed: {e}")))?;

    Ok(TaskResult {
        task_name: spec.task.name.clone(),
        passed,
        verification_exit_code: Some(actual_exit_code),
        completion_reason: "DryRun".into(),
        wall_time_secs: start.elapsed().as_secs_f64(),
        steps_taken: 0,
        ticks_used: 0,
        tokens: TokenMetrics::default(),
        model_used: String::new(),
        tool_calls: HashMap::new(),
        files_modified: vec![],
        lines_added: 0,
        lines_removed: 0,
        doom_loop_corrections: 0,
        llm_cost_cents: 0.0,
        llm_calls: 0,
        step_trace: vec![],
        context_utilization: ContextUtilization::default(),
        tick_details: vec![],
        difficulty: spec.task.difficulty.clone(),
        language: spec.task.language.clone(),
        tags: spec.task.tags.clone(),
        source_benchmark: spec.task.source_benchmark.clone(),
        source_id: spec.task.source_id.clone(),
        timestamp: Utc::now(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_task_result() -> TaskResult {
        TaskResult {
            task_name: "add-test".into(),
            passed: true,
            verification_exit_code: Some(0),
            completion_reason: "AgentComplete".into(),
            wall_time_secs: 3.5,
            steps_taken: 8,
            ticks_used: 1,
            tokens: TokenMetrics {
                total_in: 1600,
                total_out: 400,
                total: 2000,
                by_phase: HashMap::new(),
            },
            model_used: "claude-sonnet-4-20250514".into(),
            tool_calls: HashMap::new(),
            files_modified: vec!["src/lib.rs".into()],
            lines_added: 10,
            lines_removed: 0,
            doom_loop_corrections: 0,
            llm_cost_cents: 1.5,
            llm_calls: 2,
            step_trace: vec![],
            context_utilization: ContextUtilization::default(),
            tick_details: vec![],
            difficulty: Some("easy".into()),
            language: Some("rust".into()),
            tags: vec![],
            source_benchmark: None,
            source_id: None,
            timestamp: Utc::now(),
        }
    }

    #[test]
    fn format_report_basic() {
        let suite = SuiteResult {
            run_id: "test-123".into(),
            timestamp: Utc::now(),
            tasks: vec![make_task_result()],
        };

        let report = format_report(&suite);
        assert!(report.contains("add-test"));
        assert!(report.contains("PASS"));
        assert!(report.contains("100%"));
    }
}

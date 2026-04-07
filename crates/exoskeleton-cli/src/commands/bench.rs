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

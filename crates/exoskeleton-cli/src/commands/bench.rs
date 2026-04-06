//! `exo bench` — benchmark harness CLI (E10-S3, W-104/W-105).
//!
//! Runs a suite of coding tasks headlessly and reports metrics.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use chrono::Utc;
use exoskeleton_host::benchmark::{
    format_comparison, format_report, load_suite, load_task_spec, prepare_workspace,
    run_verification, SuiteResult, TaskResult,
};

use crate::client::CliError;

/// Run the `exo bench` command.
pub async fn run_bench(
    task_path: Option<String>,
    suite_path: Option<String>,
    record: Option<String>,
    compare: Option<String>,
    results_dir: Option<String>,
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

    eprintln!("Loaded {} task(s)", specs.len());

    let run_id = uuid::Uuid::new_v4().to_string()[..8].to_string();
    let mut task_results = Vec::new();

    for (_path, spec) in &specs {
        eprintln!("\n--- {} ---", spec.task.name);
        let result = run_benchmark_task(spec, &results_dir).await?;
        let status = if result.passed { "PASS" } else { "FAIL" };
        eprintln!(
            "  {} ({} steps, {:.1}s, {})",
            status, result.steps_taken, result.wall_time_secs, result.completion_reason
        );
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

/// Run a single benchmark task.
///
/// This sprint validates the harness infrastructure only:
/// workspace isolation, verification, metrics, recording, and comparison.
async fn run_benchmark_task(
    spec: &exoskeleton_host::benchmark::TaskSpec,
    _results_dir: &Path,
) -> Result<TaskResult, CliError> {
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

    let passed = run_verification(
        &spec.verify.command,
        spec.verify.expected_exit_code,
        temp_dir.path(),
    )
    .map_err(|e| CliError::Other(format!("verification failed: {e}")))?;

    Ok(TaskResult {
        task_name: spec.task.name.clone(),
        passed,
        verification_exit_code: Some(if passed {
            spec.verify.expected_exit_code
        } else {
            1
        }),
        steps_taken: 0,
        tokens_consumed: 0,
        wall_time_secs: start.elapsed().as_secs_f64(),
        completion_reason: "HarnessOnly".into(),
        files_modified: 0,
        lines_added: 0,
        lines_removed: 0,
        tool_calls: HashMap::new(),
        doom_loop_corrections: 0,
        timestamp: Utc::now(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_report_basic() {
        let suite = SuiteResult {
            run_id: "test-123".into(),
            timestamp: Utc::now(),
            tasks: vec![TaskResult {
                task_name: "add-test".into(),
                passed: true,
                verification_exit_code: Some(0),
                steps_taken: 8,
                tokens_consumed: 2000,
                wall_time_secs: 3.5,
                completion_reason: "AgentComplete".into(),
                files_modified: 1,
                lines_added: 10,
                lines_removed: 0,
                tool_calls: HashMap::new(),
                doom_loop_corrections: 0,
                timestamp: Utc::now(),
            }],
        };

        let report = format_report(&suite);
        assert!(report.contains("add-test"));
        assert!(report.contains("PASS"));
        assert!(report.contains("100%"));
    }
}

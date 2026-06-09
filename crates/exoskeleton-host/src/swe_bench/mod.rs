//! SWE-bench adapter: dataset fetching, repo management, evaluation.
//!
//! Supports Rust-SWE-bench (500 tasks from 34 repos) and SWE-bench Multilingual
//! (43 Rust tasks from 7 repos). Tasks are fetched from HuggingFace, repos are
//! cached as bare clones, and each instance gets an isolated git worktree.

pub mod dataset;
pub mod eval;
pub mod repo;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::benchmark::{
    apply_coding_benchmark_overrides, boot_vessel, extract_metrics, inject_and_poll,
    log_diagnostics, ContextUtilization, TaskResult, TokenMetrics,
};
use crate::config::VesselConfig;

/// Which SWE-bench dataset to fetch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DatasetSource {
    /// Rust-SWE-bench: 500 Rust tasks from 34 repos.
    /// HuggingFace: user2f86/rustbench
    RustBench,
    /// SWE-bench Multilingual: 300 tasks across 9 languages.
    /// Filtered to Rust only (43 tasks from 7 repos).
    /// HuggingFace: SWE-bench/SWE-bench_Multilingual
    Multilingual,
}

/// A single SWE-bench task instance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SweInstance {
    pub instance_id: String,
    pub repo: String,
    pub base_commit: String,
    pub problem_statement: String,
    #[serde(default)]
    pub hints_text: String,
    pub patch: String,
    pub test_patch: String,
    #[serde(alias = "FAIL_TO_PASS")]
    pub fail_to_pass: Vec<String>,
    #[serde(alias = "PASS_TO_PASS")]
    pub pass_to_pass: Vec<String>,
}

/// SWE-bench grading result (re-exported from eval).
pub use eval::SweGrading;

/// Result of running a single SWE-bench instance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SweResult {
    pub task_result: TaskResult,
    pub instance_id: String,
    /// The agent's diff, captured before test_patch application.
    pub model_patch: String,
    pub fail_to_pass_resolved: Vec<String>,
    pub pass_to_pass_maintained: Vec<String>,
    pub grading: SweGrading,
}

/// Aggregated results for a SWE-bench run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SweSuiteResult {
    pub run_id: String,
    pub timestamp: DateTime<Utc>,
    pub dataset: String,
    pub results: Vec<SweResult>,
}

impl SweSuiteResult {
    pub fn resolve_rate(&self) -> f64 {
        if self.results.is_empty() {
            return 0.0;
        }
        let resolved = self.results.iter().filter(|r| r.grading.resolved).count();
        resolved as f64 / self.results.len() as f64
    }
}

/// Options for a SWE-bench run.
#[derive(Debug, Clone)]
pub struct SweRunOptions {
    pub source: DatasetSource,
    pub limit: Option<usize>,
    pub instance_filter: Option<String>,
    pub refresh_dataset: bool,
    pub cache_dir: PathBuf,
    pub repos_cache: PathBuf,
    pub test_timeout: std::time::Duration,
    pub max_ticks: u32,
    pub verbose: bool,
}

impl Default for SweRunOptions {
    fn default() -> Self {
        let cache_base = dirs::cache_dir()
            .unwrap_or_else(|| PathBuf::from("/tmp"))
            .join("exo-bench");
        Self {
            source: DatasetSource::RustBench,
            limit: None,
            instance_filter: None,
            refresh_dataset: false,
            cache_dir: cache_base.join("datasets"),
            repos_cache: cache_base.join("repos"),
            test_timeout: std::time::Duration::from_secs(300),
            max_ticks: 5,
            verbose: false,
        }
    }
}

/// Orchestrates SWE-bench benchmark execution.
pub struct SweBenchRunner;

impl SweBenchRunner {
    /// Run a SWE-bench benchmark suite.
    pub async fn run(
        base_config: &VesselConfig,
        options: &SweRunOptions,
    ) -> Result<SweSuiteResult, String> {
        // 1. Fetch dataset (blocking HTTP, must run off the async runtime)
        let source = options.source;
        let cache_dir = options.cache_dir.clone();
        let refresh = options.refresh_dataset;
        let mut instances = tokio::task::spawn_blocking(move || {
            dataset::fetch_dataset(source, &cache_dir, refresh)
        })
        .await
        .map_err(|e| format!("dataset fetch task panicked: {e}"))?
        .map_err(|e| format!("dataset fetch failed: {e}"))?;

        // 2. Apply filters
        if let Some(ref filter) = options.instance_filter {
            instances.retain(|i| i.instance_id == *filter);
            if instances.is_empty() {
                return Err(format!("no instance matching '{filter}'"));
            }
        }
        if let Some(limit) = options.limit {
            instances.truncate(limit);
        }

        eprintln!("SWE-bench: {} instances to run", instances.len());

        // 3. Run each instance
        let repo_cache = repo::RepoCache::new(options.repos_cache.clone());
        let run_id = uuid::Uuid::new_v4().to_string()[..8].to_string();
        let mut results = Vec::new();

        for (i, instance) in instances.iter().enumerate() {
            eprintln!(
                "\n--- [{}/{}] {} ---",
                i + 1,
                instances.len(),
                instance.instance_id
            );

            match Self::run_instance(base_config, &repo_cache, instance, options).await {
                Ok(result) => {
                    let status = if result.grading.resolved {
                        "RESOLVED"
                    } else {
                        "FAILED"
                    };
                    eprintln!(
                        "  {} (F2P: {}/{}, P2P: {}/{}, {} steps, {:.1}s, {} tok)",
                        status,
                        result.grading.f2p_passed,
                        result.grading.f2p_total,
                        result.grading.p2p_passed,
                        result.grading.p2p_total,
                        result.task_result.steps_taken,
                        result.task_result.wall_time_secs,
                        result.task_result.tokens.total,
                    );
                    results.push(result);
                }
                Err(e) => {
                    eprintln!("  ERROR: {e}");
                    results.push(Self::error_result(instance, &e));
                }
            }
        }

        let dataset_name = match options.source {
            DatasetSource::RustBench => "rustbench",
            DatasetSource::Multilingual => "multilingual-rust",
        };

        Ok(SweSuiteResult {
            run_id,
            timestamp: chrono::Utc::now(),
            dataset: dataset_name.into(),
            results,
        })
    }

    async fn run_instance(
        base_config: &VesselConfig,
        repo_cache: &repo::RepoCache,
        instance: &SweInstance,
        options: &SweRunOptions,
    ) -> Result<SweResult, String> {
        let start = std::time::Instant::now();

        // 1-2. Prepare worktree at base_commit
        let worktree = repo_cache
            .prepare_worktree(&instance.repo, &instance.base_commit)
            .map_err(|e| format!("worktree preparation failed: {e}"))?;
        let workspace = worktree.path();

        // 3. Build vessel config with workspace-scoped coding benchmark overrides
        let config = Self::build_vessel_config(base_config, workspace);

        // 4. Boot vessel
        let vessel = boot_vessel(config).await?;

        // 5-6. Inject prompt and poll
        // Outer timeout: allow enough time for all ticks to complete.
        // A coding tick may include multiple tool actions across the coding
        // exec thread, master Decide, and Act. Some tools like cargo build can
        // take minutes. Use 30 min per tick as a generous bound.
        // The actual termination signal is max_ticks or exec-thread completion;
        // this timeout is just a safety net against hangs.
        let per_tick_budget_secs = 1800_u64;
        let timeout =
            std::time::Duration::from_secs(per_tick_budget_secs * options.max_ticks as u64 + 120);
        let (mut completion_reason, inspector) = inject_and_poll(
            &vessel,
            &instance.problem_statement,
            workspace,
            timeout,
            Some(options.max_ticks),
        )
        .await?;

        // 8. Shutdown
        vessel
            .shutdown()
            .await
            .map_err(|e| format!("shutdown failed: {e}"))?;

        // 7. Extract metrics
        if options.verbose {
            log_diagnostics(&inspector);
        }
        let metrics = extract_metrics(&inspector).unwrap_or_default();

        for anomaly in &metrics.harness_anomalies {
            eprintln!("  [warn] harness anomaly: {anomaly}");
        }

        // 9. Extract agent's diff (before applying test_patch)
        let model_patch = Self::extract_diff(workspace)?;

        // 10. Apply test_patch
        if !instance.test_patch.is_empty() {
            if let Err(e) = eval::apply_patch(workspace, &instance.test_patch) {
                eprintln!("  [warn] test_patch application failed: {e}");
            }
        }

        // 11. Run targeted FAIL_TO_PASS tests. If grading infrastructure fails,
        // keep the agent's diff and metrics instead of collapsing the run into
        // an adapter error.
        let test_output = match eval::run_cargo_fail_to_pass_tests(
            workspace,
            &model_patch,
            &instance.fail_to_pass,
            options.test_timeout,
        ) {
            Ok(output) => Some(output),
            Err(e) => {
                let error = format!("cargo test failed: {e}");
                eprintln!("  [warn] {error}");
                completion_reason = format!("{completion_reason}; GradingError: {error}");
                None
            }
        };

        // 12. Grade, or synthesize an unresolved grading result when test
        // execution failed before producing usable output.
        let grading = test_output.as_ref().map_or_else(
            || SweGrading {
                resolved: false,
                f2p_passed: 0,
                f2p_total: instance.fail_to_pass.len() as u32,
                p2p_passed: 0,
                p2p_total: instance.pass_to_pass.len() as u32,
            },
            |output| {
                eval::grade(
                    &output.tests,
                    &instance.fail_to_pass,
                    &instance.pass_to_pass,
                )
            },
        );

        // Determine which tests resolved/maintained.
        let f2p_resolved: Vec<String> = test_output.as_ref().map_or_else(Vec::new, |output| {
            instance
                .fail_to_pass
                .iter()
                .filter(|name| {
                    eval::lookup_test_pub(&output.tests, name) == Some(eval::TestOutcome::Passed)
                })
                .cloned()
                .collect()
        });
        let p2p_maintained: Vec<String> = test_output.as_ref().map_or_else(Vec::new, |output| {
            instance
                .pass_to_pass
                .iter()
                .filter(|name| {
                    matches!(
                        eval::lookup_test_pub(&output.tests, name),
                        Some(eval::TestOutcome::Passed) | None
                    )
                })
                .cloned()
                .collect()
        });
        let verification_exit_code = test_output.as_ref().map(|output| output.exit_code);

        // Preserve worktree on failure
        if !grading.resolved {
            let preserved = worktree.keep();
            eprintln!(
                "  [diag] FAILED — workspace preserved at: {}",
                preserved.display()
            );
        }

        Ok(SweResult {
            task_result: TaskResult {
                task_name: instance.instance_id.clone(),
                passed: grading.resolved,
                verification_exit_code,
                completion_reason,
                wall_time_secs: start.elapsed().as_secs_f64(),
                steps_taken: metrics.steps_taken,
                ticks_used: metrics.ticks_used,
                tokens: metrics.tokens,
                model_used: metrics.model_used,
                tool_calls: metrics.tool_calls,
                files_modified: metrics.files_modified,
                lines_added: metrics.lines_added,
                lines_removed: metrics.lines_removed,
                doom_loop_corrections: metrics.doom_loop_corrections,
                llm_cost_cents: metrics.llm_cost_cents,
                llm_calls: metrics.llm_calls,
                step_trace: metrics.step_trace,
                context_utilization: metrics.context_utilization,
                tick_details: metrics.tick_details,
                behavior_metrics: metrics.behavior_metrics,
                harness_anomalies: metrics.harness_anomalies,
                difficulty: None,
                language: Some("rust".into()),
                tags: vec![],
                source_benchmark: Some("swe-bench".into()),
                source_id: Some(instance.instance_id.clone()),
                timestamp: chrono::Utc::now(),
            },
            instance_id: instance.instance_id.clone(),
            model_patch,
            fail_to_pass_resolved: f2p_resolved,
            pass_to_pass_maintained: p2p_maintained,
            grading,
        })
    }

    /// Build VesselConfig with forced benchmark overrides.
    fn build_vessel_config(base: &VesselConfig, workspace: &Path) -> VesselConfig {
        let mut config = base.clone();
        let data_dir = workspace.join(".exo-bench");
        // Create the data dir (ignore errors — vessel boot will fail if it can't access it)
        let _ = std::fs::create_dir_all(&data_dir);
        apply_coding_benchmark_overrides(&mut config, workspace, &data_dir);
        config
    }

    /// Extract the agent's diff from a workspace using `git diff`.
    fn extract_diff(workspace: &Path) -> Result<String, String> {
        let output = std::process::Command::new("git")
            .args(["diff", "HEAD"])
            .current_dir(workspace)
            .output()
            .map_err(|e| format!("git diff failed: {e}"))?;
        if !output.status.success() {
            return Err(format!(
                "git diff exited {}: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    fn error_result(instance: &SweInstance, error: &str) -> SweResult {
        SweResult {
            task_result: TaskResult {
                task_name: instance.instance_id.clone(),
                passed: false,
                verification_exit_code: None,
                completion_reason: format!("Error: {error}"),
                wall_time_secs: 0.0,
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
                behavior_metrics: crate::benchmark::BehaviorMetrics::default(),
                harness_anomalies: vec![],
                difficulty: None,
                language: Some("rust".into()),
                tags: vec![],
                source_benchmark: Some("swe-bench".into()),
                source_id: Some(instance.instance_id.clone()),
                timestamp: chrono::Utc::now(),
            },
            instance_id: instance.instance_id.clone(),
            model_patch: String::new(),
            fail_to_pass_resolved: vec![],
            pass_to_pass_maintained: vec![],
            grading: SweGrading {
                resolved: false,
                f2p_passed: 0,
                f2p_total: instance.fail_to_pass.len() as u32,
                p2p_passed: 0,
                p2p_total: instance.pass_to_pass.len() as u32,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn swe_instance_deserialize_from_hf_row() {
        let json = r#"{
            "instance_id": "tokio-rs__tokio-4384",
            "repo": "tokio-rs/tokio",
            "base_commit": "abc123def456",
            "problem_statement": "Fix the timeout bug",
            "hints_text": "Check the timer wheel",
            "patch": "diff --git a/src/time.rs b/src/time.rs",
            "test_patch": "diff --git a/tests/time.rs b/tests/time.rs",
            "FAIL_TO_PASS": ["time::tests::test_timeout"],
            "PASS_TO_PASS": ["time::tests::test_sleep"]
        }"#;
        let instance: SweInstance = serde_json::from_str(json).unwrap();
        assert_eq!(instance.instance_id, "tokio-rs__tokio-4384");
        assert_eq!(instance.fail_to_pass, vec!["time::tests::test_timeout"]);
        assert_eq!(instance.pass_to_pass, vec!["time::tests::test_sleep"]);
    }

    #[test]
    fn swe_suite_result_resolve_rate() {
        let result = SweSuiteResult {
            run_id: "test".into(),
            timestamp: Utc::now(),
            dataset: "rustbench".into(),
            results: vec![],
        };
        assert_eq!(result.resolve_rate(), 0.0);
    }
}

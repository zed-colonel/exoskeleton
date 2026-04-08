//! SWE-bench adapter: dataset fetching, repo management, evaluation.
//!
//! Supports Rust-SWE-bench (500 tasks from 34 repos) and SWE-bench Multilingual
//! (43 Rust tasks from 7 repos). Tasks are fetched from HuggingFace, repos are
//! cached as bare clones, and each instance gets an isolated git worktree.

pub mod dataset;
pub mod eval;
pub mod repo;

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::benchmark::TaskResult;

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
            verbose: false,
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

//! Benchmark harness types and utilities (E10-S3, W-104/W-105).
//!
//! Provides task spec parsing, workspace preparation, verification execution,
//! and report/comparison formatting for headless coding benchmarks.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A benchmark task specification parsed from TOML.
#[derive(Debug, Clone, Deserialize)]
pub struct TaskSpec {
    pub task: TaskSection,
    pub verify: VerifySection,
}

/// The `[task]` section of a benchmark spec.
#[derive(Debug, Clone, Deserialize)]
pub struct TaskSection {
    pub name: String,
    pub repo_path: String,
    pub prompt: String,
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,
    #[serde(default = "default_max_steps")]
    pub max_steps: u32,
}

fn default_timeout() -> u64 {
    600
}

fn default_max_steps() -> u32 {
    50
}

/// The `[verify]` section of a benchmark spec.
#[derive(Debug, Clone, Deserialize)]
pub struct VerifySection {
    pub command: String,
    #[serde(default)]
    pub expected_exit_code: i32,
}

/// Result of running a single benchmark task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskResult {
    pub task_name: String,
    pub passed: bool,
    pub verification_exit_code: Option<i32>,
    pub steps_taken: u32,
    pub tokens_consumed: u64,
    pub wall_time_secs: f64,
    pub completion_reason: String,
    pub files_modified: u32,
    pub lines_added: i64,
    pub lines_removed: i64,
    pub tool_calls: HashMap<String, ToolCallStats>,
    pub doom_loop_corrections: u32,
    pub timestamp: DateTime<Utc>,
}

/// Per-tool call statistics.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ToolCallStats {
    pub calls: u32,
    pub successes: u32,
    pub failures: u32,
}

/// Aggregate results for a benchmark suite run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SuiteResult {
    pub run_id: String,
    pub timestamp: DateTime<Utc>,
    pub tasks: Vec<TaskResult>,
}

impl SuiteResult {
    pub fn pass_rate(&self) -> f64 {
        if self.tasks.is_empty() {
            return 0.0;
        }
        let passed = self.tasks.iter().filter(|task| task.passed).count();
        passed as f64 / self.tasks.len() as f64
    }

    pub fn median_steps(&self) -> u32 {
        median_u32(
            &self
                .tasks
                .iter()
                .map(|task| task.steps_taken)
                .collect::<Vec<_>>(),
        )
    }

    pub fn median_tokens(&self) -> u64 {
        median_u64(
            &self
                .tasks
                .iter()
                .map(|task| task.tokens_consumed)
                .collect::<Vec<_>>(),
        )
    }

    pub fn median_time(&self) -> f64 {
        median_f64(
            &self
                .tasks
                .iter()
                .map(|task| task.wall_time_secs)
                .collect::<Vec<_>>(),
        )
    }
}

fn median_u32(values: &[u32]) -> u32 {
    if values.is_empty() {
        return 0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    sorted[sorted.len() / 2]
}

fn median_u64(values: &[u64]) -> u64 {
    if values.is_empty() {
        return 0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    sorted[sorted.len() / 2]
}

fn median_f64(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    sorted[sorted.len() / 2]
}

/// Run a verification command in a workspace directory.
pub fn run_verification<P: AsRef<Path>>(
    command: &str,
    expected_exit_code: i32,
    workspace: P,
) -> Result<bool, std::io::Error> {
    let output = std::process::Command::new("sh")
        .args(["-c", command])
        .current_dir(workspace)
        .output()?;

    let actual = output.status.code().unwrap_or(-1);
    Ok(actual == expected_exit_code)
}

/// Recursively copy a directory tree.
pub fn copy_directory(src: &Path, dst: &Path) -> Result<(), std::io::Error> {
    if !dst.exists() {
        std::fs::create_dir_all(dst)?;
    }

    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if src_path.is_dir() {
            copy_directory(&src_path, &dst_path)?;
        } else {
            std::fs::copy(&src_path, &dst_path)?;
        }
    }

    Ok(())
}

/// Load a TaskSpec from a TOML file.
pub fn load_task_spec(path: &Path) -> Result<TaskSpec, String> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| format!("failed to read {}: {e}", path.display()))?;
    let mut spec: TaskSpec = toml::from_str(&content)
        .map_err(|e| format!("invalid task spec {}: {e}", path.display()))?;

    let repo_path = PathBuf::from(&spec.task.repo_path);
    if !repo_path.is_absolute() {
        let spec_dir = path.parent().unwrap_or_else(|| Path::new("."));
        let candidate_from_spec = spec_dir.join(&spec.task.repo_path);
        let resolved = if candidate_from_spec.exists() {
            candidate_from_spec
        } else {
            std::env::current_dir()
                .map(|cwd| cwd.join(&spec.task.repo_path))
                .unwrap_or(candidate_from_spec)
        };

        spec.task.repo_path = resolved
            .canonicalize()
            .unwrap_or(resolved)
            .to_string_lossy()
            .to_string();
    }

    Ok(spec)
}

/// Load all task specs from a directory.
pub fn load_suite(dir: &Path) -> Result<Vec<(PathBuf, TaskSpec)>, String> {
    let mut specs = Vec::new();
    let entries = std::fs::read_dir(dir)
        .map_err(|e| format!("failed to read suite dir {}: {e}", dir.display()))?;

    for entry in entries {
        let entry = entry.map_err(|e| format!("failed to read suite entry: {e}"))?;
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "toml") {
            specs.push((path.clone(), load_task_spec(&path)?));
        }
    }

    specs.sort_by(|a, b| a.1.task.name.cmp(&b.1.task.name));
    Ok(specs)
}

/// Prepare an isolated workspace by copying the repo to a temp directory.
pub fn prepare_workspace(repo_path: &Path) -> Result<tempfile::TempDir, std::io::Error> {
    let temp_dir = tempfile::tempdir()?;
    copy_directory(repo_path, temp_dir.path())?;
    Ok(temp_dir)
}

/// Format a suite result as a human-readable report.
pub fn format_report(result: &SuiteResult) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "Benchmark run: {} ({})\n",
        result.run_id,
        result.timestamp.format("%Y-%m-%d %H:%M:%S")
    ));
    out.push_str(&format!("{:-<70}\n", ""));
    out.push_str(&format!(
        "{:<40} {:>8} {:>8} {:>8}\n",
        "Task", "Result", "Steps", "Time"
    ));
    out.push_str(&format!("{:-<70}\n", ""));

    for task in &result.tasks {
        let status = if task.passed { "PASS" } else { "FAIL" };
        out.push_str(&format!(
            "{:<40} {:>8} {:>8} {:>7.1}s\n",
            task.task_name, status, task.steps_taken, task.wall_time_secs
        ));
    }

    out.push_str(&format!("{:-<70}\n", ""));
    out.push_str(&format!(
        "Pass rate: {:.0}%  |  Median steps: {}  |  Median tokens: {}  |  Median time: {:.1}s\n",
        result.pass_rate() * 100.0,
        result.median_steps(),
        result.median_tokens(),
        result.median_time(),
    ));
    out
}

/// Format a comparison between two suite results.
pub fn format_comparison(baseline: &SuiteResult, current: &SuiteResult) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "{:<30} {:>12} {:>12} {:>8}\n",
        "Task", "baseline", "current", "delta"
    ));
    out.push_str(&format!("{:-<66}\n", ""));

    let baseline_map: HashMap<&str, &TaskResult> = baseline
        .tasks
        .iter()
        .map(|task| (task.task_name.as_str(), task))
        .collect();

    for task in &current.tasks {
        let current_status = if task.passed { "PASS" } else { "FAIL" };
        let (baseline_str, delta) = match baseline_map.get(task.task_name.as_str()) {
            Some(baseline_task) => {
                let baseline_status = if baseline_task.passed { "PASS" } else { "FAIL" };
                let delta = if task.passed == baseline_task.passed {
                    let time_delta = task.wall_time_secs - baseline_task.wall_time_secs;
                    if time_delta.abs() < 0.5 {
                        "=".to_string()
                    } else {
                        format!("{:+.1}s", time_delta)
                    }
                } else if task.passed {
                    "+1".to_string()
                } else {
                    "-1".to_string()
                };
                (
                    format!("{} ({:.0}s)", baseline_status, baseline_task.wall_time_secs),
                    delta,
                )
            }
            None => ("-".into(), "new".into()),
        };

        out.push_str(&format!(
            "{:<30} {:>12} {:>12} {:>8}\n",
            task.task_name,
            baseline_str,
            format!("{} ({:.0}s)", current_status, task.wall_time_secs),
            delta,
        ));
    }

    out.push_str(&format!("{:-<66}\n", ""));
    out.push_str(&format!(
        "{:<30} {:>12.0}% {:>12.0}% {:>+7.0}%\n",
        "Pass rate:",
        baseline.pass_rate() * 100.0,
        current.pass_rate() * 100.0,
        (current.pass_rate() - baseline.pass_rate()) * 100.0,
    ));
    out.push_str(&format!(
        "{:<30} {:>12} {:>12} {:>+8}\n",
        "Median steps:",
        baseline.median_steps(),
        current.median_steps(),
        current.median_steps() as i64 - baseline.median_steps() as i64,
    ));
    out.push_str(&format!(
        "{:<30} {:>12} {:>12} {:>+8}\n",
        "Median tokens:",
        baseline.median_tokens(),
        current.median_tokens(),
        current.median_tokens() as i64 - baseline.median_tokens() as i64,
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_task_spec_from_toml() {
        let toml_str = r#"
[task]
name = "add-test-for-parser"
repo_path = "./benchmarks/repos/sample-rust"
prompt = "Add a unit test for the parse_config function"
timeout_secs = 300
max_steps = 25

[verify]
command = "cargo test --lib test_parse_config"
expected_exit_code = 0
"#;
        let spec: TaskSpec = toml::from_str(toml_str).unwrap();
        assert_eq!(spec.task.name, "add-test-for-parser");
        assert_eq!(spec.task.timeout_secs, 300);
        assert_eq!(spec.task.max_steps, 25);
        assert_eq!(spec.verify.expected_exit_code, 0);
    }

    #[test]
    fn parse_task_spec_defaults() {
        let toml_str = r#"
[task]
name = "simple"
repo_path = "."
prompt = "Do something"

[verify]
command = "true"
"#;
        let spec: TaskSpec = toml::from_str(toml_str).unwrap();
        assert_eq!(spec.task.timeout_secs, 600);
        assert_eq!(spec.task.max_steps, 50);
        assert_eq!(spec.verify.expected_exit_code, 0);
    }

    #[test]
    fn suite_result_pass_rate() {
        let suite = SuiteResult {
            run_id: "test".into(),
            timestamp: Utc::now(),
            tasks: vec![
                TaskResult {
                    task_name: "a".into(),
                    passed: true,
                    verification_exit_code: Some(0),
                    steps_taken: 5,
                    tokens_consumed: 1000,
                    wall_time_secs: 2.0,
                    completion_reason: "AgentComplete".into(),
                    files_modified: 1,
                    lines_added: 10,
                    lines_removed: 2,
                    tool_calls: HashMap::new(),
                    doom_loop_corrections: 0,
                    timestamp: Utc::now(),
                },
                TaskResult {
                    task_name: "b".into(),
                    passed: false,
                    verification_exit_code: Some(1),
                    steps_taken: 25,
                    tokens_consumed: 5000,
                    wall_time_secs: 10.0,
                    completion_reason: "StepLimit".into(),
                    files_modified: 0,
                    lines_added: 0,
                    lines_removed: 0,
                    tool_calls: HashMap::new(),
                    doom_loop_corrections: 0,
                    timestamp: Utc::now(),
                },
            ],
        };
        assert!((suite.pass_rate() - 0.5).abs() < 0.01);
        assert_eq!(suite.median_steps(), 25);
        assert_eq!(suite.median_tokens(), 5000);
    }

    #[test]
    fn suite_result_empty() {
        let suite = SuiteResult {
            run_id: "empty".into(),
            timestamp: Utc::now(),
            tasks: vec![],
        };
        assert!(suite.pass_rate().abs() < 0.01);
        assert_eq!(suite.median_steps(), 0);
    }

    #[test]
    fn run_verification_command_success() {
        let result = run_verification("true", 0, std::env::temp_dir());
        assert!(result.is_ok());
        assert!(result.unwrap());
    }

    #[test]
    fn run_verification_command_failure() {
        let result = run_verification("false", 0, std::env::temp_dir());
        assert!(result.is_ok());
        assert!(!result.unwrap());
    }

    #[test]
    fn run_verification_custom_exit_code() {
        let result = run_verification("exit 42", 42, std::env::temp_dir());
        assert!(result.is_ok());
        assert!(result.unwrap());
    }

    #[test]
    fn copy_repo_to_temp() {
        let src = tempfile::tempdir().unwrap();
        std::fs::write(src.path().join("file.txt"), "hello").unwrap();
        std::fs::create_dir_all(src.path().join("sub")).unwrap();
        std::fs::write(src.path().join("sub/nested.txt"), "nested").unwrap();

        let dst = tempfile::tempdir().unwrap();
        copy_directory(src.path(), dst.path()).unwrap();

        assert!(dst.path().join("file.txt").exists());
        assert!(dst.path().join("sub/nested.txt").exists());
        assert_eq!(
            std::fs::read_to_string(dst.path().join("file.txt")).unwrap(),
            "hello"
        );
    }

    #[test]
    fn suite_result_serialization_roundtrip() {
        let suite = SuiteResult {
            run_id: "test-abc".into(),
            timestamp: Utc::now(),
            tasks: vec![TaskResult {
                task_name: "task-a".into(),
                passed: true,
                verification_exit_code: Some(0),
                steps_taken: 10,
                tokens_consumed: 3000,
                wall_time_secs: 5.0,
                completion_reason: "AgentComplete".into(),
                files_modified: 1,
                lines_added: 5,
                lines_removed: 2,
                tool_calls: HashMap::from([(
                    "code.edit".into(),
                    ToolCallStats {
                        calls: 3,
                        successes: 2,
                        failures: 1,
                    },
                )]),
                doom_loop_corrections: 0,
                timestamp: Utc::now(),
            }],
        };

        let json = serde_json::to_string(&suite).unwrap();
        let deserialized: SuiteResult = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.run_id, "test-abc");
        assert_eq!(deserialized.tasks.len(), 1);
        assert_eq!(deserialized.tasks[0].task_name, "task-a");
        assert!(deserialized.tasks[0].passed);
        assert_eq!(deserialized.tasks[0].tool_calls["code.edit"].calls, 3);
    }

    #[test]
    fn format_comparison_output() {
        let baseline = SuiteResult {
            run_id: "baseline".into(),
            timestamp: Utc::now(),
            tasks: vec![
                make_task_result("task-a", true, 10, 3000, 5.0),
                make_task_result("task-b", false, 25, 8000, 20.0),
            ],
        };
        let current = SuiteResult {
            run_id: "current".into(),
            timestamp: Utc::now(),
            tasks: vec![
                make_task_result("task-a", true, 8, 2500, 4.0),
                make_task_result("task-b", true, 15, 5000, 12.0),
            ],
        };

        let comparison = format_comparison(&baseline, &current);
        assert!(comparison.contains("task-a"));
        assert!(comparison.contains("task-b"));
        assert!(comparison.contains("Pass rate:"));
    }

    #[test]
    fn load_suite_from_directory() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("task1.toml"),
            r#"
[task]
name = "first"
repo_path = "."
prompt = "do something"

[verify]
command = "true"
"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("task2.toml"),
            r#"
[task]
name = "second"
repo_path = "."
prompt = "do another thing"

[verify]
command = "true"
"#,
        )
        .unwrap();
        std::fs::write(dir.path().join("readme.md"), "not a task").unwrap();

        let specs = load_suite(dir.path()).unwrap();
        assert_eq!(specs.len(), 2);
        assert_eq!(specs[0].1.task.name, "first");
        assert_eq!(specs[1].1.task.name, "second");
    }

    fn make_task_result(
        name: &str,
        passed: bool,
        steps: u32,
        tokens: u64,
        time: f64,
    ) -> TaskResult {
        TaskResult {
            task_name: name.into(),
            passed,
            verification_exit_code: Some(if passed { 0 } else { 1 }),
            steps_taken: steps,
            tokens_consumed: tokens,
            wall_time_secs: time,
            completion_reason: if passed {
                "AgentComplete".into()
            } else {
                "StepLimit".into()
            },
            files_modified: if passed { 1 } else { 0 },
            lines_added: 5,
            lines_removed: 2,
            tool_calls: HashMap::new(),
            doom_loop_corrections: 0,
            timestamp: Utc::now(),
        }
    }
}

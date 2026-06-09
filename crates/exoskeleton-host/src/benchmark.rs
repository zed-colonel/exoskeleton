//! Benchmark harness types and utilities (E10-S3, W-104/W-105).
//!
//! Provides task spec parsing, workspace preparation, verification execution,
//! and report/comparison formatting for headless coding benchmarks.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use exoskeleton_core::inbox::Inbox;
use exoskeleton_core::{
    ActionOutcome, Artifact, ArtifactKind, ArtifactStore, EnvelopeId, EnvelopeKind, ExoError,
    MessageEnvelope, PrincipalId, TickPhase, VesselId,
};
use exoskeleton_memory::CompiledContext;
use serde::{Deserialize, Serialize};

use crate::config::VesselConfig;
use crate::inspect::VesselInspector;
use crate::kernel::policy::{PolicyRule, ToolPolicyConfig};
use crate::vessel::Vessel;

/// A benchmark task specification parsed from TOML.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskSpec {
    pub task: TaskSection,
    pub verify: VerifySection,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vessel_overrides: Option<toml::Value>,
}

/// The `[task]` section of a benchmark spec.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskSection {
    pub name: String,
    pub repo_path: String,
    pub prompt: String,
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_ticks: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub difficulty: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_benchmark: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_commit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gold_patch: Option<String>,
}

fn default_timeout() -> u64 {
    600
}

/// The `[verify]` section of a benchmark spec.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerifySection {
    pub command: String,
    #[serde(default)]
    pub expected_exit_code: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub test_patch: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TokenMetrics {
    pub total_in: u64,
    pub total_out: u64,
    pub total: u64,
    pub by_phase: HashMap<String, PhaseTokens>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhaseTokens {
    pub tokens_in: u64,
    pub tokens_out: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepTrace {
    pub step: u32,
    pub tick: u32,
    pub tool_name: String,
    pub tool_params: serde_json::Value,
    pub outcome: String,
    pub output_summary: String,
    pub reasoning: String,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub latency_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextUtilization {
    pub total_budget_tokens: u64,
    pub total_used_tokens: u64,
    pub utilization_pct: f64,
    pub sections: HashMap<String, SectionUtilization>,
}

impl Default for ContextUtilization {
    fn default() -> Self {
        Self {
            total_budget_tokens: 0,
            total_used_tokens: 0,
            utilization_pct: 0.0,
            sections: HashMap::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SectionUtilization {
    pub budget_tokens: u64,
    pub used_tokens: u64,
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TickMetrics {
    pub tick_number: u64,
    pub duration_secs: f64,
    pub tool_steps: u32,
    pub llm_calls: u32,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub actions_taken: u32,
    pub actions_succeeded: u32,
    pub completion_reason: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BehaviorMetrics {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_search_step: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_file_read_step: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_mutation_step: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_verify_step: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_semantic_step: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_completion_signal_tick: Option<u32>,
    #[serde(default)]
    pub search_steps_total: u32,
    #[serde(default)]
    pub semantic_steps_total: u32,
    #[serde(default)]
    pub search_steps_before_first_read: u32,
    #[serde(default)]
    pub semantic_steps_before_first_edit: u32,
    #[serde(default)]
    pub search_steps_after_first_read: u32,
    #[serde(default)]
    pub post_edit_search_steps: u32,
    #[serde(default)]
    pub distinct_files_read: u32,
    #[serde(default)]
    pub distinct_files_modified: u32,
    #[serde(default)]
    pub symbol_reads_total: u32,
    #[serde(default)]
    pub reference_queries_total: u32,
    #[serde(default)]
    pub zero_edit_terminal: bool,
    #[serde(default)]
    pub verification_attempted: bool,
    #[serde(default)]
    pub verification_after_last_edit: bool,
    #[serde(default)]
    pub max_repeated_same_tool_same_target: u32,
    #[serde(default)]
    pub exec_thread_originated_steps: u32,
    #[serde(default)]
    pub master_only_steps: u32,
}

impl BehaviorMetrics {
    fn is_empty(&self) -> bool {
        self == &Self::default()
    }
}

/// Result of running a single benchmark task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskResult {
    pub task_name: String,
    pub passed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification_exit_code: Option<i32>,
    pub completion_reason: String,
    pub wall_time_secs: f64,
    pub steps_taken: u32,
    #[serde(default)]
    pub ticks_used: u32,
    #[serde(default)]
    pub tokens: TokenMetrics,
    #[serde(default)]
    pub model_used: String,
    pub tool_calls: HashMap<String, ToolCallStats>,
    #[serde(default)]
    pub files_modified: Vec<String>,
    pub lines_added: u32,
    pub lines_removed: u32,
    pub doom_loop_corrections: u32,
    #[serde(default)]
    pub llm_cost_cents: f64,
    #[serde(default)]
    pub llm_calls: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub step_trace: Vec<StepTrace>,
    #[serde(default)]
    pub context_utilization: ContextUtilization,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tick_details: Vec<TickMetrics>,
    #[serde(default, skip_serializing_if = "BehaviorMetrics::is_empty")]
    pub behavior_metrics: BehaviorMetrics,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub harness_anomalies: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub difficulty: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_benchmark: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_id: Option<String>,
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
        upper_median(
            &self
                .tasks
                .iter()
                .map(|task| task.steps_taken)
                .collect::<Vec<_>>(),
        )
    }

    pub fn median_tokens(&self) -> u64 {
        upper_median(
            &self
                .tasks
                .iter()
                .map(|task| task.tokens.total)
                .collect::<Vec<_>>(),
        )
    }

    pub fn median_time(&self) -> f64 {
        upper_median(
            &self
                .tasks
                .iter()
                .map(|task| task.wall_time_secs)
                .collect::<Vec<_>>(),
        )
    }
}

/// Upper-median: for even-length arrays, returns the higher of the two middle
/// values rather than their average. This is simpler and avoids fractional
/// results for integer types. For odd-length arrays, returns the true median.
fn upper_median<T: Clone + Default + PartialOrd>(values: &[T]) -> T {
    if values.is_empty() {
        return T::default();
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    sorted[sorted.len() / 2].clone()
}

/// Run a verification command in a workspace directory.
/// Returns (passed, actual_exit_code).
pub fn run_verification<P: AsRef<Path>>(
    command: &str,
    expected_exit_code: i32,
    workspace: P,
) -> Result<(bool, i32), std::io::Error> {
    let output = std::process::Command::new("sh")
        .args(["-c", command])
        .current_dir(workspace)
        .output()?;

    let actual = output.status.code().unwrap_or(-1);
    Ok((actual == expected_exit_code, actual))
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
///
/// Relative paths (`repo_path`, `test_patch`, `gold_patch`) are resolved
/// relative to the spec file's parent directory. This makes task specs
/// portable — they work regardless of the caller's working directory.
pub fn load_task_spec(path: &Path) -> Result<TaskSpec, String> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| format!("failed to read {}: {e}", path.display()))?;
    let mut spec: TaskSpec = toml::from_str(&content)
        .map_err(|e| format!("invalid task spec {}: {e}", path.display()))?;

    let spec_dir = path.parent().unwrap_or_else(|| Path::new("."));

    // Resolve repo_path relative to spec file
    let repo_path = PathBuf::from(&spec.task.repo_path);
    if !repo_path.is_absolute() {
        let resolved = spec_dir.join(&spec.task.repo_path);
        spec.task.repo_path = resolved
            .canonicalize()
            .unwrap_or(resolved)
            .to_string_lossy()
            .to_string();
    }

    // Resolve test_patch relative to spec file
    if let Some(ref patch) = spec.verify.test_patch {
        let patch_path = PathBuf::from(patch);
        if !patch_path.is_absolute() {
            let resolved = spec_dir.join(patch);
            spec.verify.test_patch = Some(resolved.to_string_lossy().to_string());
        }
    }

    // Resolve gold_patch relative to spec file
    if let Some(ref patch) = spec.task.gold_patch {
        let patch_path = PathBuf::from(patch);
        if !patch_path.is_absolute() {
            let resolved = spec_dir.join(patch);
            spec.task.gold_patch = Some(resolved.to_string_lossy().to_string());
        }
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

/// RAII guard that restores the current working directory on drop.
///
/// WARNING: `std::env::set_current_dir()` is process-global. This is safe
/// only because benchmark tasks run sequentially. If tasks ever run in
/// parallel, replace this with `Command::current_dir()` on each subprocess.
struct CurrentDirGuard {
    previous: PathBuf,
}

impl CurrentDirGuard {
    fn enter(path: &Path) -> Result<Self, String> {
        let previous = std::env::current_dir()
            .map_err(|e| format!("failed to read current directory: {e}"))?;
        std::env::set_current_dir(path)
            .map_err(|e| format!("failed to enter workspace {}: {e}", path.display()))?;
        Ok(Self { previous })
    }
}

impl Drop for CurrentDirGuard {
    fn drop(&mut self) {
        if let Err(e) = std::env::set_current_dir(&self.previous) {
            tracing::warn!(
                error = %e,
                path = %self.previous.display(),
                "failed to restore current directory after benchmark task"
            );
        }
    }
}

/// Boot a Vessel from a VesselConfig. Shared by HeadlessRunner and SweBenchRunner.
pub async fn boot_vessel(config: VesselConfig) -> Result<Vessel, String> {
    Vessel::start(config)
        .await
        .map_err(|e| format!("vessel boot failed: {e}"))
}

/// Apply the standard coding-benchmark vessel overrides.
///
/// This enables the coding exec-thread path for a workspace-scoped run,
/// forces a permissive tool policy for autonomous benchmark execution, and
/// assigns a fresh vessel id plus benchmark-local data dir.
pub fn apply_coding_benchmark_overrides(
    config: &mut VesselConfig,
    workspace_path: &Path,
    data_dir: &Path,
) {
    config.data_dir = data_dir.to_path_buf();
    config.coding_thread.enabled = true;
    config.coding_thread.workspace_root = Some(workspace_path.to_string_lossy().into_owned());
    config.coding_thread.return_to_idle_after_completion = true;
    config.tool_policy = ToolPolicyConfig {
        default: PolicyRule::Allow,
        rules: HashMap::new(),
    };
    config.vessel_id = VesselId::new();
}

/// Format the benchmark task prompt with workspace context.
pub fn format_task_prompt(workspace_path: &Path, prompt: &str) -> String {
    format!(
        "You are working on a benchmark task.\n\
The ONLY repository you should inspect or modify is this workspace copy:\n\
{}\n\n\
Use that workspace as the repo root for all code.* tool paths.\n\
Do not read or edit similarly named files outside this workspace.\n\
Verification will run inside this workspace copy, so only changes there count.\n\n\
Task:\n{}",
        workspace_path.display(),
        prompt
    )
}

/// Inject a task prompt and poll for completion. Returns (completion_reason, inspector).
pub async fn inject_and_poll(
    vessel: &Vessel,
    prompt: &str,
    workspace_path: &Path,
    timeout: std::time::Duration,
    max_ticks: Option<u32>,
) -> Result<(String, VesselInspector), String> {
    let artifact_store = vessel.storage().artifact_store().clone();
    HeadlessRunner::submit_task_prompt(
        vessel.inbox().as_ref(),
        artifact_store.as_ref(),
        prompt,
        workspace_path,
    )?;

    let poll_result = tokio::time::timeout(timeout, async {
        HeadlessRunner::poll_for_completion_inner(vessel, max_ticks).await
    })
    .await;

    let completion_reason = match poll_result {
        Ok(Ok(reason)) => reason,
        Ok(Err(e)) => format!("PollError: {e}"),
        Err(_) => "Timeout".to_string(),
    };

    Ok((completion_reason, vessel.inspector()))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BehaviorAction {
    step: u32,
    tick: u32,
    tool_name: String,
    normalized_target: String,
    file_target: Option<String>,
    is_search: bool,
    is_read: bool,
    is_mutation: bool,
    is_verify: bool,
    is_semantic: bool,
    is_symbol_read: bool,
    is_reference_query: bool,
    exec_thread_originated: bool,
}

fn is_search_action(tool_name: &str) -> bool {
    matches!(
        tool_name,
        "code.grep" | "code.ls" | "code.glob" | "repo.context" | "repo.locate"
    )
}

fn is_read_action(tool_name: &str) -> bool {
    matches!(tool_name, "code.read" | "code.read_symbol" | "fs.read")
}

fn is_mutation_action(tool_name: &str) -> bool {
    matches!(
        tool_name,
        "code.edit" | "code.apply_patch" | "code.write" | "fs.write"
    )
}

fn is_semantic_action(tool_name: &str) -> bool {
    matches!(
        tool_name,
        "code.symbol" | "code.read_symbol" | "code.references" | "code.impls"
    )
}

fn shell_exec_command_line(params: &serde_json::Value) -> Option<String> {
    let obj = params.as_object()?;
    let command = obj.get("command")?.as_str()?.to_lowercase();
    let args = obj
        .get("args")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
        .map(|arg| arg.to_lowercase())
        .collect::<Vec<_>>();

    let mut parts = vec![command];
    parts.extend(args);
    Some(parts.join(" "))
}

fn shell_exec_looks_like_verification(params: &serde_json::Value) -> bool {
    let Some(command_line) = shell_exec_command_line(params) else {
        return false;
    };

    [
        "cargo test",
        "cargo nextest",
        "cargo check",
        "cargo build",
        "cargo clippy",
        "pytest",
        "py.test",
        "python -m pytest",
        "python3 -m pytest",
        "go test",
        "npm test",
        "pnpm test",
        "yarn test",
        "bun test",
        "deno test",
        "mvn test",
        "gradle test",
        "./gradlew test",
        "dotnet test",
        "mix test",
        "rspec",
        "jest",
        "vitest",
        "phpunit",
    ]
    .iter()
    .any(|needle| command_line.contains(needle))
}

fn is_verify_action(tool_name: &str, tool_params: &serde_json::Value) -> bool {
    matches!(tool_name, "code.test" | "code.compile")
        || (tool_name == "shell.exec" && shell_exec_looks_like_verification(tool_params))
}

fn action_tool_params(action: &exoskeleton_core::tick::ActionRecord) -> serde_json::Value {
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(&action.target) {
        if value.is_object() || value.is_array() {
            return value;
        }
    }

    match action.action_type.as_str() {
        "code.read" | "code.edit" | "code.apply_patch" | "code.write" => {
            serde_json::json!({ "file_path": action.target })
        }
        "fs.read" | "fs.write" | "code.grep" | "code.ls" | "code.glob" | "repo.context"
        | "repo.locate" | "code.symbol" | "code.read_symbol" | "code.references" | "code.impls"
        | "code.test" => {
            serde_json::json!({ "path": action.target })
        }
        _ => serde_json::Value::Null,
    }
}

fn action_file_target(action: &exoskeleton_core::tick::ActionRecord) -> Option<String> {
    if action.target.is_empty() {
        return None;
    }

    match action.action_type.as_str() {
        "code.read" | "code.read_symbol" | "code.edit" | "code.apply_patch" | "code.write"
        | "fs.read" | "fs.write" => Some(action.target.clone()),
        _ => None,
    }
}

fn normalized_action_target(
    action: &exoskeleton_core::tick::ActionRecord,
    tool_params: &serde_json::Value,
) -> String {
    if action.action_type == "shell.exec" {
        if let Some(command_line) = shell_exec_command_line(tool_params) {
            return command_line;
        }
    }

    if let Some(file_target) = action_file_target(action) {
        return file_target;
    }

    if !action.target.is_empty() {
        return action.target.clone();
    }

    action.action_type.clone()
}

fn compute_behavior_metrics(
    actions: &[BehaviorAction],
    first_completion_signal_tick: Option<u32>,
) -> BehaviorMetrics {
    let first_search_step = actions
        .iter()
        .find(|action| action.is_search)
        .map(|action| action.step);
    let first_file_read_step = actions
        .iter()
        .find(|action| action.is_read)
        .map(|action| action.step);
    let first_mutation_step = actions
        .iter()
        .find(|action| action.is_mutation)
        .map(|action| action.step);
    let first_verify_step = actions
        .iter()
        .find(|action| action.is_verify)
        .map(|action| action.step);
    let first_semantic_step = actions
        .iter()
        .find(|action| action.is_semantic)
        .map(|action| action.step);
    let last_mutation_step = actions
        .iter()
        .rev()
        .find(|action| action.is_mutation)
        .map(|action| action.step);

    let search_steps_total = actions.iter().filter(|action| action.is_search).count() as u32;
    let semantic_steps_total = actions.iter().filter(|action| action.is_semantic).count() as u32;
    let search_steps_before_first_read = actions
        .iter()
        .filter(|action| action.is_search)
        .filter(|action| first_file_read_step.is_none_or(|first_read| action.step < first_read))
        .count() as u32;
    let semantic_steps_before_first_edit = actions
        .iter()
        .filter(|action| action.is_semantic)
        .filter(|action| first_mutation_step.is_none_or(|first_edit| action.step < first_edit))
        .count() as u32;
    let search_steps_after_first_read = actions
        .iter()
        .filter(|action| action.is_search)
        .filter(|action| first_file_read_step.is_some_and(|first_read| action.step > first_read))
        .count() as u32;
    let post_edit_search_steps = actions
        .iter()
        .filter(|action| action.is_search)
        .filter(|action| last_mutation_step.is_some_and(|last_edit| action.step > last_edit))
        .count() as u32;

    let distinct_files_read = actions
        .iter()
        .filter(|action| action.is_read)
        .filter_map(|action| action.file_target.as_ref())
        .collect::<HashSet<_>>()
        .len() as u32;
    let distinct_files_modified = actions
        .iter()
        .filter(|action| action.is_mutation)
        .filter_map(|action| action.file_target.as_ref())
        .collect::<HashSet<_>>()
        .len() as u32;
    let symbol_reads_total = actions
        .iter()
        .filter(|action| action.is_symbol_read)
        .count() as u32;
    let reference_queries_total = actions
        .iter()
        .filter(|action| action.is_reference_query)
        .count() as u32;

    let exec_thread_originated_steps = actions
        .iter()
        .filter(|action| action.exec_thread_originated)
        .count() as u32;
    let master_only_steps = actions.len() as u32 - exec_thread_originated_steps;

    let mut max_repeated_same_tool_same_target = 0_u32;
    let mut current_run = 0_u32;
    let mut previous: Option<(&str, &str)> = None;
    for action in actions {
        let current = (action.tool_name.as_str(), action.normalized_target.as_str());
        if previous == Some(current) {
            current_run += 1;
        } else {
            current_run = 1;
            previous = Some(current);
        }
        max_repeated_same_tool_same_target = max_repeated_same_tool_same_target.max(current_run);
    }

    let verification_attempted = first_verify_step.is_some();
    let verification_after_last_edit = last_mutation_step
        .zip(first_verify_step)
        .is_some_and(|(last_edit, first_verify)| first_verify > last_edit)
        || actions
            .iter()
            .filter(|action| action.is_verify)
            .any(|action| last_mutation_step.is_some_and(|last_edit| action.step > last_edit));

    BehaviorMetrics {
        first_search_step,
        first_file_read_step,
        first_mutation_step,
        first_verify_step,
        first_semantic_step,
        first_completion_signal_tick,
        search_steps_total,
        semantic_steps_total,
        search_steps_before_first_read,
        semantic_steps_before_first_edit,
        search_steps_after_first_read,
        post_edit_search_steps,
        distinct_files_read,
        distinct_files_modified,
        symbol_reads_total,
        reference_queries_total,
        zero_edit_terminal: !actions.is_empty() && first_mutation_step.is_none(),
        verification_attempted,
        verification_after_last_edit,
        max_repeated_same_tool_same_target,
        exec_thread_originated_steps,
        master_only_steps,
    }
}

fn first_completion_signal_tick(inspector: &VesselInspector) -> Option<u32> {
    let mut snapshots = inspector.snapshot_history(256).ok()?;
    snapshots.reverse();
    snapshots
        .into_iter()
        .find(|snapshot| HeadlessRunner::completion_reason_from_snapshot(snapshot).is_some())
        .map(|snapshot| snapshot.tick_number as u32)
}

/// Extract comprehensive metrics from a completed benchmark run.
pub fn extract_metrics(inspector: &VesselInspector) -> Result<MetricsSnapshot, String> {
    // tick_history() returns newest-first; reverse to iterate oldest-first
    // so that step numbering and phase token attribution are chronological.
    let mut ticks = inspector
        .tick_history(256)
        .map_err(|e| format!("failed to read tick history: {e}"))?;
    ticks.reverse();

    let mut total_steps: u32 = 0;
    let mut total_tokens_in: u64 = 0;
    let mut total_tokens_out: u64 = 0;
    let mut total_cost_cents: f64 = 0.0;
    let mut total_llm_calls: u32 = 0;
    let mut tool_calls: HashMap<String, ToolCallStats> = HashMap::new();
    let mut files_modified: Vec<String> = Vec::new();
    let mut total_lines_added: u32 = 0;
    let mut total_lines_removed: u32 = 0;
    let mut doom_corrections: u32 = 0;
    let mut model_used = String::new();
    let mut step_trace: Vec<StepTrace> = Vec::new();
    let mut tick_details: Vec<TickMetrics> = Vec::new();
    let mut phase_tokens: HashMap<String, PhaseTokens> = HashMap::new();
    let mut global_step: u32 = 0;
    let mut behavior_actions: Vec<BehaviorAction> = Vec::new();

    for tick in &ticks {
        if !(tick.phase == TickPhase::Amend && tick.completed_at.is_some()) {
            continue;
        }

        let tick_tokens_in: u64 = tick.llm_calls.iter().map(|c| c.tokens_in).sum();
        let tick_tokens_out: u64 = tick.llm_calls.iter().map(|c| c.tokens_out).sum();
        let tick_cost: f64 = tick.llm_calls.iter().map(|c| c.cost_cents).sum();
        let tick_llm_calls = tick.llm_calls.len() as u32;

        total_tokens_in += tick_tokens_in;
        total_tokens_out += tick_tokens_out;
        total_cost_cents += tick_cost;
        total_llm_calls += tick_llm_calls;

        if model_used.is_empty() {
            if let Some(call) = tick.llm_calls.first() {
                model_used = call.model.clone();
            }
        }

        // Phase attribution heuristic: LlmCallRecord does not carry a phase tag,
        // so we infer from position. In a standard PODAARA tick, the first LLM
        // call is Decide, the last (if >1 total) is Reflect, and everything in
        // between is additional coding/decision work. This remains approximate
        // until phase tags are recorded directly on LLM calls.
        for (i, call) in tick.llm_calls.iter().enumerate() {
            let phase = if i == 0 {
                "decide"
            } else if i == tick.llm_calls.len() - 1 && tick.llm_calls.len() > 1 {
                "reflect"
            } else {
                "coding_exec"
            };
            let entry = phase_tokens
                .entry(phase.to_string())
                .or_insert(PhaseTokens {
                    tokens_in: 0,
                    tokens_out: 0,
                });
            entry.tokens_in += call.tokens_in;
            entry.tokens_out += call.tokens_out;
        }

        let mut tick_actions_succeeded: u32 = 0;
        for action in &tick.actions_taken {
            let entry = tool_calls
                .entry(action.action_type.clone())
                .or_insert(ToolCallStats {
                    calls: 0,
                    successes: 0,
                    failures: 0,
                });
            entry.calls += 1;
            match action.outcome {
                ActionOutcome::Success => {
                    entry.successes += 1;
                    tick_actions_succeeded += 1;
                }
                _ => {
                    entry.failures += 1;
                }
            }

            if matches!(
                action.action_type.as_str(),
                "fs.write" | "code.write" | "code.edit" | "code.apply_patch"
            ) && !action.target.is_empty()
            {
                files_modified.push(action.target.clone());
            }

            // Extract lines_added/removed from receipt artifacts (CodeDiff data)
            if let Some(ref receipt_id) = action.receipt_ref {
                if let Ok(Some(receipt)) = inspector.artifact(receipt_id) {
                    if let Ok(val) = serde_json::from_slice::<serde_json::Value>(&receipt.content) {
                        if let Some(diff) = val.get("diff") {
                            if let Some(added) = diff.get("lines_added").and_then(|v| v.as_u64()) {
                                total_lines_added += added as u32;
                            }
                            if let Some(removed) =
                                diff.get("lines_removed").and_then(|v| v.as_u64())
                            {
                                total_lines_removed += removed as u32;
                            }
                        }
                    }
                }
            }

            global_step += 1;
            let tool_params = action_tool_params(action);
            let is_search = is_search_action(&action.action_type);
            let is_read = is_read_action(&action.action_type);
            let is_mutation = is_mutation_action(&action.action_type);
            let is_verify = is_verify_action(&action.action_type, &tool_params);
            let is_semantic = is_semantic_action(&action.action_type);
            let normalized_target = normalized_action_target(action, &tool_params);
            let file_target = action_file_target(action);

            behavior_actions.push(BehaviorAction {
                step: global_step,
                tick: tick.tick_number as u32,
                tool_name: action.action_type.clone(),
                normalized_target,
                file_target,
                is_search,
                is_read,
                is_mutation,
                is_verify,
                is_semantic,
                is_symbol_read: action.action_type == "code.read_symbol",
                is_reference_query: action.action_type == "code.references",
                exec_thread_originated: action.origin_exec_thread_id.is_some(),
            });

            // NOTE: Per-step token attribution is not yet implemented. Token counts
            // are only available at the tick level (from LlmCallRecord), not per
            // individual tool call. Step trace tokens are always 0 for now.
            step_trace.push(StepTrace {
                step: global_step,
                tick: tick.tick_number as u32,
                tool_name: action.action_type.clone(),
                tool_params,
                outcome: format!("{:?}", action.outcome),
                output_summary: action.target.clone(),
                reasoning: String::new(),
                tokens_in: 0,
                tokens_out: 0,
                latency_ms: 0,
            });
        }

        total_steps += tick.actions_taken.len() as u32;

        // Count doom-loop corrections from the tick's decision rationale
        if let Some(ref rationale) = tick.decision_rationale {
            let normalized = rationale.to_lowercase();
            if normalized.contains("doom_loop") || normalized.contains("doom loop") {
                doom_corrections += 1;
            }
        }

        let tick_duration = tick
            .completed_at
            .map(|c| (c - tick.started_at).num_milliseconds() as f64 / 1000.0)
            .unwrap_or(0.0);

        let tick_reason = tick
            .decision_rationale
            .as_deref()
            .unwrap_or("Unknown")
            .to_string();
        tick_details.push(TickMetrics {
            tick_number: tick.tick_number,
            duration_secs: tick_duration,
            tool_steps: tick.actions_taken.len() as u32,
            llm_calls: tick_llm_calls,
            tokens_in: tick_tokens_in,
            tokens_out: tick_tokens_out,
            actions_taken: tick.actions_taken.len() as u32,
            actions_succeeded: tick_actions_succeeded,
            completion_reason: tick_reason,
        });
    }

    files_modified.sort();
    files_modified.dedup();

    let context_utilization = ticks
        .iter()
        .find_map(|tick| tick.context_breakdown_ref.as_ref())
        .and_then(|id| inspector.artifact(id).ok().flatten())
        .and_then(|artifact| serde_json::from_slice::<CompiledContext>(&artifact.content).ok())
        .map(compiled_context_to_utilization)
        .unwrap_or_default();

    let ticks_used = tick_details.len() as u32;
    let behavior_metrics =
        compute_behavior_metrics(&behavior_actions, first_completion_signal_tick(inspector));
    let harness_anomalies = detect_benchmark_harness_anomalies(ticks_used, &context_utilization);

    Ok(MetricsSnapshot {
        ticks_used,
        steps_taken: total_steps,
        tokens: TokenMetrics {
            total_in: total_tokens_in,
            total_out: total_tokens_out,
            total: total_tokens_in + total_tokens_out,
            by_phase: phase_tokens,
        },
        model_used,
        tool_calls,
        files_modified,
        lines_added: total_lines_added,
        lines_removed: total_lines_removed,
        doom_loop_corrections: doom_corrections,
        llm_cost_cents: total_cost_cents,
        llm_calls: total_llm_calls,
        step_trace,
        context_utilization,
        tick_details,
        behavior_metrics,
        harness_anomalies,
    })
}

/// Log diagnostic information about completed ticks.
pub fn log_diagnostics(inspector: &VesselInspector) {
    if let Ok(ticks) = inspector.tick_history(32) {
        eprintln!("  [diag] {} tick(s) recorded", ticks.len());
        for tick in &ticks {
            let completed = tick.phase == TickPhase::Amend && tick.completed_at.is_some();
            eprintln!(
                "  [diag] tick #{}: phase={:?} completed={} actions={} llm_calls={}",
                tick.tick_number,
                tick.phase,
                completed,
                tick.actions_taken.len(),
                tick.llm_calls.len(),
            );
            if let Some(ref rationale) = tick.decision_rationale {
                // Truncate to first 500 chars for readability
                let display = if rationale.len() > 500 {
                    format!("{}...", &rationale[..500])
                } else {
                    rationale.clone()
                };
                eprintln!("  [diag]   rationale: {display}");
            }
            for (i, call) in tick.llm_calls.iter().enumerate() {
                eprintln!(
                    "  [diag]   llm_call[{}]: model={} in={} out={} cost={:.4}c",
                    i, call.model, call.tokens_in, call.tokens_out, call.cost_cents,
                );
            }
            for (i, action) in tick.actions_taken.iter().enumerate() {
                eprintln!(
                    "  [diag]   action[{}]: {} -> {} ({:?})",
                    i, action.action_type, action.target, action.outcome,
                );
            }
            for contribution in &tick.exec_thread_contributions {
                eprintln!(
                    "  [diag]   exec_thread: {:?} {} proposal={:?}",
                    contribution.kind, contribution.summary, contribution.proposed_action_summary
                );
            }
        }
    }
}

/// Headless benchmark runner. Boots a Vessel per task, runs the agent,
/// and extracts metrics.
pub struct HeadlessRunner;

impl HeadlessRunner {
    #[cfg_attr(not(test), allow(dead_code))]
    fn completion_reason_from_reasoning(reasoning: &str) -> Option<&'static str> {
        let normalized = reasoning.to_lowercase();

        const KNOWN: &[(&str, &str)] = &[
            ("agent_complete", "AgentComplete"),
            ("token_budget", "TokenBudget"),
            ("timeout", "Timeout"),
            ("doom_loop", "DoomLoop"),
            ("cancelled", "Cancelled"),
            ("awaiting_input", "AwaitingInput"),
        ];

        for (needle, label) in KNOWN {
            if normalized.contains(needle) {
                return Some(label);
            }
        }

        if normalized.contains("error:") {
            return Some("Error");
        }

        None
    }

    fn completion_reason_from_snapshot(
        snapshot: &exoskeleton_core::StateSnapshot,
    ) -> Option<String> {
        let coding = snapshot
            .exec_thread_summaries
            .iter()
            .find(|summary| summary.kind == exoskeleton_core::ExecThreadKind::Coding)?;
        match coding.status {
            exoskeleton_core::ExecThreadStatus::Completed => Some("ExecCompleted".into()),
            exoskeleton_core::ExecThreadStatus::Failed => Some("ExecFailed".into()),
            exoskeleton_core::ExecThreadStatus::Blocked => Some("ExecBlocked".into()),
            exoskeleton_core::ExecThreadStatus::Idle if coding.last_completion_reason.is_some() => {
                Some("ExecCompleted".into())
            }
            exoskeleton_core::ExecThreadStatus::Active => None,
            exoskeleton_core::ExecThreadStatus::Idle => None,
        }
    }

    /// Build a VesselConfig for a benchmark task by merging the base config
    /// with task-specific and forced overrides.
    pub fn build_vessel_config(
        base: &VesselConfig,
        spec: &TaskSpec,
        workspace_path: &Path,
        data_dir: &Path,
    ) -> Result<VesselConfig, ExoError> {
        let mut config = base.clone();

        if let Some(toml::Value::Table(overrides)) = &spec.vessel_overrides {
            for key in [
                "max_tokens_per_session",
                "timeout_secs",
                "context_window_size",
                "doom_loop_threshold",
            ] {
                if overrides.contains_key(key) {
                    return Err(ExoError::Config(format!(
                        "benchmark vessel_overrides key '{key}' was removed; use task.max_steps or coding_thread config instead"
                    )));
                }
            }
        }

        apply_coding_benchmark_overrides(&mut config, workspace_path, data_dir);

        Ok(config)
    }

    /// Submit a task prompt to the vessel's inbox as a HumanMessage envelope.
    fn submit_task_prompt(
        inbox: &dyn Inbox,
        artifact_store: &dyn ArtifactStore,
        prompt: &str,
        workspace_path: &Path,
    ) -> Result<EnvelopeId, String> {
        let benchmark_prompt = format_task_prompt(workspace_path, prompt);
        let artifact = Artifact::new(
            ArtifactKind::Envelope,
            benchmark_prompt.as_bytes().to_vec(),
            "text/plain".to_string(),
        );
        let artifact_id = artifact.id.clone();
        artifact_store
            .put(&artifact)
            .map_err(|e| format!("failed to store prompt artifact: {e}"))?;

        let envelope_id = EnvelopeId::new();
        let envelope = MessageEnvelope {
            id: envelope_id,
            source: PrincipalId::new(),
            target: None,
            kind: EnvelopeKind::HumanMessage,
            payload_ref: artifact_id,
            timestamp: Utc::now(),
            in_reply_to: None,
        };
        inbox
            .submit(&envelope)
            .map_err(|e| format!("failed to submit envelope: {e}"))?;

        Ok(envelope_id)
    }

    /// Poll for task completion by watching tick history.
    ///
    /// Accepts `max_ticks` directly so callers without a `TaskSpec` (e.g.
    /// `SweBenchRunner`) can use this through `inject_and_poll`.
    pub(crate) async fn poll_for_completion_inner(
        vessel: &Vessel,
        max_ticks: Option<u32>,
    ) -> Result<String, String> {
        let mut last_seen_tick: u64 = 0;
        let poll_interval = std::time::Duration::from_millis(200);
        let inspector = vessel.inspector();

        loop {
            tokio::time::sleep(poll_interval).await;

            let mut ticks = inspector
                .tick_history(32)
                .map_err(|e| format!("tick history read failed: {e}"))?;
            ticks.reverse();

            let mut completed_ticks = 0_u64;
            for tick in &ticks {
                if tick.phase == TickPhase::Amend && tick.completed_at.is_some() {
                    completed_ticks += 1;
                }
            }

            for tick in ticks {
                if tick.tick_number <= last_seen_tick {
                    continue;
                }
                last_seen_tick = tick.tick_number;

                if !(tick.phase == TickPhase::Amend && tick.completed_at.is_some()) {
                    continue;
                }

                if let Some(snapshot) = inspector
                    .snapshot()
                    .map_err(|e| format!("snapshot read failed: {e}"))?
                {
                    if let Some(reason) = Self::completion_reason_from_snapshot(&snapshot) {
                        return Ok(reason);
                    }
                }
            }

            if let Some(max) = max_ticks {
                // Allow one additional convergence tick after the nominal limit
                // so exec-thread completion can be recorded from the previous
                // tick's action results.
                if completed_ticks > max as u64 {
                    return Ok("MaxTicks".to_string());
                }
            }
        }
    }

    /// Run a single benchmark task end-to-end.
    pub async fn run_task(
        spec: &TaskSpec,
        base_config: &VesselConfig,
    ) -> Result<TaskResult, String> {
        let start = std::time::Instant::now();

        let repo_path = PathBuf::from(&spec.task.repo_path);
        if !repo_path.exists() {
            return Err(format!("repo path does not exist: {}", repo_path.display()));
        }
        let workspace_dir = prepare_workspace(&repo_path)
            .map_err(|e| format!("workspace preparation failed: {e}"))?;
        let _cwd_guard = CurrentDirGuard::enter(workspace_dir.path())?;

        if let Some(commit) = spec.task.base_commit.as_deref() {
            let output = std::process::Command::new("git")
                .args(["checkout", commit])
                .current_dir(workspace_dir.path())
                .output()
                .map_err(|e| format!("git checkout failed: {e}"))?;
            if !output.status.success() {
                return Err(format!(
                    "git checkout {commit} failed: {}",
                    String::from_utf8_lossy(&output.stderr)
                ));
            }
        }

        let data_dir = workspace_dir.path().join(".exo-bench");
        std::fs::create_dir_all(&data_dir)
            .map_err(|e| format!("failed to create bench data dir: {e}"))?;
        let config = Self::build_vessel_config(base_config, spec, workspace_dir.path(), &data_dir)
            .map_err(|e| format!("invalid benchmark vessel config: {e}"))?;

        let vessel = boot_vessel(config).await?;

        let timeout = std::time::Duration::from_secs(spec.task.timeout_secs);
        let (completion_reason, inspector) = inject_and_poll(
            &vessel,
            &spec.task.prompt,
            workspace_dir.path(),
            timeout,
            spec.task.max_ticks,
        )
        .await?;

        vessel
            .shutdown()
            .await
            .map_err(|e| format!("vessel shutdown failed: {e}"))?;

        log_diagnostics(&inspector);

        let metrics = extract_metrics(&inspector).unwrap_or_else(|e| {
            tracing::warn!("metrics extraction failed: {e}");
            MetricsSnapshot::default()
        });

        for anomaly in &metrics.harness_anomalies {
            eprintln!("  [warn] harness anomaly: {anomaly}");
        }

        // test_patch is already resolved to an absolute path by load_task_spec()
        if let Some(patch_path) = spec.verify.test_patch.as_deref() {
            let patch_abs = PathBuf::from(patch_path);
            if patch_abs.exists() {
                let output = std::process::Command::new("git")
                    .args(["apply", &patch_abs.to_string_lossy()])
                    .current_dir(workspace_dir.path())
                    .output()
                    .map_err(|e| format!("test patch application failed: {e}"))?;
                if !output.status.success() {
                    tracing::warn!(
                        "test patch application failed: {}",
                        String::from_utf8_lossy(&output.stderr)
                    );
                }
            }
        }

        let (passed, actual_exit_code) = run_verification(
            &spec.verify.command,
            spec.verify.expected_exit_code,
            workspace_dir.path(),
        )
        .map_err(|e| format!("verification failed: {e}"))?;

        // Preserve workspace on failure for post-mortem inspection.
        // On success, the TempDir drops normally and cleans up.
        if !passed {
            let preserved = workspace_dir.keep();
            eprintln!(
                "  [diag] task FAILED — workspace preserved at: {}",
                preserved.display()
            );
            eprintln!(
                "  [diag] vessel data at: {}/.exo-bench/",
                preserved.display()
            );
        }

        Ok(TaskResult {
            task_name: spec.task.name.clone(),
            passed,
            verification_exit_code: Some(actual_exit_code),
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
            difficulty: spec.task.difficulty.clone(),
            language: spec.task.language.clone(),
            tags: spec.task.tags.clone(),
            source_benchmark: spec.task.source_benchmark.clone(),
            source_id: spec.task.source_id.clone(),
            timestamp: Utc::now(),
        })
    }
}

/// Snapshot of metrics extracted from a completed benchmark run.
pub struct MetricsSnapshot {
    pub ticks_used: u32,
    pub steps_taken: u32,
    pub tokens: TokenMetrics,
    pub model_used: String,
    pub tool_calls: HashMap<String, ToolCallStats>,
    pub files_modified: Vec<String>,
    pub lines_added: u32,
    pub lines_removed: u32,
    pub doom_loop_corrections: u32,
    pub llm_cost_cents: f64,
    pub llm_calls: u32,
    pub step_trace: Vec<StepTrace>,
    pub context_utilization: ContextUtilization,
    pub tick_details: Vec<TickMetrics>,
    pub behavior_metrics: BehaviorMetrics,
    pub harness_anomalies: Vec<String>,
}

impl Default for MetricsSnapshot {
    fn default() -> Self {
        Self {
            ticks_used: 0,
            steps_taken: 0,
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
            behavior_metrics: BehaviorMetrics::default(),
            harness_anomalies: vec![],
        }
    }
}

fn detect_benchmark_harness_anomalies(
    ticks_used: u32,
    context_utilization: &ContextUtilization,
) -> Vec<String> {
    if ticks_used == 0 {
        return Vec::new();
    }

    let mut anomalies = Vec::new();

    if context_utilization.total_budget_tokens == 0 {
        anomalies.push(
            "missing_compiled_context_artifact: no compiled context breakdown was captured"
                .to_string(),
        );
        return anomalies;
    }

    match context_utilization.sections.get("conversations") {
        Some(section) if section.used_tokens == 0 => anomalies.push(
            "missing_conversations_context: benchmark task prompt did not make it into compiled context"
                .to_string(),
        ),
        None => anomalies.push(
            "missing_conversations_section: compiled context omitted conversations section"
                .to_string(),
        ),
        _ => {}
    }

    match context_utilization.sections.get("exec_thread_outputs") {
        Some(section) if section.used_tokens == 0 => anomalies.push(
            "missing_exec_thread_context: coding exec-thread output did not make it into compiled context"
                .to_string(),
        ),
        None => anomalies.push(
            "missing_exec_thread_section: compiled context omitted exec-thread outputs section"
                .to_string(),
        ),
        _ => {}
    }

    anomalies
}

fn compiled_context_to_utilization(compiled: CompiledContext) -> ContextUtilization {
    let sections = compiled
        .sections
        .into_iter()
        .map(|s| {
            (
                s.name,
                SectionUtilization {
                    budget_tokens: s.allocated,
                    used_tokens: s.used,
                    truncated: s.truncated,
                },
            )
        })
        .collect::<HashMap<_, _>>();

    let utilization_pct = if compiled.budget == 0 {
        0.0
    } else {
        (compiled.total_tokens as f64 / compiled.budget as f64) * 100.0
    };

    ContextUtilization {
        total_budget_tokens: compiled.budget,
        total_used_tokens: compiled.total_tokens,
        utilization_pct,
        sections,
    }
}

/// Format a suite result as a human-readable report.
pub fn format_report(result: &SuiteResult) -> String {
    let mut out = String::new();
    let format_step = |step: Option<u32>| match step {
        Some(step) => step.to_string(),
        None => "-".into(),
    };
    let median_option = |values: Vec<u32>| -> Option<u32> {
        if values.is_empty() {
            return None;
        }
        let mut values = values;
        values.sort_unstable();
        Some(values[values.len() / 2])
    };

    let model = result
        .tasks
        .first()
        .map(|t| t.model_used.as_str())
        .unwrap_or("unknown");

    out.push_str(&format!(
        "\n{line}\n  Exoskeleton Benchmark Report\n  Model: {model}\n  Run: {run_id}  Date: {date}\n{line}\n\n",
        line = "=".repeat(64),
        run_id = result.run_id,
        date = result.timestamp.format("%Y-%m-%dT%H:%M:%SZ"),
    ));

    let mut total_cost = 0.0_f64;
    let mut total_harness_anomalies = 0_usize;
    let mut total_search_before_first_read = 0_u32;
    let mut total_semantic_before_first_edit = 0_u32;
    for task in &result.tasks {
        let status = if task.passed { "PASS" } else { "FAIL" };
        let dots = ".".repeat(40_usize.saturating_sub(task.task_name.len()));
        let anomaly_suffix = if task.harness_anomalies.is_empty() {
            String::new()
        } else {
            total_harness_anomalies += task.harness_anomalies.len();
            format!("  [anomaly:{}]", task.harness_anomalies.len())
        };
        total_search_before_first_read += task.behavior_metrics.search_steps_before_first_read;
        total_semantic_before_first_edit += task.behavior_metrics.semantic_steps_before_first_edit;
        let behavior_suffix = if task.behavior_metrics.is_empty() {
            String::new()
        } else {
            format!(
                "  [r:{} sem:{} e:{} s<r:{}]",
                format_step(task.behavior_metrics.first_file_read_step),
                format_step(task.behavior_metrics.first_semantic_step),
                format_step(task.behavior_metrics.first_mutation_step),
                task.behavior_metrics.search_steps_before_first_read,
            )
        };
        out.push_str(&format!(
            "  {} {} {} {:>5.0}s {:>3} steps {:>6} tok  ${:.2}{}{}\n",
            task.task_name,
            dots,
            status,
            task.wall_time_secs,
            task.steps_taken,
            task.tokens.total,
            task.llm_cost_cents / 100.0,
            behavior_suffix,
            anomaly_suffix,
        ));
        total_cost += task.llm_cost_cents;
    }

    let pass_count = result.tasks.iter().filter(|t| t.passed).count();
    let total = result.tasks.len();
    let median_first_read = median_option(
        result
            .tasks
            .iter()
            .filter_map(|task| task.behavior_metrics.first_file_read_step)
            .collect(),
    );
    let median_first_edit = median_option(
        result
            .tasks
            .iter()
            .filter_map(|task| task.behavior_metrics.first_mutation_step)
            .collect(),
    );
    let median_first_semantic = median_option(
        result
            .tasks
            .iter()
            .filter_map(|task| task.behavior_metrics.first_semantic_step)
            .collect(),
    );
    out.push_str(&format!(
        "\n{}\n  Pass rate:     {}/{} ({:.0}%)\n  Median time:   {:.1}s\n  Median steps:  {}\n  Median tokens: {}\n  Median read:   {}\n  Median semantic: {}\n  Median edit:   {}\n  Search<read:   {}\n  Semantic<edit: {}\n  Total cost:    ${:.2}\n  Anomalies:     {}\n{}\n",
        "-".repeat(64),
        pass_count,
        total,
        result.pass_rate() * 100.0,
        result.median_time(),
        result.median_steps(),
        result.median_tokens(),
        format_step(median_first_read),
        format_step(median_first_semantic),
        format_step(median_first_edit),
        total_search_before_first_read,
        total_semantic_before_first_edit,
        total_cost / 100.0,
        total_harness_anomalies,
        "=".repeat(64),
    ));

    out
}

/// Format a single step for verbose output.
pub fn format_step_verbose(step: &StepTrace) -> String {
    format!(
        "    [{}] {} -> {} ({})",
        step.step, step.tool_name, step.output_summary, step.outcome
    )
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
max_ticks = 15

[verify]
command = "cargo test --lib test_parse_config"
expected_exit_code = 0
"#;
        let spec: TaskSpec = toml::from_str(toml_str).unwrap();
        assert_eq!(spec.task.name, "add-test-for-parser");
        assert_eq!(spec.task.timeout_secs, 300);
        assert_eq!(spec.task.max_ticks, Some(15));
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
        assert_eq!(spec.verify.expected_exit_code, 0);
        assert_eq!(spec.task.max_ticks, None);
        assert!(spec.task.tags.is_empty());
        assert_eq!(spec.verify.test_patch, None);
        assert_eq!(spec.vessel_overrides, None);
    }

    #[test]
    fn parse_extended_task_spec() {
        let toml_str = r#"
[task]
name = "extended-01"
repo_path = "../repos/sample-rust"
prompt = "Fix the bug"
timeout_secs = 300
max_steps = 25
max_ticks = 3
difficulty = "medium"
language = "rust"
tags = ["bugfix", "parser"]
source_benchmark = "swe-bench-verified"
source_id = "django__django-12345"
base_commit = "abc123def"
gold_patch = "../patches/gold-01.patch"

[verify]
command = "cargo test"
expected_exit_code = 0
test_patch = "../patches/test-01.patch"

[vessel_overrides]
timeout_secs = 600
"#;
        let spec: TaskSpec = toml::from_str(toml_str).unwrap();
        assert_eq!(spec.task.name, "extended-01");
        assert_eq!(spec.task.max_ticks, Some(3));
        assert_eq!(spec.task.difficulty.as_deref(), Some("medium"));
        assert_eq!(spec.task.language.as_deref(), Some("rust"));
        assert_eq!(spec.task.tags, vec!["bugfix", "parser"]);
        assert_eq!(
            spec.task.source_benchmark.as_deref(),
            Some("swe-bench-verified")
        );
        assert_eq!(spec.task.source_id.as_deref(), Some("django__django-12345"));
        assert_eq!(spec.task.base_commit.as_deref(), Some("abc123def"));
        assert_eq!(
            spec.task.gold_patch.as_deref(),
            Some("../patches/gold-01.patch")
        );
        assert_eq!(
            spec.verify.test_patch.as_deref(),
            Some("../patches/test-01.patch")
        );
        assert!(spec.vessel_overrides.is_some());
    }

    #[test]
    fn expanded_task_result_roundtrip() {
        let result = TaskResult {
            task_name: "test-01".into(),
            passed: true,
            verification_exit_code: Some(0),
            completion_reason: "AgentComplete".into(),
            wall_time_secs: 12.5,
            steps_taken: 8,
            ticks_used: 1,
            tokens: TokenMetrics {
                total_in: 3000,
                total_out: 500,
                total: 3500,
                by_phase: {
                    let mut m = HashMap::new();
                    m.insert(
                        "decide".into(),
                        PhaseTokens {
                            tokens_in: 2000,
                            tokens_out: 300,
                        },
                    );
                    m.insert(
                        "coding_exec".into(),
                        PhaseTokens {
                            tokens_in: 800,
                            tokens_out: 150,
                        },
                    );
                    m.insert(
                        "reflect".into(),
                        PhaseTokens {
                            tokens_in: 200,
                            tokens_out: 50,
                        },
                    );
                    m
                },
            },
            model_used: "claude-sonnet-4-20250514".into(),
            tool_calls: HashMap::new(),
            files_modified: vec!["src/lib.rs".into()],
            lines_added: 10,
            lines_removed: 2,
            doom_loop_corrections: 0,
            llm_cost_cents: 1.5,
            llm_calls: 3,
            step_trace: vec![StepTrace {
                step: 1,
                tick: 1,
                tool_name: "code.read".into(),
                tool_params: serde_json::json!({"file_path": "src/lib.rs"}),
                outcome: "Success".into(),
                output_summary: "45 lines".into(),
                reasoning: "Need to read the file first".into(),
                tokens_in: 1200,
                tokens_out: 180,
                latency_ms: 850,
            }],
            context_utilization: ContextUtilization {
                total_budget_tokens: 32_000,
                total_used_tokens: 18_400,
                utilization_pct: 57.5,
                sections: {
                    let mut m = HashMap::new();
                    m.insert(
                        "system".into(),
                        SectionUtilization {
                            budget_tokens: 3200,
                            used_tokens: 2100,
                            truncated: false,
                        },
                    );
                    m
                },
            },
            tick_details: vec![TickMetrics {
                tick_number: 1,
                duration_secs: 12.5,
                tool_steps: 8,
                llm_calls: 3,
                tokens_in: 3000,
                tokens_out: 500,
                actions_taken: 8,
                actions_succeeded: 7,
                completion_reason: "AgentComplete".into(),
            }],
            behavior_metrics: BehaviorMetrics {
                first_search_step: Some(1),
                first_file_read_step: Some(2),
                first_mutation_step: Some(3),
                first_verify_step: Some(4),
                first_semantic_step: Some(2),
                first_completion_signal_tick: Some(1),
                search_steps_total: 2,
                semantic_steps_total: 1,
                search_steps_before_first_read: 1,
                semantic_steps_before_first_edit: 1,
                search_steps_after_first_read: 1,
                post_edit_search_steps: 0,
                distinct_files_read: 1,
                distinct_files_modified: 1,
                symbol_reads_total: 0,
                reference_queries_total: 0,
                zero_edit_terminal: false,
                verification_attempted: true,
                verification_after_last_edit: true,
                max_repeated_same_tool_same_target: 1,
                exec_thread_originated_steps: 3,
                master_only_steps: 5,
            },
            harness_anomalies: vec!["missing_exec_thread_context".into()],
            difficulty: Some("easy".into()),
            language: Some("rust".into()),
            tags: vec!["unit-test".into()],
            source_benchmark: None,
            source_id: None,
            timestamp: Utc::now(),
        };

        let json = serde_json::to_string_pretty(&result).unwrap();
        let parsed: TaskResult = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.task_name, "test-01");
        assert_eq!(parsed.tokens.total, 3500);
        assert_eq!(parsed.step_trace.len(), 1);
        assert_eq!(parsed.tick_details.len(), 1);
        assert_eq!(parsed.context_utilization.utilization_pct, 57.5);
        assert_eq!(parsed.behavior_metrics.first_mutation_step, Some(3));
        assert_eq!(parsed.harness_anomalies.len(), 1);
    }

    #[test]
    fn build_vessel_config_applies_forced_overrides() {
        use crate::config::{CodingThreadConfig, VesselConfig};

        let base = VesselConfig {
            mission: "test mission".into(),
            data_dir: std::path::PathBuf::from("/tmp/test"),
            coding_thread: CodingThreadConfig {
                enabled: false,
                ..CodingThreadConfig::default()
            },
            ..VesselConfig::default()
        };

        let spec = TaskSpec {
            task: TaskSection {
                name: "test".into(),
                repo_path: "../repos/sample".into(),
                prompt: "do something".into(),
                timeout_secs: 300,
                max_ticks: None,
                difficulty: None,
                language: None,
                tags: vec![],
                source_benchmark: None,
                source_id: None,
                base_commit: None,
                gold_patch: None,
            },
            verify: VerifySection {
                command: "true".into(),
                expected_exit_code: 0,
                test_patch: None,
            },
            vessel_overrides: None,
        };

        let workspace = std::path::Path::new("/tmp/workspace");
        let data_dir = std::path::Path::new("/tmp/bench-data");
        let config =
            HeadlessRunner::build_vessel_config(&base, &spec, workspace, data_dir).unwrap();

        assert!(config.coding_thread.enabled);
        assert_eq!(
            config.coding_thread.workspace_root,
            Some(workspace.to_string_lossy().into_owned())
        );
        assert!(config.coding_thread.return_to_idle_after_completion);
        assert_eq!(config.data_dir, data_dir);
        assert_eq!(
            config.tool_policy.default,
            crate::kernel::policy::PolicyRule::Allow
        );
        assert!(config.tool_policy.rules.is_empty());
    }

    #[test]
    fn build_vessel_config_rejects_removed_legacy_vessel_overrides() {
        use crate::config::{CodingThreadConfig, VesselConfig};

        let base = VesselConfig {
            mission: "test".into(),
            data_dir: std::path::PathBuf::from("/tmp/test"),
            coding_thread: CodingThreadConfig {
                enabled: false,
                ..CodingThreadConfig::default()
            },
            ..VesselConfig::default()
        };

        let mut overrides = toml::map::Map::new();
        overrides.insert("timeout_secs".into(), toml::Value::Integer(50));

        let spec = TaskSpec {
            task: TaskSection {
                name: "test".into(),
                repo_path: ".".into(),
                prompt: "do something".into(),
                timeout_secs: 300,
                max_ticks: None,
                difficulty: None,
                language: None,
                tags: vec![],
                source_benchmark: None,
                source_id: None,
                base_commit: None,
                gold_patch: None,
            },
            verify: VerifySection {
                command: "true".into(),
                expected_exit_code: 0,
                test_patch: None,
            },
            vessel_overrides: Some(toml::Value::Table(overrides)),
        };

        let err = HeadlessRunner::build_vessel_config(
            &base,
            &spec,
            std::path::Path::new("/tmp/ws"),
            std::path::Path::new("/tmp/data"),
        )
        .unwrap_err();

        assert!(
            err.to_string().contains("removed"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn suite_result_pass_rate() {
        let suite = SuiteResult {
            run_id: "test".into(),
            timestamp: Utc::now(),
            tasks: vec![
                make_task_result("a", true, 5, 1000, 2.0),
                make_task_result("b", false, 25, 5000, 10.0),
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
        let (passed, code) = run_verification("true", 0, std::env::temp_dir()).unwrap();
        assert!(passed);
        assert_eq!(code, 0);
    }

    #[test]
    fn run_verification_command_failure() {
        let (passed, code) = run_verification("false", 0, std::env::temp_dir()).unwrap();
        assert!(!passed);
        assert_eq!(code, 1);
    }

    #[test]
    fn run_verification_custom_exit_code() {
        let (passed, code) = run_verification("exit 42", 42, std::env::temp_dir()).unwrap();
        assert!(passed);
        assert_eq!(code, 42);
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
        let mut task = make_task_result("task-a", true, 10, 3000, 5.0);
        task.tool_calls = HashMap::from([(
            "code.edit".into(),
            ToolCallStats {
                calls: 3,
                successes: 2,
                failures: 1,
            },
        )]);
        let suite = SuiteResult {
            run_id: "test-abc".into(),
            timestamp: Utc::now(),
            tasks: vec![task],
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
    fn format_report_shows_model_and_cost() {
        let mut task = make_task_result("add-test", true, 8, 3500, 12.5);
        task.model_used = "claude-sonnet-4-20250514".into();
        task.llm_cost_cents = 1.5;
        task.behavior_metrics = BehaviorMetrics {
            first_file_read_step: Some(2),
            first_semantic_step: Some(3),
            first_mutation_step: Some(4),
            search_steps_before_first_read: 1,
            semantic_steps_before_first_edit: 1,
            ..BehaviorMetrics::default()
        };
        let suite = SuiteResult {
            run_id: "test-123".into(),
            timestamp: Utc::now(),
            tasks: vec![task],
        };

        let report = format_report(&suite);
        assert!(report.contains("claude-sonnet-4-20250514"));
        assert!(report.contains("$0.02") || report.contains("$0.01"));
        assert!(report.contains("3,500") || report.contains("3500"));
        assert!(report.contains("[r:2 sem:3 e:4 s<r:1]"));
        assert!(report.contains("Anomalies:"));
        assert!(report.contains("Median read:"));
        assert!(report.contains("Median semantic:"));
        assert!(report.contains("Median edit:"));
    }

    #[test]
    fn shell_exec_verification_detection_recognizes_cargo_test() {
        let params = serde_json::json!({
            "command": "cargo",
            "args": ["test", "--lib", "parse_config_line"],
        });

        assert!(shell_exec_looks_like_verification(&params));
        assert!(is_verify_action("shell.exec", &params));
    }

    #[test]
    fn compute_behavior_metrics_tracks_search_read_edit_and_verify() {
        let actions = vec![
            BehaviorAction {
                step: 1,
                tick: 1,
                tool_name: "code.grep".into(),
                normalized_target: "parse_config_line".into(),
                file_target: None,
                is_search: true,
                is_read: false,
                is_mutation: false,
                is_verify: false,
                is_semantic: false,
                is_symbol_read: false,
                is_reference_query: false,
                exec_thread_originated: true,
            },
            BehaviorAction {
                step: 2,
                tick: 1,
                tool_name: "code.read".into(),
                normalized_target: "src/lib.rs".into(),
                file_target: Some("src/lib.rs".into()),
                is_search: false,
                is_read: true,
                is_mutation: false,
                is_verify: false,
                is_semantic: false,
                is_symbol_read: false,
                is_reference_query: false,
                exec_thread_originated: true,
            },
            BehaviorAction {
                step: 3,
                tick: 2,
                tool_name: "code.edit".into(),
                normalized_target: "src/lib.rs".into(),
                file_target: Some("src/lib.rs".into()),
                is_search: false,
                is_read: false,
                is_mutation: true,
                is_verify: false,
                is_semantic: false,
                is_symbol_read: false,
                is_reference_query: false,
                exec_thread_originated: true,
            },
            BehaviorAction {
                step: 4,
                tick: 3,
                tool_name: "shell.exec".into(),
                normalized_target: "cargo test --lib".into(),
                file_target: None,
                is_search: false,
                is_read: false,
                is_mutation: false,
                is_verify: true,
                is_semantic: false,
                is_symbol_read: false,
                is_reference_query: false,
                exec_thread_originated: false,
            },
        ];

        let metrics = compute_behavior_metrics(&actions, Some(3));
        assert_eq!(metrics.first_search_step, Some(1));
        assert_eq!(metrics.first_file_read_step, Some(2));
        assert_eq!(metrics.first_mutation_step, Some(3));
        assert_eq!(metrics.first_verify_step, Some(4));
        assert_eq!(metrics.search_steps_before_first_read, 1);
        assert_eq!(metrics.search_steps_after_first_read, 0);
        assert_eq!(metrics.post_edit_search_steps, 0);
        assert_eq!(metrics.distinct_files_read, 1);
        assert_eq!(metrics.distinct_files_modified, 1);
        assert!(metrics.verification_attempted);
        assert!(metrics.verification_after_last_edit);
        assert_eq!(metrics.exec_thread_originated_steps, 3);
        assert_eq!(metrics.master_only_steps, 1);
        assert!(!metrics.zero_edit_terminal);
    }

    #[test]
    fn compute_behavior_metrics_detects_repeated_same_tool_and_target() {
        let actions = vec![
            BehaviorAction {
                step: 1,
                tick: 1,
                tool_name: "code.grep".into(),
                normalized_target: "src".into(),
                file_target: None,
                is_search: true,
                is_read: false,
                is_mutation: false,
                is_verify: false,
                is_semantic: false,
                is_symbol_read: false,
                is_reference_query: false,
                exec_thread_originated: false,
            },
            BehaviorAction {
                step: 2,
                tick: 2,
                tool_name: "code.grep".into(),
                normalized_target: "src".into(),
                file_target: None,
                is_search: true,
                is_read: false,
                is_mutation: false,
                is_verify: false,
                is_semantic: false,
                is_symbol_read: false,
                is_reference_query: false,
                exec_thread_originated: false,
            },
            BehaviorAction {
                step: 3,
                tick: 3,
                tool_name: "code.grep".into(),
                normalized_target: "src".into(),
                file_target: None,
                is_search: true,
                is_read: false,
                is_mutation: false,
                is_verify: false,
                is_semantic: false,
                is_symbol_read: false,
                is_reference_query: false,
                exec_thread_originated: false,
            },
        ];

        let metrics = compute_behavior_metrics(&actions, None);
        assert_eq!(metrics.max_repeated_same_tool_same_target, 3);
        assert!(metrics.zero_edit_terminal);
    }

    #[test]
    fn detect_benchmark_harness_anomalies_flags_missing_sections() {
        let anomalies = detect_benchmark_harness_anomalies(
            4,
            &ContextUtilization {
                total_budget_tokens: 16_384,
                total_used_tokens: 120,
                utilization_pct: 0.73,
                sections: {
                    let mut sections = HashMap::new();
                    sections.insert(
                        "conversations".into(),
                        SectionUtilization {
                            budget_tokens: 200,
                            used_tokens: 0,
                            truncated: false,
                        },
                    );
                    sections.insert(
                        "exec_thread_outputs".into(),
                        SectionUtilization {
                            budget_tokens: 80,
                            used_tokens: 0,
                            truncated: false,
                        },
                    );
                    sections
                },
            },
        );

        assert_eq!(anomalies.len(), 2);
        assert!(anomalies
            .iter()
            .any(|a| a.contains("missing_conversations_context")));
        assert!(anomalies
            .iter()
            .any(|a| a.contains("missing_exec_thread_context")));
    }

    #[test]
    fn detect_benchmark_harness_anomalies_ignores_empty_runs() {
        let anomalies = detect_benchmark_harness_anomalies(0, &ContextUtilization::default());
        assert!(anomalies.is_empty());
    }

    #[test]
    fn format_verbose_step() {
        let step = StepTrace {
            step: 1,
            tick: 1,
            tool_name: "code.read".into(),
            tool_params: serde_json::json!({"file_path": "src/lib.rs"}),
            outcome: "Success".into(),
            output_summary: "45 lines read".into(),
            reasoning: "Read before edit".into(),
            tokens_in: 1200,
            tokens_out: 180,
            latency_ms: 850,
        };

        let formatted = format_step_verbose(&step);
        assert!(formatted.contains("[1]"));
        assert!(formatted.contains("code.read"));
        assert!(formatted.contains("Success"));
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

    #[test]
    fn compiled_context_to_utilization_converts_correctly() {
        use exoskeleton_memory::compiler::SectionResult;

        let compiled = CompiledContext {
            prompt: "test prompt".into(),
            total_tokens: 1800,
            budget: 3200,
            sections: vec![
                SectionResult {
                    name: "system".into(),
                    allocated: 1000,
                    used: 800,
                    truncated: false,
                },
                SectionResult {
                    name: "plan".into(),
                    allocated: 1200,
                    used: 1000,
                    truncated: true,
                },
            ],
            truncated_sections: vec!["plan".into()],
        };

        let util = compiled_context_to_utilization(compiled);
        assert_eq!(util.total_budget_tokens, 3200);
        assert_eq!(util.total_used_tokens, 1800);
        assert!((util.utilization_pct - 56.25).abs() < 0.01);
        assert_eq!(util.sections.len(), 2);
        assert_eq!(util.sections["system"].budget_tokens, 1000);
        assert_eq!(util.sections["system"].used_tokens, 800);
        assert!(!util.sections["system"].truncated);
        assert!(util.sections["plan"].truncated);
    }

    #[test]
    fn compiled_context_to_utilization_zero_budget() {
        let compiled = CompiledContext {
            prompt: String::new(),
            total_tokens: 0,
            budget: 0,
            sections: vec![],
            truncated_sections: vec![],
        };

        let util = compiled_context_to_utilization(compiled);
        assert_eq!(util.utilization_pct, 0.0);
    }

    #[test]
    fn completion_reason_from_reasoning_detects_known_completion_tags() {
        assert_eq!(
            HeadlessRunner::completion_reason_from_reasoning(
                "Finished work\n\n[completion: agent_complete]"
            ),
            Some("AgentComplete")
        );
        // step_limit is intentionally non-terminal — the master loop ticks over
        assert_eq!(
            HeadlessRunner::completion_reason_from_reasoning("notes [completion: step_limit]"),
            None
        );
        assert_eq!(
            HeadlessRunner::completion_reason_from_reasoning(
                "waiting for user [completion: awaiting_input]"
            ),
            Some("AwaitingInput")
        );
    }

    #[test]
    fn format_task_prompt_includes_workspace_path() {
        let prompt = format_task_prompt(Path::new("/tmp/workspace"), "Fix the bug in main.rs");
        assert!(prompt.contains("/tmp/workspace"));
        assert!(prompt.contains("Fix the bug in main.rs"));
    }

    #[test]
    fn completion_reason_from_reasoning_detects_error() {
        let rationale = "some reasoning\n\n[completion: error: LLM call failed]";
        let reason = HeadlessRunner::completion_reason_from_reasoning(rationale);
        assert_eq!(reason, Some("Error"));
    }

    #[test]
    fn completion_reason_from_reasoning_ignores_plain_reasoning() {
        assert_eq!(
            HeadlessRunner::completion_reason_from_reasoning("The task is done."),
            None
        );
    }

    #[test]
    fn completion_reason_from_snapshot_returns_none_without_coding_thread() {
        let snapshot = exoskeleton_core::StateSnapshot::initial(
            exoskeleton_core::VesselId::new(),
            "test".into(),
        );

        assert_eq!(
            HeadlessRunner::completion_reason_from_snapshot(&snapshot),
            None
        );
    }

    #[test]
    fn completion_reason_from_snapshot_requires_idle_completion_reason() {
        let mut snapshot = exoskeleton_core::StateSnapshot::initial(
            exoskeleton_core::VesselId::new(),
            "test".into(),
        );
        snapshot
            .exec_thread_summaries
            .push(exoskeleton_core::ExecThreadSummary {
                thread_id: exoskeleton_core::ThreadId::new(),
                kind: exoskeleton_core::ExecThreadKind::Coding,
                name: "Coding".into(),
                status: exoskeleton_core::ExecThreadStatus::Idle,
                last_output_summary: Some("standing by".into()),
                current_focus: None,
                work_phase: None,
                evidence_complete: false,
                proposal_confidence: None,
                last_completion_reason: None,
            });

        assert_eq!(
            HeadlessRunner::completion_reason_from_snapshot(&snapshot),
            None
        );
    }

    #[test]
    fn completion_reason_from_snapshot_treats_idle_with_completion_reason_as_complete() {
        let mut snapshot = exoskeleton_core::StateSnapshot::initial(
            exoskeleton_core::VesselId::new(),
            "test".into(),
        );
        snapshot
            .exec_thread_summaries
            .push(exoskeleton_core::ExecThreadSummary {
                thread_id: exoskeleton_core::ThreadId::new(),
                kind: exoskeleton_core::ExecThreadKind::Coding,
                name: "Coding".into(),
                status: exoskeleton_core::ExecThreadStatus::Idle,
                last_output_summary: Some("completed work".into()),
                current_focus: None,
                work_phase: None,
                evidence_complete: false,
                proposal_confidence: None,
                last_completion_reason: Some("task completed".into()),
            });

        assert_eq!(
            HeadlessRunner::completion_reason_from_snapshot(&snapshot),
            Some("ExecCompleted".into())
        );
    }

    #[test]
    fn completion_reason_from_snapshot_keeps_completed_as_compatibility_state() {
        let mut snapshot = exoskeleton_core::StateSnapshot::initial(
            exoskeleton_core::VesselId::new(),
            "test".into(),
        );
        snapshot
            .exec_thread_summaries
            .push(exoskeleton_core::ExecThreadSummary {
                thread_id: exoskeleton_core::ThreadId::new(),
                kind: exoskeleton_core::ExecThreadKind::Coding,
                name: "Coding".into(),
                status: exoskeleton_core::ExecThreadStatus::Completed,
                last_output_summary: Some("legacy completed".into()),
                current_focus: None,
                work_phase: None,
                evidence_complete: false,
                proposal_confidence: None,
                last_completion_reason: Some("legacy".into()),
            });

        assert_eq!(
            HeadlessRunner::completion_reason_from_snapshot(&snapshot),
            Some("ExecCompleted".into())
        );
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
            completion_reason: if passed {
                "AgentComplete".into()
            } else {
                "StepLimit".into()
            },
            wall_time_secs: time,
            steps_taken: steps,
            ticks_used: 1,
            tokens: TokenMetrics {
                total_in: tokens * 4 / 5,
                total_out: tokens / 5,
                total: tokens,
                by_phase: HashMap::new(),
            },
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
            behavior_metrics: BehaviorMetrics::default(),
            harness_anomalies: vec![],
            difficulty: None,
            language: None,
            tags: vec![],
            source_benchmark: None,
            source_id: None,
            timestamp: Utc::now(),
        }
    }
}

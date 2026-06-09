//! Cargo test output parser and SWE-bench grading engine.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use exoskeleton_core::ExoError;
use serde::{Deserialize, Serialize};

// ── Types ──────────────────────────────────────────────────────────────────

/// Outcome of a single test case.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TestOutcome {
    Passed,
    Failed,
    Ignored,
}

/// Parsed output from a `cargo test` run.
#[derive(Debug, Clone)]
pub struct CargoTestOutput {
    /// Test name → outcome map.
    pub tests: HashMap<String, TestOutcome>,
    /// Raw stdout captured from the process.
    pub raw_stdout: String,
    /// Raw stderr captured from the process.
    pub raw_stderr: String,
    /// Process exit code (−1 if unavailable).
    pub exit_code: i32,
    /// True when exit code is non-zero and no test results were parsed
    /// (indicates a compilation failure rather than test failures).
    pub compilation_failed: bool,
}

/// Grading result comparing test outcomes against SWE-bench expectations.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SweGrading {
    /// Whether the instance is considered resolved (all F2P pass, all P2P maintained).
    pub resolved: bool,
    /// Number of FAIL_TO_PASS tests that now pass.
    pub f2p_passed: u32,
    /// Total FAIL_TO_PASS tests expected.
    pub f2p_total: u32,
    /// Number of PASS_TO_PASS tests still passing (or not found, treated as maintained).
    pub p2p_passed: u32,
    /// Total PASS_TO_PASS tests expected.
    pub p2p_total: u32,
}

// ── Parsing ────────────────────────────────────────────────────────────────

/// Parse `cargo test` stdout into a test name → outcome map.
///
/// Recognises lines of the form `test <name> ... ok|FAILED|ignored`.
pub fn parse_cargo_test_output(stdout: &str) -> HashMap<String, TestOutcome> {
    let mut results = HashMap::new();
    for line in stdout.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("test ") {
            if let Some((name, outcome_str)) = rest.rsplit_once(" ... ") {
                let name = name.trim();
                let outcome = match outcome_str.trim() {
                    "ok" => TestOutcome::Passed,
                    "FAILED" => TestOutcome::Failed,
                    "ignored" => TestOutcome::Ignored,
                    _ => continue,
                };
                results.insert(name.to_string(), outcome);
            }
        }
    }
    results
}

// ── Lookup ─────────────────────────────────────────────────────────────────

/// Look up a test by exact name, falling back to suffix match (`::name`).
fn lookup_test(parsed: &HashMap<String, TestOutcome>, name: &str) -> Option<TestOutcome> {
    if let Some(&outcome) = parsed.get(name) {
        return Some(outcome);
    }
    // Suffix fallback: e.g. "my_test" matches "module::submod::my_test".
    let suffix = format!("::{name}");
    for (full_name, &outcome) in parsed {
        if full_name.ends_with(&suffix) || full_name == name {
            return Some(outcome);
        }
    }
    None
}

/// Public wrapper around [`lookup_test`] for external callers.
pub fn lookup_test_pub(parsed: &HashMap<String, TestOutcome>, name: &str) -> Option<TestOutcome> {
    lookup_test(parsed, name)
}

// ── Grading ────────────────────────────────────────────────────────────────

/// Grade parsed test results against SWE-bench FAIL_TO_PASS and PASS_TO_PASS lists.
///
/// - Every FAIL_TO_PASS test must now be `Passed`.
/// - Every PASS_TO_PASS test must still be `Passed` **or** not found in the output
///   (treated as maintained — the test may belong to a different crate or have been
///   renamed).
pub fn grade(
    parsed: &HashMap<String, TestOutcome>,
    fail_to_pass: &[String],
    pass_to_pass: &[String],
) -> SweGrading {
    let f2p_total = fail_to_pass.len() as u32;
    let p2p_total = pass_to_pass.len() as u32;

    let f2p_passed = fail_to_pass
        .iter()
        .filter(|name| lookup_test(parsed, name) == Some(TestOutcome::Passed))
        .count() as u32;

    let p2p_passed = pass_to_pass
        .iter()
        .filter(|name| matches!(lookup_test(parsed, name), Some(TestOutcome::Passed) | None))
        .count() as u32;

    let resolved = f2p_passed == f2p_total && p2p_passed == p2p_total;

    SweGrading {
        resolved,
        f2p_passed,
        f2p_total,
        p2p_passed,
        p2p_total,
    }
}

// ── Patch application ──────────────────────────────────────────────────────

/// Apply a unified diff to a workspace via `git apply`, falling back to `--3way`.
pub fn apply_patch(workspace: &Path, patch_text: &str) -> Result<(), ExoError> {
    let patch_file = workspace.join(".swe-bench-patch.diff");
    std::fs::write(&patch_file, patch_text)
        .map_err(|e| ExoError::Engine(format!("failed to write patch file: {e}")))?;

    // First attempt: direct apply.
    let output = Command::new("git")
        .args(["apply", "--verbose", &patch_file.to_string_lossy()])
        .current_dir(workspace)
        .output()
        .map_err(|e| ExoError::Engine(format!("git apply failed to execute: {e}")))?;

    if output.status.success() {
        let _ = std::fs::remove_file(&patch_file);
        return Ok(());
    }

    // Fallback: 3-way merge.
    let output = Command::new("git")
        .args(["apply", "--3way", &patch_file.to_string_lossy()])
        .current_dir(workspace)
        .output()
        .map_err(|e| {
            let _ = std::fs::remove_file(&patch_file);
            ExoError::Engine(format!("git apply --3way failed to execute: {e}"))
        })?;

    let _ = std::fs::remove_file(&patch_file);

    if output.status.success() {
        Ok(())
    } else {
        Err(ExoError::Engine(format!(
            "git apply failed (both direct and 3way): {}",
            String::from_utf8_lossy(&output.stderr)
        )))
    }
}

// ── Test execution ─────────────────────────────────────────────────────────

/// Run `cargo test --no-fail-fast` in `workspace` with a timeout.
pub fn run_cargo_test(workspace: &Path, timeout: Duration) -> Result<CargoTestOutput, ExoError> {
    let child = Command::new("cargo")
        .args(["test", "--no-fail-fast"])
        .current_dir(workspace)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| ExoError::Engine(format!("cargo test failed to spawn: {e}")))?;

    let output = wait_with_timeout(child, timeout)?;
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let exit_code = output.status.code().unwrap_or(-1);
    let tests = parse_cargo_test_output(&stdout);
    let compilation_failed = exit_code != 0 && tests.is_empty();

    Ok(CargoTestOutput {
        tests,
        raw_stdout: stdout,
        raw_stderr: stderr,
        exit_code,
        compilation_failed,
    })
}

/// Run only the SWE-bench FAIL_TO_PASS tests in likely Cargo package roots.
///
/// This is intentionally narrower than [`run_cargo_test`]. Local smoke runs need
/// fast signal on whether the agent fixed the target regression; broad
/// workspace-level test execution is still available, but it can time out on
/// large Rust workspaces before producing useful grading evidence.
pub fn run_cargo_fail_to_pass_tests(
    workspace: &Path,
    model_patch: &str,
    fail_to_pass: &[String],
    timeout: Duration,
) -> Result<CargoTestOutput, ExoError> {
    if fail_to_pass.is_empty() {
        return Ok(empty_cargo_test_output());
    }

    let roots = candidate_cargo_roots_from_patch(workspace, model_patch);
    let mut aggregate = empty_cargo_test_output();

    for expected in fail_to_pass {
        let filters = test_filters(expected);

        'roots: for root in &roots {
            for filter in &filters {
                let output = run_cargo_test_filter(root, filter, timeout)?;
                let observed = lookup_test(&output.tests, expected);
                merge_cargo_test_output(&mut aggregate, root, filter, output);

                if observed.is_some() {
                    break 'roots;
                }
            }
        }
    }

    Ok(aggregate)
}

fn run_cargo_test_filter(
    manifest_root: &Path,
    filter: &str,
    timeout: Duration,
) -> Result<CargoTestOutput, ExoError> {
    let child = Command::new("cargo")
        .args(["test", "--no-fail-fast", filter, "--", "--nocapture"])
        .current_dir(manifest_root)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| ExoError::Engine(format!("cargo test failed to spawn: {e}")))?;

    let output = wait_with_timeout(child, timeout)?;
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let exit_code = output.status.code().unwrap_or(-1);
    let tests = parse_cargo_test_output(&stdout);
    let compilation_failed = exit_code != 0 && tests.is_empty();

    Ok(CargoTestOutput {
        tests,
        raw_stdout: stdout,
        raw_stderr: stderr,
        exit_code,
        compilation_failed,
    })
}

fn empty_cargo_test_output() -> CargoTestOutput {
    CargoTestOutput {
        tests: HashMap::new(),
        raw_stdout: String::new(),
        raw_stderr: String::new(),
        exit_code: 0,
        compilation_failed: false,
    }
}

fn merge_cargo_test_output(
    aggregate: &mut CargoTestOutput,
    root: &Path,
    filter: &str,
    output: CargoTestOutput,
) {
    aggregate.raw_stdout.push_str(&format!(
        "\n===== cargo test --no-fail-fast {filter} @ {} =====\n",
        root.display()
    ));
    aggregate.raw_stdout.push_str(&output.raw_stdout);

    aggregate.raw_stderr.push_str(&format!(
        "\n===== cargo test --no-fail-fast {filter} @ {} =====\n",
        root.display()
    ));
    aggregate.raw_stderr.push_str(&output.raw_stderr);

    for (name, outcome) in output.tests {
        aggregate.tests.insert(name, outcome);
    }
    if output.exit_code != 0 {
        aggregate.exit_code = output.exit_code;
    }
    aggregate.compilation_failed |= output.compilation_failed;
}

fn test_filters(name: &str) -> Vec<String> {
    let mut filters = vec![name.to_string()];
    if let Some(short_name) = name.rsplit("::").next() {
        if short_name != name {
            filters.push(short_name.to_string());
        }
    }
    filters
}

fn candidate_cargo_roots_from_patch(workspace: &Path, patch_text: &str) -> Vec<PathBuf> {
    let mut roots = Vec::new();

    for rel_path in modified_paths_from_patch(patch_text) {
        if let Some(root) = nearest_cargo_root_for_path(workspace, &rel_path) {
            if !roots.iter().any(|existing| existing == &root) {
                roots.push(root);
            }
        }
    }

    if roots.is_empty() {
        roots.push(workspace.to_path_buf());
    }

    roots
}

fn modified_paths_from_patch(patch_text: &str) -> Vec<PathBuf> {
    let mut paths = Vec::new();

    for line in patch_text.lines() {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            let mut parts = rest.split_whitespace();
            let _old_path = parts.next();
            if let Some(new_path) = parts.next().and_then(normalize_patch_path) {
                push_unique_path(&mut paths, new_path);
            }
            continue;
        }

        if let Some(rest) = line.strip_prefix("+++ ") {
            if let Some(new_path) = rest
                .split_whitespace()
                .next()
                .and_then(normalize_patch_path)
            {
                push_unique_path(&mut paths, new_path);
            }
        }
    }

    paths
}

fn normalize_patch_path(raw: &str) -> Option<PathBuf> {
    let raw = raw.trim().trim_matches('"');
    if raw.is_empty() || raw == "/dev/null" {
        return None;
    }
    let rel = raw
        .strip_prefix("a/")
        .or_else(|| raw.strip_prefix("b/"))
        .unwrap_or(raw);
    if rel.is_empty() || Path::new(rel).is_absolute() {
        return None;
    }
    Some(PathBuf::from(rel))
}

fn push_unique_path(paths: &mut Vec<PathBuf>, path: PathBuf) {
    if !paths.iter().any(|existing| existing == &path) {
        paths.push(path);
    }
}

fn nearest_cargo_root_for_path(workspace: &Path, rel_path: &Path) -> Option<PathBuf> {
    let mut cursor = workspace.join(rel_path);
    if !cursor.is_dir() {
        cursor.pop();
    }

    loop {
        if cursor.join("Cargo.toml").is_file() {
            return Some(cursor);
        }
        if cursor == workspace {
            break;
        }
        if !cursor.pop() {
            break;
        }
    }

    if workspace.join("Cargo.toml").is_file() {
        Some(workspace.to_path_buf())
    } else {
        None
    }
}

/// Wait for a child process to finish, killing it if `timeout` elapses.
fn wait_with_timeout(
    mut child: std::process::Child,
    timeout: Duration,
) -> Result<std::process::Output, ExoError> {
    let start = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let stdout = child.stdout.take().map_or_else(Vec::new, |mut s| {
                    let mut buf = Vec::new();
                    std::io::Read::read_to_end(&mut s, &mut buf).unwrap_or(0);
                    buf
                });
                let stderr = child.stderr.take().map_or_else(Vec::new, |mut s| {
                    let mut buf = Vec::new();
                    std::io::Read::read_to_end(&mut s, &mut buf).unwrap_or(0);
                    buf
                });
                return Ok(std::process::Output {
                    status,
                    stdout,
                    stderr,
                });
            }
            Ok(None) => {
                if start.elapsed() > timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(ExoError::Engine(format!(
                        "cargo test timed out after {}s",
                        timeout.as_secs()
                    )));
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => return Err(ExoError::Engine(format!("wait failed: {e}"))),
        }
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── parse_cargo_test_output tests ──

    #[test]
    fn parse_basic_pass_fail_ignored() {
        let output = "\
running 3 tests
test alpha::test_one ... ok
test beta::test_two ... FAILED
test gamma::test_three ... ignored

test result: FAILED. 1 passed; 1 failed; 1 ignored; 0 measured; 0 filtered out
";
        let parsed = parse_cargo_test_output(output);
        assert_eq!(parsed.len(), 3);
        assert_eq!(parsed["alpha::test_one"], TestOutcome::Passed);
        assert_eq!(parsed["beta::test_two"], TestOutcome::Failed);
        assert_eq!(parsed["gamma::test_three"], TestOutcome::Ignored);
    }

    #[test]
    fn parse_empty_output() {
        let parsed = parse_cargo_test_output("");
        assert!(parsed.is_empty());
    }

    #[test]
    fn parse_compilation_error_no_tests() {
        let output = "\
error[E0308]: mismatched types
  --> src/lib.rs:10:5
   |
10 |     42u32
   |     ^^^^^ expected `&str`, found `u32`

error: could not compile `my_crate` due to previous error
";
        let parsed = parse_cargo_test_output(output);
        assert!(parsed.is_empty());
    }

    #[test]
    fn parse_single_test_pass() {
        let output = "\
running 1 test
test my_module::tests::it_works ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
";
        let parsed = parse_cargo_test_output(output);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed["my_module::tests::it_works"], TestOutcome::Passed);
    }

    // ── grade tests ──

    #[test]
    fn grade_all_resolved() {
        let mut parsed = HashMap::new();
        parsed.insert("fix_bug".to_string(), TestOutcome::Passed);
        parsed.insert("existing_feature".to_string(), TestOutcome::Passed);

        let result = grade(
            &parsed,
            &["fix_bug".to_string()],
            &["existing_feature".to_string()],
        );

        assert!(result.resolved);
        assert_eq!(result.f2p_passed, 1);
        assert_eq!(result.f2p_total, 1);
        assert_eq!(result.p2p_passed, 1);
        assert_eq!(result.p2p_total, 1);
    }

    #[test]
    fn grade_f2p_not_resolved() {
        let mut parsed = HashMap::new();
        parsed.insert("fix_bug".to_string(), TestOutcome::Failed);
        parsed.insert("existing_feature".to_string(), TestOutcome::Passed);

        let result = grade(
            &parsed,
            &["fix_bug".to_string()],
            &["existing_feature".to_string()],
        );

        assert!(!result.resolved);
        assert_eq!(result.f2p_passed, 0);
        assert_eq!(result.f2p_total, 1);
    }

    #[test]
    fn grade_p2p_broken() {
        let mut parsed = HashMap::new();
        parsed.insert("fix_bug".to_string(), TestOutcome::Passed);
        parsed.insert("existing_feature".to_string(), TestOutcome::Failed);

        let result = grade(
            &parsed,
            &["fix_bug".to_string()],
            &["existing_feature".to_string()],
        );

        assert!(!result.resolved);
        assert_eq!(result.f2p_passed, 1);
        assert_eq!(result.p2p_passed, 0);
        assert_eq!(result.p2p_total, 1);
    }

    #[test]
    fn grade_suffix_match_fallback() {
        let mut parsed = HashMap::new();
        parsed.insert(
            "crate::module::tests::fix_bug".to_string(),
            TestOutcome::Passed,
        );

        let result = grade(&parsed, &["fix_bug".to_string()], &[]);

        assert!(result.resolved);
        assert_eq!(result.f2p_passed, 1);
        assert_eq!(result.f2p_total, 1);
    }

    #[test]
    fn grade_p2p_not_found_treated_as_maintained() {
        let parsed = HashMap::new(); // empty — no tests found at all

        let result = grade(&parsed, &[], &["vanished_test".to_string()]);

        // Not found → treated as maintained.
        assert!(result.resolved);
        assert_eq!(result.p2p_passed, 1);
        assert_eq!(result.p2p_total, 1);
    }

    #[test]
    fn grade_empty_lists() {
        let parsed = HashMap::new();

        let result = grade(&parsed, &[], &[]);

        assert!(result.resolved);
        assert_eq!(result.f2p_passed, 0);
        assert_eq!(result.f2p_total, 0);
        assert_eq!(result.p2p_passed, 0);
        assert_eq!(result.p2p_total, 0);
    }

    #[test]
    fn candidate_roots_select_nearest_cargo_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let crate_dir = dir.path().join("crates/example");
        std::fs::create_dir_all(crate_dir.join("src")).unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "[workspace]\n").unwrap();
        std::fs::write(
            crate_dir.join("Cargo.toml"),
            "[package]\nname = \"example\"\n",
        )
        .unwrap();

        let patch = "\
diff --git a/crates/example/src/lib.rs b/crates/example/src/lib.rs
--- a/crates/example/src/lib.rs
+++ b/crates/example/src/lib.rs
";

        let roots = candidate_cargo_roots_from_patch(dir.path(), patch);

        assert_eq!(roots, vec![crate_dir]);
    }

    #[test]
    fn candidate_roots_fall_back_to_workspace_for_empty_patch() {
        let dir = tempfile::tempdir().unwrap();

        let roots = candidate_cargo_roots_from_patch(dir.path(), "");

        assert_eq!(roots, vec![dir.path().to_path_buf()]);
    }

    #[test]
    fn test_filters_try_full_name_then_short_name() {
        assert_eq!(
            test_filters("crate::module::tests::fix_regression"),
            vec![
                "crate::module::tests::fix_regression".to_string(),
                "fix_regression".to_string()
            ]
        );
    }
}

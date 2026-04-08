//! Cargo test output parser and SWE-bench grading engine.

use std::collections::HashMap;
use std::path::Path;
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
/// Will be used by SweBenchRunner in Task 6.
#[allow(dead_code)]
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
}

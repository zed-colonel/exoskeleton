//! Git state context injection (E10-S2, W-102).
//!
//! Captures git state at Perceive time for Orient context injection.
//! Uses std::process::Command, not a WI connector.

use std::path::Path;
use std::process::Command;

/// Captured git state for context injection.
#[derive(Debug, Clone)]
pub struct GitState {
    /// Current branch name.
    pub branch: String,
    /// Number of modified (unstaged) files.
    pub modified: u32,
    /// Number of staged files.
    pub staged: u32,
    /// Number of untracked files.
    pub untracked: u32,
    /// Recent commit subject lines.
    pub recent_commits: Vec<String>,
}

/// Capture git state from a workspace directory.
pub fn capture_git_state(workspace_root: &Path) -> Option<GitState> {
    let branch_output = Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .current_dir(workspace_root)
        .output()
        .ok()?;

    if !branch_output.status.success() {
        return None;
    }

    let branch = String::from_utf8_lossy(&branch_output.stdout)
        .trim()
        .to_string();
    if branch.is_empty() {
        return None;
    }

    let status_output = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(workspace_root)
        .output()
        .ok()?;

    let mut modified = 0u32;
    let mut staged = 0u32;
    let mut untracked = 0u32;
    for line in String::from_utf8_lossy(&status_output.stdout).lines() {
        if line.len() < 2 {
            continue;
        }
        let bytes = line.as_bytes();
        let index_status = bytes[0];
        let worktree_status = bytes[1];
        if index_status == b'?' && worktree_status == b'?' {
            untracked += 1;
            continue;
        }
        if index_status != b' ' && index_status != b'?' {
            staged += 1;
        }
        if worktree_status != b' ' && worktree_status != b'?' {
            modified += 1;
        }
    }

    let log_output = Command::new("git")
        .args(["log", "--oneline", "--no-decorate", "-5"])
        .current_dir(workspace_root)
        .output()
        .ok()?;
    let recent_commits = String::from_utf8_lossy(&log_output.stdout)
        .lines()
        .map(|line| line.to_string())
        .collect::<Vec<_>>();

    Some(GitState {
        branch,
        modified,
        staged,
        untracked,
        recent_commits,
    })
}

/// Render a GitState as a section string for context compilation.
pub fn render_git_context(state: &GitState) -> String {
    let mut out = String::from("=== GIT CONTEXT ===\n");
    out.push_str(&format!("Branch: {}\n", state.branch));

    if state.modified == 0 && state.staged == 0 && state.untracked == 0 {
        out.push_str("Working tree: clean\n");
    } else {
        let mut parts = Vec::new();
        if state.modified > 0 {
            parts.push(format!("{} modified", state.modified));
        }
        if state.staged > 0 {
            parts.push(format!("{} staged", state.staged));
        }
        if state.untracked > 0 {
            parts.push(format!("{} untracked", state.untracked));
        }
        out.push_str(&format!("Status: {}\n", parts.join(", ")));
    }

    if !state.recent_commits.is_empty() {
        out.push_str("Recent commits:\n");
        for commit in &state.recent_commits {
            out.push_str(&format!("  {commit}\n"));
        }
    }

    out
}

/// Render optional git state, returning empty string if absent.
pub fn render_git_context_optional(state: Option<&GitState>) -> String {
    state.map(render_git_context).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_in_git_repo() {
        let repo_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf();

        let state = capture_git_state(&repo_root);
        assert!(state.is_some(), "should detect git repo");
        let state = state.unwrap();
        assert!(!state.branch.is_empty());
        assert!(!state.recent_commits.is_empty());
    }

    #[test]
    fn capture_in_non_git_dir() {
        let dir = tempfile::tempdir().unwrap();
        let state = capture_git_state(dir.path());
        assert!(state.is_none(), "should return None for non-git dirs");
    }

    #[test]
    fn render_git_state() {
        let state = GitState {
            branch: "main".into(),
            modified: 2,
            staged: 1,
            untracked: 3,
            recent_commits: vec![
                "abc1234 Fix parser bug".into(),
                "def5678 Add test for config".into(),
            ],
        };
        let rendered = render_git_context(&state);
        assert!(rendered.contains("=== GIT CONTEXT ==="));
        assert!(rendered.contains("Branch: main"));
        assert!(rendered.contains("2 modified"));
        assert!(rendered.contains("1 staged"));
        assert!(rendered.contains("3 untracked"));
        assert!(rendered.contains("abc1234"));
        assert!(rendered.contains("Fix parser bug"));
    }

    #[test]
    fn render_clean_status() {
        let state = GitState {
            branch: "feature/test".into(),
            modified: 0,
            staged: 0,
            untracked: 0,
            recent_commits: vec!["abc1234 Initial commit".into()],
        };
        let rendered = render_git_context(&state);
        assert!(rendered.contains("Branch: feature/test"));
        assert!(rendered.contains("clean"));
    }

    #[test]
    fn render_empty_state_returns_empty() {
        let rendered = render_git_context_optional(None);
        assert!(rendered.is_empty());
    }
}

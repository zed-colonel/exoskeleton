//! Bare clone cache and git worktree management for SWE-bench repos.

use std::path::{Path, PathBuf};
use std::process::Command;

use exoskeleton_core::ExoError;

/// Manages a cache of bare git clones and creates per-task worktrees.
pub struct RepoCache {
    cache_dir: PathBuf,
}

impl RepoCache {
    pub fn new(cache_dir: PathBuf) -> Self {
        Self { cache_dir }
    }

    /// Ensure a repo is available in the cache (bare clone or fetch).
    /// Returns the path to the bare repo directory.
    pub fn ensure_repo(&self, repo: &str) -> Result<PathBuf, ExoError> {
        let bare_path = self.bare_repo_path(repo);

        if bare_path.exists() {
            // Prune stale worktrees, then fetch latest
            Self::git_command(&bare_path, &["worktree", "prune"])?;
            Self::git_command(&bare_path, &["fetch", "--all", "--quiet"])?;
            tracing::debug!(repo, path = %bare_path.display(), "repo cache hit, fetched latest");
        } else {
            // First time: bare clone
            if let Some(parent) = bare_path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    ExoError::Engine(format!("failed to create repo cache dir: {e}"))
                })?;
            }
            let url = format!("https://github.com/{repo}.git");
            tracing::info!(repo, url = %url, "cloning repo (bare)");
            let output = Command::new("git")
                .args(["clone", "--bare", &url, &bare_path.to_string_lossy()])
                .output()
                .map_err(|e| ExoError::Engine(format!("git clone failed to execute: {e}")))?;
            if !output.status.success() {
                return Err(ExoError::Engine(format!(
                    "git clone --bare {} failed: {}",
                    url,
                    String::from_utf8_lossy(&output.stderr)
                )));
            }
        }

        Ok(bare_path)
    }

    /// Create an isolated worktree at a specific commit in a tempdir.
    /// Returns a `WorktreeGuard` that removes the worktree on drop.
    pub fn prepare_worktree(&self, repo: &str, commit: &str) -> Result<WorktreeGuard, ExoError> {
        let bare_path = self.ensure_repo(repo)?;

        let temp_dir = tempfile::tempdir()
            .map_err(|e| ExoError::Engine(format!("tempdir creation failed: {e}")))?;
        let worktree_path = temp_dir.path().to_path_buf();

        let output = Command::new("git")
            .args([
                "worktree",
                "add",
                "--detach",
                &worktree_path.to_string_lossy(),
                commit,
            ])
            .current_dir(&bare_path)
            .output()
            .map_err(|e| ExoError::Engine(format!("git worktree add failed: {e}")))?;

        if !output.status.success() {
            return Err(ExoError::Engine(format!(
                "git worktree add at {} failed: {}",
                commit,
                String::from_utf8_lossy(&output.stderr)
            )));
        }

        Ok(WorktreeGuard {
            bare_path,
            worktree_path,
            temp_dir: Some(temp_dir),
        })
    }

    /// Path to the bare repo in the cache.
    fn bare_repo_path(&self, repo: &str) -> PathBuf {
        // repo = "owner/name" -> cache_dir/owner/name.git
        let parts: Vec<&str> = repo.splitn(2, '/').collect();
        if parts.len() == 2 {
            self.cache_dir
                .join(parts[0])
                .join(format!("{}.git", parts[1]))
        } else {
            self.cache_dir.join(format!("{repo}.git"))
        }
    }

    fn git_command(dir: &Path, args: &[&str]) -> Result<(), ExoError> {
        let output = Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .map_err(|e| ExoError::Engine(format!("git {:?} failed: {e}", args)))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            // Worktree prune failures are non-fatal
            if args.get(..2) == Some(&["worktree", "prune"]) {
                tracing::debug!(stderr = %stderr, "git worktree prune warning");
                return Ok(());
            }
            return Err(ExoError::Engine(format!("git {:?} failed: {stderr}", args)));
        }
        Ok(())
    }
}

/// RAII guard that removes the worktree on drop.
pub struct WorktreeGuard {
    bare_path: PathBuf,
    worktree_path: PathBuf,
    temp_dir: Option<tempfile::TempDir>,
}

impl WorktreeGuard {
    /// Path to the worktree (the workspace for the benchmark task).
    pub fn path(&self) -> &Path {
        &self.worktree_path
    }

    /// Preserve the worktree (don't clean up on drop).
    /// Returns the path to the preserved directory.
    pub fn keep(mut self) -> PathBuf {
        let path = self.worktree_path.clone();
        // Remove the .git file to detach the worktree from the bare repo,
        // but leave the directory and all files intact.
        let git_file = self.worktree_path.join(".git");
        let _ = std::fs::remove_file(&git_file);
        // Prune the now-stale worktree entry from the bare repo
        let _ = Command::new("git")
            .args(["worktree", "prune"])
            .current_dir(&self.bare_path)
            .output();
        // Prevent TempDir from deleting the directory on drop
        if let Some(temp) = self.temp_dir.take() {
            let preserved = temp.keep();
            tracing::info!(path = %preserved.display(), "worktree preserved for post-mortem");
        }
        path
    }
}

impl Drop for WorktreeGuard {
    fn drop(&mut self) {
        // If keep() was called, temp_dir is already None and the directory persists.
        if self.temp_dir.is_some() {
            let _ = Command::new("git")
                .args([
                    "worktree",
                    "remove",
                    "--force",
                    &self.worktree_path.to_string_lossy(),
                ])
                .current_dir(&self.bare_path)
                .output();
        }
        // TempDir drops automatically when self.temp_dir (Option) drops
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_repo_path_formats_correctly() {
        let cache = RepoCache::new(PathBuf::from("/tmp/repos"));
        assert_eq!(
            cache.bare_repo_path("tokio-rs/tokio"),
            PathBuf::from("/tmp/repos/tokio-rs/tokio.git")
        );
        assert_eq!(
            cache.bare_repo_path("owner/repo"),
            PathBuf::from("/tmp/repos/owner/repo.git")
        );
        assert_eq!(
            cache.bare_repo_path("solo-repo"),
            PathBuf::from("/tmp/repos/solo-repo.git")
        );
    }
}

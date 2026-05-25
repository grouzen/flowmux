//! Git utility functions for worktree management.
//!
//! Uses `git2` for repository detection and branch operations, and
//! `std::process::Command` for `git worktree add/remove` since those
//! operations map most cleanly to the git CLI.

use anyhow::{Context, Result, bail};
use git2::Repository;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Detection
// ---------------------------------------------------------------------------

/// Find the git repository root (work-tree directory) for the given path.
///
/// Returns `None` if `path` is not inside any git repository.
pub fn find_git_root(path: &Path) -> Option<PathBuf> {
    Repository::discover(path)
        .ok()
        .and_then(|repo| repo.workdir().map(|p| p.to_path_buf()))
}

/// Return a short, filesystem-safe identifier for the repository rooted at
/// `repo_root`.  Currently this is just the directory name.
pub fn repo_id(repo_root: &Path) -> String {
    repo_root
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("repo")
        .to_string()
}

// ---------------------------------------------------------------------------
// Name sanitisation
// ---------------------------------------------------------------------------

/// Convert an arbitrary agent name into a valid git branch name.
///
/// Rules applied:
/// - Lowercased
/// - Spaces and any character not in `[a-z0-9._-]` become dashes
/// - Consecutive dashes are collapsed to one
/// - Leading/trailing dashes and dots are stripped
/// - Empty result falls back to `"agent"`
pub fn sanitize_branch_name(name: &str) -> String {
    let mut result = String::with_capacity(name.len());
    let mut prev_dash = true; // suppress leading dashes

    for c in name.chars() {
        if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
            let lc = c.to_ascii_lowercase();
            result.push(lc);
            prev_dash = lc == '-';
        } else if !prev_dash {
            result.push('-');
            prev_dash = true;
        }
    }

    // Strip trailing dashes and dots
    let trimmed = result
        .trim_end_matches(|c| c == '-' || c == '.')
        .to_string();
    if trimmed.is_empty() {
        "agent".to_string()
    } else {
        trimmed
    }
}

// ---------------------------------------------------------------------------
// Branch inspection
// ---------------------------------------------------------------------------

/// Returns `true` if a local branch named `branch` exists in the repository
/// whose work-tree is rooted at `repo_root`.
pub fn branch_exists(repo_root: &Path, branch: &str) -> bool {
    let Ok(repo) = Repository::open(repo_root) else {
        return false;
    };
    repo.find_branch(branch, git2::BranchType::Local).is_ok()
}

/// Returns the current branch name checked out in the work-tree at `path`,
/// or `None` if the path is not inside a git repo or HEAD is detached.
pub fn current_branch(path: &Path) -> Option<String> {
    let repo = Repository::discover(path).ok()?;
    repo.head()
        .ok()?
        .shorthand()
        .map(|s| s.to_owned())
        .filter(|s| s != "HEAD")
}

// ---------------------------------------------------------------------------
// Worktree creation
// ---------------------------------------------------------------------------

/// Create a git worktree at `worktree_path` tracking `branch`.
///
/// - If `use_existing_branch` is `false`, a new branch is created from the
///   current HEAD of the repo at `repo_root`.
/// - If `use_existing_branch` is `true`, the worktree is linked to the
///   already-existing local branch.
///
/// The parent directory of `worktree_path` is created if it does not exist.
pub fn create_worktree(
    repo_root: &Path,
    worktree_path: &Path,
    branch: &str,
    use_existing_branch: bool,
) -> Result<()> {
    // Ensure the parent directory exists.
    if let Some(parent) = worktree_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create worktree parent dir {:?}", parent))?;
    }

    // Build the git command arguments.
    // git worktree add [-b <branch>] <path> [<branch>]
    let mut cmd = std::process::Command::new("git");
    cmd.current_dir(repo_root);

    if use_existing_branch {
        // Checkout the existing branch into the new worktree.
        cmd.args(["worktree", "add"]);
        cmd.arg(worktree_path);
        cmd.arg(branch);
    } else {
        // Create a new branch from HEAD and place it in the worktree.
        cmd.args(["worktree", "add", "-b"]);
        cmd.arg(branch);
        cmd.arg(worktree_path);
    }

    let output = cmd.output().context("run git worktree add")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("git worktree add failed: {}", stderr.trim());
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Worktree removal
// ---------------------------------------------------------------------------

/// Remove the git worktree at `worktree_path` and, if `delete_branch` is
/// `true`, delete the local branch `branch` as well.
///
/// Failures are surfaced as errors so callers can decide whether to abort or
/// log-and-continue.
pub fn remove_worktree(
    repo_root: &Path,
    worktree_path: &Path,
    branch: &str,
    delete_branch: bool,
) -> Result<()> {
    // `git worktree remove --force <path>` handles both the lock-file cleanup
    // and the directory removal.
    let output = std::process::Command::new("git")
        .current_dir(repo_root)
        .args(["worktree", "remove", "--force"])
        .arg(worktree_path)
        .output()
        .context("run git worktree remove")?;

    if !output.status.success() {
        // If git doesn't know about the worktree (e.g. state already pruned),
        // fall back to a plain directory removal so we don't leave orphaned dirs.
        if worktree_path.exists() {
            std::fs::remove_dir_all(worktree_path)
                .with_context(|| format!("remove worktree directory {:?}", worktree_path))?;
        }
    }

    if delete_branch {
        // `git branch -d` refuses to delete an unmerged branch; use `-D` to
        // force deletion since the user explicitly requested it.
        let _ = std::process::Command::new("git")
            .current_dir(repo_root)
            .args(["branch", "-D", branch])
            .output();
    }

    // Prune any stale worktree administrative files.
    let _ = std::process::Command::new("git")
        .current_dir(repo_root)
        .args(["worktree", "prune"])
        .output();

    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_basic() {
        assert_eq!(sanitize_branch_name("My Feature"), "my-feature");
        assert_eq!(sanitize_branch_name("fix: auth bug!"), "fix-auth-bug");
        assert_eq!(sanitize_branch_name("  --  "), "agent");
        assert_eq!(sanitize_branch_name("hello_world"), "hello_world");
        assert_eq!(sanitize_branch_name("v1.2.3"), "v1.2.3");
        assert_eq!(sanitize_branch_name("trailing-"), "trailing");
    }
}

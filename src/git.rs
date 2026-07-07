//! Git utility functions for worktree management.
//!
//! Uses `git2` for repository detection and branch operations, and
//! `std::process::Command` for `git worktree add/remove` since those
//! operations map most cleanly to the git CLI.

use anyhow::{Context, Result, bail};
use git2::Repository;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorktreeStartPoint<'a> {
    Head,
    Ref(&'a str),
}

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
/// - Spaces and any character not in `[a-zA-Z0-9._-]` become dashes
/// - Consecutive dashes are collapsed to one
/// - Leading/trailing dashes and dots are stripped
/// - Empty result falls back to `"agent"`
pub fn sanitize_branch_name(name: &str) -> String {
    let mut result = String::with_capacity(name.len());
    let mut prev_dash = true; // suppress leading dashes

    for c in name.chars() {
        if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
            result.push(c);
            prev_dash = c == '-';
        } else if !prev_dash {
            result.push('-');
            prev_dash = true;
        }
    }

    // Strip trailing dashes and dots
    let trimmed = result.trim_end_matches(['-', '.']).to_string();
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

/// Returns local and remote-tracking branch names suitable for worktree
/// creation. Remote symbolic refs such as `origin/HEAD` are omitted.
pub fn list_branch_refs(repo_root: &Path) -> Vec<String> {
    let Ok(repo) = Repository::open(repo_root) else {
        return Vec::new();
    };

    let mut refs = Vec::new();
    let Ok(branches) = repo.branches(None) else {
        return refs;
    };

    for item in branches.flatten() {
        let (branch, kind) = item;
        let Some(name) = branch.name().ok().flatten() else {
            continue;
        };
        let normalized = if kind == git2::BranchType::Remote {
            name.strip_prefix("remotes/").unwrap_or(name)
        } else {
            name
        };
        if kind == git2::BranchType::Remote && normalized.ends_with("/HEAD") {
            continue;
        }
        refs.push(normalized.to_string());
    }

    refs.sort();
    refs.dedup();
    refs
}

pub fn validate_local_branch_name(branch: &str) -> Result<()> {
    let branch = branch.trim();
    if branch.is_empty() {
        bail!("new branch name is required");
    }

    let full_ref = format!("refs/heads/{branch}");
    if !git2::Reference::is_valid_name(&full_ref) {
        bail!("invalid local branch name: {branch}");
    }

    Ok(())
}

/// Resolve the repository's default branch ref.
///
/// Preference order:
/// - `refs/remotes/origin/HEAD`
/// - any `refs/remotes/*/HEAD`
/// - local `main`
/// - local `master`
/// - current local HEAD branch
pub fn default_branch_ref(repo_root: &Path) -> Option<String> {
    let repo = Repository::open(repo_root).ok()?;

    if let Some(target) = symbolic_ref_target(&repo, "refs/remotes/origin/HEAD") {
        return Some(target);
    }

    let refs = repo.references().ok()?;
    for reference in refs.flatten() {
        let Some(name) = reference.name() else {
            continue;
        };
        if !name.starts_with("refs/remotes/") || !name.ends_with("/HEAD") {
            continue;
        }
        if let Some(target) = reference.symbolic_target() {
            return normalize_ref_name(target);
        }
    }

    if repo.find_branch("main", git2::BranchType::Local).is_ok() {
        return Some("main".to_string());
    }
    if repo.find_branch("master", git2::BranchType::Local).is_ok() {
        return Some("master".to_string());
    }

    repo.head()
        .ok()?
        .shorthand()
        .filter(|name| *name != "HEAD")
        .map(str::to_string)
}

fn symbolic_ref_target(repo: &Repository, name: &str) -> Option<String> {
    let reference = repo.find_reference(name).ok()?;
    let target = reference.symbolic_target()?;
    normalize_ref_name(target)
}

fn normalize_ref_name(name: &str) -> Option<String> {
    name.strip_prefix("refs/remotes/")
        .or_else(|| name.strip_prefix("refs/heads/"))
        .map(str::to_string)
}

/// Returns `true` when the repo defines at least one submodule in `.gitmodules`.
pub fn repo_has_submodules(repo_root: &Path) -> bool {
    let gitmodules = repo_root.join(".gitmodules");
    let Ok(contents) = std::fs::read_to_string(gitmodules) else {
        return false;
    };
    contents.contains("[submodule ")
}

// ---------------------------------------------------------------------------
// Worktree creation
// ---------------------------------------------------------------------------

/// Create a git worktree at `worktree_path` for `branch`.
///
/// - If `use_existing_branch` is `false`, a new branch is created from the
///   given `start_point` without inheriting upstream tracking from it.
/// - If `use_existing_branch` is `true`, the worktree is linked to the
///   already-existing local branch.
///
/// The parent directory of `worktree_path` is created if it does not exist.
pub fn create_worktree(
    repo_root: &Path,
    worktree_path: &Path,
    branch: &str,
    start_point: WorktreeStartPoint<'_>,
    use_existing_branch: bool,
) -> Result<()> {
    // Ensure the parent directory exists.
    if let Some(parent) = worktree_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create worktree parent dir {:?}", parent))?;
    }

    // Build the git command arguments.
    // git worktree add [--no-track -b <branch>] <path> [<branch>]
    let mut cmd = std::process::Command::new("git");
    cmd.current_dir(repo_root);

    if use_existing_branch {
        // Checkout the existing branch into the new worktree.
        cmd.args(["worktree", "add"]);
        cmd.arg(worktree_path);
        cmd.arg(branch);
    } else {
        // Create a new branch and place it in the worktree.
        cmd.args(["worktree", "add", "--no-track", "-b"]);
        cmd.arg(branch);
        cmd.arg(worktree_path);
        if let WorktreeStartPoint::Ref(start_point) = start_point {
            cmd.arg(start_point);
        }
    }

    let output = cmd.output().context("run git worktree add")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("git worktree add failed: {}", stderr.trim());
    }

    Ok(())
}

/// Initialize submodules in the given worktree, including nested submodules.
pub fn initialize_submodules(worktree_path: &Path) -> Result<()> {
    let output = std::process::Command::new("git")
        .current_dir(worktree_path)
        .args([
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "update",
            "--init",
            "--recursive",
        ])
        .output()
        .context("run git submodule update --init --recursive")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "git submodule update --init --recursive failed: {}",
            stderr.trim()
        );
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
    use std::path::PathBuf;
    use std::process::Command;

    #[test]
    fn sanitize_basic() {
        assert_eq!(sanitize_branch_name("My Feature"), "My-Feature");
        assert_eq!(sanitize_branch_name("fix: auth bug!"), "fix-auth-bug");
        assert_eq!(sanitize_branch_name("IDY-9999 fix bug"), "IDY-9999-fix-bug");
        assert_eq!(sanitize_branch_name("  --  "), "agent");
        assert_eq!(sanitize_branch_name("hello_world"), "hello_world");
        assert_eq!(sanitize_branch_name("v1.2.3"), "v1.2.3");
        assert_eq!(sanitize_branch_name("trailing-"), "trailing");
    }

    #[test]
    fn repo_has_submodules_detects_gitmodules_entries() {
        let root = std::env::temp_dir().join(format!("flowmux-git-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();

        assert!(!repo_has_submodules(&root));

        std::fs::write(root.join(".gitmodules"), "[submodule \"private-api\"]\n").unwrap();
        assert!(repo_has_submodules(&root));

        let _ = std::fs::remove_dir_all(PathBuf::from(&root));
    }

    fn run_git(repo_root: &Path, args: &[&str]) {
        let output = Command::new("git")
            .current_dir(repo_root)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn init_test_repo(prefix: &str) -> PathBuf {
        let repo_root = std::env::temp_dir().join(format!(
            "flowmux-git-test-{prefix}-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&repo_root).unwrap();
        run_git(&repo_root, &["init"]);
        run_git(&repo_root, &["config", "user.name", "Flowmux Tests"]);
        run_git(&repo_root, &["config", "user.email", "flowmux@example.com"]);
        std::fs::write(repo_root.join("README.md"), "seed\n").unwrap();
        run_git(&repo_root, &["add", "README.md"]);
        run_git(&repo_root, &["commit", "-m", "init"]);
        repo_root
    }

    #[test]
    fn list_branch_refs_includes_local_and_remote_tracking_branches() {
        let repo_root = init_test_repo("list-refs");
        let remote_root =
            std::env::temp_dir().join(format!("flowmux-git-remote-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&remote_root).unwrap();
        run_git(&remote_root, &["init", "--bare"]);

        let remote_str = remote_root.to_string_lossy().to_string();
        run_git(&repo_root, &["remote", "add", "origin", &remote_str]);
        run_git(&repo_root, &["branch", "feature/local"]);
        run_git(&repo_root, &["push", "-u", "origin", "HEAD:teammate/work"]);
        run_git(&repo_root, &["fetch", "origin"]);

        let refs = list_branch_refs(&repo_root);

        assert!(refs.contains(&"feature/local".to_string()));
        assert!(refs.contains(&"master".to_string()) || refs.contains(&"main".to_string()));
        assert!(refs.contains(&"origin/teammate/work".to_string()));
        assert!(!refs.iter().any(|name| name.ends_with("/HEAD")));

        let _ = std::fs::remove_dir_all(repo_root);
        let _ = std::fs::remove_dir_all(remote_root);
    }

    #[test]
    fn validate_local_branch_name_rejects_invalid_names() {
        assert!(validate_local_branch_name("help/branch").is_ok());
        assert!(validate_local_branch_name("  ").is_err());
        assert!(validate_local_branch_name("bad branch").is_err());
        assert!(validate_local_branch_name("trailing.").is_err());
    }

    #[test]
    fn default_branch_ref_prefers_remote_head() {
        let repo_root = init_test_repo("default-branch-ref");
        let remote_root =
            std::env::temp_dir().join(format!("flowmux-git-remote-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&remote_root).unwrap();
        run_git(&remote_root, &["init", "--bare"]);

        let remote_str = remote_root.to_string_lossy().to_string();
        run_git(&repo_root, &["remote", "add", "origin", &remote_str]);
        run_git(&repo_root, &["push", "-u", "origin", "HEAD:main"]);
        run_git(&remote_root, &["symbolic-ref", "HEAD", "refs/heads/main"]);
        run_git(&repo_root, &["fetch", "origin"]);

        assert_eq!(
            default_branch_ref(&repo_root).as_deref(),
            Some("origin/main")
        );

        let _ = std::fs::remove_dir_all(repo_root);
        let _ = std::fs::remove_dir_all(remote_root);
    }
}

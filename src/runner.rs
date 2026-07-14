use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};

use crate::agent_discovery::DiscoveredAgents;
use crate::agents::AgentAdapter;
use crate::agents::claude::{ClaudeRuntime, install_hooks};
use crate::agents::codex::CodexAdapter;
use crate::agents::opencode::OpenCodeAdapter;
use crate::config::{AgentConfig, AgentKind};
use crate::git;
use crate::global_config::GlobalConfig;
use crate::launch;
use crate::models::AgentType;
use crate::tmux;

// ---------------------------------------------------------------------------
// AgentRunner
// ---------------------------------------------------------------------------

/// Central coordinator for agent lifecycle: discovery, restore, create, restart.
///
/// `App` holds a single `AgentRunner` and delegates all agent creation / restart
/// calls through it. Direct imports of concrete agent adapters
/// are restricted to this module.
pub struct AgentRunner {
    discovered: DiscoveredAgents,
    global_config: GlobalConfig,
    session_name: String,
    claude: Option<ClaudeRuntime>,
    /// Base directory under which git worktrees are stored.
    /// Populated from the `--git-worktrees-location` CLI arg or a default.
    pub worktrees_base: PathBuf,
    /// Optional list of enabled agent type names (e.g. ["opencode", "claude", "codex"]).
    /// When `None`, all discovered agents are available.
    enabled_agents: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default)]
pub struct WorktreeMaterialization {
    pub copy_directories: Vec<String>,
    pub symlink_directories: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorktreeStartPoint {
    Head,
    Ref(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeRequest {
    pub repo_root: String,
    pub branch_name: String,
    pub start_point: WorktreeStartPoint,
    pub initialize_submodules: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct StoredWorktree {
    repo_root: String,
    branch_name: String,
    base_ref: Option<String>,
}

impl WorktreeMaterialization {
    pub fn is_empty(&self) -> bool {
        self.copy_directories.is_empty() && self.symlink_directories.is_empty()
    }
}

impl AgentRunner {
    pub fn new(
        discovered: DiscoveredAgents,
        global_config: GlobalConfig,
        session_name: String,
        worktrees_base: PathBuf,
        enabled_agents: Option<Vec<String>>,
    ) -> Self {
        Self {
            discovered,
            global_config,
            session_name,
            claude: None,
            worktrees_base,
            enabled_agents,
        }
    }

    pub fn global_config(&self) -> &GlobalConfig {
        &self.global_config
    }

    pub fn global_config_mut(&mut self) -> &mut GlobalConfig {
        &mut self.global_config
    }

    // -----------------------------------------------------------------------
    /// Returns all agent types whose binaries were found on `$PATH` and that
    /// are enabled (if an explicit enabled list is configured).
    /// The order is stable: Opencode first, Claude second, Codex third. Future agent types
    /// should be appended here; callers must not hardcode the list.
    pub fn available_agent_types(&self) -> Vec<AgentType> {
        let mut types = Vec::new();
        if self.discovered.opencode.is_some() {
            types.push(AgentType::Opencode);
        }
        if self.discovered.claude.is_some() {
            types.push(AgentType::Claude);
        }
        if self.discovered.codex.is_some() {
            types.push(AgentType::Codex);
        }
        if let Some(ref enabled) = self.enabled_agents {
            types.retain(|t| enabled.iter().any(|e| e == t.name()));
        }
        types
    }

    // -----------------------------------------------------------------------
    // Internal: lazily start ClaudeRuntime on first Claude agent operation
    // -----------------------------------------------------------------------

    fn ensure_claude(&mut self) {
        if self.claude.is_none() {
            self.claude = Some(ClaudeRuntime::start(
                self.global_config.claude_hook_server_port,
                self.session_name.clone(),
            ));
        }
    }

    // -----------------------------------------------------------------------
    // Restore an agent from persisted config (called on startup)
    // -----------------------------------------------------------------------

    pub fn restore(&mut self, config: &AgentConfig) -> Box<dyn AgentAdapter> {
        match &config.kind {
            AgentKind::Opencode { port, session_id } => {
                Box::new(OpenCodeAdapter::new(*port, session_id.clone()))
            }
            AgentKind::Claude {
                flowmux_agent_id,
                session_id,
                transcript_path,
            } => {
                self.ensure_claude();
                let port = self.claude.as_ref().unwrap().port();
                let _ = install_hooks(port);
                let runtime = self.claude.as_ref().unwrap();
                runtime.restore(
                    flowmux_agent_id,
                    session_id.clone(),
                    transcript_path.clone(),
                    Some(&config.directory),
                );
                Box::new(runtime.make_adapter(flowmux_agent_id.clone()))
            }
            AgentKind::Codex { port, session_id } => Box::new(CodexAdapter::new(
                *port,
                config.directory.clone(),
                session_id.clone(),
            )),
        }
    }

    // -----------------------------------------------------------------------
    // Create a new agent
    // -----------------------------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    pub async fn create(
        &mut self,
        name: &str,
        dir: &str,
        project: &str,
        agent_type: AgentType,
        worktree: Option<WorktreeRequest>,
        copy_directories: Vec<String>,
        symlink_directories: Vec<String>,
    ) -> Result<(AgentConfig, Box<dyn AgentAdapter>)> {
        // --------------- Git worktree setup --------------------------------
        // If worktree creation is requested and we have a git repo root, set
        // up the worktree now and use its path as the effective working dir.
        let (effective_dir, stored_worktree) = prepare_worktree_directory(
            dir,
            worktree,
            &self.worktrees_base,
            WorktreeMaterialization {
                copy_directories,
                symlink_directories,
            },
        )?;
        // -------------------------------------------------------------------

        match agent_type {
            AgentType::Opencode => {
                let (adapter, window_index) = OpenCodeAdapter::create(&effective_dir, name).await?;
                let pane = format!("{}:{}.0", tmux::session_name(), window_index);
                let config = AgentConfig {
                    name: name.to_owned(),
                    pane,
                    directory: effective_dir,
                    project: project.to_owned(),
                    kind: AgentKind::Opencode {
                        port: adapter.port,
                        session_id: None,
                    },
                    git_repo_root: stored_worktree.as_ref().map(|wt| wt.repo_root.clone()),
                    git_worktree_branch: stored_worktree.as_ref().map(|wt| wt.branch_name.clone()),
                    git_worktree_base_ref: stored_worktree
                        .as_ref()
                        .and_then(|wt| wt.base_ref.clone()),
                };
                Ok((config, Box::new(adapter)))
            }

            AgentType::Claude => {
                self.ensure_claude();
                let port = self.claude.as_ref().unwrap().port();
                install_hooks(port)?;

                let flowmux_agent_id = uuid::Uuid::new_v4().to_string();
                let window_index = tmux::new_window(&effective_dir, name)?;
                let pane = format!("{}:{}.0", tmux::session_name(), window_index);

                // Launch claude with the flowmux agent ID exported as an env var.
                let args = vec![
                    std::ffi::OsString::from("--flowmux-agent-id"),
                    flowmux_agent_id.clone().into(),
                ];
                tmux::send_literal(&pane, &launch::flowmux_launch_command("claude", &args))?;

                let runtime = self.claude.as_ref().unwrap();
                let adapter = runtime.make_adapter(flowmux_agent_id.clone());

                let config = AgentConfig {
                    name: name.to_owned(),
                    pane,
                    directory: effective_dir,
                    project: project.to_owned(),
                    kind: AgentKind::Claude {
                        flowmux_agent_id,
                        session_id: None,
                        transcript_path: None,
                    },
                    git_repo_root: stored_worktree.as_ref().map(|wt| wt.repo_root.clone()),
                    git_worktree_branch: stored_worktree.as_ref().map(|wt| wt.branch_name.clone()),
                    git_worktree_base_ref: stored_worktree
                        .as_ref()
                        .and_then(|wt| wt.base_ref.clone()),
                };
                Ok((config, Box::new(adapter)))
            }

            AgentType::Codex => {
                let (adapter, window_index) = CodexAdapter::create(&effective_dir, name).await?;
                let pane = format!("{}:{}.0", tmux::session_name(), window_index);
                let config = AgentConfig {
                    name: name.to_owned(),
                    pane,
                    directory: effective_dir,
                    project: project.to_owned(),
                    kind: AgentKind::Codex {
                        port: adapter.port,
                        session_id: None,
                    },
                    git_repo_root: stored_worktree.as_ref().map(|wt| wt.repo_root.clone()),
                    git_worktree_branch: stored_worktree.as_ref().map(|wt| wt.branch_name.clone()),
                    git_worktree_base_ref: stored_worktree
                        .as_ref()
                        .and_then(|wt| wt.base_ref.clone()),
                };
                Ok((config, Box::new(adapter)))
            }
        }
    }

    // -----------------------------------------------------------------------
    // Restart a stopped agent
    // -----------------------------------------------------------------------

    pub async fn restart(
        &mut self,
        config: &AgentConfig,
    ) -> Result<(AgentConfig, Box<dyn AgentAdapter>)> {
        match &config.kind {
            AgentKind::Opencode { .. } => {
                let session_id = config.session_id().map(str::to_owned);
                let (new_adapter, window_index, new_port) = OpenCodeAdapter::restart(
                    &config.directory,
                    &config.name,
                    session_id.as_deref(),
                )
                .await?;
                let new_pane = format!("{}:{}.0", tmux::session_name(), window_index);
                let mut new_config = config.clone();
                new_config.pane = new_pane;
                if let AgentKind::Opencode { ref mut port, .. } = new_config.kind {
                    *port = new_port;
                }
                Ok((new_config, Box::new(new_adapter)))
            }

            AgentKind::Claude {
                flowmux_agent_id,
                session_id,
                transcript_path: _,
            } => {
                self.ensure_claude();
                let port = self.claude.as_ref().unwrap().port();
                install_hooks(port)?;

                // Open a fresh tmux window — same name and directory as before.
                let window_index = tmux::new_window(&config.directory, &config.name)?;
                let new_pane = format!("{}:{}.0", tmux::session_name(), window_index);

                // Reuse the *same* flowmux_agent_id so the hook_state entry
                // (first_prompt, context, session history) is preserved across
                // the restart. The hook server will accept events from the new
                // process because the entry already exists in the map.
                let runtime = self.claude.as_ref().unwrap();
                runtime.reset_status(flowmux_agent_id);

                // Launch claude, exporting the flowmux agent ID.
                // If we have a prior Claude session ID, resume it so the
                // conversation context is preserved across restarts.
                let mut args = vec![
                    std::ffi::OsString::from("--flowmux-agent-id"),
                    flowmux_agent_id.clone().into(),
                ];
                if let Some(sid) = session_id {
                    args.push(std::ffi::OsString::from("--session-id"));
                    args.push(sid.clone().into());
                }
                tmux::send_literal(&new_pane, &launch::flowmux_launch_command("claude", &args))?;

                let adapter = runtime.make_adapter(flowmux_agent_id.clone());
                let mut new_config = config.clone();
                new_config.pane = new_pane;
                Ok((new_config, Box::new(adapter)))
            }

            AgentKind::Codex { .. } => {
                let session_id = config.session_id().map(str::to_owned);
                let (adapter, window_index, port) =
                    CodexAdapter::restart(&config.directory, &config.name, session_id.as_deref())
                        .await?;
                let mut new_config = config.clone();
                new_config.pane = format!("{}:{}.0", tmux::session_name(), window_index);
                if let AgentKind::Codex {
                    port: stored_port, ..
                } = &mut new_config.kind
                {
                    *stored_port = port;
                }
                Ok((new_config, Box::new(adapter)))
            }
        }
    }
}

fn prepare_worktree_directory(
    dir: &str,
    worktree: Option<WorktreeRequest>,
    worktrees_base: &Path,
    materialization: WorktreeMaterialization,
) -> Result<(String, Option<StoredWorktree>)> {
    let Some(worktree) = worktree else {
        return Ok((dir.to_owned(), None));
    };

    git::validate_local_branch_name(&worktree.branch_name)?;

    let repo_root_path = Path::new(&worktree.repo_root);
    let wt_path = worktrees_base
        .join(git::repo_id(repo_root_path))
        .join(&worktree.branch_name);

    let (use_existing, stored_base_ref) = match &worktree.start_point {
        WorktreeStartPoint::Head => {
            let default_ref = git::default_branch_ref(repo_root_path).ok_or_else(|| {
                anyhow::anyhow!(
                    "could not determine repository default branch; configure a remote HEAD or use 'Start from branch'"
                )
            })?;
            (
                git::branch_exists(repo_root_path, &worktree.branch_name),
                Some(default_ref),
            )
        }
        WorktreeStartPoint::Ref(start_point) => {
            if git::branch_exists(repo_root_path, &worktree.branch_name) {
                bail!(
                    "branch {} already exists locally; choose a different new branch name",
                    worktree.branch_name
                );
            }
            (false, Some(start_point.clone()))
        }
    };

    let start_point = stored_base_ref
        .as_deref()
        .map(git::WorktreeStartPoint::Ref)
        .unwrap_or(git::WorktreeStartPoint::Head);

    git::create_worktree(
        repo_root_path,
        &wt_path,
        &worktree.branch_name,
        start_point,
        use_existing,
    )?;

    if let Err(error) =
        materialize_worktree_directories(repo_root_path, Path::new(dir), &wt_path, &materialization)
    {
        return match git::remove_worktree(
            repo_root_path,
            &wt_path,
            &worktree.branch_name,
            !use_existing,
        ) {
            Ok(()) => Err(error),
            Err(cleanup_error) => Err(error.context(format!(
                "failed to roll back git worktree {:?}: {}",
                wt_path, cleanup_error
            ))),
        };
    }

    if worktree.initialize_submodules
        && let Err(error) = git::initialize_submodules(&wt_path)
    {
        return match git::remove_worktree(
            repo_root_path,
            &wt_path,
            &worktree.branch_name,
            !use_existing,
        ) {
            Ok(()) => Err(error),
            Err(cleanup_error) => Err(error.context(format!(
                "failed to roll back git worktree {:?}: {}",
                wt_path, cleanup_error
            ))),
        };
    }

    Ok((
        wt_path.to_string_lossy().to_string(),
        Some(StoredWorktree {
            repo_root: worktree.repo_root,
            branch_name: worktree.branch_name,
            base_ref: stored_base_ref,
        }),
    ))
}

fn materialize_worktree_directories(
    repo_root: &Path,
    selected_dir: &Path,
    worktree_path: &Path,
    materialization: &WorktreeMaterialization,
) -> Result<()> {
    if materialization.is_empty() {
        return Ok(());
    }

    let relative_base = selected_dir.strip_prefix(repo_root).with_context(|| {
        format!(
            "selected directory {:?} is not inside git repo {:?}",
            selected_dir, repo_root
        )
    })?;
    let destination_base = worktree_path.join(relative_base);

    for relative in &materialization.copy_directories {
        let source = selected_dir.join(relative);
        let destination = destination_base.join(relative);
        ensure_materialization_ready(&source, &destination, "copy")?;
    }
    for relative in &materialization.symlink_directories {
        let source = selected_dir.join(relative);
        let destination = destination_base.join(relative);
        ensure_materialization_ready(&source, &destination, "symlink")?;
    }

    for relative in &materialization.copy_directories {
        let source = selected_dir.join(relative);
        let destination = destination_base.join(relative);
        copy_directory_recursive(&source, &destination)?;
    }
    for relative in &materialization.symlink_directories {
        let source = selected_dir.join(relative);
        let destination = destination_base.join(relative);
        create_directory_symlink(&source, &destination)?;
    }

    Ok(())
}

fn ensure_materialization_ready(source: &Path, destination: &Path, action: &str) -> Result<()> {
    if !source.is_dir() {
        bail!("{action} source directory {:?} does not exist", source);
    }
    if destination.exists() {
        bail!(
            "{action} destination {:?} already exists in the new worktree",
            destination
        );
    }
    Ok(())
}

fn copy_directory_recursive(source: &Path, destination: &Path) -> Result<()> {
    std::fs::create_dir_all(destination)
        .with_context(|| format!("create destination directory {:?}", destination))?;

    for entry in
        std::fs::read_dir(source).with_context(|| format!("read directory {:?}", source))?
    {
        let entry = entry.with_context(|| format!("read entry in {:?}", source))?;
        let file_type = entry
            .file_type()
            .with_context(|| format!("read file type for {:?}", entry.path()))?;
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());

        if file_type.is_dir() {
            copy_directory_recursive(&source_path, &destination_path)?;
        } else if file_type.is_file() {
            std::fs::copy(&source_path, &destination_path).with_context(|| {
                format!("copy file {:?} -> {:?}", source_path, destination_path)
            })?;
        } else if file_type.is_symlink() {
            let target = std::fs::read_link(&source_path)
                .with_context(|| format!("read symlink {:?}", source_path))?;
            create_symlink(&target, &destination_path)?;
        }
    }

    Ok(())
}

fn create_directory_symlink(source: &Path, destination: &Path) -> Result<()> {
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create symlink parent directory {:?}", parent))?;
    }
    create_symlink(source, destination)
}

fn create_symlink(source: &Path, destination: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(source, destination)
            .with_context(|| format!("create symlink {:?} -> {:?}", destination, source))?;
        Ok(())
    }
    #[cfg(windows)]
    {
        std::os::windows::fs::symlink_dir(source, destination)
            .with_context(|| format!("create symlink {:?} -> {:?}", destination, source))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    struct TestDir {
        path: PathBuf,
    }

    impl TestDir {
        fn new(prefix: &str) -> Self {
            let path =
                std::env::temp_dir().join(format!("flowmux-{prefix}-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&path).unwrap();
            Self { path }
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
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

    fn git_stdout(repo_root: &Path, args: &[&str]) -> String {
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
        String::from_utf8(output.stdout).unwrap()
    }

    fn git_config_value(repo_root: &Path, key: &str) -> Option<String> {
        let output = Command::new("git")
            .current_dir(repo_root)
            .args(["config", "--get", key])
            .output()
            .unwrap();
        if !output.status.success() {
            return None;
        }
        Some(String::from_utf8(output.stdout).unwrap().trim().to_string())
    }

    fn init_test_repo(temp: &TestDir) -> PathBuf {
        let repo_root = temp.path.join("repo");
        std::fs::create_dir_all(&repo_root).unwrap();
        run_git(&repo_root, &["init"]);
        run_git(&repo_root, &["config", "user.name", "Flowmux Tests"]);
        run_git(&repo_root, &["config", "user.email", "flowmux@example.com"]);
        std::fs::write(repo_root.join("README.md"), "seed\n").unwrap();
        run_git(&repo_root, &["add", "README.md"]);
        run_git(&repo_root, &["commit", "-m", "init"]);
        repo_root
    }

    fn git_worktree_list_contains(repo_root: &Path, path: &Path) -> bool {
        let path_str = path.to_string_lossy().to_string();
        git_stdout(repo_root, &["worktree", "list"])
            .lines()
            .any(|line| line.starts_with(&path_str))
    }

    fn worktree_request(repo_root: &Path, branch_name: &str) -> WorktreeRequest {
        WorktreeRequest {
            repo_root: repo_root.to_string_lossy().to_string(),
            branch_name: branch_name.to_string(),
            start_point: WorktreeStartPoint::Head,
            initialize_submodules: false,
        }
    }

    #[test]
    fn materialize_worktree_directories_maps_into_selected_subdir() {
        let temp = TestDir::new("materialize");
        let repo_root = temp.path.join("repo");
        let selected_dir = repo_root.join("subproj");
        let source_cache = selected_dir.join("cache/nested");
        let worktree = temp.path.join("worktree");

        std::fs::create_dir_all(&source_cache).unwrap();
        std::fs::create_dir_all(&worktree).unwrap();
        std::fs::write(source_cache.join("artifact.txt"), "hello").unwrap();

        let materialization = WorktreeMaterialization {
            copy_directories: vec!["cache".into()],
            symlink_directories: vec![],
        };

        materialize_worktree_directories(&repo_root, &selected_dir, &worktree, &materialization)
            .unwrap();

        let copied_file = worktree.join("subproj/cache/nested/artifact.txt");
        assert_eq!(std::fs::read_to_string(copied_file).unwrap(), "hello");
    }

    #[test]
    fn materialize_worktree_directories_creates_symlink() {
        let temp = TestDir::new("symlink");
        let repo_root = temp.path.join("repo");
        let selected_dir = repo_root.join("subproj");
        let source_dir = selected_dir.join("deps");
        let worktree = temp.path.join("worktree");

        std::fs::create_dir_all(&source_dir).unwrap();
        std::fs::create_dir_all(&worktree).unwrap();

        let materialization = WorktreeMaterialization {
            copy_directories: vec![],
            symlink_directories: vec!["deps".into()],
        };

        materialize_worktree_directories(&repo_root, &selected_dir, &worktree, &materialization)
            .unwrap();

        let link_path = worktree.join("subproj/deps");
        let target = std::fs::read_link(&link_path).unwrap();
        assert_eq!(target, source_dir);
    }

    #[test]
    fn materialize_worktree_directories_rejects_existing_destination() {
        let temp = TestDir::new("dest-conflict");
        let repo_root = temp.path.join("repo");
        let selected_dir = repo_root.join("subproj");
        let source_dir = selected_dir.join("cache");
        let worktree = temp.path.join("worktree");
        let destination = worktree.join("subproj/cache");

        std::fs::create_dir_all(&source_dir).unwrap();
        std::fs::create_dir_all(&destination).unwrap();

        let materialization = WorktreeMaterialization {
            copy_directories: vec!["cache".into()],
            symlink_directories: vec![],
        };

        let err = materialize_worktree_directories(
            &repo_root,
            &selected_dir,
            &worktree,
            &materialization,
        )
        .unwrap_err();
        assert!(err.to_string().contains("already exists"));
    }

    #[test]
    fn materialize_worktree_directories_rejects_missing_source() {
        let temp = TestDir::new("missing-source");
        let repo_root = temp.path.join("repo");
        let selected_dir = repo_root.join("subproj");
        let worktree = temp.path.join("worktree");

        std::fs::create_dir_all(&selected_dir).unwrap();
        std::fs::create_dir_all(&worktree).unwrap();

        let materialization = WorktreeMaterialization {
            copy_directories: vec!["cache".into()],
            symlink_directories: vec![],
        };

        let err = materialize_worktree_directories(
            &repo_root,
            &selected_dir,
            &worktree,
            &materialization,
        )
        .unwrap_err();
        assert!(err.to_string().contains("does not exist"));
    }

    #[test]
    fn prepare_worktree_directory_rolls_back_new_branch_on_materialization_failure() {
        let temp = TestDir::new("rollback-new-branch");
        let repo_root = init_test_repo(&temp);
        let selected_dir = repo_root.join("subproj");
        let worktrees_base = temp.path.join("worktrees");

        std::fs::create_dir_all(&selected_dir).unwrap();

        let branch = git::sanitize_branch_name("agent rollback");
        let err = prepare_worktree_directory(
            selected_dir.to_str().unwrap(),
            Some(worktree_request(&repo_root, &branch)),
            &worktrees_base,
            WorktreeMaterialization {
                copy_directories: vec!["missing".into()],
                symlink_directories: vec![],
            },
        )
        .unwrap_err();

        let worktree_path = worktrees_base.join(git::repo_id(&repo_root)).join(&branch);

        assert!(err.to_string().contains("does not exist"));
        assert!(!worktree_path.exists());
        assert!(!git_worktree_list_contains(&repo_root, &worktree_path));
        assert!(!git::branch_exists(&repo_root, &branch));
    }

    #[test]
    fn prepare_worktree_directory_preserves_existing_branch_on_materialization_failure() {
        let temp = TestDir::new("rollback-existing-branch");
        let repo_root = init_test_repo(&temp);
        let selected_dir = repo_root.join("subproj");
        let worktrees_base = temp.path.join("worktrees");
        let branch = git::sanitize_branch_name("existing branch");

        std::fs::create_dir_all(&selected_dir).unwrap();
        run_git(&repo_root, &["branch", &branch]);

        let err = prepare_worktree_directory(
            selected_dir.to_str().unwrap(),
            Some(worktree_request(&repo_root, &branch)),
            &worktrees_base,
            WorktreeMaterialization {
                copy_directories: vec!["missing".into()],
                symlink_directories: vec![],
            },
        )
        .unwrap_err();

        let worktree_path = worktrees_base.join(git::repo_id(&repo_root)).join(&branch);

        assert!(err.to_string().contains("does not exist"));
        assert!(!worktree_path.exists());
        assert!(!git_worktree_list_contains(&repo_root, &worktree_path));
        assert!(git::branch_exists(&repo_root, &branch));
    }

    #[test]
    fn prepare_worktree_directory_initializes_submodules_when_requested() {
        let temp = TestDir::new("init-submodules");
        let repo_root = init_test_repo(&temp);
        let submodule_remote = temp.path.join("submodule-remote");
        let worktrees_base = temp.path.join("worktrees");

        std::fs::create_dir_all(&submodule_remote).unwrap();
        run_git(&submodule_remote, &["init"]);
        run_git(&submodule_remote, &["config", "user.name", "Flowmux Tests"]);
        run_git(
            &submodule_remote,
            &["config", "user.email", "flowmux@example.com"],
        );
        std::fs::write(submodule_remote.join("submodule.txt"), "submodule\n").unwrap();
        run_git(&submodule_remote, &["add", "submodule.txt"]);
        run_git(&submodule_remote, &["commit", "-m", "init"]);

        let output = Command::new("git")
            .current_dir(&repo_root)
            .args(["-c", "protocol.file.allow=always", "submodule", "add"])
            .arg(&submodule_remote)
            .arg("private-api")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git submodule add failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        run_git(&repo_root, &["commit", "-am", "add submodule"]);

        let branch = git::sanitize_branch_name("agent with submodules");
        let (worktree_dir, _) = prepare_worktree_directory(
            repo_root.to_str().unwrap(),
            Some(WorktreeRequest {
                initialize_submodules: true,
                ..worktree_request(&repo_root, &branch)
            }),
            &worktrees_base,
            WorktreeMaterialization::default(),
        )
        .unwrap();

        let submodule_file = Path::new(&worktree_dir).join("private-api/submodule.txt");
        assert_eq!(
            std::fs::read_to_string(submodule_file).unwrap(),
            "submodule\n"
        );
    }

    #[test]
    fn prepare_worktree_directory_creates_branch_from_selected_base_ref() {
        let temp = TestDir::new("base-ref");
        let repo_root = init_test_repo(&temp);
        let remote_root = temp.path.join("remote.git");
        let worktrees_base = temp.path.join("worktrees");

        std::fs::create_dir_all(&remote_root).unwrap();
        run_git(&remote_root, &["init", "--bare"]);
        let remote_str = remote_root.to_string_lossy().to_string();
        run_git(&repo_root, &["remote", "add", "origin", &remote_str]);

        std::fs::write(repo_root.join("feature.txt"), "from teammate\n").unwrap();
        run_git(&repo_root, &["add", "feature.txt"]);
        run_git(&repo_root, &["commit", "-m", "feature work"]);
        run_git(&repo_root, &["push", "origin", "HEAD:teammate/work"]);
        run_git(&repo_root, &["fetch", "origin"]);

        let branch_name = "helper/branch";
        let (worktree_dir, stored) = prepare_worktree_directory(
            repo_root.to_str().unwrap(),
            Some(WorktreeRequest {
                repo_root: repo_root.to_string_lossy().to_string(),
                branch_name: branch_name.to_string(),
                start_point: WorktreeStartPoint::Ref("origin/teammate/work".into()),
                initialize_submodules: false,
            }),
            &worktrees_base,
            WorktreeMaterialization::default(),
        )
        .unwrap();

        assert_eq!(
            std::fs::read_to_string(Path::new(&worktree_dir).join("feature.txt")).unwrap(),
            "from teammate\n"
        );
        assert_eq!(
            stored.unwrap().base_ref.as_deref(),
            Some("origin/teammate/work")
        );
        assert_eq!(
            git_config_value(&repo_root, "branch.helper/branch.remote"),
            None
        );
        assert_eq!(
            git_config_value(&repo_root, "branch.helper/branch.merge"),
            None
        );
    }

    #[test]
    fn prepare_worktree_directory_head_mode_uses_default_branch_not_current_feature_branch() {
        let temp = TestDir::new("default-branch");
        let repo_root = init_test_repo(&temp);
        let remote_root = temp.path.join("remote.git");
        let worktrees_base = temp.path.join("worktrees");

        std::fs::create_dir_all(&remote_root).unwrap();
        run_git(&remote_root, &["init", "--bare"]);

        let remote_str = remote_root.to_string_lossy().to_string();
        run_git(&repo_root, &["remote", "add", "origin", &remote_str]);
        run_git(&repo_root, &["push", "-u", "origin", "HEAD:main"]);
        run_git(&remote_root, &["symbolic-ref", "HEAD", "refs/heads/main"]);
        run_git(&repo_root, &["fetch", "origin"]);

        std::fs::write(repo_root.join("feature-only.txt"), "feature\n").unwrap();
        run_git(&repo_root, &["add", "feature-only.txt"]);
        run_git(&repo_root, &["commit", "-m", "feature"]);

        let branch_name = "helper/from-default";
        let (worktree_dir, stored) = prepare_worktree_directory(
            repo_root.to_str().unwrap(),
            Some(WorktreeRequest {
                repo_root: repo_root.to_string_lossy().to_string(),
                branch_name: branch_name.to_string(),
                start_point: WorktreeStartPoint::Head,
                initialize_submodules: false,
            }),
            &worktrees_base,
            WorktreeMaterialization::default(),
        )
        .unwrap();

        assert!(!Path::new(&worktree_dir).join("feature-only.txt").exists());
        assert_eq!(stored.unwrap().base_ref.as_deref(), Some("origin/main"));
        assert_eq!(
            git_config_value(&repo_root, "branch.helper/from-default.remote"),
            None
        );
        assert_eq!(
            git_config_value(&repo_root, "branch.helper/from-default.merge"),
            None
        );
    }
}

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

    pub async fn create(
        &mut self,
        name: &str,
        dir: &str,
        project: &str,
        agent_type: AgentType,
        create_worktree: bool,
        git_repo_root: Option<&str>,
        copy_directories: Vec<String>,
        symlink_directories: Vec<String>,
    ) -> Result<(AgentConfig, Box<dyn AgentAdapter>)> {
        // --------------- Git worktree setup --------------------------------
        // If worktree creation is requested and we have a git repo root, set
        // up the worktree now and use its path as the effective working dir.
        let (effective_dir, stored_repo_root) = if create_worktree {
            if let Some(repo_root) = git_repo_root {
                let branch = git::sanitize_branch_name(name);
                let repo_root_path = std::path::Path::new(repo_root);
                let wt_path = self
                    .worktrees_base
                    .join(git::repo_id(repo_root_path))
                    .join(&branch);

                let use_existing = git::branch_exists(repo_root_path, &branch);
                git::create_worktree(repo_root_path, &wt_path, &branch, use_existing)?;
                let materialization = WorktreeMaterialization {
                    copy_directories,
                    symlink_directories,
                };
                materialize_worktree_directories(
                    repo_root_path,
                    Path::new(dir),
                    &wt_path,
                    &materialization,
                )?;

                let wt_str = wt_path.to_string_lossy().to_string();
                (wt_str, Some(repo_root.to_owned()))
            } else {
                (dir.to_owned(), None)
            }
        } else {
            (dir.to_owned(), None)
        };
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
                    git_repo_root: stored_repo_root,
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
                tmux::send_keys(
                    &pane,
                    &format!("FLOWMUX_AGENT_ID={} claude\n", flowmux_agent_id),
                )?;

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
                    git_repo_root: stored_repo_root,
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
                    git_repo_root: stored_repo_root,
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
                let claude_cmd = match session_id {
                    Some(sid) => format!(
                        "FLOWMUX_AGENT_ID={} claude --resume {}\n",
                        flowmux_agent_id, sid
                    ),
                    None => format!("FLOWMUX_AGENT_ID={} claude\n", flowmux_agent_id),
                };
                tmux::send_keys(&new_pane, &claude_cmd)?;

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
}

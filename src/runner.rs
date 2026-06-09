use anyhow::Result;
use std::path::PathBuf;

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
        agent_type: AgentType,
        create_worktree: bool,
        git_repo_root: Option<&str>,
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

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Application-wide (not per-session) configuration stored at
/// `~/.config/flowmux/config.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GlobalConfig {
    /// Base port for the Claude Code hook server.  The first instance binds
    /// to this port; subsequent instances automatically find the next free
    /// port.  Default: 15100.
    #[serde(default = "default_hook_port")]
    pub claude_hook_server_port: u16,

    /// Command string for the external git viewer (e.g. "lazygit" or "lazydiff diff").
    /// When set, Ctrl+V in the agent view launches the viewer in a new tmux pane.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_viewer: Option<String>,

    /// List of agent type names to enable (e.g. ["opencode", "claude", "codex"]).
    /// When `None`, all discovered agents are available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled_agents: Option<Vec<String>>,
}

fn default_hook_port() -> u16 {
    15100
}

impl Default for GlobalConfig {
    fn default() -> Self {
        Self {
            claude_hook_server_port: default_hook_port(),
            git_viewer: None,
            enabled_agents: None,
        }
    }
}

fn config_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("flowmux").join("config.toml"))
}

impl GlobalConfig {
    /// Load from `~/.config/flowmux/config.toml`.  Returns the default
    /// configuration if the file does not exist.
    pub fn load() -> Result<Self> {
        let path = match config_path() {
            Some(p) => p,
            None => return Ok(Self::default()),
        };
        if !path.exists() {
            return Ok(Self::default());
        }
        let contents = std::fs::read_to_string(&path)?;
        let config: GlobalConfig = toml::from_str(&contents)?;
        Ok(config)
    }

    /// Split the `git_viewer` string into (program, args).
    /// Returns `None` if `git_viewer` is not configured.
    pub fn git_viewer_parts(&self) -> Option<(String, Vec<String>)> {
        let raw = self.git_viewer.as_deref()?.trim();
        if raw.is_empty() {
            return None;
        }
        let mut parts = raw.split_whitespace().map(String::from);
        let program = parts.next()?;
        let args: Vec<String> = parts.collect();
        Some((program, args))
    }
}

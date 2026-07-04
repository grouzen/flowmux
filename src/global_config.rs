use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

const DEFAULT_GIT_VIEWER: &str = "git diff";

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
    /// When unset, Flowmux defaults to `git diff`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_viewer: Option<String>,

    /// List of agent type names to enable (e.g. ["opencode", "claude", "codex"]).
    /// When `None`, all discovered agents are available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled_agents: Option<Vec<String>>,

    /// Whether the first-run startup guide has been dismissed.
    #[serde(default, skip_serializing_if = "is_false")]
    pub startup_guide_dismissed: bool,

    /// Selected UI theme id for Flowmux chrome.
    /// When unset or invalid, Flowmux falls back to Gruvbox Dark.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theme: Option<String>,

    /// Per-repository remembered copy/symlink directory selections used when
    /// creating git worktrees from the launch-agent dialog.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub worktree_directory_presets: BTreeMap<String, WorktreeDirectoryPreset>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorktreeDirectoryPreset {
    #[serde(default)]
    pub copy_directories: Vec<String>,
    #[serde(default)]
    pub symlink_directories: Vec<String>,
}

fn default_hook_port() -> u16 {
    15100
}

fn is_false(value: &bool) -> bool {
    !*value
}

impl Default for GlobalConfig {
    fn default() -> Self {
        Self {
            claude_hook_server_port: default_hook_port(),
            git_viewer: None,
            enabled_agents: None,
            startup_guide_dismissed: false,
            theme: None,
            worktree_directory_presets: BTreeMap::new(),
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

    pub fn save(&self) -> Result<()> {
        let Some(path) = config_path() else {
            return Ok(());
        };

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let content = toml::to_string_pretty(self)?;
        let tmp_path = path.with_extension("toml.tmp");
        std::fs::write(&tmp_path, content)?;
        std::fs::rename(&tmp_path, &path)?;
        Ok(())
    }

    /// Split the configured git viewer command into (program, args).
    /// Falls back to `git diff` when the config value is unset or blank.
    pub fn git_viewer_parts(&self) -> Option<(String, Vec<String>)> {
        let raw = self
            .git_viewer
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(DEFAULT_GIT_VIEWER);
        if raw.is_empty() {
            return None;
        }
        let mut parts = raw.split_whitespace().map(String::from);
        let program = parts.next()?;
        let args: Vec<String> = parts.collect();
        Some((program, args))
    }
}

#[cfg(test)]
mod tests {
    use super::GlobalConfig;

    #[test]
    fn git_viewer_parts_defaults_to_git_diff_when_unset() {
        let config = GlobalConfig::default();

        assert_eq!(
            config.git_viewer_parts(),
            Some(("git".to_string(), vec!["diff".to_string()]))
        );
    }

    #[test]
    fn git_viewer_parts_defaults_to_git_diff_when_blank() {
        let config = GlobalConfig {
            git_viewer: Some("   ".to_string()),
            ..GlobalConfig::default()
        };

        assert_eq!(
            config.git_viewer_parts(),
            Some(("git".to_string(), vec!["diff".to_string()]))
        );
    }

    #[test]
    fn git_viewer_parts_uses_explicit_configured_command() {
        let config = GlobalConfig {
            git_viewer: Some("lazygit --path".to_string()),
            ..GlobalConfig::default()
        };

        assert_eq!(
            config.git_viewer_parts(),
            Some(("lazygit".to_string(), vec!["--path".to_string()]))
        );
    }

    #[test]
    fn startup_guide_dismissed_defaults_to_false_when_missing() {
        let config: GlobalConfig = toml::from_str("git_viewer = \"lazygit\"").unwrap();

        assert!(!config.startup_guide_dismissed);
    }

    #[test]
    fn startup_guide_dismissed_round_trips_through_toml() {
        let config = GlobalConfig {
            startup_guide_dismissed: true,
            ..GlobalConfig::default()
        };

        let serialized = toml::to_string(&config).unwrap();
        let parsed: GlobalConfig = toml::from_str(&serialized).unwrap();

        assert!(parsed.startup_guide_dismissed);
    }

    #[test]
    fn default_theme_is_unset() {
        assert_eq!(GlobalConfig::default().theme, None);
    }
}

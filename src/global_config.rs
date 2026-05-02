use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Application-wide (not per-session) configuration stored at
/// `~/.config/stable/config.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GlobalConfig {
    /// Port the Claude Code hook server listens on. Default: 15100.
    #[serde(default = "default_hook_port")]
    pub claude_hook_server_port: u16,
}

fn default_hook_port() -> u16 {
    15100
}

impl Default for GlobalConfig {
    fn default() -> Self {
        Self {
            claude_hook_server_port: default_hook_port(),
        }
    }
}

fn config_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("stable").join("config.toml"))
}

impl GlobalConfig {
    /// Load from `~/.config/stable/config.toml`.  Returns the default
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

    /// Atomically write the config to disk (write to `.tmp`, then rename).
    pub fn save(&self) -> Result<()> {
        let path = config_path().ok_or_else(|| anyhow::anyhow!("cannot determine config dir"))?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("toml.tmp");
        let contents = toml::to_string_pretty(self)?;
        std::fs::write(&tmp, contents)?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }
}

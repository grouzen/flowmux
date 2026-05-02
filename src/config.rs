use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

// ---------------------------------------------------------------------------
// AgentKind
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "agent_type", rename_all = "lowercase")]
pub enum AgentKind {
    Opencode {
        port: u16,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_id: Option<String>,
    },
    Claude {
        stable_agent_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        transcript_path: Option<String>,
    },
}

// ---------------------------------------------------------------------------
// AgentConfig
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    pub name: String,
    pub pane: String,
    pub directory: String,
    #[serde(flatten)]
    pub kind: AgentKind,
}

impl AgentConfig {
    /// Return a display string for the agent type (e.g. "opencode", "claude").
    pub fn agent_type_str(&self) -> &'static str {
        match &self.kind {
            AgentKind::Opencode { .. } => "opencode",
            AgentKind::Claude { .. } => "claude",
        }
    }

    /// Convenience: return the session_id regardless of agent kind.
    pub fn session_id(&self) -> Option<&str> {
        match &self.kind {
            AgentKind::Opencode { session_id, .. } => session_id.as_deref(),
            AgentKind::Claude { session_id, .. } => session_id.as_deref(),
        }
    }

    /// Convenience: set the session_id on whichever kind is active.
    pub fn set_session_id(&mut self, id: Option<String>) {
        match &mut self.kind {
            AgentKind::Opencode { session_id, .. } => *session_id = id,
            AgentKind::Claude { session_id, .. } => *session_id = id,
        }
    }
}

// ---------------------------------------------------------------------------
// Config (runtime) / ConfigFile (on-disk)
// ---------------------------------------------------------------------------

/// Serialisable portion of the config (agents list only).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct ConfigFile {
    #[serde(default)]
    agents: Vec<AgentConfig>,
}

/// Runtime config: the agents list plus the session name that determines
/// which file on disk this config is bound to.  The `session_name` field is
/// never written to disk.
#[derive(Debug, Clone, Default)]
pub struct Config {
    pub agents: Vec<AgentConfig>,
    /// The tmux session name this config belongs to.  Set by `load()`.
    pub session_name: String,
}

/// Path to the per-session config file:
///   `~/.config/stable/sessions/<session>.toml`
pub fn config_path(session: &str) -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("stable")
        .join("sessions")
        .join(format!("{}.toml", session))
}

impl Config {
    pub fn load(session: &str) -> anyhow::Result<Config> {
        let path = config_path(session);

        if !path.exists() {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("create config dir {:?}", parent))?;
            }
            return Ok(Config {
                agents: Vec::new(),
                session_name: session.to_string(),
            });
        }

        let content =
            std::fs::read_to_string(&path).with_context(|| format!("read config {:?}", path))?;

        let file: ConfigFile =
            toml::from_str(&content).with_context(|| format!("parse config {:?}", path))?;

        Ok(Config {
            agents: file.agents,
            session_name: session.to_string(),
        })
    }

    pub fn save(&self) -> anyhow::Result<()> {
        let path = config_path(&self.session_name);

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create config dir {:?}", parent))?;
        }

        let file = ConfigFile {
            agents: self.agents.clone(),
        };
        let content = toml::to_string_pretty(&file).context("serialize config to TOML")?;

        // Atomic write: write to a temp file then rename
        let tmp_path = path.with_extension("toml.tmp");
        std::fs::write(&tmp_path, content)
            .with_context(|| format!("write temp config {:?}", tmp_path))?;
        std::fs::rename(&tmp_path, &path)
            .with_context(|| format!("rename config {:?} -> {:?}", tmp_path, path))?;

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Round-trip test
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_kind_round_trip() {
        let agents = vec![
            AgentConfig {
                name: "oc".into(),
                pane: "stable:1.0".into(),
                directory: "/tmp".into(),
                kind: AgentKind::Opencode {
                    port: 9000,
                    session_id: Some("s1".into()),
                },
            },
            AgentConfig {
                name: "cl".into(),
                pane: "stable:2.0".into(),
                directory: "/tmp".into(),
                kind: AgentKind::Claude {
                    stable_agent_id: "abc-123".into(),
                    session_id: None,
                    transcript_path: Some("/tmp/t.jsonl".into()),
                },
            },
        ];

        #[derive(Serialize, Deserialize)]
        struct File {
            agents: Vec<AgentConfig>,
        }

        let toml_str = toml::to_string_pretty(&File {
            agents: agents.clone(),
        })
        .unwrap();
        let back: File = toml::from_str(&toml_str).unwrap();

        assert_eq!(back.agents[0].name, "oc");
        assert!(matches!(
            back.agents[0].kind,
            AgentKind::Opencode { port: 9000, .. }
        ));
        assert_eq!(back.agents[0].session_id(), Some("s1"));

        assert_eq!(back.agents[1].name, "cl");
        assert!(matches!(back.agents[1].kind, AgentKind::Claude { .. }));
        assert_eq!(back.agents[1].session_id(), None);
    }
}

use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::PathBuf;

pub const DEFAULT_PROJECT_NAME: &str = "Default";
pub const MAX_PROJECTS: usize = 10;

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
        flowmux_agent_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        transcript_path: Option<String>,
    },
    Codex {
        port: u16,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_id: Option<String>,
    },
    Pi {
        flowmux_agent_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_id: Option<String>,
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
    #[serde(default = "default_project_name")]
    pub project: String,
    #[serde(flatten)]
    pub kind: AgentKind,
    /// Absolute path to the git repository root the worktree belongs to.
    /// `Some(_)` iff this agent was launched with a git worktree; the agent's
    /// `directory` field already points to the worktree path itself.
    /// Required for `git worktree remove` and branch deletion on agent removal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_repo_root: Option<String>,
    /// Local branch checked out in the worktree, when Flowmux created one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_worktree_branch: Option<String>,
    /// Base ref used to create the local worktree branch, if it did not come
    /// from the repository's current HEAD.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_worktree_base_ref: Option<String>,
}

impl AgentConfig {
    /// Return a display string for the agent type (e.g. "opencode", "claude", "codex").
    pub fn agent_type_str(&self) -> &'static str {
        match &self.kind {
            AgentKind::Opencode { .. } => "opencode",
            AgentKind::Claude { .. } => "claude",
            AgentKind::Codex { .. } => "codex",
            AgentKind::Pi { .. } => "pi",
        }
    }

    /// Convenience: return the session_id regardless of agent kind.
    pub fn session_id(&self) -> Option<&str> {
        match &self.kind {
            AgentKind::Opencode { session_id, .. } => session_id.as_deref(),
            AgentKind::Claude { session_id, .. } => session_id.as_deref(),
            AgentKind::Codex { session_id, .. } => session_id.as_deref(),
            AgentKind::Pi { session_id, .. } => session_id.as_deref(),
        }
    }

    /// Convenience: set the session_id on whichever kind is active.
    pub fn set_session_id(&mut self, id: Option<String>) {
        match &mut self.kind {
            AgentKind::Opencode { session_id, .. } => *session_id = id,
            AgentKind::Claude { session_id, .. } => *session_id = id,
            AgentKind::Codex { session_id, .. } => *session_id = id,
            AgentKind::Pi { session_id, .. } => *session_id = id,
        }
    }
}

fn default_project_name() -> String {
    DEFAULT_PROJECT_NAME.to_string()
}

// ---------------------------------------------------------------------------
// Config (runtime) / ConfigFile (on-disk)
// ---------------------------------------------------------------------------

/// Serialisable portion of the config (agents list only).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct ConfigFile {
    #[serde(default)]
    projects: Vec<String>,
    #[serde(default)]
    agents: Vec<AgentConfig>,
}

/// Runtime config: the agents list plus the session name that determines
/// which file on disk this config is bound to.  The `session_name` field is
/// never written to disk.
#[derive(Debug, Clone, Default)]
pub struct Config {
    pub projects: Vec<String>,
    pub agents: Vec<AgentConfig>,
    /// The tmux session name this config belongs to.  Set by `load()`.
    pub session_name: String,
}

/// Path to the per-session config file:
///   `~/.config/flowmux/sessions/<session>.toml`
pub fn config_path(session: &str) -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("flowmux")
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
                projects: vec![default_project_name()],
                agents: Vec::new(),
                session_name: session.to_string(),
            });
        }

        let content =
            std::fs::read_to_string(&path).with_context(|| format!("read config {:?}", path))?;

        let file: ConfigFile =
            toml::from_str(&content).with_context(|| format!("parse config {:?}", path))?;

        let mut config = Config {
            projects: file.projects,
            agents: file.agents,
            session_name: session.to_string(),
        };
        config.normalize();

        Ok(config)
    }

    pub fn save(&self) -> anyhow::Result<()> {
        let path = config_path(&self.session_name);

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create config dir {:?}", parent))?;
        }

        let mut normalized = self.clone();
        normalized.normalize();
        let file = ConfigFile {
            projects: normalized.projects,
            agents: normalized.agents,
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

    pub fn normalize(&mut self) {
        let mut projects = Vec::new();
        let mut seen = HashSet::new();

        for raw_name in &self.projects {
            let name = raw_name.trim();
            if name.is_empty() || !seen.insert(name.to_string()) {
                continue;
            }
            projects.push(name.to_string());
            if projects.len() == MAX_PROJECTS {
                break;
            }
        }

        if let Some(default_idx) = projects
            .iter()
            .position(|name| name == DEFAULT_PROJECT_NAME)
        {
            if default_idx != 0 {
                let default = projects.remove(default_idx);
                projects.insert(0, default);
            }
        } else {
            projects.insert(0, default_project_name());
        }

        if projects.len() > MAX_PROJECTS {
            projects.truncate(MAX_PROJECTS);
        }

        let project_set: HashSet<&str> = projects.iter().map(String::as_str).collect();
        for agent in &mut self.agents {
            let project = agent.project.trim();
            if project.is_empty() || !project_set.contains(project) {
                agent.project = default_project_name();
            } else if agent.project != project {
                agent.project = project.to_string();
            }
        }

        self.projects = projects;
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
                pane: "flowmux:1.0".into(),
                directory: "/tmp".into(),
                project: "Default".into(),
                kind: AgentKind::Opencode {
                    port: 9000,
                    session_id: Some("s1".into()),
                },
                git_repo_root: None,
                git_worktree_branch: None,
                git_worktree_base_ref: None,
            },
            AgentConfig {
                name: "cl".into(),
                pane: "flowmux:2.0".into(),
                directory: "/tmp/wt".into(),
                project: "work".into(),
                kind: AgentKind::Claude {
                    flowmux_agent_id: "abc-123".into(),
                    session_id: None,
                    transcript_path: Some("/tmp/t.jsonl".into()),
                },
                git_repo_root: Some("/tmp/repo".into()),
                git_worktree_branch: Some("cl-help".into()),
                git_worktree_base_ref: Some("origin/feature".into()),
            },
            AgentConfig {
                name: "cx".into(),
                pane: "flowmux:3.0".into(),
                directory: "/tmp/codex".into(),
                project: "Default".into(),
                kind: AgentKind::Codex {
                    port: 9100,
                    session_id: Some("thread-1".into()),
                },
                git_repo_root: None,
                git_worktree_branch: None,
                git_worktree_base_ref: None,
            },
            AgentConfig {
                name: "pi".into(),
                pane: "flowmux:4.0".into(),
                directory: "/tmp/pi".into(),
                project: "work".into(),
                kind: AgentKind::Pi {
                    flowmux_agent_id: "pi-agent-id".into(),
                    session_id: Some("pi-session-id".into()),
                },
                git_repo_root: None,
                git_worktree_branch: None,
                git_worktree_base_ref: None,
            },
        ];

        #[derive(Serialize, Deserialize)]
        struct File {
            projects: Vec<String>,
            agents: Vec<AgentConfig>,
        }

        let toml_str = toml::to_string_pretty(&File {
            projects: vec!["Default".into(), "work".into()],
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
        assert_eq!(back.agents[1].project, "work");
        assert_eq!(
            back.agents[1].git_worktree_branch.as_deref(),
            Some("cl-help")
        );
        assert_eq!(
            back.agents[1].git_worktree_base_ref.as_deref(),
            Some("origin/feature")
        );

        assert!(matches!(
            back.agents[2].kind,
            AgentKind::Codex { port: 9100, .. }
        ));
        assert_eq!(back.agents[2].session_id(), Some("thread-1"));

        assert!(matches!(back.agents[3].kind, AgentKind::Pi { .. }));
        assert_eq!(back.agents[3].session_id(), Some("pi-session-id"));
    }

    #[test]
    fn config_normalize_adds_default_project_and_repairs_unknown_agent_projects() {
        let mut config = Config {
            projects: vec!["work".into(), "work".into(), "  ".into()],
            agents: vec![
                AgentConfig {
                    name: "oc".into(),
                    pane: "flowmux:1.0".into(),
                    directory: "/tmp".into(),
                    project: String::new(),
                    kind: AgentKind::Opencode {
                        port: 9000,
                        session_id: None,
                    },
                    git_repo_root: None,
                    git_worktree_branch: None,
                    git_worktree_base_ref: None,
                },
                AgentConfig {
                    name: "cl".into(),
                    pane: "flowmux:2.0".into(),
                    directory: "/tmp".into(),
                    project: "missing".into(),
                    kind: AgentKind::Claude {
                        flowmux_agent_id: "id".into(),
                        session_id: None,
                        transcript_path: None,
                    },
                    git_repo_root: None,
                    git_worktree_branch: None,
                    git_worktree_base_ref: None,
                },
            ],
            session_name: "flowmux".into(),
        };

        config.normalize();

        assert_eq!(
            config.projects,
            vec!["Default".to_string(), "work".to_string()]
        );
        assert_eq!(config.agents[0].project, "Default");
        assert_eq!(config.agents[1].project, "Default");
    }

    #[test]
    fn agent_config_defaults_missing_git_worktree_metadata() {
        let config: AgentConfig = toml::from_str(
            r#"
name = "agent"
pane = "flowmux:1.0"
directory = "/tmp/worktree"
project = "Default"
agent_type = "codex"
port = 9000
git_repo_root = "/tmp/repo"
"#,
        )
        .unwrap();

        assert_eq!(config.git_repo_root.as_deref(), Some("/tmp/repo"));
        assert_eq!(config.git_worktree_branch, None);
        assert_eq!(config.git_worktree_base_ref, None);
    }
}

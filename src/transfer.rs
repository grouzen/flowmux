//! Version 1 of the read-only remote listing protocol.

use std::collections::BTreeSet;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::config::{AgentConfig, Config};
use crate::models::{AgentEntry, AgentStatus};

pub const PROTOCOL_VERSION: u32 = 1;
const STATUS_MAX_AGE: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Listing {
    pub version: u32,
    pub sessions: Vec<SessionListing>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionListing {
    pub name: String,
    pub online: bool,
    pub agents: Vec<AgentListing>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentListing {
    /// Opaque identifier within the source Flowmux session.
    pub id: String,
    pub name: String,
    pub agent_type: String,
    pub session_id: Option<String>,
    pub directory: String,
    pub status: TransferStatus,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TransferStatus {
    Starting,
    Running,
    WaitingForInput,
    Idle,
    Stopped,
    Unknown,
}

impl From<&AgentStatus> for TransferStatus {
    fn from(status: &AgentStatus) -> Self {
        match status {
            AgentStatus::Starting => Self::Starting,
            AgentStatus::Running => Self::Running,
            AgentStatus::WaitingForInput => Self::WaitingForInput,
            AgentStatus::Idle => Self::Idle,
            AgentStatus::Stopped => Self::Stopped,
            AgentStatus::Unknown => Self::Unknown,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct StatusSnapshot {
    version: u32,
    written_at_ms: u64,
    agents: Vec<AgentStatusSnapshot>,
}

#[derive(Debug, Serialize, Deserialize)]
struct AgentStatusSnapshot {
    id: String,
    session_id: Option<String>,
    status: TransferStatus,
}

fn now_ms() -> Result<u64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before Unix epoch")?
        .as_millis() as u64)
}

fn snapshot_path(session: &str) -> PathBuf {
    crate::config::config_path(session).with_extension("status.json")
}

/// Remove a previous snapshot at startup and this process's snapshot at exit.
/// The dashboard holds the per-session lock for the guard's lifetime.
pub struct StatusSnapshotGuard {
    path: PathBuf,
}

impl StatusSnapshotGuard {
    pub fn new(session: &str) -> Result<Self> {
        let path = snapshot_path(session);
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        Ok(Self { path })
    }
}

impl Drop for StatusSnapshotGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Publish only status and identity. The persisted TOML remains the source of
/// truth for which agents belong to a session.
pub fn publish_status(config: &Config, agents: &[AgentEntry]) -> Result<()> {
    let snapshot = StatusSnapshot {
        version: PROTOCOL_VERSION,
        written_at_ms: now_ms()?,
        agents: config
            .agents
            .iter()
            .map(|agent| AgentStatusSnapshot {
                id: agent.pane.clone(),
                session_id: agent.session_id().map(str::to_owned),
                status: agents
                    .iter()
                    .find(|entry| entry.config.pane == agent.pane)
                    .map(|entry| TransferStatus::from(&entry.meta.status))
                    .unwrap_or(TransferStatus::Unknown),
            })
            .collect(),
    };
    let path = snapshot_path(&config.session_name);
    let parent = path.parent().context("status snapshot has no parent")?;
    std::fs::create_dir_all(parent)?;
    let temp = parent.join(format!(".transfer-status-{}.tmp", uuid::Uuid::new_v4()));
    let bytes = serde_json::to_vec(&snapshot)?;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temp)?;
    file.write_all(&bytes)?;
    drop(file);
    if let Err(error) = std::fs::rename(&temp, &path) {
        let _ = std::fs::remove_file(&temp);
        return Err(error.into());
    }
    Ok(())
}

fn fresh_snapshot(path: &Path, now: u64) -> Option<StatusSnapshot> {
    let snapshot: StatusSnapshot = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    let age = now.checked_sub(snapshot.written_at_ms)?;
    (snapshot.version == PROTOCOL_VERSION && age <= STATUS_MAX_AGE.as_millis() as u64)
        .then_some(snapshot)
}

fn list_session(config: Config, snapshot: Option<StatusSnapshot>) -> SessionListing {
    let online = snapshot.is_some();
    let agents = config
        .agents
        .iter()
        .map(|agent| {
            let status = snapshot
                .as_ref()
                .and_then(|snapshot| {
                    snapshot.agents.iter().find(|live| {
                        live.id == agent.pane && live.session_id.as_deref() == agent.session_id()
                    })
                })
                .map(|live| live.status)
                .unwrap_or(TransferStatus::Unknown);
            agent_listing(agent, status)
        })
        .collect();
    SessionListing {
        name: config.session_name,
        online,
        agents,
    }
}

fn agent_listing(agent: &AgentConfig, status: TransferStatus) -> AgentListing {
    AgentListing {
        id: agent.pane.clone(),
        name: agent.name.clone(),
        agent_type: agent.agent_type_str().to_string(),
        session_id: agent.session_id().map(str::to_owned),
        directory: agent.directory.clone(),
        status,
    }
}

pub fn list() -> Result<Listing> {
    let sessions_dir = crate::config::config_path("placeholder")
        .parent()
        .context("session config has no parent")?
        .to_path_buf();
    let mut sessions = Vec::new();
    if !sessions_dir.exists() {
        return Ok(Listing {
            version: PROTOCOL_VERSION,
            sessions,
        });
    }
    let now = now_ms()?;
    for name in discover_session_names(&sessions_dir, now)? {
        let config = if crate::config::config_path(&name).exists() {
            Config::load(&name)?
        } else {
            Config {
                session_name: name.clone(),
                ..Config::default()
            }
        };
        let snapshot = fresh_snapshot(&snapshot_path(&name), now);
        sessions.push(list_session(config, snapshot));
    }
    Ok(Listing {
        version: PROTOCOL_VERSION,
        sessions,
    })
}

fn discover_session_names(sessions_dir: &Path, now: u64) -> Result<BTreeSet<String>> {
    let mut names = BTreeSet::new();
    for entry in std::fs::read_dir(sessions_dir)? {
        let entry = entry?;
        let path = entry.path();
        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if let Some(name) = file_name.strip_suffix(".toml") {
            names.insert(name.to_string());
        } else if let Some(name) = file_name.strip_suffix(".status.json")
            && fresh_snapshot(&path, now).is_some()
        {
            names.insert(name.to_string());
        }
    }
    Ok(names)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AgentKind;

    fn config() -> Config {
        Config {
            session_name: "demo".into(),
            agents: vec![AgentConfig {
                name: "agent".into(),
                pane: "demo:1.0".into(),
                directory: "/work".into(),
                project: "Default".into(),
                kind: AgentKind::Codex {
                    port: 16100,
                    session_id: Some("native-1".into()),
                },
                git_repo_root: None,
                git_worktree_branch: None,
                git_worktree_base_ref: None,
            }],
            ..Config::default()
        }
    }

    #[test]
    fn offline_session_never_exposes_idle() {
        let result = list_session(config(), None);
        assert!(!result.online);
        assert_eq!(result.agents[0].status, TransferStatus::Unknown);
    }

    #[test]
    fn status_requires_matching_native_id() {
        let snapshot = StatusSnapshot {
            version: PROTOCOL_VERSION,
            written_at_ms: 0,
            agents: vec![AgentStatusSnapshot {
                id: "demo:1.0".into(),
                session_id: Some("older-native-id".into()),
                status: TransferStatus::Idle,
            }],
        };
        let result = list_session(config(), Some(snapshot));
        assert!(result.online);
        assert_eq!(result.agents[0].status, TransferStatus::Unknown);
    }

    #[test]
    fn stale_snapshot_is_offline() {
        let path = std::env::temp_dir().join(format!("flowmux-status-{}", uuid::Uuid::new_v4()));
        let snapshot = StatusSnapshot {
            version: PROTOCOL_VERSION,
            written_at_ms: 1000,
            agents: Vec::new(),
        };
        std::fs::write(&path, serde_json::to_vec(&snapshot).unwrap()).unwrap();
        assert!(fresh_snapshot(&path, 5000).is_none());
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn fresh_snapshot_discovers_session_without_toml() {
        let dir = std::env::temp_dir().join(format!("flowmux-sessions-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&dir).unwrap();
        let snapshot = StatusSnapshot {
            version: PROTOCOL_VERSION,
            written_at_ms: 1000,
            agents: Vec::new(),
        };
        std::fs::write(
            dir.join("new.status.json"),
            serde_json::to_vec(&snapshot).unwrap(),
        )
        .unwrap();
        assert!(discover_session_names(&dir, 2000).unwrap().contains("new"));
        assert!(!discover_session_names(&dir, 5000).unwrap().contains("new"));
        std::fs::remove_dir_all(dir).unwrap();
    }
}

//! Version 1 of the read-only remote listing protocol.

use std::collections::BTreeSet;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use tokio::process::Command;

use crate::config::{AgentConfig, Config};
use crate::global_config::GlobalConfig;
use crate::models::{AgentEntry, AgentStatus, AgentType};

pub const PROTOCOL_VERSION: u32 = 1;
const STATUS_MAX_AGE: Duration = Duration::from_secs(3);
const SSH_TIMEOUT: Duration = Duration::from_secs(20);
const COPY_TIMEOUT: Duration = Duration::from_secs(300);
const MAX_COMMAND_OUTPUT: usize = 4 * 1024 * 1024;

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

/// Stable identity from the first picker selection. The source is fetched
/// again immediately before the copy command runs.
pub struct CopyRequest<'a> {
    pub host: &'a str,
    pub flowmux_session: &'a str,
    pub source_id: &'a str,
    pub source_session_id: &'a str,
    pub agent_type: &'a str,
    pub destination_directory: &'a Path,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopiedAgent {
    pub name: String,
    pub agent_type: String,
    pub session_id: String,
    pub directory: PathBuf,
}

#[derive(Debug, Deserialize)]
struct CopyCommandResult {
    version: u32,
    agent_type: String,
    session_id: String,
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

fn validate_host(host: &str) -> Result<()> {
    ensure!(!host.is_empty(), "SSH host is empty");
    ensure!(
        !host.starts_with('-') && !host.chars().any(char::is_whitespace),
        "SSH host contains invalid characters"
    );
    Ok(())
}

async fn output_with_timeout(
    command: &mut Command,
    timeout: Duration,
    label: &str,
) -> Result<Output> {
    command.kill_on_drop(true);
    let output = tokio::time::timeout(timeout, command.output())
        .await
        .with_context(|| format!("{label} timed out after {} seconds", timeout.as_secs()))?
        .with_context(|| format!("failed to run {label}"))?;
    ensure!(
        output.stdout.len() <= MAX_COMMAND_OUTPUT && output.stderr.len() <= MAX_COMMAND_OUTPUT,
        "{label} produced too much output"
    );
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let detail = stderr.trim();
        if detail.is_empty() {
            bail!("{label} exited with {}", output.status);
        }
        bail!("{label} failed: {detail}");
    }
    Ok(output)
}

fn parse_remote_listing(bytes: &[u8]) -> Result<Listing> {
    let listing: Listing = serde_json::from_slice(bytes).context("invalid remote listing JSON")?;
    ensure!(
        listing.version == PROTOCOL_VERSION,
        "unsupported remote listing version {} (expected {})",
        listing.version,
        PROTOCOL_VERSION
    );
    Ok(listing)
}

/// Query the remote Flowmux installation without interpolating the host into
/// a shell command. SSH uses the user's host aliases and configuration.
pub async fn fetch_remote_listing(host: &str) -> Result<Listing> {
    validate_host(host)?;
    let output = output_with_timeout(
        Command::new("ssh")
            .args(["-o", "BatchMode=yes", "-o", "ConnectTimeout=10", "--", host])
            .args(["flowmux", "transfer", "list"]),
        SSH_TIMEOUT,
        "SSH remote listing",
    )
    .await?;
    parse_remote_listing(&output.stdout)
}

fn validate_selected_source<'a>(
    listing: &'a Listing,
    request: &CopyRequest<'_>,
    available_types: &[AgentType],
) -> Result<&'a AgentListing> {
    let session = listing
        .sessions
        .iter()
        .find(|session| session.name == request.flowmux_session)
        .context("remote Flowmux session no longer exists")?;
    ensure!(session.online, "remote Flowmux session is offline");
    let agent = session
        .agents
        .iter()
        .find(|agent| agent.id == request.source_id)
        .context("source agent no longer exists")?;
    ensure!(
        agent.agent_type == request.agent_type
            && agent.session_id.as_deref() == Some(request.source_session_id),
        "source agent identity changed; select it again"
    );
    ensure!(
        available_types
            .iter()
            .any(|kind| kind.name() == agent.agent_type),
        "source agent type {} is not enabled locally",
        agent.agent_type
    );
    ensure!(
        agent.status == TransferStatus::Idle,
        "source agent is {:?}, not idle",
        agent.status
    );
    ensure!(
        !request.source_session_id.is_empty(),
        "source agent has no native session ID"
    );
    Ok(agent)
}

fn parse_copy_result(bytes: &[u8], agent_type: &str, source_id: &str) -> Result<String> {
    let result: CopyCommandResult =
        serde_json::from_slice(bytes).context("invalid copy-command JSON result")?;
    ensure!(
        result.version == PROTOCOL_VERSION,
        "unsupported copy result version {} (expected {})",
        result.version,
        PROTOCOL_VERSION
    );
    ensure!(
        result.agent_type == agent_type,
        "copy result agent type changed"
    );
    ensure!(
        !result.session_id.trim().is_empty() && result.session_id.trim() == result.session_id,
        "copy result has an invalid native session ID"
    );
    ensure!(
        result.session_id != source_id,
        "copy command returned the source native session ID"
    );
    Ok(result.session_id)
}

/// Recheck the source and run the configured local copy command. The caller
/// must verify the returned native ID with its agent adapter before creating
/// or persisting a local agent card.
pub async fn copy_native_session(
    config: &GlobalConfig,
    request: &CopyRequest<'_>,
    available_types: &[AgentType],
) -> Result<CopiedAgent> {
    let directory = request
        .destination_directory
        .canonicalize()
        .context("destination directory does not exist")?;
    ensure!(directory.is_dir(), "destination is not a directory");
    let parts = config
        .agent_copy_command
        .as_ref()
        .filter(|parts| !parts.is_empty() && !parts[0].is_empty())
        .context("agent_copy_command is not configured")?;
    let listing = fetch_remote_listing(request.host).await?;
    let source = validate_selected_source(&listing, request, available_types)?;
    let destination = directory.to_string_lossy();
    let mut command = Command::new(&parts[0]);
    command.args(&parts[1..]).args([
        "--protocol-version",
        "1",
        "--host",
        request.host,
        "--flowmux-session",
        request.flowmux_session,
        "--source-id",
        &source.id,
        "--agent-name",
        &source.name,
        "--agent-type",
        &source.agent_type,
        "--source-session-id",
        request.source_session_id,
        "--source-directory",
        &source.directory,
        "--destination-directory",
        &destination,
    ]);
    let output = output_with_timeout(&mut command, COPY_TIMEOUT, "agent copy command").await?;
    let session_id = parse_copy_result(
        &output.stdout,
        &source.agent_type,
        request.source_session_id,
    )?;
    Ok(CopiedAgent {
        name: source.name.clone(),
        agent_type: source.agent_type.clone(),
        session_id,
        directory,
    })
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

    #[test]
    fn remote_listing_rejects_incompatible_version() {
        let error = parse_remote_listing(br#"{"version":2,"sessions":[]}"#).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("unsupported remote listing version")
        );
    }

    #[test]
    fn copy_result_requires_new_native_session() {
        assert!(
            parse_copy_result(
                br#"{"version":1,"agent_type":"codex","session_id":"new"}"#,
                "codex",
                "old"
            )
            .is_ok()
        );
        let error = parse_copy_result(
            br#"{"version":1,"agent_type":"codex","session_id":"old"}"#,
            "codex",
            "old",
        )
        .unwrap_err();
        assert!(error.to_string().contains("source native session ID"));
    }

    #[test]
    fn changed_source_status_blocks_copy() {
        let listing = Listing {
            version: PROTOCOL_VERSION,
            sessions: vec![SessionListing {
                name: "demo".into(),
                online: true,
                agents: vec![agent_listing(&config().agents[0], TransferStatus::Running)],
            }],
        };
        let request = CopyRequest {
            host: "laptop",
            flowmux_session: "demo",
            source_id: "demo:1.0",
            source_session_id: "native-1",
            agent_type: "codex",
            destination_directory: Path::new("/tmp"),
        };
        let error = validate_selected_source(&listing, &request, &[AgentType::Codex]).unwrap_err();
        assert!(error.to_string().contains("not idle"));
    }
}

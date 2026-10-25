use std::path::Path;
use std::time::Instant;

use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Clear, Paragraph, Wrap},
};

use crate::models::AgentType;
use crate::transfer::{AgentListing, Listing, SessionListing, TransferStatus};
use crate::ui::theme::Theme;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportStage {
    Host,
    Credential,
    Connecting,
    Session,
    Agent,
    Directory,
    Copying,
    Launching,
}

#[derive(Debug, Clone)]
pub struct SelectedSource {
    pub session: String,
    pub id: String,
    pub session_id: String,
    pub agent_type: String,
    pub name: String,
    pub directory: String,
}

#[derive(Clone)]
pub struct ImportAgentState {
    pub stage: ImportStage,
    pub host: String,
    pub credential: Option<String>,
    pub credential_input: String,
    pub saved_hosts: Vec<String>,
    pub host_idx: usize,
    pub listing: Option<Listing>,
    pub session_idx: usize,
    pub agent_idx: usize,
    pub selected: Option<SelectedSource>,
    pub directory: String,
    pub available_types: Vec<AgentType>,
    pub error: Option<String>,
    pub refresh_in_flight: bool,
    pub last_refresh: Option<Instant>,
}

impl ImportAgentState {
    pub fn new(saved_hosts: Vec<String>, available_types: Vec<AgentType>) -> Self {
        let host = saved_hosts.first().cloned().unwrap_or_default();
        Self {
            stage: ImportStage::Host,
            host,
            credential: None,
            credential_input: String::new(),
            saved_hosts,
            host_idx: 0,
            listing: None,
            session_idx: 0,
            agent_idx: 0,
            selected: None,
            directory: String::new(),
            available_types,
            error: None,
            refresh_in_flight: false,
            last_refresh: None,
        }
    }

    pub fn session(&self) -> Option<&SessionListing> {
        self.listing.as_ref()?.sessions.get(self.session_idx)
    }

    pub fn visible_agents(&self) -> Vec<&AgentListing> {
        self.session()
            .into_iter()
            .flat_map(|session| session.agents.iter())
            .filter(|agent| {
                self.available_types
                    .iter()
                    .any(|kind| kind.name() == agent.agent_type)
            })
            .collect()
    }

    pub fn agent_disabled_reason(&self, agent: &AgentListing) -> Option<&'static str> {
        if self.session().is_none_or(|session| !session.online) {
            return Some("Flowmux offline");
        }
        if agent.session_id.as_deref().is_none_or(str::is_empty) {
            return Some("no native session ID");
        }
        match agent.status {
            TransferStatus::Idle => None,
            TransferStatus::Starting => Some("starting"),
            TransferStatus::Running => Some("running"),
            TransferStatus::WaitingForInput => Some("waiting for input"),
            TransferStatus::Stopped => Some("stopped"),
            TransferStatus::Unknown => Some("status unknown"),
        }
    }

    pub fn choose_agent(&mut self) -> Result<(), String> {
        let (session_name, agent) = {
            let session = self.session().ok_or("select a remote session")?;
            let agent = self
                .visible_agents()
                .get(self.agent_idx)
                .copied()
                .ok_or("select an agent")?;
            if let Some(reason) = self.agent_disabled_reason(agent) {
                return Err(reason.to_string());
            }
            (session.name.clone(), agent.clone())
        };
        self.directory = if Path::new(&agent.directory).is_dir() {
            agent.directory.clone()
        } else {
            String::new()
        };
        self.selected = Some(SelectedSource {
            session: session_name,
            id: agent.id,
            session_id: agent
                .session_id
                .expect("eligible agent has a native session ID"),
            agent_type: agent.agent_type,
            name: agent.name,
            directory: agent.directory,
        });
        self.stage = ImportStage::Directory;
        Ok(())
    }
}

pub fn render(f: &mut Frame, area: Rect, theme: &Theme, state: &ImportAgentState) {
    let width = area.width.saturating_sub(4).min(84);
    let height = area.height.saturating_sub(4).min(21);
    let modal = Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    };
    f.render_widget(Clear, modal);
    f.render_widget(
        Block::default().style(Style::default().bg(theme.bg1)),
        modal,
    );
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(4),
            Constraint::Length(1),
        ])
        .split(modal);
    let title = match state.stage {
        ImportStage::Host | ImportStage::Connecting => "Import agent · SSH host",
        ImportStage::Credential => "Import agent · SSH authentication",
        ImportStage::Session => "Import agent · remote session",
        ImportStage::Agent => "Import agent · source agent",
        ImportStage::Directory | ImportStage::Copying | ImportStage::Launching => {
            "Import agent · local directory"
        }
    };
    f.render_widget(
        Paragraph::new(Line::styled(
            format!("  {title}"),
            Style::default().fg(theme.fg).add_modifier(Modifier::BOLD),
        )),
        rows[1],
    );

    let mut lines = Vec::new();
    match state.stage {
        ImportStage::Host | ImportStage::Connecting => {
            lines.push(Line::raw(format!("  Host: {}", state.host)));
            lines.push(Line::raw(""));
            lines.push(Line::styled(
                "  Saved hosts (up/down selects):",
                Style::default().fg(theme.gray),
            ));
            for (idx, host) in state
                .saved_hosts
                .iter()
                .enumerate()
                .skip(state.host_idx.saturating_sub(9))
                .take(10)
            {
                lines.push(item_line(host, idx == state.host_idx, theme));
            }
            if state.stage == ImportStage::Connecting {
                lines.push(Line::raw("  Connecting over SSH…"));
            }
        }
        ImportStage::Credential => {
            lines.push(Line::raw(format!("  Host: {}", state.host)));
            lines.push(Line::raw(""));
            lines.push(Line::raw("  SSH password or key passphrase:"));
            lines.push(Line::raw(format!(
                "  {}",
                "•".repeat(state.credential_input.chars().count())
            )));
            lines.push(Line::raw("  Kept only for this import; never saved."));
        }
        ImportStage::Session => {
            if let Some(listing) = &state.listing {
                for (idx, session) in listing
                    .sessions
                    .iter()
                    .enumerate()
                    .skip(state.session_idx.saturating_sub(11))
                    .take(12)
                {
                    let label = if session.online {
                        format!("{}  ({} agents)", session.name, session.agents.len())
                    } else {
                        format!("{}  (offline)", session.name)
                    };
                    lines.push(item_line(&label, idx == state.session_idx, theme));
                }
                if listing.sessions.is_empty() {
                    lines.push(Line::raw("  No Flowmux sessions found"));
                }
            }
        }
        ImportStage::Agent => {
            for (idx, agent) in state
                .visible_agents()
                .iter()
                .enumerate()
                .skip(state.agent_idx.saturating_sub(11))
                .take(12)
            {
                let reason = state.agent_disabled_reason(agent);
                let label = match reason {
                    Some(reason) => format!("{} ({}) · {reason}", agent.name, agent.agent_type),
                    None => format!("{} ({}) · idle", agent.name, agent.agent_type),
                };
                lines.push(item_line(&label, idx == state.agent_idx, theme));
            }
            if state.visible_agents().is_empty() {
                lines.push(Line::raw("  No locally enabled agents in this session"));
            }
        }
        ImportStage::Directory | ImportStage::Copying | ImportStage::Launching => {
            if let Some(source) = &state.selected {
                lines.push(Line::raw(format!("  Source: {}", source.name)));
                lines.push(Line::raw(format!(
                    "  Source directory: {}",
                    source.directory
                )));
            }
            lines.push(Line::raw(format!("  Local directory: {}", state.directory)));
            lines.push(Line::raw(
                "  Use an existing directory with synchronized project files.",
            ));
            if state.stage == ImportStage::Copying {
                lines.push(Line::raw("  Copying native session…"));
            } else if state.stage == ImportStage::Launching {
                lines.push(Line::raw("  Launching local agent…"));
            }
        }
    }
    f.render_widget(
        Paragraph::new(lines).style(Style::default().fg(theme.fg)),
        rows[3],
    );
    if let Some(error) = &state.error {
        f.render_widget(
            Paragraph::new(format!("  {error}"))
                .style(Style::default().fg(theme.red))
                .wrap(Wrap { trim: true }),
            rows[4],
        );
    }
    let help = match state.stage {
        ImportStage::Host => "  type host · up/down saved · enter connect · esc cancel",
        ImportStage::Credential => "  type secret · enter retry · esc back",
        ImportStage::Session | ImportStage::Agent => "  up/down select · enter continue · esc back",
        ImportStage::Directory => "  type path · ctrl-u clear · enter import · esc back",
        _ => "  Please wait…",
    };
    f.render_widget(
        Paragraph::new(help).style(Style::default().fg(theme.gray)),
        rows[5],
    );
}

fn item_line(label: &str, selected: bool, theme: &Theme) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            if selected { "› " } else { "  " },
            Style::default().fg(theme.blue),
        ),
        Span::styled(
            label.to_string(),
            Style::default().fg(if selected { theme.fg } else { theme.gray }),
        ),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn idle_agent_state(directory: String) -> ImportAgentState {
        let mut state = ImportAgentState::new(Vec::new(), vec![AgentType::Codex]);
        state.listing = Some(Listing {
            version: 1,
            sessions: vec![SessionListing {
                name: "remote".into(),
                online: true,
                agents: vec![AgentListing {
                    id: "pane".into(),
                    name: "worker".into(),
                    agent_type: "codex".into(),
                    session_id: Some("native".into()),
                    directory,
                    status: TransferStatus::Idle,
                }],
            }],
        });
        state
    }

    #[test]
    fn existing_source_directory_is_prefilled_locally() {
        let directory = std::env::current_dir()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let mut state = idle_agent_state(directory.clone());
        state.choose_agent().unwrap();
        assert_eq!(state.directory, directory);
        assert_eq!(state.selected.unwrap().directory, directory);
    }

    #[test]
    fn missing_source_directory_requires_local_selection() {
        let directory = format!("/flowmux-missing-source-{}", uuid::Uuid::new_v4());
        let mut state = idle_agent_state(directory.clone());
        state.choose_agent().unwrap();
        assert!(state.directory.is_empty());
        assert_eq!(state.selected.unwrap().directory, directory);
    }

    #[test]
    fn running_agent_is_visible_but_disabled() {
        let mut state = ImportAgentState::new(Vec::new(), vec![AgentType::Codex]);
        state.listing = Some(Listing {
            version: 1,
            sessions: vec![SessionListing {
                name: "remote".into(),
                online: true,
                agents: vec![AgentListing {
                    id: "pane".into(),
                    name: "worker".into(),
                    agent_type: "codex".into(),
                    session_id: Some("native".into()),
                    directory: "/remote".into(),
                    status: TransferStatus::Running,
                }],
            }],
        });
        assert_eq!(state.visible_agents().len(), 1);
        assert_eq!(state.choose_agent(), Err("running".into()));
    }
}

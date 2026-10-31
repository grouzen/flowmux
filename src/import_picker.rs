use std::path::Path;
use std::time::Instant;

use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Margin, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Cell, Clear, Paragraph, Row, Table, Wrap},
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::app::TextInputState;
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportField {
    Name,
    Directory,
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
    pub host: TextInputState,
    pub credential: Option<String>,
    pub credential_input: String,
    pub saved_hosts: Vec<String>,
    pub host_idx: usize,
    pub host_list_focused: bool,
    pub listing: Option<Listing>,
    pub session_idx: usize,
    pub agent_idx: usize,
    pub selected: Option<SelectedSource>,
    pub local_name: TextInputState,
    pub directory: String,
    pub dir_filter: String,
    pub dir_matches: Vec<String>,
    pub dir_selected_idx: usize,
    pub focus: ImportField,
    pub available_types: Vec<AgentType>,
    pub error: Option<String>,
    pub refresh_in_flight: bool,
    pub last_refresh: Option<Instant>,
}

impl ImportAgentState {
    pub fn new(saved_hosts: Vec<String>, available_types: Vec<AgentType>) -> Self {
        let host_value = saved_hosts.first().cloned().unwrap_or_default();
        let host = TextInputState {
            cursor: host_value.len(),
            value: host_value,
        };
        Self {
            stage: ImportStage::Host,
            host,
            credential: None,
            credential_input: String::new(),
            saved_hosts,
            host_idx: 0,
            host_list_focused: false,
            listing: None,
            session_idx: 0,
            agent_idx: 0,
            selected: None,
            local_name: TextInputState::default(),
            directory: String::new(),
            dir_filter: String::new(),
            dir_matches: Vec::new(),
            dir_selected_idx: 0,
            focus: ImportField::Name,
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
            .collect()
    }

    pub fn agent_disabled_reason(&self, agent: &AgentListing) -> Option<&'static str> {
        if self.session().is_none_or(|session| !session.online) {
            return Some("Flowmux offline");
        }
        if agent.session_id.as_deref().is_none_or(str::is_empty) {
            return Some("no native session ID");
        }
        if !self
            .available_types
            .iter()
            .any(|kind| kind.name() == agent.agent_type)
        {
            return Some("agent type is not enabled locally");
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
        self.local_name = TextInputState {
            cursor: agent.name.len(),
            value: agent.name.clone(),
        };
        self.directory = agent.directory.clone();
        self.dir_filter.clear();
        self.focus = ImportField::Name;
        self.refresh_dir_matches();
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

    pub fn refresh_dir_matches(&mut self) {
        let base = if self.directory.is_empty() {
            "/"
        } else {
            self.directory.as_str()
        };
        self.dir_matches = crate::app::list_subdirectories(Path::new(base), &self.dir_filter);
        self.dir_selected_idx = 0;
    }

    pub fn select_directory_match(&mut self) -> bool {
        let Some(name) = self.dir_matches.get(self.dir_selected_idx).cloned() else {
            return false;
        };
        let base = if self.directory.is_empty() {
            "/"
        } else {
            &self.directory
        };
        self.directory = Path::new(base).join(name).to_string_lossy().into_owned();
        self.dir_filter.clear();
        self.refresh_dir_matches();
        true
    }
}

pub fn render(f: &mut Frame, area: Rect, theme: &Theme, state: &ImportAgentState) {
    let width = ((area.width as u32 * 60 / 100) as u16)
        .max(60)
        .min(area.width.saturating_sub(4))
        .min(100);
    let height = area.height.saturating_sub(4).min(30);
    if width < 16 || height < 8 {
        return;
    }
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

    let inner = modal.inner(Margin {
        horizontal: 3,
        vertical: 1,
    });
    let error_height = if state.error.is_some() { 2 } else { 0 };
    let directory_stage = matches!(
        state.stage,
        ImportStage::Directory | ImportStage::Copying | ImportStage::Launching
    );
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // title
            Constraint::Length(1), // gap
            Constraint::Min(0),    // content
            Constraint::Length(error_height),
            Constraint::Length(u16::from(directory_stage)), // directory tip
            Constraint::Length(1),                          // gap
            Constraint::Length(1),                          // buttons
        ])
        .split(inner);

    let subtitle = match state.stage {
        ImportStage::Host | ImportStage::Connecting => "Connect to an SSH host",
        ImportStage::Credential => "SSH authentication",
        ImportStage::Session => "Select session",
        ImportStage::Agent => "Select agent",
        ImportStage::Directory | ImportStage::Copying | ImportStage::Launching => {
            "Select local directory"
        }
    };
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                "Import remote agent",
                Style::default().fg(theme.fg).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" ▸ ", Style::default().fg(theme.gray)),
            Span::styled(subtitle, Style::default().fg(theme.cyan)),
        ])),
        rows[0],
    );

    match state.stage {
        ImportStage::Host | ImportStage::Connecting => render_host(f, rows[2], theme, state),
        ImportStage::Credential => render_credential(f, rows[2], theme, state),
        ImportStage::Session => render_sessions(f, rows[2], theme, state),
        ImportStage::Agent => render_agents(f, rows[2], theme, state),
        ImportStage::Directory | ImportStage::Copying | ImportStage::Launching => {
            render_directory(f, rows[2], theme, state)
        }
    }

    if let Some(error) = &state.error {
        f.render_widget(
            Paragraph::new(error.as_str())
                .style(Style::default().fg(theme.red))
                .wrap(Wrap { trim: true }),
            rows[3],
        );
    }

    if directory_stage {
        f.render_widget(
            Paragraph::new("Use a local directory with synchronized project files.")
                .style(Style::default().fg(theme.gray).add_modifier(Modifier::DIM)),
            rows[4],
        );
    }

    let action = match state.stage {
        ImportStage::Host | ImportStage::Connecting => "Connect",
        ImportStage::Credential => "Retry",
        ImportStage::Session | ImportStage::Agent => "Select",
        ImportStage::Directory | ImportStage::Copying | ImportStage::Launching => "Import",
    };
    let back = if state.stage == ImportStage::Host {
        "Cancel"
    } else {
        "Back"
    };
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                format!(" {action} "),
                Style::default()
                    .bg(theme.orange)
                    .fg(theme.fg)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" enter", Style::default().fg(theme.gray)),
            Span::raw("   "),
            Span::styled(
                format!(" {back} "),
                Style::default()
                    .bg(theme.bg2)
                    .fg(theme.fg)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" esc", Style::default().fg(theme.gray)),
        ])),
        rows[6],
    );
}

fn render_host(f: &mut Frame, area: Rect, theme: &Theme, state: &ImportAgentState) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(0),
        ])
        .split(area);
    let prefix = "Host  ";
    let value_width = rows[0].width.saturating_sub(prefix.len() as u16) as usize;
    let (shown, cursor_offset) = input_view(&state.host.value, state.host.cursor, value_width);
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(prefix, Style::default().fg(theme.gray)),
            Span::styled(
                if state.host.value.is_empty() {
                    "type an SSH host..."
                } else {
                    shown.as_str()
                },
                Style::default().fg(if state.host.value.is_empty() {
                    theme.bg2
                } else {
                    theme.fg
                }),
            ),
        ])),
        rows[0],
    );
    let cursor_x = rows[0]
        .x
        .saturating_add(prefix.len() as u16)
        .saturating_add(cursor_offset as u16)
        .min(rows[0].right().saturating_sub(1));
    if !state.host_list_focused {
        f.set_cursor_position((cursor_x, rows[0].y));
    }

    if state.stage == ImportStage::Connecting {
        f.render_widget(
            Paragraph::new("Connecting over SSH…").style(Style::default().fg(theme.gray)),
            rows[2],
        );
    } else {
        f.render_widget(
            Paragraph::new("Saved hosts").style(Style::default().fg(theme.cyan)),
            rows[2],
        );
    }
    if state.saved_hosts.is_empty() {
        f.render_widget(
            Paragraph::new("No saved hosts").style(Style::default().fg(theme.gray)),
            rows[4],
        );
        return;
    }
    render_table(
        f,
        rows[4],
        theme,
        &[("Host", Constraint::Min(1))],
        state.saved_hosts.len(),
        state.host_idx,
        |idx| {
            let selected = state.host_list_focused && idx == state.host_idx;
            (vec![Cell::from(state.saved_hosts[idx].clone())], selected)
        },
    );
}

fn render_credential(f: &mut Frame, area: Rect, theme: &Theme, state: &ImportAgentState) {
    let lines = vec![
        Line::from(vec![
            Span::styled("Host  ", Style::default().fg(theme.gray)),
            Span::styled(state.host.value.as_str(), Style::default().fg(theme.fg)),
        ]),
        Line::raw(""),
        Line::styled(
            "SSH password or key passphrase",
            Style::default().fg(theme.gray),
        ),
        Line::styled(
            "•".repeat(state.credential_input.chars().count()),
            Style::default().fg(theme.fg),
        ),
        Line::raw(""),
        Line::styled(
            "Kept only for this import; never saved.",
            Style::default().fg(theme.gray),
        ),
    ];
    f.render_widget(Paragraph::new(lines), area);
    let cursor_x = area
        .x
        .saturating_add(state.credential_input.chars().count() as u16)
        .min(area.right().saturating_sub(1));
    f.set_cursor_position((cursor_x, area.y.saturating_add(3)));
}

fn render_sessions(f: &mut Frame, area: Rect, theme: &Theme, state: &ImportAgentState) {
    let Some(listing) = &state.listing else {
        return;
    };
    if listing.sessions.is_empty() {
        f.render_widget(
            Paragraph::new("No Flowmux sessions found").style(Style::default().fg(theme.gray)),
            area,
        );
        return;
    }
    render_table(
        f,
        area,
        theme,
        &[
            ("Session", Constraint::Min(12)),
            ("Agents", Constraint::Length(8)),
            ("Status", Constraint::Length(9)),
        ],
        listing.sessions.len(),
        state.session_idx,
        |idx| {
            let session = &listing.sessions[idx];
            let status = if session.online {
                ("online", theme.green)
            } else {
                ("offline", theme.red)
            };
            (
                vec![
                    Cell::from(session.name.clone()),
                    Cell::from(session.agents.len().to_string()),
                    Cell::from(status.0).style(Style::default().fg(status.1)),
                ],
                idx == state.session_idx,
            )
        },
    );
}

fn render_agents(f: &mut Frame, area: Rect, theme: &Theme, state: &ImportAgentState) {
    let agents = state.visible_agents();
    if agents.is_empty() {
        f.render_widget(
            Paragraph::new("No agents in this session").style(Style::default().fg(theme.gray)),
            area,
        );
        return;
    }
    render_table(
        f,
        area,
        theme,
        &[
            ("Agent", Constraint::Min(12)),
            ("Type", Constraint::Length(10)),
            ("Status", Constraint::Length(9)),
        ],
        agents.len(),
        state.agent_idx,
        |idx| {
            let agent = agents[idx];
            let status = status_badge(theme, agent.status);
            (
                vec![
                    Cell::from(agent.name.clone()),
                    Cell::from(agent.agent_type.clone()),
                    Cell::from(status.0).style(Style::default().fg(status.1)),
                ],
                idx == state.agent_idx,
            )
        },
    );
}

fn render_directory(f: &mut Frame, area: Rect, theme: &Theme, state: &ImportAgentState) {
    let at = |row| {
        Rect::new(
            area.x,
            area.y.saturating_add(row),
            area.width,
            u16::from(row < area.height),
        )
    };
    f.render_widget(
        Paragraph::new("Remote agent")
            .style(Style::default().fg(theme.cyan).add_modifier(Modifier::BOLD)),
        at(0),
    );
    if let Some(source) = &state.selected {
        render_detail_row(f, at(1), theme, "Name", &source.name);
        render_detail_row(f, at(2), theme, "Directory", &source.directory);
    }
    f.render_widget(
        Paragraph::new("Local agent")
            .style(Style::default().fg(theme.cyan).add_modifier(Modifier::BOLD)),
        at(4),
    );
    let editable = state.stage == ImportStage::Directory;
    render_input_row(
        f,
        at(5),
        theme,
        "Name",
        &state.local_name.value,
        state.local_name.cursor,
        editable && state.focus == ImportField::Name,
    );
    let directory = if state.dir_filter.is_empty() {
        if Path::new(&state.directory).is_dir() && state.directory != "/" {
            format!("{}/", state.directory.trim_end_matches('/'))
        } else {
            state.directory.clone()
        }
    } else {
        Path::new(&state.directory)
            .join(&state.dir_filter)
            .to_string_lossy()
            .into_owned()
    };
    render_input_row(
        f,
        at(6),
        theme,
        "Directory",
        &directory,
        directory.len(),
        editable && state.focus == ImportField::Directory,
    );
    if editable && !state.dir_matches.is_empty() && area.height > 8 {
        render_table(
            f,
            Rect::new(area.x, area.y + 8, area.width, area.height - 8),
            theme,
            &[("Suggested", Constraint::Min(1))],
            state.dir_matches.len(),
            state.dir_selected_idx,
            |idx| {
                (
                    vec![Cell::from(state.dir_matches[idx].clone())],
                    editable
                        && state.focus == ImportField::Directory
                        && idx == state.dir_selected_idx,
                )
            },
        );
    }
    if state.stage == ImportStage::Copying || state.stage == ImportStage::Launching {
        let label = if state.stage == ImportStage::Copying {
            "Copying native session…"
        } else {
            "Launching local agent…"
        };
        f.render_widget(
            Paragraph::new(label).style(Style::default().fg(theme.cyan)),
            at(area.height.saturating_sub(1)),
        );
    }
}

fn render_detail_row(f: &mut Frame, area: Rect, theme: &Theme, label: &str, value: &str) {
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(format!("{label:<11}"), Style::default().fg(theme.gray)),
            Span::styled(value.to_string(), Style::default().fg(theme.fg)),
        ])),
        area,
    );
}

fn render_input_row(
    f: &mut Frame,
    area: Rect,
    theme: &Theme,
    label: &str,
    value: &str,
    cursor: usize,
    focused: bool,
) {
    let value_width = area.width.saturating_sub(11) as usize;
    let (shown, cursor_offset) = input_view(value, cursor, value_width);
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                format!("{label:<11}"),
                Style::default().fg(if focused { theme.fg } else { theme.gray }),
            ),
            Span::styled(
                if value.is_empty() {
                    "type here..."
                } else {
                    shown.as_str()
                },
                Style::default()
                    .fg(if value.is_empty() {
                        theme.bg2
                    } else if focused {
                        theme.fg
                    } else {
                        theme.gray
                    })
                    .add_modifier(if focused {
                        Modifier::BOLD
                    } else {
                        Modifier::empty()
                    }),
            ),
        ])),
        area,
    );
    if focused && area.height > 0 {
        f.set_cursor_position((
            area.x
                .saturating_add(11 + cursor_offset as u16)
                .min(area.right().saturating_sub(1)),
            area.y,
        ));
    }
}

fn render_table<F>(
    f: &mut Frame,
    area: Rect,
    theme: &Theme,
    columns: &[(&str, Constraint)],
    total: usize,
    selected: usize,
    row: F,
) where
    F: Fn(usize) -> (Vec<Cell<'static>>, bool),
{
    let visible = area.height.saturating_sub(1) as usize;
    if area.height == 0 || area.width < 2 {
        return;
    }
    let offset = list_offset(selected, total, visible);
    let mut table_rows = Vec::new();
    for vi in 0..visible.min(total) {
        let (cells, highlighted) = row(offset + vi);
        let style = if highlighted {
            Style::default()
                .fg(theme.bg)
                .bg(theme.fg)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.gray).bg(theme.bg1)
        };
        table_rows.push(Row::new(cells).style(style));
    }
    let header = Row::new(columns.iter().map(|(name, _)| *name)).style(
        Style::default()
            .fg(theme.cyan)
            .bg(theme.bg2)
            .add_modifier(Modifier::BOLD),
    );
    let table_area = Rect {
        width: area.width - 1,
        ..area
    };
    let table = Table::new(table_rows, columns.iter().map(|(_, width)| *width))
        .header(header)
        .column_spacing(2)
        .style(Style::default().bg(theme.bg1));
    f.render_widget(table, table_area);
    for vi in 0..visible.min(total) {
        let bar = scrollbar_char(vi, visible, offset, total);
        let highlighted = row(offset + vi).1;
        f.render_widget(
            Paragraph::new(bar.to_string()).style(
                Style::default()
                    .fg(theme.bg2)
                    .bg(if highlighted { theme.fg } else { theme.bg1 }),
            ),
            Rect::new(area.right() - 1, area.y + 1 + vi as u16, 1, 1),
        );
    }
}

fn status_badge(theme: &Theme, status: TransferStatus) -> (&'static str, Color) {
    match status {
        TransferStatus::Starting => ("starting", theme.yellow),
        TransferStatus::Running => ("running", theme.green),
        TransferStatus::WaitingForInput => ("waiting", theme.yellow),
        TransferStatus::Idle => ("idle", theme.cyan),
        TransferStatus::Stopped => ("stopped", theme.red),
        TransferStatus::Unknown => ("unknown", theme.gray),
    }
}

fn list_offset(selected: usize, total: usize, visible: usize) -> usize {
    selected
        .saturating_sub(visible / 2)
        .min(total.saturating_sub(visible))
}

fn scrollbar_char(vi: usize, visible: usize, offset: usize, total: usize) -> char {
    if total <= visible || visible == 0 {
        return ' ';
    }
    let thumb_size = (visible * visible / total).max(1);
    let thumb_top = offset * (visible - thumb_size) / (total - visible);
    if (thumb_top..thumb_top + thumb_size).contains(&vi) {
        '█'
    } else {
        '░'
    }
}

fn input_view(value: &str, cursor: usize, width: usize) -> (String, usize) {
    if width == 0 {
        return (String::new(), 0);
    }
    let mut cursor = cursor.min(value.len());
    while !value.is_char_boundary(cursor) {
        cursor -= 1;
    }
    let mut start = 0;
    while UnicodeWidthStr::width(&value[start..cursor]) >= width {
        start = value[start..]
            .chars()
            .next()
            .map_or(cursor, |c| start + c.len_utf8());
    }
    let mut shown = String::new();
    for c in value[start..].chars() {
        if UnicodeWidthStr::width(shown.as_str()) + UnicodeWidthChar::width(c).unwrap_or(0) > width
        {
            break;
        }
        shown.push(c);
    }
    (shown, UnicodeWidthStr::width(&value[start..cursor]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend, buffer::Buffer};

    fn render_buffer(state: &ImportAgentState, width: u16, height: u16) -> Buffer {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| {
                render(
                    frame,
                    Rect::new(0, 0, width, height),
                    &crate::ui::theme::default_theme().theme,
                    state,
                );
            })
            .unwrap();
        terminal.backend().buffer().clone()
    }

    fn buffer_text(buffer: &Buffer) -> String {
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer.cell((x, y)).unwrap().symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

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
                    git_repo_root: None,
                    git_worktree_branch: None,
                    git_worktree_base_ref: None,
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
        assert_eq!(state.directory, directory);
        assert!(state.dir_matches.is_empty());
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
                    git_repo_root: None,
                    git_worktree_branch: None,
                    git_worktree_base_ref: None,
                    status: TransferStatus::Running,
                }],
            }],
        });
        assert_eq!(state.visible_agents().len(), 1);
        assert_eq!(state.choose_agent(), Err("running".into()));
    }

    #[test]
    fn host_dialog_shows_title_buttons_selection_and_scrollbar() {
        let mut state = ImportAgentState::new(
            (0..40).map(|idx| format!("host-{idx:02}")).collect(),
            vec![AgentType::Codex],
        );
        state.host_idx = 20;
        state.host_list_focused = true;
        let buffer = render_buffer(&state, 100, 32);
        let text = buffer_text(&buffer);
        assert!(text.contains("Import remote agent ▸ Connect to an SSH host"));
        assert!(text.contains("Saved hosts"));
        assert!(!text.contains("↑/↓"));
        assert!(text.contains("Host"));
        assert!(text.contains("host-20"));
        assert!(
            !text
                .lines()
                .any(|line| line.contains("host-20") && line.contains("enter"))
        );
        assert!(text.contains("Connect  enter"));
        assert!(text.contains("Cancel  esc"));
        assert!(text.contains('█'));
        assert!(text.contains('░'));
        let theme = crate::ui::theme::default_theme().theme;
        let selection = text
            .lines()
            .position(|line| line.contains("host-20"))
            .unwrap() as u16;
        let row = text.lines().nth(selection as usize).unwrap();
        let start = row.find("host-20").unwrap() as u16;
        assert_eq!(buffer.cell((start, selection)).unwrap().bg, theme.fg);
        assert_eq!(buffer.cell((start + 30, selection)).unwrap().bg, theme.fg);
    }

    #[test]
    fn agent_dialog_fills_height_and_uses_status_palette() {
        let mut state = idle_agent_state("/remote".into());
        let session = &mut state.listing.as_mut().unwrap().sessions[0];
        session.agents = (0..40)
            .map(|idx| AgentListing {
                id: format!("pane-{idx}"),
                name: format!("worker-{idx:02}"),
                agent_type: "codex".into(),
                session_id: Some(format!("native-{idx}")),
                directory: "/remote".into(),
                git_repo_root: None,
                git_worktree_branch: None,
                git_worktree_base_ref: None,
                status: if idx == 20 {
                    TransferStatus::Idle
                } else {
                    TransferStatus::Running
                },
            })
            .collect();
        state.stage = ImportStage::Agent;
        state.agent_idx = 20;
        let buffer = render_buffer(&state, 100, 32);
        let text = buffer_text(&buffer);
        assert!(text.contains("Import remote agent ▸ Select agent"));
        assert!(!text.lines().any(|line| line.trim() == "Agents"));
        assert!(!text.contains("↑/↓"));
        assert!(text.lines().any(|line| line.contains("Agent")
            && line.contains("Type")
            && line.contains("Status")));
        assert!(text.contains("worker-20"));
        assert!(
            !text
                .lines()
                .any(|line| line.contains("worker-20") && line.contains("enter"))
        );
        assert!(text.contains('█'));
        assert!(text.contains('░'));
        let visible_agents = text.lines().filter(|line| line.contains("worker-")).count();
        assert!(visible_agents >= 15, "visible agents: {visible_agents}");
        let theme = crate::ui::theme::default_theme().theme;
        let idle_row = text
            .lines()
            .position(|line| line.contains("worker-20"))
            .unwrap() as u16;
        let idle_col = text
            .lines()
            .nth(idle_row as usize)
            .unwrap()
            .find("idle")
            .unwrap() as u16;
        assert_eq!(buffer.cell((idle_col, idle_row)).unwrap().fg, theme.cyan);
        let running_row = text
            .lines()
            .position(|line| line.contains("running"))
            .unwrap() as u16;
        let running_col = text
            .lines()
            .nth(running_row as usize)
            .unwrap()
            .find("running")
            .unwrap() as u16;
        assert_eq!(
            buffer.cell((running_col, running_row)).unwrap().fg,
            theme.green
        );
    }

    #[test]
    fn input_view_keeps_cursor_visible_for_long_unicode_hosts() {
        let value = "éabcdefghijk";
        let (shown, cursor) = input_view(value, value.len(), 5);
        assert_eq!(shown, "hijk");
        assert_eq!(cursor, 4);
        let (shown, cursor) = input_view(value, "éabc".len(), 5);
        assert_eq!(shown, "éabcd");
        assert_eq!(cursor, 4);
    }

    #[test]
    fn agent_table_includes_types_that_are_unavailable_locally() {
        let mut state = idle_agent_state("/remote".into());
        state.listing.as_mut().unwrap().sessions[0]
            .agents
            .push(AgentListing {
                id: "other-pane".into(),
                name: "other-worker".into(),
                agent_type: "claude".into(),
                session_id: Some("other-native".into()),
                directory: "/remote".into(),
                git_repo_root: None,
                git_worktree_branch: None,
                git_worktree_base_ref: None,
                status: TransferStatus::Idle,
            });
        state.stage = ImportStage::Agent;
        state.agent_idx = 1;
        assert_eq!(state.visible_agents().len(), 2);
        assert_eq!(
            state.choose_agent(),
            Err("agent type is not enabled locally".into())
        );
        let text = buffer_text(&render_buffer(&state, 100, 32));
        assert!(text.contains("other-worker"));
        assert!(text.contains("claude"));
    }

    #[test]
    fn session_table_shows_separate_name_count_and_status_columns() {
        let mut state = idle_agent_state("/remote".into());
        state.stage = ImportStage::Session;
        let text = buffer_text(&render_buffer(&state, 100, 32));
        assert!(text.contains("Import remote agent ▸ Select session"));
        assert!(!text.lines().any(|line| line.trim() == "Sessions"));
        assert!(text.lines().any(|line| {
            line.contains("Session") && line.contains("Agents") && line.contains("Status")
        }));
        assert!(text.contains("remote"));
        assert!(text.contains("online"));
        assert!(!text.contains("(1 agents)"));
    }

    #[test]
    fn final_dialog_shows_remote_and_editable_local_details_with_tip_above_buttons() {
        let directory = std::env::current_dir()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let mut state = idle_agent_state(directory.clone());
        state.choose_agent().unwrap();
        assert_eq!(state.local_name.value, "worker");
        let text = buffer_text(&render_buffer(&state, 100, 32));
        let lines: Vec<_> = text.lines().collect();
        let remote = lines
            .iter()
            .position(|line| line.contains("Remote agent"))
            .unwrap();
        let local = lines
            .iter()
            .position(|line| line.contains("Local agent"))
            .unwrap();
        let tip = lines
            .iter()
            .position(|line| line.contains("synchronized project files"))
            .unwrap();
        let buttons = lines
            .iter()
            .position(|line| line.contains("Import  enter"))
            .unwrap();
        assert!(lines[remote + 1].contains("Name") && lines[remote + 1].contains("worker"));
        assert!(lines[remote + 2].contains("Directory"));
        assert!(lines[local + 1].contains("Name") && lines[local + 1].contains("worker"));
        assert!(lines[local + 2].contains("Directory"));
        assert_eq!(tip + 2, buttons);
    }
}

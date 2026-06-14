use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};

use crate::app::TerminalViewState;
use crate::host_terminal::HostColors;
use crate::models::{AgentEntry, AgentStatusCounts};
use crate::ui::theme::*;

pub fn render_terminal_view(
    f: &mut Frame,
    area: Rect,
    state: &TerminalViewState,
    agent_entry: &AgentEntry,
    status_counts: AgentStatusCounts,
    host_colors: HostColors,
    blink_running: bool,
    blink_waiting: bool,
) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(area);

    let top_area = chunks[0];
    let content_area = chunks[1];
    let status_area = chunks[2];

    let visible_text = state.lines.join("\r\n");

    crate::ghostty::render::render_pane_content(
        visible_text.as_bytes(),
        f,
        content_area,
        state.cursor,
        host_colors.fg,
        host_colors.bg,
    );

    let dir_str = super::dashboard::shellify_dir(&agent_entry.config.directory);
    let dir_display = if let Some(branch) =
        crate::git::current_branch(std::path::Path::new(&agent_entry.config.directory))
    {
        format!("{}:{}", dir_str, branch)
    } else {
        dir_str
    };

    let top_spans = vec![
        Span::raw(" "),
        Span::styled(
            agent_entry.config.name.to_string(),
            Style::default().fg(FG).add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled("terminal", Style::default().fg(GRAY)),
        Span::raw(" "),
        Span::styled(format!("{} ", ICON_DIR), Style::default().fg(GRAY)),
        Span::styled(dir_display.as_str(), Style::default().fg(GRAY)),
    ];
    f.render_widget(Paragraph::new(Line::from(top_spans)), top_area);

    let ctrlt_key = " ctrl+t ";
    let ctrlg_key = " ctrl+g ";
    let ctrlb_key = " ctrl+b ";
    let nav_width = (ctrlt_key.len()
        + " agent".len()
        + 1
        + ctrlg_key.len()
        + " dashboard".len()
        + 1
        + ctrlb_key.len()
        + " prefix".len()) as u16;
    let nav_spans: Vec<Span> = vec![
        Span::styled(
            ctrlt_key,
            Style::default().fg(FG).bg(BG2).add_modifier(Modifier::BOLD),
        ),
        Span::styled(" agent", Style::default().fg(FG)),
        Span::raw(" "),
        Span::styled(
            ctrlg_key,
            Style::default().fg(FG).bg(BG2).add_modifier(Modifier::BOLD),
        ),
        Span::styled(" dashboard", Style::default().fg(FG)),
        Span::raw(" "),
        Span::styled(
            ctrlb_key,
            Style::default().fg(FG).bg(BG2).add_modifier(Modifier::BOLD),
        ),
        Span::styled(" prefix", Style::default().fg(FG)),
    ];

    let (brand, brand_width) = brand_line(false);

    let (mut agent_status_spans, status_width) = status_count_spans(
        status_counts.running,
        status_counts.waiting,
        status_counts.idle,
        blink_running,
        blink_waiting,
        false,
    );
    agent_status_spans.push(Span::raw(" "));

    if state.prefix_active {
        let prefix_text = " PREFIX ";
        let prefix_width = prefix_text.len() as u16;
        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Length(nav_width),
                Constraint::Min(0),
                Constraint::Length(prefix_width),
                Constraint::Length(status_width),
                Constraint::Length(brand_width),
            ])
            .split(status_area);
        f.render_widget(Paragraph::new(Line::from(nav_spans)), chunks[0]);
        f.render_widget(
            Paragraph::new(Line::from(vec![Span::styled(
                prefix_text,
                Style::default()
                    .fg(ratatui::style::Color::Black)
                    .bg(YELLOW)
                    .add_modifier(Modifier::BOLD),
            )])),
            chunks[2],
        );
        f.render_widget(Paragraph::new(Line::from(agent_status_spans)), chunks[3]);
        f.render_widget(Paragraph::new(brand), chunks[4]);
    } else {
        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Length(nav_width),
                Constraint::Min(0),
                Constraint::Length(status_width),
                Constraint::Length(brand_width),
            ])
            .split(status_area);
        f.render_widget(Paragraph::new(Line::from(nav_spans)), chunks[0]);
        f.render_widget(Paragraph::new(Line::from(agent_status_spans)), chunks[2]);
        f.render_widget(Paragraph::new(brand), chunks[3]);
    }
}

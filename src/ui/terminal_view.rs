use ansi_to_tui::IntoText;
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Paragraph},
    Frame,
};

use crate::app::TerminalViewState;
use crate::models::{AgentEntry, AgentStatus};
use crate::ui::theme::*;

pub fn render_terminal_view(
    f: &mut Frame,
    area: Rect,
    state: &TerminalViewState,
    agent_entry: &AgentEntry,
    agents: &[AgentEntry],
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

    let visible_text = state.lines.join("\n");

    let base_style = extract_first_bg_color(visible_text.as_bytes())
        .map_or(Style::default(), |c| Style::default().bg(c));
    let text = visible_text
        .as_bytes()
        .into_text_with_style(base_style)
        .unwrap_or_else(|_| ratatui::text::Text::raw(visible_text.clone()));
    let para = Paragraph::new(text).style(base_style).block(
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Plain)
            .border_style(Style::default().fg(GRAY)),
    );
    f.render_widget(para, content_area);

    if let Some((cx, cy)) = state.cursor {
        let screen_x = content_area.x.saturating_add(cx + 1);
        let screen_y = content_area.y.saturating_add(cy + 1);
        if screen_x < content_area.x + content_area.width - 1
            && screen_y < content_area.y + content_area.height - 1
        {
            f.set_cursor_position((screen_x, screen_y));
        }
    }

    let dir_str = super::dashboard::shellify_dir(&agent_entry.config.directory);
    let dir_display = if let Some(branch) =
        crate::git::current_branch(std::path::Path::new(&agent_entry.config.directory))
    {
        format!("{}:{}", dir_str, branch)
    } else {
        dir_str
    };

    let running = agents
        .iter()
        .filter(|a| matches!(a.meta.status, AgentStatus::Running))
        .count();
    let waiting = agents
        .iter()
        .filter(|a| matches!(a.meta.status, AgentStatus::WaitingForInput))
        .count();
    let idle = agents
        .iter()
        .filter(|a| matches!(a.meta.status, AgentStatus::Idle))
        .count();

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

    let agent_status_spans: Vec<Span> = vec![
        Span::styled(
            format!(" {} {} running", ICON_RUN, running),
            Style::default().fg(GREEN),
        ),
        Span::styled(
            format!(" {} {} waiting", ICON_WAIT, waiting),
            Style::default().fg(YELLOW),
        ),
        Span::styled(
            format!(" {} {} idle", ICON_IDLE, idle),
            Style::default().fg(CYAN),
        ),
        Span::raw(" "),
    ];
    let status_width = format!(
        " {} {} running {} {} waiting {} {} idle ",
        ICON_RUN, running, ICON_WAIT, waiting, ICON_IDLE, idle
    )
    .chars()
    .count() as u16;

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

fn extract_first_bg_color(ansi: &[u8]) -> Option<ratatui::style::Color> {
    use ratatui::style::Color;
    let mut i = 0;
    while i < ansi.len() {
        if ansi[i] != 0x1b {
            i += 1;
            continue;
        }
        i += 1;
        if i >= ansi.len() || ansi[i] != b'[' {
            continue;
        }
        i += 1;
        let start = i;
        while i < ansi.len() && ansi[i] != b'm' && ansi[i] != 0x1b {
            i += 1;
        }
        if i >= ansi.len() || ansi[i] != b'm' {
            continue;
        }
        let params_bytes = &ansi[start..i];
        i += 1;

        let Ok(params_str) = std::str::from_utf8(params_bytes) else {
            continue;
        };
        let nums: Vec<u32> = params_str
            .split(';')
            .filter_map(|s| s.parse().ok())
            .collect();

        let mut j = 0;
        while j < nums.len() {
            match nums[j] {
                48 if j + 4 < nums.len() && nums[j + 1] == 2 => {
                    return Some(Color::Rgb(
                        nums[j + 2] as u8,
                        nums[j + 3] as u8,
                        nums[j + 4] as u8,
                    ));
                }
                48 if j + 2 < nums.len() && nums[j + 1] == 5 => {
                    return Some(Color::Indexed(nums[j + 2] as u8));
                }
                n @ 40..=47 => return Some(Color::Indexed((n - 40) as u8)),
                n @ 100..=107 => return Some(Color::Indexed((n - 100 + 8) as u8)),
                _ => {}
            }
            j += 1;
        }
    }
    None
}

use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Clear, Paragraph},
    Frame,
};

use crate::app::AgentViewState;
use crate::models::{AgentEntry, AgentStatus};
use crate::terminal_theme::TerminalTheme;
use crate::ui::theme::*;

pub fn render_agent_view(
    f: &mut Frame,
    area: Rect,
    state: &AgentViewState,
    agent_entry: &AgentEntry,
    agents: &[AgentEntry],
    host_theme: TerminalTheme,
) {
    // Split into top info bar, content area, and bottom status bar
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

    let viewport_height = content_area.height.saturating_sub(2) as usize;

    // Show the appropriate window of lines based on view_scroll.
    // view_scroll == 0: live view (last viewport_height lines).
    // view_scroll > 0: history view (a window offset from the end).
    //
    // Clamp effective_scroll here (read-only, no state write) so the renderer
    // never produces an empty frame when view_scroll overshoots the buffer.
    // This avoids a flicker cycle that would occur if we mutated state and
    // triggered a dirty→redraw round-trip.
    let lines = &state.lines;
    let total = lines.len();
    let max_scroll = total.saturating_sub(viewport_height);
    let effective_scroll = state.view_scroll.min(max_scroll);
    let (start, end) = if total == 0 {
        (0, 0)
    } else {
        let end = total.saturating_sub(effective_scroll);
        let start = end.saturating_sub(viewport_height);
        (start, end)
    };
    let visible_text = if total == 0 {
        String::new()
    } else {
        lines[start..end].join("\r\n")
    };

    let cursor_position = if !state.show_stopped_overlay && state.view_scroll == 0 {
        state.cursor
    } else {
        None
    };
    crate::ghostty::render::render_pane_content(
        visible_text.as_bytes(),
        f,
        content_area,
        cursor_position,
        host_theme,
    );

    // Status bar
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

    let ctx_text = if let Some(ctx) = &agent_entry.meta.context {
        let used = format_tokens(ctx.used);
        if let Some(total) = ctx.total {
            format!("{}/{}", used, format_tokens(total))
        } else {
            used
        }
    } else {
        "∞/∞".to_string()
    };

    let work_str = if agent_entry.meta.total_work_ms > 0 {
        format_uptime(agent_entry.meta.total_work_ms)
    } else {
        "< 1s".to_string()
    };

    // --- Top bar: agent meta info ---
    let mut top_spans = vec![
        Span::raw(" "),
        Span::styled(
            format!("{}", agent_entry.config.name),
            Style::default().fg(FG).add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled(ctx_text, Style::default().fg(GRAY)),
        Span::styled(
            format!(" {} {}", ICON_TIME, work_str),
            Style::default().fg(GRAY),
        ),
        Span::raw(" "),
        Span::styled(format!("{} ", ICON_DIR), Style::default().fg(GRAY)),
        Span::styled(dir_display.as_str(), Style::default().fg(GRAY)),
        Span::styled(format!(" {} ", ICON_AGENT), Style::default().fg(GRAY)),
        Span::styled(
            agent_entry.config.agent_type_str(),
            Style::default().fg(GRAY),
        ),
    ];
    if let Some(model_str) = agent_entry.meta.model_name.as_deref() {
        top_spans.push(Span::styled(
            format!(" {} {}", ICON_MODEL, model_str),
            Style::default().fg(GRAY),
        ));
    }
    f.render_widget(Paragraph::new(Line::from(top_spans)), top_area);

    // --- Left: hotkey hints (ctrl+g dashboard, ctrl+v git, ctrl+b prefix) ---
    let ctrlg_key = " ctrl+g ";
    let ctrlb_key = " ctrl+b ";
    let ctrlv_key = " ctrl+v ";
    let nav_width = (ctrlg_key.len()
        + " dashboard".len()
        + 1
        + ctrlv_key.len()
        + " git".len()
        + 1
        + ctrlb_key.len()
        + " prefix".len()) as u16;
    let nav_spans: Vec<Span> = vec![
        Span::styled(
            ctrlg_key,
            Style::default().fg(FG).bg(BG2).add_modifier(Modifier::BOLD),
        ),
        Span::styled(" dashboard", Style::default().fg(FG)),
        Span::raw(" "),
        Span::styled(
            ctrlv_key,
            Style::default().fg(FG).bg(BG2).add_modifier(Modifier::BOLD),
        ),
        Span::styled(" git", Style::default().fg(FG)),
        Span::raw(" "),
        Span::styled(
            ctrlb_key,
            Style::default().fg(FG).bg(BG2).add_modifier(Modifier::BOLD),
        ),
        Span::styled(" prefix", Style::default().fg(FG)),
    ];

    // --- Right: PREFIX badge (conditional) + agent statuses + brand ---
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

    // Stopped overlay
    if state.show_stopped_overlay {
        let has_worktree = agent_entry.config.git_repo_root.is_some();
        render_stopped_overlay(f, area, has_worktree, state.remove_worktree_on_stop);
    }
}

fn render_stopped_overlay(f: &mut Frame, area: Rect, has_worktree: bool, remove_worktree: bool) {
    let overlay_width = ((area.width as u32 * 40 / 100) as u16)
        .max(44)
        .min(area.width);
    let worktree_rows: u16 = if has_worktree { 2 } else { 0 }; // blank + checkbox
    let overlay_height = (7u16 + worktree_rows).min(area.height);
    let x = area.x + area.width.saturating_sub(overlay_width) / 2;
    let y = area.y + area.height.saturating_sub(overlay_height) / 2;
    let overlay_area = Rect::new(x, y, overlay_width, overlay_height);

    f.render_widget(Clear, overlay_area);
    f.render_widget(
        Block::default().style(Style::default().bg(BG1)),
        overlay_area,
    );

    let mut constraints = vec![
        Constraint::Length(1), // blank
        Constraint::Length(1), // title
        Constraint::Length(1), // blank
        Constraint::Length(1), // message
    ];
    if has_worktree {
        constraints.push(Constraint::Length(1)); // blank
        constraints.push(Constraint::Length(1)); // worktree checkbox
    }
    constraints.push(Constraint::Length(1)); // blank
    constraints.push(Constraint::Length(1)); // buttons
    constraints.push(Constraint::Length(1)); // blank (trailing)

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(overlay_area);

    let mut row = 0usize;

    // blank
    row += 1;

    // Title
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::raw("   "),
            Span::styled(
                "Agent stopped",
                Style::default().fg(RED).add_modifier(Modifier::BOLD),
            ),
        ]))
        .style(Style::default().bg(BG1)),
        rows[row],
    );
    row += 1;

    // blank
    row += 1;

    // Message
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::raw("   "),
            Span::styled("The agent process has exited.", Style::default().fg(GRAY)),
        ]))
        .style(Style::default().bg(BG1)),
        rows[row],
    );
    row += 1;

    // Worktree checkbox
    if has_worktree {
        // blank
        row += 1;

        let checkbox = if remove_worktree { "[x]" } else { "[ ]" };
        let checkbox_style = if remove_worktree {
            Style::default().fg(ORANGE).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(GRAY)
        };
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::raw("   "),
                Span::styled(checkbox, checkbox_style),
                Span::styled(" Remove git worktree", Style::default().fg(FG)),
                Span::styled("  space", Style::default().fg(GRAY)),
            ]))
            .style(Style::default().bg(BG1)),
            rows[row],
        );
        row += 1;
    }

    // blank
    row += 1;

    // Buttons
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::raw("   "),
            Span::styled(
                " Restart ",
                Style::default()
                    .bg(ORANGE)
                    .fg(FG)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" r", Style::default().fg(GRAY)),
            Span::raw("   "),
            Span::styled(
                " Remove ",
                Style::default().bg(RED).fg(FG).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" d", Style::default().fg(GRAY)),
            Span::raw("   "),
            Span::styled(
                " Dashboard ",
                Style::default().bg(BG2).fg(FG).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" ctrl-g", Style::default().fg(GRAY)),
        ]))
        .style(Style::default().bg(BG1)),
        rows[row],
    );
}

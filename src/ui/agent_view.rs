use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Clear, Paragraph},
};

use crate::app::AgentViewState;
use crate::host_terminal::HostColors;
use crate::models::{AgentEntry, AgentStatusCounts};
use crate::ui::theme::{
    ICON_AGENT, ICON_CTX, ICON_DIR, ICON_MODEL, ICON_TIME, Theme, brand_line, format_tokens,
    format_uptime, status_count_spans,
};

#[allow(clippy::too_many_arguments)]
pub fn render_agent_view(
    f: &mut Frame,
    area: Rect,
    theme: &Theme,
    state: &AgentViewState,
    agent_entry: &AgentEntry,
    status_counts: AgentStatusCounts,
    host_colors: HostColors,
    blink_running: bool,
    blink_waiting: bool,
    copy_notice: Option<(&str, Color)>,
    selection: Option<crate::ghostty::render::SelectionRange>,
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

    f.render_widget(
        Block::default().style(Style::default().bg(theme.bg)),
        top_area,
    );
    f.render_widget(
        Block::default().style(Style::default().bg(theme.bg)),
        status_area,
    );

    let viewport_height = content_area.height.saturating_sub(2) as usize;

    // Show the appropriate window of lines based on view_scroll.
    // view_scroll == 0: live view (last viewport_height lines).
    // view_scroll > 0: history view (a window offset from the end).
    //
    // Clamp effective_scroll here (read-only, no state write) so the renderer
    // never produces an empty frame when view_scroll overshoots the buffer.
    // This avoids a flicker cycle that would occur if we mutated state and
    // triggered a dirty→redraw round-trip.
    let visible_text =
        crate::app::pane_visible_text(&state.lines, state.view_scroll, viewport_height);

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
        host_colors.fg,
        host_colors.bg,
        selection,
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

    let work_str = format_uptime(agent_entry.meta.total_work_ms);

    // --- Top bar: agent meta info (3-zone layout) ---
    let name_text = format!(
        " {} @ {} ",
        agent_entry.config.name, agent_entry.config.project
    );
    let name_width = unicode_width::UnicodeWidthStr::width(name_text.as_str()) as u16;

    let dir_prefix = format!(" {} ", ICON_DIR);
    let dir_prefix_width = unicode_width::UnicodeWidthStr::width(dir_prefix.as_str()) as u16;

    let agent_type_text = format!(" {} {}", ICON_AGENT, agent_entry.config.agent_type_str());
    let model_text = agent_entry
        .meta
        .model_name
        .as_deref()
        .map(|m| format!(" {} {}", ICON_MODEL, m))
        .unwrap_or_default();
    let ctx_full_text = format!(" {} {}", ICON_CTX, ctx_text);
    let time_text = format!(" {} {}", ICON_TIME, work_str);

    let right_parts: Vec<&str> = vec![
        agent_type_text.as_str(),
        model_text.as_str(),
        ctx_full_text.as_str(),
        time_text.as_str(),
    ];
    let right_combined = right_parts.join(" ");
    let right_text = format!(" {} ", right_combined);
    let right_width = unicode_width::UnicodeWidthStr::width(right_text.as_str()) as u16;

    let layout = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(name_width),
            Constraint::Length(dir_prefix_width),
            Constraint::Min(0),
            Constraint::Length(right_width),
        ])
        .split(top_area);

    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::raw(" "),
            Span::styled(
                agent_entry.config.name.as_str(),
                Style::default().fg(theme.fg).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" @ ", Style::default().fg(theme.fg)),
            Span::styled(
                agent_entry.config.project.as_str(),
                Style::default().fg(theme.fg).add_modifier(Modifier::BOLD),
            ),
            Span::raw(" "),
        ])),
        layout[0],
    );

    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            &dir_prefix,
            Style::default().fg(theme.gray),
        ))),
        layout[1],
    );

    let middle_width = layout[2].width as usize;
    let dir_line_width = unicode_width::UnicodeWidthStr::width(dir_display.as_str());
    let scroll_offset = dir_line_width.saturating_sub(middle_width) as u16;
    let is_truncated = scroll_offset > 0;
    let middle_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(if is_truncated { 1 } else { 0 }),
            Constraint::Min(0),
        ])
        .split(layout[2]);
    if is_truncated {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "…",
                Style::default().fg(theme.gray),
            ))),
            middle_chunks[0],
        );
    }
    let text_width = middle_chunks[1].width as usize;
    let text_scroll = dir_line_width.saturating_sub(text_width) as u16;
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            &dir_display,
            Style::default().fg(theme.gray),
        )))
        .scroll((0, text_scroll)),
        middle_chunks[1],
    );

    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            &right_text,
            Style::default().fg(theme.gray),
        ))),
        layout[3],
    );

    // --- Left: hotkey hints (ctrl+g dashboard, [ctrl+v git], ctrl+t terminal, ctrl+q running, ctrl+o waiting, ctrl+p idle, ctrl+b prefix) ---
    let is_git =
        crate::git::find_git_root(std::path::Path::new(&agent_entry.config.directory)).is_some();
    let ctrlg_key = " ctrl+g ";
    let ctrlb_key = " ctrl+b ";
    let ctrlv_key = " ctrl+v ";
    let ctrlt_key = " ctrl+t ";
    let ctrl_running_key = " ctrl+q ";
    let ctrl_waiting_key = " ctrl+o ";
    let ctrl_idle_key = " ctrl+p ";
    let nav_width = if is_git {
        (ctrlg_key.len()
            + " dashboard".len()
            + 1
            + ctrlv_key.len()
            + " git".len()
            + 1
            + ctrlt_key.len()
            + " terminal".len()
            + 1
            + ctrl_running_key.len()
            + " next running".len()
            + 1
            + ctrl_waiting_key.len()
            + " next waiting".len()
            + 1
            + ctrl_idle_key.len()
            + " next idle".len()
            + 1
            + ctrlb_key.len()
            + " prefix".len()) as u16
    } else {
        (ctrlg_key.len()
            + " dashboard".len()
            + 1
            + ctrlt_key.len()
            + " terminal".len()
            + 1
            + ctrl_running_key.len()
            + " next running".len()
            + 1
            + ctrl_waiting_key.len()
            + " next waiting".len()
            + 1
            + ctrl_idle_key.len()
            + " next idle".len()
            + 1
            + ctrlb_key.len()
            + " prefix".len()) as u16
    };
    let mut nav_spans: Vec<Span> = vec![
        Span::styled(
            ctrlg_key,
            Style::default()
                .fg(theme.fg)
                .bg(theme.bg2)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" dashboard", Style::default().fg(theme.fg)),
    ];
    if is_git {
        nav_spans.push(Span::raw(" "));
        nav_spans.push(Span::styled(
            ctrlv_key,
            Style::default()
                .fg(theme.fg)
                .bg(theme.bg2)
                .add_modifier(Modifier::BOLD),
        ));
        nav_spans.push(Span::styled(" git", Style::default().fg(theme.fg)));
    }
    nav_spans.push(Span::raw(" "));
    nav_spans.push(Span::styled(
        ctrlt_key,
        Style::default()
            .fg(theme.fg)
            .bg(theme.bg2)
            .add_modifier(Modifier::BOLD),
    ));
    nav_spans.push(Span::styled(" terminal", Style::default().fg(theme.fg)));
    nav_spans.push(Span::raw(" "));
    nav_spans.push(Span::styled(
        ctrl_running_key,
        Style::default()
            .fg(theme.fg)
            .bg(theme.bg2)
            .add_modifier(Modifier::BOLD),
    ));
    nav_spans.push(Span::styled(" next running", Style::default().fg(theme.fg)));
    nav_spans.push(Span::raw(" "));
    nav_spans.push(Span::styled(
        ctrl_waiting_key,
        Style::default()
            .fg(theme.fg)
            .bg(theme.bg2)
            .add_modifier(Modifier::BOLD),
    ));
    nav_spans.push(Span::styled(" next waiting", Style::default().fg(theme.fg)));
    nav_spans.push(Span::raw(" "));
    nav_spans.push(Span::styled(
        ctrl_idle_key,
        Style::default()
            .fg(theme.fg)
            .bg(theme.bg2)
            .add_modifier(Modifier::BOLD),
    ));
    nav_spans.push(Span::styled(" next idle", Style::default().fg(theme.fg)));
    nav_spans.push(Span::raw(" "));
    nav_spans.push(Span::styled(
        ctrlb_key,
        Style::default()
            .fg(theme.fg)
            .bg(theme.bg2)
            .add_modifier(Modifier::BOLD),
    ));
    nav_spans.push(Span::styled(" prefix", Style::default().fg(theme.fg)));

    // --- Right: PREFIX badge (conditional) + agent statuses + brand ---
    let (brand, brand_width) = brand_line(theme, false);

    let (mut agent_status_spans, status_width) = status_count_spans(
        theme,
        status_counts.running,
        status_counts.waiting,
        status_counts.idle,
        blink_running,
        blink_waiting,
        false,
    );
    agent_status_spans.push(Span::raw(" "));

    let copy_notice_width = copy_notice
        .as_ref()
        .map(|(text, _)| text.len() as u16)
        .unwrap_or(0);

    if state.prefix_active {
        let prefix_text = " PREFIX ";
        let prefix_width = prefix_text.len() as u16;
        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Length(nav_width),
                Constraint::Min(0),
                Constraint::Length(prefix_width),
                Constraint::Length(copy_notice_width),
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
                    .bg(theme.yellow)
                    .add_modifier(Modifier::BOLD),
            )])),
            chunks[2],
        );
        if let Some((text, color)) = copy_notice {
            f.render_widget(
                Paragraph::new(Line::from(vec![Span::styled(
                    text,
                    Style::default()
                        .fg(ratatui::style::Color::Black)
                        .bg(color)
                        .add_modifier(Modifier::BOLD),
                )])),
                chunks[3],
            );
        }
        f.render_widget(Paragraph::new(Line::from(agent_status_spans)), chunks[4]);
        f.render_widget(Paragraph::new(brand), chunks[5]);
    } else {
        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Length(nav_width),
                Constraint::Min(0),
                Constraint::Length(copy_notice_width),
                Constraint::Length(status_width),
                Constraint::Length(brand_width),
            ])
            .split(status_area);
        f.render_widget(Paragraph::new(Line::from(nav_spans)), chunks[0]);
        if let Some((text, color)) = copy_notice {
            f.render_widget(
                Paragraph::new(Line::from(vec![Span::styled(
                    text,
                    Style::default()
                        .fg(ratatui::style::Color::Black)
                        .bg(color)
                        .add_modifier(Modifier::BOLD),
                )])),
                chunks[2],
            );
        }
        f.render_widget(Paragraph::new(Line::from(agent_status_spans)), chunks[3]);
        f.render_widget(Paragraph::new(brand), chunks[4]);
    }

    // Stopped overlay
    if state.show_stopped_overlay {
        let has_worktree = agent_entry.config.git_repo_root.is_some();
        render_stopped_overlay(theme, f, area, has_worktree, state.remove_worktree_on_stop);
    }
}

fn render_stopped_overlay(
    theme: &Theme,
    f: &mut Frame,
    area: Rect,
    has_worktree: bool,
    remove_worktree: bool,
) {
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
        Block::default().style(Style::default().bg(theme.bg1)),
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
                Style::default().fg(theme.red).add_modifier(Modifier::BOLD),
            ),
        ]))
        .style(Style::default().bg(theme.bg1)),
        rows[row],
    );
    row += 1;

    // blank
    row += 1;

    // Message
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::raw("   "),
            Span::styled(
                "The agent process has exited.",
                Style::default().fg(theme.gray),
            ),
        ]))
        .style(Style::default().bg(theme.bg1)),
        rows[row],
    );
    row += 1;

    // Worktree checkbox
    if has_worktree {
        // blank
        row += 1;

        let checkbox = if remove_worktree { "[x]" } else { "[ ]" };
        let checkbox_style = if remove_worktree {
            Style::default()
                .fg(theme.orange)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.gray)
        };
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::raw("   "),
                Span::styled(checkbox, checkbox_style),
                Span::styled(" Remove git worktree", Style::default().fg(theme.fg)),
                Span::styled("  space", Style::default().fg(theme.gray)),
            ]))
            .style(Style::default().bg(theme.bg1)),
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
                    .bg(theme.orange)
                    .fg(theme.fg)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" r", Style::default().fg(theme.gray)),
            Span::raw("   "),
            Span::styled(
                " Remove ",
                Style::default()
                    .bg(theme.red)
                    .fg(theme.fg)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" d", Style::default().fg(theme.gray)),
            Span::raw("   "),
            Span::styled(
                " Dashboard ",
                Style::default()
                    .bg(theme.bg2)
                    .fg(theme.fg)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" ctrl-g", Style::default().fg(theme.gray)),
        ]))
        .style(Style::default().bg(theme.bg1)),
        rows[row],
    );
}

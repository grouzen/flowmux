use ansi_to_tui::IntoText;
use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
    Frame,
};
use std::collections::HashMap;

use crate::app::AgentViewState;
use crate::models::{AgentEntry, AgentStatus};
use crate::ui::theme::*;

pub fn render_agent_view(
    f: &mut Frame,
    area: Rect,
    state: &AgentViewState,
    agent_entry: &AgentEntry,
    agents: &[AgentEntry],
) {
    // Split into content area and status bar (last row)
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(1)])
        .split(area);

    let content_area = chunks[0];
    let status_area = chunks[1];

    let viewport_height = content_area.height as usize;

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
        lines[start..end].join("\n")
    };

    // Parse ANSI escape sequences into styled ratatui Text
    let text = visible_text
        .as_bytes()
        .into_text()
        .unwrap_or_else(|_| ratatui::text::Text::raw(visible_text.clone()));

    // Use the most frequently occurring background colour in the parsed ANSI
    // content as the base style for the paragraph.  This lets cells without an
    // explicit background (e.g. spaces around a modal overlay) inherit the
    // pane's own background rather than stable's terminal default, while
    // avoiding transient per-character highlights (e.g. vim's MatchParen on
    // bracket characters) from hijacking the whole-pane background.
    let base_bg = {
        let mut freq: HashMap<ratatui::style::Color, usize> = HashMap::new();
        for span in text.lines.iter().flat_map(|l| l.spans.iter()) {
            if let Some(bg) = span.style.bg {
                *freq.entry(bg).or_insert(0) += span.content.len();
            }
        }
        freq.into_iter()
            .max_by_key(|(_, count)| *count)
            .map(|(color, _)| color)
    };
    let base_style = match base_bg {
        Some(bg) => Style::default().bg(bg),
        None => Style::default(),
    };
    let para = Paragraph::new(text).style(base_style);
    f.render_widget(para, content_area);

    // Forward the pane cursor only when showing live content (not scrolled back).
    if !state.show_stopped_overlay && state.view_scroll == 0 {
        if let Some((cx, cy)) = state.cursor {
            let screen_x = content_area.x.saturating_add(cx);
            let screen_y = content_area.y.saturating_add(cy);
            if screen_x < content_area.x + content_area.width
                && screen_y < content_area.y + content_area.height
            {
                f.set_cursor_position((screen_x, screen_y));
            }
        }
    }

    // Status bar
    let dir_str = &agent_entry.config.directory;

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

    let sep = Span::styled(" │ ", Style::default().fg(BG2));

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

    let mut status_spans = vec![
        Span::styled(
            format!(" {}", agent_entry.config.name),
            Style::default().fg(FG).add_modifier(Modifier::BOLD),
        ),
        sep.clone(),
        Span::styled(ctx_text, Style::default().fg(GRAY)),
        Span::styled(
            format!(" {} {}", ICON_TIME, work_str),
            Style::default().fg(GRAY),
        ),
        sep.clone(),
        Span::styled(format!("{} ", ICON_DIR), Style::default().fg(GRAY)),
        Span::styled(dir_str.as_str(), Style::default().fg(GRAY)),
        Span::styled(format!(" {} ", ICON_AGENT), Style::default().fg(GRAY)),
        Span::styled(
            agent_entry.config.agent_type_str(),
            Style::default().fg(GRAY),
        ),
    ];
    if let Some(model_str) = agent_entry.meta.model_name.as_deref() {
        status_spans.push(Span::styled(
            format!(" {} {}", ICON_MODEL, model_str),
            Style::default().fg(GRAY),
        ));
    }
    status_spans.push(sep.clone());
    status_spans.push(Span::styled(
        format!("{} {} running", ICON_RUN, running),
        Style::default().fg(GREEN),
    ));
    status_spans.push(Span::styled(
        format!(" {} {} waiting", ICON_WAIT, waiting),
        Style::default().fg(YELLOW),
    ));
    status_spans.push(Span::styled(
        format!(" {} {} idle", ICON_IDLE, idle),
        Style::default().fg(CYAN),
    ));
    let status_line = Line::from(status_spans);

    let (brand, brand_width) = brand_line(false);

    // When prefix mode is active, replace the nav hint with a prominent
    // [PREFIX] badge so the user knows the next key will be forwarded.
    let (nav_spans, nav_width): (Vec<Span>, u16) = if state.prefix_active {
        let text = " [PREFIX] ";
        (
            vec![Span::styled(
                text,
                Style::default()
                    .fg(ratatui::style::Color::Black)
                    .bg(YELLOW)
                    .add_modifier(Modifier::BOLD),
            )],
            text.len() as u16,
        )
    } else {
        let text = " [Ctrl+g] Dashboard";
        (
            vec![
                Span::styled(" [", Style::default().fg(BG2)),
                Span::styled("Ctrl+g", Style::default().fg(ORANGE)),
                Span::styled("]", Style::default().fg(BG2)),
                Span::styled(" Dashboard", Style::default().fg(GRAY)),
            ],
            text.len() as u16,
        )
    };

    let status_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Min(0),
            Constraint::Length(nav_width),
            Constraint::Length(brand_width),
        ])
        .split(status_area);

    f.render_widget(Paragraph::new(status_line), status_chunks[0]);
    f.render_widget(Paragraph::new(Line::from(nav_spans)), status_chunks[1]);
    f.render_widget(Paragraph::new(brand), status_chunks[2]);

    // Stopped overlay
    if state.show_stopped_overlay {
        render_stopped_overlay(f, area);
    }
}

fn render_stopped_overlay(f: &mut Frame, area: Rect) {
    // Compute a centered box: 58 wide, 7 tall
    let overlay_width = 58u16.min(area.width);
    let overlay_height = 7u16.min(area.height);
    let x = area.x + area.width.saturating_sub(overlay_width) / 2;
    let y = area.y + area.height.saturating_sub(overlay_height) / 2;
    let overlay_area = Rect::new(x, y, overlay_width, overlay_height);

    f.render_widget(Clear, overlay_area);

    let block = Block::default()
        .title(Span::styled(
            " Agent Stopped ",
            Style::default().fg(RED).add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(RED));
    let inner = block.inner(overlay_area);
    f.render_widget(block, overlay_area);

    // Content lines inside the box
    let lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            "The agent process has exited.",
            Style::default().fg(GRAY),
        )),
        Line::from(""),
        Line::from(vec![
            Span::styled("[", Style::default().fg(BG2)),
            Span::styled("r", Style::default().fg(ORANGE)),
            Span::styled("]", Style::default().fg(BG2)),
            Span::styled(" Restart", Style::default().fg(FG)),
            Span::raw("   "),
            Span::styled("[", Style::default().fg(BG2)),
            Span::styled("d", Style::default().fg(ORANGE)),
            Span::styled("]", Style::default().fg(BG2)),
            Span::styled(" Remove", Style::default().fg(FG)),
            Span::raw("   "),
            Span::styled("[", Style::default().fg(BG2)),
            Span::styled("Ctrl-g", Style::default().fg(ORANGE)),
            Span::styled("]", Style::default().fg(BG2)),
            Span::styled(" Dashboard", Style::default().fg(FG)),
        ]),
    ];

    let para = Paragraph::new(lines).alignment(Alignment::Center);
    f.render_widget(para, inner);
}

use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Padding, Paragraph},
    Frame,
};

use crate::models::{AgentEntry, AgentStatus};
use crate::ui::theme::*;

// ---------------------------------------------------------------------------
// Status count helpers (used by keybindings bar)
// ---------------------------------------------------------------------------

fn count_running(agents: &[AgentEntry]) -> usize {
    agents
        .iter()
        .filter(|a| matches!(a.meta.status, AgentStatus::Running))
        .count()
}

fn count_waiting(agents: &[AgentEntry]) -> usize {
    agents
        .iter()
        .filter(|a| matches!(a.meta.status, AgentStatus::WaitingForInput))
        .count()
}

// ---------------------------------------------------------------------------
// Style helper
// ---------------------------------------------------------------------------

/// Returns a base `Style` with `DIM` applied when `dimmed` is true.
fn ds(dimmed: bool) -> Style {
    if dimmed {
        Style::default().add_modifier(Modifier::DIM)
    } else {
        Style::default()
    }
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

pub fn render_dashboard(
    f: &mut Frame,
    area: Rect,
    agents: &[AgentEntry],
    selected: usize,
    card_scroll: &[u16],
    card_response_heights: &mut Vec<u16>,
    card_response_widths: &mut Vec<u16>,
    dimmed: bool,
) {
    // Split into main area and keybindings bar at bottom
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(1)])
        .split(area);

    let main_area = chunks[0];
    let bar_area = chunks[1];

    render_keybindings_bar(f, bar_area, agents, dimmed);
    render_grid(
        f,
        main_area,
        agents,
        selected,
        card_scroll,
        card_response_heights,
        card_response_widths,
        dimmed,
    );
}

// ---------------------------------------------------------------------------
// Grid
// ---------------------------------------------------------------------------

/// Returns (cols, rows) for the grid based on the number of agents.
///
/// Layout progression:
///   0–2  agents → 2×1  (2 cols, 1 row)
///   3    agents → 3×1  (3 cols, 1 row)
///   4–6  agents → 3×2  (3 cols, 2 rows)
///   7–8  agents → 4×2  (4 cols, 2 rows)
///   9–12 agents → 4×3  (4 cols, 3 rows)
///  13–16 agents → 4×4  (4 cols, 4 rows)
pub fn grid_layout(n: usize) -> (usize, usize) {
    if n <= 2 {
        (2, 1)
    } else if n <= 3 {
        (3, 1)
    } else if n <= 6 {
        (3, 2)
    } else if n <= 8 {
        (4, 2)
    } else if n <= 12 {
        (4, 3)
    } else {
        (4, 4)
    }
}

fn render_grid(
    f: &mut Frame,
    area: Rect,
    agents: &[AgentEntry],
    selected: usize,
    card_scroll: &[u16],
    card_response_heights: &mut Vec<u16>,
    card_response_widths: &mut Vec<u16>,
    dimmed: bool,
) {
    if agents.is_empty() {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Ratio(1, 2),
                Constraint::Length(1),
                Constraint::Ratio(1, 2),
            ])
            .split(area);
        let msg = Paragraph::new("No agents. Press [n] to create one.")
            .style(ds(dimmed).fg(GRAY))
            .alignment(Alignment::Center);
        f.render_widget(msg, chunks[1]);
        return;
    }

    let (cols, rows) = grid_layout(agents.len());

    let col_constraints: Vec<Constraint> = (0..cols)
        .map(|_| Constraint::Ratio(1, cols as u32))
        .collect();
    let row_constraints: Vec<Constraint> = (0..rows)
        .map(|_| Constraint::Ratio(1, rows as u32))
        .collect();

    let row_areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints(row_constraints)
        .split(area);

    // Ensure vecs have room for all slots
    if card_response_heights.len() < agents.len() {
        card_response_heights.resize(agents.len(), 0);
    }
    if card_response_widths.len() < agents.len() {
        card_response_widths.resize(agents.len(), 0);
    }

    for row in 0..rows {
        let col_areas = Layout::default()
            .direction(Direction::Horizontal)
            .constraints(col_constraints.clone())
            .split(row_areas[row]);

        for col in 0..cols {
            let slot = row * cols + col;
            let cell_area = col_areas[col];

            if slot < agents.len() {
                let scroll = card_scroll.get(slot).copied().unwrap_or(0);
                let (resp_h, resp_w) = render_card(
                    f,
                    cell_area,
                    &agents[slot],
                    slot == selected,
                    scroll,
                    dimmed,
                );
                card_response_heights[slot] = resp_h;
                card_response_widths[slot] = resp_w;
            }
            // Empty slots render as blank (no border)
        }
    }
}

// ---------------------------------------------------------------------------
// Card
// ---------------------------------------------------------------------------

fn status_symbol(status: &AgentStatus) -> &'static str {
    match status {
        AgentStatus::Running => ICON_RUN,
        AgentStatus::WaitingForInput => ICON_WAIT,
        AgentStatus::Stopped => ICON_STOP,
        AgentStatus::Unknown => "?",
    }
}

fn status_label(status: &AgentStatus) -> &'static str {
    match status {
        AgentStatus::Running => "Running",
        AgentStatus::WaitingForInput => "Waiting",
        AgentStatus::Stopped => "Stopped",
        AgentStatus::Unknown => "Unknown",
    }
}

fn status_color(status: &AgentStatus) -> ratatui::style::Color {
    match status {
        AgentStatus::Running => GREEN,
        AgentStatus::WaitingForInput => YELLOW,
        AgentStatus::Stopped => RED,
        AgentStatus::Unknown => GRAY,
    }
}

/// Truncates a string to `max` chars (hard cut, no ellipsis).
fn truncate(s: &str, max: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max {
        s.to_string()
    } else {
        chars[..max].iter().collect()
    }
}

/// Returns the first newline-delimited line of a prompt string.
fn first_line(s: &str) -> &str {
    s.lines().next().unwrap_or("")
}

/// Replaces the home directory prefix with `~` for compact display.
fn shellify_dir(dir: &str) -> String {
    if let Ok(home) = std::env::var("HOME") {
        if let Some(rest) = dir.strip_prefix(&home) {
            return format!("~{}", rest);
        }
    }
    dir.to_string()
}

/// Formats a millisecond duration into a human-readable string
/// (e.g. "3h 12m", "45m", "< 1m").
fn format_uptime(ms: u64) -> String {
    let secs = ms / 1000;
    let hours = secs / 3600;
    let mins = (secs % 3600) / 60;
    if hours > 0 {
        format!("{}h {}m", hours, mins)
    } else if mins > 0 {
        format!("{}m", mins)
    } else if secs > 0 {
        format!("{}s", secs)
    } else {
        "< 1s".to_string()
    }
}

fn render_card(
    f: &mut Frame,
    area: Rect,
    entry: &AgentEntry,
    is_selected: bool,
    response_scroll: u16,
    dimmed: bool,
) -> (u16, u16) {
    let (border_color, title_color) = if is_selected { (BLUE, BLUE) } else { (BG2, FG) };

    let border_style = if is_selected {
        ds(dimmed).fg(border_color).add_modifier(Modifier::BOLD)
    } else {
        ds(dimmed).fg(border_color)
    };

    let title_style = ds(dimmed).fg(title_color).add_modifier(Modifier::BOLD);

    let block = Block::default()
        .title(Span::styled(
            format!(" {} ", entry.config.name),
            title_style,
        ))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(border_style);

    // Apply 1-cell left/right inner padding
    let raw_inner = block.inner(area);
    f.render_widget(block, area);

    if raw_inner.height == 0 || raw_inner.width < 2 {
        return (0, 0);
    }
    let inner = Rect {
        x: raw_inner.x + 1,
        y: raw_inner.y,
        width: raw_inner.width.saturating_sub(2),
        height: raw_inner.height,
    };

    // -----------------------------------------------------------------------
    // Compute header content
    // -----------------------------------------------------------------------

    // Row 0: ctx + work time (left) + status badge (right) — always height 2
    let sym = status_symbol(&entry.meta.status);
    let lbl = status_label(&entry.meta.status);
    let col = status_color(&entry.meta.status);

    let ctx_text = if let Some(ctx) = &entry.meta.context {
        let used = format_tokens(ctx.used);
        if let Some(total) = ctx.total {
            format!("{}/{}", used, format_tokens(total))
        } else {
            used
        }
    } else {
        "—".to_string()
    };

    let work_text = if entry.meta.total_work_ms > 0 {
        format!(
            "  {} {}",
            ICON_TIME,
            format_uptime(entry.meta.total_work_ms)
        )
    } else {
        String::new()
    };
    let left_text = format!("{}{}", ctx_text, work_text);

    // Status badge: "[ ● Running ]"
    let badge_inner = format!("{} {}", sym, lbl);
    let badge = format!("[ {} ]", badge_inner);
    let avail = inner.width as usize;
    let padding = avail.saturating_sub(left_text.chars().count() + badge.chars().count());
    let row0 = Line::from(vec![
        Span::styled(left_text, ds(dimmed).fg(GRAY)),
        Span::raw(" ".repeat(padding)),
        Span::styled("[ ", ds(dimmed).fg(BG2)),
        Span::styled(badge_inner, ds(dimmed).fg(col).add_modifier(Modifier::BOLD)),
        Span::styled(" ]", ds(dimmed).fg(BG2)),
    ]);

    // First prompt only — single line, centered vertically with 1-cell top/bottom padding
    let fp_raw = entry.meta.first_prompt.as_deref().unwrap_or("");
    let fp_text = first_line(fp_raw);

    let prompt_h: u16 = 3; // top padding + text + bottom padding
    let row1_h = prompt_h;
    let row0_h: u16 = 2;

    // --- Info row A: directory ---
    let dir_str = shellify_dir(&entry.config.directory);
    let info_a = Line::from(vec![
        Span::styled(format!("{} ", ICON_DIR), ds(dimmed).fg(GRAY)),
        Span::styled(dir_str, ds(dimmed).fg(GRAY)),
    ]);

    // --- Info row B: agent_type · model_name ---
    let agent_type = &entry.config.agent_type;
    let model_str = entry.meta.model_name.as_deref().unwrap_or("—");
    let info_b = Line::from(vec![
        Span::styled(format!("{} ", ICON_AGENT), ds(dimmed).fg(GRAY)),
        Span::styled(agent_type.as_str(), ds(dimmed).fg(GRAY)),
        Span::styled("  ", ds(dimmed).fg(GRAY)),
        Span::styled(format!("{} ", ICON_MODEL), ds(dimmed).fg(GRAY)),
        Span::styled(model_str, ds(dimmed).fg(GRAY)),
    ]);

    let info_row_h: u16 = 1;
    let info_row_b_h: u16 = 2; // 1 text + 1 empty margin line
    let header_lines = row0_h + info_row_h + info_row_b_h + row1_h;

    // -----------------------------------------------------------------------
    // Layout: header + response block
    // -----------------------------------------------------------------------

    let (header_area, response_area) = if inner.height > header_lines {
        let splits = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(header_lines), Constraint::Min(0)])
            .split(inner);
        (splits[0], Some(splits[1]))
    } else {
        (inner, None)
    };

    // -----------------------------------------------------------------------
    // Render header rows
    // -----------------------------------------------------------------------

    // Helper: render a line with bottom padding inside a slot
    let render_centered = |f: &mut Frame, slot: Rect, line: Line, h: u16| {
        if slot.height == 0 || h == 0 {
            return;
        }
        let sub = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(h.saturating_sub(1).max(1)),
                Constraint::Length(1),
            ])
            .split(slot);
        f.render_widget(Paragraph::new(line), sub[0]);
    };

    // Helper: render first prompt with thick yellow left border, BG1 fill,
    // and padding(left=1, right=1, top=1, bottom=1) via Ratatui's Block API.
    let render_prompt = |f: &mut Frame, slot: Rect, text: &str| {
        if slot.height == 0 {
            return;
        }

        // Block: thick left border (Yellow) + BG1 background fill + padding
        let prompt_block = Block::default()
            .borders(Borders::LEFT)
            .border_type(BorderType::Thick)
            .border_style(ds(dimmed).fg(YELLOW))
            .style(ds(dimmed).bg(BG1))
            .padding(Padding::new(1, 1, 1, 1));
        let text_inner = prompt_block.inner(slot);
        f.render_widget(prompt_block, slot);

        let usable = text_inner.width as usize;
        let content = if text.is_empty() {
            Paragraph::new(Span::styled("—", ds(dimmed).fg(GRAY))).style(ds(dimmed).bg(BG1))
        } else {
            Paragraph::new(Span::styled(truncate(text, usable), ds(dimmed).fg(FG)))
                .style(ds(dimmed).bg(BG1))
        };
        f.render_widget(content, text_inner);
    };

    // Build header row areas
    let constraints = vec![
        Constraint::Length(row0_h),
        Constraint::Length(info_row_h),
        Constraint::Length(info_row_b_h),
        Constraint::Length(row1_h),
    ];
    let header_splits = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(header_area);

    render_centered(f, header_splits[0], row0, row0_h);
    f.render_widget(Paragraph::new(info_a), header_splits[1]);
    f.render_widget(Paragraph::new(info_b), header_splits[2]);
    render_prompt(f, header_splits[3], fp_text);

    // -----------------------------------------------------------------------
    // Response block
    // -----------------------------------------------------------------------

    let Some(resp_area) = response_area else {
        return (0, 0);
    };

    // 1-line gap, then content
    let content_area = if resp_area.height > 1 {
        Rect {
            x: resp_area.x,
            y: resp_area.y + 1,
            width: resp_area.width,
            height: resp_area.height - 1,
        }
    } else {
        return (0, 0);
    };

    // Render response content
    match &entry.meta.last_model_response {
        Some(response) if !response.is_empty() => {
            let md_text = tui_markdown::from_str(response);
            let scroll_offset = if is_selected { response_scroll } else { 0 };
            let para = Paragraph::new(md_text)
                .wrap(ratatui::widgets::Wrap { trim: false })
                .scroll((scroll_offset, 0));
            f.render_widget(para, content_area);

            // Scroll hint on selected card
            if is_selected && scroll_offset > 0 {
                let hint_area = Rect {
                    height: 1,
                    ..content_area
                };
                let hint = Paragraph::new("▲ PgUp")
                    .style(ds(dimmed).fg(GRAY))
                    .alignment(Alignment::Right);
                f.render_widget(hint, hint_area);
            }
        }
        _ => {
            let placeholder = Paragraph::new("no response yet").style(ds(dimmed).fg(GRAY));
            f.render_widget(placeholder, content_area);
        }
    }

    (content_area.height, content_area.width)
}

// ---------------------------------------------------------------------------
// Keybindings bar
// ---------------------------------------------------------------------------

/// Renders a styled `[key]` + ` action` pair as spans into the given vec.
fn push_keybind<'a>(spans: &mut Vec<Span<'a>>, key: &'a str, action: &'a str, dimmed: bool) {
    spans.push(Span::styled("[", ds(dimmed).fg(BG2)));
    spans.push(Span::styled(key, ds(dimmed).fg(ORANGE)));
    spans.push(Span::styled("]", ds(dimmed).fg(BG2)));
    spans.push(Span::styled(action, ds(dimmed).fg(GRAY)));
}

fn render_keybindings_bar(f: &mut Frame, area: Rect, agents: &[AgentEntry], dimmed: bool) {
    let running = count_running(agents);
    let waiting = count_waiting(agents);

    let mut spans: Vec<Span> = Vec::new();

    push_keybind(&mut spans, "n", " New", dimmed);
    spans.push(Span::raw("  "));
    push_keybind(&mut spans, "d", " Del", dimmed);
    spans.push(Span::raw("  "));
    push_keybind(&mut spans, "Enter", " Open", dimmed);
    spans.push(Span::raw("  "));
    push_keybind(&mut spans, "←↓↑→", " Navigate", dimmed);
    spans.push(Span::raw("  "));
    push_keybind(&mut spans, "Ctrl+←↓↑→", " Move", dimmed);
    spans.push(Span::raw("  "));
    push_keybind(&mut spans, "PgUp/Dn", " Scroll", dimmed);
    spans.push(Span::raw("  "));
    push_keybind(&mut spans, "q", " Quit", dimmed);

    // Status counts
    spans.push(Span::styled(
        format!("    {} {} running", ICON_RUN, running),
        ds(dimmed).fg(GREEN),
    ));
    spans.push(Span::styled(
        format!("  {} {} waiting", ICON_WAIT, waiting),
        ds(dimmed).fg(YELLOW),
    ));

    let status_line = Line::from(spans);

    let hint = " Shift+drag to select";
    let hint_width = hint.len() as u16;
    let bar_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(0), Constraint::Length(hint_width)])
        .split(area);

    f.render_widget(Paragraph::new(status_line), bar_chunks[0]);
    f.render_widget(
        Paragraph::new(hint).style(ds(dimmed).fg(GRAY)),
        bar_chunks[1],
    );
}

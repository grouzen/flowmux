use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Padding, Paragraph},
};

use crate::models::{AgentEntry, AgentStatus, AgentStatusCounts};
use crate::ui::theme::*;

pub const PROJECT_TABS_HEIGHT: u16 = 1;

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

#[allow(clippy::too_many_arguments)]
pub fn render_dashboard(
    f: &mut Frame,
    area: Rect,
    agents: &[AgentEntry],
    visible_indices: &[usize],
    selected: Option<usize>,
    projects: &[String],
    active_project_idx: usize,
    card_scroll: &[u16],
    card_response_heights: &mut Vec<u16>,
    card_response_widths: &mut Vec<u16>,
    dimmed: bool,
    status_counts: AgentStatusCounts,
    blink_running: bool,
    blink_waiting: bool,
) {
    // Split into tabs, main area, and keybindings bar at bottom
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(PROJECT_TABS_HEIGHT),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(area);

    let tabs_area = chunks[0];
    let main_area = chunks[1];
    let bar_area = chunks[2];

    render_project_tabs(f, tabs_area, projects, active_project_idx, dimmed);
    render_keybindings_bar(
        f,
        bar_area,
        dimmed,
        status_counts,
        blink_running,
        blink_waiting,
    );
    render_grid(
        f,
        main_area,
        agents,
        visible_indices,
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

#[allow(clippy::too_many_arguments)]
fn render_grid(
    f: &mut Frame,
    area: Rect,
    agents: &[AgentEntry],
    visible_indices: &[usize],
    selected: Option<usize>,
    card_scroll: &[u16],
    card_response_heights: &mut Vec<u16>,
    card_response_widths: &mut Vec<u16>,
    dimmed: bool,
) {
    if visible_indices.is_empty() {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Ratio(1, 2),
                Constraint::Length(1),
                Constraint::Ratio(1, 2),
            ])
            .split(area);
        let msg = Paragraph::new("No agents in this project. Press [n] to create one.")
            .style(ds(dimmed).fg(GRAY))
            .alignment(Alignment::Center);
        f.render_widget(msg, chunks[1]);
        return;
    }

    let (cols, rows) = grid_layout(visible_indices.len());

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

            if let Some(&agent_idx) = visible_indices.get(slot) {
                let scroll = card_scroll.get(agent_idx).copied().unwrap_or(0);
                let (resp_h, resp_w) = render_card(
                    f,
                    cell_area,
                    &agents[agent_idx],
                    selected == Some(agent_idx),
                    scroll,
                    dimmed,
                );
                card_response_heights[agent_idx] = resp_h;
                card_response_widths[agent_idx] = resp_w;
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
        AgentStatus::Idle => ICON_IDLE,
        AgentStatus::Stopped => ICON_STOP,
        AgentStatus::Unknown => "?",
    }
}

fn status_label(status: &AgentStatus) -> &'static str {
    match status {
        AgentStatus::Running => "Running",
        AgentStatus::WaitingForInput => "Waiting",
        AgentStatus::Idle => "Idle",
        AgentStatus::Stopped => "Stopped",
        AgentStatus::Unknown => "Unknown",
    }
}

fn status_color(status: &AgentStatus) -> ratatui::style::Color {
    match status {
        AgentStatus::Running => GREEN,
        AgentStatus::WaitingForInput => YELLOW,
        AgentStatus::Idle => CYAN,
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
pub(crate) fn shellify_dir(dir: &str) -> String {
    if let Ok(home) = std::env::var("HOME")
        && let Some(rest) = dir.strip_prefix(&home)
    {
        return format!("~{}", rest);
    }
    dir.to_string()
}

/// Formats a millisecond duration into a human-readable string
/// (e.g. "3h 12m", "45m", "< 1m").
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
            format!("{} {}/{}", ICON_CTX, used, format_tokens(total))
        } else {
            format!("{} {}", ICON_CTX, used)
        }
    } else {
        format!("{} ∞/∞", ICON_CTX)
    };

    let work_text = format!(
        "  {} {}",
        ICON_TIME,
        format_uptime(entry.meta.total_work_ms)
    );
    let left_text = format!("{}{}", ctx_text, work_text);

    // Status badge: colored bg pill " ● Running "
    let badge_text = format!(" {} {} ", sym, lbl);
    let avail = inner.width as usize;
    let padding = avail.saturating_sub(
        unicode_width::UnicodeWidthStr::width(left_text.as_str())
            + unicode_width::UnicodeWidthStr::width(badge_text.as_str()),
    );
    let row0 = Line::from(vec![
        Span::styled(left_text, ds(dimmed).fg(GRAY)),
        Span::raw(" ".repeat(padding)),
        Span::styled(
            badge_text,
            ds(dimmed).fg(BG1).bg(col).add_modifier(Modifier::BOLD),
        ),
    ]);

    // First prompt only — single line, centered vertically with 1-cell top/bottom padding
    let fp_raw = entry.meta.first_prompt.as_deref().unwrap_or("");
    let fp_text = first_line(fp_raw);

    let prompt_h: u16 = 3; // top padding + text + bottom padding
    let row1_h = prompt_h;
    let row0_h: u16 = 2;

    // --- Info row A: directory ---
    let dir_str = shellify_dir(&entry.config.directory);
    let dir_display = if let Some(branch) =
        crate::git::current_branch(std::path::Path::new(&entry.config.directory))
    {
        format!("{}:{}", dir_str, branch)
    } else {
        dir_str
    };
    let dir_prefix = format!("{} ", ICON_DIR);

    // --- Info row B: agent_type · model_name (only if known) ---
    let agent_type = entry.config.agent_type_str();
    let mut info_b_spans = vec![
        Span::styled(format!("{} ", ICON_AGENT), ds(dimmed).fg(GRAY)),
        Span::styled(agent_type, ds(dimmed).fg(GRAY)),
    ];
    if let Some(model_str) = entry.meta.model_name.as_deref() {
        info_b_spans.push(Span::styled(" ", ds(dimmed).fg(GRAY)));
        info_b_spans.push(Span::styled(
            format!("{} ", ICON_MODEL),
            ds(dimmed).fg(GRAY),
        ));
        info_b_spans.push(Span::styled(model_str, ds(dimmed).fg(GRAY)));
    }
    let info_b = Line::from(info_b_spans);

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
        let content = if !text.is_empty() {
            Paragraph::new(Span::styled(truncate(text, usable), ds(dimmed).fg(FG)))
                .style(ds(dimmed).bg(BG1))
        } else {
            Paragraph::new(Span::styled(
                "No prompt yet",
                ds(dimmed).fg(GRAY).add_modifier(Modifier::ITALIC),
            ))
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
    // Render directory row with left-truncation (same approach as agent_view top bar)
    {
        let slot = header_splits[1];
        let prefix_width = unicode_width::UnicodeWidthStr::width(dir_prefix.as_str()) as u16;
        let dir_area = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(prefix_width), Constraint::Min(0)])
            .split(slot);
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(&dir_prefix, ds(dimmed).fg(GRAY)))),
            dir_area[0],
        );
        let dir_text_width = dir_area[1].width as usize;
        let dir_line_width = unicode_width::UnicodeWidthStr::width(dir_display.as_str());
        let is_truncated = dir_line_width > dir_text_width;
        let text_chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Length(if is_truncated { 1 } else { 0 }),
                Constraint::Min(0),
            ])
            .split(dir_area[1]);
        if is_truncated {
            f.render_widget(
                Paragraph::new(Line::from(Span::styled("…", ds(dimmed).fg(GRAY)))),
                text_chunks[0],
            );
        }
        let text_w = text_chunks[1].width as usize;
        let text_scroll = dir_line_width.saturating_sub(text_w) as u16;
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(&dir_display, ds(dimmed).fg(GRAY))))
                .scroll((0, text_scroll)),
            text_chunks[1],
        );
    }
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
            let hint_top = content_area.height.saturating_sub(1) / 2;
            let hint_area = Rect {
                y: content_area.y + hint_top,
                height: 1,
                ..content_area
            };
            let hint = Paragraph::new(Span::styled(
                "No response yet",
                ds(dimmed).fg(GRAY).add_modifier(Modifier::ITALIC),
            ))
            .alignment(Alignment::Center);
            f.render_widget(hint, hint_area);
        }
    }

    (content_area.height, content_area.width)
}

// ---------------------------------------------------------------------------
// Keybindings bar
// ---------------------------------------------------------------------------

/// Renders a styled ` key  action` pair as spans into the given vec.
/// The key is shown on an orange background with bright white text; no brackets.
fn push_keybind<'a>(spans: &mut Vec<Span<'a>>, key: &'a str, action: &'a str, dimmed: bool) {
    spans.push(Span::styled(
        format!(" {} ", key),
        ds(dimmed).fg(FG).bg(BG2).add_modifier(Modifier::BOLD),
    ));
    spans.push(Span::styled(format!(" {}", action), ds(dimmed).fg(FG)));
}

fn render_keybindings_bar(
    f: &mut Frame,
    area: Rect,
    dimmed: bool,
    status_counts: AgentStatusCounts,
    blink_running: bool,
    blink_waiting: bool,
) {
    // Left: hotkeys
    let mut spans: Vec<Span> = Vec::new();
    spans.push(Span::raw(" "));
    push_keybind(&mut spans, "n", "new agent", dimmed);
    spans.push(Span::raw(" "));
    push_keybind(&mut spans, "p", "new project", dimmed);
    spans.push(Span::raw(" "));
    push_keybind(&mut spans, "d", "delete agent", dimmed);
    spans.push(Span::raw(" "));
    push_keybind(&mut spans, "ctrl+d", "delete project", dimmed);
    spans.push(Span::raw(" "));
    push_keybind(&mut spans, "tab (0-9)", "switch projects", dimmed);
    spans.push(Span::raw(" "));
    push_keybind(&mut spans, "enter", "open", dimmed);
    spans.push(Span::raw(" "));
    push_keybind(&mut spans, "ctrl+arrows", "move card", dimmed);
    spans.push(Span::raw(" "));
    push_keybind(&mut spans, "pgup/pgdown", "scroll", dimmed);
    spans.push(Span::raw(" "));
    push_keybind(&mut spans, "q", "quit", dimmed);

    // Right: agent status counts (leading space separates from middle chunk; trailing space before brand)
    let (status_spans, status_width) = status_count_spans(
        status_counts.running,
        status_counts.waiting,
        status_counts.idle,
        blink_running,
        blink_waiting,
        dimmed,
    );

    let (brand, brand_width) = brand_line(dimmed);

    let bar_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Min(0),
            Constraint::Length(status_width),
            Constraint::Length(brand_width),
        ])
        .split(area);

    f.render_widget(Paragraph::new(Line::from(spans)), bar_chunks[0]);
    f.render_widget(Paragraph::new(Line::from(status_spans)), bar_chunks[1]);
    f.render_widget(Paragraph::new(brand), bar_chunks[2]);
}

fn render_project_tabs(
    f: &mut Frame,
    area: Rect,
    projects: &[String],
    active_project_idx: usize,
    dimmed: bool,
) {
    let mut spans: Vec<Span> = Vec::new();
    spans.push(Span::raw(" "));

    for (idx, project) in projects.iter().enumerate() {
        let digit = match idx {
            0..=8 => char::from(b'1' + idx as u8),
            9 => '0',
            _ => '?',
        };
        let label = format!(" {} {} ", digit, project);
        let style = if idx == active_project_idx {
            ds(dimmed).fg(BG1).bg(BLUE).add_modifier(Modifier::BOLD)
        } else {
            ds(dimmed).fg(FG).bg(BG2)
        };
        spans.push(Span::styled(label, style));
        spans.push(Span::raw(" "));
    }

    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

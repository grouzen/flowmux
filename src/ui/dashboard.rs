use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Padding, Paragraph},
};

use crate::app::DashboardPreviewState;
use crate::host_terminal::HostColors;
use crate::models::{AgentEntry, AgentStatus, AgentStatusCounts};
use crate::ui::theme::*;

pub const PROJECT_TABS_HEIGHT: u16 = 1;
const KEYBINDINGS_BAR_HEIGHT: u16 = 1;
const CARD_HEADER_LINES: u16 = 8;
const CARD_RESPONSE_TOP_GAP: u16 = 1;

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
    previews: &mut [DashboardPreviewState],
    visible_indices: &[usize],
    selected: Option<usize>,
    projects: &[String],
    active_project_idx: usize,
    host_colors: HostColors,
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
            Constraint::Length(KEYBINDINGS_BAR_HEIGHT),
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
        previews,
        visible_indices,
        selected,
        host_colors,
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

pub fn dashboard_preview_sizes(area: Rect, visible_count: usize) -> Vec<(u16, u16)> {
    if visible_count == 0 {
        return Vec::new();
    }

    let main_area = dashboard_main_area(area);
    grid_card_areas(main_area, visible_count)
        .into_iter()
        .filter_map(card_preview_size)
        .take(visible_count)
        .collect()
}

fn dashboard_main_area(area: Rect) -> Rect {
    Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(PROJECT_TABS_HEIGHT),
            Constraint::Min(0),
            Constraint::Length(KEYBINDINGS_BAR_HEIGHT),
        ])
        .split(area)[1]
}

fn grid_card_areas(area: Rect, visible_count: usize) -> Vec<Rect> {
    if visible_count == 0 {
        return Vec::new();
    }

    let (cols, rows) = grid_layout(visible_count);

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

    let mut card_areas = Vec::with_capacity(visible_count);
    for row in 0..rows {
        let col_areas = Layout::default()
            .direction(Direction::Horizontal)
            .constraints(col_constraints.clone())
            .split(row_areas[row]);

        for col in 0..cols {
            if card_areas.len() == visible_count {
                return card_areas;
            }
            card_areas.push(col_areas[col]);
        }
    }

    card_areas
}

fn card_preview_size(area: Rect) -> Option<(u16, u16)> {
    let raw_inner = Rect {
        x: area.x.saturating_add(1),
        y: area.y.saturating_add(1),
        width: area.width.saturating_sub(2),
        height: area.height.saturating_sub(2),
    };
    if raw_inner.height == 0 || raw_inner.width < 2 {
        return None;
    }

    let inner = Rect {
        x: raw_inner.x.saturating_add(1),
        y: raw_inner.y,
        width: raw_inner.width.saturating_sub(2),
        height: raw_inner.height,
    };

    if inner.height <= CARD_HEADER_LINES + CARD_RESPONSE_TOP_GAP || inner.width == 0 {
        return None;
    }

    Some((
        inner.width,
        inner
            .height
            .saturating_sub(CARD_HEADER_LINES + CARD_RESPONSE_TOP_GAP),
    ))
}

fn render_grid(
    f: &mut Frame,
    area: Rect,
    agents: &[AgentEntry],
    previews: &mut [DashboardPreviewState],
    visible_indices: &[usize],
    selected: Option<usize>,
    host_colors: HostColors,
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

    let card_areas = grid_card_areas(area, visible_indices.len());
    for (slot, cell_area) in card_areas.into_iter().enumerate() {
        if let Some(&agent_idx) = visible_indices.get(slot) {
            let preview = previews.get_mut(agent_idx);
            render_card(
                f,
                cell_area,
                &agents[agent_idx],
                preview,
                selected == Some(agent_idx),
                host_colors,
                dimmed,
            );
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
    if let Ok(home) = std::env::var("HOME") {
        if let Some(rest) = dir.strip_prefix(&home) {
            return format!("~{}", rest);
        }
    }
    dir.to_string()
}

/// Formats a millisecond duration into a human-readable string
/// (e.g. "3h 12m", "45m", "< 1m").

fn render_card(
    f: &mut Frame,
    area: Rect,
    entry: &AgentEntry,
    preview: Option<&mut DashboardPreviewState>,
    is_selected: bool,
    host_colors: HostColors,
    dimmed: bool,
) {
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
        return;
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
    let header_lines = CARD_HEADER_LINES;

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
        return;
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
        return;
    };

    if let Some(preview) = preview {
        if let Some((terminal, render_state)) = preview.terminal_and_render_state_mut() {
            crate::ghostty::render::render_embedded_terminal(
                terminal,
                render_state,
                f,
                content_area,
                host_colors.fg,
                host_colors.bg,
            );
        } else {
            render_preview_placeholder(f, content_area, dimmed);
        }
        if dimmed {
            let buf = f.buffer_mut();
            for y in content_area.y..content_area.bottom() {
                for x in content_area.x..content_area.right() {
                    let cell = &mut buf[(x, y)];
                    cell.set_style(cell.style().add_modifier(Modifier::DIM));
                }
            }
        }
    } else {
        render_preview_placeholder(f, content_area, dimmed);
    }
}

fn render_preview_placeholder(f: &mut Frame, content_area: Rect, dimmed: bool) {
    let hint_top = content_area.height.saturating_sub(1) / 2;
    let hint_area = Rect {
        y: content_area.y + hint_top,
        height: 1,
        ..content_area
    };
    let hint = Paragraph::new(Span::styled(
        "No preview yet",
        ds(dimmed).fg(GRAY).add_modifier(Modifier::ITALIC),
    ))
    .alignment(Alignment::Center);
    f.render_widget(hint, hint_area);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dashboard_preview_sizes_change_with_grid_layout() {
        let one = dashboard_preview_sizes(Rect::new(0, 0, 120, 40), 1);
        let four = dashboard_preview_sizes(Rect::new(0, 0, 120, 40), 4);

        assert_eq!(one.len(), 1);
        assert_eq!(four.len(), 4);
        assert!(four[0].0 < one[0].0);
        assert!(four[0].1 < one[0].1);
    }

    #[test]
    fn dashboard_preview_sizes_skip_cards_without_response_room() {
        let sizes = dashboard_preview_sizes(Rect::new(0, 0, 20, 10), 1);
        assert!(sizes.is_empty());
    }
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

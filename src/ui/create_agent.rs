use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Clear, Paragraph},
};

use crate::app::{CreateAgentState, CreateField, MAX_DIR_VISIBLE};
use crate::models::AgentType;
use crate::ui::theme::*;

// ---------------------------------------------------------------------------
// Layout constants
// ---------------------------------------------------------------------------

/// Left padding spaces (3 chars).
const LEFT_PAD: usize = 3;
/// Width of the full label prefix: LEFT_PAD + 10-char padded label name.
const LABEL_WIDTH: u16 = (LEFT_PAD + 10) as u16; // 13

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

pub fn render_create_agent(f: &mut Frame, area: Rect, state: &CreateAgentState) {
    // 40% of terminal width, minimum 48, leave at least 4 cols margin
    let modal_width = ((area.width as u32 * 40 / 100) as u16)
        .max(48)
        .min(area.width.saturating_sub(4));

    let visible_dir_rows = state.dir_matches.len().min(MAX_DIR_VISIBLE) as u16;
    let dir_section_rows: u16 = if state.dir_matches.is_empty() {
        0
    } else {
        1 + 1 + visible_dir_rows // blank + "Suggested" label + items
    };

    // Worktree checkbox: shown when the directory is inside a git repo.
    let worktree_rows: u16 = if state.git_repo_root.is_some() { 2 } else { 0 }; // blank + checkbox

    let agent_rows = state.available_types.len().max(1) as u16;
    let agent_label_row: u16 = if state.available_types.len() > 1 {
        1
    } else {
        0
    };
    let error_rows: u16 = if state.error.is_some() { 1 } else { 0 };

    // blank + title + blank + Name + blank + Directory
    // + dir_section + worktree_rows + blank + [agent_label] + agent_rows + blank + [error] + buttons + blank
    let modal_height = 3
        + 1
        + 1
        + 1
        + dir_section_rows
        + worktree_rows
        + 1
        + agent_label_row
        + agent_rows
        + 1
        + error_rows
        + 1
        + 1;

    let modal_area = centered_rect(modal_width, modal_height, area);

    f.render_widget(Clear, modal_area);
    f.render_widget(Block::default().style(Style::default().bg(BG1)), modal_area);

    // Build layout constraints
    let mut constraints: Vec<Constraint> = vec![
        Constraint::Length(1), // blank
        Constraint::Length(1), // title
        Constraint::Length(1), // blank
        Constraint::Length(1), // Name
        Constraint::Length(1), // blank
        Constraint::Length(1), // Directory
    ];
    if !state.dir_matches.is_empty() {
        constraints.push(Constraint::Length(1)); // blank gap
        constraints.push(Constraint::Length(1)); // "Suggested" label
        for _ in 0..visible_dir_rows {
            constraints.push(Constraint::Length(1));
        }
    }
    // Git worktree checkbox (shown when directory is inside a git repo)
    if state.git_repo_root.is_some() {
        constraints.push(Constraint::Length(1)); // blank gap
        constraints.push(Constraint::Length(1)); // checkbox
    }
    constraints.push(Constraint::Length(1)); // blank
    if state.available_types.len() > 1 {
        constraints.push(Constraint::Length(1)); // "Agent" label
    }
    for _ in 0..agent_rows {
        constraints.push(Constraint::Length(1));
    }
    constraints.push(Constraint::Length(1)); // blank
    if state.error.is_some() {
        constraints.push(Constraint::Length(1));
    }
    constraints.push(Constraint::Length(1)); // buttons
    constraints.push(Constraint::Min(0)); // trailing padding

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(modal_area);

    let mut row = 0usize;

    // blank
    row += 1;

    // Title
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::raw(left_pad()),
            Span::styled(
                "Launch agent",
                Style::default().fg(FG).add_modifier(Modifier::BOLD),
            ),
        ]))
        .style(Style::default().bg(BG1)),
        rows[row],
    );
    row += 1;

    // blank
    row += 1;

    // Name input — cursor at end of typed text
    let name_focused = state.focus == CreateField::Name;
    render_field_row(
        f,
        rows[row],
        "Name",
        &state.name,
        "type a name...",
        name_focused,
    );
    if name_focused {
        let val_width = rows[row].width.saturating_sub(LABEL_WIDTH + 3);
        let displayed_len = state.name.len().min(val_width as usize) as u16;
        let cx = (rows[row].x + LABEL_WIDTH + displayed_len)
            .min(modal_area.x + modal_area.width.saturating_sub(1));
        f.set_cursor_position((cx, rows[row].y));
    }
    row += 1;

    // blank
    row += 1;

    // Directory input — trailing slash only when `directory` is a valid existing dir
    let dir_focused = state.focus == CreateField::Directory;
    // For root "/", trimming slashes yields "" which is unusable; keep "/" as-is.
    let dir_base = if state.directory == "/" {
        "/"
    } else {
        state.directory.trim_end_matches('/')
    };
    let dir_display = if !state.dir_filter.is_empty() {
        // User is typing a filter: show base/filter (no trailing slash yet)
        if dir_base == "/" {
            format!("/{}", state.dir_filter)
        } else {
            format!("{}/{}", dir_base, state.dir_filter)
        }
    } else if std::path::Path::new(dir_base).is_dir() {
        // Confirmed existing directory: append trailing slash (but root already has one)
        if dir_base == "/" {
            "/".to_string()
        } else {
            format!("{}/", dir_base)
        }
    } else {
        // Unknown/empty path: show as-is
        dir_base.to_string()
    };
    render_field_row(
        f,
        rows[row],
        "Directory",
        &dir_display,
        "type a path...",
        dir_focused,
    );
    if dir_focused {
        let val_width = rows[row].width.saturating_sub(LABEL_WIDTH + 3);
        let displayed_len = dir_display.len().min(val_width as usize) as u16;
        let cx = (rows[row].x + LABEL_WIDTH + displayed_len)
            .min(modal_area.x + modal_area.width.saturating_sub(1));
        f.set_cursor_position((cx, rows[row].y));
    }
    row += 1;

    // Directory suggestions section
    if !state.dir_matches.is_empty() {
        // blank gap
        row += 1;

        // "Suggested" label in cyan
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::raw(label_pad()),
                Span::styled("Suggested", Style::default().fg(CYAN)),
            ]))
            .style(Style::default().bg(BG1)),
            rows[row],
        );
        row += 1;

        // Suggestion items — visible window only
        let total = state.dir_matches.len();
        let offset = state.dir_scroll_offset;
        let visible_count = visible_dir_rows as usize;
        let needs_scrollbar = total > MAX_DIR_VISIBLE;

        for vi in 0..visible_count {
            let abs_idx = offset + vi;
            let Some(suggestion) = state.dir_matches.get(abs_idx) else {
                break;
            };

            let selected = abs_idx == state.dir_selected_idx && dir_focused;
            // dir_matches already contains bare directory names
            let display = suggestion.as_str();

            // Scrollbar character for this row
            let scrollbar_char = if needs_scrollbar {
                scrollbar_char(vi, visible_count, offset, total)
            } else {
                ' '
            };

            // Content width = row width minus left pad (3) minus 1 for scrollbar column.
            // Prefix " ● " / "   " = 3 chars; name fills the rest.
            // Content width = row width minus 10-char pad minus 1 for scrollbar column.
            let content_width = rows[row].width.saturating_sub(11) as usize;
            let name_width = content_width.saturating_sub(3);

            let line = if selected {
                Line::from(vec![
                    Span::raw("          "),
                    Span::styled(
                        format!(" ● {:<width$}", display, width = name_width),
                        Style::default().fg(BG).bg(FG).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(scrollbar_char.to_string(), Style::default().fg(BG2).bg(BG1)),
                ])
            } else {
                Line::from(vec![
                    Span::raw("          "),
                    Span::styled(
                        format!("   {:<width$}", display, width = name_width),
                        Style::default().fg(GRAY),
                    ),
                    Span::styled(scrollbar_char.to_string(), Style::default().fg(BG2).bg(BG1)),
                ])
            };
            f.render_widget(
                Paragraph::new(line).style(Style::default().bg(BG1)),
                rows[row],
            );
            row += 1;
        }
    }

    // Git worktree checkbox
    if state.git_repo_root.is_some() {
        // blank gap
        row += 1;

        let wt_focused = state.focus == CreateField::CreateWorktree;
        let checkbox = if state.create_worktree { "[x]" } else { "[ ]" };
        let label_style = if wt_focused {
            Style::default().fg(FG).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(GRAY)
        };
        let checkbox_style = if state.create_worktree {
            if wt_focused {
                Style::default().fg(CYAN).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(CYAN)
            }
        } else {
            Style::default().fg(GRAY)
        };
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::raw(label_pad()),
                Span::styled(checkbox, checkbox_style),
                Span::styled(" Create git worktree", label_style),
                Span::styled(
                    "  space",
                    Style::default().fg(if wt_focused { GRAY } else { BG2 }),
                ),
            ]))
            .style(Style::default().bg(BG1)),
            rows[row],
        );
        row += 1;
    }

    // blank (before agent section)
    row += 1;
    let agent_focused = state.focus == CreateField::AgentType;
    if state.available_types.len() > 1 {
        let label_style = if agent_focused {
            Style::default().fg(FG)
        } else {
            Style::default().fg(GRAY)
        };
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::raw(left_pad()),
                Span::styled("Agent", label_style),
            ]))
            .style(Style::default().bg(BG1)),
            rows[row],
        );
        row += 1;
    }

    render_agent_type_list(f, &rows[row..row + agent_rows as usize], state);
    row += agent_rows as usize;

    // blank
    row += 1;

    // Error row
    if let Some(err) = &state.error {
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::raw(left_pad()),
                Span::styled(format!("{} ", ICON_ERR), Style::default().fg(RED)),
                Span::styled(err.as_str(), Style::default().fg(RED)),
            ]))
            .style(Style::default().bg(BG1)),
            rows[row],
        );
        row += 1;
    }

    // Buttons: solid rect + gray tip text
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::raw(left_pad()),
            Span::styled(
                " Launch ",
                Style::default()
                    .bg(ORANGE)
                    .fg(FG)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" enter", Style::default().fg(GRAY)),
            Span::raw("   "),
            Span::styled(
                " Cancel ",
                Style::default().bg(BG2).fg(FG).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" esc", Style::default().fg(GRAY)),
        ]))
        .style(Style::default().bg(BG1)),
        rows[row],
    );
}

// ---------------------------------------------------------------------------
// Agent type vertical list
// ---------------------------------------------------------------------------

fn agent_type_label(t: &AgentType) -> &'static str {
    match t {
        AgentType::Opencode => "opencode",
        AgentType::Claude => "claude",
        AgentType::Codex => "codex",
    }
}

fn render_agent_type_list(f: &mut Frame, areas: &[Rect], state: &CreateAgentState) {
    let focused = state.focus == CreateField::AgentType;
    let types = &state.available_types;

    if types.is_empty() {
        if let Some(area) = areas.first() {
            f.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::raw(label_pad()),
                    Span::styled(
                        format!("{} opencode", ICON_AGENT),
                        Style::default().fg(GREEN),
                    ),
                ]))
                .style(Style::default().bg(BG1)),
                *area,
            );
        }
        return;
    }

    for (i, t) in types.iter().enumerate() {
        let Some(area) = areas.get(i) else { break };
        let selected = i == state.selected_type_idx;
        let radio = if selected { "◉" } else { "○" };

        let style = if selected && focused {
            Style::default().fg(GREEN).add_modifier(Modifier::BOLD)
        } else if selected {
            Style::default().fg(GREEN)
        } else {
            Style::default().fg(GRAY)
        };

        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::raw(label_pad()),
                Span::styled(format!("{} {}", radio, agent_type_label(t)), style),
            ]))
            .style(Style::default().bg(BG1)),
            *area,
        );
    }
}

// ---------------------------------------------------------------------------
// Input field renderer
// ---------------------------------------------------------------------------

fn render_field_row(
    f: &mut Frame,
    area: Rect,
    label: &str,
    value: &str,
    placeholder: &str,
    focused: bool,
) {
    // Reserve RIGHT_PAD chars on the right to prevent overflow
    const RIGHT_PAD: u16 = 3;
    let val_width = area
        .width
        .saturating_sub(LABEL_WIDTH)
        .saturating_sub(RIGHT_PAD);
    let displayed = truncate_left(value, val_width as usize);
    let label_text = format!("{}{:<10}", left_pad(), label);

    let spans: Vec<Span> = if focused {
        vec![
            Span::styled(label_text, Style::default().fg(FG)),
            Span::styled(
                displayed,
                Style::default().fg(FG).add_modifier(Modifier::BOLD),
            ),
        ]
    } else if value.is_empty() {
        vec![
            Span::styled(label_text, Style::default().fg(GRAY)),
            Span::styled(placeholder, Style::default().fg(BG2)),
        ]
    } else {
        vec![
            Span::styled(label_text, Style::default().fg(GRAY)),
            Span::styled(displayed, Style::default().fg(GRAY)),
        ]
    };

    f.render_widget(
        Paragraph::new(Line::from(spans)).style(Style::default().bg(BG1)),
        area,
    );
}

// ---------------------------------------------------------------------------
// Scrollbar
// ---------------------------------------------------------------------------

/// Returns the scrollbar character to display at visible row `vi` given
/// the full list size `total` and current scroll `offset`.
fn scrollbar_char(vi: usize, visible: usize, offset: usize, total: usize) -> char {
    if total <= visible {
        return ' ';
    }
    // Thumb occupies proportional rows
    let thumb_size = (visible * visible / total).max(1);
    let max_offset = total - visible;
    let thumb_top = offset * (visible - thumb_size) / max_offset;
    let thumb_bot = thumb_top + thumb_size;
    if vi >= thumb_top && vi < thumb_bot {
        '█'
    } else {
        '░'
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// 3-space left padding string.
fn left_pad() -> &'static str {
    "   "
}

/// Padding that aligns content to the value column (LEFT_PAD + label width = 13 chars).
fn label_pad() -> &'static str {
    "             " // 13 spaces
}

fn truncate_left(s: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    if s.len() <= max {
        s.to_string()
    } else {
        let start = s.len() - max;
        s[start..].to_string()
    }
}

fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let x = area.x + area.width.saturating_sub(width) / 2;
    let y = area.y + area.height.saturating_sub(height) / 2;
    Rect {
        x,
        y,
        width: width.min(area.width),
        height: height.min(area.height),
    }
}

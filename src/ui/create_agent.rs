use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Clear, Paragraph},
    Frame,
};

use crate::app::{CreateAgentState, CreateField};
use crate::models::AgentType;
use crate::ui::theme::*;

// ---------------------------------------------------------------------------
// Layout constants
// ---------------------------------------------------------------------------

/// Width of the "  Label     " prefix in input rows (2 spaces + 10-char padded label).
const LABEL_WIDTH: u16 = 12;

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

pub fn render_create_agent(f: &mut Frame, area: Rect, state: &CreateAgentState) {
    let modal_width = 56u16;

    // Directory section: only shown when there are matches
    let dir_section_rows: u16 = if state.dir_matches.is_empty() {
        0
    } else {
        1 + 1 + state.dir_matches.len() as u16 // blank + "Suggested" label + items
    };

    let agent_rows = state.available_types.len().max(1) as u16;
    let agent_label_row: u16 = if state.available_types.len() > 1 {
        1
    } else {
        0
    };
    let error_rows: u16 = if state.error.is_some() { 1 } else { 0 };

    // Rows: blank + title + blank + Name + blank + Directory
    //       + dir_section + blank + [agent_label] + agent_rows + blank + [error] + buttons + blank
    let modal_height = 3
        + 1
        + 1
        + 1
        + dir_section_rows
        + 1
        + agent_label_row
        + agent_rows
        + 1
        + error_rows
        + 1
        + 1;

    let modal_area = centered_rect(modal_width, modal_height, area);

    // Clear behind the modal, then fill with BG1 background (no border)
    f.render_widget(Clear, modal_area);
    f.render_widget(Block::default().style(Style::default().bg(BG1)), modal_area);

    // Build layout constraints
    let mut constraints: Vec<Constraint> = vec![
        Constraint::Length(1), // blank
        Constraint::Length(1), // title "Launch agent"
        Constraint::Length(1), // blank
        Constraint::Length(1), // Name input
        Constraint::Length(1), // blank
        Constraint::Length(1), // Directory input
    ];
    if !state.dir_matches.is_empty() {
        constraints.push(Constraint::Length(1)); // blank gap
        constraints.push(Constraint::Length(1)); // "Suggested" label
        for _ in 0..state.dir_matches.len() {
            constraints.push(Constraint::Length(1));
        }
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

    // Title row — "Launch agent" top-left, bold
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::raw("  "),
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

    // Name input
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
        let cx = rows[row].x
            + LABEL_WIDTH
            + state
                .name
                .len()
                .min(rows[row].width.saturating_sub(LABEL_WIDTH) as usize) as u16;
        f.set_cursor_position((cx, rows[row].y));
    }
    row += 1;

    // blank
    row += 1;

    // Directory input
    let dir_focused = state.focus == CreateField::Directory;
    render_field_row(
        f,
        rows[row],
        "Directory",
        &state.directory,
        "type a path...",
        dir_focused,
    );
    if dir_focused {
        let val_width = rows[row].width.saturating_sub(LABEL_WIDTH);
        let displayed_len = state.directory.len().min(val_width as usize);
        let cx = rows[row].x + LABEL_WIDTH + displayed_len as u16;
        f.set_cursor_position((cx, rows[row].y));
    }
    row += 1;

    // Directory suggestions section
    if !state.dir_matches.is_empty() {
        // blank gap
        row += 1;

        // "Suggested" label
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::raw("  "),
                Span::styled("Suggested", Style::default().fg(GRAY)),
            ]))
            .style(Style::default().bg(BG1)),
            rows[row],
        );
        row += 1;

        // Suggestion items
        let base = &state.directory;
        for (i, suggestion) in state.dir_matches.iter().enumerate() {
            let selected = i == state.dir_selected_idx && dir_focused;

            // Show relative part only (strip base directory prefix)
            let display = relative_path(suggestion, base);

            let line = if selected {
                // Inverted: BG as text fg, FG as background — highlight entire row
                Line::from(vec![Span::styled(
                    format!(
                        "  {:<width$}",
                        display,
                        width = (modal_width as usize).saturating_sub(2)
                    ),
                    Style::default().fg(BG).bg(FG).add_modifier(Modifier::BOLD),
                )])
            } else {
                Line::from(vec![
                    Span::raw("  "),
                    Span::styled(&display, Style::default().fg(GRAY)),
                ])
            };
            f.render_widget(
                Paragraph::new(line).style(Style::default().bg(BG1)),
                rows[row],
            );
            row += 1;
        }
    }

    // blank
    row += 1;

    // Agent section
    if state.available_types.len() > 1 {
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::raw("  "),
                Span::styled("Agent", Style::default().fg(GRAY)),
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
                Span::raw("  "),
                Span::styled(format!("{} ", ICON_ERR), Style::default().fg(RED)),
                Span::styled(err.as_str(), Style::default().fg(RED)),
            ]))
            .style(Style::default().bg(BG1)),
            rows[row],
        );
        row += 1;
    }

    // Buttons row: solid rect + gray tip text
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::raw("  "),
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
    }
}

fn render_agent_type_list(f: &mut Frame, areas: &[Rect], state: &CreateAgentState) {
    let focused = state.focus == CreateField::AgentType;
    let types = &state.available_types;

    if types.is_empty() {
        if let Some(area) = areas.first() {
            f.render_widget(
                Paragraph::new(Line::from(vec![Span::styled(
                    format!("    {} opencode", ICON_AGENT),
                    Style::default().fg(GREEN),
                )]))
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
                Span::raw("    "),
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
    let label_text = format!("  {:<10}", label);
    let val_width = area.width.saturating_sub(LABEL_WIDTH);
    let displayed = truncate_left(value, val_width as usize);

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
// Helpers
// ---------------------------------------------------------------------------

/// Returns the part of `path` that comes after `base`, or the full path if
/// stripping fails. Strips a leading `/` from the result.
fn relative_path(path: &str, base: &str) -> String {
    let base_clean = base.trim_end_matches('/');
    if let Some(rest) = path.strip_prefix(base_clean) {
        rest.trim_start_matches('/').to_string()
    } else {
        path.to_string()
    }
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

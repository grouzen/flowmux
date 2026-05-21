use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
    Frame,
};

use crate::app::{CreateAgentState, CreateField};
use crate::models::AgentType;
use crate::ui::theme::*;

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

pub fn render_create_agent(f: &mut Frame, area: Rect, state: &CreateAgentState) {
    let modal_width = 56u16;

    let dir_rows = state.dir_matches.len() as u16;
    let agent_rows = state.available_types.len().max(1) as u16;
    let error_rows: u16 = if state.error.is_some() { 1 } else { 0 };

    // Layout rows:
    //  1  blank
    //  1  Name input
    //  1  blank
    //  1  Directory input
    //  N  dir suggestions
    //  1  blank
    //  1  "Agent" label (only if multiple types)
    //  M  agent type list
    //  1  blank
    //  E  error (0 or 1)
    //  1  buttons row
    //  1  blank
    let agent_label_row: u16 = if state.available_types.len() > 1 {
        1
    } else {
        0
    };
    let modal_height =
        1 + 1 + 1 + 1 + dir_rows + 1 + agent_label_row + agent_rows + 1 + error_rows + 1 + 1 + 2; // border

    let modal_area = centered_rect(modal_width, modal_height, area);

    // Clear the area behind the modal
    f.render_widget(Clear, modal_area);

    // Filled background (BG1 — brighter than app BG)
    let bg_block = Block::default()
        .style(Style::default().bg(BG1))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(ORANGE))
        .title(Span::styled(
            " New Agent ",
            Style::default().fg(FG).add_modifier(Modifier::BOLD),
        ));
    f.render_widget(bg_block, modal_area);

    let inner = Rect {
        x: modal_area.x + 1,
        y: modal_area.y + 1,
        width: modal_area.width.saturating_sub(2),
        height: modal_area.height.saturating_sub(2),
    };

    // Build constraint list dynamically
    let mut constraints: Vec<Constraint> = vec![
        Constraint::Length(1), // blank
        Constraint::Length(1), // Name
        Constraint::Length(1), // blank
        Constraint::Length(1), // Directory
    ];
    for _ in 0..dir_rows {
        constraints.push(Constraint::Length(1)); // each suggestion
    }
    constraints.push(Constraint::Length(1)); // blank
    if state.available_types.len() > 1 {
        constraints.push(Constraint::Length(1)); // "Agent" label
    }
    for _ in 0..agent_rows {
        constraints.push(Constraint::Length(1)); // each agent type
    }
    constraints.push(Constraint::Length(1)); // blank
    if state.error.is_some() {
        constraints.push(Constraint::Length(1)); // error
    }
    constraints.push(Constraint::Length(1)); // buttons
    constraints.push(Constraint::Min(0)); // trailing padding

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(inner);

    let mut row = 0usize;

    // blank
    row += 1;

    // Name input
    render_field_row(
        f,
        rows[row],
        "Name",
        &state.name,
        "type a name...",
        state.focus == CreateField::Name,
    );
    row += 1;

    // blank
    row += 1;

    // Directory input
    render_field_row(
        f,
        rows[row],
        "Directory",
        &state.directory,
        "type a path...",
        state.focus == CreateField::Directory,
    );
    row += 1;

    // Directory suggestions
    for (i, suggestion) in state.dir_matches.iter().enumerate() {
        let selected = i == state.dir_selected_idx;
        let focused = state.focus == CreateField::Directory;
        let style = if selected && focused {
            Style::default().fg(YELLOW).add_modifier(Modifier::BOLD)
        } else if selected {
            Style::default().fg(FG)
        } else {
            Style::default().fg(GRAY)
        };
        let prefix = if selected && focused { "▶ " } else { "  " };
        let line = Line::from(vec![
            Span::raw("    "),
            Span::styled(format!("{}{}", prefix, suggestion), style),
        ]);
        f.render_widget(
            Paragraph::new(line).style(Style::default().bg(BG1)),
            rows[row],
        );
        row += 1;
    }

    // blank
    row += 1;

    // Agent type section
    if state.available_types.len() > 1 {
        // "Agent" label row
        let label_line = Line::from(vec![Span::styled("  Agent", Style::default().fg(GRAY))]);
        f.render_widget(
            Paragraph::new(label_line).style(Style::default().bg(BG1)),
            rows[row],
        );
        row += 1;
    }

    // Agent type list items
    render_agent_type_list(f, &rows[row..row + agent_rows as usize], state);
    row += agent_rows as usize;

    // blank
    row += 1;

    // Error row
    if let Some(err) = &state.error {
        let err_line = Line::from(vec![
            Span::raw("  "),
            Span::styled(format!("{} ", ICON_ERR), Style::default().fg(RED)),
            Span::styled(err.as_str(), Style::default().fg(RED)),
        ]);
        f.render_widget(
            Paragraph::new(err_line).style(Style::default().bg(BG1)),
            rows[row],
        );
        row += 1;
    }

    // Buttons row — solid rectangles
    let launch_btn = Span::styled(
        " Launch ",
        Style::default()
            .bg(ORANGE)
            .fg(BG)
            .add_modifier(Modifier::BOLD),
    );
    let cancel_btn = Span::styled(" Cancel ", Style::default().bg(BG2).fg(FG));
    let btn_line = Line::from(vec![
        Span::raw("  "),
        launch_btn,
        Span::raw("  "),
        cancel_btn,
    ]);
    f.render_widget(
        Paragraph::new(btn_line).style(Style::default().bg(BG1)),
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
        // Fallback: show static opencode
        let line = Line::from(vec![Span::styled(
            format!("  {} opencode", ICON_AGENT),
            Style::default().fg(GREEN),
        )]);
        if let Some(area) = areas.first() {
            f.render_widget(Paragraph::new(line).style(Style::default().bg(BG1)), *area);
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

        let line = Line::from(vec![
            Span::raw("    "),
            Span::styled(format!("{} {}", radio, agent_type_label(t)), style),
        ]);
        f.render_widget(Paragraph::new(line).style(Style::default().bg(BG1)), *area);
    }
}

// ---------------------------------------------------------------------------
// Input field renderer — no brackets, cursor + tip text as indicators
// ---------------------------------------------------------------------------

fn render_field_row(
    f: &mut Frame,
    area: Rect,
    label: &str,
    value: &str,
    placeholder: &str,
    focused: bool,
) {
    // Reserve space: "  Label   " prefix + rest for value
    let label_text = format!("  {:<10}", label);
    let value_width = area.width.saturating_sub(label_text.len() as u16 + 1);
    let displayed = truncate_left(value, value_width as usize);

    let spans: Vec<Span> = if focused {
        // Focused: bright label, bright value, block cursor
        vec![
            Span::styled(&label_text, Style::default().fg(FG)),
            Span::styled(
                format!(
                    "{:<width$}",
                    displayed,
                    width = value_width.saturating_sub(1) as usize
                ),
                Style::default().fg(FG).add_modifier(Modifier::BOLD),
            ),
            Span::styled("▌", Style::default().fg(YELLOW)),
        ]
    } else if value.is_empty() {
        // Unfocused, empty: dim label + placeholder tip
        vec![
            Span::styled(&label_text, Style::default().fg(GRAY)),
            Span::styled(placeholder, Style::default().fg(BG2)),
        ]
    } else {
        // Unfocused, has value: dim label + dim value
        vec![
            Span::styled(&label_text, Style::default().fg(GRAY)),
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

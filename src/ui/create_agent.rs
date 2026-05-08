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
    let modal_width = 52u16;
    let error_rows: u16 = if state.error.is_some() { 1 } else { 0 };
    let modal_height = 12 + error_rows;

    let modal_area = centered_rect(modal_width, modal_height, area);

    f.render_widget(Clear, modal_area);

    let outer = Block::default()
        .title(Span::styled(
            " New Agent ",
            Style::default().fg(FG).add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(ORANGE));
    f.render_widget(outer, modal_area);

    let inner = Rect {
        x: modal_area.x + 1,
        y: modal_area.y + 1,
        width: modal_area.width.saturating_sub(2),
        height: modal_area.height.saturating_sub(2),
    };

    // Layout: blank / Name / blank / Directory / Tab-hint / blank / Agent / blank / [error] / hints / pad
    let mut constraints = vec![
        Constraint::Length(1), // blank
        Constraint::Length(1), // Name
        Constraint::Length(1), // blank
        Constraint::Length(1), // Directory
        Constraint::Length(1), // Tab hint
        Constraint::Length(1), // blank
        Constraint::Length(1), // Agent type row
        Constraint::Length(1), // blank
    ];
    if state.error.is_some() {
        constraints.push(Constraint::Length(1));
    }
    constraints.push(Constraint::Length(1)); // action hints
    constraints.push(Constraint::Min(0)); // trailing padding

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(inner);

    let mut row = 0usize;

    // blank
    row += 1;

    // Name
    render_field_row(
        f,
        rows[row],
        "Name:      ",
        &state.name,
        state.focus == CreateField::Name,
    );
    row += 1;

    // blank
    row += 1;

    // Directory
    render_field_row(
        f,
        rows[row],
        "Directory: ",
        &state.directory,
        state.focus == CreateField::Directory,
    );
    row += 1;

    // Tab hint
    let tab_hint = Line::from(vec![
        Span::raw("             "),
        Span::styled("[", Style::default().fg(BG2)),
        Span::styled("Tab", Style::default().fg(ORANGE)),
        Span::styled("]", Style::default().fg(BG2)),
        Span::styled(" path autocomplete", Style::default().fg(GRAY)),
    ]);
    f.render_widget(Paragraph::new(tab_hint), rows[row]);
    row += 1;

    // blank
    row += 1;

    // Agent type row
    render_agent_type_row(f, rows[row], state);
    row += 1;

    // blank
    row += 1;

    // Error (optional)
    if let Some(err) = &state.error {
        let err_line = Line::from(vec![
            Span::raw("  "),
            Span::styled(format!("{} ", ICON_ERR), Style::default().fg(RED)),
            Span::styled(err.as_str(), Style::default().fg(RED)),
        ]);
        f.render_widget(Paragraph::new(err_line), rows[row]);
        row += 1;
    }

    // Action hints — show Left/Right hint only when AgentType row is selectable
    let mut actions: Vec<Span> = vec![
        Span::raw("  "),
        Span::styled("[", Style::default().fg(BG2)),
        Span::styled("Enter", Style::default().fg(ORANGE)),
        Span::styled("]", Style::default().fg(BG2)),
        Span::styled(" Launch", Style::default().fg(GRAY)),
        Span::raw("  "),
        Span::styled("[", Style::default().fg(BG2)),
        Span::styled("Esc", Style::default().fg(GRAY)),
        Span::styled("]", Style::default().fg(BG2)),
        Span::styled(" Cancel", Style::default().fg(GRAY)),
    ];
    if state.available_types.len() > 1 {
        actions.push(Span::raw("  "));
        actions.push(Span::styled("[", Style::default().fg(BG2)));
        actions.push(Span::styled("←→", Style::default().fg(ORANGE)));
        actions.push(Span::styled("]", Style::default().fg(BG2)));
        actions.push(Span::styled(" Type", Style::default().fg(GRAY)));
    }
    f.render_widget(Paragraph::new(Line::from(actions)), rows[row]);
}

// ---------------------------------------------------------------------------
// Agent-type row
// ---------------------------------------------------------------------------

fn agent_type_label(t: &AgentType) -> &'static str {
    match t {
        AgentType::Opencode => "opencode",
        AgentType::Claude => "claude",
    }
}

/// Renders the Agent row generically over `state.available_types`.
///
/// - 0 or 1 types: static label, not interactive.
/// - 2+ types: radio buttons, focusable with Left/Right cycling.
fn render_agent_type_row(f: &mut Frame, area: Rect, state: &CreateAgentState) {
    let focused = state.focus == CreateField::AgentType;
    let types = &state.available_types;

    if types.len() <= 1 {
        // Static — only one type available
        let label = types.first().map(agent_type_label).unwrap_or("opencode");
        let line = Line::from(vec![
            Span::styled("  Agent:     ", Style::default().fg(GRAY)),
            Span::styled(
                format!("{} {}", ICON_AGENT, label),
                Style::default().fg(GREEN),
            ),
        ]);
        f.render_widget(Paragraph::new(line), area);
        return;
    }

    // Radio selector — iterate available_types
    let (bracket_color, label_style) = if focused {
        (YELLOW, Style::default().fg(FG).add_modifier(Modifier::BOLD))
    } else {
        (GRAY, Style::default().fg(GRAY))
    };

    let mut spans = vec![
        Span::styled("  ", label_style),
        Span::styled("[", Style::default().fg(bracket_color)),
        Span::styled("Agent", label_style),
        Span::styled("]", Style::default().fg(bracket_color)),
        Span::raw("   "),
    ];

    for (i, t) in types.iter().enumerate() {
        let selected = i == state.selected_type_idx;
        let radio = if selected { "◉" } else { "○" };
        let style = if selected {
            Style::default().fg(GREEN).add_modifier(if focused {
                Modifier::BOLD
            } else {
                Modifier::empty()
            })
        } else {
            Style::default().fg(GRAY)
        };
        if i > 0 {
            spans.push(Span::raw("  "));
        }
        spans.push(Span::styled(
            format!("{} {}", radio, agent_type_label(t)),
            style,
        ));
    }

    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

// ---------------------------------------------------------------------------
// Field row renderer
// ---------------------------------------------------------------------------

fn render_field_row(f: &mut Frame, area: Rect, label: &str, value: &str, focused: bool) {
    let input_width = area.width.saturating_sub(label.len() as u16 + 2 + 2);
    let displayed = truncate_left(value, input_width as usize);

    let (bracket_color, input_fg, input_modifier) = if focused {
        (YELLOW, FG, Modifier::BOLD)
    } else {
        (GRAY, GRAY, Modifier::empty())
    };

    let line = Line::from(vec![
        Span::styled(format!("  {}", label), Style::default().fg(GRAY)),
        Span::styled("[", Style::default().fg(bracket_color)),
        Span::styled(
            format!("{:<width$}", displayed, width = input_width as usize),
            Style::default().fg(input_fg).add_modifier(input_modifier),
        ),
        Span::styled("]", Style::default().fg(bracket_color)),
    ]);

    f.render_widget(Paragraph::new(line), area);
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn truncate_left(s: &str, max: usize) -> String {
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

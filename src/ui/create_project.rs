use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Clear, Paragraph},
};

use crate::app::CreateProjectState;
use crate::ui::theme::*;

pub fn render_create_project(f: &mut Frame, area: Rect, state: &CreateProjectState) {
    let dialog_width = ((area.width as u32 * 40 / 100) as u16)
        .max(44)
        .min(area.width.saturating_sub(4));
    let error_rows: u16 = if state.error.is_some() { 1 } else { 0 };
    let dialog_height = 9u16 + error_rows;

    let dialog_area = centered_rect(dialog_width, dialog_height, area);

    f.render_widget(Clear, dialog_area);
    f.render_widget(
        Block::default().style(Style::default().bg(BG1)),
        dialog_area,
    );

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(error_rows),
            Constraint::Length(1),
            Constraint::Min(0),
        ])
        .split(dialog_area);

    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::raw("   "),
            Span::styled(
                "New project",
                Style::default().fg(FG).add_modifier(Modifier::BOLD),
            ),
        ]))
        .style(Style::default().bg(BG1)),
        rows[1],
    );

    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::raw("   "),
            Span::styled("Name", Style::default().fg(GRAY)),
            Span::raw("  "),
            Span::styled(
                if state.name.is_empty() {
                    "type a project name..."
                } else {
                    state.name.as_str()
                },
                if state.name.is_empty() {
                    Style::default().fg(BG2)
                } else {
                    Style::default().fg(FG)
                },
            ),
        ]))
        .style(Style::default().bg(BG1)),
        rows[3],
    );

    if let Some(error) = &state.error {
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::raw("   "),
                Span::styled(error, Style::default().fg(RED)),
            ]))
            .style(Style::default().bg(BG1)),
            rows[5],
        );
    }

    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::raw("   "),
            Span::styled(
                " Create ",
                Style::default()
                    .bg(BLUE)
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
        rows[6],
    );

    let cursor_x = (rows[3].x + 10 + state.name.len() as u16)
        .min(dialog_area.x + dialog_area.width.saturating_sub(1));
    f.set_cursor_position((cursor_x, rows[3].y));
}

fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    }
}

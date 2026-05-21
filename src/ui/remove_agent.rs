use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Clear, Paragraph},
    Frame,
};

use crate::ui::theme::*;

pub fn render_remove_agent(f: &mut Frame, area: Rect, agent_name: &str) {
    let dialog_width = 56u16.min(area.width.saturating_sub(4));
    // blank + title + blank + question + blank + buttons + blank
    let dialog_height = 7u16;

    let dialog_x = area.x + (area.width.saturating_sub(dialog_width)) / 2;
    let dialog_y = area.y + (area.height.saturating_sub(dialog_height)) / 2;

    let dialog_area = Rect {
        x: dialog_x,
        y: dialog_y,
        width: dialog_width,
        height: dialog_height,
    };

    // Clear + BG1 fill, no border
    f.render_widget(Clear, dialog_area);
    f.render_widget(
        Block::default().style(Style::default().bg(BG1)),
        dialog_area,
    );

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // blank
            Constraint::Length(1), // title
            Constraint::Length(1), // blank
            Constraint::Length(1), // question
            Constraint::Length(1), // blank
            Constraint::Length(1), // buttons
            Constraint::Length(1), // blank
        ])
        .split(dialog_area);

    // Title
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::raw("  "),
            Span::styled(
                "Remove agent",
                Style::default().fg(RED).add_modifier(Modifier::BOLD),
            ),
        ]))
        .style(Style::default().bg(BG1)),
        rows[1],
    );

    // Question
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::raw("  "),
            Span::styled("Remove ", Style::default().fg(GRAY)),
            Span::styled(
                agent_name,
                Style::default().fg(FG).add_modifier(Modifier::BOLD),
            ),
            Span::styled("?", Style::default().fg(GRAY)),
        ]))
        .style(Style::default().bg(BG1)),
        rows[3],
    );

    // Buttons — solid rectangles with gray tip text
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::raw("  "),
            Span::styled(
                " Confirm ",
                Style::default().bg(RED).fg(FG).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" y / enter", Style::default().fg(GRAY)),
            Span::raw("   "),
            Span::styled(
                " Cancel ",
                Style::default().bg(BG2).fg(FG).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" n / esc", Style::default().fg(GRAY)),
        ]))
        .style(Style::default().bg(BG1)),
        rows[5],
    );
}

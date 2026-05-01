use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
    Frame,
};

use ratatui::layout::Rect;

use crate::ui::theme::*;

pub fn render_remove_agent(f: &mut Frame, area: Rect, agent_name: &str) {
    // 2 content rows + 1 gap + 2 padding rows + 2 border rows = 7 total
    let dialog_width = 62u16.min(area.width.saturating_sub(4));
    let dialog_height = 7u16;

    let dialog_x = area.x + (area.width.saturating_sub(dialog_width)) / 2;
    let dialog_y = area.y + (area.height.saturating_sub(dialog_height)) / 2;

    let dialog_area = Rect {
        x: dialog_x,
        y: dialog_y,
        width: dialog_width,
        height: dialog_height,
    };

    f.render_widget(Clear, dialog_area);

    let block = Block::default()
        .title(Span::styled(
            " Remove Agent ",
            Style::default().fg(RED).add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(RED));

    let inner = block.inner(dialog_area);
    f.render_widget(block, dialog_area);

    // Layout: 1 padding + 1 question + 1 gap + 1 buttons + 1 padding
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // top padding
            Constraint::Length(1), // question
            Constraint::Length(1), // gap
            Constraint::Length(1), // buttons
            Constraint::Length(1), // bottom padding
        ])
        .split(inner);

    // Row 0: question
    let question = Line::from(vec![
        Span::styled("Remove agent ", Style::default().fg(GRAY)),
        Span::styled(
            agent_name,
            Style::default().fg(YELLOW).add_modifier(Modifier::BOLD),
        ),
        Span::styled("?", Style::default().fg(GRAY)),
    ]);
    f.render_widget(
        Paragraph::new(question).alignment(Alignment::Center),
        rows[1],
    );

    // Row 3 (index 3): action buttons
    let buttons = Line::from(vec![
        Span::styled("[", Style::default().fg(BG2)),
        Span::styled("y / Enter", Style::default().fg(GREEN)),
        Span::styled("]", Style::default().fg(BG2)),
        Span::styled(" confirm    ", Style::default().fg(GRAY)),
        Span::styled("[", Style::default().fg(BG2)),
        Span::styled("n / Esc", Style::default().fg(RED)),
        Span::styled("]", Style::default().fg(BG2)),
        Span::styled(" cancel", Style::default().fg(GRAY)),
    ]);
    f.render_widget(
        Paragraph::new(buttons).alignment(Alignment::Center),
        rows[3],
    );
}

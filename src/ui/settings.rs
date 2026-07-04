use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Clear, Paragraph},
};

use crate::app::SettingsState;
use crate::ui::theme::{Theme, builtin_themes};

pub fn render_settings(f: &mut Frame, area: Rect, theme: &Theme, state: &SettingsState) {
    let themes = builtin_themes();
    let dialog_width = ((area.width as u32 * 42 / 100) as u16)
        .max(42)
        .min(area.width.saturating_sub(4));
    let content_height = 8u16 + themes.len() as u16;
    let dialog_height = content_height.min(area.height.saturating_sub(2)).max(10);
    let dialog_area = centered_rect(dialog_width, dialog_height, area);

    f.render_widget(Clear, dialog_area);
    f.render_widget(
        Block::default().style(Style::default().bg(theme.bg1)),
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
            Constraint::Length(themes.len() as u16),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(dialog_area);

    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::raw("   "),
            Span::styled(
                "Settings",
                Style::default().fg(theme.fg).add_modifier(Modifier::BOLD),
            ),
        ]))
        .style(Style::default().bg(theme.bg1)),
        rows[1],
    );

    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::raw("   "),
            Span::styled("Theme", Style::default().fg(theme.gray)),
            Span::styled(
                "  preview updates immediately",
                Style::default().fg(theme.bg2),
            ),
        ]))
        .style(Style::default().bg(theme.bg1)),
        rows[3],
    );

    // Blank spacer after the section title.
    let list_rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints(vec![Constraint::Length(1); themes.len()])
        .split(rows[5]);
    for (idx, builtin) in themes.iter().enumerate() {
        let is_selected = idx == state.selected_idx;
        let marker = if is_selected { "●" } else { "○" };
        let label_style = if is_selected {
            Style::default().fg(theme.fg).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.gray)
        };
        let marker_style = if is_selected {
            Style::default().fg(theme.blue).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.bg2)
        };
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::raw("   "),
                Span::styled(format!("{marker} "), marker_style),
                Span::styled(builtin.label, label_style),
            ]))
            .style(Style::default().bg(theme.bg1)),
            list_rows[idx],
        );
    }

    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::raw("   "),
            Span::styled(
                " Save ",
                Style::default()
                    .bg(theme.blue)
                    .fg(theme.fg)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" enter", Style::default().fg(theme.gray)),
            Span::raw("   "),
            Span::styled(
                " Cancel ",
                Style::default()
                    .bg(theme.bg2)
                    .fg(theme.fg)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" esc", Style::default().fg(theme.gray)),
        ]))
        .style(Style::default().bg(theme.bg1)),
        rows[7],
    );
}

fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    }
}

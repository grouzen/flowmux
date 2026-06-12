use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Clear, Paragraph},
};

use crate::ui::theme::*;

pub fn render_remove_project(
    f: &mut Frame,
    area: Rect,
    project_name: &str,
    agent_count: usize,
    confirm_remove_agents: bool,
) {
    let dialog_width = ((area.width as u32 * 45 / 100) as u16)
        .max(52)
        .min(area.width.saturating_sub(4));
    let checkbox_rows: u16 = if agent_count > 0 { 2 } else { 0 };
    let dialog_height = 9u16 + checkbox_rows;

    let dialog_area = Rect {
        x: area.x + (area.width.saturating_sub(dialog_width)) / 2,
        y: area.y + (area.height.saturating_sub(dialog_height)) / 2,
        width: dialog_width,
        height: dialog_height,
    };

    f.render_widget(Clear, dialog_area);
    f.render_widget(
        Block::default().style(Style::default().bg(BG1)),
        dialog_area,
    );

    let mut constraints = vec![
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ];
    if agent_count > 0 {
        constraints.push(Constraint::Length(1));
        constraints.push(Constraint::Length(1));
    }
    constraints.push(Constraint::Length(1));
    constraints.push(Constraint::Min(0));

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(dialog_area);

    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::raw("   "),
            Span::styled(
                "Remove project",
                Style::default().fg(RED).add_modifier(Modifier::BOLD),
            ),
        ]))
        .style(Style::default().bg(BG1)),
        rows[1],
    );

    let summary = if agent_count == 0 {
        format!("Remove project {}?", project_name)
    } else {
        format!(
            "Remove project {} and {} agent(s)?",
            project_name, agent_count
        )
    };
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::raw("   "),
            Span::styled(summary, Style::default().fg(FG)),
        ]))
        .style(Style::default().bg(BG1)),
        rows[3],
    );

    if agent_count > 0 {
        let checkbox = if confirm_remove_agents { "[x]" } else { "[ ]" };
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::raw("   "),
                Span::styled(
                    checkbox,
                    if confirm_remove_agents {
                        Style::default().fg(ORANGE).add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(GRAY)
                    },
                ),
                Span::styled(
                    " Stop and remove all agents on this dashboard",
                    Style::default().fg(FG),
                ),
                Span::styled("  space", Style::default().fg(GRAY)),
            ]))
            .style(Style::default().bg(BG1)),
            rows[5],
        );
    }

    let confirm_style = if agent_count == 0 || confirm_remove_agents {
        Style::default().bg(RED).fg(FG).add_modifier(Modifier::BOLD)
    } else {
        Style::default().bg(BG2).fg(GRAY)
    };
    let actions_row = if agent_count > 0 { rows[7] } else { rows[5] };
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::raw("   "),
            Span::styled(" Remove ", confirm_style),
            Span::styled(" y / enter", Style::default().fg(GRAY)),
            Span::raw("   "),
            Span::styled(
                " Cancel ",
                Style::default().bg(BG2).fg(FG).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" n / esc", Style::default().fg(GRAY)),
        ]))
        .style(Style::default().bg(BG1)),
        actions_row,
    );
}

use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Clear, Paragraph},
    Frame,
};

use crate::ui::theme::*;

pub fn render_remove_agent(
    f: &mut Frame,
    area: Rect,
    agent_name: &str,
    has_worktree: bool,
    remove_worktree: bool,
) {
    let dialog_width = ((area.width as u32 * 40 / 100) as u16)
        .max(44)
        .min(area.width.saturating_sub(4));

    // blank + title + blank + question + [blank + worktree_checkbox] + blank + buttons + blank
    let worktree_rows: u16 = if has_worktree { 2 } else { 0 }; // blank + checkbox
    let dialog_height = 7u16 + worktree_rows;

    let dialog_x = area.x + (area.width.saturating_sub(dialog_width)) / 2;
    let dialog_y = area.y + (area.height.saturating_sub(dialog_height)) / 2;

    let dialog_area = Rect {
        x: dialog_x,
        y: dialog_y,
        width: dialog_width,
        height: dialog_height,
    };

    f.render_widget(Clear, dialog_area);
    f.render_widget(
        Block::default().style(Style::default().bg(BG1)),
        dialog_area,
    );

    let mut constraints = vec![
        Constraint::Length(1), // blank
        Constraint::Length(1), // title
        Constraint::Length(1), // blank
        Constraint::Length(1), // question
    ];
    if has_worktree {
        constraints.push(Constraint::Length(1)); // blank
        constraints.push(Constraint::Length(1)); // worktree checkbox
    }
    constraints.push(Constraint::Length(1)); // blank
    constraints.push(Constraint::Length(1)); // buttons
    constraints.push(Constraint::Length(1)); // blank

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(dialog_area);

    let mut row = 0usize;

    // blank
    row += 1;

    // Title
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::raw("   "),
            Span::styled(
                "Remove agent",
                Style::default().fg(RED).add_modifier(Modifier::BOLD),
            ),
        ]))
        .style(Style::default().bg(BG1)),
        rows[row],
    );
    row += 1;

    // blank
    row += 1;

    // Question
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::raw("   "),
            Span::styled("Remove ", Style::default().fg(GRAY)),
            Span::styled(
                agent_name,
                Style::default().fg(FG).add_modifier(Modifier::BOLD),
            ),
            Span::styled("?", Style::default().fg(GRAY)),
        ]))
        .style(Style::default().bg(BG1)),
        rows[row],
    );
    row += 1;

    // Worktree checkbox (only when agent has a worktree)
    if has_worktree {
        // blank
        row += 1;

        let checkbox = if remove_worktree { "[x]" } else { "[ ]" };
        let checkbox_style = if remove_worktree {
            Style::default().fg(ORANGE).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(GRAY)
        };
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::raw("   "),
                Span::styled(checkbox, checkbox_style),
                Span::styled(" Remove git worktree", Style::default().fg(FG)),
                Span::styled("  space", Style::default().fg(GRAY)),
            ]))
            .style(Style::default().bg(BG1)),
            rows[row],
        );
        row += 1;
    }

    // blank
    row += 1;

    // Buttons
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::raw("   "),
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
        rows[row],
    );
}

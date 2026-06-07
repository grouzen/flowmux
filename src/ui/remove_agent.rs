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
    stop_agent: bool,
    focus: usize,
) {
    let dialog_width = ((area.width as u32 * 40 / 100) as u16)
        .max(44)
        .min(area.width.saturating_sub(4));

    let worktree_rows: u16 = if has_worktree { 2 } else { 0 };
    let stop_rows: u16 = 2;
    let dialog_height = 7u16 + worktree_rows + stop_rows;

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
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ];
    if has_worktree {
        constraints.push(Constraint::Length(1));
        constraints.push(Constraint::Length(1));
    }
    constraints.push(Constraint::Length(1));
    constraints.push(Constraint::Length(1));
    constraints.push(Constraint::Length(1));

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(dialog_area);

    let mut row = 0usize;

    row += 1;

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

    row += 1;

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

    row += 1;

    let stop_checkbox = if stop_agent { "[x]" } else { "[ ]" };
    let stop_focus = focus == 0;
    let stop_label_style = if stop_focus {
        Style::default().fg(FG).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(GRAY)
    };
    let stop_checkbox_style = if stop_agent {
        if stop_focus {
            Style::default().fg(ORANGE).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(ORANGE)
        }
    } else {
        Style::default().fg(GRAY)
    };
    let stop_hint = Span::styled(
        "  space",
        Style::default().fg(if stop_focus { GRAY } else { BG2 }),
    );
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::raw("   "),
            Span::styled(stop_checkbox, stop_checkbox_style),
            Span::styled(" Stop agent", stop_label_style),
            stop_hint,
        ]))
        .style(Style::default().bg(BG1)),
        rows[row],
    );
    row += 1;

    if has_worktree {
        row += 1;

        let checkbox = if remove_worktree { "[x]" } else { "[ ]" };
        let wt_focus = focus == 1;
        let wt_label_style = if wt_focus {
            Style::default().fg(FG).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(GRAY)
        };
        let checkbox_style = if remove_worktree {
            if wt_focus {
                Style::default().fg(ORANGE).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(ORANGE)
            }
        } else {
            Style::default().fg(GRAY)
        };
        let wt_hint = Span::styled(
            "  space",
            Style::default().fg(if wt_focus { GRAY } else { BG2 }),
        );
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::raw("   "),
                Span::styled(checkbox, checkbox_style),
                Span::styled(" Remove git worktree", wt_label_style),
                wt_hint,
            ]))
            .style(Style::default().bg(BG1)),
            rows[row],
        );
        row += 1;
    }

    row += 1;

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

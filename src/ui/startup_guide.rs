use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Clear, Paragraph, Wrap},
};

use crate::app::StartupGuideState;
use crate::ui::theme::*;

const LEFT_PAD: &str = "   ";
const BULLET: &str = "•";
const PROGRESS_ROWS: u16 = 3;
const FOOTER_ROWS: u16 = 1;
const BOTTOM_ROWS: u16 = 1;

pub struct StartupGuidePage {
    pub title: &'static str,
    pub bullets: &'static [&'static str],
}

const STARTUP_GUIDE_PAGES: &[StartupGuidePage] = &[
    StartupGuidePage {
        title: "Welcome to Flowmux",
        bullets: &[
            "Flowmux is a tmux-backed dashboard for running multiple CLI agents at once.",
            "Each agent keeps its own working directory and terminal session.",
            "This guide walks through the main workflow before you start using the Flowmux.",
        ],
    },
    StartupGuidePage {
        title: "Projects and Agents",
        bullets: &[
            "Use projects to separate repos or workstreams.",
            "Each agent card belongs to one project and can be opened into a full terminal view.",
            "The dashboard is the overview.",
            "Individual agent views are where you interact with the real CLI session.",
        ],
    },
    StartupGuidePage {
        title: "Views",
        bullets: &[
            "Press Enter to open the live agent terminal.",
            "Press Ctrl+v to open git view for the selected agent directory.",
            "Press Ctrl+t to open a persistent shell in that same directory.",
            "Press Ctrl+g to return to the dashboard.",
        ],
    },
    StartupGuidePage {
        title: "Worktrees",
        bullets: &[
            "When you create an agent, you can place it in a git worktree for isolated branch work.",
            "This keeps parallel agent changes separated without leaving Flowmux.",
            "Worktree directory presets are remembered globally once you start using them.",
        ],
    },
    StartupGuidePage {
        title: "Git Viewer Configuration",
        bullets: &[
            "Flowmux runs the command from ~/.config/flowmux/config.toml when you enter git view.",
            "If git_viewer is unset or blank, Flowmux defaults to: git diff",
            "Example override: git_viewer = \"lazygit\"",
            "Commands with arguments also work, for example: git_viewer = \"lazydiff diff\"",
        ],
    },
    StartupGuidePage {
        title: "Persistence and Help",
        bullets: &[
            "Global settings live in ~/.config/flowmux/config.toml.",
            "Per-session state is stored under ~/.config/flowmux/sessions/ so dashboards reopen with their saved agents.",
            "Press ? on the dashboard to reopen this guide later.",
        ],
    },
];

pub fn render_startup_guide(f: &mut Frame, area: Rect, state: &StartupGuideState) {
    let dialog_area = startup_guide_dialog_area(area);
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
            Constraint::Min(0),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(dialog_area);

    let page = &STARTUP_GUIDE_PAGES[state.page.min(STARTUP_GUIDE_PAGES.len().saturating_sub(1))];
    let progress = format!("Step {} / {}", state.page + 1, STARTUP_GUIDE_PAGES.len());

    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::raw(LEFT_PAD),
            Span::styled(progress, Style::default().fg(GRAY)),
        ]))
        .style(Style::default().bg(BG1)),
        rows[1],
    );

    let content_area = rows[3];
    let inner_content = Rect {
        x: content_area.x.saturating_add(LEFT_PAD.len() as u16),
        y: content_area.y,
        width: content_area
            .width
            .saturating_sub((LEFT_PAD.len() as u16).saturating_mul(2)),
        height: content_area.height,
    };

    let page_text = startup_guide_page_text(page);
    f.render_widget(
        Paragraph::new(page_text)
            .style(Style::default().fg(FG).bg(BG1))
            .wrap(Wrap { trim: false }),
        inner_content,
    );

    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::raw(LEFT_PAD),
            Span::styled(
                " h / ← ",
                Style::default().fg(FG).bg(BG2).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" previous", Style::default().fg(FG)),
            Span::raw(" "),
            Span::styled(
                " l / → ",
                Style::default().fg(FG).bg(BG2).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" next", Style::default().fg(FG)),
            Span::raw(" "),
            Span::styled(
                " Enter ",
                Style::default().fg(FG).bg(BG2).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" close", Style::default().fg(FG)),
            Span::raw(" "),
            Span::styled(
                " Esc ",
                Style::default().fg(FG).bg(BG2).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" dismiss", Style::default().fg(FG)),
        ]))
        .style(Style::default().bg(BG1)),
        rows[4],
    );
}

pub fn startup_guide_dialog_area(area: Rect) -> Rect {
    let width = ((area.width as u32 * 40 / 100) as u16)
        .max(48)
        .min(area.width.saturating_sub(4));
    let height = startup_guide_dialog_height(area.height, width);
    Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    }
}

pub fn startup_guide_page_count() -> usize {
    STARTUP_GUIDE_PAGES.len()
}

fn startup_guide_dialog_height(term_height: u16, dialog_width: u16) -> u16 {
    let content_width = dialog_width
        .saturating_sub((LEFT_PAD.len() as u16).saturating_mul(2))
        .max(1);
    let longest_page_rows = STARTUP_GUIDE_PAGES
        .iter()
        .map(|page| startup_guide_page_line_count(page, content_width))
        .max()
        .unwrap_or(1);
    let height = PROGRESS_ROWS + longest_page_rows + FOOTER_ROWS + BOTTOM_ROWS;
    height.max(10).min(term_height.saturating_sub(4))
}

fn startup_guide_page_line_count(page: &StartupGuidePage, content_width: u16) -> u16 {
    wrapped_line_count(&startup_guide_page_text(page), content_width)
}

fn startup_guide_page_text(page: &StartupGuidePage) -> Text<'static> {
    let mut lines = Vec::with_capacity(page.bullets.len() * 2);
    lines.push(Line::from(vec![Span::styled(
        page.title.to_string(),
        Style::default().fg(FG).add_modifier(Modifier::BOLD),
    )]));
    lines.push(Line::from(""));
    for bullet in page.bullets {
        lines.push(Line::from(vec![
            Span::styled(BULLET, Style::default().fg(ORANGE)),
            Span::raw(" "),
            Span::styled((*bullet).to_string(), Style::default().fg(FG)),
        ]));
    }
    lines.push(Line::from(""));
    Text::from(lines)
}

fn wrapped_line_count(text: &Text<'_>, width: u16) -> u16 {
    if width == 0 {
        return 0;
    }
    let mut count: u16 = 0;
    for line in text.iter() {
        let line_width: usize = line
            .spans
            .iter()
            .map(|span| unicode_width::UnicodeWidthStr::width(span.content.as_ref()))
            .sum();
        let rows = if line_width == 0 {
            1
        } else {
            ((line_width as u16).saturating_sub(1) / width) + 1
        };
        count = count.saturating_add(rows);
    }
    count
}

#[cfg(test)]
mod tests {
    use super::{
        STARTUP_GUIDE_PAGES, startup_guide_dialog_height, startup_guide_page_count,
        startup_guide_page_line_count, startup_guide_page_text,
    };

    #[test]
    fn startup_guide_has_multiple_pages() {
        assert!(startup_guide_page_count() > 1);
    }

    #[test]
    fn startup_guide_page_starts_with_blank_line_before_bullets() {
        let text = startup_guide_page_text(&STARTUP_GUIDE_PAGES[0]);
        assert!(text.lines[1].spans.is_empty());
    }

    #[test]
    fn startup_guide_height_uses_longest_page_content() {
        let content_width = 24u16;
        let longest_page = STARTUP_GUIDE_PAGES
            .iter()
            .map(|page| startup_guide_page_line_count(page, content_width))
            .max()
            .unwrap();

        let height = startup_guide_dialog_height(40, 30);

        assert_eq!(height, longest_page + 5);
    }
}

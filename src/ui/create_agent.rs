use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Clear, Paragraph},
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::app::{
    CreateAgentState, CreateField, MAX_DIR_VISIBLE, RelativeDirSelector, WorktreeBaseMode,
};
use crate::models::AgentType;
use crate::ui::theme::{ICON_AGENT, ICON_ERR, Theme};

// ---------------------------------------------------------------------------
// Layout constants
// ---------------------------------------------------------------------------

/// Left padding spaces (3 chars).
const LEFT_PAD: usize = 3;
/// Right padding spaces (3 chars).
const RIGHT_PAD: usize = 3;
/// Width of the full label prefix: LEFT_PAD + 10-char padded label name.
const LABEL_WIDTH: u16 = (LEFT_PAD + 10) as u16; // 13

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

pub fn render_create_agent(f: &mut Frame, area: Rect, theme: &Theme, state: &CreateAgentState) {
    // 40% of terminal width, minimum 48, leave at least 4 cols margin
    let modal_width = ((area.width as u32 * 40 / 100) as u16)
        .max(48)
        .min(area.width.saturating_sub(4));
    let error_lines = state
        .error
        .as_deref()
        .map(|err| wrap_error_lines(theme, err, modal_width))
        .unwrap_or_default();

    let visible_dir_rows = state.dir_matches.len().min(MAX_DIR_VISIBLE) as u16;
    let dir_section_rows: u16 = if state.dir_matches.is_empty() {
        0
    } else {
        1 + 1 + visible_dir_rows // blank + "Suggested" label + items
    };

    // Worktree checkbox: shown when the directory is inside a git repo.
    let worktree_rows: u16 = if state.git_repo_root.is_some() { 2 } else { 0 }; // blank + checkbox
    let branch_mode_rows: u16 = if state.worktree_selectors_visible() {
        2
    } else {
        0
    };
    let base_branch_matches_rows = state
        .worktree_base_branch_matches
        .len()
        .min(MAX_DIR_VISIBLE) as u16;
    let base_branch_rows: u16 = if state.worktree_branch_fields_visible() {
        1 + 1
            + if state.focus != CreateField::WorktreeBaseBranch
                || state.worktree_branch_selected()
                || state.worktree_base_branch_matches.is_empty()
            {
                0
            } else {
                1 + base_branch_matches_rows
            }
    } else {
        0
    };
    let copy_rows = selector_section_rows(
        &state.copy_directories,
        state.copy_directories_enabled,
        state.focus == CreateField::CopyDirectories,
        state.worktree_selectors_visible(),
    );
    let symlink_rows = selector_section_rows(
        &state.symlink_directories,
        state.symlink_directories_enabled,
        state.focus == CreateField::SymlinkDirectories,
        state.worktree_selectors_visible(),
    );
    let initialize_submodules_rows: u16 = if state.initialize_submodules_visible() {
        2
    } else {
        0
    };

    let agent_rows = state.available_types.len().max(1) as u16;
    let agent_label_row: u16 = 1;
    let error_rows = error_lines.len() as u16;
    let error_gap_rows: u16 = if error_rows > 0 { 1 } else { 0 };

    // blank + title + blank + Name + blank + Directory
    // + dir_section + worktree_rows + blank + [agent_label] + agent_rows + blank + [error] + buttons + blank
    let modal_height = 3
        + 1
        + 1
        + 1
        + dir_section_rows
        + worktree_rows
        + branch_mode_rows
        + base_branch_rows
        + copy_rows
        + symlink_rows
        + initialize_submodules_rows
        + 1
        + agent_label_row
        + agent_rows
        + 1
        + error_rows
        + error_gap_rows
        + 1
        + 1;

    let modal_area = centered_rect(modal_width, modal_height, area);

    f.render_widget(Clear, modal_area);
    f.render_widget(
        Block::default().style(Style::default().bg(theme.bg1)),
        modal_area,
    );

    // Build layout constraints
    let mut constraints: Vec<Constraint> = vec![
        Constraint::Length(1), // blank
        Constraint::Length(1), // title
        Constraint::Length(1), // blank
        Constraint::Length(1), // Name
        Constraint::Length(1), // blank
        Constraint::Length(1), // Directory
    ];
    if !state.dir_matches.is_empty() {
        constraints.push(Constraint::Length(1)); // blank gap
        constraints.push(Constraint::Length(1)); // "Suggested" label
        for _ in 0..visible_dir_rows {
            constraints.push(Constraint::Length(1));
        }
    }
    // Git worktree checkbox (shown when directory is inside a git repo)
    if state.git_repo_root.is_some() {
        constraints.push(Constraint::Length(1)); // blank gap
        constraints.push(Constraint::Length(1)); // checkbox
    }
    if state.worktree_selectors_visible() {
        constraints.push(Constraint::Length(1)); // blank gap
        constraints.push(Constraint::Length(1)); // branch-mode checkbox
    }
    if state.worktree_branch_fields_visible() {
        constraints.push(Constraint::Length(1)); // blank gap
        constraints.push(Constraint::Length(1)); // base branch input
        if state.focus == CreateField::WorktreeBaseBranch
            && !state.worktree_branch_selected()
            && !state.worktree_base_branch_matches.is_empty()
        {
            constraints.push(Constraint::Length(1)); // blank gap
            for _ in 0..base_branch_matches_rows {
                constraints.push(Constraint::Length(1));
            }
        }
    }
    push_selector_constraints(
        &mut constraints,
        &state.copy_directories,
        state.copy_directories_enabled,
        state.focus == CreateField::CopyDirectories,
        state.worktree_selectors_visible(),
    );
    push_selector_constraints(
        &mut constraints,
        &state.symlink_directories,
        state.symlink_directories_enabled,
        state.focus == CreateField::SymlinkDirectories,
        state.worktree_selectors_visible(),
    );
    if state.initialize_submodules_visible() {
        constraints.push(Constraint::Length(1)); // blank gap
        constraints.push(Constraint::Length(1)); // checkbox
    }
    constraints.push(Constraint::Length(1)); // blank
    constraints.push(Constraint::Length(1)); // "Agent" label
    for _ in 0..agent_rows {
        constraints.push(Constraint::Length(1));
    }
    constraints.push(Constraint::Length(1)); // blank
    if error_rows > 0 {
        constraints.push(Constraint::Length(error_rows));
        constraints.push(Constraint::Length(1)); // blank after error
    }
    constraints.push(Constraint::Length(1)); // buttons
    constraints.push(Constraint::Min(0)); // trailing padding

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(modal_area);

    let mut row = 0usize;

    // blank
    row += 1;

    // Title
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::raw(left_pad()),
            Span::styled(
                "Launch agent",
                Style::default().fg(theme.fg).add_modifier(Modifier::BOLD),
            ),
        ]))
        .style(Style::default().bg(theme.bg1)),
        rows[row],
    );
    row += 1;

    // blank
    row += 1;

    // Name input — cursor at end of typed text
    let name_focused = state.focus == CreateField::Name;
    render_field_row(
        f,
        rows[row],
        theme,
        "Name",
        &state.name,
        "type a name...",
        name_focused,
    );
    if name_focused {
        let val_width = rows[row].width.saturating_sub(LABEL_WIDTH + 3);
        let displayed_start = truncate_left_start(&state.name, val_width as usize);
        let cursor = previous_char_boundary(&state.name, state.name_cursor.min(state.name.len()));
        let cursor_offset = cursor.saturating_sub(displayed_start);
        let cursor_width = UnicodeWidthStr::width(&state.name[displayed_start..][..cursor_offset]);
        let cx = (rows[row].x + LABEL_WIDTH + cursor_width as u16)
            .min(modal_area.x + modal_area.width.saturating_sub(1));
        f.set_cursor_position((cx, rows[row].y));
    }
    row += 1;

    // blank
    row += 1;

    // Directory input — trailing slash only when `directory` is a valid existing dir
    let dir_focused = state.focus == CreateField::Directory;
    // For root "/", trimming slashes yields "" which is unusable; keep "/" as-is.
    let dir_base = if state.directory == "/" {
        "/"
    } else {
        state.directory.trim_end_matches('/')
    };
    let dir_display = if !state.dir_filter.is_empty() {
        // User is typing a filter: show base/filter (no trailing slash yet)
        if dir_base == "/" {
            format!("/{}", state.dir_filter)
        } else {
            format!("{}/{}", dir_base, state.dir_filter)
        }
    } else if std::path::Path::new(dir_base).is_dir() {
        // Confirmed existing directory: append trailing slash (but root already has one)
        if dir_base == "/" {
            "/".to_string()
        } else {
            format!("{}/", dir_base)
        }
    } else {
        // Unknown/empty path: show as-is
        dir_base.to_string()
    };
    render_field_row(
        f,
        rows[row],
        theme,
        "Directory",
        &dir_display,
        "type a path...",
        dir_focused,
    );
    if dir_focused {
        let val_width = rows[row].width.saturating_sub(LABEL_WIDTH + 3);
        let displayed_len = dir_display.len().min(val_width as usize) as u16;
        let cx = (rows[row].x + LABEL_WIDTH + displayed_len)
            .min(modal_area.x + modal_area.width.saturating_sub(1));
        f.set_cursor_position((cx, rows[row].y));
    }
    row += 1;

    // Directory suggestions section
    if !state.dir_matches.is_empty() {
        // blank gap
        row += 1;

        // "Suggested" label in cyan
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::raw(label_pad()),
                Span::styled("Suggested", Style::default().fg(theme.cyan)),
            ]))
            .style(Style::default().bg(theme.bg1)),
            rows[row],
        );
        row += 1;

        // Suggestion items — visible window only
        let total = state.dir_matches.len();
        let offset = state.dir_scroll_offset;
        let visible_count = visible_dir_rows as usize;
        let needs_scrollbar = total > MAX_DIR_VISIBLE;

        for vi in 0..visible_count {
            let abs_idx = offset + vi;
            let Some(suggestion) = state.dir_matches.get(abs_idx) else {
                break;
            };

            let selected = abs_idx == state.dir_selected_idx && dir_focused;
            // dir_matches already contains bare directory names
            let display = suggestion.as_str();

            // Scrollbar character for this row
            let scrollbar_char = if needs_scrollbar {
                scrollbar_char(vi, visible_count, offset, total)
            } else {
                ' '
            };

            // Content width = row width minus left pad (3) minus 1 for scrollbar column.
            // Prefix " ● " / "   " = 3 chars; name fills the rest.
            // Content width = row width minus 10-char pad minus 1 for scrollbar column.
            let content_width = rows[row].width.saturating_sub(11) as usize;
            let hint_width = if selected { 6 } else { 0 };
            let name_width = content_width.saturating_sub(3 + hint_width);

            let line = if selected {
                Line::from(vec![
                    Span::raw("          "),
                    Span::styled(
                        format!(" ● {:<width$}", display, width = name_width),
                        Style::default()
                            .fg(theme.bg)
                            .bg(theme.fg)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(" enter", Style::default().fg(theme.gray).bg(theme.bg1)),
                    Span::styled(
                        scrollbar_char.to_string(),
                        Style::default().fg(theme.bg2).bg(theme.bg1),
                    ),
                ])
            } else {
                Line::from(vec![
                    Span::raw("          "),
                    Span::styled(
                        format!("   {:<width$}", display, width = name_width),
                        Style::default().fg(theme.gray),
                    ),
                    Span::styled(
                        scrollbar_char.to_string(),
                        Style::default().fg(theme.bg2).bg(theme.bg1),
                    ),
                ])
            };
            f.render_widget(
                Paragraph::new(line).style(Style::default().bg(theme.bg1)),
                rows[row],
            );
            row += 1;
        }
    }

    // Git worktree checkbox
    if state.git_repo_root.is_some() {
        // blank gap
        row += 1;

        let wt_focused = state.focus == CreateField::CreateWorktree;
        let checkbox = if state.create_worktree { "[x]" } else { "[ ]" };
        let label_style = if wt_focused {
            Style::default().fg(theme.fg).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.gray)
        };
        let checkbox_style = if state.create_worktree {
            if wt_focused {
                Style::default().fg(theme.cyan).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.cyan)
            }
        } else {
            Style::default().fg(theme.gray)
        };
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::raw(label_pad()),
                Span::styled(checkbox, checkbox_style),
                Span::styled(" Create git worktree", label_style),
                Span::styled(
                    "  space",
                    Style::default().fg(if wt_focused { theme.gray } else { theme.bg2 }),
                ),
            ]))
            .style(Style::default().bg(theme.bg1)),
            rows[row],
        );
        row += 1;
    }

    if state.worktree_selectors_visible() {
        row += 1;

        let focused = state.focus == CreateField::WorktreeFromBranch;
        let enabled = state.worktree_base_mode == WorktreeBaseMode::Branch;
        let checkbox = if enabled { "[x]" } else { "[ ]" };
        let label_style = if focused {
            Style::default().fg(theme.fg).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.gray)
        };
        let checkbox_style = if enabled {
            if focused {
                Style::default().fg(theme.cyan).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.cyan)
            }
        } else {
            Style::default().fg(theme.gray)
        };
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::raw(label_pad()),
                Span::styled(checkbox, checkbox_style),
                Span::styled(" Start from branch", label_style),
                Span::styled(
                    "  space",
                    Style::default().fg(if focused { theme.gray } else { theme.bg2 }),
                ),
            ]))
            .style(Style::default().bg(theme.bg1)),
            rows[row],
        );
        row += 1;
    }

    if state.worktree_branch_fields_visible() {
        row += 1;

        let base_branch_focused = state.focus == CreateField::WorktreeBaseBranch;
        if state.worktree_branch_selected() {
            render_selector_value_row(
                f,
                rows[row],
                theme,
                state
                    .selected_worktree_base_branch
                    .as_deref()
                    .unwrap_or_default(),
                false,
                base_branch_focused,
                base_branch_focused,
            );
        } else {
            render_simple_field_row(
                f,
                rows[row],
                theme,
                &state.worktree_base_branch_filter.value,
                "origin/teammate-branch",
                base_branch_focused,
            );
            if base_branch_focused {
                let val_width = rows[row]
                    .width
                    .saturating_sub(LABEL_WIDTH)
                    .saturating_sub(RIGHT_PAD as u16);
                let displayed_start = truncate_left_start(
                    &state.worktree_base_branch_filter.value,
                    val_width as usize,
                );
                let cursor = previous_char_boundary(
                    &state.worktree_base_branch_filter.value,
                    state
                        .worktree_base_branch_filter
                        .cursor
                        .min(state.worktree_base_branch_filter.value.len()),
                );
                let cursor_offset = cursor.saturating_sub(displayed_start);
                let cursor_width = UnicodeWidthStr::width(
                    &state.worktree_base_branch_filter.value[displayed_start..][..cursor_offset],
                );
                let cx = (rows[row].x + LABEL_WIDTH + cursor_width as u16)
                    .min(modal_area.x + modal_area.width.saturating_sub(1));
                f.set_cursor_position((cx, rows[row].y));
            }
        }
        row += 1;

        if base_branch_focused
            && !state.worktree_branch_selected()
            && !state.worktree_base_branch_matches.is_empty()
        {
            row += 1;
            let total = state.worktree_base_branch_matches.len();
            let offset = state.worktree_base_branch_scroll_offset;
            let visible_count = base_branch_matches_rows as usize;
            let needs_scrollbar = total > MAX_DIR_VISIBLE;

            for vi in 0..visible_count {
                let abs_idx = offset + vi;
                let Some(branch) = state.worktree_base_branch_matches.get(abs_idx) else {
                    break;
                };

                let selected =
                    abs_idx == state.worktree_base_branch_selected_idx && base_branch_focused;
                let scrollbar_char = if needs_scrollbar {
                    scrollbar_char(vi, visible_count, offset, total)
                } else {
                    ' '
                };
                let content_width = rows[row].width.saturating_sub(11) as usize;
                let hint_width = if selected { 6 } else { 0 };
                let name_width = content_width.saturating_sub(3 + hint_width);

                let line = if selected {
                    Line::from(vec![
                        Span::raw("          "),
                        Span::styled(
                            format!(" ● {:<width$}", branch, width = name_width),
                            Style::default()
                                .fg(theme.bg)
                                .bg(theme.fg)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(" enter", Style::default().fg(theme.gray).bg(theme.bg1)),
                        Span::styled(
                            scrollbar_char.to_string(),
                            Style::default().fg(theme.bg2).bg(theme.bg1),
                        ),
                    ])
                } else {
                    Line::from(vec![
                        Span::raw("          "),
                        Span::styled(
                            format!("   {:<width$}", branch, width = name_width),
                            Style::default().fg(theme.gray),
                        ),
                        Span::styled(
                            scrollbar_char.to_string(),
                            Style::default().fg(theme.bg2).bg(theme.bg1),
                        ),
                    ])
                };
                f.render_widget(
                    Paragraph::new(line).style(Style::default().bg(theme.bg1)),
                    rows[row],
                );
                row += 1;
            }
        }
    }

    row = render_selector_section(
        f,
        &rows,
        row,
        theme,
        "Copy directories",
        state.copy_directories_enabled,
        &state.copy_directories,
        state.focus == CreateField::CopyDirectories,
        state.worktree_selectors_visible(),
    );
    row = render_selector_section(
        f,
        &rows,
        row,
        theme,
        "Symlink directories",
        state.symlink_directories_enabled,
        &state.symlink_directories,
        state.focus == CreateField::SymlinkDirectories,
        state.worktree_selectors_visible(),
    );
    if state.initialize_submodules_visible() {
        row += 1;

        let focused = state.focus == CreateField::InitializeSubmodules;
        let checkbox = if state.initialize_submodules {
            "[x]"
        } else {
            "[ ]"
        };
        let label_style = if focused {
            Style::default().fg(theme.fg).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.gray)
        };
        let checkbox_style = if state.initialize_submodules {
            if focused {
                Style::default().fg(theme.cyan).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.cyan)
            }
        } else {
            Style::default().fg(theme.gray)
        };
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::raw(label_pad()),
                Span::styled(checkbox, checkbox_style),
                Span::styled(" Initialize submodules", label_style),
                Span::styled(
                    "  space",
                    Style::default().fg(if focused { theme.gray } else { theme.bg2 }),
                ),
            ]))
            .style(Style::default().bg(theme.bg1)),
            rows[row],
        );
        row += 1;
    }

    // blank (before agent section)
    row += 1;
    let agent_focused = state.focus == CreateField::AgentType;
    let label_style = if agent_focused {
        Style::default().fg(theme.fg)
    } else {
        Style::default().fg(theme.gray)
    };
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::raw(left_pad()),
            Span::styled("Agent", label_style),
        ]))
        .style(Style::default().bg(theme.bg1)),
        rows[row],
    );
    row += 1;

    render_agent_type_list(f, &rows[row..row + agent_rows as usize], theme, state);
    row += agent_rows as usize;

    // blank
    row += 1;

    // Error row
    if !error_lines.is_empty() {
        f.render_widget(
            Paragraph::new(Text::from(error_lines)).style(Style::default().bg(theme.bg1)),
            rows[row],
        );
        row += 1;
        row += 1;
    }

    // Buttons: solid rect + gray tip text
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::raw(left_pad()),
            Span::styled(
                " Launch ",
                Style::default()
                    .bg(theme.orange)
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
        AgentType::Codex => "codex",
    }
}

fn render_agent_type_list(f: &mut Frame, areas: &[Rect], theme: &Theme, state: &CreateAgentState) {
    let focused = state.focus == CreateField::AgentType;
    let types = &state.available_types;

    if types.is_empty() {
        if let Some(area) = areas.first() {
            f.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::raw(label_pad()),
                    Span::styled(
                        format!("{} opencode", ICON_AGENT),
                        Style::default().fg(theme.green),
                    ),
                ]))
                .style(Style::default().bg(theme.bg1)),
                *area,
            );
        }
        return;
    }

    for (i, t) in types.iter().enumerate() {
        let Some(area) = areas.get(i) else { break };
        let selected = i == state.selected_type_idx;
        let radio = if selected { "◉" } else { "○" };

        let style = if selected && focused {
            Style::default()
                .fg(theme.green)
                .add_modifier(Modifier::BOLD)
        } else if selected {
            Style::default().fg(theme.green)
        } else {
            Style::default().fg(theme.gray)
        };

        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::raw(label_pad()),
                Span::styled(format!("{} {}", radio, agent_type_label(t)), style),
            ]))
            .style(Style::default().bg(theme.bg1)),
            *area,
        );
    }
}

// ---------------------------------------------------------------------------
// Input field renderer
// ---------------------------------------------------------------------------

fn render_field_row(
    f: &mut Frame,
    area: Rect,
    theme: &Theme,
    label: &str,
    value: &str,
    placeholder: &str,
    focused: bool,
) {
    let val_width = area
        .width
        .saturating_sub(LABEL_WIDTH)
        .saturating_sub(RIGHT_PAD as u16);
    let displayed = truncate_left(value, val_width as usize);
    let label_text = format!("{}{:<10}", left_pad(), label);

    let spans: Vec<Span> = if focused {
        vec![
            Span::styled(label_text, Style::default().fg(theme.fg)),
            Span::styled(
                displayed,
                Style::default().fg(theme.fg).add_modifier(Modifier::BOLD),
            ),
        ]
    } else if value.is_empty() {
        vec![
            Span::styled(label_text, Style::default().fg(theme.gray)),
            Span::styled(placeholder, Style::default().fg(theme.bg2)),
        ]
    } else {
        vec![
            Span::styled(label_text, Style::default().fg(theme.gray)),
            Span::styled(displayed, Style::default().fg(theme.gray)),
        ]
    };

    f.render_widget(
        Paragraph::new(Line::from(spans)).style(Style::default().bg(theme.bg1)),
        area,
    );
}

fn render_simple_field_row(
    f: &mut Frame,
    area: Rect,
    theme: &Theme,
    value: &str,
    placeholder: &str,
    focused: bool,
) {
    let val_width = area
        .width
        .saturating_sub(LABEL_WIDTH)
        .saturating_sub(RIGHT_PAD as u16);
    let displayed = truncate_left(value, val_width as usize);

    let spans: Vec<Span> = if focused {
        vec![
            Span::raw(label_pad()),
            Span::styled(
                displayed,
                Style::default().fg(theme.fg).add_modifier(Modifier::BOLD),
            ),
        ]
    } else if value.is_empty() {
        vec![
            Span::raw(label_pad()),
            Span::styled(placeholder, Style::default().fg(theme.bg2)),
        ]
    } else {
        vec![
            Span::raw(label_pad()),
            Span::styled(displayed, Style::default().fg(theme.gray)),
        ]
    };

    f.render_widget(
        Paragraph::new(Line::from(spans)).style(Style::default().bg(theme.bg1)),
        area,
    );
}

fn selector_section_rows(
    selector: &RelativeDirSelector,
    enabled: bool,
    focused: bool,
    visible: bool,
) -> u16 {
    if !visible {
        return 0;
    }

    if !enabled {
        return 2;
    }

    let selected_rows = selector.selected_dirs.len() as u16;
    let suggestion_rows = if focused && !selector.matches.is_empty() {
        1 + selector.matches.len().min(MAX_DIR_VISIBLE) as u16
    } else {
        0
    };

    1 + 1 + 1 + selected_rows + if focused { 1 } else { 0 } + suggestion_rows
}

fn push_selector_constraints(
    constraints: &mut Vec<Constraint>,
    selector: &RelativeDirSelector,
    enabled: bool,
    focused: bool,
    visible: bool,
) {
    if !visible {
        return;
    }

    constraints.push(Constraint::Length(1)); // blank gap
    constraints.push(Constraint::Length(1)); // checkbox row
    if enabled {
        constraints.push(Constraint::Length(1)); // blank gap after checkbox
        for _ in &selector.selected_dirs {
            constraints.push(Constraint::Length(1));
        }
        if focused {
            constraints.push(Constraint::Length(1)); // current candidate
        }
        if focused && !selector.matches.is_empty() {
            constraints.push(Constraint::Length(1)); // blank gap before suggestions
            for _ in 0..selector.matches.len().min(MAX_DIR_VISIBLE) {
                constraints.push(Constraint::Length(1));
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn render_selector_section(
    f: &mut Frame,
    rows: &[Rect],
    mut row: usize,
    theme: &Theme,
    label: &str,
    enabled: bool,
    selector: &RelativeDirSelector,
    focused: bool,
    visible: bool,
) -> usize {
    if !visible {
        return row;
    }

    row += 1;

    let checkbox = if enabled { "[x]" } else { "[ ]" };
    let checkbox_style = if enabled {
        if focused {
            Style::default().fg(theme.cyan).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.cyan)
        }
    } else {
        Style::default().fg(theme.gray)
    };
    let label_style = if focused {
        Style::default().fg(theme.fg).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.gray)
    };
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::raw(label_pad()),
            Span::styled(checkbox, checkbox_style),
            Span::styled(format!(" {label}"), label_style),
            Span::styled(
                "  space",
                Style::default().fg(if focused { theme.gray } else { theme.bg2 }),
            ),
        ]))
        .style(Style::default().bg(theme.bg1)),
        rows[row],
    );
    row += 1;

    if !enabled {
        return row;
    }

    row += 1;

    let last_selected_idx = selector.selected_dirs.len().saturating_sub(1);
    for (idx, selected) in selector.selected_dirs.iter().enumerate() {
        render_selector_value_row(
            f,
            rows[row],
            theme,
            &format!("./{selected}"),
            false,
            focused,
            focused && idx == last_selected_idx,
        );
        row += 1;
    }

    if focused {
        let candidate_display = if !selector.filter.is_empty() {
            format!("{}{}", selector.current_display(), selector.filter)
        } else {
            selector.current_display()
        };
        let candidate_row = rows[row];
        render_selector_value_row(
            f,
            candidate_row,
            theme,
            &candidate_display,
            true,
            focused,
            false,
        );
        let val_width = candidate_row.width.saturating_sub(LABEL_WIDTH + 3);
        let displayed_len = candidate_display.len().min(val_width as usize) as u16;
        let cx = (candidate_row.x + LABEL_WIDTH + displayed_len)
            .min(candidate_row.x + candidate_row.width.saturating_sub(1));
        f.set_cursor_position((cx, candidate_row.y));
        row += 1;
    }

    if focused && !selector.matches.is_empty() {
        row += 1;

        let total = selector.matches.len();
        let offset = selector.scroll_offset;
        let visible_count = total.min(MAX_DIR_VISIBLE);
        let needs_scrollbar = total > MAX_DIR_VISIBLE;

        for vi in 0..visible_count {
            let abs_idx = offset + vi;
            let Some(suggestion) = selector.matches.get(abs_idx) else {
                break;
            };
            let selected = abs_idx == selector.selected_idx;
            let scrollbar_char = if needs_scrollbar {
                scrollbar_char(vi, visible_count, offset, total)
            } else {
                ' '
            };
            let content_width = rows[row].width.saturating_sub(11) as usize;
            let hint_width = if selected { 6 } else { 0 };
            let name_width = content_width.saturating_sub(3 + hint_width);

            let line = if selected {
                Line::from(vec![
                    Span::raw("          "),
                    Span::styled(
                        format!(" ● {:<width$}", suggestion, width = name_width),
                        Style::default()
                            .fg(theme.bg)
                            .bg(theme.fg)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(" enter", Style::default().fg(theme.gray).bg(theme.bg1)),
                    Span::styled(
                        scrollbar_char.to_string(),
                        Style::default().fg(theme.bg2).bg(theme.bg1),
                    ),
                ])
            } else {
                Line::from(vec![
                    Span::raw("          "),
                    Span::styled(
                        format!("   {:<width$}", suggestion, width = name_width),
                        Style::default().fg(theme.gray),
                    ),
                    Span::styled(
                        scrollbar_char.to_string(),
                        Style::default().fg(theme.bg2).bg(theme.bg1),
                    ),
                ])
            };
            f.render_widget(
                Paragraph::new(line).style(Style::default().bg(theme.bg1)),
                rows[row],
            );
            row += 1;
        }
    }

    row
}

fn render_selector_value_row(
    f: &mut Frame,
    area: Rect,
    theme: &Theme,
    value: &str,
    is_current: bool,
    focused: bool,
    show_backspace_hint: bool,
) {
    let style = if is_current {
        if focused {
            Style::default().fg(theme.fg).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.fg)
        }
    } else {
        Style::default().fg(theme.gray)
    };

    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::raw(label_pad()),
            Span::styled(value.to_string(), style),
            Span::styled(
                if show_backspace_hint {
                    "  backspace"
                } else {
                    ""
                },
                Style::default().fg(theme.gray),
            ),
        ]))
        .style(Style::default().bg(theme.bg1)),
        area,
    );
}

// ---------------------------------------------------------------------------
// Scrollbar
// ---------------------------------------------------------------------------

/// Returns the scrollbar character to display at visible row `vi` given
/// the full list size `total` and current scroll `offset`.
fn scrollbar_char(vi: usize, visible: usize, offset: usize, total: usize) -> char {
    if total <= visible {
        return ' ';
    }
    // Thumb occupies proportional rows
    let thumb_size = (visible * visible / total).max(1);
    let max_offset = total - visible;
    let thumb_top = offset * (visible - thumb_size) / max_offset;
    let thumb_bot = thumb_top + thumb_size;
    if vi >= thumb_top && vi < thumb_bot {
        '█'
    } else {
        '░'
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// 3-space left padding string.
fn left_pad() -> &'static str {
    "   "
}

/// Padding that aligns content to the value column (LEFT_PAD + label width = 13 chars).
fn label_pad() -> &'static str {
    "             " // 13 spaces
}

fn wrap_error_lines(theme: &Theme, error: &str, modal_width: u16) -> Vec<Line<'static>> {
    let icon_prefix = format!("{}{} ", left_pad(), ICON_ERR);
    let icon_prefix_width = UnicodeWidthStr::width(icon_prefix.as_str());
    let indent = " ".repeat(icon_prefix_width);
    let content_width = usize::from(modal_width)
        .saturating_sub(icon_prefix_width)
        .saturating_sub(RIGHT_PAD)
        .max(1);
    let wrapped = wrap_text_lines(error, content_width);

    wrapped
        .into_iter()
        .enumerate()
        .map(|(idx, text)| {
            if idx == 0 {
                Line::from(vec![
                    Span::raw(left_pad()),
                    Span::styled(format!("{} ", ICON_ERR), Style::default().fg(theme.red)),
                    Span::styled(text, Style::default().fg(theme.red)),
                ])
            } else {
                Line::from(vec![
                    Span::raw(indent.clone()),
                    Span::styled(text, Style::default().fg(theme.red)),
                ])
            }
        })
        .collect()
}

fn wrap_text_lines(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();

    for raw_line in text.split('\n') {
        if raw_line.is_empty() {
            lines.push(String::new());
            continue;
        }

        let mut current = String::new();
        let mut current_width = 0;

        for ch in raw_line.chars() {
            let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
            if !current.is_empty() && current_width + ch_width > width {
                lines.push(current);
                current = String::new();
                current_width = 0;
            }
            current.push(ch);
            current_width += ch_width;
        }

        if current.is_empty() {
            lines.push(String::new());
        } else {
            lines.push(current);
        }
    }

    if lines.is_empty() {
        lines.push(String::new());
    }

    lines
}

fn truncate_left(s: &str, max: usize) -> String {
    let start = truncate_left_start(s, max);
    s[start..].to_string()
}

fn truncate_left_start(s: &str, max: usize) -> usize {
    if max == 0 {
        return s.len();
    }
    if s.len() <= max {
        0
    } else {
        previous_char_boundary(s, s.len() - max)
    }
}

fn previous_char_boundary(s: &str, mut idx: usize) -> usize {
    idx = idx.min(s.len());
    while idx > 0 && !s.is_char_boundary(idx) {
        idx -= 1;
    }
    idx
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

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal as RatatuiTerminal, backend::TestBackend};

    fn render_buffer(state: &CreateAgentState, width: u16, height: u16) -> ratatui::buffer::Buffer {
        let backend = TestBackend::new(width, height);
        let mut terminal = RatatuiTerminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                render_create_agent(
                    frame,
                    Rect::new(0, 0, width, height),
                    &crate::ui::theme::default_theme().theme,
                    state,
                )
            })
            .unwrap();
        terminal.backend().buffer().clone()
    }

    fn buffer_text(buffer: &ratatui::buffer::Buffer) -> String {
        let mut text = String::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                text.push_str(buffer.cell((x, y)).unwrap().symbol());
            }
            text.push('\n');
        }
        text
    }

    #[test]
    fn render_create_agent_shows_agent_label_with_single_available_type() {
        let state = CreateAgentState {
            name: "agent-1".into(),
            directory: "/tmp".into(),
            available_types: vec![AgentType::Codex],
            selected_type_idx: 0,
            ..CreateAgentState::default()
        };

        let buffer = render_buffer(&state, 80, 24);
        let text = buffer_text(&buffer);

        assert!(text.contains("Agent"));
        assert!(text.contains("◉ codex"));
    }

    #[test]
    fn render_create_agent_can_focus_single_available_type() {
        let state = CreateAgentState {
            name: "agent-1".into(),
            directory: "/tmp".into(),
            focus: CreateField::AgentType,
            available_types: vec![AgentType::Codex],
            selected_type_idx: 0,
            ..CreateAgentState::default()
        };

        let buffer = render_buffer(&state, 80, 24);
        let text = buffer_text(&buffer);

        assert!(text.lines().any(|line| line.contains("◉ codex")));
    }

    #[test]
    fn render_create_agent_shows_enter_hint_on_active_working_directory_row() {
        let state = CreateAgentState {
            name: "agent-1".into(),
            directory: "/tmp".into(),
            focus: CreateField::Directory,
            dir_matches: vec!["src".into()],
            dir_selected_idx: 0,
            ..CreateAgentState::default()
        };

        let buffer = render_buffer(&state, 80, 24);
        let text = buffer_text(&buffer);

        assert!(
            text.lines()
                .any(|line| line.contains("src") && line.contains("enter"))
        );
    }

    #[test]
    fn render_create_agent_shows_enter_hint_on_active_directory_selector_row() {
        let state = CreateAgentState {
            name: "agent-1".into(),
            directory: "/tmp".into(),
            focus: CreateField::CopyDirectories,
            git_repo_root: Some("/tmp".into()),
            create_worktree: true,
            copy_directories_enabled: true,
            copy_directories: RelativeDirSelector {
                matches: vec!["target".into()],
                selected_idx: 0,
                ..RelativeDirSelector::default()
            },
            ..CreateAgentState::default()
        };

        let buffer = render_buffer(&state, 80, 28);
        let text = buffer_text(&buffer);

        assert!(
            text.lines()
                .any(|line| line.contains("target") && line.contains("enter"))
        );
    }

    #[test]
    fn render_create_agent_shows_backspace_hint_on_last_selected_directory() {
        let state = CreateAgentState {
            name: "agent-1".into(),
            directory: "/tmp".into(),
            focus: CreateField::SymlinkDirectories,
            git_repo_root: Some("/tmp".into()),
            create_worktree: true,
            symlink_directories_enabled: true,
            symlink_directories: RelativeDirSelector {
                selected_dirs: vec!["target".into(), "vendor".into()],
                ..RelativeDirSelector::default()
            },
            ..CreateAgentState::default()
        };

        let buffer = render_buffer(&state, 90, 28);
        let text = buffer_text(&buffer);

        assert!(
            text.lines()
                .any(|line| line.contains("./vendor") && line.contains("backspace"))
        );
        assert!(
            !text
                .lines()
                .any(|line| line.contains("./target") && line.contains("backspace"))
        );
    }

    #[test]
    fn render_create_agent_shows_backspace_hint_for_selected_branch() {
        let state = CreateAgentState {
            name: "agent-1".into(),
            directory: "/tmp/repo".into(),
            focus: CreateField::WorktreeBaseBranch,
            git_repo_root: Some("/tmp/repo".into()),
            create_worktree: true,
            worktree_base_mode: WorktreeBaseMode::Branch,
            selected_worktree_base_branch: Some("origin/teammate".into()),
            ..CreateAgentState::default()
        };

        let buffer = render_buffer(&state, 90, 28);
        let text = buffer_text(&buffer);

        assert!(
            text.lines()
                .any(|line| line.contains("origin/teammate") && line.contains("backspace"))
        );
    }

    #[test]
    fn render_create_agent_wraps_long_errors_and_keeps_buttons_below() {
        let state = CreateAgentState {
            name: "agent-1".into(),
            directory: "/tmp".into(),
            error: Some(
                "Directory ./target/debug/generated-artifacts cannot be both copied and symlinked"
                    .into(),
            ),
            ..CreateAgentState::default()
        };

        let buffer = render_buffer(&state, 80, 24);
        let text = buffer_text(&buffer);
        let lines: Vec<&str> = text.lines().collect();
        let first_error_row = lines
            .iter()
            .position(|line| line.contains(ICON_ERR) && line.contains("Directory"))
            .unwrap();
        let second_error_row = lines
            .iter()
            .position(|line| line.contains("symlinked"))
            .unwrap();
        let buttons_row = lines
            .iter()
            .position(|line| line.contains("Launch") && line.contains("Cancel"))
            .unwrap();
        let spacer_row = lines.get(second_error_row + 1).copied().unwrap_or_default();

        assert!(second_error_row > first_error_row);
        assert!(spacer_row.trim().is_empty());
        assert!(buttons_row > second_error_row + 1);
        assert!(
            lines
                .iter()
                .any(|line| line.contains("both copied and symlinked"))
        );
    }

    #[test]
    fn render_create_agent_keeps_short_error_on_single_row() {
        let state = CreateAgentState {
            name: "agent-1".into(),
            directory: "/tmp".into(),
            error: Some("Project name is required".into()),
            ..CreateAgentState::default()
        };

        let buffer = render_buffer(&state, 80, 24);
        let text = buffer_text(&buffer);
        let lines: Vec<&str> = text.lines().collect();
        let error_rows = lines
            .iter()
            .filter(|line| line.contains("Project name is required"))
            .count();

        assert_eq!(error_rows, 1);
    }

    #[test]
    fn render_create_agent_without_error_keeps_buttons_on_baseline_row() {
        let state = CreateAgentState {
            name: "agent-1".into(),
            directory: "/tmp".into(),
            ..CreateAgentState::default()
        };

        let buffer = render_buffer(&state, 80, 24);
        let text = buffer_text(&buffer);
        let lines: Vec<&str> = text.lines().collect();
        let buttons_row = lines
            .iter()
            .position(|line| line.contains("Launch") && line.contains("Cancel"))
            .unwrap();

        assert_eq!(buttons_row, 16);
    }

    #[test]
    fn render_create_agent_shows_submodule_checkbox_only_when_visible() {
        let visible = CreateAgentState {
            name: "agent-1".into(),
            directory: "/tmp/repo".into(),
            git_repo_root: Some("/tmp/repo".into()),
            create_worktree: true,
            has_git_submodules: true,
            ..CreateAgentState::default()
        };
        let hidden_without_submodules = CreateAgentState {
            name: "agent-1".into(),
            directory: "/tmp/repo".into(),
            git_repo_root: Some("/tmp/repo".into()),
            create_worktree: true,
            has_git_submodules: false,
            ..CreateAgentState::default()
        };
        let hidden_without_worktree = CreateAgentState {
            name: "agent-1".into(),
            directory: "/tmp/repo".into(),
            git_repo_root: Some("/tmp/repo".into()),
            create_worktree: false,
            has_git_submodules: true,
            ..CreateAgentState::default()
        };

        assert!(buffer_text(&render_buffer(&visible, 80, 24)).contains("Initialize submodules"));
        assert!(
            !buffer_text(&render_buffer(&hidden_without_submodules, 80, 24))
                .contains("Initialize submodules")
        );
        assert!(
            !buffer_text(&render_buffer(&hidden_without_worktree, 80, 24))
                .contains("Initialize submodules")
        );
    }
}

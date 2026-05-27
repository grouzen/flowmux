use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::widgets::{Block, BorderType, Borders};
use ratatui::Frame;

use super::{
    ghostty_blank_symbol_for_width, ghostty_buffer_symbol_into, ghostty_cell_style, ghostty_color,
    CellWide, RenderState, RowCells, RowIterator, Terminal,
};

use crate::ui::theme::GRAY;

pub fn render_pane_content(
    ansi_bytes: &[u8],
    frame: &mut Frame,
    area: Rect,
    cursor: Option<(u16, u16)>,
) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(Style::default().fg(GRAY));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let base_style =
        extract_first_bg_color(ansi_bytes).map_or(Style::default(), |c| Style::default().bg(c));
    {
        let buf = frame.buffer_mut();
        for y in 0..inner.height {
            for x in 0..inner.width {
                let cell = &mut buf[(inner.x + x, inner.y + y)];
                cell.set_style(base_style);
            }
        }
    }

    let mut terminal = match Terminal::new(inner.width, inner.height, 0) {
        Ok(t) => t,
        Err(_) => return,
    };
    terminal.write(ansi_bytes);

    let mut render_state = match RenderState::new() {
        Ok(rs) => rs,
        Err(_) => return,
    };
    if render_state.update(&terminal).is_err() {
        return;
    }

    let colors = render_state.colors().ok();
    let resolved_bg = colors.map(|c| ghostty_color(c.background));

    let mut row_iterator = match RowIterator::new() {
        Ok(it) => it,
        Err(_) => return,
    };
    let mut row_cells = match RowCells::new() {
        Ok(rc) => rc,
        Err(_) => return,
    };

    {
        let buf = frame.buffer_mut();
        let mut rows = match render_state.populate_row_iterator(&mut row_iterator) {
            Ok(r) => r,
            Err(_) => return,
        };
        let mut grapheme_scratch = Vec::new();
        let mut symbol_scratch = String::new();
        let mut y = 0u16;
        while y < inner.height && rows.next() {
            let mut cells = match rows.populate_cells(&mut row_cells) {
                Ok(c) => c,
                Err(_) => break,
            };
            let mut x = 0u16;
            while x < inner.width && cells.next() {
                let wide = cells.wide().unwrap_or(CellWide::Narrow);
                let style = ghostty_cell_style(&cells, None, None, resolved_bg);
                let symbol = match ghostty_buffer_symbol_into(
                    &cells,
                    wide,
                    &mut grapheme_scratch,
                    &mut symbol_scratch,
                ) {
                    Ok(s) => s,
                    Err(_) => {
                        symbol_scratch.clear();
                        symbol_scratch.push_str(ghostty_blank_symbol_for_width(wide));
                        symbol_scratch.as_str()
                    }
                };
                let cell = &mut buf[(inner.x + x, inner.y + y)];
                cell.reset();
                cell.set_symbol(symbol);
                cell.set_style(style);
                x += 1;
            }
            y += 1;
        }
    }

    if let Some((cx, cy)) = cursor {
        if cx < inner.width && cy < inner.height {
            frame.set_cursor_position((inner.x + cx, inner.y + cy));
        }
    }
}

fn extract_first_bg_color(ansi: &[u8]) -> Option<ratatui::style::Color> {
    use ratatui::style::Color;
    let mut i = 0;
    while i < ansi.len() {
        if ansi[i] != 0x1b {
            i += 1;
            continue;
        }
        i += 1;
        if i >= ansi.len() || ansi[i] != b'[' {
            continue;
        }
        i += 1;
        let start = i;
        while i < ansi.len() && ansi[i] != b'm' && ansi[i] != 0x1b {
            i += 1;
        }
        if i >= ansi.len() || ansi[i] != b'm' {
            continue;
        }
        let params_bytes = &ansi[start..i];
        i += 1;

        let Ok(params_str) = std::str::from_utf8(params_bytes) else {
            continue;
        };
        let nums: Vec<u32> = params_str
            .split(';')
            .filter_map(|s| s.parse().ok())
            .collect();

        let mut j = 0;
        while j < nums.len() {
            match nums[j] {
                48 if j + 4 < nums.len() && nums[j + 1] == 2 => {
                    return Some(Color::Rgb(
                        nums[j + 2] as u8,
                        nums[j + 3] as u8,
                        nums[j + 4] as u8,
                    ));
                }
                48 if j + 2 < nums.len() && nums[j + 1] == 5 => {
                    return Some(Color::Indexed(nums[j + 2] as u8));
                }
                n @ 40..=47 => return Some(Color::Indexed((n - 40) as u8)),
                n @ 100..=107 => return Some(Color::Indexed((n - 100 + 8) as u8)),
                _ => {}
            }
            j += 1;
        }
    }
    None
}

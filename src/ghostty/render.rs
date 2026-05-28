use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::widgets::{Block, BorderType, Borders};
use ratatui::Frame;

use super::{
    ghostty_blank_symbol_for_width, ghostty_buffer_symbol_into, ghostty_cell_style, ghostty_color,
    ghostty_default_bg, ghostty_default_fg, ghostty_reset_cell, CellWide, RenderState, RowCells,
    RowIterator, Terminal,
};

use crate::terminal_theme::TerminalTheme;
use crate::ui::theme::GRAY;

pub fn render_pane_content(
    ansi_bytes: &[u8],
    frame: &mut Frame,
    area: Rect,
    cursor: Option<(u16, u16)>,
    host_theme: TerminalTheme,
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
    let default_bg = colors.and_then(|c| ghostty_default_bg(c.background, host_theme, None));
    let default_fg = colors.and_then(|c| ghostty_default_fg(c.foreground, host_theme, None));
    let resolved_fg = colors.map(|c| ghostty_color(c.foreground));
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
                let style =
                    ghostty_cell_style(&cells, default_fg, default_bg, resolved_fg, resolved_bg);
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
            while x < inner.width {
                let cell = &mut buf[(inner.x + x, inner.y + y)];
                ghostty_reset_cell(cell, default_fg, default_bg);
                x += 1;
            }
            y += 1;
        }
        while y < inner.height {
            for x in 0..inner.width {
                let cell = &mut buf[(inner.x + x, inner.y + y)];
                ghostty_reset_cell(cell, default_fg, default_bg);
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

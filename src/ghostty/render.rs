use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
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
    theme_bg: Option<Color>,
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

    // Determine the background color to use:
    // - If theme_bg is provided (git view, editor), use it
    // - Otherwise (agent view), extract the first explicit bg from ANSI (the app's theme)
    let bg_color = theme_bg.or_else(|| extract_first_bg_color(ansi_bytes));

    // Pre-fill ratatui buffer with the background color for cells ghostty doesn't touch
    let base_style = bg_color.map_or(Style::default(), |c| Style::default().bg(c));
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

    // Inject the background color via OSC 11 so ghostty uses it as the default
    // This ensures cells without explicit backgrounds use the correct theme color
    if let Some(Color::Rgb(r, g, b)) = bg_color {
        let osc_bg = format!("\x1b]11;rgb:{:02x}/{:02x}/{:02x}\x1b\\", r, g, b);
        terminal.write(osc_bg.as_bytes());
    }

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

/// Extract the first explicit background color from ANSI escape sequences.
/// This is typically the embedded app's theme background color.
fn extract_first_bg_color(ansi_bytes: &[u8]) -> Option<Color> {
    let mut i = 0;
    while i < ansi_bytes.len() {
        if ansi_bytes[i] == 0x1b && i + 1 < ansi_bytes.len() && ansi_bytes[i + 1] == b'[' {
            let mut j = i + 2;
            let mut params = Vec::new();
            let mut current_param = String::new();

            while j < ansi_bytes.len() && ansi_bytes[j] != b'm' {
                if ansi_bytes[j] == b';' {
                    if !current_param.is_empty() {
                        if let Ok(n) = current_param.parse::<u32>() {
                            params.push(n);
                        }
                    }
                    current_param.clear();
                } else if ansi_bytes[j].is_ascii_digit() {
                    current_param.push(ansi_bytes[j] as char);
                }
                j += 1;
            }

            if !current_param.is_empty() {
                if let Ok(n) = current_param.parse::<u32>() {
                    params.push(n);
                }
            }

            // Look for background color parameters
            for k in 0..params.len() {
                match params[k] {
                    48 if k + 4 < params.len() && params[k + 1] == 2 => {
                        // 48;2;r;g;b - RGB background
                        let r = params[k + 2] as u8;
                        let g = params[k + 3] as u8;
                        let b = params[k + 4] as u8;
                        return Some(Color::Rgb(r, g, b));
                    }
                    40..=47 => {
                        // Standard background colors (40-47)
                        // Map to common Gruvbox-like colors
                        let color = match params[k] {
                            40 => Color::Rgb(40, 40, 40),    // Black -> BG
                            41 => Color::Rgb(204, 36, 29),   // Red
                            42 => Color::Rgb(152, 151, 26),  // Green
                            43 => Color::Rgb(215, 153, 33),  // Yellow
                            44 => Color::Rgb(69, 133, 136),  // Blue
                            45 => Color::Rgb(177, 98, 134),  // Magenta
                            46 => Color::Rgb(104, 157, 106), // Cyan
                            47 => Color::Rgb(146, 131, 116), // White -> GRAY
                            _ => Color::Rgb(40, 40, 40),
                        };
                        return Some(color);
                    }
                    _ => {}
                }
            }

            i = j + 1;
        } else {
            i += 1;
        }
    }
    None
}

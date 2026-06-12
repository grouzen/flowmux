use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::widgets::{Block, BorderType, Borders};

use super::{
    CellIterator, CellWide, RenderState, RgbColor, RowIterator, Terminal, TerminalOptions,
    ghostty_blank_symbol_for_width, ghostty_buffer_symbol_into, ghostty_cell_style, ghostty_color,
    ghostty_reset_cell,
};

use crate::ui::theme::GRAY;

/// Count visible columns in an ANSI line, skipping escape sequences and
/// accounting for UTF-8 character widths.
fn count_visible_columns(line: &[u8]) -> usize {
    let mut cols = 0;
    let mut i = 0;
    let len = line.len();

    while i < len {
        let b = line[i];

        // Skip CSI sequences: \x1b[...m (or other final byte)
        if b == 0x1b && i + 1 < len && line[i + 1] == b'[' {
            i += 2;
            while i < len && !(0x40 <= line[i] && line[i] <= 0x7e) {
                i += 1;
            }
            if i < len {
                i += 1;
            }
            continue;
        }

        // Skip OSC sequences: \x1b]...\x07 or \x1b]...\x1b\\
        if b == 0x1b && i + 1 < len && line[i + 1] == b']' {
            i += 2;
            while i < len {
                if line[i] == 0x07 {
                    i += 1;
                    break;
                }
                if line[i] == 0x1b && i + 1 < len && line[i + 1] == b'\\' {
                    i += 2;
                    break;
                }
                i += 1;
            }
            continue;
        }

        // Skip other escape sequences: \x1b followed by any byte
        if b == 0x1b && i + 1 < len {
            i += 2;
            continue;
        }

        if b < 0x80 {
            cols += 1;
            i += 1;
        } else {
            let char_len = if (b & 0xe0) == 0xc0 {
                2
            } else if (b & 0xf0) == 0xe0 {
                3
            } else if (b & 0xf8) == 0xf0 {
                4
            } else {
                1
            };
            if i + char_len <= len {
                if let Ok(s) = std::str::from_utf8(&line[i..i + char_len]) {
                    if let Some(ch) = s.chars().next() {
                        cols += unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
                    }
                }
            }
            i += char_len;
        }
    }

    cols
}

/// Pad each line with spaces to fill the full width. The SGR state at the end
/// of each line will apply to the padding spaces, ensuring correct background
/// colors for cells that tmux capture-pane stripped (trailing spaces).
fn pad_ansi_lines_to_width(input: &[u8], width: u16) -> Vec<u8> {
    let width = width as usize;
    let mut output = Vec::with_capacity(input.len() + input.len() / 10);

    let lines = input.split(|&b| b == b'\n');
    let mut first = true;

    for line in lines {
        if !first {
            output.push(b'\r');
            output.push(b'\n');
        }
        first = false;

        // Strip trailing \r if present (we'll add it back with padding)
        let line = if line.last() == Some(&b'\r') {
            &line[..line.len() - 1]
        } else {
            line
        };

        output.extend_from_slice(line);

        // Pad with spaces to fill width
        let cols = count_visible_columns(line);
        if cols < width {
            let padding = width - cols;
            output.extend(std::iter::repeat(b' ').take(padding));
        }
    }

    output
}

pub fn render_pane_content(
    ansi_bytes: &[u8],
    frame: &mut Frame,
    area: Rect,
    cursor: Option<(u16, u16)>,
    host_fg: Option<(u8, u8, u8)>,
    host_bg: Option<(u8, u8, u8)>,
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

    let mut terminal = match Terminal::new(TerminalOptions {
        cols: inner.width,
        rows: inner.height,
        max_scrollback: 0,
    }) {
        Ok(t) => t,
        Err(_) => return,
    };

    // Set default colors from host terminal
    if let Some((r, g, b)) = host_fg {
        let _ = terminal.set_default_fg_color(Some(RgbColor { r, g, b }));
    }
    if let Some((r, g, b)) = host_bg {
        let _ = terminal.set_default_bg_color(Some(RgbColor { r, g, b }));
    }

    let padded_bytes = pad_ansi_lines_to_width(ansi_bytes, inner.width);
    terminal.vt_write(&padded_bytes);

    let mut render_state = match RenderState::new() {
        Ok(rs) => rs,
        Err(_) => return,
    };
    let snapshot = match render_state.update(&terminal) {
        Ok(snapshot) => snapshot,
        Err(_) => return,
    };

    let colors = snapshot.colors().ok();
    let default_fg = colors.map(|c| ghostty_color(c.foreground));
    let default_bg = colors.map(|c| ghostty_color(c.background));
    let resolved_bg = default_bg;

    let mut row_iterator = match RowIterator::new() {
        Ok(it) => it,
        Err(_) => return,
    };
    let mut cell_iterator = match CellIterator::new() {
        Ok(rc) => rc,
        Err(_) => return,
    };

    {
        let buf = frame.buffer_mut();
        let mut rows = match row_iterator.update(&snapshot) {
            Ok(r) => r,
            Err(_) => return,
        };
        let mut symbol_scratch = String::new();
        let mut y = 0u16;
        while y < inner.height && rows.next().is_some() {
            let mut cells = match cell_iterator.update(&rows) {
                Ok(c) => c,
                Err(_) => break,
            };
            let mut x = 0u16;
            while x < inner.width && cells.next().is_some() {
                let wide = cells
                    .raw_cell()
                    .and_then(|raw_cell| raw_cell.wide())
                    .unwrap_or(CellWide::Narrow);
                let style = ghostty_cell_style(&cells, default_fg, default_bg, resolved_bg);
                let symbol = match ghostty_buffer_symbol_into(&cells, wide, &mut symbol_scratch) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_count_visible_columns_ascii() {
        assert_eq!(count_visible_columns(b"hello"), 5);
        assert_eq!(count_visible_columns(b"hello world"), 11);
        assert_eq!(count_visible_columns(b""), 0);
    }

    #[test]
    fn test_count_visible_columns_with_csi() {
        // CSI sequences should be skipped
        assert_eq!(count_visible_columns(b"\x1b[31mhello\x1b[0m"), 5);
        assert_eq!(count_visible_columns(b"\x1b[38;2;255;0;0mred\x1b[0m"), 3);
        assert_eq!(
            count_visible_columns(b"\x1b[1m\x1b[3mbold italic\x1b[0m"),
            11
        );
    }

    #[test]
    fn test_count_visible_columns_with_osc() {
        // OSC sequences should be skipped
        assert_eq!(count_visible_columns(b"\x1b]0;title\x07hello"), 5);
        assert_eq!(count_visible_columns(b"\x1b]11;rgb:ff/00/00\x1b\\hello"), 5);
    }

    #[test]
    fn test_count_visible_columns_utf8() {
        assert_eq!(count_visible_columns("café".as_bytes()), 4);
        assert_eq!(count_visible_columns("中文".as_bytes()), 4);
        assert_eq!(count_visible_columns("😀".as_bytes()), 2);
    }

    #[test]
    fn test_pad_ansi_lines_single_line() {
        let input = b"hello";
        let output = pad_ansi_lines_to_width(input, 10);
        assert_eq!(output, b"hello     ");
    }

    #[test]
    fn test_pad_ansi_lines_multiple_lines() {
        let input = b"hi\r\nworld";
        let output = pad_ansi_lines_to_width(input, 8);
        assert_eq!(output, b"hi      \r\nworld   ");
    }

    #[test]
    fn test_pad_ansi_lines_with_sgr() {
        // SGR should be preserved and padding should come after
        let input = b"\x1b[31mred\x1b[0m";
        let output = pad_ansi_lines_to_width(input, 8);
        assert_eq!(output, b"\x1b[31mred\x1b[0m     ");
    }

    #[test]
    fn test_pad_ansi_lines_already_full_width() {
        let input = b"1234567890";
        let output = pad_ansi_lines_to_width(input, 10);
        assert_eq!(output, b"1234567890");
    }

    #[test]
    fn test_pad_ansi_lines_empty() {
        let input = b"";
        let output = pad_ansi_lines_to_width(input, 5);
        assert_eq!(output, b"     ");
    }
}

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, BorderType, Borders};

use super::{
    CellIterator, CellWide, RenderState, RgbColor, RowIterator, Terminal, TerminalOptions,
    ghostty_buffer_symbol_into, ghostty_cell_style, ghostty_reset_cell,
};

use crate::ui::theme::default_theme;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SelectionRange {
    pub start_col: u16,
    pub end_col: u16,
    pub start_row: u16,
    pub end_row: u16,
}

impl SelectionRange {
    pub fn new(anchor: (u16, u16), focus: (u16, u16)) -> Self {
        let ((start_col, start_row), (end_col, end_row)) =
            if (anchor.1, anchor.0) <= (focus.1, focus.0) {
                (anchor, focus)
            } else {
                (focus, anchor)
            };

        Self {
            start_col,
            end_col,
            start_row,
            end_row,
        }
    }

    pub fn contains(self, col: u16, row: u16) -> bool {
        if row < self.start_row || row > self.end_row {
            return false;
        }

        if self.start_row == self.end_row {
            return row == self.start_row && col >= self.start_col && col <= self.end_col;
        }

        if row == self.start_row {
            return col >= self.start_col;
        }
        if row == self.end_row {
            return col <= self.end_col;
        }

        true
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaneTextGrid {
    width: u16,
    height: u16,
    cells: Vec<String>,
    row_continuations: Vec<bool>,
}

impl PaneTextGrid {
    fn new(width: u16, height: u16) -> Self {
        let len = width as usize * height as usize;
        Self {
            width,
            height,
            cells: vec![String::from(" "); len],
            row_continuations: vec![false; height as usize],
        }
    }

    fn index(&self, col: u16, row: u16) -> Option<usize> {
        if col >= self.width || row >= self.height {
            return None;
        }
        Some(row as usize * self.width as usize + col as usize)
    }

    fn set(&mut self, col: u16, row: u16, symbol: &str) {
        if let Some(idx) = self.index(col, row) {
            self.cells[idx].clear();
            self.cells[idx].push_str(symbol);
        }
    }

    pub fn extract(&self, selection: SelectionRange) -> String {
        let mut lines = Vec::new();
        let last_row = self.height.saturating_sub(1);
        let last_col = self.width.saturating_sub(1);

        for row in selection.start_row..=selection.end_row.min(last_row) {
            let start_col = if row == selection.start_row {
                selection.start_col
            } else {
                0
            };
            let end_col = if row == selection.end_row {
                selection.end_col
            } else {
                last_col
            };

            let mut line = String::new();
            for col in start_col..=end_col.min(last_col) {
                if let Some(idx) = self.index(col, row) {
                    line.push_str(&self.cells[idx]);
                }
            }

            while line.ends_with(' ') {
                line.pop();
            }
            lines.push(line);
        }

        lines.join("\n")
    }

    pub fn extract_wrap_aware(&self, selection: SelectionRange) -> String {
        let mut text = String::new();
        let last_row = self.height.saturating_sub(1);
        let last_col = self.width.saturating_sub(1);

        for row in selection.start_row..=selection.end_row.min(last_row) {
            if row > selection.start_row
                && !self
                    .row_continuations
                    .get(row as usize)
                    .copied()
                    .unwrap_or(false)
            {
                text.push('\n');
            }

            let start_col = if row == selection.start_row {
                selection.start_col
            } else {
                0
            };
            let end_col = if row == selection.end_row {
                selection.end_col
            } else {
                last_col
            };

            let line_start = text.len();
            for col in start_col..=end_col.min(last_col) {
                if let Some(idx) = self.index(col, row) {
                    text.push_str(&self.cells[idx]);
                }
            }

            while text[line_start..].ends_with(' ') {
                text.pop();
            }
        }

        text
    }
}

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
            if i + char_len <= len
                && let Ok(s) = std::str::from_utf8(&line[i..i + char_len])
                && let Some(ch) = s.chars().next()
            {
                cols += unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
            }
            i += char_len;
        }
    }

    cols
}

/// Prepare a tmux `capture-pane` screen snapshot for replay into the offscreen
/// terminal.  Capture-pane has already converted output into physical rows, so
/// row separators must move directly to the next screen row instead of letting
/// a full-width row trigger terminal autowrap before `\r\n`.
fn prepare_captured_ansi_for_replay(input: &[u8], width: u16) -> Vec<u8> {
    let width = width as usize;
    let mut output = Vec::with_capacity(input.len() + input.len() / 10);

    for (row, line) in input.split(|&b| b == b'\n').enumerate() {
        if row > 0 {
            output.extend_from_slice(format!("\x1b[{};1H", row + 1).as_bytes());
        }

        let line = if line.last() == Some(&b'\r') {
            &line[..line.len() - 1]
        } else {
            line
        };

        output.extend_from_slice(line);

        let cols = count_visible_columns(line);
        if cols < width {
            output.extend(std::iter::repeat_n(b' ', width - cols));
        }
    }

    output
}

fn logical_row_continuations(input: &[u8], width: u16, height: u16) -> Vec<bool> {
    if width == 0 || height == 0 {
        return Vec::new();
    }

    let width = width as usize;
    let mut rows = Vec::new();
    for line in input.split(|&b| b == b'\n') {
        let line = if line.last() == Some(&b'\r') {
            &line[..line.len() - 1]
        } else {
            line
        };
        let cols = count_visible_columns(line);
        let row_count = cols.max(1).div_ceil(width);
        for visual_row in 0..row_count {
            rows.push(visual_row > 0);
        }
    }

    let height = height as usize;
    let start = rows.len().saturating_sub(height);
    let mut visible = rows[start..].to_vec();
    visible.resize(height, false);
    visible
}

fn prepare_logical_ansi_for_replay(input: &[u8], width: u16, height: u16) -> Vec<u8> {
    if width == 0 || height == 0 {
        return Vec::new();
    }

    let width = width as usize;
    let mut output = Vec::with_capacity(input.len() + input.len() / 10);
    let mut row = 0usize;

    for (line_idx, line) in input.split(|&b| b == b'\n').enumerate() {
        if line_idx > 0 {
            output.extend_from_slice(format!("\x1b[{};1H", row + 1).as_bytes());
        }

        let line = if line.last() == Some(&b'\r') {
            &line[..line.len() - 1]
        } else {
            line
        };

        output.extend_from_slice(line);
        let cols = count_visible_columns(line);
        row += cols.max(1).div_ceil(width);
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
    selection: Option<SelectionRange>,
) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(Style::default().fg(default_theme().theme.gray));

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

    let replay_bytes = prepare_captured_ansi_for_replay(ansi_bytes, inner.width);
    terminal.vt_write(&replay_bytes);

    let mut render_state = match RenderState::new() {
        Ok(rs) => rs,
        Err(_) => return,
    };
    let snapshot = match render_state.update(&terminal) {
        Ok(snapshot) => snapshot,
        Err(_) => return,
    };

    let colors = snapshot.colors().ok();
    let default_fg = colors
        .as_ref()
        .map(|c| ratatui::style::Color::Rgb(c.foreground.r, c.foreground.g, c.foreground.b));
    let default_bg = colors
        .as_ref()
        .map(|c| ratatui::style::Color::Rgb(c.background.r, c.background.g, c.background.b));
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
                let symbol =
                    ghostty_buffer_symbol_into(&cells, wide, &mut symbol_scratch).unwrap_or(" ");
                let cell = &mut buf[(inner.x + x, inner.y + y)];
                cell.reset();
                cell.set_symbol(symbol);
                cell.set_style(style);
                if selection.is_some_and(|range| range.contains(x, y)) {
                    cell.set_bg(Color::Rgb(69, 133, 136));
                    cell.set_fg(Color::Rgb(40, 40, 40));
                }
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

    if let Some((cx, cy)) = cursor
        && cx < inner.width
        && cy < inner.height
    {
        frame.set_cursor_position((inner.x + cx, inner.y + cy));
    }
}

pub fn pane_text_grid(ansi_bytes: &[u8], width: u16, height: u16) -> PaneTextGrid {
    pane_text_grid_with_replay(ansi_bytes, width, height, false)
}

pub fn pane_text_grid_for_copy(ansi_bytes: &[u8], width: u16, height: u16) -> PaneTextGrid {
    pane_text_grid_with_replay(ansi_bytes, width, height, true)
}

fn pane_text_grid_with_replay(
    ansi_bytes: &[u8],
    width: u16,
    height: u16,
    logical_replay: bool,
) -> PaneTextGrid {
    let mut grid = PaneTextGrid::new(width, height);
    if width == 0 || height == 0 {
        return grid;
    }
    if logical_replay {
        grid.row_continuations = logical_row_continuations(ansi_bytes, width, height);
    }

    let mut terminal = match Terminal::new(TerminalOptions {
        cols: width,
        rows: height,
        max_scrollback: 0,
    }) {
        Ok(t) => t,
        Err(_) => return grid,
    };

    let replay_bytes = if logical_replay {
        prepare_logical_ansi_for_replay(ansi_bytes, width, height)
    } else {
        prepare_captured_ansi_for_replay(ansi_bytes, width)
    };
    terminal.vt_write(&replay_bytes);

    let mut render_state = match RenderState::new() {
        Ok(rs) => rs,
        Err(_) => return grid,
    };
    let snapshot = match render_state.update(&terminal) {
        Ok(snapshot) => snapshot,
        Err(_) => return grid,
    };

    let mut row_iterator = match RowIterator::new() {
        Ok(it) => it,
        Err(_) => return grid,
    };
    let mut cell_iterator = match CellIterator::new() {
        Ok(rc) => rc,
        Err(_) => return grid,
    };

    let mut rows = match row_iterator.update(&snapshot) {
        Ok(r) => r,
        Err(_) => return grid,
    };
    let mut symbol_scratch = String::new();
    let mut y = 0u16;
    while y < height && rows.next().is_some() {
        let mut cells = match cell_iterator.update(&rows) {
            Ok(c) => c,
            Err(_) => break,
        };
        let mut x = 0u16;
        while x < width && cells.next().is_some() {
            let wide = cells
                .raw_cell()
                .and_then(|raw_cell| raw_cell.wide())
                .unwrap_or(CellWide::Narrow);
            let symbol =
                ghostty_buffer_symbol_into(&cells, wide, &mut symbol_scratch).unwrap_or(" ");
            grid.set(x, y, symbol);
            x += 1;
        }
        while x < width {
            grid.set(x, y, " ");
            x += 1;
        }
        y += 1;
    }

    grid
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ghostty::{GhosttyStyle, ResolvedCellStyle, Underline, ghostty_style_to_ratatui};
    use ratatui::{
        Terminal as RatatuiTerminal,
        backend::TestBackend,
        style::{Color, Modifier},
    };

    fn render_buffer(
        ansi_bytes: &[u8],
        width: u16,
        height: u16,
        host_fg: Option<(u8, u8, u8)>,
        host_bg: Option<(u8, u8, u8)>,
    ) -> ratatui::buffer::Buffer {
        let backend = TestBackend::new(width, height);
        let mut terminal = RatatuiTerminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                render_pane_content(
                    ansi_bytes,
                    frame,
                    Rect::new(0, 0, width, height),
                    None,
                    host_fg,
                    host_bg,
                    None,
                );
            })
            .unwrap();
        terminal.backend().buffer().clone()
    }

    fn render_buffer_sequence(
        frames: &[&[u8]],
        width: u16,
        height: u16,
        host_fg: Option<(u8, u8, u8)>,
        host_bg: Option<(u8, u8, u8)>,
    ) -> ratatui::buffer::Buffer {
        let backend = TestBackend::new(width, height);
        let mut terminal = RatatuiTerminal::new(backend).unwrap();
        for ansi_bytes in frames {
            terminal
                .draw(|frame| {
                    render_pane_content(
                        ansi_bytes,
                        frame,
                        Rect::new(0, 0, width, height),
                        None,
                        host_fg,
                        host_bg,
                        None,
                    );
                })
                .unwrap();
        }
        terminal.backend().buffer().clone()
    }

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
    fn test_prepare_captured_ansi_for_replay_single_line() {
        let input = b"hello";
        let output = prepare_captured_ansi_for_replay(input, 10);
        assert_eq!(output, b"hello     ");
    }

    #[test]
    fn test_prepare_captured_ansi_for_replay_multiple_lines() {
        let input = b"hi\r\nworld";
        let output = prepare_captured_ansi_for_replay(input, 8);
        assert_eq!(output, b"hi      \x1b[2;1Hworld   ");
    }

    #[test]
    fn test_prepare_captured_ansi_for_replay_with_sgr() {
        // SGR should be preserved and padding should come after
        let input = b"\x1b[31mred\x1b[0m";
        let output = prepare_captured_ansi_for_replay(input, 8);
        assert_eq!(output, b"\x1b[31mred\x1b[0m     ");
    }

    #[test]
    fn test_prepare_captured_ansi_for_replay_already_full_width() {
        let input = b"1234567890";
        let output = prepare_captured_ansi_for_replay(input, 10);
        assert_eq!(output, b"1234567890");
    }

    #[test]
    fn test_prepare_captured_ansi_for_replay_empty() {
        let input = b"";
        let output = prepare_captured_ansi_for_replay(input, 5);
        assert_eq!(output, b"     ");
    }

    #[test]
    fn selection_range_spans_rows_in_stream_order() {
        let range = SelectionRange::new((3, 2), (1, 0));
        assert!(range.contains(1, 0));
        assert!(range.contains(4, 0));
        assert!(range.contains(0, 1));
        assert!(range.contains(2, 2));
        assert!(!range.contains(0, 0));
        assert!(!range.contains(4, 2));
    }

    #[test]
    fn pane_text_grid_extracts_selection_and_trims_trailing_spaces() {
        let grid = pane_text_grid(b"alpha\r\nbeta ", 5, 2);
        let selection = SelectionRange::new((1, 0), (3, 1));
        assert_eq!(grid.extract(selection), "lpha\nbeta");
    }

    #[test]
    fn pane_text_grid_for_copy_joins_soft_wrapped_rows() {
        let grid = pane_text_grid_for_copy(b"abcdef", 3, 2);
        let selection = SelectionRange::new((0, 0), (2, 1));
        assert_eq!(grid.extract_wrap_aware(selection), "abcdef");
    }

    #[test]
    fn pane_text_grid_for_copy_preserves_hard_line_breaks() {
        let grid = pane_text_grid_for_copy(b"abc\ndef", 3, 2);
        let selection = SelectionRange::new((0, 0), (2, 1));
        assert_eq!(grid.extract_wrap_aware(selection), "abc\ndef");
    }

    #[test]
    fn pane_text_grid_for_copy_preserves_full_width_hard_line_breaks() {
        let grid = pane_text_grid_for_copy(b"1234\nabcd", 4, 2);
        let selection = SelectionRange::new((0, 0), (3, 1));
        assert_eq!(grid.extract_wrap_aware(selection), "1234\nabcd");
    }

    #[test]
    fn pane_text_grid_for_copy_joins_partial_soft_wrapped_selection() {
        let grid = pane_text_grid_for_copy(b"abcdef", 4, 2);
        let selection = SelectionRange::new((2, 0), (1, 1));
        assert_eq!(grid.extract_wrap_aware(selection), "cdef");
    }

    #[test]
    fn test_style_mapping_preserves_defaults_inverse_invisible_and_underline() {
        let style = ghostty_style_to_ratatui(
            ResolvedCellStyle {
                fg: None,
                bg: None,
                style: GhosttyStyle {
                    inverse: true,
                    invisible: true,
                    underline: Underline::Double,
                    bold: true,
                    italic: true,
                    faint: true,
                    blink: true,
                    strikethrough: true,
                    ..GhosttyStyle::default()
                },
            },
            Some(Color::Rgb(1, 2, 3)),
            Some(Color::Rgb(4, 5, 6)),
            Some(Color::Rgb(4, 5, 6)),
        );

        assert_eq!(style.fg, Some(Color::Rgb(4, 5, 6)));
        assert_eq!(style.bg, Some(Color::Rgb(4, 5, 6)));
        assert!(style.add_modifier.contains(Modifier::BOLD));
        assert!(style.add_modifier.contains(Modifier::ITALIC));
        assert!(style.add_modifier.contains(Modifier::DIM));
        assert!(style.add_modifier.contains(Modifier::SLOW_BLINK));
        assert!(style.add_modifier.contains(Modifier::UNDERLINED));
        assert!(style.add_modifier.contains(Modifier::CROSSED_OUT));
    }

    #[test]
    fn test_style_mapping_prefers_explicit_cell_colors() {
        let style = ghostty_style_to_ratatui(
            ResolvedCellStyle {
                fg: Some(Color::Rgb(10, 20, 30)),
                bg: Some(Color::Rgb(40, 50, 60)),
                style: GhosttyStyle::default(),
            },
            Some(Color::Rgb(1, 2, 3)),
            Some(Color::Rgb(4, 5, 6)),
            Some(Color::Rgb(4, 5, 6)),
        );

        assert_eq!(style.fg, Some(Color::Rgb(10, 20, 30)));
        assert_eq!(style.bg, Some(Color::Rgb(40, 50, 60)));
    }

    #[test]
    fn test_render_preserves_inverse_host_default_colors() {
        let buffer = render_buffer(b"\x1b[7mA\x1b[0m", 6, 3, Some((1, 2, 3)), Some((4, 5, 6)));
        let cell = &buffer[(1, 1)];
        assert_eq!(cell.symbol(), "A");
        assert_eq!(cell.fg, Color::Rgb(4, 5, 6));
        assert_eq!(cell.bg, Color::Rgb(1, 2, 3));
    }

    #[test]
    fn test_render_preserves_invisible_text_and_trailing_background() {
        let buffer = render_buffer(b"\x1b[41m\x1b[8mA", 7, 3, Some((1, 2, 3)), Some((4, 5, 6)));
        let cell = &buffer[(1, 1)];
        assert_eq!(cell.symbol(), "A");
        assert_eq!(cell.fg, cell.bg);
        assert_eq!(buffer[(2, 1)].bg, cell.bg);
    }

    #[test]
    fn test_render_preserves_wide_cell_tail_behavior() {
        let buffer = render_buffer("界".as_bytes(), 6, 3, None, None);
        assert_eq!(buffer[(1, 1)].symbol(), "界");
        assert_eq!(buffer[(2, 1)].symbol(), " ");
    }

    #[test]
    fn test_render_ascii_and_empty_cells() {
        let buffer = render_buffer(b"A", 6, 3, Some((1, 2, 3)), Some((4, 5, 6)));
        assert_eq!(buffer[(1, 1)].symbol(), "A");
        assert_eq!(buffer[(2, 1)].symbol(), " ");
        assert_eq!(buffer[(3, 1)].symbol(), " ");
        assert_eq!(buffer[(1, 1)].fg, Color::Rgb(1, 2, 3));
        assert_eq!(buffer[(1, 1)].bg, Color::Rgb(4, 5, 6));
        assert_eq!(buffer[(2, 1)].fg, Color::Rgb(1, 2, 3));
        assert_eq!(buffer[(2, 1)].bg, Color::Rgb(4, 5, 6));
    }

    #[test]
    fn test_render_handles_crlf_line_breaks() {
        let buffer = render_buffer(b"A\r\nB", 6, 4, None, None);
        assert_eq!(buffer[(1, 1)].symbol(), "A");
        assert_eq!(buffer[(1, 2)].symbol(), "B");
        assert_eq!(buffer[(2, 1)].symbol(), " ");
        assert_eq!(buffer[(2, 2)].symbol(), " ");
    }

    #[test]
    fn test_render_does_not_double_wrap_full_width_captured_rows() {
        let buffer = render_buffer(b"1234\r\nZ", 6, 5, None, None);
        assert_eq!(buffer[(1, 1)].symbol(), "1");
        assert_eq!(buffer[(4, 1)].symbol(), "4");
        assert_eq!(buffer[(1, 2)].symbol(), "Z");
        assert_eq!(buffer[(1, 3)].symbol(), " ");
    }

    #[test]
    fn test_render_cursor_matches_captured_row_after_full_width_row() {
        let backend = TestBackend::new(6, 5);
        let mut terminal = RatatuiTerminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                render_pane_content(
                    b"1234\r\nZ",
                    frame,
                    Rect::new(0, 0, 6, 5),
                    Some((0, 1)),
                    None,
                    None,
                    None,
                );
            })
            .unwrap();

        terminal.backend_mut().assert_cursor_position((1, 2));
    }

    #[test]
    fn test_render_falls_back_on_malformed_utf8() {
        let buffer = render_buffer(b"\xffX", 6, 3, None, None);
        assert_eq!(buffer[(1, 1)].symbol(), " ");
        assert_eq!(buffer[(2, 1)].symbol(), " ");
        assert_eq!(buffer[(3, 1)].symbol(), " ");
        assert_eq!(buffer[(4, 1)].symbol(), " ");
    }

    #[test]
    fn test_render_preserves_combining_graphemes() {
        let buffer = render_buffer("e\u{301}".as_bytes(), 6, 3, None, None);
        assert_eq!(buffer[(1, 1)].symbol(), "e\u{301}");
        assert_eq!(buffer[(2, 1)].symbol(), " ");
    }

    #[test]
    fn test_render_preserves_emoji_and_cjk_wide_cells() {
        let emoji = render_buffer("😀".as_bytes(), 6, 3, None, None);
        assert_eq!(emoji[(1, 1)].symbol(), "😀");
        assert_eq!(emoji[(2, 1)].symbol(), " ");

        let cjk = render_buffer("界".as_bytes(), 6, 3, None, None);
        assert_eq!(cjk[(1, 1)].symbol(), "界");
        assert_eq!(cjk[(2, 1)].symbol(), " ");
    }

    #[test]
    fn test_render_preserves_spacer_head_for_soft_wrapped_wide_cells() {
        let buffer = render_buffer("abc界".as_bytes(), 6, 4, None, None);
        assert_eq!(buffer[(1, 1)].symbol(), "a");
        assert_eq!(buffer[(2, 1)].symbol(), "b");
        assert_eq!(buffer[(3, 1)].symbol(), "c");
        assert_eq!(buffer[(4, 1)].symbol(), " ");
        assert_eq!(buffer[(1, 2)].symbol(), "界");
        assert_eq!(buffer[(2, 2)].symbol(), " ");
    }

    #[test]
    fn test_render_explicit_cell_colors_override_host_defaults() {
        let buffer = render_buffer(
            b"\x1b[38;2;10;20;30m\x1b[48;2;40;50;60mA\x1b[0m",
            6,
            3,
            Some((1, 2, 3)),
            Some((4, 5, 6)),
        );
        let cell = &buffer[(1, 1)];
        assert_eq!(cell.symbol(), "A");
        assert_eq!(cell.fg, Color::Rgb(10, 20, 30));
        assert_eq!(cell.bg, Color::Rgb(40, 50, 60));

        let trailing = &buffer[(2, 1)];
        assert_eq!(trailing.symbol(), " ");
        assert_eq!(trailing.fg, Color::Rgb(1, 2, 3));
        assert_eq!(trailing.bg, Color::Rgb(4, 5, 6));
    }

    #[test]
    fn test_render_trailing_spaces_keep_active_background_color() {
        let buffer = render_buffer(b"\x1b[41mA", 6, 3, None, None);
        assert_eq!(buffer[(1, 1)].symbol(), "A");
        assert_ne!(buffer[(1, 1)].bg, Color::Reset);
        assert_eq!(buffer[(2, 1)].symbol(), " ");
        assert_eq!(buffer[(2, 1)].bg, buffer[(1, 1)].bg);
        assert_eq!(buffer[(3, 1)].bg, buffer[(1, 1)].bg);
    }

    #[test]
    fn test_render_clears_stale_content_when_redrawing_shorter_rows() {
        let buffer = render_buffer_sequence(
            &[b"ABCD\r\nWXYZ", b"A"],
            6,
            4,
            Some((1, 2, 3)),
            Some((4, 5, 6)),
        );

        assert_eq!(buffer[(1, 1)].symbol(), "A");
        assert_eq!(buffer[(2, 1)].symbol(), " ");
        assert_eq!(buffer[(3, 1)].symbol(), " ");
        assert_eq!(buffer[(4, 1)].symbol(), " ");

        assert_eq!(buffer[(1, 2)].symbol(), " ");
        assert_eq!(buffer[(2, 2)].symbol(), " ");
        assert_eq!(buffer[(3, 2)].symbol(), " ");
        assert_eq!(buffer[(4, 2)].symbol(), " ");
        assert_eq!(buffer[(1, 2)].fg, Color::Rgb(1, 2, 3));
        assert_eq!(buffer[(1, 2)].bg, Color::Rgb(4, 5, 6));
    }
}

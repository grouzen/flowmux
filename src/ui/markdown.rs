//! Markdown response layout for dashboard cards.
//!
//! `tui-markdown` intentionally does not render GFM tables.  This module keeps
//! it for every other Markdown construct and substitutes table ranges with
//! styled, horizontally scrollable terminal tables.

use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Paragraph, Wrap},
};
use unicode_width::UnicodeWidthStr;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ResponseMetrics {
    pub content_height: u16,
    /// The widest table.  Ordinary prose always wraps to the viewport.
    pub content_width: u16,
}

enum ResponseBlock<'a> {
    Prose(Text<'a>),
    Table(Text<'static>),
}

impl ResponseBlock<'_> {
    fn height(&self, width: u16) -> u16 {
        match self {
            Self::Prose(text) => wrapped_line_count(text, width),
            Self::Table(text) => text.height().min(u16::MAX as usize) as u16,
        }
    }

    fn width(&self) -> u16 {
        match self {
            Self::Prose(_) => 0,
            Self::Table(text) => text.width().min(u16::MAX as usize) as u16,
        }
    }
}

/// Renders a model response into `area` and returns the complete virtual
/// content dimensions.  Only table blocks honor `horizontal_scroll`.
pub fn render_response(
    f: &mut Frame,
    markdown: &str,
    area: Rect,
    vertical_scroll: u16,
    horizontal_scroll: u16,
    style: Style,
) -> ResponseMetrics {
    if area.width == 0 || area.height == 0 {
        return ResponseMetrics::default();
    }

    let blocks = response_blocks(markdown);
    let metrics = metrics_for_blocks(&blocks, area.width);
    let vertical_scroll = vertical_scroll.min(metrics.content_height.saturating_sub(area.height));
    let horizontal_scroll = horizontal_scroll.min(metrics.content_width.saturating_sub(area.width));
    let content_end = vertical_scroll.saturating_add(area.height);
    let mut block_top = 0u16;

    for block in &blocks {
        let block_height = block.height(area.width);
        let block_bottom = block_top.saturating_add(block_height);
        let visible_top = block_top.max(vertical_scroll);
        let visible_bottom = block_bottom.min(content_end);
        if visible_top < visible_bottom {
            let block_area = Rect {
                x: area.x,
                y: area.y + visible_top.saturating_sub(vertical_scroll),
                width: area.width,
                height: visible_bottom.saturating_sub(visible_top),
            };
            let block_scroll = visible_top.saturating_sub(block_top);
            match block {
                ResponseBlock::Prose(text) => f.render_widget(
                    Paragraph::new(text.clone())
                        .style(style)
                        .wrap(Wrap { trim: false })
                        .scroll((block_scroll, 0)),
                    block_area,
                ),
                ResponseBlock::Table(text) => f.render_widget(
                    Paragraph::new(text.clone())
                        .style(style)
                        .scroll((block_scroll, horizontal_scroll)),
                    block_area,
                ),
            }
        }
        block_top = block_bottom;
    }

    metrics
}

#[cfg(test)]
fn response_metrics(markdown: &str, width: u16) -> ResponseMetrics {
    metrics_for_blocks(&response_blocks(markdown), width)
}

fn metrics_for_blocks(blocks: &[ResponseBlock<'_>], width: u16) -> ResponseMetrics {
    blocks
        .iter()
        .fold(ResponseMetrics::default(), |metrics, block| {
            ResponseMetrics {
                content_height: metrics.content_height.saturating_add(block.height(width)),
                content_width: metrics.content_width.max(block.width()),
            }
        })
}

fn wrapped_line_count(text: &Text<'_>, width: u16) -> u16 {
    if width == 0 {
        return 0;
    }
    text.iter().fold(0u16, |count, line| {
        let line_width: usize = line
            .spans
            .iter()
            .map(|span| UnicodeWidthStr::width(span.content.as_ref()))
            .sum();
        let rows = if line_width == 0 {
            1
        } else {
            ((line_width as u16).saturating_sub(1) / width) + 1
        };
        count.saturating_add(rows)
    })
}

fn response_blocks(input: &str) -> Vec<ResponseBlock<'_>> {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    let mut ranges = Vec::new();
    let mut table_start = None;

    for (event, range) in Parser::new_ext(input, options).into_offset_iter() {
        match event {
            Event::Start(Tag::Table(_)) => table_start = Some(range.start),
            Event::End(TagEnd::Table) => {
                if let Some(start) = table_start.take() {
                    ranges.push(start..range.end);
                }
            }
            _ => {}
        }
    }

    if ranges.is_empty() {
        return vec![ResponseBlock::Prose(tui_markdown::from_str(input))];
    }

    let mut blocks = Vec::with_capacity(ranges.len() * 2 + 1);
    let mut cursor = 0;
    for range in ranges {
        if cursor < range.start {
            blocks.push(ResponseBlock::Prose(tui_markdown::from_str(
                &input[cursor..range.start],
            )));
        }
        blocks.push(ResponseBlock::Table(render_table(&input[range.clone()])));
        cursor = range.end;
    }
    if cursor < input.len() {
        blocks.push(ResponseBlock::Prose(tui_markdown::from_str(
            &input[cursor..],
        )));
    }
    blocks
}

#[derive(Clone, Copy)]
enum ColumnAlignment {
    Left,
    Center,
    Right,
}

fn render_table(source: &str) -> Text<'static> {
    let rows: Vec<Vec<String>> = source
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(split_table_row)
        .collect();
    if rows.len() < 2 {
        return Text::raw(source.to_owned());
    }

    let header = &rows[0];
    let separators = &rows[1];
    let column_count = header.len();
    if column_count == 0 {
        return Text::raw(source.to_owned());
    }
    let alignments = (0..column_count)
        .map(|idx| {
            let separator = separators.get(idx).map(String::as_str).unwrap_or("").trim();
            match (separator.starts_with(':'), separator.ends_with(':')) {
                (true, true) => ColumnAlignment::Center,
                (_, true) => ColumnAlignment::Right,
                _ => ColumnAlignment::Left,
            }
        })
        .collect::<Vec<_>>();

    let mut table_rows = Vec::with_capacity(rows.len() - 1);
    table_rows.push(
        header
            .iter()
            .map(|cell| inline_spans(cell))
            .collect::<Vec<_>>(),
    );
    for row in rows.iter().skip(2) {
        table_rows.push(
            (0..column_count)
                .map(|idx| inline_spans(row.get(idx).map(String::as_str).unwrap_or("")))
                .collect(),
        );
    }

    let mut widths = vec![1usize; column_count];
    for row in &table_rows {
        for (idx, cell) in row.iter().enumerate() {
            widths[idx] = widths[idx].max(spans_width(cell));
        }
    }

    let mut lines = Vec::with_capacity(table_rows.len() + 3);
    lines.push(border_line('┌', '┬', '┐', &widths));
    for (row_idx, row) in table_rows.iter().enumerate() {
        lines.push(table_row(row, &widths, &alignments, row_idx == 0));
        if row_idx == 0 {
            lines.push(border_line('├', '┼', '┤', &widths));
        }
    }
    lines.push(border_line('└', '┴', '┘', &widths));
    Text::from(lines)
}

fn split_table_row(line: &str) -> Vec<String> {
    let line = line.trim();
    let line = line.strip_prefix('|').unwrap_or(line);
    let line = line.strip_suffix('|').unwrap_or(line);
    let mut cells = Vec::new();
    let mut cell = String::new();
    let mut escaped = false;
    for ch in line.chars() {
        if escaped {
            if ch != '|' {
                cell.push('\\');
            }
            cell.push(ch);
            escaped = false;
        } else if ch == '\\' {
            escaped = true;
        } else if ch == '|' {
            cells.push(cell.trim().to_owned());
            cell.clear();
        } else {
            cell.push(ch);
        }
    }
    if escaped {
        cell.push('\\');
    }
    cells.push(cell.trim().to_owned());
    cells
}

fn inline_spans(markdown: &str) -> Vec<Span<'static>> {
    let text = tui_markdown::from_str(markdown);
    let mut spans = Vec::new();
    for (line_idx, line) in text.iter().enumerate() {
        if line_idx > 0 {
            spans.push(Span::raw(" "));
        }
        spans.extend(
            line.spans
                .iter()
                .map(|span| Span::styled(span.content.to_string(), span.style)),
        );
    }
    spans
}

fn spans_width(spans: &[Span<'_>]) -> usize {
    spans
        .iter()
        .map(|span| UnicodeWidthStr::width(span.content.as_ref()))
        .sum()
}

fn border_line(left: char, middle: char, right: char, widths: &[usize]) -> Line<'static> {
    let mut text = String::from(left);
    for (idx, width) in widths.iter().enumerate() {
        text.push_str(&"─".repeat(width + 2));
        text.push(if idx + 1 == widths.len() {
            right
        } else {
            middle
        });
    }
    Line::from(text)
}

fn table_row(
    cells: &[Vec<Span<'static>>],
    widths: &[usize],
    alignments: &[ColumnAlignment],
    header: bool,
) -> Line<'static> {
    let mut spans = vec![Span::raw("│")];
    for (idx, width) in widths.iter().enumerate() {
        let cell = cells.get(idx).map(Vec::as_slice).unwrap_or(&[]);
        let content_width = spans_width(cell);
        let padding = width.saturating_sub(content_width);
        let (left_padding, right_padding) = match alignments[idx] {
            ColumnAlignment::Left => (0, padding),
            ColumnAlignment::Center => (padding / 2, padding - padding / 2),
            ColumnAlignment::Right => (padding, 0),
        };
        spans.push(Span::raw(format!(" {}", " ".repeat(left_padding))));
        spans.extend(cell.iter().cloned().map(|span| {
            if header {
                Span::styled(
                    span.content.to_string(),
                    span.style.add_modifier(Modifier::BOLD),
                )
            } else {
                span
            }
        }));
        spans.push(Span::raw(format!("{} │", " ".repeat(right_padding))));
    }
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal as RatatuiTerminal, backend::TestBackend};

    fn render_buffer(markdown: &str, width: u16, height: u16, horizontal_scroll: u16) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = RatatuiTerminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                render_response(
                    frame,
                    markdown,
                    Rect::new(0, 0, width, height),
                    0,
                    horizontal_scroll,
                    Style::default(),
                );
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let mut text = String::new();
        for y in 0..height {
            for x in 0..width {
                text.push_str(buffer.cell((x, y)).unwrap().symbol());
            }
            text.push('\n');
        }
        text
    }

    #[test]
    fn table_renderer_preserves_cells_and_alignment() {
        let table = render_table("| Name | Score |\n| :--- | ---: |\n| **Ada** | `42` |");
        let rendered = table
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>();
        assert_eq!(rendered[0], "┌──────┬───────┐");
        assert!(rendered.iter().any(|line| line.contains("Ada")));
        assert!(rendered.iter().any(|line| line.contains("42 │")));
    }

    #[test]
    fn response_metrics_only_exposes_table_overflow() {
        let markdown = "intro\n\n| One | Two |\n| --- | --- |\n| alpha | beta |";
        let metrics = response_metrics(markdown, 5);
        assert!(metrics.content_height > 4);
        assert!(metrics.content_width > 5);
    }

    #[test]
    fn escaped_pipes_stay_in_the_cell() {
        assert_eq!(split_table_row("| a\\|b | c |"), ["a|b", "c"]);
    }

    #[test]
    fn renderer_clips_wide_tables_using_the_horizontal_offset() {
        let markdown = "| First column | Second column |\n| --- | --- |\n| alpha | beta |";
        let initial = render_buffer(markdown, 12, 5, 0);
        let shifted = render_buffer(markdown, 12, 5, 2);
        assert!(initial.starts_with("┌"));
        assert!(!shifted.starts_with("┌"));
        assert!(shifted.contains("First"));
    }
}

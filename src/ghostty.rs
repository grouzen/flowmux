pub mod render;

pub use libghostty_vt::{
    Error, RenderState, Terminal, TerminalOptions,
    render::{CellIterator, RowIterator},
    screen::CellWide,
    style::{RgbColor, Style as GhosttyStyle, Underline},
};

use libghostty_vt::render::CellIteration;
use ratatui::style::{Color, Modifier, Style};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ResolvedCellStyle {
    pub fg: Option<Color>,
    pub bg: Option<Color>,
    pub style: GhosttyStyle,
}

pub fn ghostty_blank_symbol_for_width(wide: CellWide) -> &'static str {
    match wide {
        CellWide::Wide => "  ",
        CellWide::SpacerTail => "",
        CellWide::Narrow | CellWide::SpacerHead => " ",
    }
}

pub fn ghostty_buffer_symbol_into<'a>(
    cell: &CellIteration<'_, '_>,
    wide: CellWide,
    symbol_scratch: &'a mut String,
) -> Result<&'a str, Error> {
    use unicode_width::UnicodeWidthStr;

    symbol_scratch.clear();
    match wide {
        CellWide::SpacerTail => {}
        CellWide::SpacerHead => symbol_scratch.push(' '),
        CellWide::Narrow | CellWide::Wide => {
            cell.graphemes_utf8(symbol_scratch)?;
            if symbol_scratch.is_empty() {
                symbol_scratch.push(' ');
            }
        }
    }

    let expected_width = match wide {
        CellWide::Wide => 2,
        CellWide::Narrow | CellWide::SpacerHead => 1,
        CellWide::SpacerTail => 0,
    };
    let actual_width = symbol_scratch.width();
    if actual_width != expected_width && !(wide == CellWide::Narrow && actual_width == 2) {
        symbol_scratch.clear();
        symbol_scratch.push_str(ghostty_blank_symbol_for_width(wide));
    }

    Ok(symbol_scratch.as_str())
}

pub fn ghostty_reset_cell(
    cell: &mut ratatui::buffer::Cell,
    default_fg: Option<ratatui::style::Color>,
    default_bg: Option<ratatui::style::Color>,
) {
    cell.reset();
    cell.set_symbol(" ");
    if let Some(bg) = default_bg {
        cell.set_bg(bg);
    }
    if let Some(fg) = default_fg {
        cell.set_fg(fg);
    }
}

pub fn ghostty_cell_style(
    cell: &CellIteration<'_, '_>,
    default_fg: Option<ratatui::style::Color>,
    default_bg: Option<ratatui::style::Color>,
    resolved_bg: Option<ratatui::style::Color>,
) -> ratatui::style::Style {
    ghostty_style_to_ratatui(
        ResolvedCellStyle {
            fg: cell.fg_color().ok().flatten().map(ghostty_color),
            bg: cell.bg_color().ok().flatten().map(ghostty_color),
            style: cell.style().unwrap_or_default(),
        },
        default_fg,
        default_bg,
        resolved_bg,
    )
}

pub fn ghostty_style_to_ratatui(
    resolved: ResolvedCellStyle,
    default_fg: Option<Color>,
    default_bg: Option<Color>,
    resolved_bg: Option<Color>,
) -> Style {
    let mut fg = resolved.fg.or(default_fg);
    let mut bg = resolved.bg.or(default_bg);

    if resolved.style.invisible {
        fg = bg.or(default_bg);
    }
    if resolved.style.inverse {
        if bg.is_none() {
            bg = resolved_bg;
        }
        if fg.is_none() {
            fg = default_fg;
        }
        std::mem::swap(&mut fg, &mut bg);
    }

    let mut style = Style::default();
    if let Some(fg) = fg {
        style = style.fg(fg);
    }
    if let Some(bg) = bg {
        style = style.bg(bg);
    }

    let mut modifiers = Modifier::empty();
    if resolved.style.bold {
        modifiers |= Modifier::BOLD;
    }
    if resolved.style.italic {
        modifiers |= Modifier::ITALIC;
    }
    if resolved.style.faint {
        modifiers |= Modifier::DIM;
    }
    if resolved.style.blink {
        modifiers |= Modifier::SLOW_BLINK;
    }
    if resolved.style.underline != Underline::None {
        modifiers |= Modifier::UNDERLINED;
    }
    if resolved.style.strikethrough {
        modifiers |= Modifier::CROSSED_OUT;
    }

    style.add_modifier(modifiers)
}

pub fn ghostty_color(color: RgbColor) -> ratatui::style::Color {
    ratatui::style::Color::Rgb(color.r, color.g, color.b)
}

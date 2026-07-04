/// Gruvbox Dark colour palette and Unicode icon constants.
///
/// Import with `use crate::ui::theme::*;` or selectively as needed.
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

// ---------------------------------------------------------------------------
// Colours
// ---------------------------------------------------------------------------

/// Main background — darkest surface.
#[allow(dead_code)]
pub const BG: Color = Color::Rgb(40, 40, 40);
/// Elevated surface — card inner zones, subtle zone tints.
pub const BG1: Color = Color::Rgb(60, 56, 54);
/// Higher elevated surface — prompt block backgrounds, borders.
pub const BG2: Color = Color::Rgb(80, 73, 69);
/// Primary foreground — readable body text.
pub const FG: Color = Color::Rgb(235, 219, 178);
/// Secondary / muted text — labels, hints, dim info.
pub const GRAY: Color = Color::Rgb(146, 131, 116);
/// Red — stopped state, destructive actions.
pub const RED: Color = Color::Rgb(204, 36, 29);
/// Heart red — classic crimson used for heart symbols.
pub const HEART_RED: Color = Color::Rgb(220, 20, 60);
/// Green — running state, confirm actions.
pub const GREEN: Color = Color::Rgb(152, 151, 26);
/// Yellow — waiting state, focused input, prompt borders.
pub const YELLOW: Color = Color::Rgb(215, 153, 33);
/// Blue/teal — selected card border, scroll accents.
pub const BLUE: Color = Color::Rgb(69, 133, 136);
/// Orange — keybinding key highlights, modal borders.
pub const ORANGE: Color = Color::Rgb(214, 93, 14);
/// Cyan — idle state (turn complete, awaiting next prompt).
pub const CYAN: Color = Color::Rgb(104, 157, 106); // Gruvbox aqua

// ---------------------------------------------------------------------------
// Unicode icons  (single-width, no Nerd Fonts required)
// ---------------------------------------------------------------------------

/// U+2302 HOUSE — working directory.
pub const ICON_DIR: &str = "⌂";
/// U+2699 GEAR — agent type.
pub const ICON_AGENT: &str = "⚙";
/// U+25C6 BLACK DIAMOND — model name.
pub const ICON_MODEL: &str = "◆";
/// U+23F1 STOPWATCH — elapsed / work time.
pub const ICON_TIME: &str = "⏱";
/// U+25CF BLACK CIRCLE — running status.
pub const ICON_RUN: &str = "●";
/// U+23F8 PAUSE BUTTON — waiting status.
pub const ICON_WAIT: &str = "⏸";
/// U+25A0 BLACK SQUARE — stopped status.
pub const ICON_STOP: &str = "■";
/// U+2717 BALLOT X — error indicator.
pub const ICON_ERR: &str = "✗";
/// U+25CB WHITE CIRCLE — idle status.
pub const ICON_IDLE: &str = "○";
/// U+2261 IDENTICAL TO — context / token usage.
pub const ICON_CTX: &str = "≡";

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Formats a token count compactly: `1.2M`, `34k`, or the raw number.
pub fn format_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.0}k", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

/// Formats a millisecond duration as a human-readable uptime string.
pub fn format_uptime(ms: u64) -> String {
    let secs = ms / 1000;
    let hours = secs / 3600;
    let mins = (secs % 3600) / 60;
    if hours > 0 {
        format!("{}h {}m", hours, mins)
    } else if mins > 0 {
        format!("{}m", mins)
    } else if secs > 0 {
        format!("{}s", secs)
    } else {
        "0s".to_string()
    }
}

/// Returns the brand `Line` and its display width for use in status bar layouts.
///
/// Pass `dimmed = true` when the UI is in a dimmed (modal-overlay) state.
pub fn brand_line(dimmed: bool) -> (Line<'static>, u16) {
    let base = if dimmed {
        Style::default().add_modifier(Modifier::DIM)
    } else {
        Style::default()
    };
    let version = env!("CARGO_PKG_VERSION");
    let display_width =
        unicode_width::UnicodeWidthStr::width(format!(" ♥ Flowmux v{} ", version).as_str()) as u16;
    let line = Line::from(vec![
        Span::styled(" ♥ ", base.fg(HEART_RED)),
        Span::styled("Flowmux", base.fg(GRAY)),
        Span::styled(format!(" v{} ", version), base.fg(GRAY)),
    ]);
    (line, display_width)
}

/// Returns status count spans and their total display width.
///
/// When `blink_running` or `blink_waiting` is true, the corresponding status
/// field has its fg/bg colors swapped (blink effect).
pub fn status_count_spans(
    running: usize,
    waiting: usize,
    idle: usize,
    blink_running: bool,
    blink_waiting: bool,
    dimmed: bool,
) -> (Vec<Span<'static>>, u16) {
    let base = if dimmed {
        Style::default().add_modifier(Modifier::DIM)
    } else {
        Style::default()
    };

    let running_style = if blink_running {
        base.fg(BG).bg(GREEN)
    } else {
        base.fg(GREEN)
    };

    let waiting_style = if blink_waiting {
        base.fg(BG).bg(YELLOW)
    } else {
        base.fg(YELLOW)
    };

    let spans = vec![
        Span::styled(format!(" {} ", ICON_RUN), base.fg(GREEN)),
        Span::styled(format!("{} running", running), running_style),
        Span::styled(format!(" {} ", ICON_WAIT), base.fg(YELLOW)),
        Span::styled(format!("{} waiting", waiting), waiting_style),
        Span::styled(format!(" {} {} idle ", ICON_IDLE, idle), base.fg(CYAN)),
    ];

    let width = unicode_width::UnicodeWidthStr::width(
        format!(
            " {} {} running {} {} waiting {} {} idle ",
            ICON_RUN, running, ICON_WAIT, waiting, ICON_IDLE, idle
        )
        .as_str(),
    ) as u16;

    (spans, width)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_count_spans_only_blinks_running_text_and_count() {
        let (spans, _) = status_count_spans(3, 2, 1, true, false, false);

        assert_eq!(spans[0].content, format!(" {} ", ICON_RUN));
        assert_eq!(spans[1].content, "3 running");
        assert_eq!(spans[0].style.fg, Some(GREEN));
        assert_eq!(spans[0].style.bg, None);
        assert_eq!(spans[1].style.fg, Some(BG));
        assert_eq!(spans[1].style.bg, Some(GREEN));
    }

    #[test]
    fn status_count_spans_only_blinks_waiting_text_and_count() {
        let (spans, _) = status_count_spans(3, 2, 1, false, true, false);

        assert_eq!(spans[2].content, format!(" {} ", ICON_WAIT));
        assert_eq!(spans[3].content, "2 waiting");
        assert_eq!(spans[2].style.fg, Some(YELLOW));
        assert_eq!(spans[2].style.bg, None);
        assert_eq!(spans[3].style.fg, Some(BG));
        assert_eq!(spans[3].style.bg, Some(YELLOW));
    }
}

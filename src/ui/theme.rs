use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Theme {
    pub bg: Color,
    pub bg1: Color,
    pub bg2: Color,
    pub fg: Color,
    pub gray: Color,
    pub red: Color,
    pub heart_red: Color,
    pub green: Color,
    pub yellow: Color,
    pub blue: Color,
    pub orange: Color,
    pub cyan: Color,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuiltinTheme {
    pub id: &'static str,
    pub label: &'static str,
    pub theme: Theme,
}

pub const ICON_DIR: &str = "⌂";
pub const ICON_AGENT: &str = "⚙";
pub const ICON_MODEL: &str = "◆";
pub const ICON_TIME: &str = "⏱";
pub const ICON_RUN: &str = "●";
pub const ICON_WAIT: &str = "⏸";
pub const ICON_STOP: &str = "■";
pub const ICON_ERR: &str = "✗";
pub const ICON_IDLE: &str = "○";
pub const ICON_CTX: &str = "≡";

pub const GRUVBOX_DARK: BuiltinTheme = BuiltinTheme {
    id: "gruvbox-dark",
    label: "Gruvbox Dark",
    theme: Theme {
        bg: Color::Rgb(40, 40, 40),
        bg1: Color::Rgb(60, 56, 54),
        bg2: Color::Rgb(80, 73, 69),
        fg: Color::Rgb(235, 219, 178),
        gray: Color::Rgb(146, 131, 116),
        red: Color::Rgb(204, 36, 29),
        heart_red: Color::Rgb(220, 20, 60),
        green: Color::Rgb(152, 151, 26),
        yellow: Color::Rgb(215, 153, 33),
        blue: Color::Rgb(69, 133, 136),
        orange: Color::Rgb(214, 93, 14),
        cyan: Color::Rgb(104, 157, 106),
    },
};

pub const GRUVBOX_LIGHT: BuiltinTheme = BuiltinTheme {
    id: "gruvbox-light",
    label: "Nord",
    theme: Theme {
        bg: Color::Rgb(46, 52, 64),
        bg1: Color::Rgb(59, 66, 82),
        bg2: Color::Rgb(76, 86, 106),
        fg: Color::Rgb(236, 239, 244),
        gray: Color::Rgb(216, 222, 233),
        red: Color::Rgb(191, 97, 106),
        heart_red: Color::Rgb(191, 97, 106),
        green: Color::Rgb(163, 190, 140),
        yellow: Color::Rgb(235, 203, 139),
        blue: Color::Rgb(129, 161, 193),
        orange: Color::Rgb(208, 135, 112),
        cyan: Color::Rgb(143, 188, 187),
    },
};

pub const TOKYO_NIGHT: BuiltinTheme = BuiltinTheme {
    id: "tokyo-night",
    label: "Tokyo Night",
    theme: Theme {
        bg: Color::Rgb(26, 27, 38),
        bg1: Color::Rgb(36, 40, 59),
        bg2: Color::Rgb(65, 72, 104),
        fg: Color::Rgb(192, 202, 245),
        gray: Color::Rgb(122, 162, 247),
        red: Color::Rgb(247, 118, 142),
        heart_red: Color::Rgb(255, 99, 132),
        green: Color::Rgb(158, 206, 106),
        yellow: Color::Rgb(224, 175, 104),
        blue: Color::Rgb(125, 207, 255),
        orange: Color::Rgb(255, 158, 100),
        cyan: Color::Rgb(115, 218, 202),
    },
};

pub const SOLARIZED_DARK: BuiltinTheme = BuiltinTheme {
    id: "solarized-dark",
    label: "Solarized Dark",
    theme: Theme {
        bg: Color::Rgb(0, 43, 54),
        bg1: Color::Rgb(7, 54, 66),
        bg2: Color::Rgb(88, 110, 117),
        fg: Color::Rgb(238, 232, 213),
        gray: Color::Rgb(147, 161, 161),
        red: Color::Rgb(220, 50, 47),
        heart_red: Color::Rgb(220, 80, 80),
        green: Color::Rgb(133, 153, 0),
        yellow: Color::Rgb(181, 137, 0),
        blue: Color::Rgb(38, 139, 210),
        orange: Color::Rgb(203, 75, 22),
        cyan: Color::Rgb(42, 161, 152),
    },
};

pub const CATPPUCCIN_LATTE: BuiltinTheme = BuiltinTheme {
    id: "catppuccin-latte",
    label: "One Dark",
    theme: Theme {
        bg: Color::Rgb(40, 44, 52),
        bg1: Color::Rgb(49, 54, 63),
        bg2: Color::Rgb(61, 67, 79),
        fg: Color::Rgb(171, 178, 191),
        gray: Color::Rgb(146, 153, 166),
        red: Color::Rgb(224, 108, 117),
        heart_red: Color::Rgb(224, 108, 117),
        green: Color::Rgb(152, 195, 121),
        yellow: Color::Rgb(229, 192, 123),
        blue: Color::Rgb(97, 175, 239),
        orange: Color::Rgb(209, 154, 102),
        cyan: Color::Rgb(86, 182, 194),
    },
};

pub const BUILTIN_THEMES: [BuiltinTheme; 5] = [
    GRUVBOX_DARK,
    GRUVBOX_LIGHT,
    TOKYO_NIGHT,
    SOLARIZED_DARK,
    CATPPUCCIN_LATTE,
];

pub fn builtin_themes() -> &'static [BuiltinTheme] {
    &BUILTIN_THEMES
}

pub fn default_theme() -> &'static BuiltinTheme {
    &GRUVBOX_DARK
}

pub fn theme_by_id(id: &str) -> Option<&'static BuiltinTheme> {
    builtin_themes().iter().find(|theme| theme.id == id)
}

pub fn theme_by_index(idx: usize) -> &'static BuiltinTheme {
    builtin_themes().get(idx).unwrap_or(default_theme())
}

pub fn theme_id_or_default(id: Option<&str>) -> &'static str {
    id.and_then(theme_by_id)
        .map(|theme| theme.id)
        .unwrap_or(default_theme().id)
}

pub fn theme_index_by_id(id: &str) -> usize {
    builtin_themes()
        .iter()
        .position(|theme| theme.id == id)
        .unwrap_or(0)
}

pub fn format_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.0}k", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

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

pub fn brand_line(theme: &Theme, dimmed: bool) -> (Line<'static>, u16) {
    let base = if dimmed {
        Style::default().add_modifier(Modifier::DIM)
    } else {
        Style::default()
    };
    let version = env!("CARGO_PKG_VERSION");
    let display_width =
        unicode_width::UnicodeWidthStr::width(format!(" ♥ Flowmux v{} ", version).as_str()) as u16;
    let line = Line::from(vec![
        Span::styled(" ♥ ", base.fg(theme.heart_red)),
        Span::styled("Flowmux", base.fg(theme.gray)),
        Span::styled(format!(" v{} ", version), base.fg(theme.gray)),
    ]);
    (line, display_width)
}

pub fn status_count_spans(
    theme: &Theme,
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
        base.fg(theme.bg).bg(theme.green)
    } else {
        base.fg(theme.green)
    };

    let waiting_style = if blink_waiting {
        base.fg(theme.bg).bg(theme.yellow)
    } else {
        base.fg(theme.yellow)
    };

    let spans = vec![
        Span::styled(format!(" {} ", ICON_RUN), base.fg(theme.green)),
        Span::styled(format!("{} running", running), running_style),
        Span::styled(format!(" {} ", ICON_WAIT), base.fg(theme.yellow)),
        Span::styled(format!("{} waiting", waiting), waiting_style),
        Span::styled(
            format!(" {} {} idle ", ICON_IDLE, idle),
            base.fg(theme.cyan),
        ),
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
    use std::collections::HashSet;

    #[test]
    fn status_count_spans_only_blinks_running_text_and_count() {
        let theme = default_theme().theme;
        let (spans, _) = status_count_spans(&theme, 3, 2, 1, true, false, false);

        assert_eq!(spans[0].content, format!(" {} ", ICON_RUN));
        assert_eq!(spans[1].content, "3 running");
        assert_eq!(spans[0].style.fg, Some(theme.green));
        assert_eq!(spans[0].style.bg, None);
        assert_eq!(spans[1].style.fg, Some(theme.bg));
        assert_eq!(spans[1].style.bg, Some(theme.green));
    }

    #[test]
    fn status_count_spans_only_blinks_waiting_text_and_count() {
        let theme = default_theme().theme;
        let (spans, _) = status_count_spans(&theme, 3, 2, 1, false, true, false);

        assert_eq!(spans[2].content, format!(" {} ", ICON_WAIT));
        assert_eq!(spans[3].content, "2 waiting");
        assert_eq!(spans[2].style.fg, Some(theme.yellow));
        assert_eq!(spans[2].style.bg, None);
        assert_eq!(spans[3].style.fg, Some(theme.bg));
        assert_eq!(spans[3].style.bg, Some(theme.yellow));
    }

    #[test]
    fn builtin_theme_ids_are_unique_and_resolvable() {
        let ids: HashSet<&str> = builtin_themes().iter().map(|theme| theme.id).collect();

        assert_eq!(ids.len(), builtin_themes().len());
        for theme in builtin_themes() {
            assert_eq!(theme_by_id(theme.id), Some(theme));
        }
    }

    #[test]
    fn unknown_theme_id_falls_back_to_default() {
        assert_eq!(
            theme_id_or_default(Some("missing-theme")),
            default_theme().id
        );
    }
}

use anyhow::{Context, Result};
use crossterm::terminal;
use regex::Regex;
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use tmux_interface::{NewWindow, Tmux};

static SESSION: OnceLock<String> = OnceLock::new();

/// Initialise the tmux session name.  Must be called once before any other
/// tmux function.  Subsequent calls are silently ignored (OnceLock semantics).
pub fn init(name: &str) {
    let _ = SESSION.set(name.to_string());
}

/// Returns the active tmux session name.  Falls back to `"stable"` if
/// `init()` was never called.
pub fn session_name() -> &'static str {
    SESSION.get().map(String::as_str).unwrap_or("stable")
}

/// Replace non-`[a-zA-Z0-9_-]` chars with `-`, collapse consecutive dashes, trim edges.
pub fn sanitize_name(s: &str) -> String {
    let s = s.trim();
    let sanitized: String = s
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    Regex::new(r"-{2,}")
        .unwrap()
        .replace_all(&sanitized, "-")
        .trim_matches('-')
        .to_string()
}

/// Ensure the tmux session exists; create it detached if not.
/// Uses raw `Command` so that a tmux server is started automatically when
/// none is running (tmux_interface's `HasSession` errors out in that case).
pub fn ensure_session() -> Result<()> {
    let has = Command::new("tmux")
        .args(["has-session", "-t", session_name()])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    if !has {
        // Create the session detached with the current terminal size so panes
        // aren't stuck at the tmux default (80×24).
        let (cols, rows) = terminal::size().unwrap_or((220, 50));
        Command::new("tmux")
            .args([
                "new-session",
                "-d",
                "-s",
                session_name(),
                "-x",
                &cols.to_string(),
                "-y",
                &rows.to_string(),
            ])
            .status()
            .context("failed to create tmux session")?;
    }

    // Raise the per-pane scrollback limit so stable's AgentView can show deep
    // history without requiring the user to touch ~/.tmux.conf.  We use the
    // global option so it applies to every new window; existing windows are
    // unaffected but that is acceptable (they were created with whatever limit
    // was in place at the time).  The value matches MAX_RETAINED_LINES in
    // app.rs — going higher wastes tmux memory for no visible benefit.
    let _ = Command::new("tmux")
        .args(["set-option", "-g", "history-limit", "50000"])
        .status();

    Ok(())
}

/// Create a new window in the current session with the given working directory and name.
/// Returns the window index (parsed from `#{window_index}` format output).
pub fn new_window(dir: &str, name: &str) -> Result<usize> {
    let output = Tmux::with_command(
        NewWindow::new()
            .detached()
            .target_window(session_name())
            .start_directory(dir)
            .window_name(name)
            .print()
            .format("#{window_index}")
            .build(),
    )
    .output()
    .context("failed to create tmux window")?;

    let inner = output.into_inner();
    let stdout = String::from_utf8_lossy(&inner.stdout);
    let index: usize = stdout
        .trim()
        .parse()
        .with_context(|| format!("failed to parse window index from: {:?}", stdout.trim()))?;
    Ok(index)
}

/// Send keys to a tmux pane target (e.g. `mysession:1.0`).
pub fn send_keys(target: &str, keys: &str) -> Result<()> {
    Command::new("tmux")
        .args(["send-keys", "-t", target, keys])
        .status()
        .with_context(|| format!("failed to send keys to {}", target))?;
    Ok(())
}

/// Send a literal byte string to a tmux pane target, bypassing key-name
/// lookup.  Uses `load-buffer` + `paste-buffer` instead of `send-keys -l`
/// because tmux silently drops `;` when passed as a standalone argument to
/// `send-keys` (tmux treats `;` as a command separator regardless of `-l`).
pub fn send_literal(target: &str, data: &str) -> Result<()> {
    use std::io::Write;
    let mut child = Command::new("tmux")
        .args(["load-buffer", "-b", "stable_buf", "-"])
        .stdin(Stdio::piped())
        .spawn()
        .context("failed to spawn tmux load-buffer")?;
    if let Some(ref mut stdin) = child.stdin {
        stdin.write_all(data.as_bytes())?;
    }
    child.wait().context("tmux load-buffer failed")?;

    Command::new("tmux")
        .args(["paste-buffer", "-t", target, "-b", "stable_buf"])
        .status()
        .with_context(|| format!("failed to paste buffer to {}", target))?;
    Ok(())
}

/// Capture the raw ANSI output of the pane's current visible viewport (`-p -e`).
///
/// We intentionally omit `-S -` (full scrollback) because:
///   1. We only ever render the last viewport_height lines, so history above the
///      visible area is never used.
///   2. Capturing the full scrollback causes the piped string to grow without
///      bound as the agent produces output, driving CPU usage up linearly.
///   3. Scrolling is handled by tmux copy-mode (PPage/NPage), which shifts the
///      visible viewport.  capture-pane captures whatever is currently visible,
///      so scrolled content is captured correctly without needing -S -.
pub fn capture_pane(target: &str) -> Result<String> {
    let output = Command::new("tmux")
        .args(["capture-pane", "-t", target, "-p", "-e"])
        .output()
        .with_context(|| format!("failed to capture pane {}", target))?;
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Resize a tmux window to the given dimensions.
pub fn resize_window(target: &str, width: u16, height: u16) -> Result<()> {
    Command::new("tmux")
        .args([
            "resize-window",
            "-t",
            target,
            "-x",
            &width.to_string(),
            "-y",
            &height.to_string(),
        ])
        .status()
        .with_context(|| format!("failed to resize window {}", target))?;
    Ok(())
}

/// Return the cursor position within the pane's visible screen as (col, row).
/// Returns `None` when the cursor is hidden (`cursor_flag == 0`), matching the
/// behaviour of a real terminal that respects `\e[?25l` / `\e[?25h`.
pub fn cursor_position(target: &str) -> Option<(u16, u16)> {
    let output = Command::new("tmux")
        .args([
            "display-message",
            "-t",
            target,
            "-p",
            "#{cursor_flag} #{cursor_x} #{cursor_y}",
        ])
        .output()
        .ok()?;
    let s = String::from_utf8_lossy(&output.stdout);
    let mut parts = s.trim().split_whitespace();
    let flag: u8 = parts.next()?.parse().ok()?;
    if flag == 0 {
        return None; // cursor explicitly hidden by the running app
    }
    let x: u16 = parts.next()?.parse().ok()?;
    let y: u16 = parts.next()?.parse().ok()?;
    Some((x, y))
}

/// Return `true` if the process running in the pane has enabled any form of
/// mouse reporting (`#{mouse_any_flag}` == 1).  Used to decide whether to
/// forward hover/motion mouse events: forwarding them to an application that
/// has NOT enabled mouse mode causes the leading ESC byte to be misinterpreted
/// as the Escape key, resetting insert-mode in editors like vim.
pub fn pane_mouse_active(target: &str) -> bool {
    Command::new("tmux")
        .args(["display-message", "-t", target, "-p", "#{mouse_any_flag}"])
        .output()
        .map(|o| o.stdout.first().copied() == Some(b'1'))
        .unwrap_or(false)
}

/// Check whether a pane is alive.
///
/// Uses `tmux list-panes -t <target>` which exits non-zero (and produces no
/// output) when the exact window/pane does not exist, unlike `display-message`
/// which silently falls back to the active pane in the session.
pub fn is_alive(target: &str) -> bool {
    Command::new("tmux")
        .args(["list-panes", "-t", target])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Capture the last `lines_back` lines of the pane's scrollback history.
///
/// Uses `capture-pane -S -<lines_back>` so that content above the live
/// viewport is included.  This is used by the AgentView scroll logic: when
/// `view_scroll > 0` the tick captures history instead of just the visible
/// viewport, and the renderer slices the appropriate window out of the buffer.
pub fn capture_pane_history(target: &str, lines_back: usize) -> Result<String> {
    let output = Command::new("tmux")
        .args([
            "capture-pane",
            "-t",
            target,
            "-p",
            "-e",
            "-S",
            &format!("-{}", lines_back),
        ])
        .output()
        .with_context(|| format!("failed to capture pane history {}", target))?;
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Kill a tmux window by target (e.g., `mysession:1`).
pub fn kill_window(target: &str) -> Result<()> {
    Command::new("tmux")
        .args(["kill-window", "-t", target])
        .status()
        .with_context(|| format!("failed to kill window {}", target))?;
    Ok(())
}

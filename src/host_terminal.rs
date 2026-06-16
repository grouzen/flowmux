use std::io::{self, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};

/// Probed colors from the host terminal.
#[derive(Debug, Clone, Copy)]
pub struct HostColors {
    /// Foreground color (OSC 10), if successfully queried.
    pub fg: Option<(u8, u8, u8)>,
    /// Background color (OSC 11), if successfully queried.
    pub bg: Option<(u8, u8, u8)>,
}

impl Default for HostColors {
    fn default() -> Self {
        Self { fg: None, bg: None }
    }
}

/// Probe the host terminal's default fg/bg colors via tmux passthrough.
///
/// This sends OSC 10 and OSC 11 queries through tmux to the host terminal,
/// reads the responses, and parses the RGB values.
///
/// Returns `HostColors` with `None` for any color that couldn't be probed
/// (e.g., if the terminal doesn't support the query or times out).
pub fn probe_host_colors() -> Result<HostColors> {
    // We need raw mode to reliably read from stdin
    enable_raw_mode()?;

    let colors = probe_host_colors_inner();

    // We'll let the TUI setup handle entering alternate screen later,
    // but we need to stay in raw mode for now since we're about to
    // enter the TUI event loop.
    // Actually, we should leave raw mode here and let tui::run() enable it again.
    disable_raw_mode()?;

    colors
}

fn probe_host_colors_inner() -> Result<HostColors> {
    // Send OSC 10 and 11 queries through tmux passthrough
    // Format: \x1bPtmux;\x1b<sequence>\x1b\\
    // Query: \x1b]10;?\x1b\\ and \x1b]11;?\x1b\\
    let query = b"\x1bPtmux;\x1b\x1b]10;?\x1b\x1b\\\x1b\x1b]11;?\x1b\x1b\\\x1b\\";

    let mut stdout = io::stdout();
    stdout.write_all(query)?;
    stdout.flush()?;

    // Read responses with timeout using a channel
    let (tx, rx) = std::sync::mpsc::channel();
    let stop = Arc::new(AtomicBool::new(false));
    let stop_clone = stop.clone();

    let handle = std::thread::spawn(move || {
        let fd = libc::STDIN_FILENO;
        let mut buf = [0u8; 1];
        loop {
            if stop_clone.load(Ordering::Relaxed) {
                break;
            }
            let mut pfd = libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            };
            let ret = unsafe { libc::poll(&mut pfd, 1, 50) };
            if ret > 0 && (pfd.revents & libc::POLLIN) != 0 {
                let n = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, 1) };
                match n {
                    0 => break,
                    1 => {
                        if tx.send(buf[0]).is_err() {
                            break;
                        }
                    }
                    _ => break,
                }
            } else if ret < 0 {
                break;
            }
        }
    });

    // Collect response bytes with timeout
    let mut response = Vec::new();
    let timeout = Duration::from_millis(500);
    let start = Instant::now();

    while start.elapsed() < timeout {
        let remaining = timeout - start.elapsed();
        match rx.recv_timeout(remaining) {
            Ok(byte) => {
                response.push(byte);
                // Check if we have both responses
                if has_complete_response(&response) {
                    break;
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => break,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    // Signal the reader thread to stop and wait for it to finish
    stop.store(true, Ordering::Relaxed);
    let _ = handle.join();

    // Parse responses
    let fg = parse_osc_response(&response, 10);
    let bg = parse_osc_response(&response, 11);

    Ok(HostColors { fg, bg })
}

fn has_complete_response(buf: &[u8]) -> bool {
    // Check if we have complete OSC 10 and 11 responses
    // Response format: \x1b]10;rgb:XXXX/XXXX/XXXX\x1b\\ or \x1b]11;rgb:XXXX/XXXX/XXXX\x1b\\
    // Or: \x1b]10;#RRGGBB\x1b\\ etc.

    let has_osc10 = find_osc_response(buf, 10).is_some();
    let has_osc11 = find_osc_response(buf, 11).is_some();

    has_osc10 && has_osc11
}

fn find_osc_response(buf: &[u8], osc_num: u8) -> Option<usize> {
    // Look for \x1b]<num>;...<terminator>
    // Terminator is either \x1b\\ (ST) or \x07 (BEL)

    let prefix = format!("\x1b]{};", osc_num);
    let prefix_bytes = prefix.as_bytes();

    for i in 0..buf.len().saturating_sub(prefix_bytes.len()) {
        if &buf[i..i + prefix_bytes.len()] == prefix_bytes {
            // Found the prefix, now look for terminator
            for j in (i + prefix_bytes.len())..buf.len() {
                if buf[j] == 0x07 {
                    // BEL terminator
                    return Some(j + 1);
                }
                if buf[j] == 0x1b && j + 1 < buf.len() && buf[j + 1] == b'\\' {
                    // ST terminator
                    return Some(j + 2);
                }
            }
        }
    }

    None
}

fn parse_osc_response(buf: &[u8], osc_num: u8) -> Option<(u8, u8, u8)> {
    let end_pos = find_osc_response(buf, osc_num)?;

    let prefix = format!("\x1b]{};", osc_num);
    let prefix_bytes = prefix.as_bytes();

    // Find the start of this response
    let start_pos = buf
        .windows(prefix_bytes.len())
        .position(|w| w == prefix_bytes)?;

    let response_start = start_pos + prefix_bytes.len();
    let response_end = if end_pos >= 2 && buf[end_pos - 2] == 0x1b && buf[end_pos - 1] == b'\\' {
        end_pos - 2
    } else if end_pos >= 1 && buf[end_pos - 1] == 0x07 {
        end_pos - 1
    } else {
        end_pos
    };

    if response_start >= response_end {
        return None;
    }

    let color_str = std::str::from_utf8(&buf[response_start..response_end]).ok()?;
    parse_osc_color(color_str)
}

fn parse_osc_color(s: &str) -> Option<(u8, u8, u8)> {
    // Supported formats:
    // rgb:XXXX/XXXX/XXXX (1-4 hex digits per component)
    // #RRGGBB
    // #RGB

    if let Some(rgb) = s.strip_prefix("rgb:") {
        let parts: Vec<&str> = rgb.split('/').collect();
        if parts.len() != 3 {
            return None;
        }
        let r = parse_hex_component(parts[0])?;
        let g = parse_hex_component(parts[1])?;
        let b = parse_hex_component(parts[2])?;
        return Some((r, g, b));
    }

    if let Some(hex) = s.strip_prefix('#') {
        if hex.len() == 6 {
            // #RRGGBB
            let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
            let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
            let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
            return Some((r, g, b));
        }
        if hex.len() == 3 {
            // #RGB -> #RRGGBB
            let r = u8::from_str_radix(&hex[0..1], 16).ok()? * 17;
            let g = u8::from_str_radix(&hex[1..2], 16).ok()? * 17;
            let b = u8::from_str_radix(&hex[2..3], 16).ok()? * 17;
            return Some((r, g, b));
        }
    }

    None
}

fn parse_hex_component(s: &str) -> Option<u8> {
    if s.is_empty() || s.len() > 4 {
        return None;
    }
    if !s.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }

    let value = u32::from_str_radix(s, 16).ok()?;
    let max = (1u32 << (s.len() * 4)) - 1;
    let scaled = (value * 255 + max / 2) / max;
    Some(scaled as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_hex_component() {
        assert_eq!(parse_hex_component("ff"), Some(255));
        assert_eq!(parse_hex_component("FF"), Some(255));
        assert_eq!(parse_hex_component("00"), Some(0));
        assert_eq!(parse_hex_component("80"), Some(128));
        assert_eq!(parse_hex_component("ffff"), Some(255));
        assert_eq!(parse_hex_component("0000"), Some(0));
        assert_eq!(parse_hex_component("8000"), Some(128));
        assert_eq!(parse_hex_component(""), None);
        assert_eq!(parse_hex_component("12345"), None); // 5 digits, exceeds limit
    }

    #[test]
    fn test_parse_osc_color() {
        // rgb:XXXX/XXXX/XXXX format
        assert_eq!(parse_osc_color("rgb:ffff/ffff/ffff"), Some((255, 255, 255)));
        assert_eq!(parse_osc_color("rgb:0000/0000/0000"), Some((0, 0, 0)));
        assert_eq!(parse_osc_color("rgb:8000/8000/8000"), Some((128, 128, 128)));
        assert_eq!(parse_osc_color("rgb:ff/ff/ff"), Some((255, 255, 255)));
        assert_eq!(parse_osc_color("rgb:2828/2828/2828"), Some((40, 40, 40)));

        // #RRGGBB format
        assert_eq!(parse_osc_color("#ffffff"), Some((255, 255, 255)));
        assert_eq!(parse_osc_color("#000000"), Some((0, 0, 0)));
        assert_eq!(parse_osc_color("#808080"), Some((128, 128, 128)));

        // #RGB format
        assert_eq!(parse_osc_color("#fff"), Some((255, 255, 255)));
        assert_eq!(parse_osc_color("#000"), Some((0, 0, 0)));
        assert_eq!(parse_osc_color("#888"), Some((136, 136, 136)));

        // Invalid formats
        assert_eq!(parse_osc_color("invalid"), None);
        assert_eq!(parse_osc_color("rgb:ff/ff"), None);
        assert_eq!(parse_osc_color("#gggggg"), None);
    }

    #[test]
    fn test_find_osc_response() {
        let buf1 = b"\x1b]11;rgb:2828/2828/2828\x1b\\";
        assert_eq!(find_osc_response(buf1, 11), Some(buf1.len()));

        let buf2 = b"\x1b]11;#282828\x1b\\";
        assert_eq!(find_osc_response(buf2, 11), Some(buf2.len()));

        let buf3 = b"\x1b]11;rgb:2828/2828/2828\x07";
        assert_eq!(find_osc_response(buf3, 11), Some(buf3.len()));

        let buf4 = b"\x1b]10;rgb:ebdbb2/ebdbb2/ebdbb2\x1b\\\x1b]11;rgb:2828/2828/2828\x1b\\";
        assert_eq!(find_osc_response(buf4, 10), Some(31));
        assert_eq!(find_osc_response(buf4, 11), Some(buf4.len()));

        // Incomplete
        let buf5 = b"\x1b]11;rgb:2828/2828/2828";
        assert_eq!(find_osc_response(buf5, 11), None);
    }

    #[test]
    fn test_parse_osc_response() {
        let buf = b"\x1b]11;rgb:2828/2828/2828\x1b\\";
        assert_eq!(parse_osc_response(buf, 11), Some((40, 40, 40)));

        let buf = b"\x1b]10;rgb:eb/db/b2\x1b\\";
        assert_eq!(parse_osc_response(buf, 10), Some((235, 219, 178)));

        let buf = b"\x1b]10;rgb:eb/db/b2\x1b\\\x1b]11;rgb:2828/2828/2828\x1b\\";
        assert_eq!(parse_osc_response(buf, 10), Some((235, 219, 178)));
        assert_eq!(parse_osc_response(buf, 11), Some((40, 40, 40)));
    }
}

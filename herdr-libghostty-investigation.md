# Herdr libghostty-vt Investigation

## Herdr's Rendering Architecture

### Yes, herdr uses ratatui (v0.30 + crossterm backend)

But it **completely sidesteps the ansi-to-text problem** by never parsing ANSI itself.

### The architecture: libghostty-vt as a headless terminal engine

**libghostty-vt** (the terminal emulation engine from the Ghostty terminal app, written in Zig) is compiled as a static library via `build.rs` and linked through FFI:

1. **PTY bytes -> ghostty directly.** Raw ANSI output from the shell is fed straight into `ghostty_terminal_vt_write()` -- ghostty handles all ANSI/VT parsing, SGR attributes, cursor movement, alternate screen, scrollback, etc. internally.

2. **Ghostty maintains a fully resolved cell grid.** Each cell has: Unicode graphemes (codepoints, not ANSI bytes), resolved RGB colors, and style booleans (bold, italic, underline, etc.).

3. **Rendering walks ghostty's cells and writes to ratatui's buffer directly** (`src/pane/terminal.rs:624-712`):
   ```rust
   // For each row y, each cell x:
   cells.graphemes_into(scratch);        // Vec<u32> codepoints
   let style = ghostty_cell_style(...);  // CellStyle -> ratatui::Style
   buf[(area.x + x, area.y + y)].set_symbol(symbol);
   buf[(area.x + x, area.y + y)].set_style(style);
   ```

4. **Ratatui + crossterm** diffs the buffer and emits the minimal ANSI diff to the host terminal.

### The key insight

Herdr never sees raw ANSI during rendering. The pipeline is:

```
PTY bytes -> ghostty (parses everything) -> resolved cells (graphemes + RGB + style flags)
         -> mapped 1:1 to ratatui buffer cells -> crossterm emits ANSI diff to host terminal
```

No `ansi-to-text` crate needed. No raw output mode needed. Ghostty does the heavy lifting of terminal emulation, and herdr just reads already-resolved cell data and paints it into ratatui's immediate-mode buffer.

Key files: `src/ghostty/mod.rs` (safe wrappers), `src/pane/terminal.rs` (the bridge), `vendor/libghostty-vt/` (vendored Zig source, v1.3.2).

---

## The Snapshot: `RenderState::update()` (`ghostty/mod.rs:722`)

```rust
pub fn update(&mut self, terminal: &Terminal) -> Result<(), Error> {
    unsafe { ffi::ghostty_render_state_update(self.raw, terminal.raw()).into_result() }
}
```

This single FFI call snapshots the terminal's current grid into a stable render state. All subsequent reads come from this snapshot -- no race with concurrent PTY writes.

## The Cell Walk: Two-Level Iterator Pattern

The ghostty API uses a **pull-based, reusable-handle** pattern (avoids allocation per row/cell):

```
RenderState -> populate_row_iterator(&mut RowIterator) -> RowIter
  RowIter.next()                     // advance to next row (bool)
  RowIter.populate_cells(&mut RowCells) -> RowCellIter
    RowCellIter.next()               // advance to next cell (bool)
    RowCellIter.wide()               // -> CellWide { Narrow, Wide, SpacerTail, SpacerHead }
    RowCellIter.style()              // -> CellStyle { bold, italic, faint, blink, inverse, invisible, strikethrough, overline, underlined }
    RowCellIter.fg_color()           // -> Option<RgbColor> (None = use default fg)
    RowCellIter.bg_color()           // -> Option<RgbColor> (None = transparent/default)
    RowCellIter.graphemes_into(&mut Vec<u32>)  // Unicode codepoints into scratch buffer
```

## Graphemes -> ratatui symbol (`terminal.rs:925-966`)

Codepoints (`Vec<u32>`) are converted to a `String` via `char::from_u32`. Wide char handling:
- **Narrow**: expected display width = 1
- **Wide**: expected width = 2 (CJK characters, emoji) -- symbol string must be 2-wide
- **SpacerTail**: empty string (the trailing half-cell of a wide char)
- **SpacerHead**: single space (placeholder)

If the rendered width doesn't match expected, it falls back to a blank symbol (`" "`, `"  "`, or `""`).

## Style Mapping (`terminal.rs:983-1048`)

The `ghostty_cell_style()` function converts resolved cell attributes to ratatui:

| Ghostty `CellStyle` | Ratatui `Modifier` |
|---|---|
| `bold` | `BOLD` |
| `italic` | `ITALIC` |
| `faint` | `DIM` |
| `blink` | `SLOW_BLINK` |
| `underlined` | `UNDERLINED` |
| `strikethrough` | `CROSSED_OUT` |

Special handling:
- **`invisible`**: fg is set to bg (text blends into background)
- **`inverse`**: fg/bg are swapped, but first `None` (transparent) values are resolved to the actual terminal colors -- this prevents invisible text when the host terminal's default colors match
- Colors: `RgbColor { r, g, b }` -> `Color::Rgb(r, g, b)` -- always 24-bit RGB, no palette indirection

## Buffer Write (`terminal.rs:683-686`)

Each cell is written directly into ratatui's buffer:

```rust
let cell = &mut buf[(area.x + x, area.y + y)];
cell.reset();
cell.set_symbol(symbol);   // graphemes as String
cell.set_style(style);     // fg + bg + modifiers
```

Remaining cells in the row/area that ghostty didn't fill get reset to default bg/fg via `ghostty_reset_cell()`.

---

## Stable App Analysis

### Overview

stable is a Rust TUI dashboard for managing multiple CLI coding agents (OpenCode, Claude Code) running in tmux panes. It owns a dedicated tmux session, creates agent windows, captures their terminal output, and renders it inside a ratatui-based UI. **stable does not do any PTY handling itself** -- all terminal emulation is delegated entirely to tmux.

### Current Pipeline: tmux Pane Capture to ratatui Rendering

```
  [Agent CLI (opencode/claude) running in tmux window]
                      |
                      v
  [tmux capture-pane -t <pane> -p -e]   <-- subprocess every 50ms
                      |
                      v
  Raw ANSI string (with escape sequences for colors, cursor moves, etc.)
                      |
                      v
  AgentViewState::update_lines(raw)
    - Change detection: compare raw bytes to previous capture
    - Split on \n into Vec<String>
    - Bound to MAX_RETAINED_LINES (10,000)
                      |
                      v
  [Render path in ui/agent_view.rs]
    1. Slice visible window: lines[start..end] based on view_scroll
    2. Join with \n into visible_text string
    3. extract_first_bg_color(visible_text) -- hand-rolled SGR scanner
       finds first \x1b[48;...m or \x1b[4Xm sequence
    4. visible_text.as_bytes().into_text_with_style(base_style)
       -- ansi-to-tui fork converts ANSI bytes to ratatui::text::Text
       with the seeded base background style
    5. Paragraph::new(text).style(base_style)
    6. f.render_widget(para, content_area)
                      |
                      v
  [ratatui + crossterm flushes to terminal]
```

### Dependencies

| Crate | Version | Purpose |
|---|---|---|
| `ratatui` | 0.30 | TUI rendering framework |
| `crossterm` | 0.29 (event-stream) | Terminal backend |
| **`ansi-to-tui`** | 8.0.1 (custom fork) | ANSI escape sequence to ratatui `Text` conversion |
| `tmux_interface` | 0.4 (tmux_3_3a) | Typed tmux command builders |
| `tokio` | 1 | Async runtime |
| `reqwest` | 0.12 | HTTP client for OpenCode REST API + SSE |
| `axum` | 0.8 | HTTP server for Claude Code hooks |
| `tui-markdown` | 0.3 | Markdown rendering for dashboard cards |

**Notable: No PTY library.** All terminal emulation is delegated to tmux.

**ansi-to-tui** is pulled from a **custom fork** (`grouzen/ansi-to-tui`, branch `allow-setting-style-on-init-and-reset`). This fork adds the ability to seed the parser with an initial style and to handle style resets.

### tmux Interaction

| Function | tmux Command | Purpose |
|---|---|---|
| `ensure_session()` | `tmux new-session -d -s stable` | Create session |
| `new_window()` | `tmux new-window -d` | Create agent window |
| `send_keys()` | `tmux send-keys` | Forward keyboard input |
| `send_literal()` | `tmux send-keys -l` | Send raw bytes (SGR mouse, bracketed paste) |
| `capture_pane()` | `tmux capture-pane -p -e` | Capture viewport with ANSI |
| `capture_pane_history()` | `tmux capture-pane -S -N` | Capture scrollback |
| `resize_window()` | `tmux resize-window` | Resize pane |
| `cursor_position()` | `tmux display-message '#{cursor_flag} #{cursor_x} #{cursor_y}'` | Get cursor |
| `pane_mouse_active()` | `tmux display-message '#{mouse_any_flag}'` | Check mouse mode |

### Current Pain Points

1. **Custom fork dependency for `ansi-to-tui`** -- pinned to a specific git commit from a third-party fork
2. **Hand-rolled `extract_first_bg_color`** -- duplicated in both `agent_view.rs` and `git_viewer.rs`
3. **Subprocess-per-tick capture** -- 20 subprocesses/second per viewed agent
4. **No persistent PTY connection** -- polling only, up to 50ms latency
5. **Full scrollback capture avoided** -- grows unbounded, so only viewport is captured
6. **Change detection uses full string comparison** -- ~1.5 MB buffer per agent
7. **Approximate Unicode width calculation** -- crude ASCII fast path, no CJK/combining mark support

---

## Would libghostty-vt Work for Stable?

### Scenario A: Replace `ansi-to-tui` with libghostty-vt (drop-in)

Feed `capture-pane -e` output into `ghostty_terminal_vt_write()`, then render via the cell-grid API. This would eliminate:
- The `ansi-to-tui` fork dependency
- The hand-rolled `extract_first_bg_color()` (ghostty resolves all colors natively)

**But** you'd be running terminal emulation **twice**: tmux already parsed the ANSI to maintain its pane grid, then ghostty parses it again. That's redundant work and potential state divergence.

### Scenario B: `pipe-pane` + libghostty-vt (replace tmux's emulation for rendering)

Use `tmux pipe-pane` to get the raw PTY byte stream. Feed that into libghostty-vt. This gives the full benefit but means maintaining **two terminal emulators** for the same pane (tmux's for agent interaction, ghostty's for rendering). State drift would cause bugs.

### The Core Problem

**stable doesn't own the PTY** -- tmux does. Herdr's approach works cleanly because it spawns the child process and owns the PTY directly. In stable, tmux is the terminal, stable is a viewer.

---

## libghostty-vt vs vte: Deep Comparison

### vte (alacritty's parser)

vte is a **low-level state machine parser** that gives you callbacks. You implement the `Perform` trait:

```rust
impl Perform for MyTerminal {
    fn print(&mut self, c: char) { /* character to display */ }
    fn execute(&mut self, byte: u8) { /* C0/C1 controls (BEL, BS, CR, LF, etc.) */ }
    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], ignore: bool, action: char) {
        /* SGR, cursor movement, etc. -- you parse params yourself */
    }
    fn osc_dispatch(&mut self, params: &[&[u8]], bell_terminated: bool) { /* title, etc. */ }
    fn esc_dispatch(&mut self, intermediates: &[u8], ignore: bool, byte: u8) { /* DECSC, etc. */ }
}
```

**What you get**: byte-level parsing into structured callbacks.
**What you DON'T get**: a cell grid, color resolution, style tracking, scrollback, alternate screen, cursor state, wide char handling, or anything resembling a terminal model. **You build all of that yourself.**

### libghostty-vt

A **complete terminal emulation engine** that gives you a resolved cell grid. The API surface (26 header files) includes:

| Category | What's provided |
|---|---|
| Terminal state | Full grid, cursor, modes, scrollback, alternate screen |
| Cell data | Graphemes (Unicode), resolved RGB colors, style flags, wide char metadata, hyperlinks |
| Rendering | Snapshot API (`RenderState`), row/cell iterators with dirty tracking |
| Input | Key encoder (kitty protocol + legacy), mouse encoder (SGR, button-event, any-event) |
| Text extraction | Plain text or ANSI-formatted output via formatter |
| Selection | Built-in selection model |
| Graphics | Kitty graphics protocol support |
| OSC | Title, clipboard (OSC 52), etc. |

### Side-by-Side for Stable's Use Case

| Concern | vte | libghostty-vt |
|---|---|---|
| **ANSI parsing** | Yes, callbacks | Yes, fully internal |
| **Cell grid** | You build it | Built-in, ready to render |
| **SGR -> ratatui styles** | You implement `csi_dispatch`, track current style, apply to grid cells | Pre-resolved: `CellStyle` struct with booleans + `RgbColor` |
| **256-color -> RGB** | You implement the palette lookup table | Done internally |
| **Wide char handling** | You track display width per cell | `CellWide` enum (Narrow/Wide/SpacerTail/SpacerHead) |
| **Alternate screen** | You implement screen buffer switching | Built-in |
| **Scrollback** | You implement ring buffer | Built-in with viewport scrolling |
| **Cursor tracking** | You implement position + visibility | Built-in with viewport coordinates |
| **Code to write** | ~1000-2000 lines (terminal model) | ~100-200 lines (FFI wrappers + ratatui mapping) |
| **Build dependency** | Pure Rust, `cargo build` | Zig toolchain, 18MB vendored source |
| **Linking** | Static Rust lib | Static C lib via FFI |
| **Cross-compilation** | Trivial | Requires Zig target mapping (herdr already has it) |
| **Debugging** | Full Rust, `println!` works | Opaque Zig state, limited introspection |
| **Crash surface** | Rust panics only | Undefined behavior possible via FFI (though herdr's wrappers are careful) |
| **Maintenance** | Cargo update | Re-vendor + rebuild when ghostty updates |

### The Real Cost of vte

vte is not a drop-in replacement for `ansi-to-tui`. It's a **parser primitive**. To get what herdr gets from libghostty-vt, you'd need to write:

1. A `Grid<Cell>` struct with rows, columns, scrollback ring buffer
2. Current style tracking (SGR state machine in `csi_dispatch`)
3. 256-color palette -> RGB conversion table
4. Cursor position + visibility state
5. Alternate screen buffer (two grids)
6. Wide character handling (Unicode width detection + spacer cells)
7. Scroll regions (DECSTBM)
8. Tab stops
9. Insert/delete line/character operations
10. Erase operations (ED, EL, ECH)
11. Reverse index (RI) for scrolling up
12. Character set selection (G0/G1)

This is essentially writing a terminal emulator. Alacritty's `Term` struct (which uses vte) is ~4000 lines. You'd write a simpler version since you only need viewport rendering, but it's still significant work.

### The Real Cost of libghostty-vt

1. **Zig toolchain**: developers and CI need `zig` installed. Herdr's `build.rs` handles this cleanly -- it's just `zig build` with target mapping. If Zig isn't in your CI image, add it.
2. **Vendor size**: 18MB of Zig source in your repo. Herdr vendors it directly (not a submodule).
3. **Opaque failures**: if ghostty returns an error, you get a result code, not a Rust panic with backtrace. Herdr handles this by treating errors as "skip this frame."
4. **Version coupling**: libghostty-vt v1.3.2 is pinned. Upgrading means re-vendoring and re-generating bindgen.

### For Stable Specifically (Scenario A)

In Scenario A (keep `capture-pane -e`, replace `ansi-to-tui`):

- **vte**: you'd feed `capture-pane` output into `parser.advance()`, your `Perform` impl would build a cell grid, then render it. tmux's `capture-pane -e` preserves the current style state at the start of each line via explicit SGR sequences, so this would work, but you'd still need to build the grid.

- **libghostty-vt**: same approach -- feed the `capture-pane` ANSI into `ghostty_terminal_vt_write()`, then read the cell grid. The key difference: libghostty-vt gives you the grid for free, with all colors resolved to RGB. You skip ~1500 lines of terminal model code.

### Bottom Line

| If your priority is... | Choose... |
|---|---|
| Minimum code to write | **libghostty-vt** (herdr's approach, ~200 lines of glue) |
| Pure Rust / no build deps | **vte** + write your own grid (~1500 lines) |
| Fastest time to replace `ansi-to-tui` | **libghostty-vt** |
| Maximum control / debuggability | **vte** |
| Match herdr's rendering fidelity | **libghostty-vt** |

The honest assessment: vte is a parser, not a terminal emulator. libghostty-vt is a terminal emulator that happens to expose a cell-grid rendering API. For the use case of "render captured terminal output in ratatui," libghostty-vt gets you there in a fraction of the code. The Zig build dependency is the main downside, and herdr proves it's manageable.

---

## Herdr's Complete Rendering Pipeline (Reference)

```
PTY child process
  | (writes ANSI output bytes)
  v
[Reader task]  (src/pane.rs:417-453, spawn_blocking)
  | reads from PTY fd in 8KB chunks
  | calls terminal.process_pty_bytes()
  v
[GhosttyPaneTerminal::process_pty_bytes]  (src/pane/terminal.rs:302-368)
  | 1. OSC 52 clipboard forwarding
  | 2. Filter scrollback-clear sequences
  | 3. core.terminal.write(bytes) -> FFI: ghostty_terminal_vt_write()
  |    libghostty-vt parses ALL ANSI/VT sequences internally
  | 4. Update key encoder state from terminal modes
  | 5. Returns ProcessBytesResult { request_render, clipboard_writes }
  v
[App main loop]  (src/app/mod.rs:478-486)
  | wakes on render_notify or event
  | calls terminal.draw(|frame| { ... })
  v
[crate::ui::compute_view]  (src/ui.rs:87-88)
  | Pure geometry calculation, no drawing
  v
[crate::ui::render]  (src/ui.rs:238-298)
  | Reads &AppState, draws everything
  v
[render_panes]  (src/ui/panes.rs:105-173)
  | For each pane:
  |   - Draw border block
  |   - rt.render(frame, info.inner_rect, show_cursor)
  v
[GhosttyPaneTerminal::render]  (src/pane/terminal.rs:624-712)
  | 1. render_state.update(terminal) -- FFI snapshot
  | 2. Get default colors: render_state.colors()
  | 3. Create RowIterator + RowCells (reusable FFI handles)
  | 4. render_state.populate_row_iterator(&mut row_iterator)
  | 5. For each row (y) and each cell (x):
  |    a. rows.next()
  |    b. rows.populate_cells(&mut row_cells)
  |    c. cells.next()
  |    d. cells.wide() -> CellWide
  |    e. cells.style() -> CellStyle
  |    f. cells.fg_color() / cells.bg_color() -> Option<RgbColor>
  |    g. cells.graphemes_into(scratch) -> Vec<u32> codepoints
  |    h. Convert codepoints to String symbol
  |    i. Build ratatui::Style from CellStyle + colors
  |    j. buf[(area.x + x, area.y + y)].set_symbol(symbol)
  |       buf[(area.x + x, area.y + y)].set_style(style)
  | 6. Fill remaining area with default bg/fg
  | 7. Set cursor position if visible
  v
[Ratatui flush]
  | crossterm backend diffs buffer against previous frame
  | Emits minimal ANSI escape sequences to the host terminal
  v
Host terminal renders pixels
```

### Architecture Diagram

```
+------------------------------------------------------------------+
|                        HOST TERMINAL                               |
|                    (rendered by crossterm)                          |
+----------------------------+---------------------------------------+
                             | ANSI escape sequences (via crossterm)
+----------------------------v---------------------------------------+
|                       RATATUI                                      |
|  Frame -> Buffer -> diff -> emit only changed cells                |
|  Layout, widgets, borders, overlays, dialogs                       |
+----------------------------+---------------------------------------+
                             | Buffer cell writes (symbol + style)
+----------------------------v---------------------------------------+
|              HERDR RENDER PIPELINE                                  |
|                                                                     |
|  compute_view() -> geometry      render() -> drawing                |
|                                                                     |
|  PaneTerminal::render() reads cells from ghostty RenderState        |
|  and writes them into ratatui's buffer:                              |
|    cell.set_symbol(graphemes_as_string)                              |
|    cell.set_style(Style from CellStyle + RgbColor)                  |
+----------------------------+---------------------------------------+
                             | FFI calls (C ABI)
+----------------------------v---------------------------------------+
|                    LIBGHOSTTY-VT (Zig, static lib)                  |
|                                                                     |
|  ghostty_terminal_vt_write(bytes)  <- ANSI parsing + state mgmt    |
|  ghostty_render_state_update()     <- snapshot                      |
|  RowIterator -> RowCellIter          <- cell-by-cell access         |
|    .graphemes() -> Vec<u32>          (Unicode codepoints)            |
|    .style()     -> CellStyle         (bold/italic/... booleans)     |
|    .fg_color()  -> RgbColor          (resolved RGB)                 |
|    .bg_color()  -> RgbColor          (resolved RGB)                 |
|    .wide()      -> CellWide          (narrow/wide/spacer)           |
|  ghostty_formatter_terminal_new()   <- text/ANSI extraction         |
|  ghostty_key_encoder_encode()       <- kitty/legacy key encoding    |
|  ghostty_mouse_encoder_encode()     <- mouse protocol encoding      |
+----------------------------^---------------------------------------+
                             | PTY output bytes
+----------------------------+---------------------------------------+
|                     PTY (portable-pty)                              |
|  Reader thread reads PTY fd -> process_pty_bytes -> ghostty write   |
|  Writer thread receives input bytes -> PTY fd write                 |
|  Shell / agent process (child)                                      |
+--------------------------------------------------------------------+
```

### Key Files in Herdr

#### Core rendering pipeline

| File | Role |
|---|---|
| `Cargo.toml` | ratatui 0.30, crossterm 0.29, portable-pty 0.9 |
| `build.rs` | Compiles libghostty-vt via `zig build`, links static library |
| `src/main.rs` | Entry point, `ratatui::init()`, main loop |
| `src/app/mod.rs` | `App::run()` -- main event loop |
| `src/ui.rs` | `compute_view()` (geometry) and `render()` (drawing) |

#### Terminal engine (libghostty-vt integration)

| File | Role |
|---|---|
| `src/ghostty/mod.rs` | Safe Rust wrappers: `Terminal`, `RenderState`, `RowIterator`, `RowCells`, `RowCellIter`, `KeyEncoder`, `MouseEncoder`, `CellStyle`, `RgbColor`, `CellWide` |
| `src/ghostty/bindings.rs` | Auto-generated bindgen FFI declarations (2507 lines) |
| `vendor/libghostty-vt/` | Vendored Zig source (v1.3.2) |
| `vendor/libghostty-vt/include/ghostty/vt/` | C headers: terminal.h, render.h, key/, mouse/, formatter.h, etc. |

#### Pane management and terminal I/O

| File | Role |
|---|---|
| `src/pane.rs` | `PaneRuntime` -- owns PTY, spawns reader/writer/detect tasks |
| `src/pane/terminal.rs` | `GhosttyPaneTerminal` -- core bridge between libghostty-vt and ratatui |
| `src/pane/state.rs` | `PaneState` -- pure data state for agent detection |
| `src/pane/osc.rs` | OSC sequence handling |
| `src/pane/input.rs` | Key/mouse event translation |

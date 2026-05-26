# Migration Plan: ansi-to-tui -> libghostty-vt

## Overview

Replace `ansi-to-tui` crate with `libghostty-vt` for rendering agent and git views in stable. Each tick creates a fresh `Terminal`, feeds the `capture-pane` output, and renders via ghostty's cell-grid API.

## Source

Herdr's filtered vendor from `ghostty-org/ghostty` commit `063ac3ecc` (post-v1.3.1 snapshot, version string `1.3.2-main-+063ac3ecc`). Pre-generated bindings match the vendored headers exactly.

### Version Context

| | Herdr's Vendor | Latest `tip` | Latest Stable |
|---|---|---|---|
| **Commit** | `063ac3ecc` | `2e5ad91` | `v1.3.1` (`332b2ae`) |
| **Date** | Post-v1.3.1 snapshot | May 26, 2026 | Mar 13, 2026 |
| **Commits ahead of herdr** | baseline | 127 newer | ~same era |

Only one new header added since herdr's snapshot (`grid_ref_tracked.h`). Core API (`terminal.h`, `render.h`, `types.h`) is stable.

---

## Architecture

### Current Pipeline (ansi-to-tui)

```
tmux capture-pane -p -e
  -> raw ANSI string
  -> AgentViewState::update_lines(raw) splits into Vec<String>
  -> render: slice visible window, join with \n
  -> extract_first_bg_color() hand-rolled SGR scanner
  -> ansi-to-tui fork: .into_text_with_style(base_style) -> ratatui::Text
  -> Paragraph widget renders into ratatui buffer
```

### New Pipeline (libghostty-vt)

```
tmux capture-pane -p -e
  -> raw ANSI string
  -> AgentViewState::update_lines(raw) splits into Vec<String> (unchanged)
  -> render: slice visible window, join with \n
  -> ghostty::render::render_pane_content():
       1. Terminal::new(w, h, 0) - fresh terminal
       2. terminal.write(ansi_bytes) - ghostty parses all ANSI internally
       3. RenderState::update(&terminal) - snapshot cell grid
       4. Walk row/cell iterators -> write ratatui buffer cells directly
  -> No Paragraph widget needed, cells written directly into buffer
```

### Key Differences

- **No more `extract_first_bg_color()`** - ghostty resolves all colors natively to RGB
- **No more `ansi-to-tui` fork** - ghostty handles all ANSI/VT parsing
- **No Paragraph widget** - cells written directly into ratatui buffer
- **Proper wide char support** - CJK/emoji handled via `CellWide` enum
- **All colors resolved to 24-bit RGB** - no palette indirection

---

## File Changes

| File | Action | Notes |
|---|---|---|
| `vendor/libghostty-vt/` | Copy from herdr | 18MB filtered ghostty source |
| `vendor/libghostty-vt.vendor.json` | Copy from herdr | Provenance metadata |
| `build.rs` | Create | Zig build integration (copy from herdr) |
| `Cargo.toml` | Modify | Remove `ansi-to-tui`, add `unicode-width`, add `build` |
| `src/ghostty.rs` | Create | Module root with safe wrappers (~400 lines) |
| `src/ghostty/bindings.rs` | Create (copy from herdr) | Pre-generated FFI (2507 lines) |
| `src/ghostty/render.rs` | Create | Shared rendering helper (~100 lines) |
| `src/main.rs` | Modify | Add `mod ghostty;` |
| `src/ui/agent_view.rs` | Modify | Replace ansi-to-tui with ghostty render |
| `src/ui/git_viewer.rs` | Modify | Replace ansi-to-tui with ghostty render |

### Detailed Changes

#### `Cargo.toml`

```diff
- ansi-to-tui = { git = "https://github.com/grouzen/ansi-to-tui", branch = "allow-setting-style-on-init-and-reset" }
+ unicode-width = "0.2"
```

Add `build = "build.rs"` to `[package]`.

#### `build.rs`

Copy from herdr's `build.rs` (73 lines). Compiles `vendor/libghostty-vt/` via `zig build -Demit-lib-vt`, links static library. Handles target mapping (linux-gnu, linux-musl, macos).

#### `src/ghostty.rs` (module root)

Follows repo convention: flat file + sibling directory (`ghostty.rs` + `ghostty/`).

Contains:
- `pub mod bindings;` and `pub mod render;`
- Safe Rust wrappers: `Terminal`, `RenderState`, `RowIterator`, `RowCells`, `RowCellIter`
- Data types: `CellStyle`, `RgbColor`, `CellWide`, `RenderColors`, `CursorViewport`, `Error`
- Rendering helpers (inlined from herdr's `pane/terminal.rs`):
  - `ghostty_cell_style()` - converts ghostty `CellStyle` + colors to `ratatui::Style`
  - `ghostty_buffer_symbol_into()` - converts grapheme codepoints to ratatui cell symbol
  - `ghostty_reset_cell()` - fills remaining cells with default bg/fg

**Not included** (not needed since stable doesn't own the PTY):
- `KeyEncoder`, `KeyEvent` - stable forwards keys via tmux send-keys
- `MouseEncoder`, `MouseEvent` - stable forwards mouse via tmux SGR sequences
- `FocusEvent`, `encode_focus()` - no focus tracking needed
- `Terminal::set_write_pty_callback()` - no PTY
- `Terminal::resize()` - tmux handles pane sizing
- `Terminal::mode_get/set` - no mode tracking needed
- `Terminal::scroll_viewport_*`, `scrollbar()` - tmux handles scrollback
- `Terminal::active_screen()` - no alternate screen tracking
- Selection/hyperlink/formatter APIs

This cuts the wrapper from herdr's ~1547 lines to ~400 lines.

#### `src/ghostty/bindings.rs`

Direct copy from herdr. Pre-generated bindgen output (2507 lines). Matches the vendored C headers exactly. No build-time regeneration needed.

#### `src/ghostty/render.rs`

Single public function:

```rust
pub fn render_pane_content(
    ansi_bytes: &[u8],
    frame: &mut Frame,
    area: Rect,
    cursor: Option<(u16, u16)>,
)
```

Implementation:
1. Create `Terminal::new(area.width, area.height, 0)` - fresh terminal, no scrollback
2. `terminal.write(ansi_bytes)` - ghostty parses all ANSI internally
3. `RenderState::new()` + `render_state.update(&terminal)` - snapshot cell grid
4. Get default colors from `render_state.colors()`
5. Walk row/cell iterators:
   - For each cell: get `CellWide`, `CellStyle`, fg/bg colors, graphemes
   - Convert to ratatui symbol + style
   - Write directly into `buf[(area.x + x, area.y + y)]`
6. Fill remaining cells with default bg/fg
7. Set cursor position if provided and within bounds

#### `src/main.rs`

Add `mod ghostty;` to module declarations.

#### `src/ui/agent_view.rs`

Remove:
- `use ansi_to_tui::IntoText;`
- `extract_first_bg_color()` function (~70 lines)
- The `into_text_with_style()` + `Paragraph` rendering block

Replace with:
```rust
crate::ghostty::render::render_pane_content(
    visible_text.as_bytes(),
    f,
    content_area,
    cursor_position, // from state.cursor, adjusted for border
);
```

All other rendering (top bar, status bar, stopped overlay) stays unchanged.

#### `src/ui/git_viewer.rs`

Same pattern as agent_view:
- Remove `use ansi_to_tui::IntoText;`
- Remove `extract_first_bg_color()` function (~50 lines, duplicate)
- Replace with `ghostty::render::render_pane_content()`

#### `src/app.rs` (AgentViewState / GitViewerState)

Minimal changes:
- Keep `lines: Vec<String>` - still needed for scroll-slice logic (`lines[start..end]`)
- Keep `prev_raw`, `prev_raw_len` - change detection unchanged
- `update_lines()` stays the same
- No structural state changes

---

## Implementation Order

1. Copy `vendor/libghostty-vt/` and `vendor/libghostty-vt.vendor.json` from herdr
2. Create `build.rs` (copy from herdr)
3. Update `Cargo.toml` (remove ansi-to-tui, add unicode-width, add build directive)
4. Create `src/ghostty/bindings.rs` (copy from herdr)
5. Create `src/ghostty.rs` (adapted from herdr's `src/ghostty/mod.rs`, trimmed)
6. Create `src/ghostty/render.rs` (new shared rendering helper)
7. Update `src/main.rs` (add `mod ghostty;`)
8. Rewrite `src/ui/agent_view.rs` rendering
9. Rewrite `src/ui/git_viewer.rs` rendering
10. Build and verify

---

## Benefits

1. **Eliminates custom fork dependency** - no more pinning to `grouzen/ansi-to-tui` fork
2. **Removes duplicated `extract_first_bg_color`** - ~120 lines of duplicated SGR parsing across two files
3. **Better rendering fidelity** - ghostty handles all ANSI/VT edge cases (alternate screen, scroll regions, character sets, etc.)
4. **Proper wide character support** - CJK, emoji handled correctly via `CellWide` enum
5. **All colors resolved to RGB** - no palette indirection, no hand-rolled 256-color lookups
6. **Matches herdr's proven approach** - same rendering pipeline used in production

## Risks and Mitigations

| Risk | Mitigation |
|---|---|
| **Build complexity** - requires Zig toolchain for developers and CI | Herdr's build.rs handles this cleanly; CI needs `zig` in PATH |
| **Vendor size** - 18MB of Zig source in repo | Herdr already filters to only libghostty-vt parts |
| **Double parsing** - tmux parses ANSI, then ghostty parses the capture output | Acceptable overhead; herdr proves the performance is fine |
| **API breakage on upgrade** - ghostty C API is not yet stable | Pin to specific commit, upgrade deliberately with bindings regeneration |
| **Cursor position** - current tmux cursor_position may not match ghostty's | Use tmux's cursor position (from display-message) as the source of truth |

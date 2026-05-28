# Migration Plan: Stable Rendering Pipeline → Herdr-style

## Context

Stable (`migrate-to-libghostty` branch) and herdr both use libghostty-vt for terminal rendering, but herdr's rendering pipeline is more sophisticated. This plan aligns stable's rendering with herdr's while keeping stable's tmux-based architecture and Gruvbox Dark UI constants.

All required FFI symbols are present in stable's `bindings.rs` — no binding regeneration needed.

---

## Phase 1: Type Enhancements in `ghostty.rs`

**Goal:** Add the missing types that herdr uses for richer color resolution.

**Changes to `src/ghostty.rs`:**

1. **Add `CellColor` enum** (mirrors herdr `ghostty/mod.rs:316-320`):
   ```rust
   pub enum CellColor {
       Palette(u8),
       Rgb(RgbColor),
   }
   ```

2. **Add `From<ffi::GhosttyStyleColor> for Option<CellColor>`** — convert the FFI tagged union into the Rust enum, handling `NONE`/`PALETTE`/`RGB` variants.

3. **Update `CellStyle` struct** (currently 9 bools at lines 98-109) to include:
   ```rust
   pub fg_color: Option<CellColor>,
   pub bg_color: Option<CellColor>,
   pub underline_color: Option<CellColor>,
   ```

4. **Update `From<ffi::GhosttyStyle> for CellStyle`** (lines 111-125) to populate the new color fields from `value.fg_color`, `value.bg_color`, `value.underline_color`.

5. **Add `content_bg_color()` method on `RowCellIter`** (mirrors herdr `ghostty/mod.rs:2293-2332`) — reads the cell's content tag via `ghostty_cell_get`, returns `Option<CellColor>` for `BG_COLOR_PALETTE` / `BG_COLOR_RGB` content types.

6. **Add `ghostty_cell_color()` function** (mirrors herdr `pane/terminal.rs:1388-1393`):
   ```rust
   fn ghostty_cell_color(color: CellColor) -> Color {
       match color {
           CellColor::Palette(index) => Color::Indexed(index),
           CellColor::Rgb(color) => ghostty_color(color),
       }
   }
   ```

---

## Phase 2: Three-tier Cell Style Resolution

**Goal:** Match herdr's color resolution priority chain.

**Changes to `src/ghostty.rs`:**

Rewrite `ghostty_cell_style()` (currently lines 615-677) to match herdr's version (`pane/terminal.rs:1228-1295`):

- **Foreground resolution** (currently 2-tier, becomes 3-tier):
  1. `style_data.fg_color` (SGR style, can be Palette or RGB) — **NEW**
  2. `cells.fg_color()` (resolved RGB from libghostty) — existing
  3. `default_fg` (terminal default or None = transparent) — existing param, currently always None

- **Background resolution** (currently 2-tier, becomes 4-tier):
  1. `cells.content_bg_color()` (cell content bg) — **NEW**
  2. `style_data.bg_color` (SGR style) — **NEW**
  3. `cells.bg_color()` (resolved RGB) — existing
  4. `default_bg` (terminal default or None = transparent) — existing param, currently always None

- **Add `resolved_fg` parameter** (4th parameter, currently only `resolved_bg` exists). Herdr passes both `resolved_fg` and `resolved_bg` to handle inverse correctly when defaults are None.

---

## Phase 3: Host Terminal Theme Detection

**Goal:** Query the outer terminal's default fg/bg colors so we can use `Color::Reset` for transparency.

**New file: `src/terminal_theme.rs`** — port from herdr (142 lines):
- `RgbColor`, `TerminalTheme`, `DefaultColorKind` types
- `HOST_COLOR_QUERY_SEQUENCE` constant (`\x1b]10;?\x1b\\\x1b]11;?\x1b\\`)
- `parse_default_color_response()` — parse OSC 10/11 responses from stdin
- `osc_set_default_color_sequence()` — generate OSC 10/11 set sequences
- Unit tests for parsing

**Changes to `src/app.rs`:**

1. Add `host_terminal_theme: TerminalTheme` field to `App` struct.

2. In `spawn_tasks()` or at startup, write `HOST_COLOR_QUERY_SEQUENCE` to stdout to query the outer terminal.

3. In the crossterm event reader task, detect OSC 10/11 responses in the raw stdin bytes before they become key events. Parse them with `parse_default_color_response()` and send a new `Event::HostDefaultColor { kind, color }` variant.

4. Add `Event::HostDefaultColor` variant to the `Event` enum.

5. Handle `Event::HostDefaultColor` in `handle_event()` — update `self.host_terminal_theme` and set `dirty = true`.

**Risk:** crossterm's `EventStream` may consume OSC responses before we can see them. Test early. Fallback: use a raw stdin reader thread like herdr's `raw_input.rs`.

---

## Phase 4: Transparency in `render_pane_content()`

**Goal:** Remove the manual background hacks and use herdr's transparency mechanism.

**Changes to `src/ghostty/render.rs`:**

1. **Accept `host_theme: TerminalTheme` parameter** instead of `theme_bg: Option<Color>`.

2. **Remove `extract_first_bg_color()`** function entirely (lines 133-197) — no longer needed.

3. **Remove the background pre-fill loop** (lines 38-47) — transparency handles this.

4. **Remove OSC 11 injection** (lines 56-59) — the default color comes from the terminal naturally.

5. **Add default color resolution** after `render_state.update()`:
   ```rust
   let default_bg = colors.and_then(|c|
       ghostty_default_bg(c.background, host_theme, None));
   let default_fg = colors.and_then(|c|
       ghostty_default_fg(c.foreground, host_theme, None));
   let resolved_fg = colors.map(|c| ghostty_color(c.foreground));
   let resolved_bg = colors.map(|c| ghostty_color(c.background));
   ```

6. **Pass all 4 color params** to `ghostty_cell_style()`:
   ```rust
   ghostty_cell_style(&cells, default_fg, default_bg, resolved_fg, resolved_bg)
   ```

7. **Add remaining-cell fill** (like herdr's `ghostty_reset_cell()`) for rows/cols that extend beyond the terminal content:
   ```rust
   while x < inner.width {
       ghostty_reset_cell(cell, default_fg, default_bg);
       x += 1;
   }
   ```

8. **Port `ghostty_default_bg()` and `ghostty_default_fg()`** functions from herdr (`pane/terminal.rs:1344-1378`). These compare the ghostty terminal's default color against the host theme — if they match, return `None` (Color::Reset = transparent).

9. **Port `ghostty_reset_cell()`** function from herdr (`pane/terminal.rs:1213-1226`).

**Update callers:**
- `agent_view.rs:66` — pass `app.host_terminal_theme` instead of `None`
- `git_viewer.rs:35` — pass `app.host_terminal_theme` instead of `Some(BG)`

---

## Phase 5: Minor Improvements (Optional)

1. **Kitty graphics placeholder hiding** — Add `KITTY_UNICODE_PLACEHOLDER` constant and check in `ghostty_buffer_symbol_into()` (herdr `pane/terminal.rs:1178-1180`). Prevents rendering artifacts if kitty graphics are used.

2. **Palette field in `RenderColors`** — Extract the full 256-color palette from `GhosttyRenderStateColors.palette` for potential future use. Low priority since `cells.fg_color()`/`bg_color()` already return resolved RGB.

---

## Out of Scope

| Aspect | Reason |
|--------|--------|
| tmux polling → direct PTY | Different architecture, out of scope |
| Event-driven render (Notify) | Requires PTY ownership |
| Persistent Terminal per pane | Incompatible with tmux capture model |
| herdr's Palette/theme system (18 themes) | Keeping Gruvbox constants |
| Wire protocol (client-server) | Not applicable to stable |
| Pane splits / multi-pane | Different app structure |

---

## Risk Assessment

| Risk | Mitigation |
|------|-----------|
| crossterm consumes OSC 10/11 responses before we can parse them | Test early in Phase 3. Fallback: raw stdin reader thread like herdr |
| `extract_first_bg_color()` removal breaks git_viewer (passes `Some(BG)`) | Host theme detection + transparency replaces this. If detection fails, `Color::Reset` still works correctly |
| `GhosttyStyle.fg_color` is `NONE` for most cells (no explicit SGR) | 3-tier fallback handles this: falls through to `cells.fg_color()` → `default_fg` |

---

## Implementation Order

Phase 1 → 2 → 3 → 4 → 5 (sequential, each depends on the previous)

Phases 1-2 are pure `ghostty.rs` changes with no behavioral impact (callers still pass `None` for defaults). Phase 3 is the integration point. Phase 4 activates the new pipeline. Phase 5 is optional polish.

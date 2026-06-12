# `libghostty-vt` Migration Implementation Steps

## Goal

Migrate Flowmux from the vendored Ghostty integration to the safe Rust crate:

- Repository: `https://github.com/uzaaft/libghostty-rs`
- Revision: `20edad15d7984c727acc4f4facdadf045609f543`
- Ghostty revision pinned by that commit: `bfe633a9487892ff3d27ed727db540267f22ef90`

The migration must preserve current rendering behavior, host foreground/background color handling, wide-character behavior, and static release binaries.

## Execution Rules

- Keep `render_pane_content` as the internal integration boundary for all pane views.
- Do not introduce persistent terminal emulator state in this migration.
- Do not move Ghostty objects into async tasks, shared state, or other threads. The crate types are `!Send + !Sync`.
- Keep static linking as the default. Do not enable `link-dynamic`.
- Pin the crate by exact Git revision, not by branch or crates.io release.

## Step 1: Lock the dependency and remove the custom build path

### Changes

- Update `Cargo.toml`:
  - remove `build = "build.rs"`
  - add pinned Git dependency on `libghostty-vt`
  - disable default features
- Regenerate `Cargo.lock`
- Delete:
  - `build.rs`
  - `vendor/libghostty-vt.vendor.json`

### Files

- `Cargo.toml`
- `Cargo.lock`
- `build.rs`
- `vendor/libghostty-vt.vendor.json`

### Verification

- `cargo check --locked`
- Confirm Cargo resolves `libghostty-vt` and `libghostty-vt-sys` from the pinned Git revision.

## Step 2: Remove the vendored Ghostty source and raw bindings

### Changes

- Delete the vendored source tree `vendor/libghostty-vt`
- Delete generated bindings `src/ghostty/bindings.rs`
- Remove all references to vendored build inputs and generated bindings from the codebase

### Files

- `vendor/libghostty-vt/`
- `src/ghostty/bindings.rs`

### Verification

- `rg "vendor/libghostty-vt|bindings.rs|LIBGHOSTTY_VT_" .`
- `cargo check --locked`

## Step 3: Replace the handwritten FFI wrapper with direct crate usage

### Changes

- Rewrite `src/ghostty.rs` so it no longer owns raw handles, custom errors, manual `Drop`, or unsafe `Send` impls
- Keep only:
  - the module boundary
  - Flowmux-specific helpers that still make sense locally
  - re-exports only if needed to keep call sites clean
- Import crate types directly:
  - `Terminal`
  - `TerminalOptions`
  - `RenderState`
  - `render::{RowIterator, CellIterator}`
  - `screen::CellWide`
  - `style::{RgbColor, Underline}`

### Files

- `src/ghostty.rs`

### Verification

- `cargo check --locked`
- Confirm there are no remaining references to local FFI symbols or raw Ghostty C types.

## Step 4: Port the renderer to the snapshot API

### Changes

- Update `src/ghostty/render.rs` to use crate objects directly:
  1. create `Terminal::new(TerminalOptions { ... })`
  2. set host colors with `set_default_fg_color` and `set_default_bg_color`
  3. feed data with `vt_write`
  4. call `RenderState::update` and keep the snapshot alive for iteration
  5. create/update `RowIterator`
  6. create/update `CellIterator`
- Replace local row/cell wrapper calls with:
  - `cell.raw_cell()?.wide()`
  - `cell.style()`
  - `cell.fg_color()`
  - `cell.bg_color()`
  - `cell.graphemes_utf8(&mut scratch)`
- Preserve current width validation and blank fallback behavior for invalid or unexpected grapheme output
- Preserve best-effort error handling: failed Ghostty operations should fail closed in rendering, not crash the app

### Files

- `src/ghostty/render.rs`

### Verification

- `cargo check --locked`
- Existing pane views still compile without signature changes.

## Step 5: Preserve current style and color semantics

### Changes

- Reimplement Flowmux’s ratatui style mapping using crate `Style`
- Preserve existing behavior for:
  - default foreground/background resolution
  - inverse handling
  - invisible text handling
  - wide characters
  - spacer head and spacer tail behavior
- Map underline variants to ratatui underline whenever `Underline != None`
- Continue ignoring overline because ratatui has no equivalent

### Files

- `src/ghostty/render.rs`
- `src/ghostty.rs` if helper functions remain there

### Verification

- `cargo test --locked`
- Visual smoke test with ANSI styles, truecolor, 256-color palette entries, wide characters, and trailing background color.

## Step 6: Add characterization tests around renderer behavior

### Changes

- Keep the existing ANSI visible-width and padding tests
- Add focused tests for behavior that must not regress:
  - ASCII and empty cells
  - CRLF handling
  - malformed UTF-8 fallback
  - combining graphemes
  - emoji and CJK wide cells
  - spacer head and spacer tail rendering
  - inverse and invisible styling
  - host default colors and explicit per-cell overrides
  - trailing spaces carrying background color
  - incomplete rows clearing stale ratatui buffer content

### Files

- `src/ghostty/render.rs`
- Additional test module/file only if needed

### Verification

- `cargo test --locked`

## Step 7: Update documentation and build expectations

### Changes

- Update `README.md`:
  - remove vendored Ghostty references
  - remove `build.rs` and old env var references
  - document Rust 1.90+, Zig 0.15.x, Git, first-build network access
  - document `LIBGHOSTTY_VT_SYS_OPTIMIZE`
  - document `GHOSTTY_SOURCE_DIR`
  - document `GHOSTTY_ZIG_SYSTEM_DIR`
- Update `docs/ARCHITECTURE.md`:
  - describe the crate-based integration
  - remove references to generated bindings and vendored build flow
  - keep the rendering pipeline description accurate

### Files

- `README.md`
- `docs/ARCHITECTURE.md`

### Verification

- `rg "vendored|bindings.rs|LIBGHOSTTY_VT_OPTIMIZE|LIBGHOSTTY_VT_SIMD|build.rs" README.md docs src`

## Step 8: Final cleanup and release verification

### Changes

- Remove any dead imports, helpers, or comments left from the old wrapper
- Confirm no runtime shared-library packaging has been introduced
- Confirm repository no longer tracks the vendored Ghostty subtree

### Verification

- `cargo check --locked`
- `cargo test --locked`
- `cargo build --release --locked`
- `cargo clippy --all-targets --locked`
- `git diff --check`
- `ldd target/release/flowmux` on Linux or `otool -L target/release/flowmux` on macOS
- Manual smoke test:
  - Agent View
  - Git Viewer
  - Persistent Terminal
  - scrolling
  - cursor visibility
  - resizes
  - full-screen terminal apps
  - colored output

## Recommended Commit Boundaries

1. Dependency switch and build-system removal
2. Vendored source/bindings removal
3. Wrapper replacement and renderer port
4. Characterization tests
5. Documentation and cleanup

## Done Criteria

- Flowmux builds and tests pass with the pinned `libghostty-vt` Git revision.
- No vendored Ghostty source or generated local bindings remain in the repo.
- Rendering behavior is materially unchanged for current supported pane views.
- Release binaries do not depend on `libghostty-vt.so` or `.dylib` at runtime.

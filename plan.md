# Migrate to `libghostty-vt` Rust Crate

## Summary

Replace Flowmux's vendored Ghostty source, generated bindings, custom build script, and handwritten FFI wrappers with the safe `libghostty-vt` API from:

- Repository: `https://github.com/uzaaft/libghostty-rs`
- Revision: `20edad15d7984c727acc4f4facdadf045609f543`
- Ghostty revision pinned by that commit: `bfe633a9487892ff3d27ed727db540267f22ef90`

Preserve existing terminal rendering, host foreground/background colors, wide-character handling, styling, and standalone static binaries.

## Implementation Changes

### Dependency and Build

- Add the exact Git dependency:
  ```toml
  libghostty-vt = {
      git = "https://github.com/uzaaft/libghostty-rs",
      rev = "20edad15d7984c727acc4f4facdadf045609f543",
      default-features = false
  }
  ```
- Disable default Kitty graphics support because Flowmux does not render terminal graphics.
- Regenerate `Cargo.lock`, ensuring both `libghostty-vt` and `libghostty-vt-sys` resolve from the pinned revision.
- Remove `build = "build.rs"` and delete Flowmux's custom `build.rs`.
- Delete `vendor/libghostty-vt`, its metadata file, and the generated `src/ghostty/bindings.rs`.
- Rely on `libghostty-vt-sys` to fetch and statically build its pinned Ghostty source with Zig.
- Replace old environment variables:
  - Remove documentation for `LIBGHOSTTY_VT_OPTIMIZE` and `LIBGHOSTTY_VT_SIMD`.
  - Document `LIBGHOSTTY_VT_SYS_OPTIMIZE` for build-mode overrides.
  - Document `GHOSTTY_SOURCE_DIR` for local or offline Ghostty source.
  - Document `GHOSTTY_ZIG_SYSTEM_DIR` for pre-fetched Zig packages.
- Require Rust 1.90 or newer, Zig 0.15.x, Git, and network access on the first default build.
- Keep static linking as the default; do not enable `link-dynamic` or add shared-library packaging.

### Rust Integration

- Reduce `src/ghostty.rs` to the Flowmux rendering module declaration and any genuinely Flowmux-specific helpers; remove all raw handles, manual `Drop`, error conversion, and unsafe `Send` implementations.
- Import crate types directly:
  - `Terminal` and `TerminalOptions`
  - `RenderState`
  - `render::{RowIterator, CellIterator}`
  - `screen::CellWide`
  - `style::{RgbColor, Underline}`
- Construct terminals with `Terminal::new(TerminalOptions { ... })` and feed output through `vt_write`.
- Set host colors through `set_default_fg_color(Some(RgbColor { ... }))` and `set_default_bg_color(...)`.
- Adapt rendering to the crate's snapshot API:
  1. Update `RenderState` and retain the returned snapshot.
  2. Read colors from the snapshot.
  3. Update a `RowIterator` from the snapshot.
  4. Update a `CellIterator` for each row.
- Determine width with `cell.raw_cell()?.wide()`.
- Read graphemes through `graphemes_utf8` into a reusable cleared `String`; retain Flowmux's width validation and blank fallback for malformed or unexpected clusters.
- Read resolved cell colors through `fg_color` and `bg_color`.
- Map crate `Style` fields to ratatui modifiers:
  - Bold, italic, faint, blink, strikethrough.
  - Any underline variant other than `Underline::None` maps to ratatui underline.
  - Preserve current inverse and invisible color-resolution behavior.
  - Continue ignoring overline because ratatui has no equivalent modifier.
- Preserve `render_pane_content`'s signature and best-effort behavior so agent, terminal, and git viewer call sites remain unchanged.
- Keep Ghostty objects scoped to synchronous rendering on the UI thread. Do not store them in `App`, async tasks, channels, or thread-shared state because crate handles are `!Send + !Sync`.

### Rendering Structure and Cleanup

- Keep ANSI line padding and visible-width calculation as Flowmux-specific preprocessing.
- Factor the Ghostty-to-ratatui conversion into a testable internal function that receives terminal dimensions/content and writes to a ratatui buffer.
- Reuse iterator and grapheme scratch allocations within each rendered frame.
- Do not introduce persistent emulator state in this migration; Flowmux currently reconstructs the viewport from each tmux capture, and changing that would alter scrolling and refresh semantics.
- Update README and architecture documentation to describe the Rust crate, pinned revision, static linking, build-time fetching, and removal of local FFI ownership.
- Remove references to the vendored source tree, generated bindings, custom Zig target mapping, and old wrapper types.

## Interfaces and Compatibility

- No user-facing configuration or keybinding changes.
- `render_pane_content` remains the internal integration boundary used by all three pane views.
- Supported targets remain Linux GNU, Linux musl, and macOS on x86-64/AArch64.
- The dependency additionally supports Windows, but Windows support is not added to Flowmux as part of this migration.
- The repository shrinks by approximately 23 MB and 1,247 tracked vendor files.
- Future upgrades require changing the pinned Git revision and reviewing the crate's unstable API and its pinned Ghostty revision together.

## Test Plan

- Add characterization tests before replacing the old implementation, then run the same cases against the crate implementation.
- Cover plain ASCII, empty cells, line padding, CRLF input, malformed UTF-8, combining graphemes, emoji, CJK wide cells, spacer heads, and spacer tails.
- Verify SGR rendering for true color, 256-color palette entries, bold, italic, faint, blink, underline variants, strikethrough, inverse, invisible, reset sequences, and colored trailing spaces.
- Verify host foreground/background defaults and explicit cell colors override those defaults.
- Verify incomplete rows and failed cell extraction are filled with correctly styled blank cells rather than stale ratatui buffer content.
- Retain existing ANSI visible-width and padding unit tests.
- Run:
  - `cargo check --locked`
  - `cargo test --locked`
  - `cargo build --release --locked`
  - `cargo clippy --all-targets --locked`
  - `git diff --check`
- Smoke-test Agent View, Git Viewer, and Persistent Terminal with interactive full-screen programs, colored output, scrolling, cursor visibility, wide characters, and terminal resizes.
- Inspect the release binary with `ldd` on Linux or `otool -L` on macOS and confirm there is no runtime dependency on `libghostty-vt.so` or `.dylib`.

## Assumptions

- The exact Git revision is intentionally preferred over crates.io `0.1.1` because it provides safe default-color setters, static linking, improved build controls, and a lower MSRV.
- Network fetching during the first build is acceptable; offline and packaged builds will use the documented source/cache environment variables.
- Host-color behavior and current ratatui rendering semantics must remain unchanged.
- This migration removes local FFI maintenance rather than replacing it with a compatibility wrapper or patched fork.

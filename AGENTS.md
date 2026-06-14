# Repository Guidelines

## Project Structure & Module Organization
`src/` contains the main application. `main.rs` bootstraps the CLI and session startup, `app.rs` owns the event loop and UI state machine, `runner.rs` manages agent lifecycle, and `tmux.rs` wraps tmux operations. Agent-specific adapters live in `src/agents/`, UI renderers in `src/ui/`, and Ghostty VT integration in `src/ghostty.rs` and `src/ghostty/`. Supporting docs are in `docs/`, demo assets in `docs/demo/`, and helper scripts in `tools/`. `tools/model-gen/` is a small utility crate with its own `Cargo.toml`.

## Build, Test, and Development Commands
Use Rust 1.90+, Zig 0.15.x, `git`, and `tmux`.

- `cargo build --locked` builds the debug binary.
- `cargo build --release --locked` produces `target/release/flowmux`.
- `cargo test` runs the inline unit tests across the crate.
- `cargo fmt` formats the codebase with rustfmt.
- `cargo clippy --all-targets --all-features` checks for common Rust issues.
- `./tools/build-debug-prefetched-libghostty-vt.sh` builds a debug binary using prefetched Ghostty inputs.
- `./tools/build-release-prefetched-libghostty-vt.sh` performs a reproducible release build using prefetched Ghostty inputs when available.

## Coding Style & Naming Conventions
Follow standard Rust formatting: 4-space indentation, snake_case for functions/modules, PascalCase for types, and SCREAMING_SNAKE_CASE for constants. Keep modules focused by concern (`agents/claude.rs`, `ui/dashboard.rs`). Prefer brief, explanatory comments only where behavior is non-obvious. Run `cargo fmt` before submitting changes.

## Testing Guidelines
This repository uses inline unit tests under `#[cfg(test)]` instead of a top-level `tests/` directory. Add tests next to the code they verify, especially for parser, rendering, config, and adapter logic. Name tests after the behavior they prove, for example `tmux_pane_viewport_size_saturates_for_tiny_terminals`. Run `cargo test` locally before opening a PR.

## Commit & Pull Request Guidelines
Recent history uses Conventional Commit-style subjects such as `feat(ux): ui/ux enhancements (#54)` and `fix(ui): terminal viewport size (#51)`. Follow the same pattern: `<type>(<scope>): <summary>`, using imperative phrasing and a narrow scope. Pull requests should include a short problem statement, a summary of the change, linked issues when relevant, and screenshots or screencasts for visible TUI changes.

## Configuration & Environment Tips
Global config lives at `~/.config/flowmux/config.toml`; per-session state is stored under `~/.config/flowmux/sessions/`. Keep local paths, agent credentials, and machine-specific tmux settings out of commits.

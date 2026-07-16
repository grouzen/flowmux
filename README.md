<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="docs/brand/flowmux-logo-dark.png">
    <source media="(prefers-color-scheme: light)" srcset="docs/brand/flowmux-logo.png">
    <img src="docs/brand/flowmux-logo.png" alt="Flowmux" width="720">
  </picture>
</p>

<p align="center">
  <a href="https://flowmux.dev">flowmux.dev</a>
</p>

<p align="center">
  Where agent chaos becomes flow.
</p>

# Flowmux

Flowmux is a terminal-native AI agent multiplexer for running, tracking, and switching between multiple CLI agents from one keyboard-first dashboard.

It is built for people who want fast hotkeys, a clean grid view of active work, tmux-backed persistence, and real terminal sessions instead of wrapped agent UIs.

Flowmux follows a simple Unix-style approach: it coordinates agent sessions, panes, projects, and worktrees while leaving your editor, git tools, shell, and terminal habits intact.

[▶ Watch the Flowmux screencast](docs/demo/screencast-v0.4.0.webm)

## Table of Contents

- [Quick Start](#quick-start)
- [Core Concepts](#core-concepts)
- [Typical Workflow](#typical-workflow)
- [Features](#features)
- [Installation](#installation)
- [Usage](#usage)
- [Configuration](#configuration)
- [Supported Agents](#supported-agents)
- [Architecture](#architecture)
- [Contributing](#contributing)

## Quick Start

### Prerequisites

- `tmux`
- At least one supported agent CLI: `opencode`, `claude`, `codex`, or `pi`
- [Rust 1.90+](https://rustup.rs/), [Zig 0.15.x](https://ziglang.org/), and `git` if building from source

### Launch

Run Flowmux from your terminal:

```bash
flowmux
```

On first launch, Flowmux opens a tmux-backed dashboard where you can create agents, group them by project, and jump between running, waiting, and idle work without leaving the terminal.

## Core Concepts

**Projects.** Projects are top-level dashboards. They let you separate work by repo, task, or stream and switch between them quickly.

**Agents.** Each agent runs in its own tmux pane, with its own working directory and optional git worktree.

**Dashboard.** The dashboard is the overview screen: a grid of agents with status, model information, and the latest response preview.

**Agent View.** Agent view shows the live terminal for one agent. Keys are forwarded to the pane, so you interact with the real CLI session.

**Persistence.** Flowmux stores session state and can reconnect to agents after tmux restarts, so long-running work is easier to resume.

## Typical Workflow

1. Create a project if you want to separate this work from other dashboards.
2. Add one or more agents and optionally give them isolated git worktrees.
3. Monitor the grid to see which agents are running, waiting for input, or idle.
4. Use hotkeys to jump straight to the next running or waiting agent when attention is needed.
5. Open agent view when you want to read the full terminal, respond, inspect git state, or use a dedicated terminal in that working directory.
6. Reopen Flowmux later and continue from the saved session state.

## Features

- Simple Unix-style orchestration that keeps Flowmux focused on coordinating sessions instead of replacing your workflow
- Bring your own tools: keep using your preferred editor, git UI, shell, terminal, and command-line utilities
- Keyboard-first dashboard for managing multiple CLI agents from one terminal UI
- Fast navigation between running, waiting, and idle agents
- Project-based organization with per-agent working directories and optional git worktrees
- Live agent terminals, plus quick access to a git viewer and a persistent shell in the agent directory
- tmux-backed persistence with automatic session restoration after restarts
- Response previews, model display, and status tracking in the grid view

## Installation

### Script

Install the latest Flowmux release on Linux or macOS with:

```bash
curl -fsSL https://raw.githubusercontent.com/grouzen/flowmux/main/install.sh | sh
```

### Homebrew

On macOS, install Flowmux from the upstream tap with:

```bash
brew install --cask grouzen/tap/flowmux
```

### From Source

Build the release binary with:

```bash
cargo build --release --locked
```

The binary will be available at `target/release/flowmux`.

`libghostty-vt-sys` statically builds the pinned Ghostty revision used by `libghostty-vt`. A default first build needs network access so Zig can fetch Ghostty build dependencies.

If you want a wrapper that reuses prefetched Ghostty inputs when present, use:

```bash
./tools/build-release-prefetched-libghostty-vt.sh
```

On macOS, to build one universal binary that runs on both Apple Silicon and Intel Macs, use:

```bash
./tools/build-universal-macos-prefetched-libghostty-vt.sh
```

If the required Rust macOS targets are not installed yet, the script installs `aarch64-apple-darwin` and `x86_64-apple-darwin` through `rustup` before building.

That writes the merged binary to `target/universal2-apple-darwin/release/flowmux`.

You can also install directly from the repo:

```bash
cargo install --path . --locked
```

### GitHub Releases

Pre-compiled binaries for Linux and macOS are available on the [Releases page](https://github.com/grouzen/flowmux/releases).

## Usage

### Common Commands

```bash
# Launch with default tmux session name "flowmux"
flowmux

# Launch with custom session name
flowmux --tmux-session my-session

# Specify custom worktrees location
flowmux --git-worktrees-location /path/to/worktrees

# Enable specific agents only
flowmux --enabled-agents opencode,claude,codex,pi
```

### CLI Options

| Option | Default | Description |
|--------|---------|-------------|
| `--tmux-session` | `flowmux` | Name of the tmux session to use |
| `--git-worktrees-location` | `~/.local/share/flowmux/worktrees` | Base directory for git worktrees created by Flowmux |
| `--enabled-agents` | *(all discovered)* | Comma-separated list of agent types to enable; overrides `enabled_agents` in global config |

### Essential Keybindings

| Key | Action |
|-----|--------|
| `n` | Create new agent |
| `p` | Create new project |
| `Enter` | Open agent view |
| `Ctrl+g` | Return to dashboard |
| `Ctrl+q` | Jump to next running agent |
| `Ctrl+o` | Jump to next waiting agent |
| `Ctrl+p` | Jump to next idle agent |
| `Ctrl+v` | Open configured git viewer |
| `Ctrl+t` | Open persistent terminal in the agent directory |
| `Ctrl+b` | Arm prefix mode so the next key is sent directly to the pane |
| `?` | Reopen the startup guide |
| `h/j/k/l` or arrows | Move selection |

All other keys in agent, git viewer, and terminal views are forwarded to the active tmux pane.

## Configuration

### Global Configuration

Global config lives at `~/.config/flowmux/config.toml`.

```toml
# Base port for Claude Code hook server (default: 15100)
claude_hook_server_port = 15100

# Base port for Pi extension callback server (default: 17100)
pi_hook_server_port = 17100

# External git viewer command (optional)
# Defaults to "git diff" when omitted or blank
# Examples: "lazygit", "lazydiff diff"
git_viewer = "lazygit"

# Whitelist of agent types to enable (optional)
# When omitted, all discovered agents are available
enabled_agents = ["opencode", "claude", "codex", "pi"]
```

### Per-Session Configuration

Per-session state is managed automatically under `~/.config/flowmux/sessions/<session>.toml`. It stores the ordered project list and each agent's pane target, directory, project membership, and session metadata.

Example:

```toml
projects = ["Default", "work"]

[[agents]]
name = "research"
pane = "flowmux:1.0"
directory = "/tmp/research"
project = "work"
agent_type = "opencode"
port = 9000
```

## Supported Agents

- OpenCode
- Claude Code
- Codex
- Pi

Flowmux auto-detects installed agent CLIs and enables discovered agents by default unless `enabled_agents` is set in global config.

## Architecture

See [ARCHITECTURE.md](docs/ARCHITECTURE.md) for technical details.

Core stack:

- Rust with Ratatui
- Tokio
- tmux
- `libghostty-vt`
- `git2`

## Contributing

Before opening a PR, run:

```bash
cargo build --locked
cargo test
```

Follow Conventional Commit-style subjects such as `feat(ui): improve dashboard navigation`.

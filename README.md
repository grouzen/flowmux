# Flowmux

A terminal-native AI agent multiplexer to orchestrate CLI agents for 10x engineers.

Install Flowmux to keep your trusty steed's  harness under the solid roof! :horse:

## Table of Contents

- [Behold](#behold)
- [Why](#why)
- [Installation](#installation)
- [Usage](#usage)
- [Configuration](#configuration)
- [Supported Agents](#supported-agents)
- [Plan](#plan)
- [Architecture](#architecture)
- [Contributing](#contributing)

## Behold

![](/docs/demo/screencast.gif)

## 🤔 Why

- Opinionated agent manager done my way, because I couldn't find one that's built the way I need.
- Not laser-focused on software development only!
- Pure grid layout, no left panel bullshit!
- Keyboard-driven navigation and interaction with sane amount of mouse support.
- Auto-detection of installed agent CLIs; Claude hooks are installed on first run.
- Focus on quick navigation through active agent sessions and history.
- Survives tmux restarts.
- Single binary, no stupid js runtimes!

## ✨ Features

### Multi-Agent Orchestration

- Run multiple CLI agents concurrently in isolated tmux panes
- Grid-based dashboard showing all agents at a glance
- Separate named project dashboards with tab-based switching and per-project agent grouping
- Real-time status tracking: running, waiting for input, stopped
- Context usage monitoring and model name display
- Last model response preview rendered as markdown

### Quick Navigation

- Jump to next running agent (`Ctrl+r`) or next waiting agent (`Ctrl+w`)
- Vim-style navigation (`h/j/k/l`) with arrow key support
- Mouse support: click to select, scroll to browse responses
- Reorder agent cards on the fly (`Ctrl+arrows`)

### Survives Restarts

- Auto-resumes dead agent panes on startup (e.g., after tmux restart)
- Configuration persists across sessions

### Git Worktree Integration

- Automatically create isolated git worktrees per agent
- Each agent works on its own branch without conflicts
- Optional worktree cleanup when removing agents
- Perfect for parallel feature development

### In-App Notifications

- Visual indicators when agent status changes
- Blinking status bar highlights running→waiting transitions
- Instant awareness without constant monitoring

### Configurable Git Viewer

- Launch your favorite git UI (lazygit, tig, etc) with `Ctrl+v`
- Configured via `git_viewer` in `~/.config/flowmux/config.toml`
- Opens in the agent's working directory

### Persistent Terminal

- Dedicated terminal per agent (`Ctrl+t`) in the agent's working directory
- Persists across agent view sessions
- Useful for quick commands, git operations, or file editing

### Prefix Mode

- `Ctrl+b` arms prefix mode: next key forwarded directly to the agent
- Bypass flowmux's keybindings when you need to send intercepted keys
- Works in agent view, git viewer, and terminal view

## 📦 Installation

### 📝 From source

Requires [Rust 1.90+](https://rustup.rs/), [Zig 0.15.x](https://ziglang.org/), and `git`.

`libghostty-vt-sys` statically builds the exact Ghostty revision pinned by the
current `libghostty-vt` Git dependency. A default first build needs network
access so Zig can fetch Ghostty's build dependencies.

Build with:

```bash
cargo build --release --locked
```

The binary will be at `target/release/flowmux`.

If you need an offline or reproducible local build, use the wrapper that
reuses prefetched Ghostty inputs when present, otherwise fetches them, points
Cargo at those local copies, and performs a locked release build:

```bash
./tools/build-release-prefetched-libghostty-vt.sh
```

That script defaults `LIBGHOSTTY_VT_SYS_OPTIMIZE=ReleaseFast`. If you need the
underlying manual flow or want to override paths, the equivalent commands are:

```bash
./tools/prefetch-libghostty-vt.sh
export GHOSTTY_SOURCE_DIR="$PWD/vendor/ghostty-prefetch/ghostty-src"
export GHOSTTY_ZIG_SYSTEM_DIR="$PWD/vendor/ghostty-prefetch/zig-system"
export LIBGHOSTTY_VT_SYS_OPTIMIZE=ReleaseFast
cargo build --release --locked
```

`GHOSTTY_SOURCE_DIR` points at a checked-out Ghostty tree.
`GHOSTTY_ZIG_SYSTEM_DIR` points at a prefetched Zig `--system` package
directory. `LIBGHOSTTY_VT_SYS_OPTIMIZE` overrides the Ghostty build mode when
you need something other than the crate's default.

Or install directly:

```bash
cargo install --path . --locked
```

## 🚀 Usage

### Prerequisites

- **tmux** must be installed and available in `$PATH`
- At least one supported agent CLI (`opencode`, `claude`, or `codex`)

### Launch

```bash
# Launch with default tmux session name "flowmux"
flowmux

# Launch with custom session name
flowmux --tmux-session my-session

# Specify custom worktrees location
flowmux --git-worktrees-location /path/to/worktrees

# Enable specific agents only
flowmux --enabled-agents opencode,claude,codex
```

### CLI Options

| Option | Default | Description |
|--------|---------|-------------|
| `--tmux-session` | `flowmux` | Name of the tmux session to use |
| `--git-worktrees-location` | `~/.local/share/flowmux/worktrees` | Base directory for git worktrees created by flowmux |
| `--enabled-agents` | *(all discovered)* | Comma-separated list of agent types to enable (e.g., `opencode,claude,codex`). Overrides `enabled_agents` in global config |

### Keybindings

#### Dashboard

| Key | Action |
|-----|--------|
| `q` | Quit |
| `n` | Create new agent |
| `p` | Create new project |
| `d` | Delete selected agent |
| `Ctrl+d` | Remove active project |
| `Tab`, `0-9` | Select project |
| `Enter` | Open agent view |
| `h` / `←` | Navigate left |
| `l` / `→` | Navigate right |
| `k` / `↑` | Navigate up |
| `j` / `↓` | Navigate down |
| `Ctrl+h` / `Ctrl+←` | Move card left |
| `Ctrl+l` / `Ctrl+→` | Move card right |
| `Ctrl+k` / `Ctrl+↑` | Move card up |
| `Ctrl+j` / `Ctrl+↓` | Move card down |
| `PageUp` | Scroll response up |
| `PageDown` | Scroll response down |
| Mouse click | Select agent |
| Mouse scroll | Scroll response |

#### Agent View

| Key | Action |
|-----|--------|
| `Ctrl+g` | Return to dashboard |
| `Ctrl+b` | Arm prefix mode (next key forwarded to pane) |
| `Ctrl+v` | Open git viewer (if `git_viewer` configured and in git repo) |
| `Ctrl+t` | Open persistent terminal in agent's working directory |
| `Ctrl+r` | Jump to next running/idle agent |
| `Ctrl+w` | Jump to next waiting agent |
| `PageUp` / `PageDown` | Scroll pane |
| Mouse scroll | Scroll pane |

All other keys are forwarded to the agent's tmux pane.

#### Git Viewer

| Key | Action |
|-----|--------|
| `Ctrl+b` | Arm prefix mode (next key forwarded to pane) |
| `Ctrl+v` | Close git viewer, return to agent view |
| `Ctrl+g` | Close git viewer, return to dashboard |

All other keys are forwarded to the git viewer's tmux pane.

#### Terminal View

| Key | Action |
|-----|--------|
| `Ctrl+b` | Arm prefix mode (next key forwarded to pane) |
| `Ctrl+t` | Close terminal, return to agent view |
| `Ctrl+g` | Close terminal, return to dashboard |

All other keys are forwarded to the terminal's tmux pane.

## ⚙️ Configuration

### Global Configuration

Located at `~/.config/flowmux/config.toml`:

```toml
# Base port for Claude Code hook server (default: 15100)
claude_hook_server_port = 15100

# External git viewer command (optional)
# Examples: "lazygit", "lazydiff diff"
git_viewer = "lazygit"

# Whitelist of agent types to enable (optional)
# When omitted, all discovered agents are available
enabled_agents = ["opencode", "claude", "codex"]
```

### Per-Session Configuration

Automatically managed at `~/.config/flowmux/sessions/<session>.toml`. Contains the ordered project list plus the agents with their pane targets, directories, project membership, and session IDs. You typically don't need to edit this manually.

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

## 🤖 Supported Agents

- OpenCode
- Claude Code
- Codex

## 🗺️ Plan

- [x] Improve agent status detection
- [x] Quick switching through: running, waiting (idle), last responded agents
- [x] Git awareness: branch names, worktrees, diff views
- [x] Per-project dashboards
- [ ] Support more agents: Pi, etc.
- [ ] Session history
- [ ] Filtering (with fuzzysearch): by name, agent type, working directory, etc.
- [ ] Split-screen mode to watch several running agents

## 📝 Architecture

See [ARCHITECTURE.md](docs/ARCHITECTURE.md) for detailed technical documentation.

### Tech Stack

- **Rust** with Ratatui TUI framework
- **Tokio** async runtime
- **tmux** for process isolation and pane management
- **libghostty-vt** crate for faithful terminal emulation
- **git2** for repository detection and worktree management

### Tech Notes

- Built in Rust ❤️ btw!
- Consumes around 100MB of memory and does not burn your CPU!
- Depends on tmux, so you must install it!
- The code is garbage because I vibe coded it!


## 🤝 Contributing

Contributions are welcome! Please ensure your changes build successfully:

```bash
cargo build --locked
```

1. Fork the repository
2. Create a feature branch (`git checkout -b feature/amazing-feature`)
3. Commit your changes using [Conventional Commits](https://www.conventionalcommits.org/) (`git commit -m 'feat: add amazing feature'`)
4. Push to the branch (`git push origin feature/amazing-feature`)
5. Open a Pull Request

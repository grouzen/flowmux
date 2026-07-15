# Flowmux Architecture

## Overview

Flowmux is a terminal-native AI agent multiplexer built in Rust that orchestrates CLI agents (OpenCode, Claude Code) inside tmux panes. It provides a grid-based dashboard for monitoring multiple concurrent agent sessions, and an immersive agent view that faithfully renders the agent's terminal output using an embedded terminal emulator (`libghostty-vt`).

## Technology Stack

### Core Technologies
- **Language**: Rust (Edition 2024)
- **TUI Framework**: [ratatui](https://github.com/ratatui/ratatui) with [crossterm](https://github.com/crossterm-rs/crossterm) backend
- **Async Runtime**: Tokio (multi-threaded)
- **Terminal Multiplexer**: tmux (required dependency)
- **Terminal Emulator**: [`libghostty-vt`](https://github.com/uzaaft/libghostty-rs) Rust crate (statically built via Zig through `libghostty-vt-sys`)
- **Git Integration**: [git2](https://github.com/rust-lang/git2-rs) + git CLI for worktree management

### Key Dependencies
- `ratatui` / `crossterm` — TUI rendering and input handling
- `tokio` — Async runtime for concurrent event processing
- `tmux_interface` + raw `Command` — tmux session/window/pane management
- `reqwest` — HTTP client for OpenCode SSE streaming
- `axum` — HTTP server for Claude Code hook callbacks
- `serde` / `toml` — Configuration serialization
- `clap` — CLI argument parsing
- `git2` — Repository detection, branch inspection
- `claude-code-transcripts` — Claude Code session transcript parsing
- `tui-markdown` — Markdown rendering in dashboard cards

## Architecture Overview

```
┌─────────────────────────────────────────────────────────┐
│                     Entry Point                          │
│              (main.rs — CLI, bootstrap)                   │
└───────────────────────┬─────────────────────────────────┘
                        │
                        ▼
┌─────────────────────────────────────────────────────────┐
│                   App (app.rs)                           │
│  Event Loop ◄──── mpsc channel ◄──── Background Tasks   │
│  State Machine: Dashboard / AgentView / Dialogs / ...    │
│  Key & Mouse Dispatch                                    │
└───────┬──────────────────┬──────────────────┬───────────┘
        │                  │                  │
        ▼                  ▼                  ▼
┌──────────────┐  ┌──────────────┐  ┌────────────────────┐
│  AgentRunner │  │  UI Layer    │  │  tmux Integration  │
│  (runner.rs) │  │  (ui/*.rs)   │  │  (tmux.rs)         │
│              │  │              │  │                    │
│  create()    │  │  dashboard   │  │  session mgmt      │
│  restore()   │  │  agent_view  │  │  pane capture      │
│  restart()   │  │  git_viewer  │  │  send-keys         │
└──────┬───────┘  │  term_view   │  │  resize-window     │
       │          │  dialogs     │  └────────┬───────────┘
       ▼          └──────────────┘           │
┌──────────────────────────┐                 │
│   Agent Adapters          │                 │
│   (agents/*.rs)           │                 │
│                          │                 │
│   ┌──────────────────┐   │                 │
│   │ OpenCodeAdapter  │◄──┼── SSE/HTTP      │
│   └──────────────────┘   │                 │
│   ┌──────────────────┐   │                 │
│   │ CodexAdapter     │◄──┼── WebSocket RPC │
│   └──────────────────┘   │                 │
│   ┌──────────────────┐   │                 │
│   │ ClaudeAdapter    │◄──┼── Hook Server   │
│   └──────────────────┘   │   (axum)        │
└──────────────────────────┘                 │
       │                                     │
       ▼                                     ▼
┌─────────────────────────────────────────────────────────┐
│                    tmux Server                            │
│  Session "flowmux" → Windows → Panes → Agent CLIs        │
└─────────────────────────────────────────────────────────┘
```

## Core Components

### 1. Entry Point (`main.rs`)

Minimal bootstrap that:
- Parses CLI arguments (`--tmux-session`, `--git-worktrees-location`, `--enabled-agents`)
- Acquires an exclusive file lock (`/tmp/flowmux-<session>.lock`) to prevent duplicate instances
- Probes `$PATH` for agent binaries (`opencode`, `claude`, `codex`, `pi`)
- Loads global and per-session configuration
- Ensures the tmux session exists
- Auto-resumes dead agent panes (survives tmux restarts)
- Builds the `App`, spawns background tasks, enters the TUI event loop

### 2. Application — Event Loop (`app.rs`)

The `App` struct is the central coordinator. It owns:
- `agents: Vec<AgentEntry>` — display data for each agent
- `adapters: Vec<Box<dyn AgentAdapter>>` — per-agent status providers
- `state: AppState` — current UI view (enum state machine)
- `config: Config` — persisted session state
- `runner: AgentRunner` — agent lifecycle manager
- `tx/rx` — unbounded mpsc channel for events

#### State Machine

```rust
enum AppState {
    Dashboard,                    // Grid of agent cards
    AgentView(usize),            // Full-screen pane viewer
    CreateAgentDialog,           // New agent creation form
    RemoveAgentDialog(State),    // Removal confirmation
    GitViewer(State),            // External git viewer pane
    TerminalView(State),         // Persistent terminal pane
}
```

#### Event System

Background tasks push events into the channel:
- **Crossterm events** — Key, Mouse, Paste (from `EventStream`)
- **DashboardTick** — every 500ms, polls all agent adapters for status
- **AgentViewTick** — every 50ms, captures pane output and detects stops
- **GitViewerTick / TerminalViewTick** — every 50ms, captures pane output

The main loop: `recv() → handle_event() → dirty check → draw → repeat`.

Rendering is **dirty-flag driven**: the terminal is only redrawn when `app.dirty` is `true`, avoiding unnecessary CPU usage.

### 3. Agent Runner (`runner.rs`)

`AgentRunner` is the single point of control for agent lifecycle:

- **`available_agent_types()`** — Returns discovered + enabled agent types
- **`restore(config)`** — Reconnects to an existing agent from persisted config (called on startup)
- **`create(name, dir, type, worktree, git_root)`** — Spawns a new agent in a tmux window, optionally creating a git worktree
- **`restart(config)`** — Resurrects a dead agent, reusing session IDs where possible (Claude `--resume`)

### 4. Agent Adapters (`agents/`)

The `AgentAdapter` trait provides a uniform async interface for querying agent state:

```rust
trait AgentAdapter: Send + Sync {
    async fn get_status(&self) -> AgentStatus;
    async fn get_context(&self) -> Option<ContextInfo>;
    async fn get_first_prompt(&self) -> Option<String>;
    async fn get_last_model_response(&self) -> Option<String>;
    async fn get_model_name(&self) -> Option<String>;
    async fn get_total_work_ms(&self) -> u64;
    fn get_cached_session_id(&self) -> Option<String>;
}
```

#### OpenCode Adapter (`agents/opencode.rs`)

- Launches `opencode serve` in a tmux window, connects via HTTP
- Subscribes to OpenCode's **SSE event stream** for real-time status updates
- Maintains a `LiveCache` (behind `RwLock`) updated reactively by the SSE consumer task
- Tracks: status (`Running`/`WaitingForInput`), recent messages, context usage, model name, cumulative work time
- Context window sizes resolved via the model registry

#### Claude Adapter (`agents/claude.rs`)

- Launches `claude` in a tmux window with `FLOWMUX_AGENT_ID` env var
- Runs a local **hook server** (axum) that receives callbacks from Claude Code's hook mechanism
- `ClaudeRuntime` manages a shared `HookStateMap` (Arc<Mutex<HashMap>>) keyed by flowmux agent ID
- Tracks: first prompt, context usage, last response, model name, work time
- Falls back to transcript parsing (`claude-code-transcripts`) when hook data is incomplete

#### Codex Adapter (`agents/codex.rs`)

- Launches a dedicated `codex app-server` on a loopback WebSocket port
- Launches the interactive Codex TUI with `codex --remote` against that server
- Subscribes with `thread/resume` and consumes JSON-RPC notifications for status,
  approval waits, responses, token usage, model changes, and session persistence
- Restores history and token usage through `thread/resume`; no steady-state
  app-server polling is used after subscription
- Restarts with `codex resume --remote <thread-id>`
- Reads the rollout path incrementally only for completed-turn duration when
  app-server `Turn.durationMs` is absent

#### Pi Adapter (`agents/pi.rs`)

- Launches Pi with a generated `--extension` file that posts lifecycle callbacks to a loopback Flowmux server.
- Tracks session ID, model, context usage, first prompt, latest assistant response, and running/idle/stopped state.
- Restarts with `pi --session <session-id>`.
- Vanilla Pi does not expose a universal UI-wait event, so V1 intentionally does not infer `WaitingForInput`.

### 5. tmux Integration (`tmux.rs`)

Thin wrapper around the tmux CLI (`Command`) and `tmux_interface` crate:

| Function | Purpose |
|---|---|
| `ensure_session()` | Creates the tmux session if missing, sets scrollback to 50k lines |
| `new_window(dir, name)` | Creates a detached window with working directory |
| `send_keys(target, keys)` | Sends keystrokes to a pane |
| `send_literal(target, data)` | Sends raw bytes via `load-buffer` + `paste-buffer` |
| `capture_pane(target)` | Captures the visible viewport (`-p -e`) |
| `capture_pane_history(target, n)` | Captures scrollback (`-S -n`) for scrolling |
| `cursor_position(target)` | Queries cursor coordinates and visibility |
| `pane_mouse_active(target)` | Checks `#{mouse_any_flag}` |
| `resize_window(target, w, h)` | Resizes the tmux window |
| `is_alive(target)` | Checks pane existence via `list-panes` |
| `kill_window(target)` | Destroys a tmux window |

### 6. Ghostty VT (`ghostty.rs` + `ghostty/render.rs`)

Flowmux uses the [`libghostty-vt`](https://github.com/uzaaft/libghostty-rs)
Rust crate, which wraps the Ghostty terminal emulator's VT engine. The crate's
`libghostty-vt-sys` layer statically builds the exact pinned Ghostty revision
with Zig during Cargo builds. Default first builds therefore need Rust 1.90+,
Zig 0.15.x, `git`, and network access unless `GHOSTTY_SOURCE_DIR` and
`GHOSTTY_ZIG_SYSTEM_DIR` are set to prefetched local inputs.

Purpose: **faithful rendering** of agent terminal output inside ratatui. The raw
ANSI output captured from tmux panes is fed into a Ghostty `Terminal`, and the
snapshot-based `RenderState` row/cell iterators are used to extract styled
cells (colors, bold, italic, wide chars, etc.) for display in the Agent View.

Key types:
- `Terminal` — VT parser and screen buffer
- `RenderState` — renderer state that produces a snapshot for a frame
- `RowIterator` / `CellIterator` — cell-level iteration with style/color/grapheme access

### 7. UI Layer (`ui/`)

All rendering functions live here, organized by view:

| Module | Responsibility |
|---|---|
| `dashboard.rs` | Grid layout calculation, agent card rendering, status bar |
| `agent_view.rs` | Full-screen pane viewer with Ghostty VT rendering, scrollback, stopped overlay |
| `git_viewer.rs` | External git viewer pane (e.g. lazygit) via Ghostty VT |
| `terminal_view.rs` | Persistent terminal pane via Ghostty VT |
| `create_agent.rs` | Agent creation dialog (name, directory, type, worktree) |
| `remove_agent.rs` | Removal confirmation dialog |
| `theme.rs` | Shared color/style constants |

The dashboard uses a **pure grid layout** — no side panels. Project tabs are rendered above the grid, and the active project filters which cards are visible. Cards are arranged in a `cols × rows` grid computed from the visible agent count. Each card shows: agent name, type, directory, status, context usage, model name, and the last model response (rendered as markdown).

### 8. Configuration

#### Per-Session (`config.rs`)
- Stored at `~/.config/flowmux/sessions/<session>.toml`
- Contains the ordered `projects` list plus agents with their pane targets, directories, project membership, and agent-specific data (port, session IDs)
- Atomic writes (write to `.tmp` then rename)

#### Global (`global_config.rs`)
- Stored at `~/.config/flowmux/config.toml`
- `claude_hook_server_port` — base port for the hook server (default: 15100)
- `pi_hook_server_port` — base port for the Pi extension callback server (default: 17100)
- `git_viewer` — external git viewer command (e.g. `"lazygit"`)
- `enabled_agents` — whitelist of agent types

### 9. Git Integration (`git.rs`)

Provides worktree management for isolated agent workspaces:
- `find_git_root(path)` — discovers repo root via `git2`
- `create_worktree(repo, path, branch, use_existing)` — `git worktree add`
- `remove_worktree(repo, path, branch, delete_branch)` — `git worktree remove --force` + optional branch deletion
- `sanitize_branch_name(name)` — converts agent names to valid branch names
- `branch_exists(repo, branch)` / `current_branch(path)` — branch inspection

### 10. Model Registry (`model_registry.rs`)

Static lookup table mapping model identifiers to context window sizes. Supports:
- Exact match (e.g. `claude-sonnet-4` → 200,000)
- Prefix match (e.g. `gpt-4-turbo*` → 128,000)
- Provider prefix stripping (e.g. `openrouter/anthropic/claude-sonnet-4`)
- Data generated by `tools/model-gen/`

## Data Flow

### Startup

```
CLI parse → flock → probe $PATH → load global config
  → init tmux session → load session config
  → auto-resume dead panes → restore adapters
  → probe host terminal colors → build App → spawn tasks
  → enter TUI event loop
```

### Agent Creation

```
User presses [n] → CreateAgentDialog → fill name/dir/type
  → git worktree add (optional) → tmux new-window
  → launch agent CLI → persist config → add to dashboard
```

### Dashboard Tick (every 500ms)

```
For each adapter:
  get_status() → get_context() → get_first_prompt()
  → get_last_model_response() → get_model_name()
  → get_total_work_ms()
→ update AgentEntry.meta → detect status count changes
→ set dirty flag → redraw if needed
```

### Agent View Tick (every 50ms)

```
Check pane liveness → capture pane output (visible or scrollback)
  → feed into libghostty-vt → update cursor position
  → track mouse mode → resize tmux window if terminal size changed
  → poll adapter for status → detect Stopped transition
  → set dirty flag if content changed
```

## Module Structure

```
src/
├── main.rs              # CLI parsing, bootstrap, event loop
├── app.rs               # App struct, AppState, event dispatch, key/mouse handlers
├── agents.rs            # AgentAdapter trait definition
├── agents/
│   ├── opencode.rs      # OpenCode adapter (SSE streaming, LiveCache)
│   ├── claude.rs        # Claude adapter (hook server integration)
│   ├── codex.rs         # Codex adapter (WebSocket app-server integration)
│   └── claude/
│       └── claude_hook_server.rs  # Axum HTTP server for Claude hooks
├── agent_discovery.rs   # $PATH probing for agent binaries
├── config.rs            # Per-session TOML config (agents list)
├── global_config.rs     # Global TOML config (hook port, git viewer, enabled agents)
├── git.rs               # Git worktree and branch management
├── ghostty.rs           # libghostty-vt reexports and Flowmux-specific helpers
├── ghostty/
│   └── render.rs        # Ghostty → ratatui rendering bridge
├── host_terminal.rs     # OSC 10/11 color probing via tmux passthrough
├── model_registry.rs    # Model → context window size lookup
├── model_registry_data.rs # Generated static data tables
├── models.rs            # AgentType, AgentStatus, AgentMeta, AgentEntry
├── runner.rs            # AgentRunner — agent lifecycle coordinator
├── tmux.rs              # tmux CLI wrappers (session, window, pane ops)
├── tui.rs               # Terminal setup/teardown (raw mode, alt screen, panic hook)
├── ui.rs                # UI module declarations
└── ui/
    ├── dashboard.rs     # Grid layout, agent cards, status bar
    ├── agent_view.rs    # Full-screen pane viewer
    ├── create_agent.rs  # Agent creation dialog
    ├── remove_agent.rs  # Removal confirmation dialog
    ├── git_viewer.rs    # External git viewer pane
    ├── terminal_view.rs # Persistent terminal pane
    └── theme.rs         # Shared colors and styles
```

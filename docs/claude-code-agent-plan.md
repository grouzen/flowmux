# Claude Code Agent Integration Plan

## Overview

Add `ClaudeAdapter` to stable so that Claude Code sessions appear alongside OpenCode agents in the dashboard with live status, context window usage, and prompt history.

Architecture: Claude Code's native **HTTP hooks** push events to an embedded HTTP server inside stable. Token/model data is read from Claude's **transcript JSONL** files using the `claude-code-transcripts` crate — but only on `Stop` events, with the result cached in memory so that `get_context` is always a pure in-memory read.

---

## Decisions

| Topic | Decision |
|---|---|
| Hook mechanism | `type: "http"` hooks — no subprocess, no state files |
| Hook receiver | Embedded `axum` HTTP server inside stable (default port `15100`) |
| Why axum over actix-web | `axum`'s entire dep graph (`hyper`, `tower`, `tower-http`) is already present via `reqwest`; only 5 thin new crates vs 39 for actix-web (which also pulls in C builds for `zstd-sys`); native `tokio::spawn` integration matches existing patterns |
| Port config | `hook_server_port` in `~/.config/stable/stable.toml` |
| Context window data | Parse transcript JSONL via `claude-code-transcripts = "0.1.8"` on `Stop` events only; result cached in `ClaudeHookState` |
| Hook installation | Automatic on first Claude agent creation; merges into `~/.claude/settings.json`; gated on agent discovery |
| Agent identity | stable generates a UUID (`stable_agent_id`) at creation; injected as `STABLE_AGENT_ID` env var |
| Routing | HTTP hooks carry `X-Stable-Agent-Id` header; server routes to correct adapter |
| Transcript risk | Accepted — `claude-code-transcripts` is a typed parser that catches schema drift |
| PreToolUse hook | Not used — unnecessary for stable's purposes |
| Hook subcommands | Not needed — HTTP server handles everything in-process |

---

## Config Schema Changes

### `~/.config/stable/config.toml` (new global config)

```toml
claude_hook_server_port = 15100
```

### `~/.config/stable/sessions/<session>.toml` (per-session agents)

```toml
[[agents]]
name = "my-refactor"
pane = "stable:1.0"
agent_type = "claude"
directory = "/home/user/projects/foo"
stable_agent_id = "550e8400-e29b-41d4-a716-446655440000"
session_id = "abc123"           # Claude's session_id from first SessionStart hook
transcript_path = "/home/user/.claude/projects/.../abc123.jsonl"

[[agents]]
name = "add-feature"
pane = "stable:2.0"
agent_type = "opencode"
directory = "/home/user/projects/bar"
port = 14101
session_id = "sess_abc123"
```

### Rust structs

```rust
pub struct AgentConfig {
    pub name: String,
    pub pane: String,
    pub directory: String,
    #[serde(flatten)]
    pub kind: AgentKind,
}

// serde(flatten) + serde(tag) has a known limitation with the toml crate.
// Use a manual Deserialize impl or a newtype wrapper if needed.
#[serde(tag = "agent_type", rename_all = "lowercase")]
pub enum AgentKind {
    Opencode {
        port: u16,
        session_id: Option<String>,
    },
    Claude {
        stable_agent_id: String,
        session_id: Option<String>,       // Claude's session_id from first SessionStart
        transcript_path: Option<String>,  // stored for cold-restart recovery
    },
}
```

> **Note**: `serde(flatten)` + `serde(tag)` is not supported by the `toml` crate out of the box.
> A proof-of-concept compile check is required before committing to this exact shape.
> Fallback: keep a flat `AgentConfig` with all fields `Option<_>` and validate in code.

---

## Agent Discovery

At startup, stable probes `$PATH` for known agent binaries before attempting any agent-specific setup (such as hook installation).

### `DiscoveredAgents` struct

```rust
pub struct DiscoveredAgents {
    pub claude: Option<PathBuf>,    // path to `claude` binary if found
    pub opencode: Option<PathBuf>,  // path to `opencode` binary if found
}

impl DiscoveredAgents {
    pub fn probe() -> Self {
        Self {
            claude: which::which("claude").ok(),
            opencode: which::which("opencode").ok(),
        }
    }
}
```

### Rules

- `DiscoveredAgents::probe()` is called once in `main.rs` before the TUI starts; the result is passed into `App`.
- Hook installation (`install_hooks()`) is gated: if `discovered.claude.is_none()`, skip silently and surface a warning in the UI if the user tries to create a Claude agent.
- The `CreateAgentDialog` disables the Claude option (with a tooltip) if `claude` is not discovered.
- Discovery is intentionally static (no background re-check); the user restarts stable after installing a new agent binary.

---

## Hook Server

### Startup

`main.rs` starts the hook server once at launch (unconditionally — it's cheap).
The server binds to `127.0.0.1:<hook_server_port>` and runs as a background `tokio` task.

### Shared state

```rust
// Keyed by stable_agent_id
type HookStateMap = Arc<Mutex<HashMap<String, ClaudeHookState>>>;

struct ClaudeHookState {
    status: AgentStatus,
    first_prompt: Option<String>,
    last_model_response: Option<String>,
    model_name: Option<String>,
    session_id: Option<String>,
    transcript_path: Option<String>,
    context_used: Option<u64>,  // cached from last Stop event; avoids repeated transcript I/O
}
```

The map is populated on startup from session toml config for all Claude agents, then updated live by incoming hooks.

### HTTP endpoint

```
POST /hook
Header: X-Stable-Agent-Id: <uuid>
Body: Claude Code hook JSON payload
```

Handler logic:
1. Extract `X-Stable-Agent-Id` header → look up `ClaudeHookState` in map
2. Deserialize `hook_event_name` from body
3. Dispatch to per-event handler (see below)
4. Return `200 OK` immediately (Claude Code expects a fast response)

### Per-event handlers

| Event | Fields used | State update |
|---|---|---|
| `SessionStart` | `session_id`, `transcript_path`, `model` | Store `session_id` + `transcript_path`; set `model_name`; set `status = Running`; persist to `agents.toml` |
| `UserPromptSubmit` | `prompt` | Set `first_prompt` if not yet set; set `status = Running` |
| `Stop` | `last_assistant_message` | Parse transcript → update `context_used`; set `last_model_response`; set `status = WaitingForInput` |
| `SessionEnd` | — | Set `status = Stopped` |

---

## ClaudeAdapter

### Struct

```rust
pub struct ClaudeAdapter {
    stable_agent_id: String,
    hook_state: Arc<Mutex<HashMap<String, ClaudeHookState>>>,
}
```

### Data sources

| `AgentAdapter` method | Source |
|---|---|
| `get_status` | `hook_state[stable_agent_id].status` |
| `get_first_prompt` | `hook_state[stable_agent_id].first_prompt` |
| `get_last_model_response` | `hook_state[stable_agent_id].last_model_response` |
| `get_model_name` | `hook_state[stable_agent_id].model_name` |
| `get_context` | `hook_state[stable_agent_id].context_used` — in-memory read; no file I/O |

### `get_context` detail

`context_used` is populated by the `Stop` event handler (and during cold restart recovery), not by `get_context` itself. The handler:

```
1. Read transcript_path from hook_state (populated by SessionStart)
2. Open JSONL file; iterate lines using claude-code-transcripts Entry parser
3. Find the last Entry::Assistant variant
4. context_used = usage.input_tokens
                + usage.cache_read_input_tokens.unwrap_or(0)
                + usage.cache_creation_input_tokens.unwrap_or(0)
5. Store result in hook_state[stable_agent_id].context_used
```

`get_context` then simply reads `hook_state[stable_agent_id].context_used` and returns
`Some(ContextInfo { used: context_used, total: None })` (or `None` if not yet populated).

> `total` is `None` because Claude Code does not expose the model's context limit in the
> transcript or hook payloads. The dashboard will display `42k / ?` for Claude agents.
> If a model-to-limit lookup table is added later, `total` can be populated from `model_name`.

### Cold restart recovery

On startup, if a Claude agent has `transcript_path` and `session_id` in `agents.toml`:
1. Pre-populate `ClaudeHookState` from stored fields
2. Re-parse transcript to restore `first_prompt`, `last_model_response`, `model_name`, `context_used`
3. Set `status = WaitingForInput` until the next hook event arrives

---

## Agent Launch

### Launch command

```
STABLE_AGENT_ID=<uuid> claude
```

The UUID is generated by stable at agent creation time and stored in `agents.toml`.

### Hook installation (automatic)

On first Claude agent creation, stable checks `~/.claude/settings.json`:
- Gated on `DiscoveredAgents.claude.is_some()` (see Agent Discovery)
- If the stable hooks block is absent, merge it in using `serde_json` read/modify/write (atomic)
- No user-facing subcommands required

### Hooks block installed into `~/.claude/settings.json`

```json
{
  "hooks": {
    "SessionStart": [{
      "hooks": [{
        "type": "http",
        "url": "http://127.0.0.1:15100/hook",
        "headers": { "X-Stable-Agent-Id": "$STABLE_AGENT_ID" },
        "allowedEnvVars": ["STABLE_AGENT_ID"]
      }]
    }],
    "UserPromptSubmit": [{
      "hooks": [{
        "type": "http",
        "url": "http://127.0.0.1:15100/hook",
        "headers": { "X-Stable-Agent-Id": "$STABLE_AGENT_ID" },
        "allowedEnvVars": ["STABLE_AGENT_ID"]
      }]
    }],
    "Stop": [{
      "hooks": [{
        "type": "http",
        "url": "http://127.0.0.1:15100/hook",
        "headers": { "X-Stable-Agent-Id": "$STABLE_AGENT_ID" },
        "allowedEnvVars": ["STABLE_AGENT_ID"]
      }]
    }],
    "SessionEnd": [{
      "hooks": [{
        "type": "http",
        "url": "http://127.0.0.1:15100/hook",
        "headers": { "X-Stable-Agent-Id": "$STABLE_AGENT_ID" },
        "allowedEnvVars": ["STABLE_AGENT_ID"]
      }]
    }]
  }
}
```

> The port in the URL must match `hook_server_port` from `stable.toml`.
> If the user changes the port, stable must re-merge the updated URL into `settings.json`.

The potential discrepancy between the config port value and the port used in the hooks needs to be addressed.

---

## Project Structure Changes

```
src/
  main.rs              # probe DiscoveredAgents; start hook server; no claude-code subcommands needed
  global_config.rs     # NEW: stable.toml (hook_server_port)
  config.rs            # CHANGED: AgentConfig → AgentKind enum
  agents/
    mod.rs             # AgentAdapter trait (unchanged)
    claude.rs          # NEW: ClaudeAdapter + ClaudeHookState; declares mod claude_hook_server
    claude/
      claude_hook_server.rs  # NEW: axum HTTP server; HookStateMap; per-event handlers
    opencode.rs        # unchanged
```

---

## New Dependencies

```toml
claude-code-transcripts = "0.1.8"
axum        = "0.8"
serde_json  = "1"
uuid        = { version = "1", features = ["v4"] }
which       = "6"
```

| Crate | Purpose |
|---|---|
| `claude-code-transcripts` | Typed parser for `~/.claude/projects/**/*.jsonl` |
| `axum` | Embedded HTTP server for Claude hook receiver; dep graph already present via `reqwest` |
| `serde_json` | Merge hooks block into `~/.claude/settings.json` |
| `uuid` | Generate `stable_agent_id` at agent creation |
| `which` | Probe `$PATH` for `claude`, `opencode` binaries at startup |

---

## Implementation Phases

0. **Agent discovery** — `DiscoveredAgents::probe()` in `main.rs`; wire into `App`; disable Claude creation UI if `claude` not found

1. **Global config** — `global_config.rs`: load/save `config.toml`; `claude_hook_server_port` with default `15100`

2. **AgentConfig refactor** — change `config.rs` flat struct to `AgentKind` enum; verify `serde` + `toml` round-trips; update all callsites in `app.rs` and `main.rs`

3. **Hook server** — `agents/claude/claude_hook_server.rs`: `axum` POST `/hook`; `HookStateMap`; per-event dispatch; start in `main.rs`

4. **ClaudeAdapter** — `agents/claude.rs`: implement all `AgentAdapter` methods against `HookStateMap`; `get_context` reads cached `context_used`; cold restart recovery

5. **Hook installation** — `install_hooks()` in `agents/claude.rs`: gated on discovery; atomic merge into `~/.claude/settings.json`; called on first Claude agent creation in `app.rs`

6. **CreateAgentDialog** — add agent type radio (already in plan); wire claude path: generate UUID, store in `AgentKind::Claude`, call `install_hooks()`, launch with `STABLE_AGENT_ID=<uuid> claude`

7. **Dashboard wiring** — dispatch `Box<dyn AgentAdapter>` based on `AgentKind`; `ClaudeAdapter` served from shared `HookStateMap`

8. **Cold restart** — on startup, pre-populate `HookStateMap` from `agents.toml` for all Claude agents; re-parse transcripts

---

## Known Risks

| Risk | Mitigation |
|---|---|
| `serde(flatten)` + `serde(tag)` unsupported by `toml` crate | Proof-of-concept compile check in phase 2; fallback to flat struct with `Option` fields |
| `claude-code-transcripts` schema drift if Anthropic changes JSONL format | Crate has a round-trip validator; `Entry::Unknown` catch-all prevents parse failures |
| HTTP hook URL hardcodes port; breaks if user changes `hook_server_port` after installation | Re-merge `settings.json` whenever port changes |
| Hook server port conflict at startup | Fail fast with a clear error message and hint to change `claude_hook_server_port` in `config.toml` |
| `transcript_path` absent until first `SessionStart` | `get_context` returns `None` gracefully; dashboard shows `inf/inf` as it does for opencode already |
| Transcript parsing on every `get_context` call causing performance degradation | Mitigated: transcript parsed only in `Stop` handler and cold restart; `get_context` is a pure in-memory read of cached `context_used` |
| `claude` binary not installed when user creates a Claude agent | Gated by `DiscoveredAgents` at startup; UI disables the option with a tooltip if binary not found |

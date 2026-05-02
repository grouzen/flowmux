# Claude Code Agent — Implementation Todo

Tracks every concrete code change required to implement the plan. Steps are ordered by dependency; each step should compile before the next begins.

---

## Phase 0 — Dependencies

- [ ] Add to `Cargo.toml`:
  ```toml
  axum        = "0.8"
  uuid        = { version = "1", features = ["v4"] }
  which       = "6"
  claude-code-transcripts = "0.1.8"
  ```
  (`serde_json` is already present.)
- [ ] Run `cargo check` to confirm the dependency graph resolves cleanly.

---

## Phase 1 — Agent Discovery (`src/agent_discovery.rs`)

- [ ] Create `src/agent_discovery.rs` with:
  ```rust
  pub struct DiscoveredAgents {
      pub claude: Option<std::path::PathBuf>,
      pub opencode: Option<std::path::PathBuf>,
  }
  impl DiscoveredAgents {
      pub fn probe() -> Self { ... }  // which::which("claude"), which::which("opencode")
  }
  ```
- [ ] Declare `mod agent_discovery;` in `src/main.rs`.
- [ ] Call `DiscoveredAgents::probe()` near the top of `main()` (before tmux init) and bind to a local `discovered` variable.
- [ ] Pass `discovered` into `App::new(...)` (requires updating `App` struct and its constructor — see Phase 7).

---

## Phase 2 — Global Config (`src/global_config.rs`)

- [ ] Create `src/global_config.rs` with:
  - `GlobalConfig` struct: `pub claude_hook_server_port: u16` (default `15100`).
  - `GlobalConfig::load() -> anyhow::Result<GlobalConfig>`: reads `~/.config/stable/config.toml` via `dirs::config_dir()`; returns default if file absent.
  - `GlobalConfig::save(&self) -> anyhow::Result<()>`: atomic write (write to temp file, rename).
- [ ] Declare `mod global_config;` in `src/main.rs`.
- [ ] Call `GlobalConfig::load()` in `main()` and bind to `global_config`.
- [ ] Pass `global_config.claude_hook_server_port` into the hook server spawn call (Phase 4).

---

## Phase 3 — `AgentConfig` Refactor (`src/config.rs`)

- [ ] **Proof-of-concept first**: add `AgentKind` enum alongside the existing flat struct and write a round-trip test (`cargo test`) to confirm `serde` + `toml` handles `#[serde(flatten)]` + `#[serde(tag)]`. If it fails, use the flat fallback (see below).

- [ ] **If serde+toml supports it**, replace the flat `AgentConfig` with:
  ```rust
  #[derive(Debug, Clone, Serialize, Deserialize)]
  pub struct AgentConfig {
      pub name: String,
      pub pane: String,
      pub directory: String,
      #[serde(flatten)]
      pub kind: AgentKind,
  }

  #[derive(Debug, Clone, Serialize, Deserialize)]
  #[serde(tag = "agent_type", rename_all = "lowercase")]
  pub enum AgentKind {
      Opencode { port: u16, session_id: Option<String> },
      Claude {
          stable_agent_id: String,
          session_id: Option<String>,
          transcript_path: Option<String>,
      },
  }
  ```

- [ ] **Fallback (if flatten+tag fails)**: keep a flat `AgentConfig` with all variant fields as `Option<_>` and an `agent_type: String` discriminant; add a `fn kind(&self) -> AgentKind` method that constructs the enum from the flat fields.

- [ ] Update `config_path()` / `Config::load()` / `Config::save()` if the serialisation shape changed.

- [ ] Fix every compile error in `src/main.rs` caused by the changed struct:
  - `agent_config.port` → `agent_config.kind` / `AgentKind::Opencode { port, .. }`
  - `agent_config.session_id` → extracted from the correct `AgentKind` variant
  - `agent_config.agent_type` → replaced by the enum tag
  - The auto-restart loop in `main()` should only apply to `AgentKind::Opencode` agents.

- [ ] Fix compile errors in `src/app.rs` for the same field accesses.

- [ ] Run `cargo check` — zero errors before proceeding.

---

## Phase 4 — Hook Server (`src/agents/claude/claude_hook_server.rs`)

- [ ] Create directory `src/agents/claude/` (the `claude.rs` module file will live at `src/agents/claude.rs` and Rust resolves the submodule automatically).

- [ ] Create `src/agents/claude/claude_hook_server.rs` with:

  **Types:**
  ```rust
  pub type HookStateMap = Arc<Mutex<HashMap<String, ClaudeHookState>>>;

  pub struct ClaudeHookState {
      pub status: AgentStatus,
      pub first_prompt: Option<String>,
      pub last_model_response: Option<String>,
      pub model_name: Option<String>,
      pub session_id: Option<String>,
      pub transcript_path: Option<String>,
      pub context_used: Option<u64>,
  }
  ```

  **axum handler** (`POST /hook`):
  - Extract `X-Stable-Agent-Id` header → look up entry in `HookStateMap`; return `400` if missing.
  - Deserialize `hook_event_name` field from JSON body.
  - Dispatch:
    - `"SessionStart"` → set `session_id`, `transcript_path`, `model_name`, `status = Running`; persist agent toml (see note below).
    - `"UserPromptSubmit"` → set `first_prompt` if `None`; set `status = Running`.
    - `"Stop"` → parse transcript file for last `Entry::Assistant` usage; update `context_used`; set `last_model_response`; set `status = WaitingForInput`.
    - `"SessionEnd"` → set `status = Stopped`.
    - Unknown events → no-op.
  - Always return `StatusCode::OK`.

  **Spawn function:**
  ```rust
  pub fn spawn_hook_server(state: HookStateMap, port: u16) {
      tokio::spawn(async move {
          let app = Router::new()
              .route("/hook", post(hook_handler))
              .layer(Extension(state));
          let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await
              .expect("hook server bind failed — check claude_hook_server_port in config.toml");
          axum::serve(listener, app).await.unwrap();
      });
  }
  ```

  > Note on `SessionStart` → persist: call a callback or channel to signal `App` that the agent's `session_id` / `transcript_path` should be written to the session toml. Simplest approach: pass an `UnboundedSender<HookPersistEvent>` into the handler state and have `App` drain it on each tick.

- [ ] Extract the transcript-parsing logic into a private helper:
  ```rust
  fn parse_context_used(transcript_path: &str) -> Option<u64>
  ```
  Uses `claude_code_transcripts` to iterate entries, finds last `Assistant` variant, sums token fields.

---

## Phase 5 — `ClaudeAdapter` (`src/agents/claude.rs`)

- [ ] Create `src/agents/claude.rs` with:
  ```rust
  pub mod claude_hook_server;  // resolves to src/agents/claude/claude_hook_server.rs
  ```

- [ ] Define `ClaudeAdapter`:
  ```rust
  pub struct ClaudeAdapter {
      stable_agent_id: String,
      hook_state: HookStateMap,
  }
  impl ClaudeAdapter {
      pub fn new(stable_agent_id: String, hook_state: HookStateMap) -> Self { ... }
  }
  ```

- [ ] Implement `AgentAdapter` for `ClaudeAdapter`:
  - `get_status` → lock map, read `status`.
  - `get_context` → lock map, read `context_used`; wrap in `ContextInfo { used, total: None }`.
  - `get_first_prompt` → lock map, read `first_prompt`.
  - `get_last_model_response` → lock map, read `last_model_response`.
  - `get_model_name` → lock map, read `model_name`.
  - `get_total_work_ms` → return `0` (not tracked for Claude agents; Claude Code tracks this internally).
  - `get_cached_session_id` → lock map, read `session_id`.

- [ ] Add `pub mod claude;` to `src/agents.rs`.

---

## Phase 6 — Hook Installation (`src/agents/claude.rs`)

- [ ] Add `pub fn install_hooks(port: u16) -> anyhow::Result<()>` in `src/agents/claude.rs`:
  - Resolve `~/.claude/settings.json` via `dirs::home_dir()`.
  - Read existing JSON (or start with `{}`).
  - Check if the stable hooks block is already present (keyed on the URL containing `/hook`).
  - If absent, merge the four-event hooks block (using the given `port`).
  - Write back atomically (temp file + rename).
- [ ] Add `pub fn uninstall_hooks() -> anyhow::Result<()>` (removes only stable's entries) — needed if the port changes; called before re-installing with the new port.
- [ ] Handle port-change case: in `GlobalConfig::save()`, if `claude_hook_server_port` changed from its previous value, call `uninstall_hooks()` then `install_hooks(new_port)`.

---

## Phase 7 — Wire into `App` and `main.rs`

### `App` struct changes (`src/app.rs`)

- [ ] Add fields to `App`:
  ```rust
  pub hook_state: HookStateMap,        // shared with hook server and ClaudeAdapters
  pub discovered: DiscoveredAgents,    // from Phase 1
  ```
- [ ] Update `App::new(...)` signature to accept `discovered: DiscoveredAgents` and `hook_state: HookStateMap`.

### `main.rs` changes

- [ ] After `GlobalConfig::load()`, create the shared `HookStateMap`:
  ```rust
  let hook_state: HookStateMap = Arc::new(Mutex::new(HashMap::new()));
  ```
- [ ] Call `spawn_hook_server(hook_state.clone(), global_config.claude_hook_server_port)`.
- [ ] In the agent reconstruction loop, dispatch on `AgentKind`:
  - `AgentKind::Opencode { port, session_id }` → `OpenCodeAdapter::new(port, session_id)` (existing path).
  - `AgentKind::Claude { stable_agent_id, session_id, transcript_path }` → pre-populate `HookStateMap` entry (cold restart); construct `ClaudeAdapter::new(stable_agent_id, hook_state.clone())`.
- [ ] Cold restart pre-population: for each Claude agent with `transcript_path` set, call `parse_context_used()` and populate `ClaudeHookState` with `status = WaitingForInput` and all recovered fields.
- [ ] Pass `discovered` and `hook_state` into `App::new(...)`.

---

## Phase 8 — `CreateAgentDialog` (`src/ui/create_agent.rs` + `src/app.rs`)

- [ ] Add `agent_type` radio to `CreateAgentState`:
  ```rust
  pub enum CreateAgentType { Opencode, Claude }
  pub struct CreateAgentState {
      // existing fields ...
      pub agent_type: CreateAgentType,
  }
  ```
- [ ] Render the radio in `render_create_agent()`:
  - Show both options.
  - If `app.discovered.claude.is_none()`, render the Claude option as disabled with a `(claude not found in $PATH)` label.
- [ ] In `App::handle_event` for the confirm action in `CreateAgentDialog`:
  - **Opencode path** (existing): unchanged.
  - **Claude path** (new):
    1. Generate `stable_agent_id = uuid::Uuid::new_v4().to_string()`.
    2. Create a new tmux window via `tmux::new_window(name, dir)`.
    3. Call `install_hooks(global_config.claude_hook_server_port)` (no-op if already installed).
    4. Insert a blank `ClaudeHookState` entry into `hook_state` keyed by `stable_agent_id`.
    5. Build `AgentConfig` with `AgentKind::Claude { stable_agent_id, session_id: None, transcript_path: None }`.
    6. Push config, push `AgentEntry`, push `Box::new(ClaudeAdapter::new(...))`.
    7. Save config.
    8. Launch claude in the tmux pane: `tmux send-keys "STABLE_AGENT_ID=<uuid> claude" Enter`.

---

## Phase 9 — Dashboard Display

- [ ] Confirm `ui/dashboard.rs` and `ui/agent_view.rs` only read from `AgentMeta` (which is populated generically from any `AgentAdapter`). If they access `AgentConfig` fields that are now inside `AgentKind` (e.g. `port`), update those accesses.
- [ ] Ensure `context.total == None` displays as `?` (not a crash) in the context widget — confirm or fix the existing rendering code.

---

## Phase 10 — Final verification

- [ ] `cargo check` — zero errors.
- [ ] `cargo clippy` — no warnings introduced by new code.
- [ ] Manual smoke test:
  1. Start stable; confirm hook server binds on port 15100.
  2. Create a Claude agent; confirm `~/.claude/settings.json` contains the hooks block.
  3. Run `claude` in the pane; confirm `SessionStart` hook fires and agent status changes to `Running`.
  4. Submit a prompt; confirm `UserPromptSubmit` fires and `first_prompt` is set.
  5. Wait for response; confirm `Stop` fires, `context_used` is populated, status is `WaitingForInput`.
  6. Kill claude; confirm `SessionEnd` fires and status is `Stopped`.
  7. Restart stable; confirm cold restart recovery restores `first_prompt`, `last_model_response`, `context_used`.

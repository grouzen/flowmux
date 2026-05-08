pub mod claude_hook_server;

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::agents::AgentAdapter;
use crate::models::{AgentStatus, ContextInfo};
use claude_hook_server::{ClaudeHookState, HookStateMap};

// ---------------------------------------------------------------------------
// ClaudeAdapter
// ---------------------------------------------------------------------------

pub struct ClaudeAdapter {
    stable_agent_id: String,
    hook_state: HookStateMap,
}

impl ClaudeAdapter {
    pub fn new(stable_agent_id: String, hook_state: HookStateMap) -> Self {
        Self { stable_agent_id, hook_state }
    }
}

#[async_trait]
impl AgentAdapter for ClaudeAdapter {
    async fn get_status(&self) -> AgentStatus {
        let map = self.hook_state.lock().unwrap();
        map.get(&self.stable_agent_id)
            .map(|s| s.status.clone())
            .unwrap_or(AgentStatus::Unknown)
    }

    async fn get_context(&self) -> Option<ContextInfo> {
        let map = self.hook_state.lock().unwrap();
        let entry = map.get(&self.stable_agent_id)?;
        let context_used = entry.context_used?;
        let total = entry.model_name.as_deref().and_then(model_context_window);
        Some(ContextInfo { used: context_used, total })
    }

    async fn get_first_prompt(&self) -> Option<String> {
        let map = self.hook_state.lock().unwrap();
        map.get(&self.stable_agent_id)?.first_prompt.clone()
    }

    async fn get_last_model_response(&self) -> Option<String> {
        let map = self.hook_state.lock().unwrap();
        map.get(&self.stable_agent_id)?.last_model_response.clone()
    }

    async fn get_model_name(&self) -> Option<String> {
        let map = self.hook_state.lock().unwrap();
        map.get(&self.stable_agent_id)?.model_name.clone()
    }

    /// Returns total model generation time summed from `TurnDuration` transcript entries.
    async fn get_total_work_ms(&self) -> u64 {
        let map = self.hook_state.lock().unwrap();
        map.get(&self.stable_agent_id)
            .map(|s| s.total_work_ms)
            .unwrap_or(0)
    }

    fn get_cached_session_id(&self) -> Option<String> {
        let map = self.hook_state.lock().unwrap();
        map.get(&self.stable_agent_id)?.session_id.clone()
    }
}

// ---------------------------------------------------------------------------
// Claude model context-window table
// ---------------------------------------------------------------------------

/// Return the context-window size (in tokens) for a known Claude model ID.
///
/// Source: <https://docs.anthropic.com/en/docs/about-claude/models>
///
/// All Claude 3+ models ship with a 200 k token context window.  Only the
/// legacy Claude 2 / Instant lines have smaller windows; those are listed
/// explicitly.  Any unrecognised `claude-*` string defaults to 200 k so that
/// newly released models are handled gracefully without a code change.
pub fn model_context_window(model: &str) -> Option<u64> {
    // Explicit 100 k exceptions (Claude 2.0 and Instant 1.x lines).
    const HUNDRED_K: &[&str] = &[
        "claude-2.0",
        "claude-instant-1",
    ];
    // If the model matches any 100 k prefix, return 100 k.
    for prefix in HUNDRED_K {
        if model.starts_with(prefix) {
            return Some(100_000);
        }
    }
    // Any other "claude-*" model gets 200 k.
    if model.starts_with("claude") {
        Some(200_000)
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// ClaudeRuntime — owns the HookStateMap and hook server lifecycle
// ---------------------------------------------------------------------------

pub(crate) struct ClaudeRuntime {
    hook_state: HookStateMap,
}

impl ClaudeRuntime {
    /// Spawn the hook server and a background persist task, return a runtime
    /// handle.  `session_name` is used by the persist task to find the correct
    /// config file when patching session_id / transcript_path.
    pub(crate) fn start(port: u16, session_name: String) -> Self {
        let hook_state: HookStateMap = Arc::new(Mutex::new(HashMap::new()));
        let (persist_tx, mut persist_rx) =
            tokio::sync::mpsc::unbounded_channel::<claude_hook_server::HookPersistEvent>();

        claude_hook_server::spawn_hook_server(hook_state.clone(), persist_tx, port);

        // Background task: receive persist events and patch the session config file.
        tokio::spawn(async move {
            while let Some(event) = persist_rx.recv().await {
                if let Ok(mut config) = crate::config::Config::load(&session_name) {
                    for agent in config.agents.iter_mut() {
                        if let crate::config::AgentKind::Claude {
                            stable_agent_id,
                            session_id,
                            transcript_path,
                        } = &mut agent.kind
                        {
                            if *stable_agent_id == event.stable_agent_id {
                                if let Some(sid) = event.session_id.clone() {
                                    *session_id = Some(sid);
                                }
                                if event.transcript_path.is_some() {
                                    *transcript_path = event.transcript_path.clone();
                                }
                            }
                        }
                    }
                    let _ = config.save();
                }
            }
        });

        Self { hook_state }
    }

    /// Create a `ClaudeAdapter` for a given `stable_agent_id`, pre-inserting
    /// a default entry in the shared map if one doesn't already exist.
    pub(crate) fn make_adapter(&self, stable_agent_id: String) -> ClaudeAdapter {
        {
            let mut map = self.hook_state.lock().unwrap();
            map.entry(stable_agent_id.clone())
                .or_insert_with(ClaudeHookState::default);
        }
        ClaudeAdapter::new(stable_agent_id, self.hook_state.clone())
    }

    /// Pre-populate the hook state from persisted config so that the dashboard
    /// shows meaningful data immediately on startup (before the first hook fires).
    pub(crate) fn restore(
        &self,
        id: &str,
        session_id: Option<String>,
        transcript_path: Option<String>,
    ) {
        let mut map = self.hook_state.lock().unwrap();
        let entry = map
            .entry(id.to_owned())
            .or_insert_with(ClaudeHookState::default);

        if session_id.is_some() {
            entry.session_id = session_id;
        }
        if let Some(ref path) = transcript_path {
            entry.transcript_path = Some(path.clone());
            if let Some(info) = claude_hook_server::parse_transcript(path) {
                entry.context_used = Some(info.context_used);
                entry.last_model_response = info.last_response_text;
                if info.model_name.is_some() {
                    entry.model_name = info.model_name;
                }
                entry.total_work_ms = info.total_work_ms;
                if info.first_prompt.is_some() {
                    entry.first_prompt = info.first_prompt;
                }
            }
            entry.status = AgentStatus::WaitingForInput;
        }
    }

    /// Reset the status of an existing agent entry to `Unknown` so the UI
    /// reflects "restarting" rather than "stopped" while the new process boots.
    /// If no entry exists for `id` this is a no-op.
    pub(crate) fn reset_status(&self, id: &str) {
        let mut map = self.hook_state.lock().unwrap();
        if let Some(entry) = map.get_mut(id) {
            entry.status = AgentStatus::Unknown;
        }
    }
}

// ---------------------------------------------------------------------------
// Hook installation
// ---------------------------------------------------------------------------

/// The URL pattern that identifies stable's hook entries inside
/// `~/.claude/settings.json`.  Used to detect whether installation is
/// already present and to remove stale entries when the port changes.
const HOOK_URL_PATH: &str = "/hook";

/// Build the four-event hooks block that stable merges into
/// `~/.claude/settings.json`.
fn build_hooks_block(port: u16) -> Value {
    let url = format!("http://127.0.0.1:{}{}", port, HOOK_URL_PATH);

    let make_hook = |event: &str| -> (String, Value) {
        let entry = serde_json::json!([{
            "hooks": [{
                "type": "http",
                "url": url,
                "headers": { "X-Stable-Agent-Id": "$STABLE_AGENT_ID" },
                "allowedEnvVars": ["STABLE_AGENT_ID"]
            }]
        }]);
        (event.to_owned(), entry)
    };

    let hooks_map: serde_json::Map<String, Value> = [
        make_hook("SessionStart"),
        make_hook("UserPromptSubmit"),
        make_hook("Stop"),
        make_hook("SessionEnd"),
    ]
    .into_iter()
    .collect();

    Value::Object(hooks_map)
}

/// Return `true` if `hooks_root` already contains at least one stable hook
/// entry (identified by a URL ending in `/hook` pointing to `127.0.0.1`).
fn has_stable_hooks(hooks_root: &Value) -> bool {
    let Some(obj) = hooks_root.as_object() else { return false };
    for event_val in obj.values() {
        let Some(arr) = event_val.as_array() else { continue };
        for hook_group in arr {
            let Some(inner) = hook_group.get("hooks").and_then(Value::as_array) else { continue };
            for h in inner {
                let url = h.get("url").and_then(Value::as_str).unwrap_or("");
                if url.contains("127.0.0.1") && url.ends_with(HOOK_URL_PATH) {
                    return true;
                }
            }
        }
    }
    false
}

fn settings_path() -> Option<std::path::PathBuf> {
    dirs::home_dir().map(|h| h.join(".claude").join("settings.json"))
}

/// Merge stable's HTTP hooks into `~/.claude/settings.json`.
///
/// This is a no-op if the hooks are already present for any port (idempotent).
/// To update the port, call `uninstall_hooks()` first.
pub fn install_hooks(port: u16) -> Result<()> {
    let path = settings_path().context("cannot determine home directory")?;

    // Read existing JSON or start from an empty object.
    let mut root: Value = if path.exists() {
        let raw = std::fs::read_to_string(&path)
            .with_context(|| format!("read {:?}", path))?;
        serde_json::from_str(&raw)
            .with_context(|| format!("parse {:?}", path))?
    } else {
        serde_json::json!({})
    };

    // Ensure the "hooks" key exists.
    let hooks = root
        .as_object_mut()
        .context("settings.json root is not an object")?
        .entry("hooks")
        .or_insert_with(|| serde_json::json!({}));

    // Nothing to do if stable's hooks are already present.
    if has_stable_hooks(hooks) {
        return Ok(());
    }

    // Merge our four-event block into the existing hooks object.
    let new_block = build_hooks_block(port);
    let hooks_obj = hooks.as_object_mut().context("hooks is not an object")?;
    let new_obj = new_block.as_object().unwrap();

    for (event, new_entries) in new_obj {
        let event_arr = hooks_obj
            .entry(event.clone())
            .or_insert_with(|| serde_json::json!([]));
        let arr = event_arr.as_array_mut().context("event hook list is not an array")?;
        if let Some(entries) = new_entries.as_array() {
            arr.extend(entries.iter().cloned());
        }
    }

    write_settings(&path, &root)
}

/// Atomically write `value` as pretty-printed JSON to `path`
/// (write to `.tmp` then rename).
fn write_settings(path: &std::path::Path, value: &Value) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create dir {:?}", parent))?;
    }
    let tmp = path.with_extension("json.tmp");
    let json = serde_json::to_string_pretty(value).context("serialize settings.json")?;
    std::fs::write(&tmp, json).with_context(|| format!("write {:?}", tmp))?;
    std::fs::rename(&tmp, path)
        .with_context(|| format!("rename {:?} -> {:?}", tmp, path))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_settings(port: u16) -> Value {
        let mut root = serde_json::json!({});
        install_hooks_into(&mut root, port);
        root
    }

    /// Helper: run the install logic against an in-memory Value.
    fn install_hooks_into(root: &mut Value, port: u16) {
        let hooks = root
            .as_object_mut()
            .unwrap()
            .entry("hooks")
            .or_insert_with(|| serde_json::json!({}));
        if !has_stable_hooks(hooks) {
            let new_block = build_hooks_block(port);
            let hooks_obj = hooks.as_object_mut().unwrap();
            for (event, new_entries) in new_block.as_object().unwrap() {
                let arr = hooks_obj
                    .entry(event.clone())
                    .or_insert_with(|| serde_json::json!([]))
                    .as_array_mut()
                    .unwrap();
                if let Some(entries) = new_entries.as_array() {
                    arr.extend(entries.iter().cloned());
                }
            }
        }
    }

    #[test]
    fn install_adds_four_events() {
        let root = make_settings(15100);
        let hooks = root.get("hooks").unwrap().as_object().unwrap();
        for event in &["SessionStart", "UserPromptSubmit", "Stop", "SessionEnd"] {
            assert!(hooks.contains_key(*event), "missing event: {event}");
        }
    }

    #[test]
    fn install_is_idempotent() {
        let mut root = make_settings(15100);
        // Second install should not duplicate entries.
        install_hooks_into(&mut root, 15100);
        let hooks = root.get("hooks").unwrap().as_object().unwrap();
        let start_arr = hooks["SessionStart"].as_array().unwrap();
        assert_eq!(start_arr.len(), 1, "duplicate hook groups added");
    }

    #[test]
    fn uninstall_removes_stable_entries() {
        let mut root = make_settings(15100);
        if let Some(hooks) = root.get_mut("hooks") {
            remove_stable_hooks(hooks);
        }
        let hooks = root.get("hooks").unwrap();
        assert!(!has_stable_hooks(hooks), "stable hooks still present after removal");
    }

    #[test]
    fn uninstall_preserves_other_hooks() {
        let mut root = serde_json::json!({
            "hooks": {
                "SessionStart": [{
                    "hooks": [{"type": "command", "command": "echo hi"}]
                }]
            }
        });
        install_hooks_into(&mut root, 15100);
        if let Some(hooks) = root.get_mut("hooks") {
            remove_stable_hooks(hooks);
        }
        // The user's "command" hook should still be present.
        let arr = root["hooks"]["SessionStart"].as_array().unwrap();
        assert_eq!(arr.len(), 1, "user hook was incorrectly removed");
        let inner = arr[0]["hooks"].as_array().unwrap();
        assert_eq!(inner[0]["type"], "command");
    }
}

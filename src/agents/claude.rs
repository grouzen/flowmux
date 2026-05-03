pub mod claude_hook_server;

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde_json::Value;

use crate::agents::AgentAdapter;
use crate::models::{AgentStatus, ContextInfo};
use claude_hook_server::HookStateMap;

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
        let context_used = map.get(&self.stable_agent_id)?.context_used?;
        Some(ContextInfo { used: context_used, total: None })
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

/// Remove all stable-managed hook entries from `hooks_root` (in-place).
/// An entry is identified by a URL pointing to `127.0.0.1` and ending with
/// `/hook`.  Empty event arrays are removed entirely.
fn remove_stable_hooks(hooks_root: &mut Value) {
    let Some(obj) = hooks_root.as_object_mut() else { return };
    for event_val in obj.values_mut() {
        let Some(arr) = event_val.as_array_mut() else { continue };
        for hook_group in arr.iter_mut() {
            let Some(inner) = hook_group.get_mut("hooks").and_then(Value::as_array_mut) else {
                continue;
            };
            inner.retain(|h| {
                let url = h.get("url").and_then(Value::as_str).unwrap_or("");
                !(url.contains("127.0.0.1") && url.ends_with(HOOK_URL_PATH))
            });
        }
        // Remove hook groups that are now empty.
        arr.retain(|g| {
            g.get("hooks")
                .and_then(Value::as_array)
                .map(|a| !a.is_empty())
                .unwrap_or(true)
        });
    }
    // Remove event keys that have no remaining hook groups.
    obj.retain(|_, v| v.as_array().map(|a| !a.is_empty()).unwrap_or(true));
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

/// Remove stable's HTTP hooks from `~/.claude/settings.json`.
///
/// Safe to call even if hooks are not installed (no-op in that case).
pub fn uninstall_hooks() -> Result<()> {
    let path = settings_path().context("cannot determine home directory")?;
    if !path.exists() {
        return Ok(());
    }

    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("read {:?}", path))?;
    let mut root: Value = serde_json::from_str(&raw)
        .with_context(|| format!("parse {:?}", path))?;

    if let Some(hooks) = root.get_mut("hooks") {
        remove_stable_hooks(hooks);
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

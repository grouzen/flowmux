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
        Self {
            stable_agent_id,
            hook_state,
        }
    }

    fn maybe_reparse_transcript(&self) {
        let transcript_path = {
            let map = self.hook_state.lock().unwrap();
            let Some(entry) = map.get(&self.stable_agent_id) else {
                return;
            };
            if entry.context_used.is_some() {
                return;
            }
            match entry.transcript_path.clone() {
                Some(p) => p,
                None => return,
            }
        };

        let Some(info) = claude_hook_server::parse_transcript(&transcript_path) else {
            return;
        };

        let mut map = self.hook_state.lock().unwrap();
        if let Some(entry) = map.get_mut(&self.stable_agent_id) {
            if entry.context_used.is_none() {
                entry.context_used = Some(info.context_used);
            }
            if entry.total_work_ms == 0 {
                entry.total_work_ms = info.total_work_ms;
            }
            if entry.model_name.is_none() && info.model_name.is_some() {
                entry.model_name = info.model_name;
            }
            if entry.last_model_response.is_none() && info.last_response_text.is_some() {
                entry.last_model_response = info.last_response_text;
            }
            if entry.first_prompt.is_none() && info.first_prompt.is_some() {
                entry.first_prompt = info.first_prompt;
            }
        }
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
        self.maybe_reparse_transcript();
        let map = self.hook_state.lock().unwrap();
        let entry = map.get(&self.stable_agent_id)?;
        let context_used = entry.context_used?;
        let total = entry
            .model_name
            .as_deref()
            .and_then(crate::model_registry::model_context_window);
        Some(ContextInfo {
            used: context_used,
            total,
        })
    }

    async fn get_first_prompt(&self) -> Option<String> {
        self.maybe_reparse_transcript();
        let map = self.hook_state.lock().unwrap();
        map.get(&self.stable_agent_id)?.first_prompt.clone()
    }

    async fn get_last_model_response(&self) -> Option<String> {
        self.maybe_reparse_transcript();
        let map = self.hook_state.lock().unwrap();
        map.get(&self.stable_agent_id)?.last_model_response.clone()
    }

    async fn get_model_name(&self) -> Option<String> {
        self.maybe_reparse_transcript();
        let map = self.hook_state.lock().unwrap();
        map.get(&self.stable_agent_id)?.model_name.clone()
    }

    async fn get_total_work_ms(&self) -> u64 {
        self.maybe_reparse_transcript();
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
// ClaudeRuntime — owns the HookStateMap and hook server lifecycle
// ---------------------------------------------------------------------------

pub(crate) struct ClaudeRuntime {
    hook_state: HookStateMap,
    persist_tx: tokio::sync::mpsc::UnboundedSender<claude_hook_server::HookPersistEvent>,
}

impl ClaudeRuntime {
    /// Spawn the hook server and a background persist task, return a runtime
    /// handle.  `session_name` is used by the persist task to find the correct
    /// config file when patching session_id / transcript_path.
    pub(crate) fn start(port: u16, session_name: String) -> Self {
        let hook_state: HookStateMap = Arc::new(Mutex::new(HashMap::new()));
        let (persist_tx, mut persist_rx) =
            tokio::sync::mpsc::unbounded_channel::<claude_hook_server::HookPersistEvent>();

        let persist_tx_clone = persist_tx.clone();
        claude_hook_server::spawn_hook_server(hook_state.clone(), persist_tx_clone, port);

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

        Self {
            hook_state,
            persist_tx,
        }
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
    ///
    /// If `transcript_path` is absent but `session_id` is known, attempts to
    /// locate the transcript file under `~/.claude/projects/` using the agent's
    /// working `directory` as a hint.  When found the path is persisted back to
    /// the config so subsequent restarts don't need to re-infer it.
    pub(crate) fn restore(
        &self,
        id: &str,
        session_id: Option<String>,
        transcript_path: Option<String>,
        directory: Option<&str>,
    ) {
        // If transcript_path is missing but we have a session_id, try to find
        // the transcript on disk so meta info is available immediately.
        let transcript_path = transcript_path.or_else(|| {
            let sid = session_id.as_deref()?;
            infer_transcript_path(sid, directory)
        });

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
            entry.status = AgentStatus::Idle;

            // Persist the (possibly newly inferred) transcript_path back to
            // the config file so future restarts don't need to re-infer it.
            let _ = self.persist_tx.send(claude_hook_server::HookPersistEvent {
                stable_agent_id: id.to_owned(),
                session_id: entry.session_id.clone(),
                transcript_path: Some(path.clone()),
            });
        } else if entry.session_id.is_some() {
            // If we have a session_id but no transcript_path yet (e.g., stable restarted
            // before the first Stop hook), assume the agent is waiting for input.
            entry.status = AgentStatus::Idle;
        }
    }

    /// Reset the status of an existing agent entry to `Idle` so the UI
    /// reflects "restarting" rather than "stopped" while the new process boots.
    /// If no entry exists for `id` this is a no-op.
    pub(crate) fn reset_status(&self, id: &str) {
        let mut map = self.hook_state.lock().unwrap();
        if let Some(entry) = map.get_mut(id) {
            entry.status = AgentStatus::Idle;
        }
    }
}

// ---------------------------------------------------------------------------
// Transcript path inference
// ---------------------------------------------------------------------------

/// Try to locate a Claude Code transcript file for a known `session_id`.
///
/// Claude Code stores transcripts at:
///   `~/.claude/projects/<encoded-dir>/<session_id>.jsonl`
///
/// where `<encoded-dir>` is derived from the project directory by replacing
/// every `/` with `-` (stripping the leading slash).  For example,
/// `/home/alice/myproject` → `-home-alice-myproject`.
///
/// If `directory` is supplied the expected path is constructed directly;
/// otherwise a glob-style walk of `~/.claude/projects/` is performed to find
/// the file in any project sub-directory.
///
/// Returns `None` if no matching file exists on disk.
fn infer_transcript_path(session_id: &str, directory: Option<&str>) -> Option<String> {
    let home = std::env::var("HOME").ok()?;
    let projects_root = std::path::Path::new(&home).join(".claude").join("projects");

    // Fast path: derive the expected directory encoding from the agent's CWD.
    if let Some(dir) = directory {
        let encoded = dir.replace('/', "-");
        let candidate = projects_root
            .join(&encoded)
            .join(format!("{session_id}.jsonl"));
        if candidate.exists() {
            return candidate.to_str().map(str::to_owned);
        }
    }

    // Slow path: scan all project sub-directories for the session file.
    let read_dir = std::fs::read_dir(&projects_root).ok()?;
    for entry in read_dir.flatten() {
        let candidate = entry.path().join(format!("{session_id}.jsonl"));
        if candidate.exists() {
            return candidate.to_str().map(str::to_owned);
        }
    }

    None
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

    // `Notification` with matcher `permission_prompt` fires when ANY permission
    // dialog appears — including for built-in tools like `Skill` that bypass the
    // `PermissionRequest` hook entirely.
    let notification_entry = serde_json::json!([{
        "matcher": "permission_prompt",
        "hooks": [{
            "type": "http",
            "url": url,
            "headers": { "X-Stable-Agent-Id": "$STABLE_AGENT_ID" },
            "allowedEnvVars": ["STABLE_AGENT_ID"]
        }]
    }]);

    let mut hooks_map: serde_json::Map<String, Value> = [
        make_hook("SessionStart"),
        make_hook("UserPromptSubmit"),
        make_hook("PreToolUse"),
        make_hook("PostToolUse"),
        make_hook("SubagentStop"),
        make_hook("PermissionRequest"),
        make_hook("Stop"),
        make_hook("SessionEnd"),
    ]
    .into_iter()
    .collect();

    hooks_map.insert("Notification".to_owned(), notification_entry);

    Value::Object(hooks_map)
}

/// The canonical set of hook event names that stable registers.
/// Changing this list is enough to trigger a re-install on the next run.
const STABLE_HOOK_EVENTS: &[&str] = &[
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "SubagentStop",
    "PermissionRequest",
    "Notification",
    "Stop",
    "SessionEnd",
];

/// Return `true` if `hooks_root` already contains at least one stable hook
/// entry (identified by a URL ending in `/hook` pointing to `127.0.0.1`).
fn has_stable_hooks(hooks_root: &Value) -> bool {
    let Some(obj) = hooks_root.as_object() else {
        return false;
    };
    for event_val in obj.values() {
        let Some(arr) = event_val.as_array() else {
            continue;
        };
        for hook_group in arr {
            let Some(inner) = hook_group.get("hooks").and_then(Value::as_array) else {
                continue;
            };
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

/// Return `true` if all events in `STABLE_HOOK_EVENTS` have a stable hook
/// registered in `hooks_root`.  A `false` return means the installation is
/// stale (e.g. stable was updated and new events were added) and a re-install
/// is required.
fn has_all_stable_hook_events(hooks_root: &Value) -> bool {
    let Some(obj) = hooks_root.as_object() else {
        return false;
    };
    STABLE_HOOK_EVENTS.iter().all(|event| {
        let Some(arr) = obj.get(*event).and_then(Value::as_array) else {
            return false;
        };
        arr.iter().any(|hook_group| {
            let Some(inner) = hook_group.get("hooks").and_then(Value::as_array) else {
                return false;
            };
            inner.iter().any(|h| {
                let url = h.get("url").and_then(Value::as_str).unwrap_or("");
                url.contains("127.0.0.1") && url.ends_with(HOOK_URL_PATH)
            })
        })
    })
}

/// Remove all stable hook entries from the hooks object (in-place).
///
/// A hook group is considered a stable entry when it contains at least one
/// `http` hook whose URL points to `127.0.0.1` and ends with `/hook`.
fn remove_stable_hooks(hooks_root: &mut Value) {
    let Some(obj) = hooks_root.as_object_mut() else {
        return;
    };
    for event_val in obj.values_mut() {
        let Some(arr) = event_val.as_array_mut() else {
            continue;
        };
        arr.retain(|hook_group| {
            let Some(inner) = hook_group.get("hooks").and_then(Value::as_array) else {
                return true;
            };
            !inner.iter().any(|h| {
                let url = h.get("url").and_then(Value::as_str).unwrap_or("");
                url.contains("127.0.0.1") && url.ends_with(HOOK_URL_PATH)
            })
        });
    }
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
        let raw = std::fs::read_to_string(&path).with_context(|| format!("read {:?}", path))?;
        serde_json::from_str(&raw).with_context(|| format!("parse {:?}", path))?
    } else {
        serde_json::json!({})
    };

    // Ensure the "hooks" key exists.
    let hooks = root
        .as_object_mut()
        .context("settings.json root is not an object")?
        .entry("hooks")
        .or_insert_with(|| serde_json::json!({}));

    // Nothing to do if all expected hook events are already registered.
    // If only some events are present (stale install from an older version),
    // remove the existing stable entries and re-merge the full set.
    if has_all_stable_hook_events(hooks) {
        return Ok(());
    }
    if has_stable_hooks(hooks) {
        // Partial / stale install — strip old entries before re-merging.
        remove_stable_hooks(hooks);
    }

    // Merge our four-event block into the existing hooks object.
    let new_block = build_hooks_block(port);
    let hooks_obj = hooks.as_object_mut().context("hooks is not an object")?;
    let new_obj = new_block.as_object().unwrap();

    for (event, new_entries) in new_obj {
        let event_arr = hooks_obj
            .entry(event.clone())
            .or_insert_with(|| serde_json::json!([]));
        let arr = event_arr
            .as_array_mut()
            .context("event hook list is not an array")?;
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
        std::fs::create_dir_all(parent).with_context(|| format!("create dir {:?}", parent))?;
    }
    let tmp = path.with_extension("json.tmp");
    let json = serde_json::to_string_pretty(value).context("serialize settings.json")?;
    std::fs::write(&tmp, json).with_context(|| format!("write {:?}", tmp))?;
    std::fs::rename(&tmp, path).with_context(|| format!("rename {:?} -> {:?}", tmp, path))?;
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
        if !has_all_stable_hook_events(hooks) {
            if has_stable_hooks(hooks) {
                remove_stable_hooks(hooks);
            }
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
        for event in &[
            "SessionStart",
            "UserPromptSubmit",
            "PreToolUse",
            "PostToolUse",
            "SubagentStop",
            "PermissionRequest",
            "Notification",
            "Stop",
            "SessionEnd",
        ] {
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
        assert!(
            !has_stable_hooks(hooks),
            "stable hooks still present after removal"
        );
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

    #[test]
    fn stale_install_is_upgraded() {
        // Simulate an old stable install that only has 4 events registered.
        let old_url = "http://127.0.0.1:15100/hook";
        let stable_entry = serde_json::json!([{
            "hooks": [{"type": "http", "url": old_url}]
        }]);
        let mut root = serde_json::json!({
            "hooks": {
                "SessionStart":     stable_entry.clone(),
                "UserPromptSubmit": stable_entry.clone(),
                "Stop":             stable_entry.clone(),
                "SessionEnd":       stable_entry.clone(),
            }
        });

        // The stale root should be detected as incomplete.
        let hooks = root.get("hooks").unwrap();
        assert!(has_stable_hooks(hooks), "should detect existing hooks");
        assert!(
            !has_all_stable_hook_events(hooks),
            "should detect stale install"
        );

        // After re-install all 8 events must be present with a single entry each.
        install_hooks_into(&mut root, 15100);
        let hooks = root.get("hooks").unwrap().as_object().unwrap();
        for event in STABLE_HOOK_EVENTS {
            let arr = hooks
                .get(*event)
                .and_then(Value::as_array)
                .unwrap_or_else(|| panic!("missing event after upgrade: {event}"));
            assert_eq!(arr.len(), 1, "duplicate hook groups for {event}");
        }
    }
}

pub mod claude_hook_server;

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::agents::AgentAdapter;
use crate::models::{AgentStatus, ContextInfo, ModelResponseEntry};
use claude_hook_server::HookStateMap;

// ---------------------------------------------------------------------------
// ClaudeAdapter
// ---------------------------------------------------------------------------

pub struct ClaudeAdapter {
    flowmux_agent_id: String,
    hook_state: HookStateMap,
}

impl ClaudeAdapter {
    pub fn new(flowmux_agent_id: String, hook_state: HookStateMap) -> Self {
        Self {
            flowmux_agent_id,
            hook_state,
        }
    }

    fn maybe_reparse_transcript(&self) {
        let transcript_path = {
            let map = self.hook_state.lock().unwrap();
            let Some(entry) = map.get(&self.flowmux_agent_id) else {
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
        if let Some(entry) = map.get_mut(&self.flowmux_agent_id) {
            if entry.context_used.is_none() {
                entry.context_used = Some(info.context_used);
            }
            if entry.total_work_ms == 0 {
                entry.total_work_ms = info.total_work_ms;
            }
            if entry.model_name.is_none() && info.model_name.is_some() {
                entry.model_name = info.model_name;
            }
            if entry.model_response_history.is_empty() && !info.response_history.is_empty() {
                entry.model_response_history = info.response_history.clone();
                entry.last_model_response = info
                    .response_history
                    .last()
                    .map(|response| response.text.clone());
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
        map.get(&self.flowmux_agent_id)
            .map(|s| s.status.clone())
            .unwrap_or(AgentStatus::Unknown)
    }

    async fn get_context(&self) -> Option<ContextInfo> {
        self.maybe_reparse_transcript();
        let map = self.hook_state.lock().unwrap();
        let entry = map.get(&self.flowmux_agent_id)?;
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
        map.get(&self.flowmux_agent_id)?.first_prompt.clone()
    }

    async fn get_model_response_history(&self) -> Vec<ModelResponseEntry> {
        self.maybe_reparse_transcript();
        let map = self.hook_state.lock().unwrap();
        map.get(&self.flowmux_agent_id)
            .map(|entry| entry.model_response_history.clone())
            .unwrap_or_default()
    }

    async fn get_last_model_response(&self) -> Option<String> {
        self.maybe_reparse_transcript();
        let map = self.hook_state.lock().unwrap();
        map.get(&self.flowmux_agent_id)?.last_model_response.clone()
    }

    async fn get_model_name(&self) -> Option<String> {
        self.maybe_reparse_transcript();
        let map = self.hook_state.lock().unwrap();
        map.get(&self.flowmux_agent_id)?.model_name.clone()
    }

    async fn get_total_work_ms(&self) -> u64 {
        self.maybe_reparse_transcript();
        let map = self.hook_state.lock().unwrap();
        map.get(&self.flowmux_agent_id)
            .map(|s| s.total_work_ms)
            .unwrap_or(0)
    }

    fn get_cached_session_id(&self) -> Option<String> {
        let map = self.hook_state.lock().unwrap();
        map.get(&self.flowmux_agent_id)?.session_id.clone()
    }
}

// ---------------------------------------------------------------------------
// ClaudeRuntime — owns the HookStateMap and hook server lifecycle
// ---------------------------------------------------------------------------

pub(crate) struct ClaudeRuntime {
    hook_state: HookStateMap,
    persist_tx: tokio::sync::mpsc::UnboundedSender<claude_hook_server::HookPersistEvent>,
    port: u16,
}

impl Drop for ClaudeRuntime {
    fn drop(&mut self) {
        let _ = uninstall_hooks(self.port);
    }
}

impl ClaudeRuntime {
    /// Spawn the hook server and a background persist task, return a runtime
    /// handle.  `session_name` is used by the persist task to find the correct
    /// config file when patching session_id / transcript_path.
    pub(crate) fn start(base_port: u16, session_name: String) -> Self {
        let hook_state: HookStateMap = Arc::new(Mutex::new(HashMap::new()));
        let (persist_tx, mut persist_rx) =
            tokio::sync::mpsc::unbounded_channel::<claude_hook_server::HookPersistEvent>();

        let persist_tx_clone = persist_tx.clone();
        let port =
            claude_hook_server::spawn_hook_server(hook_state.clone(), persist_tx_clone, base_port);

        // Background task: receive persist events and patch the session config file.
        tokio::spawn(async move {
            while let Some(event) = persist_rx.recv().await {
                if let Ok(mut config) = crate::config::Config::load(&session_name) {
                    for agent in config.agents.iter_mut() {
                        if let crate::config::AgentKind::Claude {
                            flowmux_agent_id,
                            session_id,
                            transcript_path,
                        } = &mut agent.kind
                            && *flowmux_agent_id == event.flowmux_agent_id
                        {
                            if let Some(sid) = event.session_id.clone() {
                                *session_id = Some(sid);
                            }
                            if event.transcript_path.is_some() {
                                *transcript_path = event.transcript_path.clone();
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
            port,
        }
    }

    /// The actual port the hook server is listening on (may differ from
    /// the base port when multiple instances are running).
    pub(crate) fn port(&self) -> u16 {
        self.port
    }

    /// Create a `ClaudeAdapter` for a given `flowmux_agent_id`, pre-inserting
    /// a default entry in the shared map if one doesn't already exist.
    pub(crate) fn make_adapter(&self, flowmux_agent_id: String) -> ClaudeAdapter {
        {
            let mut map = self.hook_state.lock().unwrap();
            map.entry(flowmux_agent_id.clone()).or_default();
        }
        ClaudeAdapter::new(flowmux_agent_id, self.hook_state.clone())
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
        let entry = map.entry(id.to_owned()).or_default();

        if session_id.is_some() {
            entry.session_id = session_id;
        }
        if let Some(ref path) = transcript_path {
            entry.transcript_path = Some(path.clone());
            if let Some(info) = claude_hook_server::parse_transcript(path) {
                entry.context_used = Some(info.context_used);
                entry.model_response_history = info.response_history.clone();
                entry.last_model_response = info
                    .response_history
                    .last()
                    .map(|response| response.text.clone());
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
                flowmux_agent_id: id.to_owned(),
                session_id: entry.session_id.clone(),
                transcript_path: Some(path.clone()),
            });
        } else if entry.session_id.is_some() {
            // If we have a session_id but no transcript_path yet (e.g., flowmux restarted
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

/// The URL pattern that identifies flowmux's hook entries inside
/// `~/.claude/settings.json`.  Used to detect whether installation is
/// already present and to remove stale entries when the port changes.
const HOOK_URL_PATH: &str = "/hook";

/// Build the four-event hooks block that flowmux merges into
/// `~/.claude/settings.json`.
fn build_hooks_block(port: u16) -> Value {
    let url = format!("http://127.0.0.1:{}{}", port, HOOK_URL_PATH);

    let make_hook = |event: &str| -> (String, Value) {
        let entry = serde_json::json!([{
            "hooks": [{
                "type": "http",
                "url": url,
                "headers": { "X-Flowmux-Agent-Id": "$FLOWMUX_AGENT_ID" },
                "allowedEnvVars": ["FLOWMUX_AGENT_ID"]
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
            "headers": { "X-Flowmux-Agent-Id": "$FLOWMUX_AGENT_ID" },
            "allowedEnvVars": ["FLOWMUX_AGENT_ID"]
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

/// The canonical set of hook event names that flowmux registers.
/// Changing this list is enough to trigger a re-install on the next run.
const FLOWMUX_HOOK_EVENTS: &[&str] = &[
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

/// Return `true` if `hooks_root` contains a flowmux hook entry for the
/// given `port` (identified by the exact URL `http://127.0.0.1:<port>/hook`).
fn has_flowmux_hooks_for_port(hooks_root: &Value, port: u16) -> bool {
    let url = format!("http://127.0.0.1:{}{}", port, HOOK_URL_PATH);
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
                if h.get("url").and_then(Value::as_str) == Some(&url) {
                    return true;
                }
            }
        }
    }
    false
}

/// Return `true` if all events in `FLOWMUX_HOOK_EVENTS` have a flowmux hook
/// registered for the given `port` in `hooks_root`.  A `false` return means
/// the installation is incomplete or stale and a re-install is required.
fn has_all_flowmux_hook_events_for_port(hooks_root: &Value, port: u16) -> bool {
    let url = format!("http://127.0.0.1:{}{}", port, HOOK_URL_PATH);
    let Some(obj) = hooks_root.as_object() else {
        return false;
    };
    FLOWMUX_HOOK_EVENTS.iter().all(|event| {
        let Some(arr) = obj.get(*event).and_then(Value::as_array) else {
            return false;
        };
        arr.iter().any(|hook_group| {
            let Some(inner) = hook_group.get("hooks").and_then(Value::as_array) else {
                return false;
            };
            inner
                .iter()
                .any(|h| h.get("url").and_then(Value::as_str) == Some(&url))
        })
    })
}

/// Remove flowmux hook entries for a specific `port` from the hooks object
/// (in-place).  Entries for other ports are preserved.
fn remove_flowmux_hooks_for_port(hooks_root: &mut Value, port: u16) {
    let url = format!("http://127.0.0.1:{}{}", port, HOOK_URL_PATH);
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
            !inner
                .iter()
                .any(|h| h.get("url").and_then(Value::as_str) == Some(&url))
        });
    }
}

/// Extract all unique flowmux hook ports from the hooks object.
fn extract_flowmux_ports(hooks_root: &Value) -> Vec<u16> {
    let mut ports = Vec::new();
    let Some(obj) = hooks_root.as_object() else {
        return ports;
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
                if let Some(url) = h.get("url").and_then(Value::as_str)
                    && url.contains("127.0.0.1")
                    && url.ends_with(HOOK_URL_PATH)
                    && let Some(port_str) = url
                        .strip_prefix("http://127.0.0.1:")
                        .and_then(|s| s.strip_suffix(HOOK_URL_PATH))
                    && let Ok(port) = port_str.parse::<u16>()
                    && !ports.contains(&port)
                {
                    ports.push(port);
                }
            }
        }
    }
    ports
}

/// Return `true` if a TCP connection to `127.0.0.1:<port>` succeeds within
/// 100ms, indicating the hook server is still alive.
fn is_port_alive(port: u16) -> bool {
    use std::time::Duration;
    let addr = format!("127.0.0.1:{}", port);
    std::net::TcpStream::connect_timeout(&addr.parse().unwrap(), Duration::from_millis(100)).is_ok()
}

/// Remove hook entries for flowmux instances whose servers are no longer
/// running (dead ports).  Preserves entries for live ports.
fn cleanup_dead_hooks(hooks_root: &mut Value) {
    let ports = extract_flowmux_ports(hooks_root);
    for port in ports {
        if !is_port_alive(port) {
            remove_flowmux_hooks_for_port(hooks_root, port);
        }
    }
}

fn settings_path() -> Option<std::path::PathBuf> {
    dirs::home_dir().map(|h| h.join(".claude").join("settings.json"))
}

/// Merge flowmux's HTTP hooks into `~/.claude/settings.json` for this instance's port.
///
/// - Cleans up hooks from dead flowmux instances first.
/// - No-op if all expected events are already registered for this port.
/// - Upgrades stale/partial installations for this port by removing and
///   re-adding them.
/// - Preserves hooks from other live flowmux instances running on different ports.
pub fn install_hooks(port: u16) -> Result<()> {
    let path = settings_path().context("cannot determine home directory")?;

    let mut root: Value = if path.exists() {
        let raw = std::fs::read_to_string(&path).with_context(|| format!("read {:?}", path))?;
        serde_json::from_str(&raw).with_context(|| format!("parse {:?}", path))?
    } else {
        serde_json::json!({})
    };

    let hooks = root
        .as_object_mut()
        .context("settings.json root is not an object")?
        .entry("hooks")
        .or_insert_with(|| serde_json::json!({}));

    cleanup_dead_hooks(hooks);

    if has_all_flowmux_hook_events_for_port(hooks, port) {
        return Ok(());
    }
    if has_flowmux_hooks_for_port(hooks, port) {
        remove_flowmux_hooks_for_port(hooks, port);
    }

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

/// Remove this instance's hook entries from `~/.claude/settings.json`.
/// Hooks from other flowmux instances are preserved.
pub fn uninstall_hooks(port: u16) -> Result<()> {
    let path = settings_path().context("cannot determine home directory")?;
    uninstall_hooks_at(&path, port)
}

fn uninstall_hooks_at(path: &std::path::Path, port: u16) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }

    let raw = std::fs::read_to_string(path).with_context(|| format!("read {:?}", path))?;
    let mut root: Value =
        serde_json::from_str(&raw).with_context(|| format!("parse {:?}", path))?;

    if let Some(hooks) = root.get_mut("hooks") {
        remove_flowmux_hooks_for_port(hooks, port);
    }

    write_settings(path, &root)
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

    fn install_hooks_into(root: &mut Value, port: u16) {
        let hooks = root
            .as_object_mut()
            .unwrap()
            .entry("hooks")
            .or_insert_with(|| serde_json::json!({}));
        if !has_all_flowmux_hook_events_for_port(hooks, port) {
            if has_flowmux_hooks_for_port(hooks, port) {
                remove_flowmux_hooks_for_port(hooks, port);
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
    fn install_adds_all_events() {
        let root = make_settings(15100);
        let hooks = root.get("hooks").unwrap().as_object().unwrap();
        for event in FLOWMUX_HOOK_EVENTS {
            assert!(hooks.contains_key(*event), "missing event: {event}");
        }
    }

    #[test]
    fn install_is_idempotent() {
        let mut root = make_settings(15100);
        install_hooks_into(&mut root, 15100);
        let hooks = root.get("hooks").unwrap().as_object().unwrap();
        let start_arr = hooks["SessionStart"].as_array().unwrap();
        assert_eq!(start_arr.len(), 1, "duplicate hook groups added");
    }

    #[test]
    fn uninstall_removes_flowmux_entries() {
        let mut root = make_settings(15100);
        if let Some(hooks) = root.get_mut("hooks") {
            remove_flowmux_hooks_for_port(hooks, 15100);
        }
        let hooks = root.get("hooks").unwrap();
        assert!(
            !has_flowmux_hooks_for_port(hooks, 15100),
            "flowmux hooks still present after removal"
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
            remove_flowmux_hooks_for_port(hooks, 15100);
        }
        let arr = root["hooks"]["SessionStart"].as_array().unwrap();
        assert_eq!(arr.len(), 1, "user hook was incorrectly removed");
        let inner = arr[0]["hooks"].as_array().unwrap();
        assert_eq!(inner[0]["type"], "command");
    }

    #[test]
    fn stale_install_is_upgraded() {
        let old_url = "http://127.0.0.1:15100/hook";
        let flowmux_entry = serde_json::json!([{
            "hooks": [{"type": "http", "url": old_url}]
        }]);
        let mut root = serde_json::json!({
            "hooks": {
                "SessionStart":     flowmux_entry.clone(),
                "UserPromptSubmit": flowmux_entry.clone(),
                "Stop":             flowmux_entry.clone(),
                "SessionEnd":       flowmux_entry.clone(),
            }
        });

        let hooks = root.get("hooks").unwrap();
        assert!(
            has_flowmux_hooks_for_port(hooks, 15100),
            "should detect existing hooks"
        );
        assert!(
            !has_all_flowmux_hook_events_for_port(hooks, 15100),
            "should detect stale install"
        );

        install_hooks_into(&mut root, 15100);
        let hooks = root.get("hooks").unwrap().as_object().unwrap();
        for event in FLOWMUX_HOOK_EVENTS {
            let arr = hooks
                .get(*event)
                .and_then(Value::as_array)
                .unwrap_or_else(|| panic!("missing event after upgrade: {event}"));
            assert_eq!(arr.len(), 1, "duplicate hook groups for {event}");
        }
    }

    #[test]
    fn two_ports_coexist() {
        let mut root = serde_json::json!({});
        install_hooks_into(&mut root, 15100);
        install_hooks_into(&mut root, 15101);

        let hooks_val = root.get("hooks").unwrap();
        let hooks = hooks_val.as_object().unwrap();
        for event in FLOWMUX_HOOK_EVENTS {
            let arr = hooks
                .get(*event)
                .and_then(Value::as_array)
                .unwrap_or_else(|| panic!("missing event: {event}"));
            assert_eq!(
                arr.len(),
                2,
                "expected two hook groups for {event} (one per port)"
            );
        }

        let hooks = root.get("hooks").unwrap();
        assert!(has_all_flowmux_hook_events_for_port(hooks, 15100));
        assert!(has_all_flowmux_hook_events_for_port(hooks, 15101));
    }

    #[test]
    fn uninstall_preserves_other_port() {
        let mut root = serde_json::json!({});
        install_hooks_into(&mut root, 15100);
        install_hooks_into(&mut root, 15101);

        if let Some(hooks) = root.get_mut("hooks") {
            remove_flowmux_hooks_for_port(hooks, 15100);
        }

        let hooks = root.get("hooks").unwrap();
        assert!(!has_flowmux_hooks_for_port(hooks, 15100));
        assert!(has_all_flowmux_hook_events_for_port(hooks, 15101));
    }

    #[test]
    fn extract_ports_finds_both() {
        let mut root = serde_json::json!({});
        install_hooks_into(&mut root, 15100);
        install_hooks_into(&mut root, 15101);

        let hooks = root.get("hooks").unwrap();
        let mut ports = extract_flowmux_ports(hooks);
        ports.sort();
        assert_eq!(ports, vec![15100, 15101]);
    }

    #[test]
    fn extract_ports_empty() {
        let root = serde_json::json!({"hooks": {}});
        let ports = extract_flowmux_ports(root.get("hooks").unwrap());
        assert!(ports.is_empty());
    }

    #[test]
    fn uninstall_hooks_removes_entries_from_file() {
        let dir =
            std::env::temp_dir().join(format!("flowmux-test-uninstall-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");

        let root = make_settings(15100);
        write_settings(&path, &root).unwrap();

        uninstall_hooks_at(&path, 15100).unwrap();

        let raw = std::fs::read_to_string(&path).unwrap();
        let updated: Value = serde_json::from_str(&raw).unwrap();
        assert!(
            !has_flowmux_hooks_for_port(&updated, 15100),
            "hooks for port 15100 should be removed"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}

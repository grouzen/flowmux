use std::collections::HashMap;
use std::io::BufRead;
use std::net::TcpListener as StdTcpListener;
use std::sync::{Arc, Mutex};

use axum::extract::Extension;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::post;
use axum::{Json, Router};
use serde_json::Value;
use tokio::sync::mpsc::UnboundedSender;

use crate::models::AgentStatus;

// ---------------------------------------------------------------------------
// Shared state types
// ---------------------------------------------------------------------------

/// Per-agent state maintained by the hook server.
pub struct ClaudeHookState {
    pub status: AgentStatus,
    pub first_prompt: Option<String>,
    pub last_model_response: Option<String>,
    pub model_name: Option<String>,
    pub session_id: Option<String>,
    pub transcript_path: Option<String>,
    pub context_used: Option<u64>,
    /// Sum of all `TurnDuration` system-entry `durationMs` values in the transcript.
    pub total_work_ms: u64,
}

impl Default for ClaudeHookState {
    fn default() -> Self {
        Self {
            status: AgentStatus::Idle,
            first_prompt: None,
            last_model_response: None,
            model_name: None,
            session_id: None,
            transcript_path: None,
            context_used: None,
            total_work_ms: 0,
        }
    }
}

/// Map from `flowmux_agent_id` → hook state.
pub type HookStateMap = Arc<Mutex<HashMap<String, ClaudeHookState>>>;

// ---------------------------------------------------------------------------
// Persist channel (hooks signal App to write session_id/path to toml)
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct HookPersistEvent {
    pub flowmux_agent_id: String,
    /// `None` means "no change to session_id in config".
    pub session_id: Option<String>,
    pub transcript_path: Option<String>,
}

// ---------------------------------------------------------------------------
// Shared handler state (passed via axum Extension)
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct HookServerState {
    hook_state: HookStateMap,
    persist_tx: UnboundedSender<HookPersistEvent>,
}

// ---------------------------------------------------------------------------
// axum POST /hook handler
// ---------------------------------------------------------------------------

async fn hook_handler(
    headers: HeaderMap,
    Extension(state): Extension<HookServerState>,
    Json(body): Json<Value>,
) -> StatusCode {
    // Identify which agent this hook belongs to via the custom header.
    let agent_id = match headers
        .get("x-flowmux-agent-id")
        .and_then(|v| v.to_str().ok())
    {
        Some(id) => id.to_owned(),
        None => return StatusCode::BAD_REQUEST,
    };

    // The entry must have been pre-inserted by App before the agent launches.
    // With fan-out from multiple flowmux instances, this server may receive
    // hooks for agents it doesn't own — silently ignore them.
    {
        let map = state.hook_state.lock().unwrap();
        if !map.contains_key(&agent_id) {
            return StatusCode::OK;
        }
    }

    let event_name = match body.get("hook_event_name").and_then(Value::as_str) {
        Some(n) => n.to_owned(),
        None => return StatusCode::OK, // unknown / malformed — ignore
    };

    match event_name.as_str() {
        "SessionStart" => {
            let transcript_path = body
                .get("transcript_path")
                .and_then(Value::as_str)
                .map(str::to_owned);
            let model_name = body.get("model").and_then(Value::as_str).map(str::to_owned);

            // Prefer session_id from the hook payload; fall back to deriving
            // it from the transcript filename (the UUID stem is the session ID
            // used by `claude --resume`).
            let session_id = body
                .get("session_id")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .or_else(|| session_id_from_transcript_path(transcript_path.as_deref()?));

            if session_id.is_some() || transcript_path.is_some() {
                // Signal the background persist task to save session_id +
                // transcript_path to the on-disk config for this session.
                let _ = state.persist_tx.send(HookPersistEvent {
                    flowmux_agent_id: agent_id.clone(),
                    session_id: session_id.clone(),
                    transcript_path: transcript_path.clone(),
                });
            }

            let mut map = state.hook_state.lock().unwrap();
            if let Some(entry) = map.get_mut(&agent_id) {
                entry.session_id = session_id;
                entry.transcript_path = transcript_path;
                if model_name.is_some() {
                    entry.model_name = model_name;
                }
                entry.status = AgentStatus::Running;
            }
        }

        "UserPromptSubmit" => {
            let prompt = body
                .get("prompt")
                .and_then(Value::as_str)
                .map(str::to_owned);

            let mut map = state.hook_state.lock().unwrap();
            if let Some(entry) = map.get_mut(&agent_id) {
                if entry.first_prompt.is_none() {
                    entry.first_prompt = prompt.clone();
                }
                // Only set status to Running if this is a real user prompt (not internal scaffolding)
                let is_real_prompt = prompt
                    .as_ref()
                    .map(|p| {
                        let trimmed = p.trim_start();
                        !trimmed.is_empty() && !trimmed.starts_with('<')
                    })
                    .unwrap_or(false);

                if is_real_prompt {
                    entry.status = AgentStatus::Running;
                }
            }
        }

        "Stop" => {
            // The Stop hook payload includes `last_assistant_message` — the
            // text of Claude's final response for this turn.  Use it directly
            // so we never miss an update due to transcript-file flush timing.
            let last_assistant_message = body
                .get("last_assistant_message")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_owned);

            // Optionally a fresh transcript_path in the payload.
            let transcript_path_override = body
                .get("transcript_path")
                .and_then(Value::as_str)
                .map(str::to_owned);

            let transcript_path = {
                let map = state.hook_state.lock().unwrap();
                transcript_path_override
                    .clone()
                    .or_else(|| map.get(&agent_id)?.transcript_path.clone())
            };

            let parsed = transcript_path.as_deref().and_then(parse_transcript);

            let mut map = state.hook_state.lock().unwrap();
            if let Some(entry) = map.get_mut(&agent_id) {
                // Prefer the payload's last_assistant_message (guaranteed
                // fresh) over the transcript-parsed text.  Fall back to the
                // transcript value so that tool-use-only turns (no text in
                // the payload) don't erase the previous response.
                if last_assistant_message.is_some() {
                    entry.last_model_response = last_assistant_message;
                } else if let Some(ref info) = parsed {
                    if info.last_response_text.is_some() {
                        entry.last_model_response = info.last_response_text.clone();
                    }
                }
                if let Some(info) = parsed {
                    entry.context_used = Some(info.context_used);
                    if info.model_name.is_some() {
                        entry.model_name = info.model_name;
                    }
                    entry.total_work_ms = info.total_work_ms;
                }
                if transcript_path_override.is_some() {
                    entry.transcript_path = transcript_path_override;
                }
                // Derive session_id from transcript filename if still unknown.
                if entry.session_id.is_none() {
                    entry.session_id = entry
                        .transcript_path
                        .as_deref()
                        .and_then(session_id_from_transcript_path);
                }
                entry.status = AgentStatus::Idle;

                // Persist transcript_path (and session_id if known) so that on
                // the next startup restore() can parse the transcript and show
                // meta info immediately without waiting for a new prompt.
                if entry.transcript_path.is_some() || entry.session_id.is_some() {
                    let _ = state.persist_tx.send(HookPersistEvent {
                        flowmux_agent_id: agent_id.clone(),
                        session_id: entry.session_id.clone(),
                        transcript_path: entry.transcript_path.clone(),
                    });
                }
            }
        }

        "SessionEnd" => {
            let mut map = state.hook_state.lock().unwrap();
            if let Some(entry) = map.get_mut(&agent_id) {
                entry.status = AgentStatus::Stopped;
            }
        }

        "PreToolUse" | "PostToolUse" | "SubagentStop" => {
            let mut map = state.hook_state.lock().unwrap();
            if let Some(entry) = map.get_mut(&agent_id) {
                entry.status = AgentStatus::Running;
            }
        }

        "PermissionRequest" => {
            let mut map = state.hook_state.lock().unwrap();
            if let Some(entry) = map.get_mut(&agent_id) {
                entry.status = AgentStatus::WaitingForInput;
            }
        }

        // `Notification` with `permission_prompt` matcher fires when a permission
        // dialog appears, including for built-in tools (e.g. `Skill`) that bypass
        // the `PermissionRequest` hook.  We only register this hook with the
        // `permission_prompt` matcher so every `Notification` payload we receive
        // here is guaranteed to be a permission prompt.
        "Notification" => {
            let mut map = state.hook_state.lock().unwrap();
            if let Some(entry) = map.get_mut(&agent_id) {
                entry.status = AgentStatus::WaitingForInput;
            }
        }

        _ => {} // unknown event — no-op
    }

    StatusCode::OK
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Derive a Claude Code session ID from a transcript file path.
///
/// Claude Code names transcript files as `<session-uuid>.jsonl`, so the stem
/// of the filename is the session ID accepted by `claude --resume`.
///
/// Returns `None` if the path has no stem or if the stem doesn't look like a
/// UUID (36 chars with hyphens).
fn session_id_from_transcript_path(transcript_path: &str) -> Option<String> {
    let stem = std::path::Path::new(transcript_path)
        .file_stem()
        .and_then(|s| s.to_str())?;
    // Basic sanity check: UUIDs are 36 chars (32 hex + 4 hyphens).
    if stem.len() == 36 && stem.chars().filter(|&c| c == '-').count() == 4 {
        Some(stem.to_owned())
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Spawn the hook server
// ---------------------------------------------------------------------------

fn find_free_port(from: u16) -> u16 {
    let mut port = from;
    loop {
        if StdTcpListener::bind(format!("127.0.0.1:{}", port)).is_ok() {
            return port;
        }
        port += 1;
    }
}

pub fn spawn_hook_server(
    hook_state: HookStateMap,
    persist_tx: UnboundedSender<HookPersistEvent>,
    base_port: u16,
) -> u16 {
    let port = find_free_port(base_port);
    tokio::spawn(async move {
        let state = HookServerState {
            hook_state,
            persist_tx,
        };
        let app = Router::new()
            .route("/hook", post(hook_handler))
            .layer(Extension(state));
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
            .await
            .expect("bind should succeed after find_free_port");
        axum::serve(listener, app).await.unwrap();
    });
    port
}

// ---------------------------------------------------------------------------
// Transcript parsing helpers
// ---------------------------------------------------------------------------

/// Data recovered from the last assistant entry in a transcript file.
pub struct TranscriptInfo {
    /// Total input-context tokens from the last assistant turn.
    pub context_used: u64,
    /// Plain text of the last assistant response (first Text block).
    pub last_response_text: Option<String>,
    /// Model name reported by the last assistant message.
    pub model_name: Option<String>,
    /// Sum of all `TurnDuration` system-entry `durationMs` values — equivalent
    /// to OpenCode's `total_work_ms` (model generation time across the session).
    pub total_work_ms: u64,
    /// Text of the first human prompt in the session (for cold-restart display).
    pub first_prompt: Option<String>,
}

/// Parse `transcript_path` (JSONL) and return info from the last assistant entry.
/// Returns `None` if the file cannot be opened or contains no assistant entries.
pub fn parse_transcript(transcript_path: &str) -> Option<TranscriptInfo> {
    use claude_code_transcripts::types::{
        AssistantContentBlock, Entry, SystemSubtype, UserContent, UserContentBlock, UserRole,
    };

    let file = std::fs::File::open(transcript_path).ok()?;
    let reader = std::io::BufReader::new(file);

    let mut first_prompt: Option<String> = None;
    let mut last_context_used: Option<u64> = None;
    let mut last_response_text: Option<String> = None;
    let mut last_model_name: Option<String> = None;

    let mut turn_duration_sum_ms: u64 = 0;

    let mut ts_total_work_ms: u64 = 0;
    let mut current_turn_user_ts: Option<i64> = None;
    let mut current_turn_last_assistant_ts: Option<i64> = None;

    for line in reader.lines().map_while(Result::ok) {
        let line = line.trim().to_owned();
        if line.is_empty() {
            continue;
        }
        let entry: Entry = match serde_json::from_str(&line) {
            Ok(e) => e,
            Err(_) => continue,
        };

        match entry {
            Entry::User(u) => {
                if u.envelope.is_meta == Some(true) {
                    continue;
                }
                if !matches!(u.message.role, UserRole::User) {
                    continue;
                }
                if let UserContent::Blocks(blocks) = &u.message.content {
                    if blocks
                        .iter()
                        .any(|b| matches!(b, UserContentBlock::ToolResult { .. }))
                    {
                        continue;
                    }
                }

                let text = match &u.message.content {
                    UserContent::Text(s) => Some(s.clone()),
                    UserContent::Blocks(blocks) => blocks.iter().find_map(|b| {
                        if let UserContentBlock::Text { text } = b {
                            Some(text.clone())
                        } else {
                            None
                        }
                    }),
                    UserContent::Other(_) => None,
                };

                let text = match text {
                    Some(t) if t.trim_start().starts_with('<') => None,
                    other => other,
                };

                if text.is_some() {
                    if let (Some(u_ts), Some(a_ts)) =
                        (current_turn_user_ts, current_turn_last_assistant_ts)
                    {
                        let delta = a_ts.saturating_sub(u_ts).max(0) as u64;
                        ts_total_work_ms += delta;
                    }
                    current_turn_user_ts = parse_ts_ms(&u.envelope.timestamp);
                    current_turn_last_assistant_ts = None;

                    if first_prompt.is_none() {
                        first_prompt = text;
                    }
                }
            }

            Entry::Assistant(a) => {
                let usage = &a.message.usage;
                let context_used = usage.input_tokens
                    + usage.cache_read_input_tokens.unwrap_or(0)
                    + usage.cache_creation_input_tokens.unwrap_or(0);

                let response_text = a.message.content.iter().find_map(|block| {
                    if let AssistantContentBlock::Text { text } = block {
                        Some(text.clone())
                    } else {
                        None
                    }
                });

                last_context_used = Some(context_used);
                if response_text.is_some() {
                    last_response_text = response_text;
                }
                last_model_name = a.message.model.clone();

                current_turn_last_assistant_ts = parse_ts_ms(&a.envelope.timestamp);
            }

            Entry::System(s) => {
                if matches!(s.subtype, SystemSubtype::TurnDuration) {
                    if let Some(ms) = s.duration_ms {
                        turn_duration_sum_ms += ms as u64;
                    }
                }
            }

            _ => {}
        }
    }

    if let (Some(u_ts), Some(a_ts)) = (current_turn_user_ts, current_turn_last_assistant_ts) {
        let delta = a_ts.saturating_sub(u_ts).max(0) as u64;
        ts_total_work_ms += delta;
    }

    let total_work_ms = if turn_duration_sum_ms > 0 {
        turn_duration_sum_ms
    } else {
        ts_total_work_ms
    };

    last_context_used.map(|context_used| TranscriptInfo {
        context_used,
        last_response_text,
        model_name: last_model_name,
        total_work_ms,
        first_prompt,
    })
}

/// Parse an ISO 8601 timestamp string (e.g. `"2026-05-08T05:27:28.047Z"`) into
/// milliseconds since the Unix epoch.  Returns `None` on parse failure.
fn parse_ts_ms(ts: &str) -> Option<i64> {
    use chrono::DateTime;
    DateTime::parse_from_rfc3339(ts)
        .ok()
        .map(|dt| dt.timestamp_millis())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn make_transcript(lines: &[&str]) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("flowmux-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("transcript.jsonl");
        let mut f = std::fs::File::create(&path).unwrap();
        for line in lines {
            writeln!(f, "{}", line).unwrap();
        }
        f.flush().unwrap();
        path
    }

    fn cleanup(path: &std::path::Path) {
        if let Some(parent) = path.parent() {
            let _ = std::fs::remove_dir_all(parent);
        }
    }

    const USER_T0: &str = r#"{"type":"user","message":{"role":"user","content":"hello"},"uuid":"u1","parentUuid":null,"isSidechain":false,"sessionId":"s1","timestamp":"2026-01-01T00:00:00.000Z"}"#;
    const ASSISTANT_T5: &str = r#"{"type":"assistant","message":{"id":"m1","type":"message","role":"assistant","model":"claude-sonnet-4-5","content":[{"type":"text","text":"hi"}],"stop_reason":null,"stop_sequence":null,"stop_details":null,"usage":{"input_tokens":100,"output_tokens":50}},"uuid":"a1","parentUuid":"u1","isSidechain":false,"sessionId":"s1","timestamp":"2026-01-01T00:00:05.000Z"}"#;
    const TOOL_RESULT_T50: &str = r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"tu1","content":"result"}]},"uuid":"tr1","parentUuid":"a1","isSidechain":false,"sessionId":"s1","timestamp":"2026-01-01T00:00:50.000Z"}"#;
    const ASSISTANT_T60: &str = r#"{"type":"assistant","message":{"id":"m2","type":"message","role":"assistant","model":"claude-sonnet-4-5","content":[{"type":"text","text":"done"}],"stop_reason":null,"stop_sequence":null,"stop_details":null,"usage":{"input_tokens":200,"output_tokens":100}},"uuid":"a2","parentUuid":"tr1","isSidechain":false,"sessionId":"s1","timestamp":"2026-01-01T00:01:00.000Z"}"#;

    #[test]
    fn parse_transcript_skips_tool_results() {
        let path = make_transcript(&[USER_T0, ASSISTANT_T5, TOOL_RESULT_T50, ASSISTANT_T60]);
        let info = parse_transcript(path.to_str().unwrap()).unwrap();
        cleanup(&path);
        assert_eq!(info.total_work_ms, 60_000);
    }

    #[test]
    fn parse_transcript_multiple_turns() {
        let path = make_transcript(&[
            r#"{"type":"user","message":{"role":"user","content":"first"},"uuid":"u1","parentUuid":null,"isSidechain":false,"sessionId":"s1","timestamp":"2026-01-01T00:00:00.000Z"}"#,
            r#"{"type":"assistant","message":{"id":"m1","type":"message","role":"assistant","model":"claude-sonnet-4-5","content":[{"type":"text","text":"r1"}],"stop_reason":null,"stop_sequence":null,"stop_details":null,"usage":{"input_tokens":100,"output_tokens":50}},"uuid":"a1","parentUuid":"u1","isSidechain":false,"sessionId":"s1","timestamp":"2026-01-01T00:00:10.000Z"}"#,
            r#"{"type":"user","message":{"role":"user","content":"second"},"uuid":"u2","parentUuid":"a1","isSidechain":false,"sessionId":"s1","timestamp":"2026-01-01T00:01:40.000Z"}"#,
            r#"{"type":"assistant","message":{"id":"m2","type":"message","role":"assistant","model":"claude-sonnet-4-5","content":[{"type":"text","text":"r2"}],"stop_reason":null,"stop_sequence":null,"stop_details":null,"usage":{"input_tokens":200,"output_tokens":100}},"uuid":"a2","parentUuid":"u2","isSidechain":false,"sessionId":"s1","timestamp":"2026-01-01T00:02:00.000Z"}"#,
        ]);
        let info = parse_transcript(path.to_str().unwrap()).unwrap();
        cleanup(&path);
        assert_eq!(info.total_work_ms, 30_000);
    }

    #[test]
    fn parse_transcript_skips_meta_entries() {
        let path = make_transcript(&[
            r#"{"type":"user","message":{"role":"user","content":"real prompt"},"uuid":"u1","parentUuid":null,"isSidechain":false,"isMeta":false,"sessionId":"s1","timestamp":"2026-01-01T00:00:00.000Z"}"#,
            r#"{"type":"user","message":{"role":"user","content":"meta prompt"},"uuid":"u2","parentUuid":"u1","isSidechain":false,"isMeta":true,"sessionId":"s1","timestamp":"2026-01-01T00:00:05.000Z"}"#,
            r#"{"type":"assistant","message":{"id":"m1","type":"message","role":"assistant","model":"claude-sonnet-4-5","content":[{"type":"text","text":"hi"}],"stop_reason":null,"stop_sequence":null,"stop_details":null,"usage":{"input_tokens":100,"output_tokens":50}},"uuid":"a1","parentUuid":"u2","isSidechain":false,"sessionId":"s1","timestamp":"2026-01-01T00:00:10.000Z"}"#,
        ]);
        let info = parse_transcript(path.to_str().unwrap()).unwrap();
        cleanup(&path);
        assert_eq!(info.total_work_ms, 10_000);
    }

    #[test]
    fn parse_transcript_prefers_turn_duration_over_timestamps() {
        let path = make_transcript(&[
            USER_T0,
            ASSISTANT_T5,
            TOOL_RESULT_T50,
            ASSISTANT_T60,
            r#"{"type":"system","subtype":"turn_duration","durationMs":12958,"messageCount":15,"uuid":"s1","parentUuid":"a2","isSidechain":false,"isMeta":false,"sessionId":"s1","timestamp":"2026-01-01T00:01:00.100Z"}"#,
        ]);
        let info = parse_transcript(path.to_str().unwrap()).unwrap();
        cleanup(&path);
        assert_eq!(info.total_work_ms, 12958);
    }

    #[test]
    fn parse_transcript_sums_multiple_turn_durations() {
        let path = make_transcript(&[
            USER_T0,
            ASSISTANT_T5,
            r#"{"type":"system","subtype":"turn_duration","durationMs":5000,"messageCount":3,"uuid":"s1","parentUuid":"a1","isSidechain":false,"isMeta":false,"sessionId":"s1","timestamp":"2026-01-01T00:00:05.100Z"}"#,
            r#"{"type":"user","message":{"role":"user","content":"second"},"uuid":"u2","parentUuid":"a1","isSidechain":false,"sessionId":"s1","timestamp":"2026-01-01T00:01:00.000Z"}"#,
            ASSISTANT_T60,
            r#"{"type":"system","subtype":"turn_duration","durationMs":8000,"messageCount":4,"uuid":"s2","parentUuid":"a2","isSidechain":false,"isMeta":false,"sessionId":"s1","timestamp":"2026-01-01T00:01:05.100Z"}"#,
        ]);
        let info = parse_transcript(path.to_str().unwrap()).unwrap();
        cleanup(&path);
        assert_eq!(info.total_work_ms, 13000);
    }
}

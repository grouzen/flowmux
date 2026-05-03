use std::collections::HashMap;
use std::io::BufRead;
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
}

impl Default for ClaudeHookState {
    fn default() -> Self {
        Self {
            status: AgentStatus::Unknown,
            first_prompt: None,
            last_model_response: None,
            model_name: None,
            session_id: None,
            transcript_path: None,
            context_used: None,
        }
    }
}

/// Map from `stable_agent_id` → hook state.
pub type HookStateMap = Arc<Mutex<HashMap<String, ClaudeHookState>>>;

// ---------------------------------------------------------------------------
// Persist channel (SessionStart signals App to write session_id/path to toml)
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct HookPersistEvent {
    pub stable_agent_id: String,
    pub session_id: String,
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
        .get("x-stable-agent-id")
        .and_then(|v| v.to_str().ok())
    {
        Some(id) => id.to_owned(),
        None => return StatusCode::BAD_REQUEST,
    };

    // The entry must have been pre-inserted by App before the agent launches.
    {
        let map = state.hook_state.lock().unwrap();
        if !map.contains_key(&agent_id) {
            return StatusCode::BAD_REQUEST;
        }
    }

    let event_name = match body.get("hook_event_name").and_then(Value::as_str) {
        Some(n) => n.to_owned(),
        None => return StatusCode::OK, // unknown / malformed — ignore
    };

    match event_name.as_str() {
        "SessionStart" => {
            let session_id = body
                .get("session_id")
                .and_then(Value::as_str)
                .map(str::to_owned);
            let transcript_path = body
                .get("transcript_path")
                .and_then(Value::as_str)
                .map(str::to_owned);
            let model_name = body
                .get("model")
                .and_then(Value::as_str)
                .map(str::to_owned);

            if let Some(sid) = &session_id {
                // Signal App to persist session_id + transcript_path to disk.
                let _ = state.persist_tx.send(HookPersistEvent {
                    stable_agent_id: agent_id.clone(),
                    session_id: sid.clone(),
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
                    entry.first_prompt = prompt;
                }
                entry.status = AgentStatus::Running;
            }
        }

        "Stop" => {
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

            let parsed = transcript_path
                .as_deref()
                .and_then(parse_transcript);

            let mut map = state.hook_state.lock().unwrap();
            if let Some(entry) = map.get_mut(&agent_id) {
                if let Some(info) = parsed {
                    entry.context_used = Some(info.context_used);
                    entry.last_model_response = info.last_response_text;
                    if info.model_name.is_some() {
                        entry.model_name = info.model_name;
                    }
                }
                if transcript_path_override.is_some() {
                    entry.transcript_path = transcript_path_override;
                }
                entry.status = AgentStatus::WaitingForInput;
            }
        }

        "SessionEnd" => {
            let mut map = state.hook_state.lock().unwrap();
            if let Some(entry) = map.get_mut(&agent_id) {
                entry.status = AgentStatus::Stopped;
            }
        }

        _ => {} // unknown event — no-op
    }

    StatusCode::OK
}

// ---------------------------------------------------------------------------
// Spawn the hook server
// ---------------------------------------------------------------------------

pub fn spawn_hook_server(
    hook_state: HookStateMap,
    persist_tx: UnboundedSender<HookPersistEvent>,
    port: u16,
) {
    tokio::spawn(async move {
        let state = HookServerState { hook_state, persist_tx };
        let app = Router::new()
            .route("/hook", post(hook_handler))
            .layer(Extension(state));
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
            .await
            .unwrap_or_else(|e| {
                panic!(
                    "hook server: failed to bind 127.0.0.1:{port} — {e}\n\
                     Tip: change claude_hook_server_port in ~/.config/stable/config.toml"
                )
            });
        axum::serve(listener, app).await.unwrap();
    });
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
}

/// Parse `transcript_path` (JSONL) and return info from the last assistant entry.
/// Returns `None` if the file cannot be opened or contains no assistant entries.
pub fn parse_transcript(transcript_path: &str) -> Option<TranscriptInfo> {
    use claude_code_transcripts::types::{AssistantContentBlock, Entry};

    let file = std::fs::File::open(transcript_path).ok()?;
    let reader = std::io::BufReader::new(file);

    let mut last: Option<TranscriptInfo> = None;

    for line in reader.lines().map_while(Result::ok) {
        let line = line.trim().to_owned();
        if line.is_empty() {
            continue;
        }
        let entry: Entry = match serde_json::from_str(&line) {
            Ok(e) => e,
            Err(_) => continue,
        };

        if let Entry::Assistant(a) = entry {
            let usage = &a.message.usage;
            let context_used = usage.input_tokens
                + usage.cache_read_input_tokens.unwrap_or(0)
                + usage.cache_creation_input_tokens.unwrap_or(0);

            // Extract the first Text block as the response preview.
            let last_response_text = a.message.content.iter().find_map(|block| {
                if let AssistantContentBlock::Text { text } = block {
                    Some(text.clone())
                } else {
                    None
                }
            });

            let model_name = a.message.model.clone();

            last = Some(TranscriptInfo {
                context_used,
                last_response_text,
                model_name,
            });
        }
    }

    last
}

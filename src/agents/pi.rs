use std::collections::HashMap;
use std::net::TcpListener as StdTcpListener;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use async_trait::async_trait;
use axum::extract::Extension;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::post;
use axum::{Json, Router};
use serde_json::Value;
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};

use crate::agents::AgentAdapter;
use crate::models::{AgentStatus, ContextInfo};

/// State reported by the Flowmux-owned Pi extension for one interactive Pi process.
pub struct PiHookState {
    status: AgentStatus,
    first_prompt: Option<String>,
    last_model_response: Option<String>,
    model_name: Option<String>,
    session_id: Option<String>,
    context: Option<ContextInfo>,
    total_work_ms: u64,
    work_started_at: Option<Instant>,
    active_generation: u64,
}

impl Default for PiHookState {
    fn default() -> Self {
        Self {
            status: AgentStatus::Idle,
            first_prompt: None,
            last_model_response: None,
            model_name: None,
            session_id: None,
            context: None,
            total_work_ms: 0,
            work_started_at: None,
            active_generation: 0,
        }
    }
}

type HookStateMap = Arc<Mutex<HashMap<String, PiHookState>>>;

#[derive(Debug)]
struct PersistEvent {
    flowmux_agent_id: String,
    session_id: String,
}

#[derive(Clone)]
struct HookServerState {
    hook_state: HookStateMap,
    persist_tx: UnboundedSender<PersistEvent>,
}

/// A Pi adapter reads state pushed by the extension callback server. Pi has no
/// generic waiting-for-input extension event, so V1 only reports running,
/// idle, and stopped lifecycle states.
pub struct PiAdapter {
    flowmux_agent_id: String,
    hook_state: HookStateMap,
}

impl PiAdapter {
    fn new(flowmux_agent_id: String, hook_state: HookStateMap) -> Self {
        Self {
            flowmux_agent_id,
            hook_state,
        }
    }
}

#[async_trait]
impl AgentAdapter for PiAdapter {
    async fn get_status(&self) -> AgentStatus {
        let state = self.hook_state.lock().unwrap();
        state
            .get(&self.flowmux_agent_id)
            .map(|entry| entry.status.clone())
            .unwrap_or(AgentStatus::Unknown)
    }

    async fn get_context(&self) -> Option<ContextInfo> {
        self.hook_state
            .lock()
            .unwrap()
            .get(&self.flowmux_agent_id)
            .and_then(|entry| entry.context.clone())
    }

    async fn get_first_prompt(&self) -> Option<String> {
        self.hook_state
            .lock()
            .unwrap()
            .get(&self.flowmux_agent_id)
            .and_then(|entry| entry.first_prompt.clone())
    }

    async fn get_last_model_response(&self) -> Option<String> {
        self.hook_state
            .lock()
            .unwrap()
            .get(&self.flowmux_agent_id)
            .and_then(|entry| entry.last_model_response.clone())
    }

    async fn get_model_name(&self) -> Option<String> {
        self.hook_state
            .lock()
            .unwrap()
            .get(&self.flowmux_agent_id)
            .and_then(|entry| entry.model_name.clone())
    }

    async fn get_total_work_ms(&self) -> u64 {
        let state = self.hook_state.lock().unwrap();
        let Some(entry) = state.get(&self.flowmux_agent_id) else {
            return 0;
        };
        entry.total_work_ms
            + entry
                .work_started_at
                .map(|started| started.elapsed().as_millis() as u64)
                .unwrap_or(0)
    }

    fn get_cached_session_id(&self) -> Option<String> {
        self.hook_state
            .lock()
            .unwrap()
            .get(&self.flowmux_agent_id)
            .and_then(|entry| entry.session_id.clone())
    }
}

/// Owns the callback server shared by all Pi agents in one Flowmux instance.
pub(crate) struct PiRuntime {
    hook_state: HookStateMap,
    port: u16,
}

impl PiRuntime {
    pub(crate) fn start(base_port: u16, session_name: String) -> Self {
        let hook_state = Arc::new(Mutex::new(HashMap::new()));
        let (persist_tx, mut persist_rx) = unbounded_channel::<PersistEvent>();
        let port = spawn_hook_server(hook_state.clone(), persist_tx, base_port);

        tokio::spawn(async move {
            while let Some(event) = persist_rx.recv().await {
                if let Ok(mut config) = crate::config::Config::load(&session_name) {
                    for agent in &mut config.agents {
                        if let crate::config::AgentKind::Pi {
                            flowmux_agent_id,
                            session_id,
                        } = &mut agent.kind
                            && *flowmux_agent_id == event.flowmux_agent_id
                        {
                            *session_id = Some(event.session_id.clone());
                        }
                    }
                    let _ = config.save();
                }
            }
        });

        Self { hook_state, port }
    }

    pub(crate) fn port(&self) -> u16 {
        self.port
    }

    pub(crate) fn make_adapter(&self, flowmux_agent_id: String) -> PiAdapter {
        self.hook_state
            .lock()
            .unwrap()
            .entry(flowmux_agent_id.clone())
            .or_default();
        PiAdapter::new(flowmux_agent_id, self.hook_state.clone())
    }

    pub(crate) fn restore(&self, flowmux_agent_id: &str, session_id: Option<String>) {
        let mut state = self.hook_state.lock().unwrap();
        let entry = state.entry(flowmux_agent_id.to_owned()).or_default();
        if session_id.is_some() {
            entry.session_id = session_id;
        }
        entry.status = AgentStatus::Idle;
    }

    pub(crate) fn reset_status(&self, flowmux_agent_id: &str) {
        let mut state = self.hook_state.lock().unwrap();
        let entry = state.entry(flowmux_agent_id.to_owned()).or_default();
        entry.status = AgentStatus::Idle;
        entry.work_started_at = None;
    }
}

async fn hook_handler(
    headers: HeaderMap,
    Extension(state): Extension<HookServerState>,
    Json(body): Json<Value>,
) -> StatusCode {
    let Some(agent_id) = headers
        .get("x-flowmux-agent-id")
        .and_then(|value| value.to_str().ok())
    else {
        return StatusCode::BAD_REQUEST;
    };

    let event = body
        .get("event")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let session_id = body
        .get("session_id")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let mut persist_session_id = None;

    {
        let mut states = state.hook_state.lock().unwrap();
        let Some(entry) = states.get_mut(agent_id) else {
            // Another Flowmux instance may receive this loopback callback.
            return StatusCode::OK;
        };

        if let Some(session_id) = session_id
            && entry.session_id.as_deref() != Some(&session_id)
        {
            entry.session_id = Some(session_id.clone());
            persist_session_id = Some(session_id);
        }
        update_metadata(entry, &body);

        match event {
            "session_start" => {
                entry.status = if body.get("is_idle").and_then(Value::as_bool).unwrap_or(true) {
                    AgentStatus::Idle
                } else {
                    AgentStatus::Running
                };
            }
            "input" | "message_update" => entry.status = AgentStatus::Running,
            "agent_start" => {
                let generation = body.get("generation").and_then(Value::as_u64).unwrap_or(0);
                entry.active_generation = generation;
                entry.status = AgentStatus::Running;
                entry.work_started_at = Some(Instant::now());
            }
            "agent_end" => {
                let generation = body.get("generation").and_then(Value::as_u64).unwrap_or(0);
                if generation == entry.active_generation {
                    if let Some(started) = entry.work_started_at.take() {
                        entry.total_work_ms += started.elapsed().as_millis() as u64;
                    }
                    entry.status = AgentStatus::Idle;
                }
            }
            "session_shutdown" => {
                if body.get("reason").and_then(Value::as_str) == Some("quit") {
                    entry.status = AgentStatus::Stopped;
                    if let Some(started) = entry.work_started_at.take() {
                        entry.total_work_ms += started.elapsed().as_millis() as u64;
                    }
                }
            }
            "message_end" | "model_select" => {}
            _ => return StatusCode::OK,
        }
    }

    if let Some(session_id) = persist_session_id {
        let _ = state.persist_tx.send(PersistEvent {
            flowmux_agent_id: agent_id.to_owned(),
            session_id,
        });
    }

    StatusCode::OK
}

fn update_metadata(entry: &mut PiHookState, body: &Value) {
    if entry.first_prompt.is_none() {
        entry.first_prompt = body
            .get("first_prompt")
            .and_then(Value::as_str)
            .filter(|prompt| !prompt.trim().is_empty())
            .map(str::to_owned);
    }
    if let Some(response) = body
        .get("last_model_response")
        .and_then(Value::as_str)
        .filter(|response| !response.is_empty())
    {
        entry.last_model_response = Some(response.to_owned());
    }
    if let Some(model_name) = body
        .get("model_name")
        .and_then(Value::as_str)
        .filter(|model_name| !model_name.is_empty())
    {
        entry.model_name = Some(model_name.to_owned());
    }

    if let Some(used) = body.get("context_used").and_then(Value::as_u64) {
        entry.context = Some(ContextInfo {
            used,
            total: body.get("context_total").and_then(Value::as_u64),
        });
    }
}

fn find_free_port(from: u16) -> u16 {
    let mut port = from;
    loop {
        if StdTcpListener::bind(("127.0.0.1", port)).is_ok() {
            return port;
        }
        port += 1;
    }
}

fn spawn_hook_server(
    hook_state: HookStateMap,
    persist_tx: UnboundedSender<PersistEvent>,
    base_port: u16,
) -> u16 {
    // Kept separate from the caller's requested port so the listener is bound
    // before Pi is launched, avoiding a first-callback race.
    let port = find_free_port(base_port);
    tokio::spawn(async move {
        let app = Router::new()
            .route("/hook", post(hook_handler))
            .layer(Extension(HookServerState {
                hook_state,
                persist_tx,
            }));
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
            .await
            .expect("bind should succeed after finding a free Pi callback port");
        axum::serve(listener, app)
            .await
            .expect("Pi callback server failed");
    });
    port
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_keeps_first_prompt_and_updates_latest_response() {
        let mut state = PiHookState::default();
        update_metadata(
            &mut state,
            &serde_json::json!({
                "first_prompt": "First request",
                "last_model_response": "First response",
                "model_name": "openai/gpt-5",
                "context_used": 42,
                "context_total": 100,
            }),
        );
        update_metadata(
            &mut state,
            &serde_json::json!({
                "first_prompt": "Later request",
                "last_model_response": "Latest response",
            }),
        );

        assert_eq!(state.first_prompt.as_deref(), Some("First request"));
        assert_eq!(
            state.last_model_response.as_deref(),
            Some("Latest response")
        );
        assert_eq!(state.model_name.as_deref(), Some("openai/gpt-5"));
        assert_eq!(
            state
                .context
                .as_ref()
                .map(|context| (context.used, context.total)),
            Some((42, Some(100)))
        );
    }
}

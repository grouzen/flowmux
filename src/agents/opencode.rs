use anyhow::anyhow;
use async_trait::async_trait;
use futures::StreamExt;
use reqwest::Client;
use serde_json::Value;
use std::collections::HashMap;
use std::net::TcpListener;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};
use tokio::time::sleep;

use crate::agents::AgentAdapter;
use crate::models::{AgentStatus, ContextInfo};
use crate::tmux;

// ---------------------------------------------------------------------------
// LiveCache — shared state updated reactively by the SSE task
// ---------------------------------------------------------------------------

struct LiveCache {
    /// Current agent status derived from `session.status` events.
    status: AgentStatus,
    /// Newest ~5 messages, refreshed on `message.updated` / `message.part.updated`.
    recent_messages: Option<Vec<Value>>,
    /// The very first user prompt in the session. Set once, never cleared.
    first_prompt: Option<String>,
    /// Permanent map of (provider_id, model_id) → context window size.
    /// Populated lazily; never evicted.
    provider_limits: HashMap<(String, String), u64>,
    /// Model ID from the most recent assistant message (e.g. "claude-sonnet-4-5").
    model_id: Option<String>,
    /// Sum of (time.completed - time.created) for every assistant message, in ms.
    total_work_ms: u64,
    /// The highest `time.created` among assistant messages already counted in
    /// `total_work_ms`. Used to avoid double-counting on incremental tail updates.
    last_counted_assistant_created: u64,
    /// True once a full-session scan has been done to initialise `total_work_ms`.
    work_time_initialized: bool,
}

impl LiveCache {
    fn new() -> Self {
        Self {
            status: AgentStatus::Unknown,
            recent_messages: None,
            first_prompt: None,
            provider_limits: HashMap::new(),
            model_id: None,
            total_work_ms: 0,
            last_counted_assistant_created: 0,
            work_time_initialized: false,
        }
    }
}

// ---------------------------------------------------------------------------
// How many recent messages the SSE task fetches on each message event.
// Agentic responses can span many assistant messages (one per tool-calling
// step).  We need the window to comfortably hold a full response turn plus
// the preceding user message.  50 is a safe upper bound.
// ---------------------------------------------------------------------------
const RECENT_LIMIT: usize = 50;

// Minimum gap between successive tail-fetches triggered by streaming part
// events.  Part deltas fire many times per second; without this guard each
// delta would cause an HTTP round-trip.
const PART_DEBOUNCE: Duration = Duration::from_millis(200);

// ---------------------------------------------------------------------------
// Internal launch helper
// ---------------------------------------------------------------------------

/// Creates a new tmux window, sends the `opencode` command into it, and
/// waits for the health endpoint to respond.  Returns `(window_index, pane)`.
///
/// If `session_id` is `Some` the command includes `--session <id>` so
/// opencode resumes an existing session rather than starting a fresh one.
async fn launch(
    dir: &str,
    name: &str,
    port: u16,
    session_id: Option<&str>,
) -> anyhow::Result<(usize, String)> {
    let window_index = tmux::new_window(dir, name)?;
    let pane = format!("{}:{}.0", tmux::session_name(), window_index);

    let cmd = match session_id {
        Some(sid) => format!("opencode --port {} --session {}\n", port, sid),
        None => format!("opencode --port {}\n", port),
    };
    tmux::send_keys(&pane, &cmd)?;

    let client = Client::new();
    let health_url = format!("http://127.0.0.1:{}/global/health", port);

    let mut healthy = false;
    for _ in 0..25 {
        sleep(tokio::time::Duration::from_millis(200)).await;
        if let Ok(resp) = client.get(&health_url).send().await {
            if let Ok(body) = resp.json::<Value>().await {
                if body.get("healthy").and_then(Value::as_bool).unwrap_or(false) {
                    healthy = true;
                    break;
                }
            }
        }
    }

    if !healthy {
        return Err(anyhow!("opencode did not become healthy within timeout"));
    }

    Ok((window_index, pane))
}

// ---------------------------------------------------------------------------
// OpenCodeAdapter
// ---------------------------------------------------------------------------

pub struct OpenCodeAdapter {
    pub port: u16,
    pub client: Client,
    /// Long-lived session ID shared with the SSE task — persists so history
    /// is visible while idle.  Both the adapter and the SSE task hold a clone
    /// of this Arc, so updates made by the task are immediately visible here.
    cached_session_id: Arc<Mutex<Option<String>>>,
    /// Reactive in-memory state kept up to date by the SSE background task.
    live_cache: Arc<RwLock<LiveCache>>,
    /// Holds the SSE task; dropped (and therefore aborted) when the adapter
    /// is dropped.
    _sse_task: tokio::task::JoinHandle<()>,
}

impl OpenCodeAdapter {
    pub fn new(port: u16, session_id: Option<String>) -> Self {
        let client = Client::new();
        let live_cache = Arc::new(RwLock::new(LiveCache::new()));
        let cached_session_id = Arc::new(Mutex::new(session_id));

        let task = tokio::spawn(run_sse_loop(
            port,
            client.clone(),
            live_cache.clone(),
            cached_session_id.clone(),
        ));

        Self {
            port,
            client,
            cached_session_id,
            live_cache,
            _sse_task: task,
        }
    }

    fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    /// Creates a new opencode agent: allocates port, opens tmux window, launches
    /// opencode, waits for health.  Returns (adapter, window_index).
    pub async fn create(dir: &str, name: &str) -> anyhow::Result<(OpenCodeAdapter, usize)> {
        let port = find_free_port(14100);
        let (window_index, _) = launch(dir, name, port, None).await?;
        let adapter = OpenCodeAdapter::new(port, None);
        Ok((adapter, window_index))
    }

    /// Restarts a stopped agent: scans for a free port, opens a new tmux window,
    /// launches opencode resuming the previous session (via `--session <id>`),
    /// and waits for health.  Returns (adapter, window_index, new_port).
    ///
    /// The caller is responsible for updating `AgentConfig.pane` and
    /// `AgentConfig.port` with the returned values and saving the config.
    pub async fn restart(
        dir: &str,
        name: &str,
        session_id: Option<&str>,
    ) -> anyhow::Result<(OpenCodeAdapter, usize, u16)> {
        let port = find_free_port(14100);
        let (window_index, _) = launch(dir, name, port, session_id).await?;
        // Preserve the existing session_id so continuity is maintained.
        let adapter = OpenCodeAdapter::new(port, session_id.map(str::to_string));
        Ok((adapter, window_index, port))
    }
}

// ---------------------------------------------------------------------------
// SSE background task
// ---------------------------------------------------------------------------

/// Long-running task that maintains `live_cache` by subscribing to the
/// opencode SSE event stream.  Reconnects with exponential backoff on any
/// error.  Sets `Stopped` in the cache when the server is unreachable so
/// the UI can show the "agent stopped" overlay.
async fn run_sse_loop(
    port: u16,
    client: Client,
    live_cache: Arc<RwLock<LiveCache>>,
    cached_session_id: Arc<Mutex<Option<String>>>,
) {
    let mut backoff_secs: u64 = 1;
    // Tracks when we last did a tail-fetch so part-delta events can be
    // debounced.
    let mut last_tail_fetch = Instant::now() - PART_DEBOUNCE * 2;

    loop {
        // --- initial population -------------------------------------------
        let reachable = populate_initial(port, &client, &live_cache, &cached_session_id).await;

        if !reachable {
            // Server is down — mark Stopped so the UI shows the overlay,
            // then wait before retrying.
            live_cache.write().unwrap().status = AgentStatus::Stopped;
            sleep(Duration::from_secs(backoff_secs)).await;
            backoff_secs = (backoff_secs * 2).min(30);
            continue;
        }

        // --- connect to event stream ---------------------------------------
        let url = format!("http://127.0.0.1:{}/event", port);
        let resp = match client.get(&url).send().await {
            Ok(r) => r,
            Err(_) => {
                // Can't reach the event endpoint — server likely just stopped.
                live_cache.write().unwrap().status = AgentStatus::Stopped;
                sleep(Duration::from_secs(backoff_secs)).await;
                backoff_secs = (backoff_secs * 2).min(30);
                continue;
            }
        };

        backoff_secs = 1; // connected successfully — reset backoff

        let mut stream = resp.bytes_stream();
        let mut line_buf = String::new();

        loop {
            match stream.next().await {
                None => break, // stream ended — reconnect
                Some(Err(_)) => break,
                Some(Ok(chunk)) => {
                    // SSE uses UTF-8; ignore non-UTF-8 chunks
                    let text = match std::str::from_utf8(&chunk) {
                        Ok(s) => s,
                        Err(_) => continue,
                    };
                    line_buf.push_str(text);

                    // Process all complete lines in the buffer
                    while let Some(nl) = line_buf.find('\n') {
                        let line = line_buf[..nl].trim_end_matches('\r').to_string();
                        line_buf.drain(..=nl);

                        if let Some(json_str) = line.strip_prefix("data: ") {
                            if let Ok(envelope) = serde_json::from_str::<Value>(json_str) {
                                handle_event(
                                    port,
                                    &client,
                                    &live_cache,
                                    &cached_session_id,
                                    &envelope,
                                    &mut last_tail_fetch,
                                )
                                .await;
                            }
                        }
                    }
                }
            }
        }

        // Stream disconnected — wait then reconnect
        sleep(Duration::from_secs(backoff_secs)).await;
        backoff_secs = (backoff_secs * 2).min(30);
    }
}

/// Seed `live_cache` with current state before entering the SSE loop (or
/// after a reconnect).  Returns `true` if the server responded, `false` if
/// it was unreachable (e.g. opencode has exited).
async fn populate_initial(
    port: u16,
    client: &Client,
    live_cache: &Arc<RwLock<LiveCache>>,
    cached_session_id: &Arc<Mutex<Option<String>>>,
) -> bool {
    let base = format!("http://127.0.0.1:{}", port);

    // --- session status ---------------------------------------------------
    let status_url = format!("{}/session/status", base);
    match client.get(&status_url).send().await {
        Err(_) => return false, // connection refused — server is down
        Ok(resp) => {
            if let Ok(body) = resp.json::<Value>().await {
                if let Some(obj) = body.as_object() {
                    if !obj.is_empty() {
                        let mut best = AgentStatus::Idle;
                        for entry in obj.values() {
                            match entry.get("status").and_then(Value::as_str).unwrap_or("") {
                                "busy" | "retry" => {
                                    best = AgentStatus::Running;
                                    break;
                                }
                                _ => {}
                            }
                        }
                        let id = obj
                            .keys()
                            .max_by_key(|id| {
                                obj[*id]
                                    .get("time")
                                    .and_then(|t| t.get("updated"))
                                    .and_then(Value::as_u64)
                                    .unwrap_or(0)
                            })
                            .map(|id| id.to_string());
                        if let Some(ref sid) = id {
                            *cached_session_id.lock().unwrap() = Some(sid.clone());
                        }
                        live_cache.write().unwrap().status = best;
                    } else {
                        live_cache.write().unwrap().status = AgentStatus::Idle;
                    }
                }
            }
        }
    }

    // --- recent messages -------------------------------------------------
    let sid = cached_session_id.lock().unwrap().clone();
    if let Some(sid) = sid {
        fetch_and_store_tail(port, client, live_cache, &sid, true).await;
    }
    true
}

/// Dispatch a single parsed SSE event envelope.
async fn handle_event(
    port: u16,
    client: &Client,
    live_cache: &Arc<RwLock<LiveCache>>,
    cached_session_id: &Arc<Mutex<Option<String>>>,
    envelope: &Value,
    last_tail_fetch: &mut Instant,
) {
    let event_type = match envelope.get("type").and_then(Value::as_str) {
        Some(t) => t,
        None => return,
    };
    let props = envelope.get("properties").unwrap_or(&Value::Null);

    match event_type {
        // -----------------------------------------------------------------
        "session.status" => {
            let sid = props
                .get("sessionID")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();

            let status_type = props
                .get("status")
                .and_then(|s| s.get("type"))
                .and_then(Value::as_str)
                .unwrap_or("idle");

            let new_status = match status_type {
                "busy" | "retry" => AgentStatus::Running,
                _ => AgentStatus::Idle,
            };

            if !sid.is_empty() {
                *cached_session_id.lock().unwrap() = Some(sid.clone());
            }
            live_cache.write().unwrap().status = new_status;
        }

        // -----------------------------------------------------------------
        "message.updated" => {
            let sid = props
                .get("sessionID")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            if sid.is_empty() {
                return;
            }
            *cached_session_id.lock().unwrap() = Some(sid.clone());
            fetch_and_store_tail(port, client, live_cache, &sid, true).await;
            *last_tail_fetch = Instant::now();
        }

        // -----------------------------------------------------------------
        // Part events fire many times per second during streaming.
        // Debounce to avoid an HTTP call on every token.
        "message.part.updated" => {
            if last_tail_fetch.elapsed() < PART_DEBOUNCE {
                return;
            }
            let sid = props
                .get("sessionID")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            if sid.is_empty() {
                return;
            }
            *cached_session_id.lock().unwrap() = Some(sid.clone());
            fetch_and_store_tail(port, client, live_cache, &sid, false).await;
            *last_tail_fetch = Instant::now();
        }

        // Sub-token streaming deltas — no useful state to cache.
        "message.part.delta" => {}

        // Reconnect confirmation — redo initial population.
        "server.connected" => {
            populate_initial(port, client, live_cache, cached_session_id).await;
        }

        _ => {}
    }
}

/// Fetch the newest `RECENT_LIMIT` messages for `session_id` and write them
/// into `live_cache.recent_messages`.
///
/// If `try_first_prompt` is true and `live_cache.first_prompt` is still `None`,
/// also attempts to populate it.  For short sessions the tail already contains
/// the first user message; for long sessions a one-time full fetch is done.
async fn fetch_and_store_tail(
    port: u16,
    client: &Client,
    live_cache: &Arc<RwLock<LiveCache>>,
    session_id: &str,
    try_first_prompt: bool,
) {
    let base = format!("http://127.0.0.1:{}", port);
    let url = format!(
        "{}/session/{}/message?limit={}",
        base, session_id, RECENT_LIMIT
    );
    let Ok(resp) = client.get(&url).send().await else {
        return;
    };
    let Ok(msgs) = resp.json::<Vec<Value>>().await else {
        return;
    };

    let need_first_prompt =
        try_first_prompt && live_cache.read().unwrap().first_prompt.is_none();
    let need_work_init = !live_cache.read().unwrap().work_time_initialized;
    // If the session is long (tail is full), do a one-time full fetch for both
    // first_prompt and total_work_ms initialisation.
    let is_long_session = msgs.len() >= RECENT_LIMIT;

    let first_prompt_value: Option<String> = if need_first_prompt {
        if is_long_session {
            // Session is longer than RECENT_LIMIT — the first user message may
            // not be in the tail at all, so always do a one-time full fetch.
            fetch_first_prompt(port, client, session_id).await
        } else {
            // Session is short — the tail already starts from message 0.
            msgs.iter()
                .find(|m| msg_role(m) == Some("user"))
                .and_then(|m| all_text_parts(m))
        }
    } else {
        None
    };

    // Compute total work time. For long sessions initialise from a full fetch;
    // for short sessions (or after init) use the tail incrementally.
    let full_msgs_for_work: Option<Vec<Value>> = if need_work_init && is_long_session {
        let url = format!("http://127.0.0.1:{}/session/{}/message", port, session_id);
        match client.get(&url).send().await {
            Ok(r) => r.json::<Vec<Value>>().await.ok(),
            Err(_) => None,
        }
    } else {
        None
    };

    let mut cache = live_cache.write().unwrap();

    if need_work_init {
        // Seed total_work_ms from either the full fetch or the short tail.
        let source = full_msgs_for_work.as_deref().unwrap_or(&msgs);
        let (sum, max_created) = assistant_work_sum(source);
        cache.total_work_ms = sum;
        cache.last_counted_assistant_created = max_created;
        cache.work_time_initialized = true;
    } else {
        // Incremental update: only count assistant messages newer than what we've seen.
        let threshold = cache.last_counted_assistant_created;
        let (delta, max_created) = assistant_work_sum_after(&msgs, threshold);
        cache.total_work_ms += delta;
        if max_created > cache.last_counted_assistant_created {
            cache.last_counted_assistant_created = max_created;
        }
    }

    cache.recent_messages = Some(msgs);
    if let Some(fp) = first_prompt_value {
        cache.first_prompt = Some(fp);
    }
}

/// One-time full fetch to extract the very first user message text.
async fn fetch_first_prompt(port: u16, client: &Client, session_id: &str) -> Option<String> {
    let url = format!(
        "http://127.0.0.1:{}/session/{}/message",
        port, session_id
    );
    let resp = client.get(&url).send().await.ok()?;
    let msgs: Vec<Value> = resp.json().await.ok()?;
    msgs.into_iter()
        .find(|m| msg_role(m) == Some("user"))
        .and_then(|m| all_text_parts(&m))
}

// ---------------------------------------------------------------------------
// Message field helpers
// ---------------------------------------------------------------------------

/// Returns `info.time.completed` for a message (0 if absent).
fn msg_time_completed(msg: &Value) -> u64 {
    msg.get("info")
        .and_then(|i| i.get("time"))
        .and_then(|t| t.get("completed"))
        .and_then(Value::as_u64)
        .unwrap_or(0)
}

/// Sums `(time.completed - time.created)` for all assistant messages.
/// Returns `(total_ms, max_time_created)`.
fn assistant_work_sum(msgs: &[Value]) -> (u64, u64) {
    let mut total: u64 = 0;
    let mut max_created: u64 = 0;
    for m in msgs {
        if msg_role(m) != Some("assistant") {
            continue;
        }
        let created = msg_time_created(m);
        let completed = msg_time_completed(m);
        if completed > created {
            total += completed - created;
        }
        if created > max_created {
            max_created = created;
        }
    }
    (total, max_created)
}

/// Like `assistant_work_sum` but only considers messages with
/// `time.created > threshold` (for incremental updates).
fn assistant_work_sum_after(msgs: &[Value], threshold: u64) -> (u64, u64) {
    let mut total: u64 = 0;
    let mut max_created: u64 = 0;
    for m in msgs {
        if msg_role(m) != Some("assistant") {
            continue;
        }
        let created = msg_time_created(m);
        if created <= threshold {
            continue;
        }
        let completed = msg_time_completed(m);
        if completed > created {
            total += completed - created;
        }
        if created > max_created {
            max_created = created;
        }
    }
    (total, max_created)
}

fn all_text_parts(msg: &Value) -> Option<String> {
    let parts = msg.get("parts")?.as_array()?;
    let texts: Vec<&str> = parts
        .iter()
        .filter_map(|p: &Value| {
            if p.get("type").and_then(Value::as_str) == Some("text") {
                p.get("text").and_then(Value::as_str)
            } else {
                None
            }
        })
        .collect();
    if texts.is_empty() {
        None
    } else {
        Some(texts.join("\n"))
    }
}

fn msg_role(msg: &Value) -> Option<&str> {
    msg.get("info")?.get("role")?.as_str()
}

fn msg_time_created(msg: &Value) -> u64 {
    msg.get("info")
        .and_then(|i| i.get("time"))
        .and_then(|t| t.get("created"))
        .and_then(Value::as_u64)
        .unwrap_or(0)
}

fn msg_tokens(msg: &Value) -> Option<&Value> {
    msg.get("info")?.get("tokens")
}

// ---------------------------------------------------------------------------
// Misc
// ---------------------------------------------------------------------------

pub fn find_free_port(from: u16) -> u16 {
    let mut port = from;
    loop {
        if TcpListener::bind(format!("127.0.0.1:{}", port)).is_ok() {
            return port;
        }
        port += 1;
    }
}

// ---------------------------------------------------------------------------
// Provider context limit — resolved lazily and cached permanently
// ---------------------------------------------------------------------------

impl OpenCodeAdapter {
    /// Returns the context-window size for `(provider_id, model_id)`.
    ///
    /// On the first call for a given pair this issues `GET /provider` and
    /// stores the result in `live_cache.provider_limits`.  All subsequent
    /// calls are pure HashMap lookups — no HTTP.
    async fn resolve_context_total(&self, provider_id: &str, model_id: &str) -> Option<u64> {
        if provider_id.is_empty() || model_id.is_empty() {
            return None;
        }

        let key = (provider_id.to_string(), model_id.to_string());

        // Fast path — already cached
        if let Some(&v) = self.live_cache.read().unwrap().provider_limits.get(&key) {
            return Some(v);
        }

        // Slow path — fetch once
        let url = format!("{}/provider", self.base_url());
        let body: Value = self.client.get(&url).send().await.ok()?.json().await.ok()?;

        let providers = body.get("all").and_then(Value::as_array)?;
        let provider = providers
            .iter()
            .find(|p: &&Value| p.get("id").and_then(Value::as_str) == Some(provider_id))?;

        let limit = provider
            .get("models")
            .and_then(|m| m.get(model_id))
            .and_then(|m| m.get("limit"))
            .and_then(|l| l.get("context"))
            .and_then(Value::as_u64)?;

        self.live_cache
            .write()
            .unwrap()
            .provider_limits
            .insert(key, limit);

        Some(limit)
    }
}

// ---------------------------------------------------------------------------
// AgentAdapter implementation — all methods are pure in-memory reads
// ---------------------------------------------------------------------------

#[async_trait]
impl AgentAdapter for OpenCodeAdapter {
    async fn get_status(&self) -> AgentStatus {
        self.live_cache.read().unwrap().status.clone()
    }

    async fn get_context(&self) -> Option<ContextInfo> {
        let messages: Vec<Value> = self
            .live_cache
            .read()
            .unwrap()
            .recent_messages
            .clone()?;

        // Find the latest assistant message with non-zero token usage.
        // In-flight messages exist but have zeroed token counts while streaming.
        let latest_assistant = messages
            .iter()
            .filter(|m: &&Value| msg_role(m) == Some("assistant"))
            .filter(|m: &&Value| {
                msg_tokens(m)
                    .map(|t| {
                        let input = t.get("input").and_then(Value::as_u64).unwrap_or(0);
                        let output = t.get("output").and_then(Value::as_u64).unwrap_or(0);
                        input > 0 || output > 0
                    })
                    .unwrap_or(false)
            })
            .max_by_key(|m: &&Value| msg_time_created(m))?
            .clone();

        let tokens = msg_tokens(&latest_assistant)?.clone();
        let used = tokens.get("total").and_then(Value::as_u64).unwrap_or_else(|| {
            let input = tokens.get("input").and_then(Value::as_u64).unwrap_or(0);
            let output = tokens.get("output").and_then(Value::as_u64).unwrap_or(0);
            let cache_read = tokens
                .get("cache")
                .and_then(|c: &Value| c.get("read"))
                .and_then(Value::as_u64)
                .unwrap_or(0);
            let cache_write = tokens
                .get("cache")
                .and_then(|c: &Value| c.get("write"))
                .and_then(Value::as_u64)
                .unwrap_or(0);
            input + output + cache_read + cache_write
        });

        let provider_id = latest_assistant
            .get("info")
            .and_then(|i| i.get("providerID"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let model_id = latest_assistant
            .get("info")
            .and_then(|i| i.get("modelID"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();

        let total = self.resolve_context_total(&provider_id, &model_id).await;

        // Persist the model ID so the card can display it.
        if !model_id.is_empty() {
            self.live_cache.write().unwrap().model_id = Some(model_id);
        }

        Some(ContextInfo { used, total })
    }

    async fn get_first_prompt(&self) -> Option<String> {
        self.live_cache.read().unwrap().first_prompt.clone()
    }

    async fn get_last_model_response(&self) -> Option<String> {
        let mut messages = self.live_cache.read().unwrap().recent_messages.clone()?;
        // Sort oldest-first by creation timestamp so positional ordering is reliable.
        messages.sort_by_key(|m| msg_time_created(m));

        // Collect the start index of each response turn (the index just after
        // each user message).
        let turn_starts: Vec<usize> = messages
            .iter()
            .enumerate()
            .filter(|(_, m)| msg_role(m) == Some("user"))
            .map(|(i, _)| i + 1)
            .collect();

        // Walk turns newest-to-oldest; return the first one that contains text.
        // If the agent is mid-run and the latest turn has no text yet, this
        // naturally falls back to the previous completed response.
        for &start in turn_starts.iter().rev() {
            let parts: Vec<String> = messages[start..]
                .iter()
                .filter(|m| msg_role(m) == Some("assistant"))
                .filter_map(|m| all_text_parts(m))
                .collect();
            if !parts.is_empty() {
                return Some(parts.join("\n\n"));
            }
        }

        // Fallback: assistant text before the first user message.
        // If there are no user messages in the tail at all, search all tail
        // messages (unwrap_or(0) would produce an empty slice otherwise).
        let first_turn_start = turn_starts.first().copied().unwrap_or(messages.len());
        let parts: Vec<String> = messages[..first_turn_start]
            .iter()
            .filter(|m| msg_role(m) == Some("assistant"))
            .filter_map(|m| all_text_parts(m))
            .collect();
        if !parts.is_empty() { Some(parts.join("\n\n")) } else { None }
    }

    fn get_cached_session_id(&self) -> Option<String> {
        self.cached_session_id.lock().unwrap().clone()
    }

    async fn get_model_name(&self) -> Option<String> {
        self.live_cache.read().unwrap().model_id.clone()
    }

    async fn get_total_work_ms(&self) -> u64 {
        self.live_cache.read().unwrap().total_work_ms
    }
}

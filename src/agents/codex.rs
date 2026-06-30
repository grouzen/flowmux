use std::collections::HashSet;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::time::{interval, sleep};

use crate::agents::AgentAdapter;
use crate::launch;
use crate::models::{AgentStatus, ContextInfo};
use crate::tmux;

const DISCOVERY_INTERVAL: Duration = Duration::from_millis(750);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RequestKind {
    Discover,
    Resume,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PendingRequest {
    id: u64,
    kind: RequestKind,
    sent_at: Instant,
}

struct LiveCache {
    status: AgentStatus,
    first_prompt: Option<String>,
    last_model_response: Option<String>,
    model_name: Option<String>,
    context: Option<ContextInfo>,
    total_work_ms: u64,
    completed_turn_ids: HashSet<String>,
    rollout_path: Option<String>,
    rollout_offset: u64,
}

impl Default for LiveCache {
    fn default() -> Self {
        Self {
            status: AgentStatus::Idle,
            first_prompt: None,
            last_model_response: None,
            model_name: None,
            context: None,
            total_work_ms: 0,
            completed_turn_ids: HashSet::new(),
            rollout_path: None,
            rollout_offset: 0,
        }
    }
}

pub struct CodexAdapter {
    pub port: u16,
    cached_session_id: Arc<Mutex<Option<String>>>,
    live_cache: Arc<RwLock<LiveCache>>,
    _task: tokio::task::JoinHandle<()>,
}

impl Drop for CodexAdapter {
    fn drop(&mut self) {
        self._task.abort();
    }
}

impl CodexAdapter {
    pub fn new(port: u16, directory: String, session_id: Option<String>) -> Self {
        Self::with_min_created_at(port, directory, session_id, 0)
    }

    fn with_min_created_at(
        port: u16,
        directory: String,
        session_id: Option<String>,
        min_created_at: i64,
    ) -> Self {
        let cached_session_id = Arc::new(Mutex::new(session_id));
        let live_cache = Arc::new(RwLock::new(LiveCache::default()));
        let task = tokio::spawn(run_loop(
            port,
            directory,
            min_created_at,
            cached_session_id.clone(),
            live_cache.clone(),
        ));

        Self {
            port,
            cached_session_id,
            live_cache,
            _task: task,
        }
    }

    pub async fn create(dir: &str, name: &str) -> Result<(Self, usize)> {
        let port = find_free_port(16100);
        let created_at = unix_timestamp();
        let window_index = launch(dir, name, port, None).await?;
        let adapter = Self::with_min_created_at(port, dir.to_owned(), None, created_at);
        Ok((adapter, window_index))
    }

    pub async fn restart(
        dir: &str,
        name: &str,
        session_id: Option<&str>,
    ) -> Result<(Self, usize, u16)> {
        let port = find_free_port(16100);
        let window_index = launch(dir, name, port, session_id).await?;
        let adapter =
            Self::with_min_created_at(port, dir.to_owned(), session_id.map(str::to_owned), 0);
        Ok((adapter, window_index, port))
    }
}

#[async_trait]
impl AgentAdapter for CodexAdapter {
    async fn stop(&self) -> Result<()> {
        self._task.abort();
        stop_server(self.port).await
    }

    async fn get_status(&self) -> AgentStatus {
        self.live_cache.read().unwrap().status.clone()
    }

    async fn get_context(&self) -> Option<ContextInfo> {
        self.live_cache.read().unwrap().context.clone()
    }

    async fn get_first_prompt(&self) -> Option<String> {
        self.live_cache.read().unwrap().first_prompt.clone()
    }

    async fn get_last_model_response(&self) -> Option<String> {
        self.live_cache.read().unwrap().last_model_response.clone()
    }

    async fn get_model_name(&self) -> Option<String> {
        self.live_cache.read().unwrap().model_name.clone()
    }

    async fn get_total_work_ms(&self) -> u64 {
        self.live_cache.read().unwrap().total_work_ms
    }

    fn get_cached_session_id(&self) -> Option<String> {
        self.cached_session_id.lock().unwrap().clone()
    }
}

async fn launch(dir: &str, name: &str, port: u16, session_id: Option<&str>) -> Result<usize> {
    let window_index = tmux::new_window(dir, name)?;
    let pane = format!("{}:{}.0", tmux::session_name(), window_index);
    let mut args = vec![
        std::ffi::OsString::from("--port"),
        std::ffi::OsString::from(port.to_string()),
    ];
    if let Some(session_id) = session_id {
        args.push(std::ffi::OsString::from("--session-id"));
        args.push(std::ffi::OsString::from(session_id));
    }
    tmux::send_literal(&pane, &launch::flowmux_launch_command("codex", &args))?;

    let client = reqwest::Client::builder()
        .timeout(Duration::from_millis(500))
        .build()?;
    for _ in 0..25 {
        if app_server_ready(&client, port).await {
            return Ok(window_index);
        }
        sleep(Duration::from_millis(200)).await;
    }

    Err(anyhow!(
        "codex app-server did not become available in pane {pane}"
    ))
}

async fn app_server_ready(client: &reqwest::Client, port: u16) -> bool {
    let url = format!("http://127.0.0.1:{port}/readyz");
    client
        .get(url)
        .send()
        .await
        .is_ok_and(|response| response.status().is_success())
}

fn server_pid_path(port: u16) -> String {
    format!("/tmp/flowmux-codex-{port}.pid")
}

async fn stop_server(port: u16) -> Result<()> {
    let pid_path = server_pid_path(port);
    let pid = std::fs::read_to_string(&pid_path)
        .ok()
        .and_then(|value| value.trim().parse::<i32>().ok());

    let signaled_pid = pid.is_some_and(|pid| signal_server_pid(pid, port, libc::SIGTERM));
    if !signaled_pid {
        signal_port_owner(port, "TERM");
    }

    for _ in 0..10 {
        if TcpStream::connect(("127.0.0.1", port)).await.is_err() {
            let _ = std::fs::remove_file(&pid_path);
            return Ok(());
        }
        sleep(Duration::from_millis(100)).await;
    }

    let killed_pid = pid.is_some_and(|pid| signal_server_pid(pid, port, libc::SIGKILL));
    if !killed_pid {
        signal_port_owner(port, "KILL");
    }
    let _ = std::fs::remove_file(pid_path);
    Ok(())
}

fn signal_server_pid(pid: i32, port: u16, signal: i32) -> bool {
    let cmdline = std::fs::read(format!("/proc/{pid}/cmdline")).unwrap_or_default();
    let matches = server_cmdline_matches_port(&cmdline, port);
    if matches {
        unsafe {
            libc::kill(pid, signal);
        }
    }
    matches
}

fn server_cmdline_matches_port(cmdline: &[u8], port: u16) -> bool {
    let expected = format!("ws://127.0.0.1:{port}");
    cmdline
        .split(|byte| *byte == 0)
        .any(|arg| arg == expected.as_bytes())
}

fn signal_port_owner(port: u16, signal: &str) {
    let _ = std::process::Command::new("fuser")
        .args(["-k", &format!("-{signal}"), &format!("{port}/tcp")])
        .status();
}

async fn run_loop(
    port: u16,
    directory: String,
    min_created_at: i64,
    cached_session_id: Arc<Mutex<Option<String>>>,
    live_cache: Arc<RwLock<LiveCache>>,
) {
    let mut backoff = 1u64;

    loop {
        let (mut reader, mut writer) = match connect_websocket(port).await {
            Ok(connection) => connection,
            Err(_) => {
                mark_observer_unavailable(&cached_session_id, &live_cache);
                sleep(Duration::from_secs(backoff)).await;
                backoff = (backoff * 2).min(30);
                continue;
            }
        };
        if initialize(&mut reader, &mut writer).await.is_err() {
            mark_observer_unavailable(&cached_session_id, &live_cache);
            sleep(Duration::from_secs(backoff)).await;
            backoff = (backoff * 2).min(30);
            continue;
        }
        backoff = 1;

        let mut ticker = interval(DISCOVERY_INTERVAL);
        let mut request_id = 10u64;
        let mut pending = None;
        let mut subscribed_thread_id: Option<String> = None;

        if let Some(thread_id) = cached_thread_id(&cached_session_id)
            && send_request(
                &mut writer,
                &mut request_id,
                &mut pending,
                RequestKind::Resume,
                Some(&thread_id),
                &directory,
            )
            .await
            .is_err()
        {
            mark_observer_unavailable(&cached_session_id, &live_cache);
            sleep(Duration::from_secs(backoff)).await;
            backoff = (backoff * 2).min(30);
            continue;
        }

        loop {
            tokio::select! {
                _ = ticker.tick() => {
                    if pending.is_some_and(|request| request.sent_at.elapsed() >= REQUEST_TIMEOUT) {
                        break;
                    }
                    let thread_id = cached_thread_id(&cached_session_id);
                    if pending.is_some()
                        || subscription_matches_thread(
                            subscribed_thread_id.as_deref(),
                            thread_id.as_deref(),
                        )
                    {
                        continue;
                    }
                    let kind = if thread_id.is_some() {
                        RequestKind::Resume
                    } else {
                        RequestKind::Discover
                    };
                    if send_request(
                        &mut writer,
                        &mut request_id,
                        &mut pending,
                        kind,
                        thread_id.as_deref(),
                        &directory,
                    )
                    .await
                    .is_err() {
                        break;
                    }
                }
                message = read_text(&mut reader) => {
                    let Ok(Some(text)) = message else { break };
                    let Ok(value) = serde_json::from_str::<Value>(&text) else { continue };
                    let previous_thread_id = cached_thread_id(&cached_session_id);

                    if value.get("method").is_some() {
                        handle_message(
                            &value,
                            &directory,
                            min_created_at,
                            &cached_session_id,
                            &live_cache,
                        );
                    } else if let Some(response_id) = value.get("id").and_then(Value::as_u64)
                        && pending.is_some_and(|request| request.id == response_id)
                    {
                        let request = pending.take().unwrap();
                        if value.get("error").is_some() {
                            match request.kind {
                                RequestKind::Discover => {}
                                RequestKind::Resume if is_unmaterialized_thread_error(&value) => {
                                    live_cache.write().unwrap().status = AgentStatus::Idle;
                                    continue;
                                }
                                _ => break,
                            }
                        } else {
                            match request.kind {
                                RequestKind::Discover => {
                                    handle_message(
                                        &value,
                                        &directory,
                                        min_created_at,
                                        &cached_session_id,
                                        &live_cache,
                                    );
                                }
                                RequestKind::Resume => {
                                    if !handle_thread_response(
                                        &value,
                                        &cached_session_id,
                                        &live_cache,
                                    ) {
                                        break;
                                    }
                                    subscribed_thread_id = cached_thread_id(&cached_session_id);
                                }
                            }
                        }
                    }

                    let current_thread_id = cached_thread_id(&cached_session_id);
                    if current_thread_id != previous_thread_id {
                        subscribed_thread_id = None;
                    }

                    if pending.is_none()
                        && !subscription_matches_thread(
                            subscribed_thread_id.as_deref(),
                            current_thread_id.as_deref(),
                        )
                        && let Some(thread_id) = current_thread_id
                        && send_request(
                            &mut writer,
                            &mut request_id,
                            &mut pending,
                            RequestKind::Resume,
                            Some(&thread_id),
                            &directory,
                        )
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
            }
        }

        mark_observer_unavailable(&cached_session_id, &live_cache);
        sleep(Duration::from_secs(backoff)).await;
        backoff = (backoff * 2).min(30);
    }
}

async fn send_request(
    writer: &mut OwnedWriteHalf,
    request_id: &mut u64,
    pending: &mut Option<PendingRequest>,
    kind: RequestKind,
    thread_id: Option<&str>,
    directory: &str,
) -> Result<()> {
    let id = *request_id;
    let request = request_for(id, kind, thread_id, directory);
    send_text(writer, &request.to_string()).await?;
    *request_id += 1;
    *pending = Some(PendingRequest {
        id,
        kind,
        sent_at: Instant::now(),
    });
    Ok(())
}

fn request_for(id: u64, kind: RequestKind, thread_id: Option<&str>, directory: &str) -> Value {
    match kind {
        RequestKind::Discover => json!({
            "id": id,
            "method": "thread/list",
            "params": {
                "cwd": directory,
                "limit": 10,
                "sortKey": "created_at",
                "sortDirection": "desc"
            }
        }),
        RequestKind::Resume => json!({
            "id": id,
            "method": "thread/resume",
            "params": {
                "threadId": thread_id.expect("resume request requires a thread id")
            }
        }),
    }
}

async fn initialize(reader: &mut OwnedReadHalf, writer: &mut OwnedWriteHalf) -> Result<()> {
    let request = json!({
        "id": 1,
        "method": "initialize",
        "params": {
            "clientInfo": {
                "name": "flowmux",
                "title": "Flowmux",
                "version": env!("CARGO_PKG_VERSION")
            },
            "capabilities": {"experimentalApi": true}
        }
    });
    send_text(writer, &request.to_string()).await?;
    let response = read_text(reader)
        .await?
        .ok_or_else(|| anyhow!("codex app-server closed during initialization"))?;
    let response: Value = serde_json::from_str(&response)?;
    if response.get("id").and_then(Value::as_u64) != Some(1) || response.get("result").is_none() {
        return Err(anyhow!("codex app-server initialization failed"));
    }
    send_text(
        writer,
        &json!({"method": "initialized", "params": {}}).to_string(),
    )
    .await?;
    Ok(())
}

async fn connect_websocket(port: u16) -> Result<(OwnedReadHalf, OwnedWriteHalf)> {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).await?;
    let request = format!(
        "GET / HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: Zmxvd211eC1jb2RleC0xIQ==\r\nSec-WebSocket-Version: 13\r\n\r\n"
    );
    stream.write_all(request.as_bytes()).await?;

    let mut response = Vec::with_capacity(512);
    let mut byte = [0u8; 1];
    while !response.ends_with(b"\r\n\r\n") && response.len() < 16 * 1024 {
        stream.read_exact(&mut byte).await?;
        response.push(byte[0]);
    }
    let response = std::str::from_utf8(&response)?;
    if !response.starts_with("HTTP/1.1 101") {
        return Err(anyhow!("codex app-server rejected WebSocket upgrade"));
    }
    Ok(stream.into_split())
}

async fn send_text(writer: &mut OwnedWriteHalf, text: &str) -> Result<()> {
    let payload = text.as_bytes();
    let mut frame = Vec::with_capacity(payload.len() + 14);
    frame.push(0x81);
    match payload.len() {
        len @ 0..=125 => frame.push(0x80 | len as u8),
        len @ 126..=65535 => {
            frame.push(0x80 | 126);
            frame.extend_from_slice(&(len as u16).to_be_bytes());
        }
        len => {
            frame.push(0x80 | 127);
            frame.extend_from_slice(&(len as u64).to_be_bytes());
        }
    }
    let mask = (unix_timestamp() as u32).to_be_bytes();
    frame.extend_from_slice(&mask);
    frame.extend(
        payload
            .iter()
            .enumerate()
            .map(|(index, byte)| byte ^ mask[index % 4]),
    );
    writer.write_all(&frame).await?;
    Ok(())
}

async fn read_text(reader: &mut OwnedReadHalf) -> Result<Option<String>> {
    let mut message = Vec::new();
    loop {
        let mut header = [0u8; 2];
        reader.read_exact(&mut header).await?;
        let finished = header[0] & 0x80 != 0;
        let opcode = header[0] & 0x0f;
        let masked = header[1] & 0x80 != 0;
        let mut length = (header[1] & 0x7f) as u64;
        if length == 126 {
            let mut extended = [0u8; 2];
            reader.read_exact(&mut extended).await?;
            length = u16::from_be_bytes(extended) as u64;
        } else if length == 127 {
            let mut extended = [0u8; 8];
            reader.read_exact(&mut extended).await?;
            length = u64::from_be_bytes(extended);
        }
        if length > 16 * 1024 * 1024 {
            return Err(anyhow!("codex app-server WebSocket frame is too large"));
        }

        let mask = if masked {
            let mut mask = [0u8; 4];
            reader.read_exact(&mut mask).await?;
            Some(mask)
        } else {
            None
        };
        let mut payload = vec![0u8; length as usize];
        reader.read_exact(&mut payload).await?;
        if let Some(mask) = mask {
            for (index, byte) in payload.iter_mut().enumerate() {
                *byte ^= mask[index % 4];
            }
        }

        match opcode {
            0x1 | 0x0 => message.extend_from_slice(&payload),
            0x8 => return Ok(None),
            0x9 | 0xa => continue,
            _ => continue,
        }
        if finished {
            return Ok(Some(String::from_utf8(message)?));
        }
    }
}

fn handle_message(
    message: &Value,
    directory: &str,
    min_created_at: i64,
    cached_session_id: &Arc<Mutex<Option<String>>>,
    live_cache: &Arc<RwLock<LiveCache>>,
) {
    if let Some(method) = message.get("method").and_then(Value::as_str) {
        let params = message.get("params").unwrap_or(&Value::Null);
        handle_notification(method, params, directory, cached_session_id, live_cache);
        return;
    }

    let Some(result) = message.get("result") else {
        return;
    };

    let Some(threads) = result.get("data").and_then(Value::as_array) else {
        return;
    };
    if cached_session_id.lock().unwrap().is_some() {
        return;
    }
    let selected = threads.iter().find(|thread| {
        thread.get("cwd").and_then(Value::as_str) == Some(directory)
            && thread.get("parentThreadId").is_none_or(Value::is_null)
            && thread
                .get("createdAt")
                .and_then(Value::as_i64)
                .unwrap_or_default()
                >= min_created_at
    });
    if let Some(thread) = selected {
        update_from_thread(thread, cached_session_id, live_cache);
    }
}

fn handle_thread_response(
    message: &Value,
    cached_session_id: &Arc<Mutex<Option<String>>>,
    live_cache: &Arc<RwLock<LiveCache>>,
) -> bool {
    let Some(result) = message.get("result") else {
        return false;
    };
    let Some(thread) = result.get("thread") else {
        return false;
    };
    update_from_thread(thread, cached_session_id, live_cache);
    if let Some(model) = result.get("model").and_then(Value::as_str) {
        live_cache.write().unwrap().model_name = Some(model.to_owned());
    }
    true
}

fn handle_notification(
    method: &str,
    params: &Value,
    directory: &str,
    cached_session_id: &Arc<Mutex<Option<String>>>,
    live_cache: &Arc<RwLock<LiveCache>>,
) {
    match method {
        "thread/started" => {
            if let Some(thread) = params.get("thread") {
                let current = cached_session_id.lock().unwrap().clone();
                let is_current = current.as_deref() == thread.get("id").and_then(Value::as_str);
                let is_new_root = is_root_thread_for_directory(thread, directory);
                if is_current || is_new_root {
                    if is_current || current.is_none() {
                        update_from_thread(thread, cached_session_id, live_cache);
                    } else {
                        switch_to_thread(thread, cached_session_id, live_cache);
                    }
                }
            }
        }
        "thread/status/changed" if is_current_thread(params, cached_session_id) => {
            live_cache.write().unwrap().status = status_from_value(params.get("status"));
        }
        "thread/settings/updated" if is_current_thread(params, cached_session_id) => {
            let model = params
                .get("threadSettings")
                .and_then(|s| s.get("model"))
                .and_then(Value::as_str)
                .map(str::to_owned);
            if model.is_some() {
                live_cache.write().unwrap().model_name = model;
            }
        }
        "thread/tokenUsage/updated" => {
            if !is_current_thread(params, cached_session_id) {
                return;
            }
            let usage = params.get("tokenUsage").unwrap_or(&Value::Null);
            let used = usage
                .get("last")
                .and_then(|v| v.get("totalTokens"))
                .and_then(Value::as_i64)
                .filter(|n| *n >= 0)
                .map(|n| n as u64);
            let total = usage
                .get("modelContextWindow")
                .and_then(Value::as_i64)
                .filter(|n| *n > 0)
                .map(|n| n as u64);
            if let Some(used) = used {
                live_cache.write().unwrap().context = Some(ContextInfo { used, total });
            }
        }
        "turn/started" if is_current_thread(params, cached_session_id) => {
            live_cache.write().unwrap().status = AgentStatus::Running;
        }
        "turn/completed" if is_current_thread(params, cached_session_id) => {
            let thread_id = cached_thread_id(cached_session_id);
            let mut cache = live_cache.write().unwrap();
            cache.status = AgentStatus::Idle;
            if let Some(turn) = params.get("turn") {
                record_turn_duration(
                    &mut cache,
                    turn.get("id").and_then(Value::as_str),
                    turn.get("durationMs").and_then(Value::as_i64),
                );
            }
            if cache.rollout_path.is_none()
                && let Some(thread_id) = thread_id
            {
                cache.rollout_path = find_rollout_path(&thread_id);
            }
            if let Some(path) = cache.rollout_path.clone() {
                enrich_from_rollout(&path, &mut cache);
            }
        }
        "item/completed" => {
            if !is_current_thread(params, cached_session_id) {
                return;
            }
            let item = params.get("item").unwrap_or(&Value::Null);
            let item_type = item.get("type").and_then(Value::as_str);
            if item_type == Some("agentMessage") || codex_item_role(item) == Some("assistant") {
                let mut cache = live_cache.write().unwrap();
                if let Some(text) = codex_item_text(item) {
                    cache.last_model_response = Some(text);
                }
            } else if live_cache.read().unwrap().first_prompt.is_none()
                && let Some(text) = user_message_item_text(item)
            {
                let mut cache = live_cache.write().unwrap();
                if cache.first_prompt.is_none() {
                    cache.first_prompt = Some(text);
                }
            }
        }
        "item/commandExecution/requestApproval"
        | "item/fileChange/requestApproval"
        | "item/permissions/requestApproval"
        | "item/tool/requestUserInput"
            if is_current_thread(params, cached_session_id) =>
        {
            live_cache.write().unwrap().status = AgentStatus::WaitingForInput;
        }
        "thread/closed" if is_current_thread(params, cached_session_id) => {
            live_cache.write().unwrap().status = AgentStatus::Stopped;
        }
        _ => {}
    }
}

fn is_root_thread_for_directory(thread: &Value, directory: &str) -> bool {
    thread.get("parentThreadId").is_none_or(Value::is_null)
        && thread.get("cwd").and_then(Value::as_str) == Some(directory)
}

fn switch_to_thread(
    thread: &Value,
    cached_session_id: &Arc<Mutex<Option<String>>>,
    live_cache: &Arc<RwLock<LiveCache>>,
) {
    reset_session_cache(live_cache);
    update_from_thread(thread, cached_session_id, live_cache);
}

fn reset_session_cache(live_cache: &Arc<RwLock<LiveCache>>) {
    let mut cache = live_cache.write().unwrap();
    cache.status = AgentStatus::Idle;
    cache.first_prompt = None;
    cache.last_model_response = None;
    cache.context = None;
    cache.total_work_ms = 0;
    cache.completed_turn_ids.clear();
    cache.rollout_path = None;
    cache.rollout_offset = 0;
}

fn update_from_thread(
    thread: &Value,
    cached_session_id: &Arc<Mutex<Option<String>>>,
    live_cache: &Arc<RwLock<LiveCache>>,
) {
    let Some(id) = thread.get("id").and_then(Value::as_str) else {
        return;
    };
    *cached_session_id.lock().unwrap() = Some(id.to_owned());

    let mut cache = live_cache.write().unwrap();
    cache.status = status_from_value(thread.get("status"));
    cache.rollout_path = thread
        .get("path")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| find_rollout_path(id))
        .or_else(|| cache.rollout_path.clone());
    if cache.first_prompt.is_none() {
        cache.first_prompt = first_prompt_from_turns(thread);
    }

    if let Some(turns) = thread.get("turns").and_then(Value::as_array) {
        for turn in turns {
            record_turn_duration(
                &mut cache,
                turn.get("id").and_then(Value::as_str),
                turn.get("durationMs").and_then(Value::as_i64),
            );
        }
        cache.last_model_response = turns
            .iter()
            .rev()
            .flat_map(|turn| {
                turn.get("items")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .rev()
            })
            .find(|item| item.get("type").and_then(Value::as_str) == Some("agentMessage"))
            .and_then(|item| item.get("text").and_then(Value::as_str))
            .map(str::to_owned)
            .or_else(|| cache.last_model_response.clone());
    }

    if let Some(path) = cache.rollout_path.clone() {
        enrich_from_rollout(&path, &mut cache);
    }
}

fn first_prompt_from_turns(thread: &Value) -> Option<String> {
    thread
        .get("turns")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .flat_map(|turn| {
            turn.get("items")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
        })
        .find_map(user_message_item_text)
}

fn user_message_item_text(item: &Value) -> Option<String> {
    (item.get("type").and_then(Value::as_str) == Some("userMessage"))
        .then(|| codex_item_text(item))
        .flatten()
}

fn codex_item_role(item: &Value) -> Option<&str> {
    item.get("role")
        .and_then(Value::as_str)
        .or_else(|| item.get("payload")?.get("role")?.as_str())
}

fn codex_item_text(item: &Value) -> Option<String> {
    item.get("text")
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
        .or_else(|| content_text(item.get("content")?))
        .or_else(|| codex_item_text(item.get("payload")?))
}

fn content_text(content: &Value) -> Option<String> {
    if let Some(text) = content.as_str().filter(|text| !text.is_empty()) {
        return Some(text.to_owned());
    }

    if let Some(parts) = content.as_array() {
        let text = parts
            .iter()
            .filter_map(|part| {
                part.get("text")
                    .and_then(Value::as_str)
                    .filter(|text| !text.is_empty())
                    .or_else(|| part.as_str().filter(|text| !text.is_empty()))
            })
            .collect::<Vec<_>>()
            .join("\n");
        return (!text.is_empty()).then_some(text);
    }

    content
        .get("text")
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

fn enrich_from_rollout(path: &str, cache: &mut LiveCache) {
    use std::io::{BufRead, Seek};

    let Ok(mut file) = std::fs::File::open(path) else {
        return;
    };
    let length = file.metadata().map(|meta| meta.len()).unwrap_or_default();
    if length < cache.rollout_offset {
        cache.rollout_offset = 0;
    }
    if file
        .seek(std::io::SeekFrom::Start(cache.rollout_offset))
        .is_err()
    {
        return;
    }
    let mut reader = std::io::BufReader::new(file);
    let mut line = String::new();
    loop {
        line.clear();
        let Ok(bytes_read) = reader.read_line(&mut line) else {
            break;
        };
        if bytes_read == 0 {
            break;
        }
        if !line.ends_with('\n') {
            break;
        }
        cache.rollout_offset += bytes_read as u64;
        let Ok(value) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let kind = value.get("type").and_then(Value::as_str);
        let payload = value.get("payload").unwrap_or(&Value::Null);
        if cache.first_prompt.is_none() {
            cache.first_prompt = first_prompt_from_rollout_record(kind, payload);
        }
        if kind == Some("event_msg")
            && payload.get("type").and_then(Value::as_str) == Some("task_complete")
        {
            record_turn_duration(
                cache,
                payload.get("turn_id").and_then(Value::as_str),
                payload.get("duration_ms").and_then(Value::as_i64),
            );
        }
    }
}

fn first_prompt_from_rollout_record(kind: Option<&str>, payload: &Value) -> Option<String> {
    match kind {
        Some("event_msg")
            if payload.get("type").and_then(Value::as_str) == Some("user_message") =>
        {
            payload
                .get("message")
                .and_then(Value::as_str)
                .filter(|text| !text.is_empty())
                .map(str::to_owned)
        }
        _ => None,
    }
}

fn find_rollout_path(thread_id: &str) -> Option<String> {
    let sessions_dir = codex_home().join("sessions");
    let mut stack = vec![sessions_dir];

    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if rollout_filename_matches_thread_id(&path, thread_id) {
                return Some(path.to_string_lossy().into_owned());
            }
        }
    }

    None
}

fn codex_home() -> PathBuf {
    std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".codex")))
        .unwrap_or_else(|| PathBuf::from(".codex"))
}

fn rollout_filename_matches_thread_id(path: &Path, thread_id: &str) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| {
            name.starts_with("rollout-")
                && name
                    .strip_suffix(".jsonl")
                    .is_some_and(|stem| stem.ends_with(thread_id))
        })
}

fn record_turn_duration(cache: &mut LiveCache, turn_id: Option<&str>, duration_ms: Option<i64>) {
    let (Some(turn_id), Some(duration_ms)) = (turn_id, duration_ms) else {
        return;
    };
    if duration_ms <= 0 || !cache.completed_turn_ids.insert(turn_id.to_owned()) {
        return;
    }
    cache.total_work_ms = cache.total_work_ms.saturating_add(duration_ms as u64);
}

fn status_from_value(status: Option<&Value>) -> AgentStatus {
    let Some(status) = status else {
        return AgentStatus::Unknown;
    };
    match status.get("type").and_then(Value::as_str) {
        Some("idle") => AgentStatus::Idle,
        Some("active") => {
            let waiting = status
                .get("activeFlags")
                .and_then(Value::as_array)
                .is_some_and(|flags| {
                    flags.iter().any(|flag| {
                        matches!(
                            flag.as_str(),
                            Some("waitingOnApproval" | "waitingOnUserInput")
                        )
                    })
                });
            if waiting {
                AgentStatus::WaitingForInput
            } else {
                AgentStatus::Running
            }
        }
        Some("notLoaded" | "systemError") => AgentStatus::Stopped,
        _ => AgentStatus::Unknown,
    }
}

fn is_current_thread(params: &Value, cached_session_id: &Arc<Mutex<Option<String>>>) -> bool {
    let event_id = params.get("threadId").and_then(Value::as_str);
    let current = cached_session_id.lock().unwrap();
    current.is_none() || event_id == current.as_deref()
}

fn subscription_matches_thread(
    subscribed_thread_id: Option<&str>,
    current_thread_id: Option<&str>,
) -> bool {
    matches!(
        (subscribed_thread_id, current_thread_id),
        (Some(subscribed), Some(current)) if subscribed == current
    )
}

fn cached_thread_id(cached_session_id: &Arc<Mutex<Option<String>>>) -> Option<String> {
    cached_session_id.lock().unwrap().clone()
}

fn is_unmaterialized_thread_error(message: &Value) -> bool {
    let error = message
        .get("error")
        .and_then(|error| error.get("message"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    error.starts_with("no rollout found for thread id ")
        || (error.starts_with("thread ")
            && error.contains(" is not materialized yet; ")
            && error.contains(" unavailable before first user message"))
}

fn mark_observer_unavailable(
    cached_session_id: &Arc<Mutex<Option<String>>>,
    live_cache: &Arc<RwLock<LiveCache>>,
) {
    if cached_session_id.lock().unwrap().is_none() {
        return;
    }

    let mut cache = live_cache.write().unwrap();
    if cache.status != AgentStatus::Stopped {
        cache.status = AgentStatus::Unknown;
    }
}

fn find_free_port(from: u16) -> u16 {
    let mut port = from;
    loop {
        if TcpListener::bind(("127.0.0.1", port)).is_ok() {
            return port;
        }
        port += 1;
    }
}

fn unix_timestamp() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener as TokioTcpListener;

    async fn accept_test_websocket(listener: TokioTcpListener) -> TcpStream {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let mut byte = [0u8; 1];
        while !request.ends_with(b"\r\n\r\n") {
            stream.read_exact(&mut byte).await.unwrap();
            request.push(byte[0]);
        }
        stream
            .write_all(
                b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\r\n",
            )
            .await
            .unwrap();
        stream
    }

    async fn read_test_client_text(stream: &mut TcpStream) -> String {
        let mut header = [0u8; 2];
        stream.read_exact(&mut header).await.unwrap();
        assert_eq!(header[0] & 0x0f, 0x1);
        assert_ne!(header[1] & 0x80, 0);
        let mut length = (header[1] & 0x7f) as usize;
        if length == 126 {
            let mut extended = [0u8; 2];
            stream.read_exact(&mut extended).await.unwrap();
            length = u16::from_be_bytes(extended) as usize;
        } else if length == 127 {
            let mut extended = [0u8; 8];
            stream.read_exact(&mut extended).await.unwrap();
            length = u64::from_be_bytes(extended) as usize;
        }
        let mut mask = [0u8; 4];
        stream.read_exact(&mut mask).await.unwrap();
        let mut payload = vec![0u8; length];
        stream.read_exact(&mut payload).await.unwrap();
        for (index, byte) in payload.iter_mut().enumerate() {
            *byte ^= mask[index % 4];
        }
        String::from_utf8(payload).unwrap()
    }

    async fn write_test_server_text(stream: &mut TcpStream, value: Value) {
        let payload = value.to_string();
        let mut frame = Vec::with_capacity(payload.len() + 10);
        frame.push(0x81);
        match payload.len() {
            len @ 0..=125 => frame.push(len as u8),
            len @ 126..=65535 => {
                frame.push(126);
                frame.extend_from_slice(&(len as u16).to_be_bytes());
            }
            len => {
                frame.push(127);
                frame.extend_from_slice(&(len as u64).to_be_bytes());
            }
        }
        frame.extend_from_slice(payload.as_bytes());
        stream.write_all(&frame).await.unwrap();
    }

    #[test]
    fn maps_active_waiting_status() {
        let status = json!({
            "type": "active",
            "activeFlags": ["waitingOnApproval"]
        });
        assert_eq!(
            status_from_value(Some(&status)),
            AgentStatus::WaitingForInput
        );
    }

    #[test]
    fn new_adapter_state_starts_idle() {
        assert_eq!(LiveCache::default().status, AgentStatus::Idle);
    }

    #[test]
    fn server_pid_signal_requires_matching_port_argument() {
        let cmdline = b"codex\0app-server\0--listen\0ws://127.0.0.1:32123\0";
        assert!(server_cmdline_matches_port(cmdline, 32123));
        assert!(!server_cmdline_matches_port(cmdline, 32124));
    }

    #[tokio::test]
    async fn readiness_probe_uses_http_endpoint() {
        let listener = TokioTcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut byte = [0u8; 1];
            while !request.ends_with(b"\r\n\r\n") {
                stream.read_exact(&mut byte).await.unwrap();
                request.push(byte[0]);
            }

            let request = String::from_utf8(request).unwrap();
            assert!(request.starts_with("GET /readyz HTTP/1.1\r\n"));
            assert!(!request.to_ascii_lowercase().contains("upgrade: websocket"));
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .await
                .unwrap();
        });

        let client = reqwest::Client::builder()
            .timeout(Duration::from_millis(500))
            .build()
            .unwrap();
        assert!(app_server_ready(&client, port).await);
        server.await.unwrap();
    }

    #[test]
    fn observer_failure_keeps_fresh_agent_idle() {
        let session_id = Arc::new(Mutex::new(None));
        let cache = Arc::new(RwLock::new(LiveCache::default()));

        mark_observer_unavailable(&session_id, &cache);
        assert_eq!(cache.read().unwrap().status, AgentStatus::Idle);
    }

    #[test]
    fn observer_failure_is_unknown_after_session_discovery() {
        let session_id = Arc::new(Mutex::new(Some("thread-1".to_owned())));
        let cache = Arc::new(RwLock::new(LiveCache {
            status: AgentStatus::Running,
            ..LiveCache::default()
        }));

        mark_observer_unavailable(&session_id, &cache);
        assert_eq!(cache.read().unwrap().status, AgentStatus::Unknown);

        cache.write().unwrap().status = AgentStatus::Stopped;
        mark_observer_unavailable(&session_id, &cache);
        assert_eq!(cache.read().unwrap().status, AgentStatus::Stopped);
    }

    #[test]
    fn observer_request_timeout_allows_slow_startup() {
        assert!(REQUEST_TIMEOUT >= Duration::from_secs(30));
    }

    #[test]
    fn extracts_thread_metadata() {
        let session = Arc::new(Mutex::new(None));
        let cache = Arc::new(RwLock::new(LiveCache::default()));
        let thread = json!({
            "id": "thread-1",
            "preview": "Fix the tests",
            "status": {"type": "idle"},
            "turns": [{
                "id": "turn-1",
                "durationMs": 1200,
                "items": [
                    {"type": "userMessage", "text": "Fix the tests"},
                    {"type": "agentMessage", "text": "Done"}
                ]
            }]
        });

        update_from_thread(&thread, &session, &cache);

        assert_eq!(session.lock().unwrap().as_deref(), Some("thread-1"));
        let cache = cache.read().unwrap();
        assert_eq!(cache.status, AgentStatus::Idle);
        assert_eq!(cache.first_prompt.as_deref(), Some("Fix the tests"));
        assert_eq!(cache.last_model_response.as_deref(), Some("Done"));
        assert_eq!(cache.total_work_ms, 1200);
    }

    #[test]
    fn extracts_first_prompt_from_turns_when_preview_is_missing() {
        let session = Arc::new(Mutex::new(None));
        let cache = Arc::new(RwLock::new(LiveCache::default()));
        let thread = json!({
            "id": "thread-1",
            "status": {"type": "idle"},
            "turns": [{
                "id": "turn-1",
                "items": [
                    {"type": "userMessage", "text": "Implement the parser"},
                    {"type": "agentMessage", "text": "Done"}
                ]
            }]
        });

        update_from_thread(&thread, &session, &cache);

        let cache = cache.read().unwrap();
        assert_eq!(cache.first_prompt.as_deref(), Some("Implement the parser"));
    }

    #[test]
    fn response_item_user_message_is_not_used_as_first_prompt() {
        let session = Arc::new(Mutex::new(None));
        let cache = Arc::new(RwLock::new(LiveCache::default()));
        let thread = json!({
            "id": "thread-1",
            "status": {"type": "idle"},
            "turns": [{
                "id": "turn-1",
                "items": [{
                    "type": "response_item",
                    "payload": {
                        "type": "message",
                        "role": "user",
                        "content": [{
                            "type": "input_text",
                            "text": "Implement the parser"
                        }]
                    }
                }]
            }]
        });

        update_from_thread(&thread, &session, &cache);

        let cache = cache.read().unwrap();
        assert_eq!(cache.first_prompt, None);
    }

    #[test]
    fn response_item_user_message_does_not_win_before_user_message_item() {
        let thread = json!({
            "turns": [{
                "items": [{
                    "type": "response_item",
                    "payload": {
                        "type": "message",
                        "role": "user",
                        "content": [{
                            "type": "input_text",
                            "text": "Synthetic context"
                        }]
                    }
                }, {
                    "type": "userMessage",
                    "text": "test hello"
                }]
            }]
        });

        assert_eq!(
            first_prompt_from_turns(&thread).as_deref(),
            Some("test hello")
        );
    }

    #[test]
    fn new_root_thread_switches_and_resets_session_meta() {
        let session = Arc::new(Mutex::new(Some("thread-1".to_string())));
        let cache = Arc::new(RwLock::new(LiveCache {
            status: AgentStatus::WaitingForInput,
            first_prompt: Some("Old prompt".to_string()),
            last_model_response: Some("Old response".to_string()),
            model_name: Some("gpt-old".to_string()),
            context: Some(ContextInfo {
                used: 50,
                total: Some(100),
            }),
            total_work_ms: 1234,
            completed_turn_ids: HashSet::from([String::from("turn-old")]),
            rollout_path: Some("/tmp/old-rollout.jsonl".to_string()),
            rollout_offset: 99,
        }));

        let params = json!({
            "thread": {
                "id": "thread-2",
                "cwd": "/tmp",
                "parentThreadId": null,
                "preview": "New prompt",
                "status": {"type": "active"},
                "turns": [{
                    "items": [{"type": "userMessage", "text": "New prompt"}]
                }]
            }
        });

        handle_notification("thread/started", &params, "/tmp", &session, &cache);

        assert_eq!(session.lock().unwrap().as_deref(), Some("thread-2"));
        let cache = cache.read().unwrap();
        assert_eq!(cache.status, AgentStatus::Running);
        assert_eq!(cache.first_prompt.as_deref(), Some("New prompt"));
        assert_eq!(cache.last_model_response, None);
        assert_eq!(cache.model_name.as_deref(), Some("gpt-old"));
        assert!(cache.context.is_none());
        assert_eq!(cache.total_work_ms, 0);
        assert!(cache.completed_turn_ids.is_empty());
        assert_eq!(cache.rollout_path, None);
        assert_eq!(cache.rollout_offset, 0);
    }

    #[test]
    fn switched_thread_ignores_old_events_and_accepts_new_ones() {
        let session = Arc::new(Mutex::new(Some("thread-1".to_string())));
        let cache = Arc::new(RwLock::new(LiveCache {
            status: AgentStatus::Idle,
            first_prompt: Some("Old prompt".to_string()),
            last_model_response: Some("Old response".to_string()),
            model_name: Some("gpt-old".to_string()),
            context: Some(ContextInfo {
                used: 50,
                total: Some(100),
            }),
            total_work_ms: 1234,
            completed_turn_ids: HashSet::from([String::from("turn-old")]),
            rollout_path: None,
            rollout_offset: 0,
        }));

        handle_notification(
            "thread/started",
            &json!({
                "thread": {
                    "id": "thread-2",
                    "cwd": "/tmp",
                    "parentThreadId": null,
                    "preview": "New prompt",
                    "status": {"type": "idle"},
                    "turns": []
                }
            }),
            "/tmp",
            &session,
            &cache,
        );

        handle_notification(
            "thread/tokenUsage/updated",
            &json!({
                "threadId": "thread-1",
                "tokenUsage": {
                    "last": {"totalTokens": 999},
                    "modelContextWindow": 1000
                }
            }),
            "/tmp",
            &session,
            &cache,
        );
        handle_notification(
            "item/completed",
            &json!({
                "threadId": "thread-1",
                "item": {
                    "type": "agentMessage",
                    "text": "Ignored old response"
                }
            }),
            "/tmp",
            &session,
            &cache,
        );
        handle_notification(
            "turn/completed",
            &json!({
                "threadId": "thread-1",
                "turn": {"id": "turn-old-2", "durationMs": 777}
            }),
            "/tmp",
            &session,
            &cache,
        );

        handle_notification(
            "thread/tokenUsage/updated",
            &json!({
                "threadId": "thread-2",
                "tokenUsage": {
                    "last": {"totalTokens": 80},
                    "modelContextWindow": 200
                }
            }),
            "/tmp",
            &session,
            &cache,
        );
        handle_notification(
            "item/completed",
            &json!({
                "threadId": "thread-2",
                "item": {
                    "type": "agentMessage",
                    "text": "New response"
                }
            }),
            "/tmp",
            &session,
            &cache,
        );
        handle_notification(
            "turn/completed",
            &json!({
                "threadId": "thread-2",
                "turn": {"id": "turn-2", "durationMs": 900}
            }),
            "/tmp",
            &session,
            &cache,
        );

        let cache = cache.read().unwrap();
        let context = cache
            .context
            .as_ref()
            .expect("new thread should update context");
        assert_eq!(context.used, 80);
        assert_eq!(context.total, Some(200));
        assert_eq!(cache.last_model_response.as_deref(), Some("New response"));
        assert_eq!(cache.total_work_ms, 900);
        assert!(cache.completed_turn_ids.contains("turn-2"));
        assert!(!cache.completed_turn_ids.contains("turn-old"));
        assert!(!cache.completed_turn_ids.contains("turn-old-2"));
    }

    #[test]
    fn new_thread_backfills_first_prompt_from_user_item_event() {
        let session = Arc::new(Mutex::new(Some("thread-1".to_string())));
        let cache = Arc::new(RwLock::new(LiveCache {
            status: AgentStatus::Idle,
            first_prompt: Some("Old prompt".to_string()),
            last_model_response: Some("Old response".to_string()),
            model_name: None,
            context: None,
            total_work_ms: 0,
            completed_turn_ids: HashSet::new(),
            rollout_path: None,
            rollout_offset: 0,
        }));

        handle_notification(
            "thread/started",
            &json!({
                "thread": {
                    "id": "thread-2",
                    "cwd": "/tmp",
                    "parentThreadId": null,
                    "status": {"type": "active"},
                    "turns": []
                }
            }),
            "/tmp",
            &session,
            &cache,
        );

        handle_notification(
            "item/completed",
            &json!({
                "threadId": "thread-2",
                "item": {
                    "type": "userMessage",
                    "text": "New prompt"
                }
            }),
            "/tmp",
            &session,
            &cache,
        );

        let cache = cache.read().unwrap();
        assert_eq!(cache.first_prompt.as_deref(), Some("New prompt"));
        assert_eq!(cache.last_model_response, None);
    }

    #[test]
    fn live_nested_assistant_item_updates_response_not_first_prompt() {
        let session = Arc::new(Mutex::new(Some("thread-1".to_string())));
        let cache = Arc::new(RwLock::new(LiveCache::default()));

        handle_notification(
            "item/completed",
            &json!({
                "threadId": "thread-1",
                "item": {
                    "type": "response_item",
                    "payload": {
                        "type": "message",
                        "role": "assistant",
                        "content": [{
                            "type": "output_text",
                            "text": "New response"
                        }]
                    }
                }
            }),
            "/tmp",
            &session,
            &cache,
        );

        let cache = cache.read().unwrap();
        assert_eq!(cache.first_prompt, None);
        assert_eq!(cache.last_model_response.as_deref(), Some("New response"));
    }

    #[test]
    fn live_response_items_do_not_win_before_user_message_item() {
        let session = Arc::new(Mutex::new(Some("thread-1".to_string())));
        let cache = Arc::new(RwLock::new(LiveCache::default()));

        handle_notification(
            "item/completed",
            &json!({
                "threadId": "thread-1",
                "item": {
                    "type": "response_item",
                    "payload": {
                        "type": "message",
                        "role": "developer",
                        "content": [{
                            "type": "input_text",
                            "text": "Developer instructions"
                        }]
                    }
                }
            }),
            "/tmp",
            &session,
            &cache,
        );
        handle_notification(
            "item/completed",
            &json!({
                "threadId": "thread-1",
                "item": {
                    "type": "response_item",
                    "payload": {
                        "role": "user",
                        "content": [{
                            "type": "input_text",
                            "text": "Synthetic context with user role"
                        }]
                    }
                }
            }),
            "/tmp",
            &session,
            &cache,
        );
        handle_notification(
            "item/completed",
            &json!({
                "threadId": "thread-1",
                "item": {
                    "type": "userMessage",
                    "text": "Hello test one one two trheeeee"
                }
            }),
            "/tmp",
            &session,
            &cache,
        );

        let cache = cache.read().unwrap();
        assert_eq!(
            cache.first_prompt.as_deref(),
            Some("Hello test one one two trheeeee")
        );
    }

    #[test]
    fn rollout_task_complete_tracks_and_deduplicates_work_time() {
        let path = std::env::temp_dir().join(format!(
            "flowmux-codex-duration-{}.jsonl",
            std::process::id()
        ));
        std::fs::write(
            &path,
            concat!(
                "{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_complete\",\"turn_id\":\"turn-1\",\"duration_ms\":1629}}\n",
                "{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_complete\",\"turn_id\":\"turn-2\",\"duration_ms\":2371}}\n"
            ),
        )
        .unwrap();

        let mut cache = LiveCache::default();
        enrich_from_rollout(path.to_str().unwrap(), &mut cache);
        enrich_from_rollout(path.to_str().unwrap(), &mut cache);

        assert_eq!(cache.total_work_ms, 4000);
        assert_eq!(cache.completed_turn_ids.len(), 2);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn rollout_event_msg_user_message_backfills_first_prompt() {
        let path = std::env::temp_dir().join(format!(
            "flowmux-codex-first-prompt-event-{}.jsonl",
            std::process::id()
        ));
        std::fs::write(
            &path,
            concat!(
                "{\"type\":\"event_msg\",\"payload\":{\"type\":\"user_message\",\"message\":\"Hello test one one two\"}}\n",
                "{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_complete\",\"turn_id\":\"turn-1\",\"duration_ms\":1629}}\n"
            ),
        )
        .unwrap();

        let mut cache = LiveCache::default();
        enrich_from_rollout(path.to_str().unwrap(), &mut cache);

        assert_eq!(
            cache.first_prompt.as_deref(),
            Some("Hello test one one two")
        );
        assert_eq!(cache.total_work_ms, 1629);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn rollout_uses_user_message_event_not_response_item_transcript() {
        let path = std::env::temp_dir().join(format!(
            "flowmux-codex-first-prompt-agents-bootstrap-{}.jsonl",
            std::process::id()
        ));
        std::fs::write(
            &path,
            concat!(
                "{\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"Synthetic context\"}]}}\n",
                "{\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"Transcript copy of prompt\"}]}}\n",
                "{\"type\":\"event_msg\",\"payload\":{\"type\":\"user_message\",\"message\":\"test hello\"}}\n"
            ),
        )
        .unwrap();

        let mut cache = LiveCache::default();
        enrich_from_rollout(path.to_str().unwrap(), &mut cache);

        assert_eq!(cache.first_prompt.as_deref(), Some("test hello"));
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn rollout_filename_match_requires_rollout_jsonl_for_thread_id() {
        assert!(rollout_filename_matches_thread_id(
            Path::new("rollout-2026-06-19T23-04-40-019ee1b3-3ce0-7281-8116-2e57425c9410.jsonl"),
            "019ee1b3-3ce0-7281-8116-2e57425c9410"
        ));
        assert!(!rollout_filename_matches_thread_id(
            Path::new("other-019ee1b3-3ce0-7281-8116-2e57425c9410.jsonl"),
            "019ee1b3-3ce0-7281-8116-2e57425c9410"
        ));
        assert!(!rollout_filename_matches_thread_id(
            Path::new("rollout-2026-06-19T22-49-02-019ee1a4-ea83-7c31-b461-138e19476c23.jsonl"),
            "019ee1b3-3ce0-7281-8116-2e57425c9410"
        ));
        assert!(!rollout_filename_matches_thread_id(
            Path::new("rollout-2026-06-19T23-04-40-019ee1b3-3ce0-7281-8116-2e57425c9410.txt"),
            "019ee1b3-3ce0-7281-8116-2e57425c9410"
        ));
    }

    #[test]
    fn app_server_and_rollout_duration_do_not_double_count() {
        let mut cache = LiveCache::default();
        record_turn_duration(&mut cache, Some("turn-1"), Some(1500));
        record_turn_duration(&mut cache, Some("turn-1"), Some(1500));
        assert_eq!(cache.total_work_ms, 1500);
    }

    #[test]
    fn launch_command_uses_flowmux_subcommand() {
        let command = crate::launch::flowmux_launch_command(
            "codex",
            &[
                std::ffi::OsString::from("--port"),
                std::ffi::OsString::from("16123"),
            ],
        );

        assert!(command.contains("launch codex"));
        assert!(command.contains("--port 16123"));
        assert!(!command.contains("app-server"));
    }

    #[test]
    fn turn_completion_updates_cache_without_requiring_history_refresh() {
        let session = Arc::new(Mutex::new(Some("thread-1".to_string())));
        let cache = Arc::new(RwLock::new(LiveCache::default()));
        let params = json!({
            "threadId": "thread-1",
            "turn": {"id": "turn-2", "durationMs": 900}
        });

        handle_notification("turn/completed", &params, "", &session, &cache);

        {
            let cached = cache.read().unwrap();
            assert_eq!(cached.status, AgentStatus::Idle);
            assert_eq!(cached.total_work_ms, 900);
        }

        let thread = json!({
            "id": "thread-1",
            "status": {"type": "idle"},
            "turns": [{
                "id": "turn-2",
                "items": [{"type": "agentMessage", "text": "Finished"}]
            }]
        });
        update_from_thread(&thread, &session, &cache);

        let cached = cache.read().unwrap();
        assert_eq!(cached.last_model_response.as_deref(), Some("Finished"));
        assert_eq!(cached.total_work_ms, 900);
    }

    #[test]
    fn builds_full_resume_request() {
        assert_eq!(
            request_for(10, RequestKind::Resume, Some("thread-1"), ""),
            json!({
                "id": 10,
                "method": "thread/resume",
                "params": {
                    "threadId": "thread-1"
                }
            })
        );
    }

    #[tokio::test]
    async fn unmaterialized_thread_stays_idle_and_retries_resume() {
        let listener = TokioTcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let (retry_tx, retry_rx) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let mut stream = accept_test_websocket(listener).await;

            let initialize: Value =
                serde_json::from_str(&read_test_client_text(&mut stream).await).unwrap();
            assert_eq!(
                initialize.get("method").and_then(Value::as_str),
                Some("initialize")
            );
            write_test_server_text(&mut stream, json!({"id": 1, "result": {}})).await;
            let _initialized = read_test_client_text(&mut stream).await;

            let resume: Value =
                serde_json::from_str(&read_test_client_text(&mut stream).await).unwrap();
            assert_eq!(
                resume.get("method").and_then(Value::as_str),
                Some("thread/resume")
            );
            let resume_id = resume.get("id").and_then(Value::as_u64).unwrap();
            write_test_server_text(
                &mut stream,
                json!({
                    "id": resume_id,
                    "error": {
                        "code": -32600,
                        "message": "no rollout found for thread id thread-1"
                    }
                }),
            )
            .await;

            let retry: Value =
                serde_json::from_str(&read_test_client_text(&mut stream).await).unwrap();
            assert_eq!(
                retry.get("method").and_then(Value::as_str),
                Some("thread/resume")
            );
            retry_tx.send(()).unwrap();
            sleep(Duration::from_millis(100)).await;
        });

        let adapter = CodexAdapter::new(port, "/tmp".to_string(), Some("thread-1".to_string()));
        tokio::time::timeout(Duration::from_secs(2), retry_rx)
            .await
            .expect("observer did not retry thread/resume")
            .unwrap();
        assert_eq!(adapter.get_status().await, AgentStatus::Idle);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn subscribed_observer_updates_from_events_without_recurring_reads() {
        let listener = TokioTcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let mut stream = accept_test_websocket(listener).await;

            let initialize: Value =
                serde_json::from_str(&read_test_client_text(&mut stream).await).unwrap();
            assert_eq!(
                initialize.get("method").and_then(Value::as_str),
                Some("initialize")
            );
            write_test_server_text(&mut stream, json!({"id": 1, "result": {}})).await;

            let initialized: Value =
                serde_json::from_str(&read_test_client_text(&mut stream).await).unwrap();
            assert_eq!(
                initialized.get("method").and_then(Value::as_str),
                Some("initialized")
            );

            let discover: Value =
                serde_json::from_str(&read_test_client_text(&mut stream).await).unwrap();
            assert_eq!(
                discover.get("method").and_then(Value::as_str),
                Some("thread/list")
            );
            let discover_id = discover.get("id").and_then(Value::as_u64).unwrap();
            write_test_server_text(
                &mut stream,
                json!({
                    "id": discover_id,
                    "result": {
                        "data": [{
                            "id": "thread-1",
                            "cwd": "/tmp",
                            "createdAt": 1,
                            "parentThreadId": null,
                            "preview": "Test prompt",
                            "status": {"type": "idle"},
                            "turns": []
                        }]
                    }
                }),
            )
            .await;

            let resume: Value =
                serde_json::from_str(&read_test_client_text(&mut stream).await).unwrap();
            assert_eq!(
                resume.get("method").and_then(Value::as_str),
                Some("thread/resume")
            );
            let resume_id = resume.get("id").and_then(Value::as_u64).unwrap();
            write_test_server_text(
                &mut stream,
                json!({
                    "id": resume_id,
                    "result": {
                        "model": "gpt-test",
                        "thread": {
                            "id": "thread-1",
                            "preview": "Test prompt",
                            "status": {"type": "idle"},
                            "turns": [{
                                "id": "turn-1",
                                "items": [{"type": "agentMessage", "text": "Initial"}]
                            }]
                        }
                    }
                }),
            )
            .await;

            write_test_server_text(
                &mut stream,
                json!({
                    "method": "thread/tokenUsage/updated",
                    "params": {
                        "threadId": "thread-1",
                        "turnId": "turn-1",
                        "tokenUsage": {
                            "total": {"totalTokens": 150},
                            "last": {"totalTokens": 90},
                            "modelContextWindow": 200000
                        }
                    }
                }),
            )
            .await;

            write_test_server_text(
                &mut stream,
                json!({
                    "method": "item/completed",
                    "params": {
                        "threadId": "thread-1",
                        "turnId": "turn-2",
                        "completedAtMs": 1,
                        "item": {
                            "id": "item-2",
                            "type": "agentMessage",
                            "text": "Live response"
                        }
                    }
                }),
            )
            .await;

            assert!(
                tokio::time::timeout(
                    Duration::from_millis(900),
                    read_test_client_text(&mut stream)
                )
                .await
                .is_err(),
                "observer sent a recurring request after subscription"
            );
        });

        let adapter = CodexAdapter::new(port, "/tmp".to_string(), None);
        let mut response = None;
        let mut context = None;
        for _ in 0..50 {
            response = adapter.get_last_model_response().await;
            context = adapter.get_context().await;
            if response.as_deref() == Some("Live response") && context.is_some() {
                break;
            }
            sleep(Duration::from_millis(20)).await;
        }

        assert_eq!(response.as_deref(), Some("Live response"));
        let context = context.unwrap();
        assert_eq!(context.used, 90);
        assert_eq!(context.total, Some(200_000));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn subscribed_observer_updates_first_prompt_from_live_user_item() {
        let listener = TokioTcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let mut stream = accept_test_websocket(listener).await;

            let initialize: Value =
                serde_json::from_str(&read_test_client_text(&mut stream).await).unwrap();
            assert_eq!(
                initialize.get("method").and_then(Value::as_str),
                Some("initialize")
            );
            write_test_server_text(&mut stream, json!({"id": 1, "result": {}})).await;

            let initialized: Value =
                serde_json::from_str(&read_test_client_text(&mut stream).await).unwrap();
            assert_eq!(
                initialized.get("method").and_then(Value::as_str),
                Some("initialized")
            );

            let discover: Value =
                serde_json::from_str(&read_test_client_text(&mut stream).await).unwrap();
            assert_eq!(
                discover.get("method").and_then(Value::as_str),
                Some("thread/list")
            );
            let discover_id = discover.get("id").and_then(Value::as_u64).unwrap();
            write_test_server_text(
                &mut stream,
                json!({
                    "id": discover_id,
                    "result": {
                        "data": [{
                            "id": "thread-1",
                            "cwd": "/tmp",
                            "createdAt": 1,
                            "parentThreadId": null,
                            "status": {"type": "idle"},
                            "turns": []
                        }]
                    }
                }),
            )
            .await;

            let resume: Value =
                serde_json::from_str(&read_test_client_text(&mut stream).await).unwrap();
            assert_eq!(
                resume.get("method").and_then(Value::as_str),
                Some("thread/resume")
            );
            let resume_id = resume.get("id").and_then(Value::as_u64).unwrap();
            write_test_server_text(
                &mut stream,
                json!({
                    "id": resume_id,
                    "result": {
                        "model": "gpt-test",
                        "thread": {
                            "id": "thread-1",
                            "status": {"type": "idle"},
                            "turns": []
                        }
                    }
                }),
            )
            .await;

            write_test_server_text(
                &mut stream,
                json!({
                    "method": "item/completed",
                    "params": {
                        "threadId": "thread-1",
                        "turnId": "turn-1",
                        "item": {
                            "type": "userMessage",
                            "text": "Live prompt"
                        }
                    }
                }),
            )
            .await;

            assert!(
                tokio::time::timeout(
                    Duration::from_millis(900),
                    read_test_client_text(&mut stream)
                )
                .await
                .is_err(),
                "observer sent a recurring request after live first prompt"
            );
        });

        let adapter = CodexAdapter::new(port, "/tmp".to_string(), None);
        let mut prompt = None;
        for _ in 0..50 {
            prompt = adapter.get_first_prompt().await;
            if prompt.as_deref() == Some("Live prompt") {
                break;
            }
            sleep(Duration::from_millis(20)).await;
        }

        assert_eq!(prompt.as_deref(), Some("Live prompt"));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn new_root_thread_triggers_resubscribe_and_refreshes_meta() {
        let listener = TokioTcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let mut stream = accept_test_websocket(listener).await;

            let initialize: Value =
                serde_json::from_str(&read_test_client_text(&mut stream).await).unwrap();
            assert_eq!(
                initialize.get("method").and_then(Value::as_str),
                Some("initialize")
            );
            write_test_server_text(&mut stream, json!({"id": 1, "result": {}})).await;

            let initialized: Value =
                serde_json::from_str(&read_test_client_text(&mut stream).await).unwrap();
            assert_eq!(
                initialized.get("method").and_then(Value::as_str),
                Some("initialized")
            );

            let discover: Value =
                serde_json::from_str(&read_test_client_text(&mut stream).await).unwrap();
            assert_eq!(
                discover.get("method").and_then(Value::as_str),
                Some("thread/list")
            );
            let discover_id = discover.get("id").and_then(Value::as_u64).unwrap();
            write_test_server_text(
                &mut stream,
                json!({
                    "id": discover_id,
                    "result": {
                        "data": [{
                            "id": "thread-1",
                            "cwd": "/tmp",
                            "createdAt": 1,
                            "parentThreadId": null,
                            "preview": "Old prompt",
                            "status": {"type": "idle"},
                            "turns": [{
                                "id": "turn-1",
                                "durationMs": 100,
                                "items": [{"type": "agentMessage", "text": "Old response"}]
                            }]
                        }]
                    }
                }),
            )
            .await;

            let resume_old: Value =
                serde_json::from_str(&read_test_client_text(&mut stream).await).unwrap();
            assert_eq!(
                resume_old.get("method").and_then(Value::as_str),
                Some("thread/resume")
            );
            assert_eq!(
                resume_old
                    .get("params")
                    .and_then(|p| p.get("threadId"))
                    .and_then(Value::as_str),
                Some("thread-1")
            );
            let resume_old_id = resume_old.get("id").and_then(Value::as_u64).unwrap();
            write_test_server_text(
                &mut stream,
                json!({
                    "id": resume_old_id,
                    "result": {
                        "model": "gpt-old",
                        "thread": {
                            "id": "thread-1",
                            "preview": "Old prompt",
                            "status": {"type": "idle"},
                            "turns": [{
                                "id": "turn-1",
                                "durationMs": 100,
                                "items": [{"type": "agentMessage", "text": "Old response"}]
                            }]
                        }
                    }
                }),
            )
            .await;

            write_test_server_text(
                &mut stream,
                json!({
                    "method": "thread/started",
                    "params": {
                        "thread": {
                            "id": "thread-2",
                            "cwd": "/tmp",
                            "parentThreadId": null,
                            "status": {"type": "idle"},
                            "turns": [{
                                "id": "turn-2",
                                "items": [{
                                    "type": "userMessage",
                                    "text": "New prompt"
                                }]
                            }]
                        }
                    }
                }),
            )
            .await;

            let resume_new: Value =
                serde_json::from_str(&read_test_client_text(&mut stream).await).unwrap();
            assert_eq!(
                resume_new.get("method").and_then(Value::as_str),
                Some("thread/resume")
            );
            assert_eq!(
                resume_new
                    .get("params")
                    .and_then(|p| p.get("threadId"))
                    .and_then(Value::as_str),
                Some("thread-2")
            );
            let resume_new_id = resume_new.get("id").and_then(Value::as_u64).unwrap();
            write_test_server_text(
                &mut stream,
                json!({
                    "id": resume_new_id,
                    "result": {
                        "model": "gpt-new",
                        "thread": {
                            "id": "thread-2",
                            "path": "/tmp/thread-2.jsonl",
                            "status": {"type": "idle"},
                            "turns": [{
                                "id": "turn-2",
                                "durationMs": 900,
                                "items": [
                                    {"type": "userMessage", "text": "New prompt"},
                                    {"type": "agentMessage", "text": "New response"}
                                ]
                            }]
                        }
                    }
                }),
            )
            .await;

            write_test_server_text(
                &mut stream,
                json!({
                    "method": "thread/tokenUsage/updated",
                    "params": {
                        "threadId": "thread-2",
                        "tokenUsage": {
                            "last": {"totalTokens": 80},
                            "modelContextWindow": 200
                        }
                    }
                }),
            )
            .await;

            assert!(
                tokio::time::timeout(
                    Duration::from_millis(900),
                    read_test_client_text(&mut stream)
                )
                .await
                .is_err(),
                "observer sent an unexpected recurring request after resubscribe"
            );
        });

        let adapter = CodexAdapter::new(port, "/tmp".to_string(), None);
        let mut context_used = None;
        let mut total_work_ms = 0;
        let mut prompt = None;
        let mut response = None;
        let mut model = None;
        for _ in 0..50 {
            context_used = adapter.get_context().await.map(|ctx| ctx.used);
            total_work_ms = adapter.get_total_work_ms().await;
            prompt = adapter.get_first_prompt().await;
            response = adapter.get_last_model_response().await;
            model = adapter.get_model_name().await;
            if context_used == Some(80)
                && total_work_ms == 900
                && prompt.as_deref() == Some("New prompt")
                && response.as_deref() == Some("New response")
                && model.as_deref() == Some("gpt-new")
            {
                break;
            }
            sleep(Duration::from_millis(20)).await;
        }

        assert_eq!(context_used, Some(80));
        assert_eq!(total_work_ms, 900);
        assert_eq!(prompt.as_deref(), Some("New prompt"));
        assert_eq!(response.as_deref(), Some("New response"));
        assert_eq!(model.as_deref(), Some("gpt-new"));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn websocket_text_round_trip() {
        let listener = TokioTcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut byte = [0u8; 1];
            while !request.ends_with(b"\r\n\r\n") {
                stream.read_exact(&mut byte).await.unwrap();
                request.push(byte[0]);
            }
            stream
                .write_all(
                    b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\r\n",
                )
                .await
                .unwrap();

            let mut header = [0u8; 2];
            stream.read_exact(&mut header).await.unwrap();
            assert_eq!(header[0], 0x81);
            let length = (header[1] & 0x7f) as usize;
            assert_ne!(header[1] & 0x80, 0);
            let mut mask = [0u8; 4];
            stream.read_exact(&mut mask).await.unwrap();
            let mut payload = vec![0u8; length];
            stream.read_exact(&mut payload).await.unwrap();
            for (index, byte) in payload.iter_mut().enumerate() {
                *byte ^= mask[index % 4];
            }
            assert_eq!(payload, br#"{"ping":true}"#);

            let response = br#"{"pong":true}"#;
            stream
                .write_all(&[0x81, response.len() as u8])
                .await
                .unwrap();
            stream.write_all(response).await.unwrap();
        });

        let (mut reader, mut writer) = connect_websocket(port).await.unwrap();
        send_text(&mut writer, r#"{"ping":true}"#).await.unwrap();
        assert_eq!(
            read_text(&mut reader).await.unwrap().as_deref(),
            Some(r#"{"pong":true}"#)
        );
        server.await.unwrap();
    }

    #[tokio::test]
    #[ignore = "requires an installed codex CLI"]
    async fn installed_codex_app_server_smoke() {
        let port = find_free_port(19100);
        let mut child = std::process::Command::new("codex")
            .args(["app-server", "--listen", &format!("ws://127.0.0.1:{port}")])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();

        let client = reqwest::Client::builder()
            .timeout(Duration::from_millis(500))
            .build()
            .unwrap();
        let mut ready = false;
        for _ in 0..25 {
            if app_server_ready(&client, port).await {
                ready = true;
                break;
            }
            sleep(Duration::from_millis(100)).await;
        }
        assert!(ready, "codex app-server did not start");

        let (mut reader, mut writer) = connect_websocket(port).await.unwrap();
        initialize(&mut reader, &mut writer).await.unwrap();
        send_text(
            &mut writer,
            &json!({
                "id": 2,
                "method": "thread/list",
                "params": {"limit": 1}
            })
            .to_string(),
        )
        .await
        .unwrap();

        loop {
            let response: Value =
                serde_json::from_str(&read_text(&mut reader).await.unwrap().unwrap()).unwrap();
            if response.get("id").and_then(Value::as_u64) == Some(2) {
                assert!(response.get("result").and_then(|r| r.get("data")).is_some());
                break;
            }
        }

        child.kill().unwrap();
        child.wait().unwrap();
    }

    #[tokio::test]
    #[ignore = "requires an installed codex CLI"]
    async fn installed_codex_stop_closes_server() {
        let port = find_free_port(19100);
        let mut child = std::process::Command::new("codex")
            .args(["app-server", "--listen", &format!("ws://127.0.0.1:{port}")])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        std::fs::write(server_pid_path(port), child.id().to_string()).unwrap();

        let client = reqwest::Client::builder()
            .timeout(Duration::from_millis(500))
            .build()
            .unwrap();
        for _ in 0..25 {
            if app_server_ready(&client, port).await {
                break;
            }
            sleep(Duration::from_millis(100)).await;
        }
        assert!(app_server_ready(&client, port).await);

        stop_server(port).await.unwrap();
        child.wait().unwrap();
        assert!(TcpStream::connect(("127.0.0.1", port)).await.is_err());
        assert!(!std::path::Path::new(&server_pid_path(port)).exists());
    }
}

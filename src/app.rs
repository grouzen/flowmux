use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::time::{interval, Duration};

use crate::agents::AgentAdapter;
use crate::config::Config;
use crate::models::{AgentEntry, AgentMeta, AgentStatus, AgentType};
use crate::runner::AgentRunner;
use crate::tmux;
use crate::ui::dashboard::grid_layout;

// ---------------------------------------------------------------------------
// AppState
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub enum AppState {
    Dashboard,
    CreateAgentDialog,
    AgentView(usize),
    RemoveAgentDialog(usize),
}

// ---------------------------------------------------------------------------
// Event
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum Event {
    Key(KeyEvent),
    Mouse(MouseEvent),
    Paste(String),
    DashboardTick,
    AgentViewTick,
}

// ---------------------------------------------------------------------------
// AgentViewState (owned by App, rendered by ui)
// ---------------------------------------------------------------------------

/// Maximum number of lines retained in memory for the agent view.
/// Older lines beyond this cap are discarded; the tmux pane itself retains
/// the full scrollback so copy-mode scrolling is unaffected.
const MAX_RETAINED_LINES: usize = 2000;

#[derive(Debug, Default)]
pub struct AgentViewState {
    pub lines: Vec<String>,
    pub last_refresh: Option<std::time::SystemTime>,
    pub show_stopped_overlay: bool,
    /// Cursor position within the pane's visible screen (col, row).
    pub cursor: Option<(u16, u16)>,
    /// Last dimensions sent to tmux resize-window (width, height).  Used to
    /// skip redundant resize calls that would otherwise send SIGWINCH to any
    /// process (e.g. vim) running inside the pane on every dirty frame.
    pub last_pane_size: Option<(u16, u16)>,
    /// Whether the process currently running in the pane has enabled any mouse
    /// reporting mode (tmux #{mouse_any_flag}).  Polled every tick so that
    /// hover / all-motion events are only forwarded when the pane application
    /// actually expects them.  When false (e.g. vim opened as $EDITOR without
    /// `set mouse=a`), forwarding hover events would send a leading ESC byte
    /// that exits insert mode.
    pub pane_mouse_active: bool,
    /// Track previous status to detect edge transitions
    prev_status: Option<AgentStatus>,
    /// Byte length of the last captured raw string, used to skip no-op ticks.
    prev_raw_len: usize,
    /// Last raw capture for byte-exact change detection.
    prev_raw: String,
}

impl AgentViewState {
    /// Returns `true` if the lines were updated (raw content changed),
    /// `false` if the capture was identical to the previous tick.
    pub fn update_lines(&mut self, raw: &str) -> bool {
        // Fast path: length differs → definitely changed.
        // Slow path: same length → do a full byte comparison to catch same-length
        // rewrites (e.g. opencode redraws its input field with ANSI in-place).
        if raw.len() == self.prev_raw_len && raw == self.prev_raw {
            return false;
        }
        self.prev_raw_len = raw.len();
        self.prev_raw = raw.to_owned();

        let all_lines = raw.trim_end_matches('\n').split('\n');
        // Keep only the last MAX_RETAINED_LINES to bound allocation cost.
        let new_lines: Vec<String> = all_lines.map(|s| s.to_string()).collect();
        let start = new_lines.len().saturating_sub(MAX_RETAINED_LINES);
        self.lines = new_lines[start..].to_vec();
        self.last_refresh = Some(std::time::SystemTime::now());
        true
    }
}

// ---------------------------------------------------------------------------
// CreateAgentState
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub enum CreateField {
    Name,
    Directory,
    AgentType,
}

#[derive(Debug)]
pub struct CreateAgentState {
    pub name: String,
    pub directory: String,
    pub focus: CreateField,
    pub error: Option<String>,
    pub tab_matches: Vec<String>,
    pub tab_idx: usize,
    /// Agent types available when the dialog was opened (from runner discovery).
    pub available_types: Vec<AgentType>,
    /// Index into `available_types` for the currently selected type.
    pub selected_type_idx: usize,
}

impl Default for CreateAgentState {
    fn default() -> Self {
        Self {
            name: String::new(),
            directory: String::new(),
            focus: CreateField::Name,
            error: None,
            tab_matches: Vec::new(),
            tab_idx: 0,
            available_types: vec![],
            selected_type_idx: 0,
        }
    }
}

impl CreateAgentState {
    pub fn selected_agent_type(&self) -> AgentType {
        self.available_types
            .get(self.selected_type_idx)
            .cloned()
            .unwrap_or(AgentType::Opencode)
    }

    pub fn is_valid(&self) -> bool {
        !self.name.trim().is_empty() && !self.directory.trim().is_empty()
    }

    /// Perform Tab completion on the Directory field.
    pub fn handle_tab(&mut self) {
        let current = self.directory.clone();
        let path = std::path::Path::new(&current);

        let (parent, prefix) = if current.ends_with('/') || current.is_empty() {
            (
                if current.is_empty() {
                    std::path::PathBuf::from(".")
                } else {
                    std::path::PathBuf::from(&current)
                },
                String::new(),
            )
        } else {
            let p = path.parent().unwrap_or(std::path::Path::new("."));
            let fname = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_string();
            (p.to_path_buf(), fname)
        };

        // If no matches cached (or prefix changed), rebuild
        if self.tab_matches.is_empty() {
            if let Ok(rd) = std::fs::read_dir(&parent) {
                let mut matches: Vec<String> = rd
                    .flatten()
                    .filter_map(|e| {
                        let name = e.file_name().into_string().ok()?;
                        if name.starts_with(&prefix) && e.file_type().ok()?.is_dir() {
                            let mut full = parent.join(&name);
                            // Canonicalize for nicer display
                            if let Ok(c) = full.canonicalize() {
                                full = c;
                            }
                            Some(full.to_string_lossy().to_string())
                        } else {
                            None
                        }
                    })
                    .collect();
                matches.sort();
                if current.is_empty() {
                    if let Ok(cwd) = std::env::current_dir() {
                        matches.insert(0, cwd.to_string_lossy().to_string());
                    }
                }
                self.tab_matches = matches;
                self.tab_idx = 0;
            }
        } else {
            self.tab_idx = (self.tab_idx + 1) % self.tab_matches.len().max(1);
        }

        if let Some(m) = self.tab_matches.get(self.tab_idx) {
            self.directory = m.clone();
        }
    }
}

// ---------------------------------------------------------------------------
// App
// ---------------------------------------------------------------------------

pub struct App {
    pub agents: Vec<AgentEntry>,
    pub adapters: Vec<Box<dyn AgentAdapter>>,
    pub state: AppState,
    pub selected: usize,
    pub config: Config,
    pub runner: AgentRunner,
    pub agent_view_state: AgentViewState,
    pub create_state: CreateAgentState,
    pub tx: UnboundedSender<Event>,
    pub rx: UnboundedReceiver<Event>,
    /// Set to `true` whenever state changes and a redraw is needed.
    /// Cleared to `false` by the render loop after each draw.
    pub dirty: bool,
    /// Per-card scroll offset for the model response block on the dashboard.
    pub card_scroll: Vec<u16>,
    /// Per-card response viewport height, updated every render frame.
    /// Used to cap scroll so content doesn't scroll past the last line.
    pub card_response_heights: Vec<u16>,
    /// Per-card response content area width, updated every render frame.
    /// Used together with Paragraph::line_count to compute the true
    /// wrapped line count for accurate max-scroll calculation.
    pub card_response_widths: Vec<u16>,
}

impl App {
    pub fn new(config: Config, agents: Vec<AgentEntry>, adapters: Vec<Box<dyn AgentAdapter>>, runner: AgentRunner) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let card_count = agents.len();
        Self {
            agents,
            adapters,
            state: AppState::Dashboard,
            selected: 0,
            config,
            runner,
            agent_view_state: AgentViewState::default(),
            create_state: CreateAgentState::default(),
            tx,
            rx,
            dirty: true, // force initial draw
            card_scroll: vec![0u16; card_count],
            card_response_heights: vec![0u16; card_count],
            card_response_widths: vec![0u16; card_count],
        }
    }

    pub fn is_claude_available(&self) -> bool {
        self.runner.is_claude_available()
    }

    pub fn is_opencode_available(&self) -> bool {
        self.runner.is_opencode_available()
    }

    /// Spawn background tasks (crossterm events, dashboard ticker, agent view ticker).
    pub fn spawn_tasks(&self) {
        // Crossterm event reader
        let tx = self.tx.clone();
        tokio::spawn(async move {
            use crossterm::event::{Event as CEvent, EventStream};
            use futures::StreamExt;
            let mut stream = EventStream::new();
            while let Some(Ok(event)) = stream.next().await {
                match event {
                    CEvent::Key(k) => {
                        let _ = tx.send(Event::Key(k));
                    }
                    CEvent::Mouse(m) => {
                        let _ = tx.send(Event::Mouse(m));
                    }
                    CEvent::Paste(text) => {
                        let _ = tx.send(Event::Paste(text));
                    }

                    _ => {}
                }
            }
        });

        // Dashboard ticker — 500 ms
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let mut ticker = interval(Duration::from_millis(500));
            loop {
                ticker.tick().await;
                let _ = tx.send(Event::DashboardTick);
            }
        });

        // AgentView ticker — 50 ms
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let mut ticker = interval(Duration::from_millis(50));
            loop {
                ticker.tick().await;
                let _ = tx.send(Event::AgentViewTick);
            }
        });
    }

    // -----------------------------------------------------------------------
    // Event dispatch
    // -----------------------------------------------------------------------

    /// Returns false when the app should quit.
    pub async fn handle_event(&mut self, event: Event) -> bool {
        match event {
            Event::Key(key) => {
                self.dirty = true;
                self.handle_key(key).await
            }
            Event::Mouse(mouse) => {
                self.dirty = true;
                self.handle_mouse(mouse);
                true
            }
            Event::Paste(text) => {
                self.dirty = true;
                self.handle_paste(text);
                true
            }
            Event::DashboardTick => {
                self.handle_dashboard_tick().await;
                self.dirty = true;
                true
            }
            Event::AgentViewTick => {
                // handle_agent_view_tick sets self.dirty = true only when
                // the captured output has actually changed.
                self.handle_agent_view_tick().await;
                true
            }
        }
    }

    /// Returns the dashboard card slot index (into `self.agents`) for a given
    /// terminal cell `(col, row)`, or `None` if the position is out of bounds
    /// (e.g. the keybindings bar row) or the agents list is empty.
    fn dashboard_slot_at(&self, col: u16, row: u16) -> Option<usize> {
        let n = self.agents.len();
        if n == 0 {
            return None;
        }
        let (term_w, term_h) = crossterm::terminal::size().unwrap_or((80, 24));
        // The bottom row is the keybindings bar — ignore clicks there.
        if row >= term_h.saturating_sub(1) {
            return None;
        }
        let main_h = term_h.saturating_sub(1);
        let (cols, rows) = grid_layout(n);
        let cell_w = term_w / cols as u16;
        let cell_h = main_h / rows as u16;
        if cell_w == 0 || cell_h == 0 {
            return None;
        }
        let c = (col / cell_w).min(cols as u16 - 1) as usize;
        let r = (row / cell_h).min(rows as u16 - 1) as usize;
        let slot = r * cols + c;
        if slot < n { Some(slot) } else { None }
    }

    fn handle_dashboard_mouse(&mut self, mouse: MouseEvent) {
        match mouse.kind {
            MouseEventKind::Down(_) => {
                if let Some(slot) = self.dashboard_slot_at(mouse.column, mouse.row) {
                    self.selected = slot;
                    self.dirty = true;
                }
            }
            MouseEventKind::ScrollUp => {
                if let Some(slot) = self.dashboard_slot_at(mouse.column, mouse.row) {
                    if let Some(s) = self.card_scroll.get_mut(slot) {
                        *s = s.saturating_sub(1);
                        self.dirty = true;
                    }
                }
            }
            MouseEventKind::ScrollDown => {
                if let Some(slot) = self.dashboard_slot_at(mouse.column, mouse.row) {
                    let viewport_h = self
                        .card_response_heights
                        .get(slot)
                        .copied()
                        .unwrap_or(1)
                        .max(1);
                    let content_w = self
                        .card_response_widths
                        .get(slot)
                        .copied()
                        .unwrap_or(80)
                        .max(1);
                    let max_scroll = self
                        .agents
                        .get(slot)
                        .and_then(|e| e.meta.last_model_response.as_deref())
                        .map(|r| {
                            let text = tui_markdown::from_str(r);
                            let total = wrapped_line_count(&text, content_w);
                            total.saturating_sub(viewport_h)
                        })
                        .unwrap_or(0);
                    if let Some(s) = self.card_scroll.get_mut(slot) {
                        *s = s.saturating_add(1).min(max_scroll);
                        self.dirty = true;
                    }
                }
            }
            _ => {}
        }
    }

    fn handle_mouse(&mut self, mouse: MouseEvent) {
        if matches!(self.state, AppState::Dashboard) {
            self.handle_dashboard_mouse(mouse);
            return;
        }

        let AppState::AgentView(idx) = self.state else {
            return;
        };

        // Scroll events are forwarded as named tmux keys.
        match mouse.kind {
            MouseEventKind::ScrollUp => {
                if let Some(entry) = self.agents.get(idx) {
                    let _ = tmux::send_keys(&entry.config.pane, "PPage");
                }
                return;
            }
            MouseEventKind::ScrollDown => {
                if let Some(entry) = self.agents.get(idx) {
                    let _ = tmux::send_keys(&entry.config.pane, "NPage");
                }
                return;
            }
            _ => {}
        }

        // For all other mouse events, forward as an SGR escape sequence via
        // `send-keys -l` so the application inside the pane receives them.

        // Guard: skip events on the status bar (last row of the terminal).
        let term_height = crossterm::terminal::size().map(|(_, h)| h).unwrap_or(24);
        if mouse.row >= term_height.saturating_sub(1) {
            return;
        }

        // Guard: don't forward while the "agent stopped" overlay is visible.
        if self.agent_view_state.show_stopped_overlay {
            return;
        }

        // Map event kind → (SGR button code, is_press).
        // SGR button encoding: 0=left 1=middle 2=right; drag adds 32; hover=35.
        // Modifiers: Shift+4, Alt+8, Ctrl+16.
        //
        // Hover / all-motion events (Moved) are only forwarded when the pane
        // application has enabled mouse reporting (#{mouse_any_flag} == 1).
        // If the pane application has NOT enabled mouse mode — for example vim
        // opened as $EDITOR without `set mouse=a` — the leading \x1b of the
        // SGR hover sequence would be interpreted as Escape, exiting insert
        // mode and potentially triggering normal-mode commands (the trailing
        // 'M' maps to vim's "move to middle of screen").
        if mouse.kind == MouseEventKind::Moved && !self.agent_view_state.pane_mouse_active {
            return;
        }

        let (mut cb, press) = match mouse.kind {
            MouseEventKind::Down(btn) => (Self::sgr_button(btn), true),
            MouseEventKind::Up(btn) => (Self::sgr_button(btn), false),
            MouseEventKind::Drag(btn) => (Self::sgr_button(btn) + 32, true),
            MouseEventKind::Moved => (35u8, true),
            _ => return,
        };

        if mouse.modifiers.contains(KeyModifiers::SHIFT) {
            cb += 4;
        }
        if mouse.modifiers.contains(KeyModifiers::ALT) {
            cb += 8;
        }
        if mouse.modifiers.contains(KeyModifiers::CONTROL) {
            cb += 16;
        }

        // SGR format: ESC [ < Cb ; Cx ; Cy M (press) or m (release).
        // Coordinates are 1-based.
        let suffix = if press { 'M' } else { 'm' };
        let seq = format!(
            "\x1b[<{};{};{}{}",
            cb,
            mouse.column + 1,
            mouse.row + 1,
            suffix
        );

        if let Some(entry) = self.agents.get(idx) {
            let _ = tmux::send_literal(&entry.config.pane, &seq);
        }
    }

    fn sgr_button(btn: MouseButton) -> u8 {
        match btn {
            MouseButton::Left => 0,
            MouseButton::Middle => 1,
            MouseButton::Right => 2,
        }
    }

    /// Forward a paste event to the active tmux pane using bracketed paste
    /// sequences (`\x1b[200~...\x1b[201~`).  This tells the editor inside the
    /// pane to suppress auto-indentation for the pasted content, fixing broken
    /// indentation.  The entire text is sent in a single `send_literal` call
    /// (one tmux subprocess) rather than character-by-character, which makes
    /// pasting large blocks of text fast.
    fn handle_paste(&mut self, text: String) {
        let AppState::AgentView(idx) = self.state else {
            return;
        };
        if self.agent_view_state.show_stopped_overlay {
            return;
        }
        if let Some(entry) = self.agents.get(idx) {
            let seq = format!("\x1b[200~{}\x1b[201~", text);
            let _ = tmux::send_literal(&entry.config.pane, &seq);
        }
    }

    async fn handle_key(&mut self, key: KeyEvent) -> bool {
        match &self.state.clone() {
            AppState::Dashboard => self.handle_dashboard_key(key),
            AppState::AgentView(idx) => {
                let idx = *idx;
                self.handle_agent_view_key(key, idx).await
            }
            AppState::CreateAgentDialog => self.handle_create_key(key).await,
            AppState::RemoveAgentDialog(idx) => {
                let idx = *idx;
                self.handle_remove_key(key, idx)
            }
        }
    }

    // -----------------------------------------------------------------------
    // Dashboard key handler
    // -----------------------------------------------------------------------

    /// Swap the selected card with `target`, keeping `selected` tracking the
    /// moved card.  Scroll offsets and cached geometry follow the swap.
    fn move_card(&mut self, target: usize) {
        self.agents.swap(self.selected, target);
        self.adapters.swap(self.selected, target);
        self.config.agents.swap(self.selected, target);
        self.card_scroll.swap(self.selected, target);
        let max_idx = self.selected.max(target);
        if self.card_response_heights.len() > max_idx {
            self.card_response_heights.swap(self.selected, target);
            self.card_response_widths.swap(self.selected, target);
        }
        self.selected = target;
        self.dirty = true;
        let _ = self.config.save();
    }

    fn handle_dashboard_key(&mut self, key: KeyEvent) -> bool {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('q') => return false,
            KeyCode::Char('n') => {
                self.create_state = CreateAgentState {
                    available_types: self.runner.available_agent_types(),
                    ..CreateAgentState::default()
                };
                self.state = AppState::CreateAgentDialog;
            }
            KeyCode::Char('d') => {
                if !self.agents.is_empty() {
                    self.state = AppState::RemoveAgentDialog(self.selected);
                }
            }
            KeyCode::Enter => {
                if !self.agents.is_empty() {
                    self.agent_view_state = AgentViewState::default();
                    self.state = AppState::AgentView(self.selected);
                }
            }
            // ---------------------------------------------------------------
            // Card movement: Ctrl+arrows / Ctrl+hjkl
            // ---------------------------------------------------------------
            KeyCode::Left if ctrl => {
                if !self.agents.is_empty() {
                    let (cols, _) = grid_layout(self.agents.len());
                    // Mirror navigate-left wrapping: not at leftmost col OR
                    // not on first row (wrap to last slot of previous row).
                    if self.selected % cols > 0 || self.selected >= cols {
                        self.move_card(self.selected - 1);
                    }
                }
            }
            KeyCode::Right if ctrl => {
                if !self.agents.is_empty() {
                    // Mirror navigate-right wrapping: any next card exists.
                    if self.selected + 1 < self.agents.len() {
                        self.move_card(self.selected + 1);
                    }
                }
            }
            KeyCode::Up if ctrl => {
                if !self.agents.is_empty() {
                    let (cols, _) = grid_layout(self.agents.len());
                    if self.selected >= cols {
                        self.move_card(self.selected - cols);
                    }
                }
            }
            KeyCode::Down if ctrl => {
                if !self.agents.is_empty() {
                    let (cols, _) = grid_layout(self.agents.len());
                    if self.selected + cols < self.agents.len() {
                        self.move_card(self.selected + cols);
                    }
                }
            }
            // ---------------------------------------------------------------
            // Navigation: arrows / hjkl (with Left/Right row-edge wrapping)
            // ---------------------------------------------------------------
            KeyCode::Left | KeyCode::Char('h') => {
                if !self.agents.is_empty() {
                    let (cols, _) = grid_layout(self.agents.len());
                    // Move left within row; at col 0 wrap to last slot of
                    // the previous row (same index arithmetic: selected - 1).
                    if self.selected % cols > 0 || self.selected >= cols {
                        self.selected -= 1;
                        self.reset_card_scroll();
                    }
                }
            }
            KeyCode::Right | KeyCode::Char('l') => {
                if !self.agents.is_empty() {
                    // Move right within row; at last col wrap to first slot
                    // of the next row, as long as a next card exists.
                    if self.selected + 1 < self.agents.len() {
                        self.selected += 1;
                        self.reset_card_scroll();
                    }
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if !self.agents.is_empty() {
                    let (cols, _) = grid_layout(self.agents.len());
                    if self.selected >= cols {
                        self.selected -= cols;
                        self.reset_card_scroll();
                    }
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if !self.agents.is_empty() {
                    let (cols, _) = grid_layout(self.agents.len());
                    if self.selected + cols < self.agents.len() {
                        self.selected += cols;
                        self.reset_card_scroll();
                    }
                }
            }
            KeyCode::PageDown => {
                if let Some(s) = self.card_scroll.get_mut(self.selected) {
                    let viewport_h = self
                        .card_response_heights
                        .get(self.selected)
                        .copied()
                        .unwrap_or(1)
                        .max(1);
                    let content_w = self
                        .card_response_widths
                        .get(self.selected)
                        .copied()
                        .unwrap_or(80)
                        .max(1);
                    let max_scroll = self
                        .agents
                        .get(self.selected)
                        .and_then(|e| e.meta.last_model_response.as_deref())
                        .map(|r| {
                            let text = tui_markdown::from_str(r);
                            let total = wrapped_line_count(&text, content_w);
                            total.saturating_sub(viewport_h)
                        })
                        .unwrap_or(0);
                    *s = s.saturating_add(5).min(max_scroll);
                    self.dirty = true;
                }
            }
            KeyCode::PageUp => {
                if let Some(s) = self.card_scroll.get_mut(self.selected) {
                    *s = s.saturating_sub(5);
                    self.dirty = true;
                }
            }
            _ => {}
        }
        true
    }

    fn reset_card_scroll(&mut self) {
        if let Some(s) = self.card_scroll.get_mut(self.selected) {
            *s = 0;
        }
        self.dirty = true;
    }

    // -----------------------------------------------------------------------
    // Dashboard tick — poll all agents
    // -----------------------------------------------------------------------

    async fn handle_dashboard_tick(&mut self) {
        let len = self.adapters.len();
        let mut config_dirty = false;
        for i in 0..len {
            let status = self.adapters[i].get_status().await;
            let context = self.adapters[i].get_context().await;
            let first_prompt = self.adapters[i].get_first_prompt().await;
            let last_model_response = self.adapters[i].get_last_model_response().await;
            let model_name = self.adapters[i].get_model_name().await;
            let total_work_ms = self.adapters[i].get_total_work_ms().await;

            // Persist newly discovered session IDs so the dashboard shows
            // correct history immediately on the next startup.
            let session_id = self.adapters[i].get_cached_session_id();
            if let Some(agent_config) = self.config.agents.get_mut(i) {
                if session_id.is_some() && session_id.as_deref() != agent_config.session_id() {
                    agent_config.set_session_id(session_id);
                    config_dirty = true;
                }
            }

            if let Some(entry) = self.agents.get_mut(i) {
                entry.meta.status = status;
                entry.meta.context = context;
                entry.meta.first_prompt = first_prompt;
                entry.meta.last_model_response = last_model_response;
                entry.meta.model_name = model_name;
                entry.meta.total_work_ms = total_work_ms;
            }
        }
        // Ensure card_scroll has an entry for every agent (agents may be added at runtime).
        if self.card_scroll.len() < self.agents.len() {
            self.card_scroll.resize(self.agents.len(), 0);
        }
        if self.card_response_heights.len() < self.agents.len() {
            self.card_response_heights.resize(self.agents.len(), 0);
        }
        if self.card_response_widths.len() < self.agents.len() {
            self.card_response_widths.resize(self.agents.len(), 0);
        }
        if config_dirty {
            let _ = self.config.save();
        }
    }

    // -----------------------------------------------------------------------
    // AgentView tick — capture pane, detect stopped
    // -----------------------------------------------------------------------

    async fn handle_agent_view_tick(&mut self) {
        let idx = match &self.state {
            AppState::AgentView(i) => *i,
            _ => return,
        };

        if let Some(entry) = self.agents.get(idx) {
            let pane = entry.config.pane.clone();

            // Check liveness before paying for cursor_position on dead panes.
            if !tmux::is_alive(&pane) {
                let prev = self.agent_view_state.prev_status.clone();
                if prev.as_ref() != Some(&AgentStatus::Stopped) {
                    self.agent_view_state.show_stopped_overlay = true;
                    self.dirty = true;
                }
                self.agent_view_state.prev_status = Some(AgentStatus::Stopped.clone());
                if let Some(e) = self.agents.get_mut(idx) {
                    e.meta.status = AgentStatus::Stopped;
                }
                return;
            }

            if let Ok(raw) = tmux::capture_pane(&pane) {
                // update_lines returns true only when content changed.
                if self.agent_view_state.update_lines(&raw) {
                    self.dirty = true;
                }
            }
            let new_cursor = tmux::cursor_position(&pane);
            if new_cursor != self.agent_view_state.cursor {
                self.agent_view_state.cursor = new_cursor;
                self.dirty = true;
            }

            // Track whether the pane application has mouse mode enabled.
            // Hover events are only forwarded when this is true, to avoid
            // sending a raw ESC byte to programs (e.g. vim as $EDITOR) that
            // have not requested mouse input.
            self.agent_view_state.pane_mouse_active = tmux::pane_mouse_active(&pane);

            // Resize the tmux window to fill the viewport, but only when the
            // terminal dimensions have actually changed.  Calling resize-window
            // on every tick would send SIGWINCH to any process running in the
            // pane (e.g. vim), causing it to redraw, move the cursor, and
            // potentially reset the editing mode on every poll cycle.
            if let Ok((term_cols, term_rows)) = crossterm::terminal::size() {
                let content_height = term_rows.saturating_sub(1); // reserve status bar row
                let desired = (term_cols, content_height);
                if self.agent_view_state.last_pane_size != Some(desired) {
                    let _ = tmux::resize_window(&pane, term_cols, content_height);
                    self.agent_view_state.last_pane_size = Some(desired);
                }
            }

            // Update status via adapter
            if let Some(adapter) = self.adapters.get(idx) {
                let new_status = adapter.get_status().await;
                let prev = self.agent_view_state.prev_status.clone();
                // Detect edge transition to Stopped
                if new_status == AgentStatus::Stopped
                    && prev.as_ref() != Some(&AgentStatus::Stopped)
                {
                    self.agent_view_state.show_stopped_overlay = true;
                }
                if prev.as_ref() != Some(&new_status) {
                    self.dirty = true;
                }
                self.agent_view_state.prev_status = Some(new_status.clone());
                if let Some(e) = self.agents.get_mut(idx) {
                    e.meta.status = new_status;
                }
            }
        }
    }

    // -----------------------------------------------------------------------
    // AgentView key handler
    // -----------------------------------------------------------------------

    async fn handle_agent_view_key(&mut self, key: KeyEvent, idx: usize) -> bool {
        if self.agent_view_state.show_stopped_overlay {
            match key.code {
                KeyCode::Char('r') => {
                    self.restart_agent(idx).await;
                    self.agent_view_state.show_stopped_overlay = false;
                    self.dirty = true;
                }
                KeyCode::Char('d') => {
                    self.remove_agent(idx);
                    self.state = AppState::Dashboard;
                }
                KeyCode::Char('g') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.agent_view_state.show_stopped_overlay = false;
                    self.state = AppState::Dashboard;
                }
                _ => {}
            }
            return true;
        }

        match key.code {
            KeyCode::Char('g') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.state = AppState::Dashboard;
            }
            _ => {
                // Forward key to tmux pane
                if let Some(entry) = self.agents.get(idx) {
                    let pane = entry.config.pane.clone();
                    let keys = key_event_to_tmux(&key);
                    if !keys.is_empty() {
                        let _ = tmux::send_keys(&pane, &keys);
                    }
                }
            }
        }
        true
    }

    // -----------------------------------------------------------------------
    // CreateAgentDialog key handler
    // -----------------------------------------------------------------------

    async fn handle_create_key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Esc => {
                self.state = AppState::Dashboard;
            }
            KeyCode::Tab => {
                if self.create_state.focus == CreateField::Directory {
                    self.create_state.handle_tab();
                }
            }
            KeyCode::Up => {
                self.create_state.focus = match self.create_state.focus {
                    CreateField::Name => CreateField::Name,
                    CreateField::Directory => CreateField::Name,
                    CreateField::AgentType => CreateField::Directory,
                };
            }
            KeyCode::Down => {
                self.create_state.focus = match self.create_state.focus {
                    CreateField::Name => CreateField::Directory,
                    CreateField::Directory => {
                        if self.create_state.available_types.len() > 1 {
                            CreateField::AgentType
                        } else {
                            CreateField::Directory
                        }
                    }
                    CreateField::AgentType => CreateField::AgentType,
                };
            }
            // Left / Right cycle agent type when that row is focused.
            KeyCode::Left | KeyCode::Right
                if self.create_state.focus == CreateField::AgentType =>
            {
                let n = self.create_state.available_types.len();
                if n > 0 {
                    let idx = self.create_state.selected_type_idx;
                    self.create_state.selected_type_idx = if key.code == KeyCode::Right {
                        (idx + 1) % n
                    } else {
                        (idx + n - 1) % n
                    };
                }
            }
            KeyCode::Enter => {
                if self.create_state.is_valid() {
                    let name = tmux::sanitize_name(&self.create_state.name.clone());
                    let dir = self.create_state.directory.clone();
                    let agent_type = self.create_state.selected_agent_type();
                    match self.runner.create(&name, &dir, agent_type).await {
                        Ok((config, adapter)) => {
                            self.config.agents.push(config.clone());
                            let _ = self.config.save();
                            let entry = AgentEntry {
                                config,
                                meta: AgentMeta::default(),
                            };
                            self.agents.push(entry);
                            self.adapters.push(adapter);
                            let new_idx = self.agents.len() - 1;
                            self.selected = new_idx;
                            self.agent_view_state = AgentViewState::default();
                            self.state = AppState::AgentView(new_idx);
                        }
                        Err(e) => {
                            self.create_state.error = Some(e.to_string());
                        }
                    }
                }
            }
            KeyCode::Backspace => {
                match self.create_state.focus {
                    CreateField::Name => {
                        self.create_state.name.pop();
                    }
                    CreateField::Directory => {
                        self.create_state.directory.pop();
                        // Invalidate tab matches when user edits
                        self.create_state.tab_matches.clear();
                        self.create_state.tab_idx =0;
                    }
                    CreateField::AgentType => {}
                }
            }
            KeyCode::Char(c) => {
                match self.create_state.focus {
                    CreateField::Name => {
                        self.create_state.name.push(c);
                    }
                    CreateField::Directory => {
                        self.create_state.directory.push(c);
                        // Invalidate tab matches when user edits
                        self.create_state.tab_matches.clear();
                        self.create_state.tab_idx = 0;
                    }
                    CreateField::AgentType => {}
                }
            }
            _ => {}
        }
        true
    }

    // -----------------------------------------------------------------------
    // RemoveAgentDialog key handler
    // -----------------------------------------------------------------------

    fn handle_remove_key(&mut self, key: KeyEvent, idx: usize) -> bool {
        match key.code {
            KeyCode::Char('y') | KeyCode::Enter => {
                self.remove_agent(idx);
                self.state = AppState::Dashboard;
            }
            KeyCode::Char('n') | KeyCode::Esc => {
                self.state = AppState::Dashboard;
            }
            _ => {}
        }
        true
    }

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    fn remove_agent(&mut self, idx: usize) {
        if idx < self.agents.len() {
            // Kill the tmux window before removing the agent
            if let Some(agent_config) = self.config.agents.get(idx) {
                // Extract window target from pane (e.g., "stable:1.0" -> "stable:1")
                if let Some(colon_pos) = agent_config.pane.find(':') {
                    if let Some(dot_pos) = agent_config.pane[colon_pos..].find('.') {
                        let window_target = &agent_config.pane[..colon_pos + dot_pos];
                        let _ = tmux::kill_window(window_target);
                    }
                }
            }
            self.agents.remove(idx);
            self.adapters.remove(idx);
            self.config.agents.remove(idx);
            let _ = self.config.save();
            // Adjust selected if needed
            if self.selected >= self.agents.len() && !self.agents.is_empty() {
                self.selected = self.agents.len() - 1;
            }
        }
    }

    /// Restart a stopped agent via AgentRunner, then update in-memory state
    /// and persist the config.
    pub async fn restart_agent(&mut self, idx: usize) {
        let config = match self.config.agents.get(idx) {
            Some(c) => c.clone(),
            None => return,
        };

        match self.runner.restart(&config).await {
            Ok((new_config, new_adapter)) => {
                // Update persisted config.
                if let Some(c) = self.config.agents.get_mut(idx) {
                    *c = new_config.clone();
                }
                let _ = self.config.save();

                // Update in-memory agent entry.
                if let Some(entry) = self.agents.get_mut(idx) {
                    entry.config = new_config;
                    entry.meta.status = AgentStatus::Unknown;
                }

                // Swap in the new adapter.
                if idx < self.adapters.len() {
                    self.adapters[idx] = new_adapter;
                }
            }
            Err(_) => {
                // Restart failed — leave state as-is (agent stays Stopped).
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Key → tmux string conversion
// ---------------------------------------------------------------------------

/// Count the number of visual (wrapped) lines a `Text` will occupy in a
/// widget of the given `width`.  This is a lightweight approximation: it
/// sums the display-column widths of each `Line`'s spans and divides by
/// `width`, rounding up.  Empty logical lines count as one visual line.
fn wrapped_line_count(text: &ratatui::text::Text, width: u16) -> u16 {
    if width == 0 {
        return 0;
    }
    let mut count: u16 = 0;
    for line in text.iter() {
        let line_width: usize = line
            .spans
            .iter()
            .map(|s| unicode_display_width(s.content.as_ref()))
            .sum();
        let rows = if line_width == 0 {
            1
        } else {
            ((line_width as u16).saturating_sub(1) / width) + 1
        };
        count = count.saturating_add(rows);
    }
    count
}

/// Approximate display-column width of a string (ASCII fast path; falls back
/// to character count for non-ASCII so we don't need a heavy Unicode library).
fn unicode_display_width(s: &str) -> usize {
    if s.is_ascii() {
        s.len()
    } else {
        s.chars().count()
    }
}

fn key_event_to_tmux(key: &KeyEvent) -> String {
    // Ctrl combos
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        if let KeyCode::Char(c) = key.code {
            return format!("C-{}", c);
        }
    }
    match key.code {
        KeyCode::Char(c) => c.to_string(),
        KeyCode::Enter => "Enter".to_string(),
        KeyCode::Backspace => "BSpace".to_string(),
        KeyCode::Tab => "Tab".to_string(),
        KeyCode::Esc => "Escape".to_string(),
        KeyCode::Left => "Left".to_string(),
        KeyCode::Right => "Right".to_string(),
        KeyCode::Up => "Up".to_string(),
        KeyCode::Down => "Down".to_string(),
        KeyCode::PageUp => "PPage".to_string(),
        KeyCode::PageDown => "NPage".to_string(),
        KeyCode::Home => "Home".to_string(),
        KeyCode::End => "End".to_string(),
        KeyCode::Delete => "DC".to_string(),
        _ => String::new(),
    }
}

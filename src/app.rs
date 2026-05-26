use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::time::{Duration, interval};

use crate::agents::AgentAdapter;
use crate::config::{AgentKind, Config};
use crate::models::{AgentEntry, AgentMeta, AgentStatus, AgentType};
use crate::runner::AgentRunner;
use crate::tmux;
use crate::ui::dashboard::grid_layout;

// ---------------------------------------------------------------------------
// AppState
// ---------------------------------------------------------------------------

/// State carried by the remove-agent confirmation dialog.
#[derive(Debug, Clone)]
pub struct RemoveAgentState {
    /// Index of the agent to remove.
    pub idx: usize,
    /// Whether to also remove the git worktree (only shown when the agent has one).
    pub remove_worktree: bool,
}

/// State for the git viewer pane view.
#[derive(Debug, Clone)]
pub struct GitViewerState {
    /// Index of the agent we came from (to return to on exit).
    pub agent_idx: usize,
    /// tmux pane target (e.g. "stable:5.0").
    pub pane: String,
    /// Captured pane output lines.
    pub lines: Vec<String>,
    /// Cursor position within the pane's visible screen (col, row).
    pub cursor: Option<(u16, u16)>,
    /// Last dimensions sent to tmux resize-window (width, height).
    pub last_pane_size: Option<(u16, u16)>,
    /// Whether the process currently running in the pane has enabled mouse reporting.
    pub pane_mouse_active: bool,
    /// When true, the next keypress will be forwarded directly to the tmux pane.
    pub prefix_active: bool,
    /// Byte length of the last captured raw string (for change detection).
    prev_raw_len: usize,
    /// Last raw capture for byte-exact change detection.
    prev_raw: String,
}

impl GitViewerState {
    pub fn new(agent_idx: usize, pane: String) -> Self {
        Self {
            agent_idx,
            pane,
            lines: Vec::new(),
            cursor: None,
            last_pane_size: None,
            pane_mouse_active: false,
            prefix_active: false,
            prev_raw_len: 0,
            prev_raw: String::new(),
        }
    }

    pub fn update_lines(&mut self, raw: &str) -> bool {
        if raw.len() == self.prev_raw_len && raw == self.prev_raw {
            return false;
        }
        self.prev_raw_len = raw.len();
        self.prev_raw = raw.to_owned();

        let all_lines = raw.trim_end_matches('\n').split('\n');
        let new_lines: Vec<String> = all_lines.map(|s| s.to_string()).collect();
        self.lines = new_lines;
        true
    }
}

#[derive(Debug, Clone)]
pub enum AppState {
    Dashboard,
    CreateAgentDialog,
    AgentView(usize),
    RemoveAgentDialog(RemoveAgentState),
    GitViewer(GitViewerState),
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
    GitViewerTick,
}

// ---------------------------------------------------------------------------
// AgentViewState (owned by App, rendered by ui)
// ---------------------------------------------------------------------------

/// Maximum number of lines retained in memory for the agent view.
///
/// At ~150 bytes/line this costs ≈1.5 MB per agent while scrolled (plus
/// another ≈1.5 MB for the change-detection buffer in `AgentViewState`).
/// Going beyond ~20 k starts making the per-tick `capture-pane -S -N`
/// subprocess noticeably heavier.  Note that tmux's own `history-limit`
/// (default 2000) also bounds how much history is actually available;
/// users wanting deep scroll should set `set-option -g history-limit 50000`
/// (or similar) in their tmux.conf.
const MAX_RETAINED_LINES: usize = 10_000;

#[derive(Debug, Default)]
pub struct AgentViewState {
    pub lines: Vec<String>,
    pub last_refresh: Option<std::time::SystemTime>,
    pub show_stopped_overlay: bool,
    /// Number of lines from the bottom of the captured history to offset the
    /// displayed window.  0 = live (bottom) view.  When > 0, the tick uses
    /// `capture_pane_history` to pull scrollback and the renderer shows
    /// `lines[end-scroll-height..end-scroll]` instead of the last N lines.
    pub view_scroll: usize,
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
    /// When true, the next keypress will be forwarded directly to the tmux
    /// pane instead of being intercepted by the app's hotkey handler.
    /// Armed by pressing Ctrl-b; shown as a [PREFIX] indicator in the UI.
    pub prefix_active: bool,
    /// Whether to remove the git worktree when the user presses [d] on the
    /// stopped overlay.  Defaults to `true` when the agent has a worktree,
    /// and can be toggled with Space before confirming.
    pub remove_worktree_on_stop: bool,
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
    CreateWorktree,
}

/// Maximum number of directory suggestions visible at once in the list.
pub const MAX_DIR_VISIBLE: usize = 6;

#[derive(Debug)]
pub struct CreateAgentState {
    pub name: String,
    /// The confirmed base directory (always an existing dir or empty).
    pub directory: String,
    /// The filter prefix the user is currently typing within `directory`.
    pub dir_filter: String,
    pub focus: CreateField,
    pub error: Option<String>,
    /// Alphabetically sorted subdirectory name suggestions (up to 10).
    /// Contains bare directory names, not full paths.
    pub dir_matches: Vec<String>,
    /// Index of the currently highlighted suggestion in `dir_matches`.
    pub dir_selected_idx: usize,
    /// First visible row index for the directory suggestion list (scroll offset).
    pub dir_scroll_offset: usize,
    /// Agent types available when the dialog was opened (from runner discovery).
    pub available_types: Vec<AgentType>,
    /// Index into `available_types` for the currently selected type.
    pub selected_type_idx: usize,
    /// Git repository root discovered for the selected directory.
    /// `None` if the directory is not inside a git repo.
    pub git_repo_root: Option<std::path::PathBuf>,
    /// Whether to create a git worktree for this agent.
    /// Only meaningful (and shown in the UI) when `git_repo_root.is_some()`.
    pub create_worktree: bool,
}

impl Default for CreateAgentState {
    fn default() -> Self {
        Self {
            name: String::new(),
            directory: String::new(),
            dir_filter: String::new(),
            focus: CreateField::Name,
            error: None,
            dir_matches: Vec::new(),
            dir_selected_idx: 0,
            dir_scroll_offset: 0,
            available_types: vec![],
            selected_type_idx: 0,
            git_repo_root: None,
            create_worktree: false,
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

    /// Rebuild the directory suggestion list and re-detect the git repository
    /// root for the current directory.
    ///
    /// Lists non-hidden subdirectories of `self.directory` whose names start
    /// with `self.dir_filter`. Results are sorted alphabetically, capped at 10,
    /// and stored as bare names (not full paths).
    pub fn refresh_dir_matches(&mut self) {
        // Always floor directory at "/" so it is never empty.
        if self.directory.is_empty() {
            self.directory = "/".to_string();
        }
        // For root "/" trimming all slashes gives "" which is not a valid path,
        // so use the directory string as-is when it equals "/".
        let base: &str = if self.directory == "/" {
            "/"
        } else {
            self.directory.trim_end_matches('/')
        };
        let base_path = std::path::Path::new(base);

        if !base_path.is_dir() {
            self.dir_matches.clear();
            self.dir_selected_idx = 0;
            self.dir_scroll_offset = 0;
            return;
        }

        let prefix = &self.dir_filter;
        let mut matches: Vec<String> = std::fs::read_dir(base_path)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().into_string().ok()?;

                if e.file_type().ok()?.is_dir() && name.starts_with(prefix.as_str()) {
                    Some(name)
                } else {
                    None
                }
            })
            .collect();

        matches.sort();
        self.dir_matches = matches;
        self.dir_selected_idx = 0;
        self.dir_scroll_offset = 0;

        // Re-detect git root for the current (confirmed) directory.
        self.detect_git_repo();
    }

    /// Update `git_repo_root` and `create_worktree` based on the currently
    /// confirmed `directory`.
    fn detect_git_repo(&mut self) {
        let path = std::path::Path::new(self.directory.as_str());
        let new_root = crate::git::find_git_root(path);
        match (&self.git_repo_root, &new_root) {
            (None, Some(_)) => {
                // Transitioned into a git repo → enable worktree by default.
                self.create_worktree = true;
            }
            (Some(_), None) => {
                // Left the git repo → disable worktree.
                self.create_worktree = false;
            }
            _ => {} // unchanged
        }
        self.git_repo_root = new_root;
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
    pub git_viewer_state: Option<GitViewerState>,
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
    pub fn new(
        config: Config,
        agents: Vec<AgentEntry>,
        adapters: Vec<Box<dyn AgentAdapter>>,
        runner: AgentRunner,
    ) -> Self {
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
            git_viewer_state: None,
            create_state: CreateAgentState::default(),
            tx,
            rx,
            dirty: true, // force initial draw
            card_scroll: vec![0u16; card_count],
            card_response_heights: vec![0u16; card_count],
            card_response_widths: vec![0u16; card_count],
        }
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

        // GitViewer ticker — 50 ms
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let mut ticker = interval(Duration::from_millis(50));
            loop {
                ticker.tick().await;
                let _ = tx.send(Event::GitViewerTick);
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
            Event::GitViewerTick => {
                self.handle_git_viewer_tick().await;
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

        match &self.state {
            AppState::AgentView(idx) => {
                let idx = *idx;
                self.handle_agent_view_mouse(mouse, idx);
            }
            AppState::GitViewer(gv) => {
                let pane = gv.pane.clone();
                let mouse_active = gv.pane_mouse_active;
                self.handle_pane_mouse_generic(mouse, &pane, mouse_active, false);
            }
            _ => {}
        }
    }

    fn handle_agent_view_mouse(&mut self, mouse: MouseEvent, idx: usize) {
        let is_claude = self
            .agents
            .get(idx)
            .map(|e| matches!(e.config.kind, AgentKind::Claude { .. }))
            .unwrap_or(false);

        match mouse.kind {
            MouseEventKind::ScrollUp => {
                if is_claude {
                    self.agent_view_state.view_scroll = self
                        .agent_view_state
                        .view_scroll
                        .saturating_add(3)
                        .min(MAX_RETAINED_LINES);
                    self.dirty = true;
                } else if let Some(entry) = self.agents.get(idx) {
                    let _ = tmux::send_keys(&entry.config.pane, "PPage");
                }
                return;
            }
            MouseEventKind::ScrollDown => {
                if is_claude {
                    self.agent_view_state.view_scroll =
                        self.agent_view_state.view_scroll.saturating_sub(3);
                    self.dirty = true;
                } else if let Some(entry) = self.agents.get(idx) {
                    let _ = tmux::send_keys(&entry.config.pane, "NPage");
                }
                return;
            }
            _ => {}
        }

        self.handle_pane_mouse_generic(
            mouse,
            &self
                .agents
                .get(idx)
                .map(|e| e.config.pane.clone())
                .unwrap_or_default(),
            self.agent_view_state.pane_mouse_active,
            self.agent_view_state.show_stopped_overlay,
        );
    }

    fn handle_pane_mouse_generic(
        &mut self,
        mouse: MouseEvent,
        pane: &str,
        mouse_active: bool,
        show_overlay: bool,
    ) {
        match mouse.kind {
            MouseEventKind::ScrollUp => {
                let _ = tmux::send_keys(pane, "PPage");
                return;
            }
            MouseEventKind::ScrollDown => {
                let _ = tmux::send_keys(pane, "NPage");
                return;
            }
            _ => {}
        }

        let term_height = crossterm::terminal::size().map(|(_, h)| h).unwrap_or(24);
        if mouse.row >= term_height.saturating_sub(1) {
            return;
        }

        if show_overlay {
            return;
        }

        if mouse.kind == MouseEventKind::Moved && !mouse_active {
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

        let suffix = if press { 'M' } else { 'm' };
        let seq = format!(
            "\x1b[<{};{};{}{}",
            cb,
            mouse.column + 1,
            mouse.row + 1,
            suffix
        );

        let _ = tmux::send_literal(pane, &seq);
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
        match &self.state {
            AppState::AgentView(idx) => {
                let idx = *idx;
                if self.agent_view_state.show_stopped_overlay {
                    return;
                }
                if let Some(entry) = self.agents.get(idx) {
                    let seq = format!("\x1b[200~{}\x1b[201~", text);
                    let _ = tmux::send_literal(&entry.config.pane, &seq);
                }
            }
            AppState::GitViewer(gv) => {
                let pane = gv.pane.clone();
                let seq = format!("\x1b[200~{}\x1b[201~", text);
                let _ = tmux::send_literal(&pane, &seq);
            }
            _ => {}
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
            AppState::RemoveAgentDialog(state) => {
                let state = state.clone();
                self.handle_remove_key(key, state)
            }
            AppState::GitViewer(_) => self.handle_git_viewer_key(key),
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
                let cwd = std::env::current_dir()
                    .unwrap_or_else(|_| std::path::PathBuf::from("."))
                    .to_string_lossy()
                    .to_string();
                let mut cs = CreateAgentState {
                    available_types: self.runner.available_agent_types(),
                    directory: cwd,
                    ..CreateAgentState::default()
                };
                cs.refresh_dir_matches();
                self.create_state = cs;
                self.state = AppState::CreateAgentDialog;
            }
            KeyCode::Char('d') => {
                if !self.agents.is_empty() {
                    let has_worktree = self
                        .agents
                        .get(self.selected)
                        .and_then(|e| e.config.git_repo_root.as_ref())
                        .is_some();
                    self.state = AppState::RemoveAgentDialog(RemoveAgentState {
                        idx: self.selected,
                        remove_worktree: has_worktree,
                    });
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
                    self.agent_view_state.remove_worktree_on_stop = self
                        .agents
                        .get(idx)
                        .and_then(|e| e.config.git_repo_root.as_ref())
                        .is_some();
                    self.dirty = true;
                }
                self.agent_view_state.prev_status = Some(AgentStatus::Stopped.clone());
                if let Some(e) = self.agents.get_mut(idx) {
                    e.meta.status = AgentStatus::Stopped;
                }
                return;
            }

            if let Ok(raw) = if self.agent_view_state.view_scroll > 0 {
                tmux::capture_pane_history(&pane, MAX_RETAINED_LINES)
            } else {
                tmux::capture_pane(&pane)
            } {
                // update_lines returns true only when content changed.
                if self.agent_view_state.update_lines(&raw) {
                    self.dirty = true;
                }
            }

            // Silently clamp view_scroll to the actual available history so
            // that scrolling down responds immediately after the user reaches
            // the top.  We do NOT set dirty here: the renderer already applies
            // the same clamp for display, so nothing visible changes and there
            // is no flicker-inducing extra redraw.
            if self.agent_view_state.view_scroll > 0 {
                let term_h = crossterm::terminal::size()
                    .map(|(_, h)| h as usize)
                    .unwrap_or(24);
                let viewport_h = term_h.saturating_sub(4);
                let max_scroll = self.agent_view_state.lines.len().saturating_sub(viewport_h);
                if self.agent_view_state.view_scroll > max_scroll {
                    self.agent_view_state.view_scroll = max_scroll;
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
                let content_height = term_rows.saturating_sub(4); // reserve top info bar + bottom status bar + border (2 rows)
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
                    self.agent_view_state.remove_worktree_on_stop = self
                        .agents
                        .get(idx)
                        .and_then(|e| e.config.git_repo_root.as_ref())
                        .is_some();
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
        // --- Prefix pass-through ---
        // When prefix_active is true, the next keypress is forwarded directly
        // to the tmux pane and then prefix mode is disarmed.  This allows the
        // user to send Ctrl-g (or any other app-intercepted key) through to the
        // agent by pressing Ctrl-b first.
        if self.agent_view_state.prefix_active {
            self.agent_view_state.prefix_active = false;
            self.dirty = true;
            if let Some(entry) = self.agents.get(idx) {
                let pane = entry.config.pane.clone();
                let keys = key_event_to_tmux(&key);
                if !keys.is_empty() {
                    let _ = tmux::send_keys(&pane, &keys);
                }
            }
            return true;
        }

        if self.agent_view_state.show_stopped_overlay {
            match key.code {
                KeyCode::Char('r') => {
                    self.restart_agent(idx).await;
                    self.agent_view_state.show_stopped_overlay = false;
                    self.dirty = true;
                }
                KeyCode::Char('d') => {
                    let remove_wt = self.agent_view_state.remove_worktree_on_stop;
                    self.remove_agent(idx, remove_wt);
                    self.state = AppState::Dashboard;
                }
                KeyCode::Char(' ') => {
                    // Toggle the "remove worktree" checkbox (only when agent has one)
                    let has_worktree = self
                        .agents
                        .get(idx)
                        .and_then(|e| e.config.git_repo_root.as_ref())
                        .is_some();
                    if has_worktree {
                        self.agent_view_state.remove_worktree_on_stop =
                            !self.agent_view_state.remove_worktree_on_stop;
                        self.dirty = true;
                    }
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
            // Arm prefix mode: next keypress will be forwarded to the pane
            // verbatim, bypassing all app hotkeys.  This lets the user send
            // keys like Ctrl-g to the agent (e.g. Claude Code's editor shortcut)
            // without triggering the stable dashboard switch.
            KeyCode::Char('b') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.agent_view_state.prefix_active = true;
                self.dirty = true;
            }
            KeyCode::Char('g') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.state = AppState::Dashboard;
            }
            KeyCode::Char('v') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.launch_git_viewer(idx);
            }
            KeyCode::PageUp => {
                if let Some(entry) = self.agents.get(idx) {
                    if matches!(entry.config.kind, AgentKind::Claude { .. }) {
                        let page = crossterm::terminal::size()
                            .map(|(_, h)| h as usize)
                            .unwrap_or(24)
                            .saturating_sub(2);
                        self.agent_view_state.view_scroll = self
                            .agent_view_state
                            .view_scroll
                            .saturating_add(page)
                            .min(MAX_RETAINED_LINES);
                        self.dirty = true;
                    } else {
                        let _ = tmux::send_keys(&entry.config.pane, "PPage");
                    }
                }
            }
            KeyCode::PageDown => {
                if let Some(entry) = self.agents.get(idx) {
                    if matches!(entry.config.kind, AgentKind::Claude { .. }) {
                        let page = crossterm::terminal::size()
                            .map(|(_, h)| h as usize)
                            .unwrap_or(24)
                            .saturating_sub(2);
                        self.agent_view_state.view_scroll =
                            self.agent_view_state.view_scroll.saturating_sub(page);
                        self.dirty = true;
                    } else {
                        let _ = tmux::send_keys(&entry.config.pane, "NPage");
                    }
                }
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
    // Git Viewer
    // -----------------------------------------------------------------------

    fn launch_git_viewer(&mut self, agent_idx: usize) {
        let git_viewer = match self.runner.global_config().git_viewer_parts() {
            Some(parts) => parts,
            None => return,
        };

        let directory = match self.agents.get(agent_idx) {
            Some(entry) => entry.config.directory.clone(),
            None => return,
        };

        let window_index = match tmux::new_window(&directory, "git") {
            Ok(idx) => idx,
            Err(_) => return,
        };

        let pane = format!("{}:{}.0", tmux::session_name(), window_index);

        let (program, args) = git_viewer;
        let cmd = if args.is_empty() {
            format!("{}\n", program)
        } else {
            format!("{} {}\n", program, args.join(" "))
        };

        if tmux::send_keys(&pane, &cmd).is_err() {
            let _ = tmux::kill_window(&format!("{}:{}", tmux::session_name(), window_index));
            return;
        }

        self.git_viewer_state = Some(GitViewerState::new(agent_idx, pane.clone()));
        self.state = AppState::GitViewer(self.git_viewer_state.clone().unwrap());
        self.dirty = true;
    }

    fn handle_git_viewer_key(&mut self, key: KeyEvent) -> bool {
        let pane = match &self.state {
            AppState::GitViewer(gv) => gv.pane.clone(),
            _ => return true,
        };

        let prefix_active = match &self.state {
            AppState::GitViewer(gv) => gv.prefix_active,
            _ => false,
        };

        if prefix_active {
            if let AppState::GitViewer(ref mut gv) = self.state {
                gv.prefix_active = false;
            }
            self.dirty = true;
            let keys = key_event_to_tmux(&key);
            if !keys.is_empty() {
                let _ = tmux::send_keys(&pane, &keys);
            }
            return true;
        }

        match key.code {
            KeyCode::Char('b') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let AppState::GitViewer(ref mut gv) = self.state {
                    gv.prefix_active = true;
                }
                self.dirty = true;
            }
            KeyCode::Char('v') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.exit_git_viewer_to_agent();
            }
            KeyCode::Char('g') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.exit_git_viewer_to_dashboard();
            }
            _ => {
                let keys = key_event_to_tmux(&key);
                if !keys.is_empty() {
                    let _ = tmux::send_keys(&pane, &keys);
                }
            }
        }
        true
    }

    fn exit_git_viewer_to_agent(&mut self) {
        let (agent_idx, pane) = match &self.state {
            AppState::GitViewer(gv) => (gv.agent_idx, gv.pane.clone()),
            _ => return,
        };

        if let Some(colon_pos) = pane.find(':') {
            if let Some(dot_pos) = pane[colon_pos..].find('.') {
                let window_target = &pane[..colon_pos + dot_pos];
                let _ = tmux::kill_window(window_target);
            }
        }

        self.agent_view_state = AgentViewState::default();
        self.state = AppState::AgentView(agent_idx);
        self.git_viewer_state = None;
        self.dirty = true;
    }

    fn exit_git_viewer_to_dashboard(&mut self) {
        let pane = match &self.state {
            AppState::GitViewer(gv) => gv.pane.clone(),
            _ => return,
        };

        if let Some(colon_pos) = pane.find(':') {
            if let Some(dot_pos) = pane[colon_pos..].find('.') {
                let window_target = &pane[..colon_pos + dot_pos];
                let _ = tmux::kill_window(window_target);
            }
        }

        self.state = AppState::Dashboard;
        self.git_viewer_state = None;
        self.dirty = true;
    }

    async fn handle_git_viewer_tick(&mut self) {
        let pane = match &self.state {
            AppState::GitViewer(gv) => gv.pane.clone(),
            _ => return,
        };

        if !tmux::is_alive(&pane) {
            self.exit_git_viewer_to_agent();
            return;
        }

        if let Ok(raw) = tmux::capture_pane(&pane) {
            let changed = if let AppState::GitViewer(ref mut gv) = self.state {
                gv.update_lines(&raw)
            } else {
                false
            };
            if changed {
                self.dirty = true;
            }
        }

        let new_cursor = tmux::cursor_position(&pane);
        if let AppState::GitViewer(ref mut gv) = self.state {
            if new_cursor != gv.cursor {
                gv.cursor = new_cursor;
                self.dirty = true;
            }

            gv.pane_mouse_active = tmux::pane_mouse_active(&pane);

            if let Ok((term_cols, term_rows)) = crossterm::terminal::size() {
                let content_height = term_rows.saturating_sub(4); // reserve top info bar + bottom status bar + border (2 rows)
                let desired = (term_cols, content_height);
                if gv.last_pane_size != Some(desired) {
                    let _ = tmux::resize_window(&pane, term_cols, content_height);
                    gv.last_pane_size = Some(desired);
                }
            }
        }
    }

    // -----------------------------------------------------------------------
    // CreateAgentDialog key handler
    // -----------------------------------------------------------------------

    async fn handle_create_key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Esc => {
                self.state = AppState::Dashboard;
            }

            // Tab cycles focus: Name → Directory → [Worktree] → AgentType → Name
            KeyCode::Tab => {
                self.create_state.focus = match self.create_state.focus {
                    CreateField::Name => CreateField::Directory,
                    CreateField::Directory => {
                        if self.create_state.git_repo_root.is_some() {
                            CreateField::CreateWorktree
                        } else if self.create_state.available_types.len() > 1 {
                            CreateField::AgentType
                        } else {
                            CreateField::Name
                        }
                    }
                    CreateField::CreateWorktree => {
                        if self.create_state.available_types.len() > 1 {
                            CreateField::AgentType
                        } else {
                            CreateField::Name
                        }
                    }
                    CreateField::AgentType => CreateField::Name,
                };
            }

            // Up / Down navigate within-field (directory suggestions or agent list)
            KeyCode::Up => match self.create_state.focus {
                CreateField::Directory => {
                    let n = self.create_state.dir_matches.len();
                    if n > 0 {
                        let new_idx = self.create_state.dir_selected_idx.saturating_sub(1);
                        self.create_state.dir_selected_idx = new_idx;
                        // Scroll up if needed
                        if new_idx < self.create_state.dir_scroll_offset {
                            self.create_state.dir_scroll_offset = new_idx;
                        }
                    }
                }
                CreateField::AgentType => {
                    let n = self.create_state.available_types.len();
                    if n > 0 {
                        let idx = self.create_state.selected_type_idx;
                        self.create_state.selected_type_idx = idx.saturating_sub(1);
                    }
                }
                CreateField::Name | CreateField::CreateWorktree => {}
            },
            KeyCode::Down => match self.create_state.focus {
                CreateField::Directory => {
                    let n = self.create_state.dir_matches.len();
                    if n > 0 {
                        let new_idx = (self.create_state.dir_selected_idx + 1).min(n - 1);
                        self.create_state.dir_selected_idx = new_idx;
                        // Scroll down if needed
                        if new_idx >= self.create_state.dir_scroll_offset + MAX_DIR_VISIBLE {
                            self.create_state.dir_scroll_offset = new_idx + 1 - MAX_DIR_VISIBLE;
                        }
                    }
                }
                CreateField::AgentType => {
                    let n = self.create_state.available_types.len();
                    if n > 0 {
                        let idx = self.create_state.selected_type_idx;
                        self.create_state.selected_type_idx = (idx + 1).min(n - 1);
                    }
                }
                CreateField::Name | CreateField::CreateWorktree => {}
            },

            KeyCode::Enter => {
                // When Directory focused: commit the highlighted suggestion name
                if self.create_state.focus == CreateField::Directory {
                    if let Some(name) = self
                        .create_state
                        .dir_matches
                        .get(self.create_state.dir_selected_idx)
                        .cloned()
                    {
                        let base = self
                            .create_state
                            .directory
                            .trim_end_matches('/')
                            .to_string();
                        self.create_state.directory = format!("{}/{}", base, name);
                        self.create_state.dir_filter.clear();
                        self.create_state.refresh_dir_matches();
                    }
                } else if self.create_state.is_valid() {
                    let name = tmux::sanitize_name(&self.create_state.name.clone());
                    let dir = self.create_state.directory.clone();
                    let agent_type = self.create_state.selected_agent_type();
                    let create_worktree = self.create_state.create_worktree
                        && self.create_state.git_repo_root.is_some();
                    let git_repo_root = self
                        .create_state
                        .git_repo_root
                        .as_ref()
                        .map(|p| p.to_string_lossy().to_string());
                    match self
                        .runner
                        .create(
                            &name,
                            &dir,
                            agent_type,
                            create_worktree,
                            git_repo_root.as_deref(),
                        )
                        .await
                    {
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

            KeyCode::Backspace if key.modifiers.contains(KeyModifiers::CONTROL) => {
                // Ctrl+W: delete back to the last word boundary (unix shell style)
                match self.create_state.focus {
                    CreateField::Name => {
                        ctrl_w_delete(&mut self.create_state.name);
                    }
                    CreateField::Directory => {
                        if !self.create_state.dir_filter.is_empty() {
                            self.create_state.dir_filter.clear();
                        } else {
                            ctrl_w_delete_path(&mut self.create_state.directory);
                        }
                        self.create_state.refresh_dir_matches();
                    }
                    CreateField::AgentType | CreateField::CreateWorktree => {}
                }
            }

            KeyCode::Char('w') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                // Ctrl+W: delete back to the last word boundary (unix shell style)
                match self.create_state.focus {
                    CreateField::Name => {
                        ctrl_w_delete(&mut self.create_state.name);
                    }
                    CreateField::Directory => {
                        if !self.create_state.dir_filter.is_empty() {
                            self.create_state.dir_filter.clear();
                        } else {
                            ctrl_w_delete_path(&mut self.create_state.directory);
                        }
                        self.create_state.refresh_dir_matches();
                    }
                    CreateField::AgentType | CreateField::CreateWorktree => {}
                }
            }

            KeyCode::Backspace => {
                match self.create_state.focus {
                    CreateField::Name => {
                        self.create_state.name.pop();
                    }
                    CreateField::Directory => {
                        if !self.create_state.dir_filter.is_empty() {
                            // Delete from the filter first
                            self.create_state.dir_filter.pop();
                        } else {
                            // Filter empty: go up one directory level
                            let d = self
                                .create_state
                                .directory
                                .trim_end_matches('/')
                                .to_string();
                            if let Some(pos) = d.rfind('/') {
                                self.create_state.directory = d[..pos].to_string();
                            } else {
                                // Already at root (e.g. "/foo" with no parent slash after
                                // stripping) — floor to "/" rather than going empty.
                                self.create_state.directory = "/".to_string();
                            }
                        }
                        self.create_state.refresh_dir_matches();
                    }
                    CreateField::AgentType | CreateField::CreateWorktree => {}
                }
            }

            KeyCode::Char(c) => {
                match self.create_state.focus {
                    CreateField::Name => {
                        self.create_state.name.push(c);
                    }
                    CreateField::Directory => {
                        self.create_state.dir_filter.push(c);
                        self.create_state.refresh_dir_matches();
                    }
                    CreateField::AgentType => {}
                    CreateField::CreateWorktree => {
                        // Space handled separately; other chars are no-ops here.
                        if c == ' ' {
                            if self.create_state.git_repo_root.is_some() {
                                self.create_state.create_worktree =
                                    !self.create_state.create_worktree;
                            }
                        }
                    }
                }
            }

            _ => {}
        }
        true
    }

    // -----------------------------------------------------------------------
    // RemoveAgentDialog key handler
    // -----------------------------------------------------------------------

    fn handle_remove_key(&mut self, key: KeyEvent, state: RemoveAgentState) -> bool {
        match key.code {
            KeyCode::Char('y') | KeyCode::Enter => {
                self.remove_agent(state.idx, state.remove_worktree);
                self.state = AppState::Dashboard;
            }
            KeyCode::Char('n') | KeyCode::Esc => {
                self.state = AppState::Dashboard;
            }
            // Space toggles the "remove worktree" checkbox (only when agent has one)
            KeyCode::Char(' ') => {
                let has_worktree = self
                    .agents
                    .get(state.idx)
                    .and_then(|e| e.config.git_repo_root.as_ref())
                    .is_some();
                if has_worktree {
                    if let AppState::RemoveAgentDialog(ref mut s) = self.state {
                        s.remove_worktree = !s.remove_worktree;
                    }
                }
            }
            _ => {}
        }
        true
    }

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    fn remove_agent(&mut self, idx: usize, remove_worktree: bool) {
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

                // Remove the git worktree if requested and present.
                if remove_worktree {
                    if let (Some(wt_path), Some(repo_root)) = (
                        Some(agent_config.directory.as_str()),
                        agent_config.git_repo_root.as_deref(),
                    ) {
                        let branch = crate::git::sanitize_branch_name(&agent_config.name);
                        // Non-fatal: log error but continue removal.
                        if let Err(e) = crate::git::remove_worktree(
                            std::path::Path::new(repo_root),
                            std::path::Path::new(wt_path),
                            &branch,
                            true,
                        ) {
                            // Surface in the UI via a best-effort approach.
                            // We cannot show a dialog here since we're already
                            // tearing down — just emit to stderr.
                            eprintln!("warning: failed to remove git worktree: {}", e);
                        }
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
                    entry.meta.status = AgentStatus::Idle;
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
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);

    // Each arm yields (base_name, apply_shift_prefix).
    // Char keys encode Shift in the character value (upper/lowercase), and
    // BackTab encodes Shift implicitly, so neither gets an S- prefix.
    let (base, apply_shift): (String, bool) = match key.code {
        KeyCode::BackTab => ("BTab".into(), false),
        KeyCode::Char(c) => (c.to_string(), false),
        KeyCode::Enter => ("Enter".into(), true),
        KeyCode::Backspace => ("BSpace".into(), true),
        KeyCode::Tab => ("Tab".into(), true),
        KeyCode::Esc => ("Escape".into(), true),
        KeyCode::Left => ("Left".into(), true),
        KeyCode::Right => ("Right".into(), true),
        KeyCode::Up => ("Up".into(), true),
        KeyCode::Down => ("Down".into(), true),
        KeyCode::PageUp => ("PPage".into(), true),
        KeyCode::PageDown => ("NPage".into(), true),
        KeyCode::Home => ("Home".into(), true),
        KeyCode::End => ("End".into(), true),
        KeyCode::Delete => ("DC".into(), true),
        _ => return String::new(),
    };

    let mut result = String::new();
    if ctrl {
        result.push_str("C-");
    }
    if alt {
        result.push_str("M-");
    }
    if shift && apply_shift {
        result.push_str("S-");
    }
    result.push_str(&base);
    result
}

// ---------------------------------------------------------------------------
// Ctrl+W helpers
// ---------------------------------------------------------------------------

/// Deletes the last "word" from a generic string (space-delimited).
fn ctrl_w_delete(s: &mut String) {
    // Trim trailing spaces, then remove back to the next space
    let trimmed_len = s.trim_end().len();
    s.truncate(trimmed_len);
    if let Some(pos) = s.rfind(|c: char| c == ' ') {
        s.truncate(pos + 1);
    } else {
        s.clear();
    }
}

/// Deletes the last path component from a directory string.
/// Behaves like Ctrl+W in a unix shell: removes back to the last `/`.
fn ctrl_w_delete_path(s: &mut String) {
    // Strip trailing slash first, then remove back to the previous slash
    let trimmed = s.trim_end_matches('/');
    if let Some(pos) = trimmed.rfind('/') {
        s.truncate(pos + 1); // keep the slash
    } else {
        // Already at the top — floor to root rather than clearing.
        *s = "/".to_string();
    }
}

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::time::{Duration, interval};

use crate::agents::AgentAdapter;
use crate::config::{AgentKind, Config, DEFAULT_PROJECT_NAME, MAX_PROJECTS};
use crate::global_config::WorktreeDirectoryPreset;
use crate::host_terminal::HostColors;
use crate::models::{AgentEntry, AgentMeta, AgentStatus, AgentStatusCounts, AgentType};
use crate::runner::AgentRunner;
use crate::tmux;
use crate::ui::dashboard::{PROJECT_TABS_HEIGHT, grid_layout};

// ---------------------------------------------------------------------------
// StatusNotification — blink tracking for status bar
// ---------------------------------------------------------------------------

#[derive(Default)]
pub struct StatusNotification {
    pub prev_running: usize,
    pub prev_waiting: usize,
    pub running_blink: Option<std::time::Instant>,
    pub waiting_blink: Option<std::time::Instant>,
    pub initialized: bool,
}

const BLINK_DURATION: std::time::Duration = std::time::Duration::from_secs(3);
const BLINK_INTERVAL_MS: u128 = 500;
const PANE_CHROME_HEIGHT: u16 = 4;
const PANE_BORDER_WIDTH: u16 = 2;
const MOUSE_WHEEL_SCROLL_LINES: usize = 3;

impl StatusNotification {
    pub fn reset(&mut self, counts: AgentStatusCounts) {
        self.prev_running = counts.running;
        self.prev_waiting = counts.waiting;
        self.running_blink = None;
        self.waiting_blink = None;
        self.initialized = true;
    }

    pub fn observe(&mut self, counts: AgentStatusCounts) {
        if self.initialized {
            let running_decrease = self.prev_running.saturating_sub(counts.running);
            let waiting_increase = counts.waiting.saturating_sub(self.prev_waiting);

            if running_decrease > waiting_increase {
                self.running_blink = Some(std::time::Instant::now());
            }
            if counts.waiting > self.prev_waiting {
                self.waiting_blink = Some(std::time::Instant::now());
            }
        }

        self.prev_running = counts.running;
        self.prev_waiting = counts.waiting;
        self.initialized = true;
    }

    pub fn is_blinking_running(&self) -> bool {
        self.running_blink
            .map(|t| t.elapsed() < BLINK_DURATION)
            .unwrap_or(false)
    }

    pub fn is_blinking_waiting(&self) -> bool {
        self.waiting_blink
            .map(|t| t.elapsed() < BLINK_DURATION)
            .unwrap_or(false)
    }

    pub fn blink_phase(start: std::time::Instant) -> bool {
        (start.elapsed().as_millis() / BLINK_INTERVAL_MS).is_multiple_of(2)
    }

    pub fn should_render_blink_running(&self) -> bool {
        self.running_blink
            .map(|t| t.elapsed() < BLINK_DURATION && Self::blink_phase(t))
            .unwrap_or(false)
    }

    pub fn should_render_blink_waiting(&self) -> bool {
        self.waiting_blink
            .map(|t| t.elapsed() < BLINK_DURATION && Self::blink_phase(t))
            .unwrap_or(false)
    }
}

fn tmux_pane_viewport_size(term_cols: u16, term_rows: u16) -> (u16, u16) {
    (
        term_cols.saturating_sub(PANE_BORDER_WIDTH),
        term_rows.saturating_sub(PANE_CHROME_HEIGHT),
    )
}

// ---------------------------------------------------------------------------
// AppState
// ---------------------------------------------------------------------------

/// State carried by the remove-agent confirmation dialog.
#[derive(Debug, Clone)]
pub struct RemoveAgentState {
    pub idx: usize,
    pub remove_worktree: bool,
    pub stop_agent: bool,
    pub focus: usize,
}

#[derive(Debug, Clone, Default)]
pub struct CreateProjectState {
    pub name: String,
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct RemoveProjectState {
    pub idx: usize,
    pub name: String,
    pub agent_count: usize,
    pub confirm_remove_agents: bool,
}

/// State for the git viewer pane view.
#[derive(Debug, Clone)]
pub struct GitViewerState {
    /// Index of the agent we came from (to return to on exit).
    pub agent_idx: usize,
    /// tmux pane target (e.g. "flowmux:5.0").
    pub pane: String,
    /// Captured pane output lines.
    pub lines: Vec<String>,
    /// Number of lines scrolled up from the live bottom of the pane.
    pub view_scroll: usize,
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

/// State for the persistent terminal view.
#[derive(Debug, Clone)]
pub struct TerminalViewState {
    /// Index of the agent we came from (to return to on exit).
    pub agent_idx: usize,
    /// tmux pane target (e.g. "flowmux:5.0").
    pub pane: String,
    /// Captured pane output lines.
    pub lines: Vec<String>,
    /// Number of lines scrolled up from the live bottom of the pane.
    pub view_scroll: usize,
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

impl TerminalViewState {
    pub fn new(agent_idx: usize, pane: String) -> Self {
        Self {
            agent_idx,
            pane,
            lines: Vec::new(),
            view_scroll: 0,
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
        let start = new_lines.len().saturating_sub(MAX_RETAINED_LINES);
        self.lines = new_lines[start..].to_vec();
        true
    }
}

impl GitViewerState {
    pub fn new(agent_idx: usize, pane: String) -> Self {
        Self {
            agent_idx,
            pane,
            lines: Vec::new(),
            view_scroll: 0,
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
        let start = new_lines.len().saturating_sub(MAX_RETAINED_LINES);
        self.lines = new_lines[start..].to_vec();
        true
    }
}

#[derive(Debug, Clone)]
pub enum AppState {
    Dashboard,
    CreateAgentDialog,
    CreateProjectDialog,
    AgentView(usize),
    RemoveAgentDialog(RemoveAgentState),
    RemoveProjectDialog(RemoveProjectState),
    GitViewer(GitViewerState),
    TerminalView(TerminalViewState),
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
    TerminalViewTick,
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
    CreateWorktree,
    CopyDirectories,
    SymlinkDirectories,
    AgentType,
}

/// Maximum number of directory suggestions visible at once in the list.
pub const MAX_DIR_VISIBLE: usize = 6;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct RelativeDirSelector {
    /// Confirmed relative subdirectories selected by the user, stored without
    /// the leading "./". The empty list means nothing will be copied/symlinked.
    pub selected_dirs: Vec<String>,
    /// Current candidate path relative to the selected base directory.
    /// Empty means "./".
    pub current_dir: String,
    /// Filter prefix for the next path component under `current_dir`.
    pub filter: String,
    /// Alphabetically sorted subdirectory suggestions under `current_dir`.
    pub matches: Vec<String>,
    pub selected_idx: usize,
    pub scroll_offset: usize,
}

impl RelativeDirSelector {
    pub fn current_display(&self) -> String {
        if self.current_dir.is_empty() {
            "./".to_string()
        } else {
            format!("./{}/", self.current_dir)
        }
    }

    pub fn current_candidate(&self) -> Option<String> {
        if self.current_dir.is_empty() {
            None
        } else {
            Some(self.current_dir.clone())
        }
    }

    pub fn is_at_root(&self) -> bool {
        self.current_dir.is_empty() && self.filter.is_empty()
    }

    pub fn clear_all(&mut self) {
        self.selected_dirs.clear();
        self.reset_candidate();
    }

    pub fn reset_candidate(&mut self) {
        self.current_dir.clear();
        self.filter.clear();
        self.matches.clear();
        self.selected_idx = 0;
        self.scroll_offset = 0;
    }

    pub fn refresh_matches(&mut self, base_dir: &str) {
        let search_dir = selector_search_dir(base_dir, &self.current_dir);
        self.matches = list_subdirectories(&search_dir, &self.filter);
        self.selected_idx = 0;
        self.scroll_offset = 0;
    }

    pub fn descend(&mut self) -> bool {
        let Some(name) = self.matches.get(self.selected_idx).cloned() else {
            return false;
        };
        if self.current_dir.is_empty() {
            self.current_dir = name;
        } else {
            self.current_dir.push('/');
            self.current_dir.push_str(&name);
        }
        self.filter.clear();
        true
    }

    pub fn navigate_up(&mut self) -> bool {
        if self.current_dir.is_empty() {
            return false;
        }
        if let Some((parent, _)) = self.current_dir.rsplit_once('/') {
            self.current_dir = parent.to_string();
        } else {
            self.current_dir.clear();
        }
        true
    }
}

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
    pub copy_directories_enabled: bool,
    pub symlink_directories_enabled: bool,
    pub copy_directories: RelativeDirSelector,
    pub symlink_directories: RelativeDirSelector,
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
            copy_directories_enabled: false,
            symlink_directories_enabled: false,
            copy_directories: RelativeDirSelector::default(),
            symlink_directories: RelativeDirSelector::default(),
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
        !self.name.trim().is_empty()
            && !self.directory.trim().is_empty()
            && !self.available_types.is_empty()
    }

    pub fn worktree_selectors_visible(&self) -> bool {
        self.git_repo_root.is_some() && self.create_worktree
    }

    pub fn selector_enabled(&self, field: &CreateField) -> bool {
        match field {
            CreateField::CopyDirectories => self.copy_directories_enabled,
            CreateField::SymlinkDirectories => self.symlink_directories_enabled,
            _ => false,
        }
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

        self.dir_matches = list_subdirectories(base_path, &self.dir_filter);
        self.dir_selected_idx = 0;
        self.dir_scroll_offset = 0;

        // Re-detect git root for the current (confirmed) directory.
        self.detect_git_repo();
        self.clear_worktree_selections();
    }

    pub fn refresh_worktree_selector_matches(&mut self) {
        let base_dir = self.directory.clone();
        self.copy_directories.refresh_matches(&base_dir);
        self.symlink_directories.refresh_matches(&base_dir);
    }

    fn clear_worktree_selections(&mut self) {
        self.copy_directories_enabled = false;
        self.symlink_directories_enabled = false;
        self.copy_directories.clear_all();
        self.symlink_directories.clear_all();
    }

    fn selector_mut(&mut self, field: &CreateField) -> Option<&mut RelativeDirSelector> {
        match field {
            CreateField::CopyDirectories => Some(&mut self.copy_directories),
            CreateField::SymlinkDirectories => Some(&mut self.symlink_directories),
            _ => None,
        }
    }

    fn selector_ref(&self, field: &CreateField) -> Option<&RelativeDirSelector> {
        match field {
            CreateField::CopyDirectories => Some(&self.copy_directories),
            CreateField::SymlinkDirectories => Some(&self.symlink_directories),
            _ => None,
        }
    }

    fn selector_label(field: &CreateField) -> Option<&'static str> {
        match field {
            CreateField::CopyDirectories => Some("Copy directories"),
            CreateField::SymlinkDirectories => Some("Symlink directories"),
            _ => None,
        }
    }

    fn selector_conflicts_with_other(&self, field: &CreateField, candidate: &str) -> bool {
        match field {
            CreateField::CopyDirectories => self
                .symlink_directories
                .selected_dirs
                .iter()
                .any(|p| p == candidate),
            CreateField::SymlinkDirectories => self
                .copy_directories
                .selected_dirs
                .iter()
                .any(|p| p == candidate),
            _ => false,
        }
    }

    fn commit_selector_candidate(
        &mut self,
        field: &CreateField,
    ) -> std::result::Result<bool, String> {
        let Some(candidate) = self
            .selector_ref(field)
            .and_then(RelativeDirSelector::current_candidate)
        else {
            return Ok(false);
        };

        if self
            .selector_ref(field)
            .is_some_and(|selector| selector.selected_dirs.iter().any(|p| p == &candidate))
        {
            let label = Self::selector_label(field).unwrap_or("directories");
            return Err(format!("{} already contains ./{}", label, candidate));
        }
        if self.selector_conflicts_with_other(field, &candidate) {
            return Err(format!(
                "Directory ./{} cannot be both copied and symlinked",
                candidate
            ));
        }

        let base_dir = self.directory.clone();
        if let Some(selector) = self.selector_mut(field) {
            selector.selected_dirs.push(candidate);
            selector.reset_candidate();
            selector.refresh_matches(&base_dir);
        }
        Ok(true)
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

fn list_subdirectories(base_path: &std::path::Path, prefix: &str) -> Vec<String> {
    let mut matches: Vec<String> = std::fs::read_dir(base_path)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;

            if e.file_type().ok()?.is_dir() && name.starts_with(prefix) {
                Some(name)
            } else {
                None
            }
        })
        .collect();
    matches.sort();
    matches
}

fn selector_search_dir(base_dir: &str, current_dir: &str) -> std::path::PathBuf {
    let mut path = std::path::PathBuf::from(base_dir);
    if !current_dir.is_empty() {
        path.push(current_dir);
    }
    path
}

// ---------------------------------------------------------------------------
// App
// ---------------------------------------------------------------------------

pub struct App {
    pub agents: Vec<AgentEntry>,
    pub adapters: Vec<Box<dyn AgentAdapter>>,
    pub state: AppState,
    pub active_project_idx: usize,
    pub selected: usize,
    pub config: Config,
    pub runner: AgentRunner,
    pub agent_view_state: AgentViewState,
    pub git_viewer_state: Option<GitViewerState>,
    pub terminal_view_state: Option<TerminalViewState>,
    pub terminal_panes: std::collections::HashMap<usize, String>,
    pub create_state: CreateAgentState,
    pub create_project_state: CreateProjectState,
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
    /// Host terminal default colors (fg/bg), probed once at startup via OSC 10/11.
    /// Used as the default bg/fg for ghostty cells without explicit colors.
    pub host_colors: HostColors,
    pub notification: StatusNotification,
}

impl App {
    pub fn new(
        config: Config,
        agents: Vec<AgentEntry>,
        adapters: Vec<Box<dyn AgentAdapter>>,
        runner: AgentRunner,
        host_colors: HostColors,
    ) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let card_count = agents.len();
        let mut app = Self {
            agents,
            adapters,
            state: AppState::Dashboard,
            active_project_idx: 0,
            selected: 0,
            config,
            runner,
            agent_view_state: AgentViewState::default(),
            git_viewer_state: None,
            terminal_view_state: None,
            terminal_panes: std::collections::HashMap::new(),
            create_state: CreateAgentState::default(),
            create_project_state: CreateProjectState::default(),
            tx,
            rx,
            dirty: true, // force initial draw
            card_scroll: vec![0u16; card_count],
            card_response_heights: vec![0u16; card_count],
            card_response_widths: vec![0u16; card_count],
            host_colors,
            notification: StatusNotification::default(),
        };
        app.ensure_project_selection();
        app.reset_project_notification();
        app
    }

    pub fn active_project_name(&self) -> &str {
        self.config
            .projects
            .get(self.active_project_idx)
            .map(String::as_str)
            .unwrap_or(DEFAULT_PROJECT_NAME)
    }

    fn load_create_state_worktree_presets(&mut self) {
        let Some(repo_root) = self.create_state.git_repo_root.as_ref() else {
            self.create_state.clear_worktree_selections();
            return;
        };

        let key = repo_root.to_string_lossy().to_string();
        let preset = self
            .runner
            .global_config()
            .worktree_directory_presets
            .get(&key)
            .cloned()
            .unwrap_or_default();

        self.create_state.copy_directories_enabled = !preset.copy_directories.is_empty();
        self.create_state.symlink_directories_enabled = !preset.symlink_directories.is_empty();
        self.create_state.copy_directories.selected_dirs = preset.copy_directories;
        self.create_state.symlink_directories.selected_dirs = preset.symlink_directories;
        self.create_state.copy_directories.reset_candidate();
        self.create_state.symlink_directories.reset_candidate();
    }

    fn persist_create_state_worktree_presets(&mut self) {
        let Some(repo_root) = self.create_state.git_repo_root.as_ref() else {
            return;
        };

        let key = repo_root.to_string_lossy().to_string();
        let preset = WorktreeDirectoryPreset {
            copy_directories: if self.create_state.copy_directories_enabled {
                self.create_state.copy_directories.selected_dirs.clone()
            } else {
                Vec::new()
            },
            symlink_directories: if self.create_state.symlink_directories_enabled {
                self.create_state.symlink_directories.selected_dirs.clone()
            } else {
                Vec::new()
            },
        };

        let global_config = self.runner.global_config_mut();
        if preset.copy_directories.is_empty() && preset.symlink_directories.is_empty() {
            global_config.worktree_directory_presets.remove(&key);
        } else {
            global_config.worktree_directory_presets.insert(key, preset);
        }
        let _ = global_config.save();
    }

    pub fn visible_agent_indices(&self) -> Vec<usize> {
        visible_agent_indices_for_project(&self.agents, self.active_project_name())
    }

    pub fn active_project_status_counts(&self) -> AgentStatusCounts {
        AgentStatusCounts::for_project(&self.agents, self.active_project_name())
    }

    fn reset_project_notification(&mut self) {
        let counts = self.active_project_status_counts();
        self.notification.reset(counts);
    }

    fn selected_visible_position(&self, visible_indices: &[usize]) -> Option<usize> {
        visible_indices.iter().position(|&idx| idx == self.selected)
    }

    fn ensure_project_selection(&mut self) {
        let visible_indices = self.visible_agent_indices();
        if visible_indices.is_empty() {
            self.selected = self.selected.min(self.agents.len().saturating_sub(1));
            return;
        }

        if !visible_indices.contains(&self.selected) {
            self.selected = visible_indices[0];
        }
    }

    fn set_active_project_idx(&mut self, idx: usize) {
        if idx >= self.config.projects.len() {
            return;
        }
        let project_changed = self.active_project_idx != idx;
        self.active_project_idx = idx;
        self.ensure_project_selection();
        if project_changed {
            self.reset_project_notification();
        }
        self.dirty = true;
    }

    fn cycle_projects(&mut self) {
        if self.config.projects.is_empty() {
            return;
        }
        let next = (self.active_project_idx + 1) % self.config.projects.len();
        self.set_active_project_idx(next);
    }

    fn switch_to_project_by_digit(&mut self, digit: char) {
        let idx = match digit {
            '1'..='9' => digit as usize - '1' as usize,
            '0' => 9,
            _ => return,
        };
        if idx < self.config.projects.len() {
            self.set_active_project_idx(idx);
        }
    }

    fn current_project_agent_count(&self) -> usize {
        self.agents
            .iter()
            .filter(|entry| entry.config.project == self.active_project_name())
            .count()
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

        // TerminalView ticker — 50 ms
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let mut ticker = interval(Duration::from_millis(50));
            loop {
                ticker.tick().await;
                let _ = tx.send(Event::TerminalViewTick);
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
                if self.notification.is_blinking_running()
                    || self.notification.is_blinking_waiting()
                {
                    self.dirty = true;
                }
                true
            }
            Event::GitViewerTick => {
                self.handle_git_viewer_tick().await;
                if self.notification.is_blinking_running()
                    || self.notification.is_blinking_waiting()
                {
                    self.dirty = true;
                }
                true
            }
            Event::TerminalViewTick => {
                self.handle_terminal_view_tick().await;
                if self.notification.is_blinking_running()
                    || self.notification.is_blinking_waiting()
                {
                    self.dirty = true;
                }
                true
            }
        }
    }

    /// Returns the dashboard card slot index (into `self.agents`) for a given
    /// visible dashboard slot index for a given terminal cell `(col, row)`, or
    /// `None` if the position is out of bounds.
    fn dashboard_slot_at(&self, col: u16, row: u16) -> Option<usize> {
        let visible_indices = self.visible_agent_indices();
        let n = visible_indices.len();
        if n == 0 {
            return None;
        }
        let (term_w, term_h) = crossterm::terminal::size().unwrap_or((80, 24));
        if row >= term_h.saturating_sub(1) {
            return None;
        }
        if row < PROJECT_TABS_HEIGHT {
            return None;
        }
        let main_h = term_h.saturating_sub(1).saturating_sub(PROJECT_TABS_HEIGHT);
        let (cols, rows) = grid_layout(n);
        let cell_w = term_w / cols as u16;
        let cell_h = main_h / rows as u16;
        if cell_w == 0 || cell_h == 0 {
            return None;
        }
        let c = (col / cell_w).min(cols as u16 - 1) as usize;
        let grid_row = row.saturating_sub(PROJECT_TABS_HEIGHT);
        let r = (grid_row / cell_h).min(rows as u16 - 1) as usize;
        let slot = r * cols + c;
        if slot < n { Some(slot) } else { None }
    }

    fn handle_dashboard_mouse(&mut self, mouse: MouseEvent) {
        match mouse.kind {
            MouseEventKind::Down(_) => {
                if let Some(slot) = self.dashboard_slot_at(mouse.column, mouse.row) {
                    if let Some(global_idx) = self.visible_agent_indices().get(slot).copied() {
                        self.selected = global_idx;
                    }
                    self.dirty = true;
                }
            }
            MouseEventKind::ScrollUp => {
                if let Some(slot) = self.dashboard_slot_at(mouse.column, mouse.row) {
                    if let Some(global_idx) = self.visible_agent_indices().get(slot).copied() {
                        if let Some(s) = self.card_scroll.get_mut(global_idx) {
                            *s = s.saturating_sub(1);
                            self.dirty = true;
                        }
                    }
                }
            }
            MouseEventKind::ScrollDown => {
                if let Some(slot) = self.dashboard_slot_at(mouse.column, mouse.row) {
                    let Some(global_idx) = self.visible_agent_indices().get(slot).copied() else {
                        return;
                    };
                    let viewport_h = self
                        .card_response_heights
                        .get(global_idx)
                        .copied()
                        .unwrap_or(1)
                        .max(1);
                    let content_w = self
                        .card_response_widths
                        .get(global_idx)
                        .copied()
                        .unwrap_or(80)
                        .max(1);
                    let max_scroll = self
                        .agents
                        .get(global_idx)
                        .and_then(|e| e.meta.last_model_response.as_deref())
                        .map(|r| {
                            let text = tui_markdown::from_str(r);
                            let total = wrapped_line_count(&text, content_w);
                            total.saturating_sub(viewport_h)
                        })
                        .unwrap_or(0);
                    if let Some(s) = self.card_scroll.get_mut(global_idx) {
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
                if !pane_handles_own_scroll(mouse_active) {
                    match mouse.kind {
                        MouseEventKind::ScrollUp => {
                            if let AppState::GitViewer(ref mut gv) = self.state {
                                gv.view_scroll = gv
                                    .view_scroll
                                    .saturating_add(MOUSE_WHEEL_SCROLL_LINES)
                                    .min(MAX_RETAINED_LINES);
                            }
                            self.dirty = true;
                            return;
                        }
                        MouseEventKind::ScrollDown => {
                            if let AppState::GitViewer(ref mut gv) = self.state {
                                gv.view_scroll =
                                    gv.view_scroll.saturating_sub(MOUSE_WHEEL_SCROLL_LINES);
                            }
                            self.dirty = true;
                            return;
                        }
                        MouseEventKind::Down(_) => {
                            if let AppState::GitViewer(ref mut gv) = self.state {
                                gv.view_scroll = 0;
                            }
                        }
                        _ => {}
                    }
                }
                self.handle_pane_mouse_generic(mouse, &pane, mouse_active, false);
            }
            AppState::TerminalView(tv) => {
                let pane = tv.pane.clone();
                let mouse_active = tv.pane_mouse_active;
                if !pane_handles_own_scroll(mouse_active) {
                    match mouse.kind {
                        MouseEventKind::ScrollUp => {
                            if let AppState::TerminalView(ref mut tv) = self.state {
                                tv.view_scroll = tv
                                    .view_scroll
                                    .saturating_add(MOUSE_WHEEL_SCROLL_LINES)
                                    .min(MAX_RETAINED_LINES);
                            }
                            self.dirty = true;
                            return;
                        }
                        MouseEventKind::ScrollDown => {
                            if let AppState::TerminalView(ref mut tv) = self.state {
                                tv.view_scroll =
                                    tv.view_scroll.saturating_sub(MOUSE_WHEEL_SCROLL_LINES);
                            }
                            self.dirty = true;
                            return;
                        }
                        MouseEventKind::Down(_) => {
                            if let AppState::TerminalView(ref mut tv) = self.state {
                                tv.view_scroll = 0;
                            }
                        }
                        _ => {}
                    }
                }
                self.handle_pane_mouse_generic(mouse, &pane, mouse_active, false);
            }
            _ => {}
        }
    }

    fn handle_agent_view_mouse(&mut self, mouse: MouseEvent, idx: usize) {
        let uses_captured_scrollback = self
            .agents
            .get(idx)
            .map(|e| uses_captured_scrollback(&e.config.kind))
            .unwrap_or(false);

        match mouse.kind {
            MouseEventKind::ScrollUp => {
                if uses_captured_scrollback {
                    self.agent_view_state.view_scroll = self
                        .agent_view_state
                        .view_scroll
                        .saturating_add(3)
                        .min(MAX_RETAINED_LINES);
                    self.dirty = true;
                } else if let Some(entry) = self.agents.get(idx) {
                    let pane = entry.config.pane.clone();
                    if pane_handles_own_scroll(self.agent_view_state.pane_mouse_active) {
                        self.forward_mouse_to_pane(
                            mouse,
                            &pane,
                            false,
                            self.agent_view_state.pane_mouse_active,
                        );
                    } else {
                        let _ = tmux::scroll_lines_up(&pane, MOUSE_WHEEL_SCROLL_LINES);
                    }
                }
                return;
            }
            MouseEventKind::ScrollDown => {
                if uses_captured_scrollback {
                    self.agent_view_state.view_scroll =
                        self.agent_view_state.view_scroll.saturating_sub(3);
                    self.dirty = true;
                } else if let Some(entry) = self.agents.get(idx) {
                    let pane = entry.config.pane.clone();
                    if pane_handles_own_scroll(self.agent_view_state.pane_mouse_active) {
                        self.forward_mouse_to_pane(
                            mouse,
                            &pane,
                            false,
                            self.agent_view_state.pane_mouse_active,
                        );
                    } else {
                        let _ = tmux::scroll_lines_down(&pane, MOUSE_WHEEL_SCROLL_LINES);
                    }
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
                if pane_handles_own_scroll(mouse_active) {
                    self.forward_mouse_to_pane(mouse, pane, show_overlay, mouse_active);
                } else {
                    let _ = tmux::scroll_lines_up(pane, MOUSE_WHEEL_SCROLL_LINES);
                }
                return;
            }
            MouseEventKind::ScrollDown => {
                if pane_handles_own_scroll(mouse_active) {
                    self.forward_mouse_to_pane(mouse, pane, show_overlay, mouse_active);
                } else {
                    let _ = tmux::scroll_lines_down(pane, MOUSE_WHEEL_SCROLL_LINES);
                }
                return;
            }
            _ => {}
        }

        self.forward_mouse_to_pane(mouse, pane, show_overlay, mouse_active);
    }

    fn forward_mouse_to_pane(
        &mut self,
        mouse: MouseEvent,
        pane: &str,
        show_overlay: bool,
        mouse_active: bool,
    ) {
        let Some(seq) = mouse_event_to_sgr(mouse, show_overlay) else {
            return;
        };

        let term_height = crossterm::terminal::size().map(|(_, h)| h).unwrap_or(24);
        if mouse.row >= term_height.saturating_sub(1) {
            return;
        }

        if mouse.kind == MouseEventKind::Moved && !mouse_active {
            return;
        }

        let _ = tmux::send_literal(pane, &seq);
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
            AppState::CreateProjectDialog => self.handle_create_project_key(key),
            AppState::RemoveAgentDialog(state) => {
                let state = state.clone();
                self.handle_remove_key(key, state).await
            }
            AppState::RemoveProjectDialog(state) => {
                let state = state.clone();
                self.handle_remove_project_key(key, state).await
            }
            AppState::GitViewer(_) => self.handle_git_viewer_key(key),
            AppState::TerminalView(_) => self.handle_terminal_view_key(key),
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

    fn move_visible_card(&mut self, visible_indices: &[usize], target_visible_idx: usize) {
        let Some(target) = visible_indices.get(target_visible_idx).copied() else {
            return;
        };
        self.move_card(target);
    }

    fn handle_dashboard_key(&mut self, key: KeyEvent) -> bool {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let visible_indices = self.visible_agent_indices();
        let selected_visible = self.selected_visible_position(&visible_indices);
        match key.code {
            KeyCode::Char('q') => return false,
            KeyCode::Tab => self.cycle_projects(),
            KeyCode::Char(digit @ ('0'..='9')) => self.switch_to_project_by_digit(digit),
            KeyCode::Char('n') => {
                let available = self.runner.available_agent_types();
                if available.is_empty() {
                    return true;
                }
                let cwd = std::env::current_dir()
                    .unwrap_or_else(|_| std::path::PathBuf::from("."))
                    .to_string_lossy()
                    .to_string();
                let mut cs = CreateAgentState {
                    available_types: available,
                    directory: cwd,
                    ..CreateAgentState::default()
                };
                cs.refresh_dir_matches();
                self.create_state = cs;
                self.load_create_state_worktree_presets();
                self.state = AppState::CreateAgentDialog;
            }
            KeyCode::Char('p') => {
                self.create_project_state = CreateProjectState::default();
                self.state = AppState::CreateProjectDialog;
            }
            KeyCode::Char('d') if ctrl => {
                if self.active_project_name() != DEFAULT_PROJECT_NAME {
                    self.state = AppState::RemoveProjectDialog(RemoveProjectState {
                        idx: self.active_project_idx,
                        name: self.active_project_name().to_string(),
                        agent_count: self.current_project_agent_count(),
                        confirm_remove_agents: false,
                    });
                }
            }
            KeyCode::Char('d') => {
                if let Some(idx) = selected_visible
                    .and_then(|pos| visible_indices.get(pos))
                    .copied()
                {
                    let has_worktree = self
                        .agents
                        .get(idx)
                        .and_then(|e| e.config.git_repo_root.as_ref())
                        .is_some();
                    self.state = AppState::RemoveAgentDialog(RemoveAgentState {
                        idx,
                        remove_worktree: has_worktree,
                        stop_agent: true,
                        focus: 0,
                    });
                }
            }
            KeyCode::Enter => {
                if let Some(idx) = selected_visible
                    .and_then(|pos| visible_indices.get(pos))
                    .copied()
                {
                    self.agent_view_state = AgentViewState::default();
                    self.state = AppState::AgentView(idx);
                }
            }
            // ---------------------------------------------------------------
            // Card movement: Ctrl+arrows / Ctrl+hjkl
            // ---------------------------------------------------------------
            KeyCode::Left if ctrl => {
                if let Some(selected_pos) = selected_visible {
                    let (cols, _) = grid_layout(visible_indices.len());
                    // Mirror navigate-left wrapping: not at leftmost col OR
                    // not on first row (wrap to last slot of previous row).
                    if selected_pos % cols > 0 || selected_pos >= cols {
                        self.move_visible_card(&visible_indices, selected_pos - 1);
                    }
                }
            }
            KeyCode::Right if ctrl => {
                if let Some(selected_pos) = selected_visible {
                    // Mirror navigate-right wrapping: any next card exists.
                    if selected_pos + 1 < visible_indices.len() {
                        self.move_visible_card(&visible_indices, selected_pos + 1);
                    }
                }
            }
            KeyCode::Up if ctrl => {
                if let Some(selected_pos) = selected_visible {
                    let (cols, _) = grid_layout(visible_indices.len());
                    if selected_pos >= cols {
                        self.move_visible_card(&visible_indices, selected_pos - cols);
                    }
                }
            }
            KeyCode::Down if ctrl => {
                if let Some(selected_pos) = selected_visible {
                    let (cols, _) = grid_layout(visible_indices.len());
                    if selected_pos + cols < visible_indices.len() {
                        self.move_visible_card(&visible_indices, selected_pos + cols);
                    }
                }
            }
            // ---------------------------------------------------------------
            // Navigation: arrows / hjkl (with Left/Right row-edge wrapping)
            // ---------------------------------------------------------------
            KeyCode::Left | KeyCode::Char('h') => {
                if let Some(selected_pos) = selected_visible {
                    let (cols, _) = grid_layout(visible_indices.len());
                    // Move left within row; at col 0 wrap to last slot of
                    // the previous row (same index arithmetic: selected - 1).
                    if selected_pos % cols > 0 || selected_pos >= cols {
                        self.selected = visible_indices[selected_pos - 1];
                        self.reset_card_scroll();
                    }
                }
            }
            KeyCode::Right | KeyCode::Char('l') => {
                if let Some(selected_pos) = selected_visible {
                    // Move right within row; at last col wrap to first slot
                    // of the next row, as long as a next card exists.
                    if selected_pos + 1 < visible_indices.len() {
                        self.selected = visible_indices[selected_pos + 1];
                        self.reset_card_scroll();
                    }
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if let Some(selected_pos) = selected_visible {
                    let (cols, _) = grid_layout(visible_indices.len());
                    if selected_pos >= cols {
                        self.selected = visible_indices[selected_pos - cols];
                        self.reset_card_scroll();
                    }
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if let Some(selected_pos) = selected_visible {
                    let (cols, _) = grid_layout(visible_indices.len());
                    if selected_pos + cols < visible_indices.len() {
                        self.selected = visible_indices[selected_pos + cols];
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
                if entry.meta.status != status {
                    entry.meta.status_changed_at = Some(std::time::Instant::now());
                }
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
                let desired = tmux_pane_viewport_size(term_cols, term_rows);
                if self.agent_view_state.last_pane_size != Some(desired) {
                    let _ = tmux::resize_window(&pane, desired.0, desired.1);
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
                    self.remove_agent(idx, remove_wt, true).await;
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
            // without triggering the flowmux dashboard switch.
            KeyCode::Char('b') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.agent_view_state.prefix_active = true;
                self.dirty = true;
            }
            KeyCode::Char('g') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.state = AppState::Dashboard;
            }
            KeyCode::Char('v') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                let is_git = self.agents.get(idx).is_some_and(|entry| {
                    crate::git::find_git_root(std::path::Path::new(&entry.config.directory))
                        .is_some()
                });
                if is_git {
                    self.launch_git_viewer(idx);
                }
            }
            KeyCode::Char('t') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.launch_terminal(idx);
            }
            KeyCode::PageUp => {
                if let Some(entry) = self.agents.get(idx) {
                    if uses_captured_scrollback(&entry.config.kind) {
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
                    } else if pane_handles_own_scroll(self.agent_view_state.pane_mouse_active) {
                        let _ = tmux::send_keys(&entry.config.pane, "PPage");
                    } else {
                        let _ = tmux::scroll_page_up(&entry.config.pane);
                    }
                }
            }
            KeyCode::PageDown => {
                if let Some(entry) = self.agents.get(idx) {
                    if uses_captured_scrollback(&entry.config.kind) {
                        let page = crossterm::terminal::size()
                            .map(|(_, h)| h as usize)
                            .unwrap_or(24)
                            .saturating_sub(2);
                        self.agent_view_state.view_scroll =
                            self.agent_view_state.view_scroll.saturating_sub(page);
                        self.dirty = true;
                    } else if pane_handles_own_scroll(self.agent_view_state.pane_mouse_active) {
                        let _ = tmux::send_keys(&entry.config.pane, "NPage");
                    } else {
                        let _ = tmux::scroll_page_down(&entry.config.pane);
                    }
                }
            }
            KeyCode::Char('q') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.switch_to_next_by_status(AgentStatus::Running);
            }
            KeyCode::Char('o') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.switch_to_next_by_status(AgentStatus::WaitingForInput);
            }
            KeyCode::Char('p') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.switch_to_next_by_status(AgentStatus::Idle);
            }
            _ => {
                if let Some(entry) = self.agents.get(idx) {
                    let pane = entry.config.pane.clone();
                    let is_plain_char = matches!(key.code, KeyCode::Char(_))
                        && !key.modifiers.contains(KeyModifiers::CONTROL)
                        && !key.modifiers.contains(KeyModifiers::ALT);
                    let keys = key_event_to_tmux(&key);
                    if !keys.is_empty() {
                        if is_plain_char {
                            let _ = tmux::send_literal(&pane, &keys);
                        } else {
                            let _ = tmux::send_keys(&pane, &keys);
                        }
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
        let pane_mouse_active = match &self.state {
            AppState::GitViewer(gv) => gv.pane_mouse_active,
            _ => false,
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
            KeyCode::PageUp => {
                if pane_handles_own_scroll(pane_mouse_active) {
                    let _ = tmux::send_keys(&pane, "PPage");
                } else {
                    if let AppState::GitViewer(ref mut gv) = self.state {
                        gv.view_scroll = gv
                            .view_scroll
                            .saturating_add(pane_page_scroll())
                            .min(MAX_RETAINED_LINES);
                    }
                    self.dirty = true;
                }
            }
            KeyCode::PageDown => {
                if pane_handles_own_scroll(pane_mouse_active) {
                    let _ = tmux::send_keys(&pane, "NPage");
                } else {
                    if let AppState::GitViewer(ref mut gv) = self.state {
                        gv.view_scroll = gv.view_scroll.saturating_sub(pane_page_scroll());
                    }
                    self.dirty = true;
                }
            }
            _ => {
                if let AppState::GitViewer(ref mut gv) = self.state {
                    gv.view_scroll = 0;
                }
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

        if let AppState::GitViewer(ref mut gv) = self.state {
            if let Ok((term_cols, term_rows)) = crossterm::terminal::size() {
                let desired = tmux_pane_viewport_size(term_cols, term_rows);
                if gv.last_pane_size != Some(desired) {
                    let _ = tmux::resize_window(&pane, desired.0, desired.1);
                    gv.last_pane_size = Some(desired);
                }
            }
        }

        let changed = if let AppState::GitViewer(ref mut gv) = self.state {
            if gv.view_scroll > 0 {
                tmux::capture_pane_history(&pane, MAX_RETAINED_LINES)
                    .ok()
                    .map(|raw| {
                        let changed = gv.update_lines(&raw);
                        clamp_external_pane_scroll(&mut gv.view_scroll, gv.lines.len());
                        changed
                    })
                    .unwrap_or(false)
            } else {
                tmux::capture_pane(&pane)
                    .ok()
                    .map(|raw| gv.update_lines(&raw))
                    .unwrap_or(false)
            }
        } else {
            false
        };
        if changed {
            self.dirty = true;
        }

        let new_cursor = tmux::cursor_position(&pane);
        if let AppState::GitViewer(ref mut gv) = self.state {
            if new_cursor != gv.cursor {
                gv.cursor = new_cursor;
                self.dirty = true;
            }

            gv.pane_mouse_active = tmux::pane_mouse_active(&pane);
        }
    }

    // -----------------------------------------------------------------------
    // Terminal View
    // -----------------------------------------------------------------------

    fn launch_terminal(&mut self, agent_idx: usize) {
        let directory = match self.agents.get(agent_idx) {
            Some(entry) => entry.config.directory.clone(),
            None => return,
        };

        if let Some(existing_pane) = self.terminal_panes.get(&agent_idx) {
            if tmux::is_alive(existing_pane) {
                let pane = existing_pane.clone();
                self.terminal_view_state = Some(TerminalViewState::new(agent_idx, pane));
                self.state = AppState::TerminalView(self.terminal_view_state.clone().unwrap());
                self.dirty = true;
                return;
            } else {
                self.terminal_panes.remove(&agent_idx);
            }
        }

        let window_index = match tmux::new_window(&directory, "terminal") {
            Ok(idx) => idx,
            Err(_) => return,
        };

        let pane = format!("{}:{}.0", tmux::session_name(), window_index);

        self.terminal_panes.insert(agent_idx, pane.clone());
        self.terminal_view_state = Some(TerminalViewState::new(agent_idx, pane));
        self.state = AppState::TerminalView(self.terminal_view_state.clone().unwrap());
        self.dirty = true;
    }

    fn handle_terminal_view_key(&mut self, key: KeyEvent) -> bool {
        let pane = match &self.state {
            AppState::TerminalView(tv) => tv.pane.clone(),
            _ => return true,
        };
        let pane_mouse_active = match &self.state {
            AppState::TerminalView(tv) => tv.pane_mouse_active,
            _ => false,
        };

        let prefix_active = match &self.state {
            AppState::TerminalView(tv) => tv.prefix_active,
            _ => false,
        };

        if prefix_active {
            if let AppState::TerminalView(ref mut tv) = self.state {
                tv.prefix_active = false;
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
                if let AppState::TerminalView(ref mut tv) = self.state {
                    tv.prefix_active = true;
                }
                self.dirty = true;
            }
            KeyCode::Char('t') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.exit_terminal_to_agent();
            }
            KeyCode::Char('g') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.exit_terminal_to_dashboard();
            }
            KeyCode::PageUp => {
                if pane_handles_own_scroll(pane_mouse_active) {
                    let _ = tmux::send_keys(&pane, "PPage");
                } else {
                    if let AppState::TerminalView(ref mut tv) = self.state {
                        tv.view_scroll = tv
                            .view_scroll
                            .saturating_add(pane_page_scroll())
                            .min(MAX_RETAINED_LINES);
                    }
                    self.dirty = true;
                }
            }
            KeyCode::PageDown => {
                if pane_handles_own_scroll(pane_mouse_active) {
                    let _ = tmux::send_keys(&pane, "NPage");
                } else {
                    if let AppState::TerminalView(ref mut tv) = self.state {
                        tv.view_scroll = tv.view_scroll.saturating_sub(pane_page_scroll());
                    }
                    self.dirty = true;
                }
            }
            _ => {
                if let AppState::TerminalView(ref mut tv) = self.state {
                    tv.view_scroll = 0;
                }
                let keys = key_event_to_tmux(&key);
                if !keys.is_empty() {
                    let _ = tmux::send_keys(&pane, &keys);
                }
            }
        }
        true
    }

    fn exit_terminal_to_agent(&mut self) {
        let agent_idx = match &self.state {
            AppState::TerminalView(tv) => tv.agent_idx,
            _ => return,
        };

        self.agent_view_state = AgentViewState::default();
        self.state = AppState::AgentView(agent_idx);
        self.terminal_view_state = None;
        self.dirty = true;
    }

    fn exit_terminal_to_dashboard(&mut self) {
        self.state = AppState::Dashboard;
        self.terminal_view_state = None;
        self.dirty = true;
    }

    async fn handle_terminal_view_tick(&mut self) {
        let pane = match &self.state {
            AppState::TerminalView(tv) => tv.pane.clone(),
            _ => return,
        };

        if !tmux::is_alive(&pane) {
            let agent_idx = match &self.state {
                AppState::TerminalView(tv) => tv.agent_idx,
                _ => return,
            };
            self.terminal_panes.remove(&agent_idx);
            self.exit_terminal_to_agent();
            return;
        }

        if let AppState::TerminalView(ref mut tv) = self.state {
            if let Ok((term_cols, term_rows)) = crossterm::terminal::size() {
                let desired = tmux_pane_viewport_size(term_cols, term_rows);
                if tv.last_pane_size != Some(desired) {
                    let _ = tmux::resize_window(&pane, desired.0, desired.1);
                    tv.last_pane_size = Some(desired);
                }
            }
        }

        let changed = if let AppState::TerminalView(ref mut tv) = self.state {
            if tv.view_scroll > 0 {
                tmux::capture_pane_history(&pane, MAX_RETAINED_LINES)
                    .ok()
                    .map(|raw| {
                        let changed = tv.update_lines(&raw);
                        clamp_external_pane_scroll(&mut tv.view_scroll, tv.lines.len());
                        changed
                    })
                    .unwrap_or(false)
            } else {
                tmux::capture_pane(&pane)
                    .ok()
                    .map(|raw| tv.update_lines(&raw))
                    .unwrap_or(false)
            }
        } else {
            false
        };
        if changed {
            self.dirty = true;
        }

        let new_cursor = tmux::cursor_position(&pane);
        if let AppState::TerminalView(ref mut tv) = self.state {
            if new_cursor != tv.cursor {
                tv.cursor = new_cursor;
                self.dirty = true;
            }

            tv.pane_mouse_active = tmux::pane_mouse_active(&pane);
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

            // Tab cycles focus: Name → Directory → [Worktree] → [Copy] → [Symlink] → AgentType → Name
            KeyCode::Tab => {
                let current_focus = self.create_state.focus.clone();
                let current_dir = self.create_state.directory.clone();
                if matches!(
                    current_focus,
                    CreateField::CopyDirectories | CreateField::SymlinkDirectories
                ) && self.create_state.selector_enabled(&current_focus)
                {
                    let action = {
                        let selector = self.create_state.selector_ref(&current_focus).unwrap();
                        if selector.current_candidate().is_some() {
                            Some(true)
                        } else {
                            Some(false)
                        }
                    };

                    if let Some(should_commit) = action {
                        if should_commit {
                            match self.create_state.commit_selector_candidate(&current_focus) {
                                Ok(true) => {
                                    self.create_state.error = None;
                                }
                                Ok(false) => {}
                                Err(err) => {
                                    self.create_state.error = Some(err);
                                }
                            }
                            return true;
                        }
                    }
                }

                self.create_state.focus = next_create_field(
                    &current_focus,
                    self.create_state.git_repo_root.is_some(),
                    self.create_state.create_worktree,
                    self.create_state.available_types.len() > 1,
                );
                self.create_state.error = None;
                if matches!(
                    self.create_state.focus,
                    CreateField::CopyDirectories | CreateField::SymlinkDirectories
                ) {
                    self.create_state.refresh_worktree_selector_matches();
                } else if current_focus == CreateField::Directory
                    && current_dir != self.create_state.directory
                {
                    self.create_state.error = None;
                }
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
                CreateField::CopyDirectories | CreateField::SymlinkDirectories => {
                    let focus = self.create_state.focus.clone();
                    if self.create_state.selector_enabled(&focus) {
                        if let Some(selector) = self.create_state.selector_mut(&focus) {
                            let n = selector.matches.len();
                            if n > 0 {
                                let new_idx = selector.selected_idx.saturating_sub(1);
                                selector.selected_idx = new_idx;
                                if new_idx < selector.scroll_offset {
                                    selector.scroll_offset = new_idx;
                                }
                            }
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
                CreateField::CopyDirectories | CreateField::SymlinkDirectories => {
                    let focus = self.create_state.focus.clone();
                    if self.create_state.selector_enabled(&focus) {
                        if let Some(selector) = self.create_state.selector_mut(&focus) {
                            let n = selector.matches.len();
                            if n > 0 {
                                let new_idx = (selector.selected_idx + 1).min(n - 1);
                                selector.selected_idx = new_idx;
                                if new_idx >= selector.scroll_offset + MAX_DIR_VISIBLE {
                                    selector.scroll_offset = new_idx + 1 - MAX_DIR_VISIBLE;
                                }
                            }
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
                        self.load_create_state_worktree_presets();
                    }
                } else if matches!(
                    self.create_state.focus,
                    CreateField::CopyDirectories | CreateField::SymlinkDirectories
                ) && self.create_state.selector_enabled(&self.create_state.focus)
                {
                    let focus = self.create_state.focus.clone();
                    let directory = self.create_state.directory.clone();
                    if let Some(selector) = self.create_state.selector_mut(&focus) {
                        if selector.descend() {
                            selector.refresh_matches(&directory);
                            self.create_state.error = None;
                        }
                    }
                } else if self.create_state.is_valid() {
                    let name = tmux::sanitize_name(&self.create_state.name.clone());
                    let dir = self.create_state.directory.clone();
                    let project = self.active_project_name().to_string();
                    let agent_type = self.create_state.selected_agent_type();
                    let create_worktree = self.create_state.create_worktree
                        && self.create_state.git_repo_root.is_some();
                    let git_repo_root = self
                        .create_state
                        .git_repo_root
                        .as_ref()
                        .map(|p| p.to_string_lossy().to_string());
                    let copy_directories = if self.create_state.copy_directories_enabled {
                        self.create_state.copy_directories.selected_dirs.clone()
                    } else {
                        Vec::new()
                    };
                    let symlink_directories = if self.create_state.symlink_directories_enabled {
                        self.create_state.symlink_directories.selected_dirs.clone()
                    } else {
                        Vec::new()
                    };
                    match self
                        .runner
                        .create(
                            &name,
                            &dir,
                            &project,
                            agent_type,
                            create_worktree,
                            git_repo_root.as_deref(),
                            copy_directories,
                            symlink_directories,
                        )
                        .await
                    {
                        Ok((config, adapter)) => {
                            self.persist_create_state_worktree_presets();
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
                        self.load_create_state_worktree_presets();
                    }
                    CreateField::CopyDirectories | CreateField::SymlinkDirectories => {
                        let focus = self.create_state.focus.clone();
                        if self.create_state.selector_enabled(&focus) {
                            let directory = self.create_state.directory.clone();
                            if let Some(selector) = self.create_state.selector_mut(&focus) {
                                if !selector.filter.is_empty() {
                                    selector.filter.clear();
                                } else {
                                    ctrl_w_delete_relative_path(&mut selector.current_dir);
                                }
                                selector.refresh_matches(&directory);
                                self.create_state.error = None;
                            }
                        }
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
                        self.load_create_state_worktree_presets();
                    }
                    CreateField::CopyDirectories | CreateField::SymlinkDirectories => {
                        let focus = self.create_state.focus.clone();
                        if self.create_state.selector_enabled(&focus) {
                            let directory = self.create_state.directory.clone();
                            if let Some(selector) = self.create_state.selector_mut(&focus) {
                                if !selector.filter.is_empty() {
                                    selector.filter.clear();
                                } else {
                                    ctrl_w_delete_relative_path(&mut selector.current_dir);
                                }
                                selector.refresh_matches(&directory);
                                self.create_state.error = None;
                            }
                        }
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
                        self.load_create_state_worktree_presets();
                    }
                    CreateField::CopyDirectories | CreateField::SymlinkDirectories => {
                        let focus = self.create_state.focus.clone();
                        if self.create_state.selector_enabled(&focus) {
                            let directory = self.create_state.directory.clone();
                            if let Some(selector) = self.create_state.selector_mut(&focus) {
                                if !selector.filter.is_empty() {
                                    selector.filter.pop();
                                } else if selector.navigate_up() {
                                    // already updated
                                } else {
                                    selector.selected_dirs.pop();
                                }
                                selector.refresh_matches(&directory);
                                self.create_state.error = None;
                            }
                        }
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
                        self.load_create_state_worktree_presets();
                    }
                    CreateField::CopyDirectories | CreateField::SymlinkDirectories => {
                        let focus = self.create_state.focus.clone();
                        if c == ' ' {
                            match focus {
                                CreateField::CopyDirectories => {
                                    self.create_state.copy_directories_enabled =
                                        !self.create_state.copy_directories_enabled;
                                    if self.create_state.copy_directories_enabled {
                                        self.create_state
                                            .copy_directories
                                            .refresh_matches(&self.create_state.directory);
                                    }
                                }
                                CreateField::SymlinkDirectories => {
                                    self.create_state.symlink_directories_enabled =
                                        !self.create_state.symlink_directories_enabled;
                                    if self.create_state.symlink_directories_enabled {
                                        self.create_state
                                            .symlink_directories
                                            .refresh_matches(&self.create_state.directory);
                                    }
                                }
                                _ => {}
                            }
                            self.create_state.error = None;
                        } else if self.create_state.selector_enabled(&focus) {
                            let directory = self.create_state.directory.clone();
                            if let Some(selector) = self.create_state.selector_mut(&focus) {
                                selector.filter.push(c);
                                selector.refresh_matches(&directory);
                                self.create_state.error = None;
                            }
                        }
                    }
                    CreateField::AgentType => {}
                    CreateField::CreateWorktree => {
                        // Space handled separately; other chars are no-ops here.
                        if c == ' ' {
                            if self.create_state.git_repo_root.is_some() {
                                self.create_state.create_worktree =
                                    !self.create_state.create_worktree;
                                if self.create_state.create_worktree {
                                    self.create_state.refresh_worktree_selector_matches();
                                } else if matches!(
                                    self.create_state.focus,
                                    CreateField::CopyDirectories | CreateField::SymlinkDirectories
                                ) {
                                    self.create_state.focus = next_create_field(
                                        &CreateField::CreateWorktree,
                                        self.create_state.git_repo_root.is_some(),
                                        self.create_state.create_worktree,
                                        self.create_state.available_types.len() > 1,
                                    );
                                }
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
    // CreateProjectDialog key handler
    // -----------------------------------------------------------------------

    fn handle_create_project_key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Esc => {
                self.state = AppState::Dashboard;
            }
            KeyCode::Enter => {
                let trimmed = self.create_project_state.name.trim();
                if trimmed.is_empty() {
                    self.create_project_state.error = Some("Project name is required".into());
                    return true;
                }
                if self.config.projects.len() >= MAX_PROJECTS {
                    self.create_project_state.error =
                        Some(format!("Maximum {} projects", MAX_PROJECTS));
                    return true;
                }
                if self
                    .config
                    .projects
                    .iter()
                    .any(|project| project == trimmed)
                {
                    self.create_project_state.error = Some("Project name must be unique".into());
                    return true;
                }

                self.config.projects.push(trimmed.to_string());
                self.config.normalize();
                let _ = self.config.save();
                if let Some(idx) = self.config.projects.iter().position(|p| p == trimmed) {
                    self.set_active_project_idx(idx);
                }
                self.state = AppState::Dashboard;
            }
            KeyCode::Backspace if key.modifiers.contains(KeyModifiers::CONTROL) => {
                ctrl_w_delete(&mut self.create_project_state.name);
                self.create_project_state.error = None;
            }
            KeyCode::Char('w') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                ctrl_w_delete(&mut self.create_project_state.name);
                self.create_project_state.error = None;
            }
            KeyCode::Backspace => {
                self.create_project_state.name.pop();
                self.create_project_state.error = None;
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.create_project_state.name.push(c);
                self.create_project_state.error = None;
            }
            _ => {}
        }
        true
    }

    // -----------------------------------------------------------------------
    // RemoveAgentDialog key handler
    // -----------------------------------------------------------------------

    async fn handle_remove_key(&mut self, key: KeyEvent, state: RemoveAgentState) -> bool {
        match key.code {
            KeyCode::Char('y') | KeyCode::Enter => {
                self.remove_agent(state.idx, state.remove_worktree, state.stop_agent)
                    .await;
                self.state = AppState::Dashboard;
            }
            KeyCode::Char('n') | KeyCode::Esc => {
                self.state = AppState::Dashboard;
            }
            KeyCode::Tab => {
                let has_worktree = self
                    .agents
                    .get(state.idx)
                    .and_then(|e| e.config.git_repo_root.as_ref())
                    .is_some();
                let max_focus = if has_worktree { 1 } else { 0 };
                if let AppState::RemoveAgentDialog(ref mut s) = self.state {
                    s.focus = if s.focus >= max_focus { 0 } else { s.focus + 1 };
                }
            }
            KeyCode::Char(' ') => {
                if let AppState::RemoveAgentDialog(ref mut s) = self.state {
                    match s.focus {
                        0 => s.stop_agent = !s.stop_agent,
                        1 => {
                            let has_worktree = self
                                .agents
                                .get(state.idx)
                                .and_then(|e| e.config.git_repo_root.as_ref())
                                .is_some();
                            if has_worktree {
                                s.remove_worktree = !s.remove_worktree;
                            }
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
        true
    }

    // -----------------------------------------------------------------------
    // RemoveProjectDialog key handler
    // -----------------------------------------------------------------------

    async fn handle_remove_project_key(
        &mut self,
        key: KeyEvent,
        state: RemoveProjectState,
    ) -> bool {
        match key.code {
            KeyCode::Char('y') | KeyCode::Enter => {
                if state.agent_count == 0 || state.confirm_remove_agents {
                    self.remove_project(state.idx).await;
                    self.state = AppState::Dashboard;
                }
            }
            KeyCode::Char('n') | KeyCode::Esc => {
                self.state = AppState::Dashboard;
            }
            KeyCode::Char(' ') => {
                if let AppState::RemoveProjectDialog(ref mut s) = self.state {
                    s.confirm_remove_agents = !s.confirm_remove_agents;
                }
            }
            _ => {}
        }
        true
    }

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    fn switch_to_next_by_status(&mut self, target: AgentStatus) {
        let current = self.selected;
        let Some(next) = next_agent_by_status(
            &self.agents,
            self.active_project_name(),
            current,
            target,
            None,
        ) else {
            return;
        };

        if next != current {
            self.selected = next;
            self.state = AppState::AgentView(next);
            self.agent_view_state = AgentViewState::default();
            self.dirty = true;
        }
    }

    async fn remove_agent(&mut self, idx: usize, remove_worktree: bool, stop_agent: bool) {
        if idx < self.agents.len() {
            if let Some(agent_config) = self.config.agents.get(idx) {
                if stop_agent {
                    // Extract window target from pane (e.g., "flowmux:1.0" -> "flowmux:1")
                    if let Some(colon_pos) = agent_config.pane.find(':') {
                        if let Some(dot_pos) = agent_config.pane[colon_pos..].find('.') {
                            let window_target = &agent_config.pane[..colon_pos + dot_pos];
                            let _ = tmux::kill_window(window_target);
                        }
                    }
                    if let Err(error) = self.adapters[idx].stop().await {
                        eprintln!("warning: failed to stop agent: {error}");
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
            self.ensure_project_selection();
            self.dirty = true;
        }
    }

    async fn remove_project(&mut self, project_idx: usize) {
        let Some(project_name) = self.config.projects.get(project_idx).cloned() else {
            return;
        };
        if project_name == DEFAULT_PROJECT_NAME {
            return;
        }

        let mut agent_indices: Vec<usize> = self
            .agents
            .iter()
            .enumerate()
            .filter(|(_, entry)| entry.config.project == project_name)
            .map(|(idx, _)| idx)
            .collect();
        agent_indices.sort_unstable_by(|a, b| b.cmp(a));

        for idx in agent_indices {
            let remove_worktree = self
                .agents
                .get(idx)
                .and_then(|entry| entry.config.git_repo_root.as_ref())
                .is_some();
            self.remove_agent(idx, remove_worktree, true).await;
        }

        if project_idx < self.config.projects.len() {
            self.config.projects.remove(project_idx);
            self.config.normalize();
            let next_idx = project_idx.min(self.config.projects.len().saturating_sub(1));
            self.active_project_idx = next_idx;
            self.ensure_project_selection();
            self.reset_project_notification();
            let _ = self.config.save();
            self.dirty = true;
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
// Project helpers
// ---------------------------------------------------------------------------

fn visible_agent_indices_for_project(agents: &[AgentEntry], project: &str) -> Vec<usize> {
    agents
        .iter()
        .enumerate()
        .filter(|(_, entry)| entry.config.project == project)
        .map(|(idx, _)| idx)
        .collect()
}

fn next_agent_by_status(
    agents: &[AgentEntry],
    project: &str,
    current: usize,
    target: AgentStatus,
    fallback: Option<AgentStatus>,
) -> Option<usize> {
    let now = std::time::Instant::now();
    let mut matches = matching_agent_indices(agents, project, &target);

    if matches.is_empty()
        && let Some(fallback) = fallback
    {
        matches = matching_agent_indices(agents, project, &fallback);
    }

    matches.sort_by_key(|&idx| agents[idx].meta.status_changed_at.unwrap_or(now));

    match matches.iter().position(|&idx| idx == current) {
        Some(pos) => Some(matches[(pos + 1) % matches.len()]),
        None => matches.first().copied(),
    }
}

fn matching_agent_indices(
    agents: &[AgentEntry],
    project: &str,
    status: &AgentStatus,
) -> Vec<usize> {
    agents
        .iter()
        .enumerate()
        .filter(|(_, entry)| entry.config.project == project && entry.meta.status == *status)
        .map(|(idx, _)| idx)
        .collect()
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

fn unicode_display_width(s: &str) -> usize {
    use unicode_width::UnicodeWidthStr;
    UnicodeWidthStr::width(s)
}

fn pane_handles_own_scroll(mouse_active: bool) -> bool {
    mouse_active
}

fn pane_page_scroll() -> usize {
    pane_content_height().max(1)
}

pub(crate) fn pane_content_height() -> usize {
    crossterm::terminal::size()
        .map(|(_, rows)| rows.saturating_sub(PANE_CHROME_HEIGHT) as usize)
        .unwrap_or(20)
}

pub(crate) fn pane_visible_line_range(
    total_lines: usize,
    view_scroll: usize,
    viewport_height: usize,
) -> (usize, usize) {
    let max_scroll = total_lines.saturating_sub(viewport_height);
    let effective_scroll = view_scroll.min(max_scroll);
    if total_lines == 0 {
        (0, 0)
    } else {
        let end = total_lines.saturating_sub(effective_scroll);
        let start = end.saturating_sub(viewport_height);
        (start, end)
    }
}

fn clamp_external_pane_scroll(view_scroll: &mut usize, total_lines: usize) {
    let max_scroll = total_lines.saturating_sub(pane_content_height());
    if *view_scroll > max_scroll {
        *view_scroll = max_scroll;
    }
}

fn mouse_event_to_sgr(mouse: MouseEvent, show_overlay: bool) -> Option<String> {
    if show_overlay {
        return None;
    }

    let (mut cb, suffix) = match mouse.kind {
        MouseEventKind::Down(btn) => (sgr_button(btn), 'M'),
        MouseEventKind::Up(btn) => (sgr_button(btn), 'm'),
        MouseEventKind::Drag(btn) => (sgr_button(btn) + 32, 'M'),
        MouseEventKind::Moved => (35u8, 'M'),
        MouseEventKind::ScrollUp => (64u8, 'M'),
        MouseEventKind::ScrollDown => (65u8, 'M'),
        _ => return None,
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

    Some(format!(
        "\x1b[<{};{};{}{}",
        cb,
        mouse.column.saturating_sub(1) + 1,
        mouse.row.saturating_sub(2) + 1,
        suffix
    ))
}

fn sgr_button(btn: MouseButton) -> u8 {
    match btn {
        MouseButton::Left => 0,
        MouseButton::Middle => 1,
        MouseButton::Right => 2,
    }
}

fn uses_captured_scrollback(kind: &AgentKind) -> bool {
    matches!(kind, AgentKind::Claude { .. } | AgentKind::Codex { .. })
}

#[cfg(test)]
mod project_tests {
    use super::*;

    #[test]
    fn claude_and_codex_use_captured_scrollback() {
        assert!(uses_captured_scrollback(&AgentKind::Claude {
            flowmux_agent_id: "claude-test".to_string(),
            session_id: None,
            transcript_path: None,
        }));
        assert!(uses_captured_scrollback(&AgentKind::Codex {
            port: 16100,
            session_id: None,
        }));
        assert!(!uses_captured_scrollback(&AgentKind::Opencode {
            port: 14100,
            session_id: None,
        }));
    }

    #[test]
    fn ctrl_q_o_p_are_distinct_control_letter_events() {
        assert_eq!(
            KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL).code,
            KeyCode::Char('q')
        );
        assert_eq!(
            KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL).code,
            KeyCode::Char('o')
        );
        assert_eq!(
            KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL).code,
            KeyCode::Char('p')
        );
    }

    #[test]
    fn scroll_wheel_mouse_events_encode_as_sgr_sequences() {
        let scroll_up = MouseEvent {
            kind: MouseEventKind::ScrollUp,
            column: 4,
            row: 6,
            modifiers: KeyModifiers::empty(),
        };
        let scroll_down = MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: 4,
            row: 6,
            modifiers: KeyModifiers::SHIFT,
        };

        assert_eq!(
            mouse_event_to_sgr(scroll_up, false).as_deref(),
            Some("\x1b[<64;4;5M")
        );
        assert_eq!(
            mouse_event_to_sgr(scroll_down, false).as_deref(),
            Some("\x1b[<69;4;5M")
        );
    }

    #[test]
    fn pane_visible_line_range_slices_from_bottom_with_offset() {
        assert_eq!(pane_visible_line_range(6, 0, 3), (3, 6));
        assert_eq!(pane_visible_line_range(6, 2, 3), (1, 4));
        assert_eq!(pane_visible_line_range(2, 8, 3), (0, 2));
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

fn ctrl_w_delete_relative_path(s: &mut String) {
    if let Some((parent, _)) = s.rsplit_once('/') {
        *s = parent.to_string();
    } else {
        s.clear();
    }
}

fn next_create_field(
    current: &CreateField,
    has_git_repo: bool,
    create_worktree: bool,
    has_multiple_agent_types: bool,
) -> CreateField {
    match current {
        CreateField::Name => CreateField::Directory,
        CreateField::Directory => {
            if has_git_repo {
                CreateField::CreateWorktree
            } else if has_multiple_agent_types {
                CreateField::AgentType
            } else {
                CreateField::Name
            }
        }
        CreateField::CreateWorktree => {
            if create_worktree {
                CreateField::CopyDirectories
            } else if has_multiple_agent_types {
                CreateField::AgentType
            } else {
                CreateField::Name
            }
        }
        CreateField::CopyDirectories => CreateField::SymlinkDirectories,
        CreateField::SymlinkDirectories => {
            if has_multiple_agent_types {
                CreateField::AgentType
            } else {
                CreateField::Name
            }
        }
        CreateField::AgentType => CreateField::Name,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_discovery::DiscoveredAgents;
    use crate::config::{AgentConfig, AgentKind};
    use crate::global_config::GlobalConfig;
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    struct TestDir {
        path: PathBuf,
    }

    impl TestDir {
        fn new(prefix: &str) -> Self {
            let path =
                std::env::temp_dir().join(format!("flowmux-app-{prefix}-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&path).unwrap();
            Self { path }
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    fn test_agent(name: &str, project: &str, status: AgentStatus) -> AgentEntry {
        AgentEntry {
            config: AgentConfig {
                name: name.into(),
                pane: format!("flowmux:{}.0", name),
                directory: format!("/tmp/{}", name),
                project: project.into(),
                kind: AgentKind::Opencode {
                    port: 9000,
                    session_id: None,
                },
                git_repo_root: None,
            },
            meta: AgentMeta {
                status,
                ..AgentMeta::default()
            },
        }
    }

    fn test_app_with_global_config(global_config: GlobalConfig) -> App {
        let runner = AgentRunner::new(
            DiscoveredAgents {
                claude: None,
                codex: None,
                opencode: None,
            },
            global_config,
            "test-session".into(),
            std::env::temp_dir().join("flowmux-worktrees-test"),
            None,
        );
        App::new(
            Config::default(),
            Vec::new(),
            Vec::new(),
            runner,
            HostColors::default(),
        )
    }

    #[test]
    fn visible_agent_indices_are_project_scoped() {
        let agents = vec![
            AgentEntry {
                config: AgentConfig {
                    name: "a".into(),
                    pane: "flowmux:1.0".into(),
                    directory: "/tmp/a".into(),
                    project: "Default".into(),
                    kind: AgentKind::Opencode {
                        port: 9000,
                        session_id: None,
                    },
                    git_repo_root: None,
                },
                meta: AgentMeta::default(),
            },
            AgentEntry {
                config: AgentConfig {
                    name: "b".into(),
                    pane: "flowmux:2.0".into(),
                    directory: "/tmp/b".into(),
                    project: "work".into(),
                    kind: AgentKind::Opencode {
                        port: 9001,
                        session_id: None,
                    },
                    git_repo_root: None,
                },
                meta: AgentMeta::default(),
            },
            AgentEntry {
                config: AgentConfig {
                    name: "c".into(),
                    pane: "flowmux:3.0".into(),
                    directory: "/tmp/c".into(),
                    project: "work".into(),
                    kind: AgentKind::Claude {
                        flowmux_agent_id: "id".into(),
                        session_id: None,
                        transcript_path: None,
                    },
                    git_repo_root: None,
                },
                meta: AgentMeta::default(),
            },
        ];

        assert_eq!(
            visible_agent_indices_for_project(&agents, "Default"),
            vec![0]
        );
        assert_eq!(
            visible_agent_indices_for_project(&agents, "work"),
            vec![1, 2]
        );
    }

    #[test]
    fn status_counts_are_project_scoped() {
        let agents = vec![
            test_agent("default-running", "Default", AgentStatus::Running),
            test_agent("work-waiting", "work", AgentStatus::WaitingForInput),
            test_agent("work-idle", "work", AgentStatus::Idle),
            test_agent("other-running", "other", AgentStatus::Running),
        ];

        assert_eq!(
            AgentStatusCounts::for_project(&agents, "work"),
            AgentStatusCounts {
                running: 0,
                waiting: 1,
                idle: 1,
            }
        );
    }

    #[test]
    fn next_agent_by_status_never_crosses_projects() {
        let now = std::time::Instant::now();
        let mut agents = vec![
            test_agent("work-old", "work", AgentStatus::WaitingForInput),
            test_agent("other", "other", AgentStatus::WaitingForInput),
            test_agent("work-new", "work", AgentStatus::WaitingForInput),
        ];
        agents[0].meta.status_changed_at = Some(now - std::time::Duration::from_secs(2));
        agents[1].meta.status_changed_at = Some(now - std::time::Duration::from_secs(3));
        agents[2].meta.status_changed_at = Some(now - std::time::Duration::from_secs(1));

        assert_eq!(
            next_agent_by_status(&agents, "work", 0, AgentStatus::WaitingForInput, None,),
            Some(2)
        );
        assert_eq!(
            next_agent_by_status(&agents, "work", 2, AgentStatus::WaitingForInput, None,),
            Some(0)
        );
    }

    #[test]
    fn next_running_agent_stays_project_scoped() {
        let agents = vec![
            test_agent("other-running", "other", AgentStatus::Running),
            test_agent("work-old", "work", AgentStatus::Running),
            test_agent("work-new", "work", AgentStatus::Running),
        ];

        assert_eq!(
            next_agent_by_status(&agents, "work", 1, AgentStatus::Running, None),
            Some(2)
        );
        assert_eq!(
            next_agent_by_status(&agents, "work", 2, AgentStatus::Running, None),
            Some(1)
        );
    }

    #[test]
    fn next_waiting_agent_stays_project_scoped() {
        let agents = vec![
            test_agent("other-waiting", "other", AgentStatus::WaitingForInput),
            test_agent("work-old", "work", AgentStatus::WaitingForInput),
            test_agent("work-new", "work", AgentStatus::WaitingForInput),
        ];

        assert_eq!(
            next_agent_by_status(&agents, "work", 1, AgentStatus::WaitingForInput, None),
            Some(2)
        );
        assert_eq!(
            next_agent_by_status(&agents, "work", 2, AgentStatus::WaitingForInput, None),
            Some(1)
        );
    }

    #[test]
    fn next_idle_agent_stays_project_scoped() {
        let agents = vec![
            test_agent("other-idle", "other", AgentStatus::Idle),
            test_agent("work-old", "work", AgentStatus::Idle),
            test_agent("work-new", "work", AgentStatus::Idle),
        ];

        assert_eq!(
            next_agent_by_status(&agents, "work", 1, AgentStatus::Idle, None),
            Some(2)
        );
        assert_eq!(
            next_agent_by_status(&agents, "work", 2, AgentStatus::Idle, None),
            Some(1)
        );
    }

    #[test]
    fn notification_reset_clears_blinks_and_uses_a_fresh_baseline() {
        let mut notification = StatusNotification::default();
        notification.reset(AgentStatusCounts {
            running: 1,
            waiting: 0,
            idle: 0,
        });
        notification.observe(AgentStatusCounts {
            running: 0,
            waiting: 0,
            idle: 1,
        });
        assert!(notification.running_blink.is_some());

        notification.reset(AgentStatusCounts {
            running: 0,
            waiting: 2,
            idle: 0,
        });

        assert!(notification.running_blink.is_none());
        assert!(notification.waiting_blink.is_none());

        notification.observe(AgentStatusCounts {
            running: 0,
            waiting: 2,
            idle: 0,
        });
        assert!(notification.running_blink.is_none());
        assert!(notification.waiting_blink.is_none());
    }

    #[test]
    fn notification_observe_blinks_for_active_project_count_changes() {
        let mut notification = StatusNotification::default();
        notification.reset(AgentStatusCounts {
            running: 0,
            waiting: 0,
            idle: 1,
        });
        notification.observe(AgentStatusCounts {
            running: 0,
            waiting: 1,
            idle: 0,
        });

        assert!(notification.waiting_blink.is_some());
        assert!(notification.running_blink.is_none());
    }

    #[test]
    fn tmux_pane_viewport_size_reserves_ui_chrome() {
        assert_eq!(tmux_pane_viewport_size(120, 40), (118, 36));
    }

    #[test]
    fn tmux_pane_viewport_size_saturates_for_tiny_terminals() {
        assert_eq!(tmux_pane_viewport_size(1, 1), (0, 0));
        assert_eq!(tmux_pane_viewport_size(2, 4), (0, 0));
        assert_eq!(tmux_pane_viewport_size(3, 5), (1, 1));
    }

    #[test]
    fn commit_selector_candidate_resets_to_root_and_keeps_selection() {
        let temp = TestDir::new("commit-selector");
        let selected = temp.path.join("selected");
        let cache_dir = selected.join("cache");
        std::fs::create_dir_all(&cache_dir).unwrap();

        let mut state = CreateAgentState {
            directory: selected.to_string_lossy().to_string(),
            create_worktree: true,
            git_repo_root: Some(temp.path.clone()),
            ..CreateAgentState::default()
        };
        state.copy_directories.current_dir = "cache".into();

        assert_eq!(
            state
                .commit_selector_candidate(&CreateField::CopyDirectories)
                .unwrap(),
            true
        );
        assert_eq!(state.copy_directories.selected_dirs, vec!["cache"]);
        assert_eq!(state.copy_directories.current_display(), "./");
        assert_eq!(state.copy_directories.matches, vec!["cache"]);
    }

    #[test]
    fn commit_selector_candidate_rejects_cross_section_duplicates() {
        let mut state = CreateAgentState::default();
        state
            .symlink_directories
            .selected_dirs
            .push("vendor".into());
        state.copy_directories.current_dir = "vendor".into();

        let err = state
            .commit_selector_candidate(&CreateField::CopyDirectories)
            .unwrap_err();

        assert!(err.contains("cannot be both copied and symlinked"));
    }

    #[test]
    fn refresh_dir_matches_clears_worktree_selections_when_directory_changes() {
        let temp = TestDir::new("clear-selections");
        let base = temp.path.join("base");
        let nested = base.join("nested");
        std::fs::create_dir_all(&nested).unwrap();

        let mut state = CreateAgentState {
            directory: base.to_string_lossy().to_string(),
            copy_directories: RelativeDirSelector {
                selected_dirs: vec!["cache".into()],
                current_dir: "cache".into(),
                ..RelativeDirSelector::default()
            },
            symlink_directories: RelativeDirSelector {
                selected_dirs: vec!["deps".into()],
                ..RelativeDirSelector::default()
            },
            ..CreateAgentState::default()
        };

        state.directory = nested.to_string_lossy().to_string();
        state.refresh_dir_matches();

        assert!(state.copy_directories.selected_dirs.is_empty());
        assert!(state.symlink_directories.selected_dirs.is_empty());
        assert_eq!(state.copy_directories.current_display(), "./");
    }

    #[test]
    fn next_create_field_skips_worktree_selectors_when_disabled() {
        assert_eq!(
            next_create_field(&CreateField::CreateWorktree, true, false, true),
            CreateField::AgentType
        );
        assert_eq!(
            next_create_field(&CreateField::CreateWorktree, true, true, true),
            CreateField::CopyDirectories
        );
        assert_eq!(
            next_create_field(&CreateField::SymlinkDirectories, true, true, false),
            CreateField::Name
        );
    }

    #[test]
    fn ctrl_w_delete_relative_path_removes_last_component() {
        let mut path = "cache/nested".to_string();
        ctrl_w_delete_relative_path(&mut path);
        assert_eq!(path, "cache");

        ctrl_w_delete_relative_path(&mut path);
        assert!(path.is_empty());
    }

    #[test]
    fn load_create_state_worktree_presets_populates_enabled_sections_for_repo_root() {
        let temp = TestDir::new("preset-load");
        let repo_root = temp.path.join("repo");
        let mut presets = BTreeMap::new();
        presets.insert(
            repo_root.to_string_lossy().to_string(),
            WorktreeDirectoryPreset {
                copy_directories: vec!["node_modules".into(), "target".into()],
                symlink_directories: vec!["vendor".into()],
            },
        );

        let mut app = test_app_with_global_config(GlobalConfig {
            worktree_directory_presets: presets,
            ..GlobalConfig::default()
        });
        app.create_state.git_repo_root = Some(repo_root);
        app.create_state.copy_directories.selected_dirs = vec!["stale".into()];
        app.create_state.symlink_directories.selected_dirs = vec!["stale".into()];

        app.load_create_state_worktree_presets();

        assert!(app.create_state.copy_directories_enabled);
        assert!(app.create_state.symlink_directories_enabled);
        assert_eq!(
            app.create_state.copy_directories.selected_dirs,
            vec!["node_modules", "target"]
        );
        assert_eq!(
            app.create_state.symlink_directories.selected_dirs,
            vec!["vendor"]
        );
        assert_eq!(app.create_state.copy_directories.current_display(), "./");
    }

    #[test]
    fn load_create_state_worktree_presets_clears_when_repo_has_no_preset() {
        let temp = TestDir::new("preset-clear");
        let mut app = test_app_with_global_config(GlobalConfig::default());
        app.create_state.git_repo_root = Some(temp.path.join("repo"));
        app.create_state.copy_directories_enabled = true;
        app.create_state.copy_directories.selected_dirs = vec!["target".into()];
        app.create_state.symlink_directories_enabled = true;
        app.create_state.symlink_directories.selected_dirs = vec!["vendor".into()];

        app.load_create_state_worktree_presets();

        assert!(!app.create_state.copy_directories_enabled);
        assert!(!app.create_state.symlink_directories_enabled);
        assert!(app.create_state.copy_directories.selected_dirs.is_empty());
        assert!(
            app.create_state
                .symlink_directories
                .selected_dirs
                .is_empty()
        );
    }
}

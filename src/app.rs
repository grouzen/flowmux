use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use std::io::Write;
use std::process::{Command, Stdio};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::time::{Duration, interval};

use crate::agents::AgentAdapter;
use crate::config::{AgentKind, Config, DEFAULT_PROJECT_NAME, MAX_PROJECTS};
use crate::global_config::WorktreeDirectoryPreset;
use crate::host_terminal::HostColors;
use crate::models::{AgentEntry, AgentMeta, AgentStatus, AgentStatusCounts, AgentType};
use crate::runner::AgentRunner;
use crate::tmux;
use crate::ui::dashboard::{PROJECT_TABS_HEIGHT, grid_layout, project_tab_label};
use crate::ui::theme::{
    Theme, theme_by_id, theme_by_index, theme_id_or_default, theme_index_by_id,
};

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
const DASHBOARD_DOUBLE_CLICK_WINDOW: std::time::Duration = std::time::Duration::from_millis(400);
const COPY_FEEDBACK_DURATION: std::time::Duration = std::time::Duration::from_secs(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PaneCellPoint {
    col: u16,
    row: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PaneBufferPoint {
    col: u16,
    row: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PaneMouseCell {
    Inside(PaneCellPoint),
    Above(PaneCellPoint),
    Below(PaneCellPoint),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CopyScrollDirection {
    Up,
    Down,
}

#[derive(Debug, Clone, Copy)]
struct PendingPaneClick {
    mouse: MouseEvent,
    anchor: PaneBufferPoint,
}

#[derive(Debug, Clone, Copy)]
struct ActiveCopySelection {
    anchor: PaneBufferPoint,
    focus: PaneBufferPoint,
    last_drag: MouseEvent,
}

impl ActiveCopySelection {
    fn buffer_range(self) -> crate::ghostty::render::SelectionRange {
        let start_row = self.anchor.row.min(u16::MAX as usize) as u16;
        let end_row = self.focus.row.min(u16::MAX as usize) as u16;
        crate::ghostty::render::SelectionRange::new(
            (self.anchor.col, start_row),
            (self.focus.col, end_row),
        )
    }
}

#[derive(Debug, Clone)]
struct CopyFeedback {
    message: String,
    success: bool,
    expires_at: std::time::Instant,
}

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
pub struct TextInputState {
    pub value: String,
    pub cursor: usize,
}

impl TextInputState {
    fn move_left(&mut self) {
        move_text_cursor_left(&self.value, &mut self.cursor);
    }

    fn move_right(&mut self) {
        move_text_cursor_right(&self.value, &mut self.cursor);
    }

    fn move_home(&mut self) {
        self.cursor = 0;
    }

    fn move_end(&mut self) {
        self.cursor = self.value.len();
    }

    fn insert_char(&mut self, c: char) {
        insert_text_char(&mut self.value, &mut self.cursor, c);
    }

    fn backspace(&mut self) {
        backspace_text(&mut self.value, &mut self.cursor);
    }

    fn ctrl_w_delete(&mut self) {
        ctrl_w_delete_text(&mut self.value, &mut self.cursor);
    }
}

#[derive(Debug, Clone, Default)]
pub struct CreateProjectState {
    pub name: TextInputState,
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct RemoveProjectState {
    pub idx: usize,
    pub name: String,
    pub agent_count: usize,
    pub confirm_remove_agents: bool,
}

#[derive(Debug, Clone)]
pub struct StartupGuideState {
    pub page: usize,
    pub persist_on_close: bool,
}

impl StartupGuideState {
    fn first_run() -> Self {
        Self {
            page: 0,
            persist_on_close: true,
        }
    }

    fn reopened() -> Self {
        Self {
            page: 0,
            persist_on_close: false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SettingsState {
    pub selected_idx: usize,
    pub committed_theme_id: String,
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
    StartupGuide(StartupGuideState),
    CreateAgentDialog,
    CreateProjectDialog,
    SettingsDialog(SettingsState),
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
    /// Last viewport height used to interpret `view_scroll`.
    last_viewport_height: Option<usize>,
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

#[derive(Debug, Clone, Copy, Default)]
struct StoredAgentViewScroll {
    view_scroll: usize,
    viewport_height: Option<usize>,
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
    InitializeSubmodules,
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
    pub name_cursor: usize,
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
    pub has_git_submodules: bool,
    pub initialize_submodules: bool,
    pub copy_directories_enabled: bool,
    pub symlink_directories_enabled: bool,
    pub copy_directories: RelativeDirSelector,
    pub symlink_directories: RelativeDirSelector,
}

impl Default for CreateAgentState {
    fn default() -> Self {
        Self {
            name: String::new(),
            name_cursor: 0,
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
            has_git_submodules: false,
            initialize_submodules: true,
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

    fn move_name_cursor_left(&mut self) {
        move_text_cursor_left(&self.name, &mut self.name_cursor);
    }

    fn move_name_cursor_right(&mut self) {
        move_text_cursor_right(&self.name, &mut self.name_cursor);
    }

    fn move_name_cursor_home(&mut self) {
        self.name_cursor = 0;
    }

    fn move_name_cursor_end(&mut self) {
        self.name_cursor = self.name.len();
    }

    fn insert_name_char(&mut self, c: char) {
        insert_text_char(&mut self.name, &mut self.name_cursor, c);
    }

    fn backspace_name(&mut self) {
        backspace_text(&mut self.name, &mut self.name_cursor);
    }

    fn ctrl_w_delete_name(&mut self) {
        ctrl_w_delete_text(&mut self.name, &mut self.name_cursor);
    }

    pub fn worktree_selectors_visible(&self) -> bool {
        self.git_repo_root.is_some() && self.create_worktree
    }

    pub fn initialize_submodules_visible(&self) -> bool {
        self.worktree_selectors_visible() && self.has_git_submodules
    }

    pub fn selector_enabled(&self, field: &CreateField) -> bool {
        match field {
            CreateField::CopyDirectories => self.copy_directories_enabled,
            CreateField::SymlinkDirectories => self.symlink_directories_enabled,
            _ => false,
        }
    }

    fn disable_empty_selector(&mut self, field: &CreateField) {
        match field {
            CreateField::CopyDirectories if self.copy_directories.selected_dirs.is_empty() => {
                self.copy_directories_enabled = false;
            }
            CreateField::SymlinkDirectories
                if self.symlink_directories.selected_dirs.is_empty() =>
            {
                self.symlink_directories_enabled = false;
            }
            _ => {}
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
        self.has_git_submodules = new_root
            .as_ref()
            .is_some_and(|repo_root| crate::git::repo_has_submodules(repo_root));
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
    dashboard_selected_by_project: std::collections::HashMap<String, usize>,
    pub config: Config,
    pub runner: AgentRunner,
    pub agent_view_state: AgentViewState,
    pub git_viewer_state: Option<GitViewerState>,
    pub terminal_view_state: Option<TerminalViewState>,
    pub terminal_panes: std::collections::HashMap<usize, String>,
    pub create_state: CreateAgentState,
    pub create_project_state: CreateProjectState,
    pub active_theme_id: String,
    pub preview_theme_id: Option<String>,
    pub tx: UnboundedSender<Event>,
    pub rx: UnboundedReceiver<Event>,
    last_dashboard_left_click: Option<(usize, std::time::Instant)>,
    pending_pane_click: Option<PendingPaneClick>,
    active_copy_selection: Option<ActiveCopySelection>,
    copy_feedback: Option<CopyFeedback>,
    /// Set to `true` whenever state changes and a redraw is needed.
    /// Cleared to `false` by the render loop after each draw.
    pub dirty: bool,
    agent_view_scroll: Vec<StoredAgentViewScroll>,
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
        let show_startup_guide = !runner.global_config().startup_guide_dismissed;
        let (tx, rx) = mpsc::unbounded_channel();
        let card_count = agents.len();
        let active_theme_id =
            theme_id_or_default(runner.global_config().theme.as_deref()).to_string();
        let mut app = Self {
            agents,
            adapters,
            state: if show_startup_guide {
                AppState::StartupGuide(StartupGuideState::first_run())
            } else {
                AppState::Dashboard
            },
            active_project_idx: 0,
            selected: 0,
            dashboard_selected_by_project: std::collections::HashMap::new(),
            config,
            runner,
            agent_view_state: AgentViewState::default(),
            git_viewer_state: None,
            terminal_view_state: None,
            terminal_panes: std::collections::HashMap::new(),
            create_state: CreateAgentState::default(),
            create_project_state: CreateProjectState::default(),
            active_theme_id,
            preview_theme_id: None,
            tx,
            rx,
            last_dashboard_left_click: None,
            pending_pane_click: None,
            active_copy_selection: None,
            copy_feedback: None,
            dirty: true, // force initial draw
            agent_view_scroll: vec![StoredAgentViewScroll::default(); card_count],
            card_scroll: vec![0u16; card_count],
            card_response_heights: vec![0u16; card_count],
            card_response_widths: vec![0u16; card_count],
            host_colors,
            notification: StatusNotification::default(),
        };
        app.ensure_project_selection();
        app.reset_notification();
        app
    }

    pub fn active_project_name(&self) -> &str {
        self.config
            .projects
            .get(self.active_project_idx)
            .map(String::as_str)
            .unwrap_or(DEFAULT_PROJECT_NAME)
    }

    pub fn theme(&self) -> &'static Theme {
        let id = self
            .preview_theme_id
            .as_deref()
            .unwrap_or(&self.active_theme_id);
        &theme_by_id(id).unwrap_or(theme_by_index(0)).theme
    }

    fn set_theme_preview_by_id(&mut self, theme_id: &str) {
        self.preview_theme_id = Some(theme_id_or_default(Some(theme_id)).to_string());
        self.dirty = true;
    }

    fn open_settings_dialog(&mut self) {
        let committed_theme_id = self.active_theme_id.clone();
        self.preview_theme_id = Some(committed_theme_id.clone());
        self.state = AppState::SettingsDialog(SettingsState {
            selected_idx: theme_index_by_id(&committed_theme_id),
            committed_theme_id,
        });
        self.dirty = true;
    }

    fn apply_selected_theme(&mut self, selected_idx: usize) {
        let selected_theme = theme_by_index(selected_idx);
        let selected_id = selected_theme.id.to_string();
        self.active_theme_id = selected_id.clone();
        self.preview_theme_id = None;
        self.runner.global_config_mut().theme = Some(selected_id);
    }

    fn confirm_settings_dialog(&mut self, state: SettingsState) {
        self.apply_selected_theme(state.selected_idx);
        let _ = self.runner.global_config().save();
        self.state = AppState::Dashboard;
        self.dirty = true;
    }

    fn cancel_settings_dialog(&mut self, state: SettingsState) {
        self.active_theme_id = theme_id_or_default(Some(&state.committed_theme_id)).to_string();
        self.preview_theme_id = None;
        self.state = AppState::Dashboard;
        self.dirty = true;
    }

    fn clear_pane_copy_interaction(&mut self) {
        self.pending_pane_click = None;
        self.active_copy_selection = None;
    }

    fn shift_active_copy_selection_rows(&mut self, rows: usize) {
        if rows == 0 {
            return;
        }
        if let Some(selection) = self.active_copy_selection.as_mut() {
            selection.anchor.row = selection.anchor.row.saturating_add(rows);
            selection.focus.row = selection.focus.row.saturating_add(rows);
        }
        if let Some(pending) = self.pending_pane_click.as_mut() {
            pending.anchor.row = pending.anchor.row.saturating_add(rows);
        }
    }

    fn expire_copy_feedback(&mut self) {
        if self
            .copy_feedback
            .as_ref()
            .is_some_and(|feedback| std::time::Instant::now() >= feedback.expires_at)
        {
            self.copy_feedback = None;
            self.dirty = true;
        }
    }

    fn set_copy_feedback(&mut self, message: impl Into<String>, success: bool) {
        self.copy_feedback = Some(CopyFeedback {
            message: message.into(),
            success,
            expires_at: std::time::Instant::now() + COPY_FEEDBACK_DURATION,
        });
        self.dirty = true;
    }

    pub(crate) fn copy_feedback_badge(&self) -> Option<(String, ratatui::style::Color)> {
        self.copy_feedback.as_ref().and_then(|feedback| {
            (std::time::Instant::now() < feedback.expires_at).then(|| {
                let color = if feedback.success {
                    self.theme().green
                } else {
                    self.theme().red
                };
                (format!(" {} ", feedback.message), color)
            })
        })
    }

    pub(crate) fn current_copy_selection_range(
        &self,
    ) -> Option<crate::ghostty::render::SelectionRange> {
        self.active_copy_selection
            .and_then(|selection| self.viewport_selection_range(selection))
    }

    fn begin_pending_pane_click(&mut self, mouse: MouseEvent) -> bool {
        let Some(anchor) =
            mouse_to_pane_cell(mouse, false).and_then(|cell| self.pane_cell_to_buffer_point(cell))
        else {
            return false;
        };
        self.pending_pane_click = Some(PendingPaneClick { mouse, anchor });
        self.active_copy_selection = None;
        true
    }

    fn update_copy_selection_drag(&mut self, mouse: MouseEvent) -> bool {
        let Some(mouse_cell) = mouse_to_pane_cell_edge(mouse) else {
            self.clear_pane_copy_interaction();
            return false;
        };
        let cell = match mouse_cell {
            PaneMouseCell::Inside(cell)
            | PaneMouseCell::Above(cell)
            | PaneMouseCell::Below(cell) => cell,
        };
        let Some(focus) = Some(cell).and_then(|cell| self.pane_cell_to_buffer_point(cell)) else {
            self.clear_pane_copy_interaction();
            return false;
        };

        if let Some(selection) = self.active_copy_selection.as_mut() {
            selection.focus = focus;
            selection.last_drag = mouse;
            self.dirty = true;
            return true;
        }

        let Some(pending) = self.pending_pane_click.take() else {
            return false;
        };

        if pending.anchor == focus && matches!(mouse_cell, PaneMouseCell::Inside(_)) {
            self.pending_pane_click = Some(pending);
            return false;
        }

        self.active_copy_selection = Some(ActiveCopySelection {
            anchor: pending.anchor,
            focus,
            last_drag: mouse,
        });
        self.dirty = true;
        true
    }

    fn copy_active_selection(&mut self) -> bool {
        let Some(selection) = self.active_copy_selection.take() else {
            return false;
        };
        self.pending_pane_click = None;

        let Some(text) = self.current_pane_selection_text(selection) else {
            self.set_copy_feedback("copy failed", false);
            return true;
        };

        if text.is_empty() {
            self.set_copy_feedback("copy empty", false);
            return true;
        }

        let command_ok = write_text_to_system_clipboards(&text);
        let osc_ok = write_text_via_osc52(&text);
        let success = copy_backend_succeeded(command_ok, osc_ok);
        self.set_copy_feedback(
            if success {
                format!("copied {} chars", text.chars().count())
            } else {
                "copy failed".to_string()
            },
            success,
        );
        true
    }

    fn replay_pending_pane_click(
        &mut self,
        mouse_up: MouseEvent,
        pane: &str,
        show_overlay: bool,
        mouse_active: bool,
    ) -> bool {
        let Some(pending) = self.pending_pane_click.take() else {
            return false;
        };
        if mouse_to_pane_cell(mouse_up, false).is_none() {
            return true;
        }
        self.forward_mouse_to_pane(pending.mouse, pane, show_overlay, mouse_active);
        self.forward_mouse_to_pane(mouse_up, pane, show_overlay, mouse_active);
        true
    }

    fn viewport_selection_range(
        &self,
        selection: ActiveCopySelection,
    ) -> Option<crate::ghostty::render::SelectionRange> {
        let viewport_height = self.pane_viewport_height()?;
        let (_, view_scroll, total_lines) = self.current_pane_lines_scroll()?;
        let (visible_start, visible_end) =
            pane_visible_line_range(total_lines, view_scroll, viewport_height);
        if visible_start >= visible_end {
            return None;
        }

        let range = normalized_buffer_range(selection);
        let start_row = range.start_row as usize;
        let end_row = range.end_row as usize;
        let visible_selection_start = start_row.max(visible_start);
        let visible_selection_end = end_row.min(visible_end.saturating_sub(1));
        if visible_selection_start > visible_selection_end {
            return None;
        }

        let start_col = if visible_selection_start == start_row {
            range.start_col
        } else {
            0
        };
        let end_col = if visible_selection_end == end_row {
            range.end_col
        } else {
            u16::MAX
        };

        Some(crate::ghostty::render::SelectionRange::new(
            (
                start_col,
                (visible_selection_start - visible_start).min(u16::MAX as usize) as u16,
            ),
            (
                end_col,
                (visible_selection_end - visible_start).min(u16::MAX as usize) as u16,
            ),
        ))
    }

    fn current_pane_selection_text(&self, selection: ActiveCopySelection) -> Option<String> {
        let (term_cols, term_rows) = crossterm::terminal::size().ok()?;
        let inner = pane_inner_rect(term_cols, term_rows);
        if inner.width == 0 || inner.height == 0 {
            return None;
        }

        let viewport_height = inner.height as usize;
        let (lines, view_scroll, live_pane) = match &self.state {
            AppState::AgentView(idx) => {
                if self.agent_view_state.show_stopped_overlay {
                    return None;
                }
                (
                    self.agent_view_state.lines.as_slice(),
                    self.agent_view_state.view_scroll,
                    if self.agent_view_state.view_scroll == 0 {
                        self.agents
                            .get(*idx)
                            .map(|entry| entry.config.pane.as_str())
                    } else {
                        None
                    },
                )
            }
            AppState::GitViewer(gv) => (
                gv.lines.as_slice(),
                gv.view_scroll,
                if gv.view_scroll == 0 {
                    Some(gv.pane.as_str())
                } else {
                    None
                },
            ),
            AppState::TerminalView(tv) => (
                tv.lines.as_slice(),
                tv.view_scroll,
                if tv.view_scroll == 0 {
                    Some(tv.pane.as_str())
                } else {
                    None
                },
            ),
            _ => return None,
        };

        let buffer_range = normalized_buffer_range(selection);
        let (visible_start, visible_end) =
            pane_visible_line_range(lines.len(), view_scroll, viewport_height);
        let selection_within_live_view = buffer_range.start_row as usize >= visible_start
            && (buffer_range.end_row as usize) < visible_end;
        if let Some(pane) = live_pane
            && selection_within_live_view
            && let Ok(joined_text) = tmux::capture_pane_joined(pane)
        {
            let joined_text = joined_text.trim_end_matches('\n');
            let visible_range = selection_buffer_range_to_visible(buffer_range, visible_start)?;
            let grid = crate::ghostty::render::pane_text_grid_for_copy(
                joined_text.as_bytes(),
                inner.width,
                inner.height,
            );
            return Some(grid.extract_wrap_aware(visible_range));
        }

        if lines.is_empty() {
            return Some(String::new());
        }
        let all_text = lines.join("\r\n");
        let grid = crate::ghostty::render::pane_text_grid(
            all_text.as_bytes(),
            inner.width,
            lines.len().min(u16::MAX as usize) as u16,
        );
        Some(grid.extract(buffer_range))
    }

    fn pane_viewport_height(&self) -> Option<usize> {
        let (term_cols, term_rows) = crossterm::terminal::size().ok()?;
        let inner = pane_inner_rect(term_cols, term_rows);
        (inner.width > 0 && inner.height > 0).then_some(inner.height as usize)
    }

    fn current_pane_lines_scroll(&self) -> Option<(&[String], usize, usize)> {
        match &self.state {
            AppState::AgentView(_) => Some((
                self.agent_view_state.lines.as_slice(),
                self.agent_view_state.view_scroll,
                self.agent_view_state.lines.len(),
            )),
            AppState::GitViewer(gv) => Some((gv.lines.as_slice(), gv.view_scroll, gv.lines.len())),
            AppState::TerminalView(tv) => {
                Some((tv.lines.as_slice(), tv.view_scroll, tv.lines.len()))
            }
            _ => None,
        }
    }

    fn pane_cell_to_buffer_point(&self, cell: PaneCellPoint) -> Option<PaneBufferPoint> {
        let viewport_height = self.pane_viewport_height()?;
        let (_, view_scroll, total_lines) = self.current_pane_lines_scroll()?;
        let (visible_start, _) = pane_visible_line_range(total_lines, view_scroll, viewport_height);
        Some(PaneBufferPoint {
            col: cell.col,
            row: visible_start.saturating_add(cell.row as usize),
        })
    }

    fn apply_copy_autoscroll(&mut self) -> bool {
        let Some(selection) = self.active_copy_selection else {
            return false;
        };
        let Some(mouse_cell) = mouse_to_pane_cell_edge(selection.last_drag) else {
            return false;
        };

        let (direction, cell) = match mouse_cell {
            PaneMouseCell::Above(cell) => (CopyScrollDirection::Up, cell),
            PaneMouseCell::Below(cell) => (CopyScrollDirection::Down, cell),
            PaneMouseCell::Inside(_) => return false,
        };

        if !self.scroll_active_pane_for_copy(direction, MOUSE_WHEEL_SCROLL_LINES) {
            return false;
        }

        if let Some(focus) = self.pane_cell_to_buffer_point(cell)
            && let Some(selection) = self.active_copy_selection.as_mut()
        {
            selection.focus = focus;
        }
        self.dirty = true;
        true
    }

    fn scroll_active_pane_for_copy(
        &mut self,
        direction: CopyScrollDirection,
        lines: usize,
    ) -> bool {
        match &mut self.state {
            AppState::AgentView(_) => {
                let before = self.agent_view_state.view_scroll;
                self.agent_view_state.view_scroll = match direction {
                    CopyScrollDirection::Up => before.saturating_add(lines).min(MAX_RETAINED_LINES),
                    CopyScrollDirection::Down => before.saturating_sub(lines),
                };
                self.agent_view_state.view_scroll != before
            }
            AppState::GitViewer(gv) => {
                let before = gv.view_scroll;
                gv.view_scroll = match direction {
                    CopyScrollDirection::Up => before.saturating_add(lines).min(MAX_RETAINED_LINES),
                    CopyScrollDirection::Down => before.saturating_sub(lines),
                };
                gv.view_scroll != before
            }
            AppState::TerminalView(tv) => {
                let before = tv.view_scroll;
                tv.view_scroll = match direction {
                    CopyScrollDirection::Up => before.saturating_add(lines).min(MAX_RETAINED_LINES),
                    CopyScrollDirection::Down => before.saturating_sub(lines),
                };
                tv.view_scroll != before
            }
            _ => false,
        }
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

    pub fn global_status_counts(&self) -> AgentStatusCounts {
        AgentStatusCounts::for_agents(&self.agents)
    }

    fn reset_notification(&mut self) {
        let counts = self.global_status_counts();
        self.notification.reset(counts);
    }

    fn selected_visible_position(&self, visible_indices: &[usize]) -> Option<usize> {
        visible_indices.iter().position(|&idx| idx == self.selected)
    }

    fn remember_project_selection(&mut self, project_name: &str, idx: usize) {
        if self
            .agents
            .get(idx)
            .is_some_and(|entry| entry.config.project == project_name)
        {
            self.dashboard_selected_by_project
                .insert(project_name.to_string(), idx);
        }
    }

    fn remember_active_project_selection(&mut self) {
        let project_name = self.active_project_name().to_string();
        self.remember_project_selection(&project_name, self.selected);
    }

    fn set_dashboard_selected(&mut self, idx: usize) {
        self.selected = idx;
        self.remember_active_project_selection();
    }

    fn ensure_project_selection(&mut self) {
        let visible_indices = self.visible_agent_indices();
        if visible_indices.is_empty() {
            self.selected = self.selected.min(self.agents.len().saturating_sub(1));
            return;
        }

        if !visible_indices.contains(&self.selected) {
            self.set_dashboard_selected(visible_indices[0]);
        } else {
            self.remember_active_project_selection();
        }
    }

    fn set_active_project_idx(&mut self, idx: usize) {
        if idx >= self.config.projects.len() {
            return;
        }
        self.remember_active_project_selection();
        self.active_project_idx = idx;
        let project_name = self.active_project_name().to_string();
        if let Some(selected) = self
            .dashboard_selected_by_project
            .get(&project_name)
            .copied()
            .filter(|&selected| {
                self.agents
                    .get(selected)
                    .is_some_and(|entry| entry.config.project == project_name)
            })
        {
            self.selected = selected;
        }
        self.ensure_project_selection();
        self.dirty = true;
    }

    fn cycle_projects(&mut self) {
        if self.config.projects.is_empty() {
            return;
        }
        let next = (self.active_project_idx + 1) % self.config.projects.len();
        self.set_active_project_idx(next);
    }

    fn startup_guide_last_page(&self) -> usize {
        crate::ui::startup_guide::startup_guide_page_count().saturating_sub(1)
    }

    fn open_startup_guide(&mut self, persist_on_close: bool) {
        self.state = AppState::StartupGuide(if persist_on_close {
            StartupGuideState::first_run()
        } else {
            StartupGuideState::reopened()
        });
        self.dirty = true;
    }

    fn close_startup_guide(&mut self, persist_on_close: bool) {
        if persist_on_close {
            let global_config = self.runner.global_config_mut();
            if !global_config.startup_guide_dismissed {
                global_config.startup_guide_dismissed = true;
                let _ = global_config.save();
            }
        }
        self.state = AppState::Dashboard;
        self.dirty = true;
    }

    fn handle_startup_guide_key(&mut self, key: KeyEvent, state: StartupGuideState) -> bool {
        let last_page = self.startup_guide_last_page();
        match key.code {
            KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') => {
                self.close_startup_guide(state.persist_on_close);
            }
            KeyCode::Left | KeyCode::Char('h') => {
                if let AppState::StartupGuide(ref mut guide) = self.state {
                    guide.page = guide.page.saturating_sub(1);
                }
                self.dirty = true;
            }
            KeyCode::Right | KeyCode::Char('l') => {
                if let AppState::StartupGuide(ref mut guide) = self.state {
                    guide.page = guide.page.saturating_add(1).min(last_page);
                }
                self.dirty = true;
            }
            KeyCode::Home => {
                if let AppState::StartupGuide(ref mut guide) = self.state {
                    guide.page = 0;
                }
                self.dirty = true;
            }
            KeyCode::End => {
                if let AppState::StartupGuide(ref mut guide) = self.state {
                    guide.page = last_page;
                }
                self.dirty = true;
            }
            _ => {}
        }
        true
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

    fn persist_current_agent_view_scroll(&mut self) {
        let AppState::AgentView(idx) = self.state else {
            return;
        };
        if let Some(stored) = self.agent_view_scroll.get_mut(idx) {
            stored.view_scroll = self.agent_view_state.view_scroll;
            stored.viewport_height = self.agent_view_state.last_viewport_height;
        }
    }

    fn enter_agent_view(&mut self, idx: usize) {
        self.persist_current_agent_view_scroll();
        self.clear_pane_copy_interaction();
        let stored = self.agent_view_scroll.get(idx).copied().unwrap_or_default();
        self.selected = idx;
        let project_name = self
            .agents
            .get(idx)
            .map(|entry| entry.config.project.clone());
        if let Some(project_name) = project_name {
            self.remember_project_selection(&project_name, idx);
        }
        self.agent_view_state = AgentViewState {
            view_scroll: stored.view_scroll,
            last_viewport_height: stored.viewport_height,
            ..AgentViewState::default()
        };
        self.state = AppState::AgentView(idx);
        self.dirty = true;
    }

    fn exit_agent_view_to_dashboard(&mut self) {
        self.persist_current_agent_view_scroll();
        self.clear_pane_copy_interaction();
        self.state = AppState::Dashboard;
        self.dirty = true;
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
        self.expire_copy_feedback();
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
                self.apply_copy_autoscroll();
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
                self.apply_copy_autoscroll();
                self.handle_git_viewer_tick().await;
                if self.notification.is_blinking_running()
                    || self.notification.is_blinking_waiting()
                {
                    self.dirty = true;
                }
                true
            }
            Event::TerminalViewTick => {
                self.apply_copy_autoscroll();
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

    fn dashboard_project_tab_at(&self, col: u16, row: u16) -> Option<usize> {
        project_tab_at(&self.config.projects, col, row)
    }

    fn handle_dashboard_mouse(&mut self, mouse: MouseEvent) {
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(project_idx) = self.dashboard_project_tab_at(mouse.column, mouse.row) {
                    self.last_dashboard_left_click = None;
                    self.set_active_project_idx(project_idx);
                    return;
                }
                if let Some(slot) = self.dashboard_slot_at(mouse.column, mouse.row) {
                    if let Some(global_idx) = self.visible_agent_indices().get(slot).copied() {
                        self.set_dashboard_selected(global_idx);
                        let now = std::time::Instant::now();
                        let is_double_click = self
                            .last_dashboard_left_click
                            .map(|(last_idx, last_at)| {
                                last_idx == global_idx
                                    && now.duration_since(last_at) <= DASHBOARD_DOUBLE_CLICK_WINDOW
                            })
                            .unwrap_or(false);
                        if is_double_click {
                            self.last_dashboard_left_click = None;
                            self.open_agent_view(global_idx);
                            return;
                        }
                        self.last_dashboard_left_click = Some((global_idx, now));
                    }
                    self.dirty = true;
                }
            }
            MouseEventKind::ScrollUp => {
                if let Some(slot) = self.dashboard_slot_at(mouse.column, mouse.row)
                    && let Some(global_idx) = self.visible_agent_indices().get(slot).copied()
                    && let Some(s) = self.card_scroll.get_mut(global_idx)
                {
                    *s = s.saturating_sub(1);
                    self.dirty = true;
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

    fn open_agent_view(&mut self, idx: usize) {
        self.enter_agent_view(idx);
    }

    fn handle_mouse(&mut self, mouse: MouseEvent) {
        if matches!(self.state, AppState::Dashboard) {
            self.handle_dashboard_mouse(mouse);
            return;
        }

        match &self.state {
            AppState::StartupGuide(_) => match mouse.kind {
                MouseEventKind::ScrollUp => {
                    if let AppState::StartupGuide(ref mut guide) = self.state {
                        guide.page = guide.page.saturating_sub(1);
                    }
                    self.dirty = true;
                }
                MouseEventKind::ScrollDown => {
                    let last_page = self.startup_guide_last_page();
                    if let AppState::StartupGuide(ref mut guide) = self.state {
                        guide.page = guide.page.saturating_add(1).min(last_page);
                    }
                    self.dirty = true;
                }
                _ => {}
            },
            AppState::AgentView(idx) => {
                let idx = *idx;
                self.handle_agent_view_mouse(mouse, idx);
            }
            AppState::GitViewer(gv) => {
                let pane = gv.pane.clone();
                let mouse_active = gv.pane_mouse_active;
                if is_middle_button_down(mouse) {
                    self.paste_host_selection_to_pane(&pane);
                    return;
                }
                if is_middle_button_up(mouse) {
                    return;
                }
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
                        _ => {}
                    }
                }
                if matches!(mouse.kind, MouseEventKind::Up(MouseButton::Left))
                    && self.pending_pane_click.is_some()
                    && self.active_copy_selection.is_none()
                    && !pane_handles_own_scroll(mouse_active)
                    && let AppState::GitViewer(ref mut gv) = self.state
                {
                    gv.view_scroll = 0;
                }
                self.handle_pane_mouse_generic(mouse, &pane, mouse_active, false);
            }
            AppState::TerminalView(tv) => {
                let pane = tv.pane.clone();
                let mouse_active = tv.pane_mouse_active;
                if is_middle_button_down(mouse) {
                    self.paste_host_selection_to_pane(&pane);
                    return;
                }
                if is_middle_button_up(mouse) {
                    return;
                }
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
                        _ => {}
                    }
                }
                if matches!(mouse.kind, MouseEventKind::Up(MouseButton::Left))
                    && self.pending_pane_click.is_some()
                    && self.active_copy_selection.is_none()
                    && !pane_handles_own_scroll(mouse_active)
                    && let AppState::TerminalView(ref mut tv) = self.state
                {
                    tv.view_scroll = 0;
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
        if self.pending_pane_click.is_some() || self.active_copy_selection.is_some() {
            match mouse.kind {
                MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                    self.clear_pane_copy_interaction();
                }
                _ => {}
            }
        }

        if !show_overlay {
            match mouse.kind {
                MouseEventKind::Down(MouseButton::Left)
                    if is_unmodified_left_button(mouse) && self.begin_pending_pane_click(mouse) =>
                {
                    return;
                }
                MouseEventKind::Drag(MouseButton::Left)
                    if self.pending_pane_click.is_some()
                        || self.active_copy_selection.is_some() =>
                {
                    self.update_copy_selection_drag(mouse);
                    return;
                }
                MouseEventKind::Up(MouseButton::Left) => {
                    if self.active_copy_selection.is_some() {
                        self.copy_active_selection();
                        return;
                    }
                    if self.pending_pane_click.is_some() {
                        self.replay_pending_pane_click(mouse, pane, show_overlay, mouse_active);
                        return;
                    }
                }
                _ => {}
            }
        }

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
                    paste_text_to_pane(&entry.config.pane, &text);
                }
            }
            AppState::GitViewer(gv) => {
                let pane = gv.pane.clone();
                paste_text_to_pane(&pane, &text);
            }
            AppState::TerminalView(tv) => {
                let pane = tv.pane.clone();
                paste_text_to_pane(&pane, &text);
            }
            _ => {}
        }
    }

    fn paste_host_selection_to_pane(&mut self, pane: &str) -> bool {
        let Some(text) = read_host_selection(HostSelection::Primary)
            .or_else(|| read_host_selection(HostSelection::Clipboard))
        else {
            return false;
        };
        if text.is_empty() {
            return false;
        }
        paste_text_to_pane(pane, &text);
        true
    }

    async fn handle_key(&mut self, key: KeyEvent) -> bool {
        match &self.state.clone() {
            AppState::Dashboard => self.handle_dashboard_key(key),
            AppState::StartupGuide(state) => {
                let state = state.clone();
                self.handle_startup_guide_key(key, state)
            }
            AppState::SettingsDialog(state) => self.handle_settings_key(key, state.clone()),
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
        for selected in self.dashboard_selected_by_project.values_mut() {
            if *selected == self.selected {
                *selected = target;
            } else if *selected == target {
                *selected = self.selected;
            }
        }
        self.agents.swap(self.selected, target);
        self.adapters.swap(self.selected, target);
        self.config.agents.swap(self.selected, target);
        if self.agent_view_scroll.len() > self.selected.max(target) {
            self.agent_view_scroll.swap(self.selected, target);
        }
        self.swap_terminal_pane_ownership(self.selected, target);
        self.card_scroll.swap(self.selected, target);
        let max_idx = self.selected.max(target);
        if self.card_response_heights.len() > max_idx {
            self.card_response_heights.swap(self.selected, target);
            self.card_response_widths.swap(self.selected, target);
        }
        if let Some(ref mut tv) = self.terminal_view_state {
            if tv.agent_idx == self.selected {
                tv.agent_idx = target;
            } else if tv.agent_idx == target {
                tv.agent_idx = self.selected;
            }
        }
        if let AppState::TerminalView(ref mut tv) = self.state {
            if tv.agent_idx == self.selected {
                tv.agent_idx = target;
            } else if tv.agent_idx == target {
                tv.agent_idx = self.selected;
            }
        }
        self.set_dashboard_selected(target);
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
            KeyCode::Char('?') => self.open_startup_guide(false),
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
            KeyCode::Char('S') => {
                self.open_settings_dialog();
            }
            KeyCode::Char('p') => {
                self.create_project_state = CreateProjectState::default();
                self.state = AppState::CreateProjectDialog;
            }
            KeyCode::Char('d') if ctrl && self.active_project_name() != DEFAULT_PROJECT_NAME => {
                self.state = AppState::RemoveProjectDialog(RemoveProjectState {
                    idx: self.active_project_idx,
                    name: self.active_project_name().to_string(),
                    agent_count: self.current_project_agent_count(),
                    confirm_remove_agents: false,
                });
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
                    self.open_agent_view(idx);
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
                        self.set_dashboard_selected(visible_indices[selected_pos - 1]);
                        self.reset_card_scroll();
                    }
                }
            }
            KeyCode::Right | KeyCode::Char('l') => {
                if let Some(selected_pos) = selected_visible {
                    // Move right within row; at last col wrap to first slot
                    // of the next row, as long as a next card exists.
                    if selected_pos + 1 < visible_indices.len() {
                        self.set_dashboard_selected(visible_indices[selected_pos + 1]);
                        self.reset_card_scroll();
                    }
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if let Some(selected_pos) = selected_visible {
                    let (cols, _) = grid_layout(visible_indices.len());
                    if selected_pos >= cols {
                        self.set_dashboard_selected(visible_indices[selected_pos - cols]);
                        self.reset_card_scroll();
                    }
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if let Some(selected_pos) = selected_visible {
                    let (cols, _) = grid_layout(visible_indices.len());
                    if selected_pos + cols < visible_indices.len() {
                        self.set_dashboard_selected(visible_indices[selected_pos + cols]);
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

    fn handle_settings_key(&mut self, key: KeyEvent, state: SettingsState) -> bool {
        match key.code {
            KeyCode::Esc => self.cancel_settings_dialog(state),
            KeyCode::Enter => self.confirm_settings_dialog(state),
            KeyCode::Up | KeyCode::Char('k') => {
                if let AppState::SettingsDialog(ref mut settings) = self.state
                    && settings.selected_idx > 0
                {
                    settings.selected_idx -= 1;
                    let theme_id = theme_by_index(settings.selected_idx).id.to_string();
                    self.set_theme_preview_by_id(&theme_id);
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if let AppState::SettingsDialog(ref mut settings) = self.state
                    && settings.selected_idx + 1 < crate::ui::theme::builtin_themes().len()
                {
                    settings.selected_idx += 1;
                    let theme_id = theme_by_index(settings.selected_idx).id.to_string();
                    self.set_theme_preview_by_id(&theme_id);
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
            if let Some(agent_config) = self.config.agents.get_mut(i)
                && session_id.is_some()
                && session_id.as_deref() != agent_config.session_id()
            {
                agent_config.set_session_id(session_id);
                config_dirty = true;
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
        if self.agent_view_scroll.len() < self.agents.len() {
            self.agent_view_scroll
                .resize(self.agents.len(), StoredAgentViewScroll::default());
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

            let capture_history =
                self.agent_view_state.view_scroll > 0 || self.active_copy_selection.is_some();
            if let Ok(raw) = if capture_history {
                tmux::capture_pane_history(&pane, MAX_RETAINED_LINES)
            } else {
                tmux::capture_pane(&pane)
            } {
                let old_lines = if self.active_copy_selection.is_some() {
                    self.agent_view_state.lines.clone()
                } else {
                    Vec::new()
                };
                // update_lines returns true only when content changed.
                if self.agent_view_state.update_lines(&raw) {
                    if capture_history {
                        let prepended_rows =
                            prepended_row_count(&old_lines, &self.agent_view_state.lines)
                                .unwrap_or(0);
                        self.shift_active_copy_selection_rows(prepended_rows);
                    }
                    self.dirty = true;
                }
            }

            let viewport_height = pane_content_height();
            if let Some(previous_height) = self.agent_view_state.last_viewport_height
                && previous_height != viewport_height
            {
                let new_scroll = resized_view_scroll(
                    self.agent_view_state.lines.len(),
                    self.agent_view_state.view_scroll,
                    previous_height,
                    viewport_height,
                );
                if new_scroll != self.agent_view_state.view_scroll {
                    self.agent_view_state.view_scroll = new_scroll;
                    self.dirty = true;
                }
            }
            self.agent_view_state.last_viewport_height = Some(viewport_height);

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
        if matches!(key.code, KeyCode::Esc) && self.active_copy_selection.is_some() {
            self.clear_pane_copy_interaction();
            self.dirty = true;
            return true;
        }

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
                    self.exit_agent_view_to_dashboard();
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
                    self.exit_agent_view_to_dashboard();
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
                self.exit_agent_view_to_dashboard();
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

        self.persist_current_agent_view_scroll();
        self.clear_pane_copy_interaction();
        self.git_viewer_state = Some(GitViewerState::new(agent_idx, pane.clone()));
        self.state = AppState::GitViewer(self.git_viewer_state.clone().unwrap());
        self.dirty = true;
    }

    fn handle_git_viewer_key(&mut self, key: KeyEvent) -> bool {
        if matches!(key.code, KeyCode::Esc) && self.active_copy_selection.is_some() {
            self.clear_pane_copy_interaction();
            self.dirty = true;
            return true;
        }

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
            KeyCode::Insert if key.modifiers.contains(KeyModifiers::SHIFT) => {
                self.paste_host_selection_to_pane(&pane);
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

        if let Some(colon_pos) = pane.find(':')
            && let Some(dot_pos) = pane[colon_pos..].find('.')
        {
            let window_target = &pane[..colon_pos + dot_pos];
            let _ = tmux::kill_window(window_target);
        }

        self.enter_agent_view(agent_idx);
        self.git_viewer_state = None;
    }

    fn exit_git_viewer_to_dashboard(&mut self) {
        let pane = match &self.state {
            AppState::GitViewer(gv) => gv.pane.clone(),
            _ => return,
        };

        if let Some(colon_pos) = pane.find(':')
            && let Some(dot_pos) = pane[colon_pos..].find('.')
        {
            let window_target = &pane[..colon_pos + dot_pos];
            let _ = tmux::kill_window(window_target);
        }

        self.clear_pane_copy_interaction();
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

        if let AppState::GitViewer(ref mut gv) = self.state
            && let Ok((term_cols, term_rows)) = crossterm::terminal::size()
        {
            let desired = tmux_pane_viewport_size(term_cols, term_rows);
            if gv.last_pane_size != Some(desired) {
                let _ = tmux::resize_window(&pane, desired.0, desired.1);
                gv.last_pane_size = Some(desired);
            }
        }

        let mut prepended_rows = 0;
        let copy_selection_active = self.active_copy_selection.is_some();
        let changed = if let AppState::GitViewer(ref mut gv) = self.state {
            if gv.view_scroll > 0 || copy_selection_active {
                tmux::capture_pane_history(&pane, MAX_RETAINED_LINES)
                    .ok()
                    .map(|raw| {
                        let old_lines = if copy_selection_active {
                            gv.lines.clone()
                        } else {
                            Vec::new()
                        };
                        let changed = gv.update_lines(&raw);
                        if changed {
                            prepended_rows =
                                prepended_row_count(&old_lines, &gv.lines).unwrap_or(0);
                        }
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
        self.shift_active_copy_selection_rows(prepended_rows);
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
                self.persist_current_agent_view_scroll();
                self.clear_pane_copy_interaction();
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
        self.persist_current_agent_view_scroll();
        self.clear_pane_copy_interaction();
        self.terminal_view_state = Some(TerminalViewState::new(agent_idx, pane));
        self.state = AppState::TerminalView(self.terminal_view_state.clone().unwrap());
        self.dirty = true;
    }

    fn handle_terminal_view_key(&mut self, key: KeyEvent) -> bool {
        if matches!(key.code, KeyCode::Esc) && self.active_copy_selection.is_some() {
            self.clear_pane_copy_interaction();
            self.dirty = true;
            return true;
        }

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
            KeyCode::Insert if key.modifiers.contains(KeyModifiers::SHIFT) => {
                self.paste_host_selection_to_pane(&pane);
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

        self.enter_agent_view(agent_idx);
        self.terminal_view_state = None;
    }

    fn exit_terminal_to_dashboard(&mut self) {
        self.clear_pane_copy_interaction();
        self.state = AppState::Dashboard;
        self.terminal_view_state = None;
        self.dirty = true;
    }

    fn swap_terminal_pane_ownership(&mut self, first: usize, second: usize) {
        let first_pane = self.terminal_panes.remove(&first);
        let second_pane = self.terminal_panes.remove(&second);
        if let Some(pane) = first_pane {
            self.terminal_panes.insert(second, pane);
        }
        if let Some(pane) = second_pane {
            self.terminal_panes.insert(first, pane);
        }
    }

    fn reindex_state_after_agent_removal(&mut self, removed_idx: usize) {
        if removed_idx < self.agent_view_scroll.len() {
            self.agent_view_scroll.remove(removed_idx);
        }
        if removed_idx < self.card_scroll.len() {
            self.card_scroll.remove(removed_idx);
        }
        if removed_idx < self.card_response_heights.len() {
            self.card_response_heights.remove(removed_idx);
        }
        if removed_idx < self.card_response_widths.len() {
            self.card_response_widths.remove(removed_idx);
        }

        let mut reindexed_terminal_panes =
            std::collections::HashMap::with_capacity(self.terminal_panes.len());
        for (agent_idx, pane) in self.terminal_panes.drain() {
            if agent_idx == removed_idx {
                continue;
            }
            let new_idx = if agent_idx > removed_idx {
                agent_idx - 1
            } else {
                agent_idx
            };
            reindexed_terminal_panes.insert(new_idx, pane);
        }
        self.terminal_panes = reindexed_terminal_panes;

        let mut clear_live_terminal_view = false;
        if let AppState::TerminalView(ref mut tv) = self.state {
            if tv.agent_idx == removed_idx {
                clear_live_terminal_view = true;
            } else if tv.agent_idx > removed_idx {
                tv.agent_idx -= 1;
            }
        }
        if let Some(ref mut tv) = self.terminal_view_state {
            if tv.agent_idx == removed_idx {
                clear_live_terminal_view = true;
            } else if tv.agent_idx > removed_idx {
                tv.agent_idx -= 1;
            }
        }
        if clear_live_terminal_view {
            self.clear_pane_copy_interaction();
            self.state = AppState::Dashboard;
            self.terminal_view_state = None;
        }
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

        if let AppState::TerminalView(ref mut tv) = self.state
            && let Ok((term_cols, term_rows)) = crossterm::terminal::size()
        {
            let desired = tmux_pane_viewport_size(term_cols, term_rows);
            if tv.last_pane_size != Some(desired) {
                let _ = tmux::resize_window(&pane, desired.0, desired.1);
                tv.last_pane_size = Some(desired);
            }
        }

        let mut prepended_rows = 0;
        let copy_selection_active = self.active_copy_selection.is_some();
        let changed = if let AppState::TerminalView(ref mut tv) = self.state {
            if tv.view_scroll > 0 || copy_selection_active {
                tmux::capture_pane_history(&pane, MAX_RETAINED_LINES)
                    .ok()
                    .map(|raw| {
                        let old_lines = if copy_selection_active {
                            tv.lines.clone()
                        } else {
                            Vec::new()
                        };
                        let changed = tv.update_lines(&raw);
                        if changed {
                            prepended_rows =
                                prepended_row_count(&old_lines, &tv.lines).unwrap_or(0);
                        }
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
        self.shift_active_copy_selection_rows(prepended_rows);
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

                    if let Some(should_commit) = action
                        && should_commit
                    {
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
                    self.create_state.disable_empty_selector(&current_focus);
                }

                self.create_state.focus = next_create_field(
                    &current_focus,
                    self.create_state.git_repo_root.is_some(),
                    self.create_state.create_worktree,
                    self.create_state.has_git_submodules,
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
                    if self.create_state.selector_enabled(&focus)
                        && let Some(selector) = self.create_state.selector_mut(&focus)
                    {
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
                CreateField::AgentType => {
                    let n = self.create_state.available_types.len();
                    if n > 0 {
                        let idx = self.create_state.selected_type_idx;
                        self.create_state.selected_type_idx = idx.saturating_sub(1);
                    }
                }
                CreateField::Name
                | CreateField::CreateWorktree
                | CreateField::InitializeSubmodules => {}
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
                    if self.create_state.selector_enabled(&focus)
                        && let Some(selector) = self.create_state.selector_mut(&focus)
                    {
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
                CreateField::AgentType => {
                    let n = self.create_state.available_types.len();
                    if n > 0 {
                        let idx = self.create_state.selected_type_idx;
                        self.create_state.selected_type_idx = (idx + 1).min(n - 1);
                    }
                }
                CreateField::Name
                | CreateField::CreateWorktree
                | CreateField::InitializeSubmodules => {}
            },

            KeyCode::Left if self.create_state.focus == CreateField::Name => {
                self.create_state.move_name_cursor_left();
            }

            KeyCode::Right if self.create_state.focus == CreateField::Name => {
                self.create_state.move_name_cursor_right();
            }

            KeyCode::Home if self.create_state.focus == CreateField::Name => {
                self.create_state.move_name_cursor_home();
            }

            KeyCode::End if self.create_state.focus == CreateField::Name => {
                self.create_state.move_name_cursor_end();
            }

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
                    if let Some(selector) = self.create_state.selector_mut(&focus)
                        && selector.descend()
                    {
                        selector.refresh_matches(&directory);
                        self.create_state.error = None;
                    }
                } else if self.create_state.is_valid() {
                    let name = tmux::sanitize_name(&self.create_state.name.clone());
                    let dir = self.create_state.directory.clone();
                    let project = self.active_project_name().to_string();
                    let agent_type = self.create_state.selected_agent_type();
                    let create_worktree = self.create_state.create_worktree
                        && self.create_state.git_repo_root.is_some();
                    let initialize_submodules =
                        create_worktree && self.create_state.initialize_submodules_visible();
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
                            initialize_submodules,
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
                            self.agent_view_scroll
                                .push(StoredAgentViewScroll::default());
                            let new_idx = self.agents.len() - 1;
                            self.enter_agent_view(new_idx);
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
                        self.create_state.ctrl_w_delete_name();
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
                    CreateField::AgentType
                    | CreateField::CreateWorktree
                    | CreateField::InitializeSubmodules => {}
                }
            }

            KeyCode::Char('w') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                // Ctrl+W: delete back to the last word boundary (unix shell style)
                match self.create_state.focus {
                    CreateField::Name => {
                        self.create_state.ctrl_w_delete_name();
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
                    CreateField::AgentType
                    | CreateField::CreateWorktree
                    | CreateField::InitializeSubmodules => {}
                }
            }

            KeyCode::Backspace => {
                match self.create_state.focus {
                    CreateField::Name => {
                        self.create_state.backspace_name();
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
                    CreateField::AgentType
                    | CreateField::CreateWorktree
                    | CreateField::InitializeSubmodules => {}
                }
            }

            KeyCode::Char(c) => {
                match self.create_state.focus {
                    CreateField::Name => {
                        self.create_state.insert_name_char(c);
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
                        if c == ' ' && self.create_state.git_repo_root.is_some() {
                            self.create_state.create_worktree = !self.create_state.create_worktree;
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
                                    self.create_state.has_git_submodules,
                                );
                            }
                        }
                    }
                    CreateField::InitializeSubmodules => {
                        if c == ' ' && self.create_state.initialize_submodules_visible() {
                            self.create_state.initialize_submodules =
                                !self.create_state.initialize_submodules;
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
                let trimmed = self.create_project_state.name.value.trim();
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
                self.create_project_state.name.ctrl_w_delete();
                self.create_project_state.error = None;
            }
            KeyCode::Char('w') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.create_project_state.name.ctrl_w_delete();
                self.create_project_state.error = None;
            }
            KeyCode::Backspace => {
                self.create_project_state.name.backspace();
                self.create_project_state.error = None;
            }
            KeyCode::Left => {
                self.create_project_state.name.move_left();
            }
            KeyCode::Right => {
                self.create_project_state.name.move_right();
            }
            KeyCode::Home => {
                self.create_project_state.name.move_home();
            }
            KeyCode::End => {
                self.create_project_state.name.move_end();
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.create_project_state.name.insert_char(c);
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
            KeyCode::Char('y') | KeyCode::Enter
                if state.agent_count == 0 || state.confirm_remove_agents =>
            {
                self.remove_project(state.idx).await;
                self.state = AppState::Dashboard;
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
        let Some(next) = next_agent_by_status(&self.agents, current, target, None) else {
            return;
        };

        if next != current {
            if let Some(project_idx) = self
                .config
                .projects
                .iter()
                .position(|project| project == &self.agents[next].config.project)
            {
                self.set_active_project_idx(project_idx);
            }
            self.enter_agent_view(next);
        }
    }

    async fn remove_agent(&mut self, idx: usize, remove_worktree: bool, stop_agent: bool) {
        if idx < self.agents.len() {
            if let Some(agent_config) = self.config.agents.get(idx) {
                if stop_agent {
                    if let Err(error) = self.adapters[idx].stop().await {
                        log::warn!("failed to stop agent: {error}");
                    }
                    // Extract window target from pane (e.g., "flowmux:1.0" -> "flowmux:1")
                    if let Some(colon_pos) = agent_config.pane.find(':')
                        && let Some(dot_pos) = agent_config.pane[colon_pos..].find('.')
                    {
                        let window_target = &agent_config.pane[..colon_pos + dot_pos];
                        if let Err(error) = tmux::kill_window(window_target) {
                            log::warn!("failed to kill tmux window {window_target}: {error}");
                        }
                    }
                }

                // Remove the git worktree if requested and present.
                if remove_worktree
                    && let (Some(wt_path), Some(repo_root)) = (
                        Some(agent_config.directory.as_str()),
                        agent_config.git_repo_root.as_deref(),
                    )
                {
                    let branch = crate::git::sanitize_branch_name(&agent_config.name);
                    // Non-fatal: log error but continue removal.
                    if let Err(e) = crate::git::remove_worktree(
                        std::path::Path::new(repo_root),
                        std::path::Path::new(wt_path),
                        &branch,
                        true,
                    ) {
                        log::warn!("failed to remove git worktree: {e}");
                    }
                }
            }
            self.agents.remove(idx);
            self.adapters.remove(idx);
            self.config.agents.remove(idx);
            self.dashboard_selected_by_project.retain(|_, selected| {
                if *selected == idx {
                    false
                } else {
                    if *selected > idx {
                        *selected -= 1;
                    }
                    true
                }
            });
            self.reindex_state_after_agent_removal(idx);
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
            self.dashboard_selected_by_project.remove(&project_name);
            self.config.projects.remove(project_idx);
            self.config.normalize();
            let next_idx = project_idx.min(self.config.projects.len().saturating_sub(1));
            self.active_project_idx = next_idx;
            self.ensure_project_selection();
            self.reset_notification();
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
    current: usize,
    target: AgentStatus,
    fallback: Option<AgentStatus>,
) -> Option<usize> {
    let now = std::time::Instant::now();
    let mut matches = matching_agent_indices(agents, &target);

    if matches.is_empty()
        && let Some(fallback) = fallback
    {
        matches = matching_agent_indices(agents, &fallback);
    }

    matches.sort_by_key(|&idx| agents[idx].meta.status_changed_at.unwrap_or(now));

    match matches.iter().position(|&idx| idx == current) {
        Some(pos) => Some(matches[(pos + 1) % matches.len()]),
        None => matches.first().copied(),
    }
}

fn matching_agent_indices(agents: &[AgentEntry], status: &AgentStatus) -> Vec<usize> {
    agents
        .iter()
        .enumerate()
        .filter(|(_, entry)| entry.meta.status == *status)
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

fn project_tab_at(projects: &[String], col: u16, row: u16) -> Option<usize> {
    if row >= PROJECT_TABS_HEIGHT {
        return None;
    }

    let mut current_col = 1u16;
    for (idx, project) in projects.iter().enumerate() {
        let tab_width = project_tab_label(idx, project).chars().count() as u16;
        let tab_end = current_col.saturating_add(tab_width);
        if (current_col..tab_end).contains(&col) {
            return Some(idx);
        }
        current_col = tab_end.saturating_add(1);
    }

    None
}

fn is_middle_button_down(mouse: MouseEvent) -> bool {
    matches!(mouse.kind, MouseEventKind::Down(MouseButton::Middle))
}

fn is_middle_button_up(mouse: MouseEvent) -> bool {
    matches!(mouse.kind, MouseEventKind::Up(MouseButton::Middle))
}

fn paste_text_to_pane(pane: &str, text: &str) {
    let seq = format!("\x1b[200~{}\x1b[201~", text);
    let _ = tmux::send_literal(pane, &seq);
}

#[derive(Clone, Copy)]
enum HostSelection {
    Primary,
    Clipboard,
}

fn read_host_selection(selection: HostSelection) -> Option<String> {
    host_selection_commands(selection)
        .into_iter()
        .find_map(|(program, args)| read_host_selection_command(program, args))
}

fn host_selection_commands(selection: HostSelection) -> Vec<(&'static str, Vec<&'static str>)> {
    match selection {
        HostSelection::Primary => vec![
            ("wl-paste", vec!["--no-newline", "--primary"]),
            ("xclip", vec!["-selection", "primary", "-o"]),
            ("xsel", vec!["--primary", "--output"]),
        ],
        HostSelection::Clipboard => vec![
            ("wl-paste", vec!["--no-newline"]),
            ("xclip", vec!["-selection", "clipboard", "-o"]),
            ("xsel", vec!["--clipboard", "--output"]),
            ("pbpaste", vec![]),
        ],
    }
}

fn read_host_selection_command(program: &str, args: Vec<&str>) -> Option<String> {
    let output = Command::new(program).args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout).ok()
}

fn write_text_to_system_clipboards(text: &str) -> bool {
    let clipboard_ok = write_text_to_system_selection(HostSelection::Clipboard, text);
    let primary_ok = write_text_to_system_selection(HostSelection::Primary, text);
    system_clipboards_succeeded(clipboard_ok, primary_ok)
}

fn write_text_to_system_selection(selection: HostSelection, text: &str) -> bool {
    host_selection_write_commands(selection)
        .into_iter()
        .any(|(program, args)| write_text_with_command(program, args, text))
}

fn host_selection_write_commands(
    selection: HostSelection,
) -> Vec<(&'static str, Vec<&'static str>)> {
    match selection {
        HostSelection::Primary => vec![
            ("wl-copy", vec!["--primary"]),
            ("xclip", vec!["-selection", "primary", "-in"]),
            ("xsel", vec!["--primary", "--input"]),
        ],
        HostSelection::Clipboard => vec![
            ("wl-copy", vec![]),
            ("xclip", vec!["-selection", "clipboard", "-in"]),
            ("xsel", vec!["--clipboard", "--input"]),
            ("pbcopy", vec![]),
            ("clip.exe", vec![]),
        ],
    }
}

fn write_text_with_command(program: &str, args: Vec<&str>, text: &str) -> bool {
    let mut child = match Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(_) => return false,
    };

    if let Some(stdin) = child.stdin.as_mut()
        && stdin.write_all(text.as_bytes()).is_err()
    {
        let _ = child.kill();
        let _ = child.wait();
        return false;
    }

    child.wait().map(|status| status.success()).unwrap_or(false)
}

fn write_text_via_osc52(text: &str) -> bool {
    let encoded = {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD.encode(text.as_bytes())
    };

    let seq = if std::env::var_os("TMUX").is_some() {
        format!("\x1bPtmux;\x1b\x1b]52;c;{}\x07\x1b\\", encoded)
    } else {
        format!("\x1b]52;c;{}\x07", encoded)
    };

    let mut stdout = std::io::stdout();
    stdout.write_all(seq.as_bytes()).is_ok() && stdout.flush().is_ok()
}

fn system_clipboards_succeeded(clipboard_ok: bool, primary_ok: bool) -> bool {
    clipboard_ok || primary_ok
}

fn copy_backend_succeeded(system_ok: bool, osc_ok: bool) -> bool {
    system_ok || osc_ok
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

pub(crate) fn pane_visible_text(
    lines: &[String],
    view_scroll: usize,
    viewport_height: usize,
) -> String {
    let (start, end) = pane_visible_line_range(lines.len(), view_scroll, viewport_height);
    if lines.is_empty() {
        String::new()
    } else {
        lines[start..end].join("\r\n")
    }
}

fn prepended_row_count(old_lines: &[String], new_lines: &[String]) -> Option<usize> {
    if old_lines.is_empty() || new_lines.len() < old_lines.len() {
        return None;
    }
    let start = new_lines.len() - old_lines.len();
    (new_lines[start..] == *old_lines).then_some(start)
}

fn normalized_buffer_range(
    selection: ActiveCopySelection,
) -> crate::ghostty::render::SelectionRange {
    selection.buffer_range()
}

fn selection_buffer_range_to_visible(
    selection: crate::ghostty::render::SelectionRange,
    visible_start: usize,
) -> Option<crate::ghostty::render::SelectionRange> {
    let start_row = (selection.start_row as usize).checked_sub(visible_start)?;
    let end_row = (selection.end_row as usize).checked_sub(visible_start)?;
    Some(crate::ghostty::render::SelectionRange::new(
        (selection.start_col, start_row.min(u16::MAX as usize) as u16),
        (selection.end_col, end_row.min(u16::MAX as usize) as u16),
    ))
}

fn resized_view_scroll(
    total_lines: usize,
    view_scroll: usize,
    old_viewport_height: usize,
    new_viewport_height: usize,
) -> usize {
    if total_lines == 0 || old_viewport_height == new_viewport_height {
        return view_scroll.min(total_lines.saturating_sub(new_viewport_height));
    }
    let (start, _) = pane_visible_line_range(total_lines, view_scroll, old_viewport_height);
    let end = total_lines.min(start.saturating_add(new_viewport_height));
    total_lines.saturating_sub(end)
}

fn clamp_external_pane_scroll(view_scroll: &mut usize, total_lines: usize) {
    let max_scroll = total_lines.saturating_sub(pane_content_height());
    if *view_scroll > max_scroll {
        *view_scroll = max_scroll;
    }
}

fn pane_inner_rect(term_cols: u16, term_rows: u16) -> Rect {
    Rect::new(
        1,
        2,
        term_cols.saturating_sub(PANE_BORDER_WIDTH),
        term_rows.saturating_sub(PANE_CHROME_HEIGHT),
    )
}

fn mouse_to_pane_cell(mouse: MouseEvent, clamp: bool) -> Option<PaneCellPoint> {
    let (term_cols, term_rows) = crossterm::terminal::size().ok()?;
    let inner = pane_inner_rect(term_cols, term_rows);
    mouse_to_pane_cell_in_rect(mouse, inner, clamp)
}

fn mouse_to_pane_cell_in_rect(
    mouse: MouseEvent,
    inner: Rect,
    clamp: bool,
) -> Option<PaneCellPoint> {
    if inner.width == 0 || inner.height == 0 {
        return None;
    }

    let col = if clamp {
        mouse.column.clamp(inner.x, inner.x + inner.width - 1)
    } else if mouse.column >= inner.x && mouse.column < inner.x + inner.width {
        mouse.column
    } else {
        return None;
    };

    let row = if clamp {
        mouse.row.clamp(inner.y, inner.y + inner.height - 1)
    } else if mouse.row >= inner.y && mouse.row < inner.y + inner.height {
        mouse.row
    } else {
        return None;
    };

    Some(PaneCellPoint {
        col: col.saturating_sub(inner.x),
        row: row.saturating_sub(inner.y),
    })
}

fn mouse_to_pane_cell_edge(mouse: MouseEvent) -> Option<PaneMouseCell> {
    let (term_cols, term_rows) = crossterm::terminal::size().ok()?;
    let inner = pane_inner_rect(term_cols, term_rows);
    mouse_to_pane_cell_edge_in_rect(mouse, inner)
}

fn mouse_to_pane_cell_edge_in_rect(mouse: MouseEvent, inner: Rect) -> Option<PaneMouseCell> {
    if inner.width == 0 || inner.height == 0 {
        return None;
    }

    let col = mouse.column.clamp(inner.x, inner.x + inner.width - 1);
    let point_col = col.saturating_sub(inner.x);
    if mouse.row < inner.y {
        return Some(PaneMouseCell::Above(PaneCellPoint {
            col: point_col,
            row: 0,
        }));
    }
    if mouse.row >= inner.y + inner.height {
        return Some(PaneMouseCell::Below(PaneCellPoint {
            col: point_col,
            row: inner.height - 1,
        }));
    }

    Some(PaneMouseCell::Inside(PaneCellPoint {
        col: point_col,
        row: mouse.row.saturating_sub(inner.y),
    }))
}

fn is_unmodified_left_button(mouse: MouseEvent) -> bool {
    mouse.modifiers.is_empty()
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
    fn system_clipboard_write_commands_include_clipboard_and_primary_targets() {
        assert_eq!(
            host_selection_write_commands(HostSelection::Clipboard),
            vec![
                ("wl-copy", vec![]),
                ("xclip", vec!["-selection", "clipboard", "-in"]),
                ("xsel", vec!["--clipboard", "--input"]),
                ("pbcopy", vec![]),
                ("clip.exe", vec![]),
            ]
        );
        assert_eq!(
            host_selection_write_commands(HostSelection::Primary),
            vec![
                ("wl-copy", vec!["--primary"]),
                ("xclip", vec!["-selection", "primary", "-in"]),
                ("xsel", vec!["--primary", "--input"]),
            ]
        );
    }

    #[test]
    fn copy_success_accepts_any_system_selection_or_osc52_backend() {
        assert!(system_clipboards_succeeded(true, false));
        assert!(system_clipboards_succeeded(false, true));
        assert!(!system_clipboards_succeeded(false, false));

        assert!(copy_backend_succeeded(true, false));
        assert!(copy_backend_succeeded(false, true));
        assert!(!copy_backend_succeeded(false, false));
    }

    #[test]
    fn pane_visible_line_range_slices_from_bottom_with_offset() {
        assert_eq!(pane_visible_line_range(6, 0, 3), (3, 6));
        assert_eq!(pane_visible_line_range(6, 2, 3), (1, 4));
        assert_eq!(pane_visible_line_range(2, 8, 3), (0, 2));
    }

    #[test]
    fn prepended_row_count_detects_history_prefix_growth() {
        let old = vec!["visible-a".to_string(), "visible-b".to_string()];
        let new = vec![
            "history-a".to_string(),
            "history-b".to_string(),
            "visible-a".to_string(),
            "visible-b".to_string(),
        ];
        assert_eq!(prepended_row_count(&old, &new), Some(2));

        let appended = vec![
            "visible-a".to_string(),
            "visible-b".to_string(),
            "live-c".to_string(),
        ];
        assert_eq!(prepended_row_count(&old, &appended), None);
    }

    #[test]
    fn mouse_to_pane_cell_edge_classifies_vertical_edges_and_clamps_columns() {
        let inner = Rect {
            x: 10,
            y: 5,
            width: 20,
            height: 6,
        };

        let above = MouseEvent {
            kind: MouseEventKind::Drag(MouseButton::Left),
            column: 4,
            row: 2,
            modifiers: KeyModifiers::empty(),
        };
        assert_eq!(
            mouse_to_pane_cell_edge_in_rect(above, inner),
            Some(PaneMouseCell::Above(PaneCellPoint { col: 0, row: 0 }))
        );

        let below = MouseEvent {
            kind: MouseEventKind::Drag(MouseButton::Left),
            column: 99,
            row: 12,
            modifiers: KeyModifiers::empty(),
        };
        assert_eq!(
            mouse_to_pane_cell_edge_in_rect(below, inner),
            Some(PaneMouseCell::Below(PaneCellPoint { col: 19, row: 5 }))
        );

        let inside = MouseEvent {
            kind: MouseEventKind::Drag(MouseButton::Left),
            column: 13,
            row: 8,
            modifiers: KeyModifiers::empty(),
        };
        assert_eq!(
            mouse_to_pane_cell_edge_in_rect(inside, inner),
            Some(PaneMouseCell::Inside(PaneCellPoint { col: 3, row: 3 }))
        );
    }

    #[test]
    fn selection_buffer_range_to_visible_preserves_columns() {
        let selection = crate::ghostty::render::SelectionRange::new((4, 10), (7, 14));
        let visible = selection_buffer_range_to_visible(selection, 10).unwrap();

        assert_eq!(
            visible,
            crate::ghostty::render::SelectionRange::new((4, 0), (7, 4))
        );
    }

    #[test]
    fn active_copy_selection_normalizes_buffer_rows() {
        let drag = MouseEvent {
            kind: MouseEventKind::Drag(MouseButton::Left),
            column: 1,
            row: 1,
            modifiers: KeyModifiers::empty(),
        };
        let selection = ActiveCopySelection {
            anchor: PaneBufferPoint { col: 8, row: 12 },
            focus: PaneBufferPoint { col: 2, row: 9 },
            last_drag: drag,
        };

        assert_eq!(
            normalized_buffer_range(selection),
            crate::ghostty::render::SelectionRange::new((2, 9), (8, 12))
        );
    }

    #[test]
    fn project_tab_hit_testing_matches_rendered_tab_positions() {
        let projects = vec!["Default".into(), "work".into(), "other".into()];

        assert_eq!(project_tab_at(&projects, 0, 0), None);
        assert_eq!(project_tab_at(&projects, 1, 0), Some(0));
        assert_eq!(project_tab_at(&projects, 11, 0), Some(0));
        assert_eq!(project_tab_at(&projects, 12, 0), None);
        assert_eq!(project_tab_at(&projects, 13, 0), Some(1));
        assert_eq!(project_tab_at(&projects, 20, 0), Some(1));
        assert_eq!(project_tab_at(&projects, 21, 0), None);
        assert_eq!(project_tab_at(&projects, 22, 0), Some(2));
        assert_eq!(project_tab_at(&projects, 30, 0), Some(2));
        assert_eq!(project_tab_at(&projects, 31, 0), None);
        assert_eq!(project_tab_at(&projects, 5, 1), None);
    }

    #[test]
    fn project_tab_hit_testing_supports_tenth_project_zero_label() {
        let projects = (0..10).map(|idx| format!("p{idx}")).collect::<Vec<_>>();
        let tab_col = 1 + projects
            .iter()
            .take(9)
            .enumerate()
            .map(|(idx, project)| project_tab_label(idx, project).chars().count() as u16 + 1)
            .sum::<u16>();

        assert_eq!(project_tab_at(&projects, tab_col, 0), Some(9));
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

fn clamp_text_cursor(text: &str, cursor: &mut usize) {
    *cursor = (*cursor).min(text.len());
    while *cursor > 0 && !text.is_char_boundary(*cursor) {
        *cursor -= 1;
    }
}

fn move_text_cursor_left(text: &str, cursor: &mut usize) {
    clamp_text_cursor(text, cursor);
    if *cursor == 0 {
        return;
    }
    *cursor = text[..*cursor]
        .char_indices()
        .last()
        .map(|(idx, _)| idx)
        .unwrap_or(0);
}

fn move_text_cursor_right(text: &str, cursor: &mut usize) {
    clamp_text_cursor(text, cursor);
    if *cursor >= text.len() {
        return;
    }
    *cursor = text[*cursor..]
        .char_indices()
        .nth(1)
        .map(|(idx, _)| *cursor + idx)
        .unwrap_or(text.len());
}

fn insert_text_char(text: &mut String, cursor: &mut usize, c: char) {
    clamp_text_cursor(text, cursor);
    text.insert(*cursor, c);
    *cursor += c.len_utf8();
}

fn backspace_text(text: &mut String, cursor: &mut usize) {
    clamp_text_cursor(text, cursor);
    if *cursor == 0 {
        return;
    }
    let prev = text[..*cursor]
        .char_indices()
        .last()
        .map(|(idx, _)| idx)
        .unwrap_or(0);
    text.drain(prev..*cursor);
    *cursor = prev;
}

fn ctrl_w_delete_text(text: &mut String, cursor: &mut usize) {
    clamp_text_cursor(text, cursor);
    let after_cursor = text.split_off(*cursor);
    ctrl_w_delete(text);
    *cursor = text.len();
    text.push_str(&after_cursor);
}

/// Deletes the last "word" from a generic string (space-delimited).
fn ctrl_w_delete(s: &mut String) {
    // Trim trailing spaces, then remove back to the next space
    let trimmed_len = s.trim_end().len();
    s.truncate(trimmed_len);
    if let Some(pos) = s.rfind(' ') {
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
    has_git_submodules: bool,
) -> CreateField {
    match current {
        CreateField::Name => CreateField::Directory,
        CreateField::Directory => {
            if has_git_repo {
                CreateField::CreateWorktree
            } else {
                CreateField::AgentType
            }
        }
        CreateField::CreateWorktree => {
            if create_worktree {
                CreateField::CopyDirectories
            } else {
                CreateField::AgentType
            }
        }
        CreateField::CopyDirectories => CreateField::SymlinkDirectories,
        CreateField::SymlinkDirectories => {
            if has_git_submodules {
                CreateField::InitializeSubmodules
            } else {
                CreateField::AgentType
            }
        }
        CreateField::InitializeSubmodules => CreateField::AgentType,
        CreateField::AgentType => CreateField::Name,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_discovery::DiscoveredAgents;
    use crate::agents::AgentAdapter;
    use crate::config::{AgentConfig, AgentKind};
    use crate::global_config::GlobalConfig;
    use crate::models::ContextInfo;
    use async_trait::async_trait;
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

    struct NoopAdapter;

    #[async_trait]
    impl AgentAdapter for NoopAdapter {
        async fn get_status(&self) -> AgentStatus {
            AgentStatus::Idle
        }

        async fn get_context(&self) -> Option<ContextInfo> {
            None
        }

        async fn get_first_prompt(&self) -> Option<String> {
            None
        }

        async fn get_last_model_response(&self) -> Option<String> {
            None
        }

        async fn get_model_name(&self) -> Option<String> {
            None
        }

        async fn get_total_work_ms(&self) -> u64 {
            0
        }

        fn get_cached_session_id(&self) -> Option<String> {
            None
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

    fn test_app_with_exact_global_config(global_config: GlobalConfig) -> App {
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

    fn test_app_with_global_config(global_config: GlobalConfig) -> App {
        test_app_with_exact_global_config(GlobalConfig {
            startup_guide_dismissed: true,
            ..global_config
        })
    }

    fn dashboard_mouse_event(kind: MouseEventKind) -> MouseEvent {
        MouseEvent {
            kind,
            column: 0,
            row: PROJECT_TABS_HEIGHT,
            modifiers: KeyModifiers::empty(),
        }
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
    fn status_counts_are_global() {
        let agents = vec![
            test_agent("default-running", "Default", AgentStatus::Running),
            test_agent("work-waiting", "work", AgentStatus::WaitingForInput),
            test_agent("work-idle", "work", AgentStatus::Idle),
            test_agent("other-running", "other", AgentStatus::Running),
        ];

        assert_eq!(
            AgentStatusCounts::for_agents(&agents),
            AgentStatusCounts {
                running: 2,
                waiting: 1,
                idle: 1,
            }
        );
    }

    #[test]
    fn next_agent_by_status_crosses_projects() {
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
            next_agent_by_status(&agents, 0, AgentStatus::WaitingForInput, None,),
            Some(2)
        );
        assert_eq!(
            next_agent_by_status(&agents, 2, AgentStatus::WaitingForInput, None,),
            Some(1)
        );
        assert_eq!(
            next_agent_by_status(&agents, 1, AgentStatus::WaitingForInput, None,),
            Some(0)
        );
    }

    #[test]
    fn next_running_agent_cycles_globally() {
        let agents = vec![
            test_agent("other-running", "other", AgentStatus::Running),
            test_agent("work-old", "work", AgentStatus::Running),
            test_agent("work-new", "work", AgentStatus::Running),
        ];

        assert_eq!(
            next_agent_by_status(&agents, 1, AgentStatus::Running, None),
            Some(2)
        );
        assert_eq!(
            next_agent_by_status(&agents, 2, AgentStatus::Running, None),
            Some(0)
        );
        assert_eq!(
            next_agent_by_status(&agents, 99, AgentStatus::Running, None),
            Some(0)
        );
    }

    #[test]
    fn next_waiting_agent_cycles_globally() {
        let agents = vec![
            test_agent("other-waiting", "other", AgentStatus::WaitingForInput),
            test_agent("work-old", "work", AgentStatus::WaitingForInput),
            test_agent("work-new", "work", AgentStatus::WaitingForInput),
        ];

        assert_eq!(
            next_agent_by_status(&agents, 1, AgentStatus::WaitingForInput, None),
            Some(2)
        );
        assert_eq!(
            next_agent_by_status(&agents, 2, AgentStatus::WaitingForInput, None),
            Some(0)
        );
    }

    #[test]
    fn next_idle_agent_cycles_globally() {
        let agents = vec![
            test_agent("other-idle", "other", AgentStatus::Idle),
            test_agent("work-old", "work", AgentStatus::Idle),
            test_agent("work-new", "work", AgentStatus::Idle),
        ];

        assert_eq!(
            next_agent_by_status(&agents, 1, AgentStatus::Idle, None),
            Some(2)
        );
        assert_eq!(
            next_agent_by_status(&agents, 2, AgentStatus::Idle, None),
            Some(0)
        );
    }

    #[test]
    fn switch_to_next_by_status_follows_target_project() {
        let mut app = test_app_with_global_config(GlobalConfig::default());
        app.config.projects = vec!["Default".into(), "work".into(), "other".into()];
        app.agents = vec![
            test_agent("default-idle", "Default", AgentStatus::Idle),
            test_agent("work-running", "work", AgentStatus::Running),
            test_agent("other-running", "other", AgentStatus::Running),
        ];
        app.adapters = Vec::new();
        app.selected = 1;
        app.active_project_idx = 1;

        app.switch_to_next_by_status(AgentStatus::Running);

        assert_eq!(app.selected, 2);
        assert_eq!(app.active_project_idx, 2);
        assert_eq!(app.active_project_name(), "other");
        assert!(matches!(app.state, AppState::AgentView(2)));
    }

    #[test]
    fn dashboard_single_left_click_selects_without_opening() {
        let mut app = test_app_with_global_config(GlobalConfig::default());
        app.agents = vec![
            test_agent("left", "Default", AgentStatus::Idle),
            test_agent("right", "Default", AgentStatus::Idle),
        ];
        app.adapters = Vec::new();
        app.selected = 1;

        app.handle_dashboard_mouse(dashboard_mouse_event(MouseEventKind::Down(
            MouseButton::Left,
        )));

        assert_eq!(app.selected, 0);
        assert!(matches!(app.state, AppState::Dashboard));
        assert!(app.last_dashboard_left_click.is_some());
    }

    #[test]
    fn dashboard_double_left_click_opens_agent_view() {
        let mut app = test_app_with_global_config(GlobalConfig::default());
        app.agents = vec![test_agent("only", "Default", AgentStatus::Idle)];
        app.adapters = Vec::new();

        let click = dashboard_mouse_event(MouseEventKind::Down(MouseButton::Left));
        app.handle_dashboard_mouse(click);
        app.handle_dashboard_mouse(click);

        assert!(matches!(app.state, AppState::AgentView(0)));
        assert!(app.last_dashboard_left_click.is_none());
    }

    #[test]
    fn dashboard_double_click_requires_same_card() {
        let mut app = test_app_with_global_config(GlobalConfig::default());
        app.agents = vec![
            test_agent("left", "Default", AgentStatus::Idle),
            test_agent("right", "Default", AgentStatus::Idle),
        ];
        app.adapters = Vec::new();

        app.handle_dashboard_mouse(dashboard_mouse_event(MouseEventKind::Down(
            MouseButton::Left,
        )));
        app.handle_dashboard_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 40,
            row: PROJECT_TABS_HEIGHT,
            modifiers: KeyModifiers::empty(),
        });

        assert!(matches!(app.state, AppState::Dashboard));
        assert_eq!(app.selected, 1);
        assert_eq!(app.last_dashboard_left_click.map(|(idx, _)| idx), Some(1));
    }

    #[test]
    fn dashboard_double_click_times_out() {
        let mut app = test_app_with_global_config(GlobalConfig::default());
        app.agents = vec![test_agent("only", "Default", AgentStatus::Idle)];
        app.adapters = Vec::new();
        app.last_dashboard_left_click = Some((
            0,
            std::time::Instant::now()
                - DASHBOARD_DOUBLE_CLICK_WINDOW
                - std::time::Duration::from_millis(1),
        ));

        app.handle_dashboard_mouse(dashboard_mouse_event(MouseEventKind::Down(
            MouseButton::Left,
        )));

        assert!(matches!(app.state, AppState::Dashboard));
        assert_eq!(app.last_dashboard_left_click.map(|(idx, _)| idx), Some(0));
    }

    #[test]
    fn dashboard_non_left_mouse_does_not_arm_double_click() {
        let mut app = test_app_with_global_config(GlobalConfig::default());
        app.agents = vec![test_agent("only", "Default", AgentStatus::Idle)];
        app.adapters = Vec::new();

        app.handle_dashboard_mouse(dashboard_mouse_event(MouseEventKind::Down(
            MouseButton::Right,
        )));

        assert!(matches!(app.state, AppState::Dashboard));
        assert!(app.last_dashboard_left_click.is_none());
    }

    #[test]
    fn notification_observe_blinks_for_global_count_changes() {
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
    fn switching_projects_does_not_reset_notification_baseline() {
        let mut app = test_app_with_global_config(GlobalConfig::default());
        app.config.projects = vec!["Default".into(), "work".into()];
        app.agents = vec![
            test_agent("default-running", "Default", AgentStatus::Running),
            test_agent("work-idle", "work", AgentStatus::Idle),
        ];
        app.notification.reset(app.global_status_counts());
        app.notification.observe(AgentStatusCounts {
            running: 0,
            waiting: 1,
            idle: 1,
        });

        assert!(app.notification.waiting_blink.is_some());

        app.set_active_project_idx(1);

        assert!(app.notification.waiting_blink.is_some());
        assert_eq!(app.active_project_name(), "work");
    }

    #[test]
    fn switching_projects_restores_last_selected_agent_for_each_project() {
        let mut app = test_app_with_global_config(GlobalConfig::default());
        app.config.projects = vec!["Default".into(), "work".into()];
        app.agents = vec![
            test_agent("default-a", "Default", AgentStatus::Idle),
            test_agent("default-b", "Default", AgentStatus::Idle),
            test_agent("work-a", "work", AgentStatus::Idle),
            test_agent("work-b", "work", AgentStatus::Idle),
        ];
        app.set_dashboard_selected(1);

        app.set_active_project_idx(1);
        assert_eq!(app.selected, 2);

        app.set_dashboard_selected(3);
        app.set_active_project_idx(0);
        assert_eq!(app.selected, 1);

        app.set_active_project_idx(1);
        assert_eq!(app.selected, 3);
    }

    #[test]
    fn switching_projects_without_history_falls_back_to_first_visible_card() {
        let mut app = test_app_with_global_config(GlobalConfig::default());
        app.config.projects = vec!["Default".into(), "work".into()];
        app.agents = vec![
            test_agent("default-agent", "Default", AgentStatus::Idle),
            test_agent("work-a", "work", AgentStatus::Idle),
            test_agent("work-b", "work", AgentStatus::Idle),
        ];
        app.selected = 0;
        app.active_project_idx = 0;

        app.set_active_project_idx(1);

        assert_eq!(app.selected, 1);
    }

    #[test]
    fn removing_remembered_agent_falls_back_to_first_visible_card() {
        let mut app = test_app_with_global_config(GlobalConfig::default());
        app.config.projects = vec!["Default".into(), "work".into()];
        app.agents = vec![
            test_agent("default-agent", "Default", AgentStatus::Idle),
            test_agent("work-a", "work", AgentStatus::Idle),
            test_agent("work-b", "work", AgentStatus::Idle),
        ];
        app.config.agents = app
            .agents
            .iter()
            .map(|entry| entry.config.clone())
            .collect();
        app.adapters = vec![
            Box::new(NoopAdapter),
            Box::new(NoopAdapter),
            Box::new(NoopAdapter),
        ];
        app.card_scroll = vec![0; app.agents.len()];
        app.card_response_heights = vec![0; app.agents.len()];
        app.card_response_widths = vec![0; app.agents.len()];
        app.active_project_idx = 1;
        app.set_dashboard_selected(2);
        app.set_active_project_idx(0);

        futures::executor::block_on(app.remove_agent(2, false, false));

        app.set_active_project_idx(1);

        assert_eq!(app.selected, 1);
    }

    #[test]
    fn reordering_cards_preserves_remembered_agent_when_switching_back() {
        let mut app = test_app_with_global_config(GlobalConfig::default());
        app.config.projects = vec!["Default".into(), "work".into()];
        app.agents = vec![
            test_agent("default-agent", "Default", AgentStatus::Idle),
            test_agent("work-a", "work", AgentStatus::Idle),
            test_agent("work-b", "work", AgentStatus::Idle),
        ];
        app.config.agents = app
            .agents
            .iter()
            .map(|entry| entry.config.clone())
            .collect();
        app.adapters = vec![
            Box::new(NoopAdapter),
            Box::new(NoopAdapter),
            Box::new(NoopAdapter),
        ];
        app.card_scroll = vec![0; app.agents.len()];
        app.card_response_heights = vec![0; app.agents.len()];
        app.card_response_widths = vec![0; app.agents.len()];
        app.active_project_idx = 1;
        app.set_dashboard_selected(2);

        app.move_card(1);
        app.set_active_project_idx(0);
        app.set_active_project_idx(1);

        assert_eq!(app.selected, 1);
        assert_eq!(app.agents[app.selected].config.name, "work-b");
    }

    #[test]
    fn moving_cards_swaps_terminal_pane_ownership_with_agents() {
        let mut app = test_app_with_global_config(GlobalConfig::default());
        app.agents = vec![
            test_agent("one", "Default", AgentStatus::Idle),
            test_agent("two", "Default", AgentStatus::Idle),
        ];
        app.adapters = vec![Box::new(NoopAdapter), Box::new(NoopAdapter)];
        app.config.agents = app
            .agents
            .iter()
            .map(|entry| entry.config.clone())
            .collect();
        app.selected = 0;
        app.card_scroll = vec![0; app.agents.len()];
        app.card_response_heights = vec![0; app.agents.len()];
        app.card_response_widths = vec![0; app.agents.len()];
        app.terminal_panes.insert(0, "flowmux:10.0".into());
        app.terminal_panes.insert(1, "flowmux:11.0".into());

        app.move_card(1);

        assert_eq!(
            app.terminal_panes.get(&0).map(String::as_str),
            Some("flowmux:11.0")
        );
        assert_eq!(
            app.terminal_panes.get(&1).map(String::as_str),
            Some("flowmux:10.0")
        );
    }

    #[test]
    fn moving_cards_updates_live_terminal_view_owner() {
        let mut app = test_app_with_global_config(GlobalConfig::default());
        app.agents = vec![
            test_agent("one", "Default", AgentStatus::Idle),
            test_agent("two", "Default", AgentStatus::Idle),
        ];
        app.adapters = vec![Box::new(NoopAdapter), Box::new(NoopAdapter)];
        app.config.agents = app
            .agents
            .iter()
            .map(|entry| entry.config.clone())
            .collect();
        app.selected = 0;
        app.card_scroll = vec![0; app.agents.len()];
        app.card_response_heights = vec![0; app.agents.len()];
        app.card_response_widths = vec![0; app.agents.len()];
        app.terminal_view_state = Some(TerminalViewState::new(0, "flowmux:10.0".into()));
        app.state = AppState::TerminalView(TerminalViewState::new(0, "flowmux:10.0".into()));

        app.move_card(1);

        match &app.state {
            AppState::TerminalView(tv) => assert_eq!(tv.agent_idx, 1),
            state => panic!("expected terminal view, got {state:?}"),
        }
        assert_eq!(
            app.terminal_view_state.as_ref().map(|tv| tv.agent_idx),
            Some(1)
        );
    }

    #[test]
    fn left_click_on_project_tab_switches_projects_without_selecting_a_card() {
        let mut app = test_app_with_global_config(GlobalConfig::default());
        app.config.projects = vec!["Default".into(), "work".into()];
        app.agents = vec![
            test_agent("default-agent", "Default", AgentStatus::Idle),
            test_agent("work-agent-a", "work", AgentStatus::Idle),
            test_agent("work-agent-b", "work", AgentStatus::Idle),
        ];
        app.selected = 0;
        app.active_project_idx = 0;
        app.state = AppState::Dashboard;
        app.set_active_project_idx(1);
        app.set_dashboard_selected(2);
        app.set_active_project_idx(0);

        app.handle_dashboard_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 13,
            row: 0,
            modifiers: KeyModifiers::empty(),
        });

        assert_eq!(app.active_project_idx, 1);
        assert_eq!(app.active_project_name(), "work");
        assert_eq!(app.selected, 2);
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
    fn enter_agent_view_restores_saved_scroll_state() {
        let mut app = test_app_with_global_config(GlobalConfig::default());
        app.agents = vec![
            test_agent("one", "Default", AgentStatus::Idle),
            test_agent("two", "Default", AgentStatus::Idle),
        ];
        app.agent_view_scroll = vec![
            StoredAgentViewScroll {
                view_scroll: 7,
                viewport_height: Some(14),
            },
            StoredAgentViewScroll {
                view_scroll: 3,
                viewport_height: Some(9),
            },
        ];

        app.enter_agent_view(1);

        assert!(matches!(app.state, AppState::AgentView(1)));
        assert_eq!(app.selected, 1);
        assert_eq!(app.agent_view_state.view_scroll, 3);
        assert_eq!(app.agent_view_state.last_viewport_height, Some(9));
    }

    #[test]
    fn switch_to_next_by_status_persists_previous_agent_scroll() {
        let mut app = test_app_with_global_config(GlobalConfig::default());
        app.config.projects = vec!["Default".into()];
        app.agents = vec![
            test_agent("one", "Default", AgentStatus::Running),
            test_agent("two", "Default", AgentStatus::Running),
        ];
        app.agent_view_scroll = vec![
            StoredAgentViewScroll {
                view_scroll: 4,
                viewport_height: Some(10),
            },
            StoredAgentViewScroll {
                view_scroll: 9,
                viewport_height: Some(12),
            },
        ];
        app.selected = 0;
        app.state = AppState::AgentView(0);
        app.agent_view_state.view_scroll = 6;
        app.agent_view_state.last_viewport_height = Some(11);

        app.switch_to_next_by_status(AgentStatus::Running);

        assert!(matches!(app.state, AppState::AgentView(1)));
        assert_eq!(app.agent_view_scroll[0].view_scroll, 6);
        assert_eq!(app.agent_view_scroll[0].viewport_height, Some(11));
        assert_eq!(app.agent_view_state.view_scroll, 9);
        assert_eq!(app.agent_view_state.last_viewport_height, Some(12));
    }

    #[test]
    fn exiting_git_viewer_restores_agent_scroll() {
        let mut app = test_app_with_global_config(GlobalConfig::default());
        app.agents = vec![test_agent("one", "Default", AgentStatus::Idle)];
        app.agent_view_scroll = vec![StoredAgentViewScroll {
            view_scroll: 8,
            viewport_height: Some(13),
        }];
        app.state = AppState::GitViewer(GitViewerState::new(0, "dummy".into()));

        app.exit_git_viewer_to_agent();

        assert!(matches!(app.state, AppState::AgentView(0)));
        assert_eq!(app.agent_view_state.view_scroll, 8);
        assert_eq!(app.agent_view_state.last_viewport_height, Some(13));
    }

    #[test]
    fn exiting_terminal_view_restores_agent_scroll() {
        let mut app = test_app_with_global_config(GlobalConfig::default());
        app.agents = vec![test_agent("one", "Default", AgentStatus::Idle)];
        app.agent_view_scroll = vec![StoredAgentViewScroll {
            view_scroll: 5,
            viewport_height: Some(7),
        }];
        app.state = AppState::TerminalView(TerminalViewState::new(0, "dummy".into()));

        app.exit_terminal_to_agent();

        assert!(matches!(app.state, AppState::AgentView(0)));
        assert_eq!(app.agent_view_state.view_scroll, 5);
        assert_eq!(app.agent_view_state.last_viewport_height, Some(7));
    }

    #[test]
    fn remove_agent_keeps_scroll_entries_aligned() {
        let mut app = test_app_with_global_config(GlobalConfig::default());
        app.agents = vec![
            test_agent("one", "Default", AgentStatus::Idle),
            test_agent("two", "Default", AgentStatus::Idle),
            test_agent("three", "Default", AgentStatus::Idle),
        ];
        app.adapters = vec![
            Box::new(NoopAdapter),
            Box::new(NoopAdapter),
            Box::new(NoopAdapter),
        ];
        app.config.agents = app
            .agents
            .iter()
            .map(|entry| entry.config.clone())
            .collect();
        app.agent_view_scroll = vec![
            StoredAgentViewScroll {
                view_scroll: 1,
                viewport_height: Some(5),
            },
            StoredAgentViewScroll {
                view_scroll: 2,
                viewport_height: Some(6),
            },
            StoredAgentViewScroll {
                view_scroll: 3,
                viewport_height: Some(7),
            },
        ];

        tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(app.remove_agent(1, false, false));

        assert_eq!(app.agents.len(), 2);
        assert_eq!(app.agent_view_scroll.len(), 2);
        assert_eq!(app.agent_view_scroll[0].view_scroll, 1);
        assert_eq!(app.agent_view_scroll[1].view_scroll, 3);
        assert_eq!(app.agent_view_scroll[1].viewport_height, Some(7));
    }

    #[test]
    fn remove_agent_keeps_card_scroll_entries_aligned() {
        let mut app = test_app_with_global_config(GlobalConfig::default());
        app.agents = vec![
            test_agent("one", "Default", AgentStatus::Idle),
            test_agent("two", "Default", AgentStatus::Idle),
            test_agent("three", "Default", AgentStatus::Idle),
        ];
        app.adapters = vec![
            Box::new(NoopAdapter),
            Box::new(NoopAdapter),
            Box::new(NoopAdapter),
        ];
        app.config.agents = app
            .agents
            .iter()
            .map(|entry| entry.config.clone())
            .collect();
        app.card_scroll = vec![4, 9, 2];
        app.card_response_heights = vec![11, 12, 13];
        app.card_response_widths = vec![81, 82, 83];

        tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(app.remove_agent(1, false, false));

        assert_eq!(app.card_scroll, vec![4, 2]);
        assert_eq!(app.card_response_heights, vec![11, 13]);
        assert_eq!(app.card_response_widths, vec![81, 83]);
    }

    #[test]
    fn remove_agent_reindexes_terminal_pane_ownership() {
        let mut app = test_app_with_global_config(GlobalConfig::default());
        app.agents = vec![
            test_agent("one", "Default", AgentStatus::Idle),
            test_agent("two", "Default", AgentStatus::Idle),
            test_agent("three", "Default", AgentStatus::Idle),
        ];
        app.adapters = vec![
            Box::new(NoopAdapter),
            Box::new(NoopAdapter),
            Box::new(NoopAdapter),
        ];
        app.config.agents = app
            .agents
            .iter()
            .map(|entry| entry.config.clone())
            .collect();
        app.terminal_panes.insert(0, "flowmux:10.0".into());
        app.terminal_panes.insert(2, "flowmux:12.0".into());

        tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(app.remove_agent(1, false, false));

        assert_eq!(app.terminal_panes.len(), 2);
        assert_eq!(
            app.terminal_panes.get(&0).map(String::as_str),
            Some("flowmux:10.0")
        );
        assert_eq!(
            app.terminal_panes.get(&1).map(String::as_str),
            Some("flowmux:12.0")
        );
    }

    #[test]
    fn remove_agent_drops_removed_terminal_pane_cache() {
        let mut app = test_app_with_global_config(GlobalConfig::default());
        app.agents = vec![
            test_agent("one", "Default", AgentStatus::Idle),
            test_agent("two", "Default", AgentStatus::Idle),
        ];
        app.adapters = vec![Box::new(NoopAdapter), Box::new(NoopAdapter)];
        app.config.agents = app
            .agents
            .iter()
            .map(|entry| entry.config.clone())
            .collect();
        app.terminal_panes.insert(1, "flowmux:11.0".into());

        tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(app.remove_agent(1, false, false));

        assert!(app.terminal_panes.is_empty());
    }

    #[test]
    fn remove_agent_reindexes_live_terminal_view_owner() {
        let mut app = test_app_with_global_config(GlobalConfig::default());
        app.agents = vec![
            test_agent("one", "Default", AgentStatus::Idle),
            test_agent("two", "Default", AgentStatus::Idle),
            test_agent("three", "Default", AgentStatus::Idle),
        ];
        app.adapters = vec![
            Box::new(NoopAdapter),
            Box::new(NoopAdapter),
            Box::new(NoopAdapter),
        ];
        app.config.agents = app
            .agents
            .iter()
            .map(|entry| entry.config.clone())
            .collect();
        app.terminal_view_state = Some(TerminalViewState::new(2, "flowmux:12.0".into()));
        app.state = AppState::TerminalView(TerminalViewState::new(2, "flowmux:12.0".into()));

        tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(app.remove_agent(1, false, false));

        match &app.state {
            AppState::TerminalView(tv) => assert_eq!(tv.agent_idx, 1),
            state => panic!("expected terminal view, got {state:?}"),
        }
        assert_eq!(
            app.terminal_view_state.as_ref().map(|tv| tv.agent_idx),
            Some(1)
        );
    }

    #[test]
    fn remove_agent_clears_live_terminal_view_for_removed_owner() {
        let mut app = test_app_with_global_config(GlobalConfig::default());
        app.agents = vec![
            test_agent("one", "Default", AgentStatus::Idle),
            test_agent("two", "Default", AgentStatus::Idle),
        ];
        app.adapters = vec![Box::new(NoopAdapter), Box::new(NoopAdapter)];
        app.config.agents = app
            .agents
            .iter()
            .map(|entry| entry.config.clone())
            .collect();
        app.terminal_view_state = Some(TerminalViewState::new(1, "flowmux:11.0".into()));
        app.state = AppState::TerminalView(TerminalViewState::new(1, "flowmux:11.0".into()));
        app.terminal_panes.insert(1, "flowmux:11.0".into());

        tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(app.remove_agent(1, false, false));

        assert!(matches!(app.state, AppState::Dashboard));
        assert!(app.terminal_view_state.is_none());
        assert!(app.terminal_panes.is_empty());
    }

    #[test]
    fn resized_view_scroll_preserves_top_visible_line_when_growing() {
        let old_range = pane_visible_line_range(20, 4, 5);
        let new_scroll = resized_view_scroll(20, 4, 5, 8);
        let new_range = pane_visible_line_range(20, new_scroll, 8);

        assert_eq!(old_range.0, new_range.0);
    }

    #[test]
    fn resized_view_scroll_preserves_top_visible_line_when_shrinking() {
        let old_range = pane_visible_line_range(20, 4, 8);
        let new_scroll = resized_view_scroll(20, 4, 8, 5);
        let new_range = pane_visible_line_range(20, new_scroll, 5);

        assert_eq!(old_range.0, new_range.0);
    }

    #[test]
    fn resized_view_scroll_clamps_when_anchor_exceeds_available_history() {
        let new_scroll = resized_view_scroll(6, 5, 3, 8);
        assert_eq!(new_scroll, 0);
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

        assert!(
            state
                .commit_selector_candidate(&CreateField::CopyDirectories)
                .unwrap()
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
            next_create_field(&CreateField::CreateWorktree, true, false, false),
            CreateField::AgentType
        );
        assert_eq!(
            next_create_field(&CreateField::CreateWorktree, true, true, true),
            CreateField::CopyDirectories
        );
        assert_eq!(
            next_create_field(&CreateField::SymlinkDirectories, true, true, true),
            CreateField::InitializeSubmodules
        );
        assert_eq!(
            next_create_field(&CreateField::InitializeSubmodules, true, true, true),
            CreateField::AgentType
        );
        assert_eq!(
            next_create_field(&CreateField::SymlinkDirectories, true, true, false),
            CreateField::AgentType
        );
        assert_eq!(
            next_create_field(&CreateField::Directory, false, false, false),
            CreateField::AgentType
        );
    }

    #[test]
    fn name_input_cursor_edits_in_place() {
        let mut state = CreateAgentState {
            name: "alpha beta".into(),
            name_cursor: "alpha".len(),
            ..CreateAgentState::default()
        };

        state.insert_name_char('-');
        assert_eq!(state.name, "alpha- beta");
        assert_eq!(state.name_cursor, "alpha-".len());

        state.move_name_cursor_left();
        state.backspace_name();
        assert_eq!(state.name, "alph- beta");
        assert_eq!(state.name_cursor, "alph".len());

        state.move_name_cursor_right();
        state.move_name_cursor_right();
        state.ctrl_w_delete_name();
        assert_eq!(state.name, "beta");
        assert_eq!(state.name_cursor, 0);
    }

    #[test]
    fn name_input_home_and_end_move_to_boundaries() {
        let mut state = CreateAgentState {
            name: "alpha".into(),
            name_cursor: 2,
            ..CreateAgentState::default()
        };

        state.move_name_cursor_end();
        state.insert_name_char('!');
        assert_eq!(state.name, "alpha!");

        state.move_name_cursor_home();
        state.insert_name_char('#');
        assert_eq!(state.name, "#alpha!");
    }

    #[test]
    fn create_project_name_input_uses_cursor_navigation() {
        let mut state = CreateProjectState {
            name: TextInputState {
                value: "alpha beta".into(),
                cursor: "alpha".len(),
            },
            error: Some("stale".into()),
        };

        state.name.move_end();
        state.name.insert_char('!');
        state.name.move_home();
        state.name.insert_char('#');
        state.name.backspace();
        state.name.ctrl_w_delete();

        assert_eq!(state.name.value, "alpha beta!");
        assert_eq!(state.name.cursor, 0);
    }

    #[tokio::test]
    async fn create_agent_name_home_and_end_keys_edit_at_boundaries() {
        let mut app = test_app_with_global_config(GlobalConfig::default());
        app.create_state = CreateAgentState {
            name: "alpha".into(),
            name_cursor: 2,
            focus: CreateField::Name,
            available_types: vec![AgentType::Codex],
            ..CreateAgentState::default()
        };

        app.handle_create_key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE))
            .await;
        app.handle_create_key(KeyEvent::new(KeyCode::Char('!'), KeyModifiers::NONE))
            .await;
        app.handle_create_key(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE))
            .await;
        app.handle_create_key(KeyEvent::new(KeyCode::Char('#'), KeyModifiers::NONE))
            .await;

        assert_eq!(app.create_state.name, "#alpha!");
    }

    #[test]
    fn create_project_name_home_and_end_keys_edit_at_boundaries() {
        let mut app = test_app_with_global_config(GlobalConfig::default());
        app.create_project_state = CreateProjectState {
            name: TextInputState {
                value: "alpha".into(),
                cursor: 2,
            },
            error: None,
        };

        app.handle_create_project_key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE));
        app.handle_create_project_key(KeyEvent::new(KeyCode::Char('!'), KeyModifiers::NONE));
        app.handle_create_project_key(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE));
        app.handle_create_project_key(KeyEvent::new(KeyCode::Char('#'), KeyModifiers::NONE));

        assert_eq!(app.create_project_state.name.value, "#alpha!");
    }

    #[tokio::test]
    async fn tab_leaving_empty_directory_selector_disables_checkbox() {
        let mut app = test_app_with_global_config(GlobalConfig::default());
        app.create_state = CreateAgentState {
            focus: CreateField::CopyDirectories,
            directory: std::env::temp_dir().to_string_lossy().to_string(),
            git_repo_root: Some(std::env::temp_dir()),
            create_worktree: true,
            copy_directories_enabled: true,
            available_types: vec![AgentType::Codex],
            ..CreateAgentState::default()
        };

        app.handle_create_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE))
            .await;

        assert!(!app.create_state.copy_directories_enabled);
        assert_eq!(app.create_state.focus, CreateField::SymlinkDirectories);
    }

    #[tokio::test]
    async fn tab_leaving_selected_directory_selector_keeps_checkbox_enabled() {
        let mut app = test_app_with_global_config(GlobalConfig::default());
        app.create_state = CreateAgentState {
            focus: CreateField::SymlinkDirectories,
            directory: std::env::temp_dir().to_string_lossy().to_string(),
            git_repo_root: Some(std::env::temp_dir()),
            create_worktree: true,
            symlink_directories_enabled: true,
            symlink_directories: RelativeDirSelector {
                selected_dirs: vec!["vendor".into()],
                ..RelativeDirSelector::default()
            },
            available_types: vec![AgentType::Codex],
            ..CreateAgentState::default()
        };

        app.handle_create_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE))
            .await;

        assert!(app.create_state.symlink_directories_enabled);
        assert_eq!(app.create_state.focus, CreateField::AgentType);
    }

    #[test]
    fn create_agent_state_defaults_to_initializing_submodules() {
        let state = CreateAgentState::default();
        assert!(state.initialize_submodules);
    }

    #[test]
    fn initialize_submodules_visibility_requires_worktree_and_submodules() {
        let mut state = CreateAgentState::default();
        assert!(!state.initialize_submodules_visible());

        state.git_repo_root = Some(PathBuf::from("/tmp/repo"));
        state.create_worktree = true;
        assert!(!state.initialize_submodules_visible());

        state.has_git_submodules = true;
        assert!(state.initialize_submodules_visible());
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

    #[test]
    fn app_starts_with_startup_guide_when_not_dismissed() {
        let app = test_app_with_exact_global_config(GlobalConfig::default());

        assert!(matches!(app.state, AppState::StartupGuide(_)));
    }

    #[test]
    fn app_starts_on_dashboard_when_startup_guide_was_dismissed() {
        let app = test_app_with_exact_global_config(GlobalConfig {
            startup_guide_dismissed: true,
            ..GlobalConfig::default()
        });

        assert!(matches!(app.state, AppState::Dashboard));
    }

    #[test]
    fn dashboard_question_mark_reopens_startup_guide() {
        let mut app = test_app_with_global_config(GlobalConfig {
            startup_guide_dismissed: true,
            ..GlobalConfig::default()
        });

        assert!(matches!(app.state, AppState::Dashboard));
        assert!(app.handle_dashboard_key(KeyEvent::from(KeyCode::Char('?'))));
        assert!(matches!(
            app.state,
            AppState::StartupGuide(StartupGuideState {
                persist_on_close: false,
                ..
            })
        ));
    }

    #[test]
    fn startup_guide_navigation_saturates_at_page_bounds() {
        let mut app = test_app_with_exact_global_config(GlobalConfig::default());
        let last_page = app.startup_guide_last_page();

        app.state = AppState::StartupGuide(StartupGuideState::reopened());
        for _ in 0..200 {
            assert!(app.handle_startup_guide_key(
                KeyEvent::from(KeyCode::Right),
                StartupGuideState::reopened(),
            ));
        }

        match &app.state {
            AppState::StartupGuide(guide) => assert_eq!(guide.page, last_page),
            state => panic!("expected startup guide state, got {state:?}"),
        }

        for _ in 0..200 {
            assert!(app.handle_startup_guide_key(
                KeyEvent::from(KeyCode::Left),
                StartupGuideState::reopened(),
            ));
        }

        match &app.state {
            AppState::StartupGuide(guide) => assert_eq!(guide.page, 0),
            state => panic!("expected startup guide state, got {state:?}"),
        }
    }

    #[test]
    fn app_defaults_to_gruvbox_dark_when_global_theme_missing_or_invalid() {
        let app = test_app_with_global_config(GlobalConfig::default());
        assert_eq!(app.active_theme_id, "gruvbox-dark");

        let invalid = test_app_with_global_config(GlobalConfig {
            theme: Some("missing-theme".into()),
            ..GlobalConfig::default()
        });
        assert_eq!(invalid.active_theme_id, "gruvbox-dark");
    }

    #[test]
    fn opening_settings_seeds_selection_and_preview_theme() {
        let mut app = test_app_with_global_config(GlobalConfig {
            theme: Some("tokyo-night".into()),
            ..GlobalConfig::default()
        });

        app.open_settings_dialog();

        let AppState::SettingsDialog(state) = &app.state else {
            panic!("expected settings dialog");
        };
        assert_eq!(state.committed_theme_id, "tokyo-night");
        assert_eq!(app.preview_theme_id.as_deref(), Some("tokyo-night"));
        assert_eq!(state.selected_idx, theme_index_by_id("tokyo-night"));
    }

    #[test]
    fn settings_cancel_restores_committed_theme_without_persisting() {
        let mut app = test_app_with_global_config(GlobalConfig {
            theme: Some("gruvbox-dark".into()),
            ..GlobalConfig::default()
        });
        app.open_settings_dialog();

        if let AppState::SettingsDialog(ref mut state) = app.state {
            state.selected_idx = theme_index_by_id("solarized-dark");
        }
        app.set_theme_preview_by_id("solarized-dark");

        let state = match &app.state {
            AppState::SettingsDialog(state) => state.clone(),
            _ => panic!("expected settings dialog"),
        };
        app.cancel_settings_dialog(state);

        assert!(matches!(app.state, AppState::Dashboard));
        assert_eq!(app.active_theme_id, "gruvbox-dark");
        assert_eq!(app.preview_theme_id, None);
        assert_eq!(
            app.runner.global_config().theme.as_deref(),
            Some("gruvbox-dark")
        );
    }

    #[test]
    fn applying_selected_theme_updates_active_and_global_config() {
        let mut app = test_app_with_global_config(GlobalConfig {
            theme: Some("gruvbox-dark".into()),
            ..GlobalConfig::default()
        });
        app.preview_theme_id = Some("catppuccin-latte".into());

        app.apply_selected_theme(theme_index_by_id("catppuccin-latte"));

        assert_eq!(app.active_theme_id, "catppuccin-latte");
        assert_eq!(app.preview_theme_id, None);
        assert_eq!(
            app.runner.global_config().theme.as_deref(),
            Some("catppuccin-latte")
        );
    }
}

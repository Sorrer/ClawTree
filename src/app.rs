use std::cell::Cell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Instant;
use tokio::sync::mpsc;

use ratatui::layout::Rect;

use crate::event::AppEvent;
use crate::session::{ClaudeUsage, GlobalUsage, Session};
use crate::url;
use crate::worktree::{self, Worktree, WorktreeStatus};

/// Stores the screen regions of interactive UI elements, updated each frame.
/// Uses `Cell<Rect>` for interior mutability since `draw()` takes `&App`.
pub struct LayoutAreas {
    // Panel-level regions (for focus switching)
    pub sidebar: Cell<Rect>,
    pub terminal_pane: Cell<Rect>,
    pub prompt_queue: Cell<Rect>,
    pub status_bar: Cell<Rect>,
    // Inner areas (inside borders, for item hit-testing)
    pub sidebar_inner: Cell<Rect>,
    pub terminal_pane_inner: Cell<Rect>,
    pub prompt_queue_inner: Cell<Rect>,
    // Sidebar sub-panels
    pub sidebar_terminal_panel: Cell<Rect>,
    pub sidebar_terminal_panel_inner: Cell<Rect>,
    // Overlays
    pub help_overlay: Cell<Rect>,
    pub dialog: Cell<Rect>,
    // Mini mode
    pub mini_tree: Cell<Rect>,
    pub mini_tree_inner: Cell<Rect>,
    pub mini_detail: Cell<Rect>,
}

impl Default for LayoutAreas {
    fn default() -> Self {
        Self {
            sidebar: Cell::new(Rect::default()),
            terminal_pane: Cell::new(Rect::default()),
            prompt_queue: Cell::new(Rect::default()),
            status_bar: Cell::new(Rect::default()),
            sidebar_inner: Cell::new(Rect::default()),
            terminal_pane_inner: Cell::new(Rect::default()),
            prompt_queue_inner: Cell::new(Rect::default()),
            sidebar_terminal_panel: Cell::new(Rect::default()),
            sidebar_terminal_panel_inner: Cell::new(Rect::default()),
            help_overlay: Cell::new(Rect::default()),
            dialog: Cell::new(Rect::default()),
            mini_tree: Cell::new(Rect::default()),
            mini_tree_inner: Cell::new(Rect::default()),
            mini_detail: Cell::new(Rect::default()),
        }
    }
}

impl LayoutAreas {
    /// Reset all areas to default (zero-sized). Call at the start of each frame.
    pub fn reset(&self) {
        self.sidebar.set(Rect::default());
        self.terminal_pane.set(Rect::default());
        self.prompt_queue.set(Rect::default());
        self.status_bar.set(Rect::default());
        self.sidebar_inner.set(Rect::default());
        self.terminal_pane_inner.set(Rect::default());
        self.prompt_queue_inner.set(Rect::default());
        self.sidebar_terminal_panel.set(Rect::default());
        self.sidebar_terminal_panel_inner.set(Rect::default());
        self.help_overlay.set(Rect::default());
        self.dialog.set(Rect::default());
        self.mini_tree.set(Rect::default());
        self.mini_tree_inner.set(Rect::default());
        self.mini_detail.set(Rect::default());
    }
}

/// In-app text selection state (anchor + current drag position in vt100 coords).
#[derive(Debug, Clone, Copy)]
pub struct TextSelection {
    pub anchor: (u16, u16),   // vt100 (row, col) where drag started
    pub endpoint: (u16, u16), // vt100 (row, col) current drag position
}

impl TextSelection {
    /// Return (start, end) in reading order (top-left to bottom-right).
    pub fn ordered(&self) -> ((u16, u16), (u16, u16)) {
        let (ar, ac) = self.anchor;
        let (er, ec) = self.endpoint;
        if ar < er || (ar == er && ac <= ec) {
            ((ar, ac), (er, ec))
        } else {
            ((er, ec), (ar, ac))
        }
    }
}

/// Which screen is currently displayed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScreenMode {
    Normal,
    Mini,
    MiniDrilldown,
}

/// Computed agent status for mini mode display.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentStatus {
    Working,
    Idle,
    NeedsInput,
    Exited,
}

/// Which element has focus within mini mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MiniModeFocus {
    AgentList,
    DetailInput,
    PromptInput,
    SavedPrompts,
    WorktreeSelector,
}

/// State for the mini mode view.
pub struct MiniModeState {
    pub selected: usize,
    /// Tree items (worktrees + sessions) mirroring sidebar structure.
    pub items: Vec<SidebarItem>,
    pub focus: MiniModeFocus,
    pub prompt_input: String,
    pub saved_prompt_selected: usize,
    pub target_worktree_idx: usize,
    /// Input buffer for sending text to the selected agent from the detail pane.
    pub detail_input: String,
}

impl Default for MiniModeState {
    fn default() -> Self {
        Self {
            selected: 0,
            items: Vec::new(),
            focus: MiniModeFocus::AgentList,
            prompt_input: String::new(),
            saved_prompt_selected: 0,
            target_worktree_idx: 0,
            detail_input: String::new(),
        }
    }
}

/// A saved prompt template for quick agent creation.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SavedPrompt {
    pub name: String,
    pub prompt: String,
}

/// Status message severity for color coding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusSeverity {
    Info,
    Success,
    Error,
}

/// Which pane has keyboard focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusTarget {
    Sidebar,
    TerminalPane,
    PromptQueue,
}

/// Current input mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputMode {
    /// Sidebar/navigation mode.
    Normal,
    /// All keys go to the PTY.
    Terminal,
    /// A modal dialog is open.
    Dialog,
}

/// A single action entry in the context menu.
#[derive(Debug, Clone)]
pub struct ContextAction {
    pub label: String,
    pub hotkey: String,
    pub kind: ContextActionKind,
}

/// Action kinds for the context menu, mirroring existing sidebar hotkeys.
#[derive(Debug, Clone)]
pub enum ContextActionKind {
    NewClaude,
    NewClaudeYolo,
    NewTerminal,
    StageCommit,
    StageAllClaudeCommit,
    Push,
    Pull,
    MergeBranch,
    NewWorktree,
    DeleteWorktree,
    ForceDeleteWorktree,
    RenameSession,
    DeleteSession,
    CopyLastUrl,
    OpenLastUrl,
    OpenWindowsTerminal,
    OpenWindowsTerminalClaude,
}

/// Build a list of context actions appropriate for the given sidebar item.
pub fn context_actions_for_item(item: &SidebarItem, wt_available: bool) -> Vec<ContextAction> {
    match item {
        SidebarItem::Worktree(_) => {
            let mut actions = vec![
                ContextAction { label: "New Claude".into(), hotkey: "c".into(), kind: ContextActionKind::NewClaude },
                ContextAction { label: "New Claude (YOLO)".into(), hotkey: "C".into(), kind: ContextActionKind::NewClaudeYolo },
                ContextAction { label: "New Terminal".into(), hotkey: "t".into(), kind: ContextActionKind::NewTerminal },
                ContextAction { label: "Stage & Commit".into(), hotkey: "s".into(), kind: ContextActionKind::StageCommit },
                ContextAction { label: "Stage All + AI Commit".into(), hotkey: "^s".into(), kind: ContextActionKind::StageAllClaudeCommit },
                ContextAction { label: "Push".into(), hotkey: "p".into(), kind: ContextActionKind::Push },
                ContextAction { label: "Pull".into(), hotkey: "P".into(), kind: ContextActionKind::Pull },
                ContextAction { label: "Merge Branch".into(), hotkey: "m".into(), kind: ContextActionKind::MergeBranch },
                ContextAction { label: "New Worktree".into(), hotkey: "n".into(), kind: ContextActionKind::NewWorktree },
                ContextAction { label: "Copy Last URL".into(), hotkey: "u".into(), kind: ContextActionKind::CopyLastUrl },
                ContextAction { label: "Open Last URL".into(), hotkey: "U".into(), kind: ContextActionKind::OpenLastUrl },
                ContextAction { label: "Delete Worktree".into(), hotkey: "d".into(), kind: ContextActionKind::DeleteWorktree },
                ContextAction { label: "Force Delete Worktree".into(), hotkey: "D".into(), kind: ContextActionKind::ForceDeleteWorktree },
            ];
            if wt_available {
                actions.insert(actions.len() - 2, ContextAction { label: "Windows Terminal".into(), hotkey: "w".into(), kind: ContextActionKind::OpenWindowsTerminal });
                actions.insert(actions.len() - 2, ContextAction { label: "Win Terminal + Claude".into(), hotkey: "W".into(), kind: ContextActionKind::OpenWindowsTerminalClaude });
            }
            actions
        }
        SidebarItem::Session(_, _) | SidebarItem::Terminal(_) => {
            vec![
                ContextAction { label: "Rename Session".into(), hotkey: "r".into(), kind: ContextActionKind::RenameSession },
                ContextAction { label: "Copy Last URL".into(), hotkey: "u".into(), kind: ContextActionKind::CopyLastUrl },
                ContextAction { label: "Open Last URL".into(), hotkey: "U".into(), kind: ContextActionKind::OpenLastUrl },
                ContextAction { label: "Delete Session".into(), hotkey: "d".into(), kind: ContextActionKind::DeleteSession },
            ]
        }
    }
}

/// Dialog types.
#[derive(Debug, Clone)]
pub enum Dialog {
    CreateWorktree {
        branch_input: String,
        base_branch: String,
        /// 0 = branch field, 1 = base branch field
        focused_field: usize,
    },
    MergeBranch {
        /// Index of the source worktree (merge FROM this one)
        source_worktree_idx: usize,
        /// Available target branches (merge INTO one of these)
        branches: Vec<String>,
        /// Currently selected branch index
        selected: usize,
    },
    Confirm {
        message: String,
        on_confirm: ConfirmAction,
    },
    /// Dangerous confirmation requiring the user to type "yes" before proceeding.
    ConfirmDangerous {
        message: String,
        input: String,
        on_confirm: ConfirmAction,
    },
    /// Initialize a new bare-repo workflow in the current directory.
    InitRepo {
        /// Remote URL to clone (empty = init from scratch)
        url_input: String,
        /// Initial branch name (default "main")
        branch_input: String,
        /// 0 = url, 1 = branch
        focused_field: usize,
    },
    /// Rename/nickname a session.
    RenameSession {
        session_id: u64,
        input: String,
    },
    /// Merge resulted in conflicts — choose how to resolve.
    MergeConflict {
        worktree_idx: usize,
        source_branch: String,
        selected: usize,
    },
    /// Dirty worktree detected during merge — choose how to handle.
    DirtyWorktree {
        worktree_idx: usize,
        files: Vec<(String, String)>, // (status_display, path) for read-only display
        selected: usize,              // 0=Commit, 1=Claude, 2=Ignore, 3=Cancel
    },
    /// Merge completed successfully — show result.
    MergeSuccess {
        message: String,
    },
    /// Convert an existing regular repo to bare worktree layout.
    ConvertRepo {
        /// 0 = in-place, 1 = different location
        mode: usize,
        /// Target path (only used when mode == 1)
        target_path_input: String,
        /// Branch name (auto-detected from current branch)
        branch_name: String,
        /// 0=mode, 1=target_path (skipped if mode==0), 2=branch
        focused_field: usize,
        /// Detected regular repo path
        source_repo_path: PathBuf,
        /// Whether the user has confirmed the warning
        confirmed: bool,
    },
    /// Git pull failed with a non-conflict error — show error and offer resolution.
    PullError {
        worktree_idx: usize,
        error_message: String,
        selected: usize,   // 0=Claude, 1=Claude (skip perms), 2=Dismiss
    },
    /// Git authentication failed — show friendly instructions.
    AuthError {
        operation: String,  // "push", "pull", "clone"
        message: String,    // the raw git error
        selected: usize,    // 0 = Dismiss
    },
    /// Right-click / "+" context menu for sidebar items.
    ContextMenu {
        item: SidebarItem,
        selected: usize,
        actions: Vec<ContextAction>,
    },
    /// Interactive staging and commit UI.
    GitCommit {
        worktree_idx: usize,
        unstaged: Vec<(char, String)>,  // (status_char, path)
        staged: Vec<(char, String)>,    // (status_char, path)
        section: usize,                 // 0=unstaged, 1=staged
        selected: usize,                // index within current section
        phase: CommitPhase,
        commit_message: String,
        cursor_pos: usize,              // character offset into commit_message
    },
}

/// Number of conflict resolution options in MergeConflict dialog.
pub const CONFLICT_RESOLVER_COUNT: usize = 5;

/// Number of options in DirtyWorktree dialog.
pub const DIRTY_WORKTREE_OPTION_COUNT: usize = 4;

/// Number of options in PullError dialog.
pub const PULL_ERROR_OPTION_COUNT: usize = 3;

/// Number of options in AuthError dialog.
pub const AUTH_ERROR_OPTION_COUNT: usize = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommitPhase {
    Staging,           // navigating files, staging/unstaging
    GeneratingMessage, // waiting for Claude to generate commit message
    Message,           // typing commit message
}

#[derive(Debug, Clone)]
pub enum ConfirmAction {
    DeleteSession(u64),
    DeleteWorktree(PathBuf),
    ForceDeleteWorktree(PathBuf),
}

/// A blocking git action queued for execution by the main loop.
/// The main loop draws a loading overlay first, then runs the action.
#[derive(Debug)]
pub enum PendingAction {
    StageFile { worktree_idx: usize, file: String },
    UnstageFile { worktree_idx: usize, file: String },
    StageAll { worktree_idx: usize, then_claude: bool },
    Commit { worktree_idx: usize, message: String },
    CommitAndPush { worktree_idx: usize, message: String },
    RefreshWorktreeStatus,
    OpenStageCommit { worktree_idx: usize },
    StageAllAndCommitClaude { worktree_idx: usize },
    MergeExecute {
        source_worktree_idx: usize,
        target_branch: String,
    },
}

/// Sidebar item types for the tree view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidebarItem {
    Worktree(usize),
    Session(usize, usize), // (worktree_index, session_index within worktree)
    Terminal(usize),        // index into app.terminal_ids
}

/// Which sidebar panel has focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidebarPanel {
    Worktrees,
    Terminals,
}

/// Main application state.
pub struct App {
    pub bare_repo_path: PathBuf,
    pub worktrees: Vec<Worktree>,
    pub sessions: HashMap<u64, Session>,
    pub next_session_id: u64,
    pub focus: FocusTarget,
    pub input_mode: InputMode,
    pub active_session_id: Option<u64>,
    pub active_worktree_idx: Option<usize>,
    pub worktree_status: Option<WorktreeStatus>,
    /// Cursor position for interactive file list in the info panel.
    pub info_panel_section: usize,  // 0=unstaged, 1=staged
    pub info_panel_cursor: usize,   // index within current section
    pub dialog: Option<Dialog>,
    /// Loading overlay message shown during blocking git operations.
    pub loading_message: Option<String>,
    /// Queued blocking action to execute after drawing the loading overlay.
    pub pending_action: Option<PendingAction>,
    pub should_quit: bool,
    pub sidebar_selected: usize,
    pub sidebar_hovered: Option<usize>,
    pub terminal_panel_hovered: Option<usize>,
    pub sidebar_items: Vec<SidebarItem>,
    pub event_tx: mpsc::UnboundedSender<AppEvent>,
    pub status_message: Option<String>,
    pub status_message_time: Option<Instant>,
    pub status_severity: StatusSeverity,
    pub sidebar_visible: bool,
    /// Whether a valid bare repo was detected at startup.
    pub repo_detected: bool,
    /// Path to a regular (non-bare) git repo detected at startup, if any.
    pub regular_repo_path: Option<PathBuf>,
    /// Whether tmux is available for session persistence.
    pub tmux_available: bool,
    /// Whether wt.exe (Windows Terminal) is available.
    pub wt_available: bool,
    /// Scrollback offset for the active terminal (0 = live view, >0 = scrolled up).
    pub terminal_scroll: usize,
    /// Merge to retry after a commit completes (set when target worktree was dirty).
    pub pending_merge: Option<PendingAction>,
    /// Whether the prompt queue panel is visible.
    pub prompt_queue_visible: bool,
    /// Per-session prompt queues.
    pub prompt_queues: HashMap<u64, Vec<String>>,
    /// Selected index in the prompt queue list.
    pub prompt_queue_selected: usize,
    /// Current input text in the prompt queue input field.
    pub prompt_queue_input: String,
    /// Cursor byte offset within prompt_queue_input.
    pub prompt_queue_cursor: usize,
    /// Index of the queue item being edited (None = not editing).
    pub prompt_queue_editing: Option<usize>,
    /// Timestamp of last auto-sent prompt (for cooldown).
    pub prompt_queue_last_send: Option<Instant>,
    /// Timestamp of last auto-send check (throttle tmux queries to ~1/sec).
    pub prompt_queue_last_check: Option<Instant>,
    /// Whether the hotkey help overlay is visible.
    pub show_help: bool,
    /// Currently selected tab in the help overlay (0=Global, 1=Sidebar, etc.).
    pub help_tab: usize,
    /// Scroll offset within the help key list.
    pub help_scroll: usize,
    /// Spinner animation frame counter (incremented on tick).
    pub spinner_frame: usize,
    /// Per-worktree cached git status from the background poller, keyed by path.
    pub worktree_statuses: HashMap<PathBuf, WorktreeStatus>,
    /// When the next background status refresh cycle will start.
    pub next_status_refresh: Option<Instant>,
    /// Path of a worktree that needs an immediate background status fetch.
    pub request_status_fetch: Option<PathBuf>,
    /// Current screen mode (Normal, Mini, MiniDrilldown).
    pub screen_mode: ScreenMode,
    /// State for mini mode view.
    pub mini: MiniModeState,
    /// Saved prompt templates for quick agent creation.
    pub saved_prompts: Vec<SavedPrompt>,
    /// Auto-captured summaries keyed by session id.
    pub agent_summaries: HashMap<u64, String>,
    /// Session id of the agent being drilled down into.
    pub mini_drilldown_session: Option<u64>,
    /// Whether mouse capture is active (enables scroll wheel, disables text selection).
    pub mouse_captured: bool,
    /// When mouse capture was last disabled (for auto-re-enable timer).
    pub mouse_capture_disabled_at: Option<Instant>,
    /// In-app text selection (drag-to-select in terminal pane).
    pub text_selection: Option<TextSelection>,
    /// Cached URL scan results from the active terminal screen.
    pub url_cache: url::UrlCache,
    /// Whether the URL cache needs to be refreshed (set on new PTY output, scroll, etc.).
    pub url_cache_dirty: bool,
    /// Claude Code context window usage per session, from debug logs.
    pub claude_usage: HashMap<u64, ClaudeUsage>,
    /// Global account-level usage from the Anthropic API.
    pub global_usage: Option<GlobalUsage>,
    /// Session IDs for terminal sessions (separate from worktree session_ids).
    pub terminal_ids: Vec<u64>,
    /// Selected index within the terminal panel.
    pub terminal_panel_selected: usize,
    /// Which sidebar panel currently has focus.
    pub sidebar_panel: SidebarPanel,
    /// Flat list of terminal panel items for rendering/navigation.
    pub terminal_panel_items: Vec<SidebarItem>,
    /// Screen regions of interactive UI elements, updated each frame for mouse hit-testing.
    pub areas: LayoutAreas,
    /// Latest version string from GitHub Releases, if newer than current.
    pub update_available: Option<String>,
    /// Mutex to serialize git operations across threads (status poller, push/pull, etc.).
    pub git_lock: std::sync::Arc<std::sync::Mutex<()>>,
}

impl App {
    pub fn new(bare_repo_path: PathBuf, event_tx: mpsc::UnboundedSender<AppEvent>, repo_detected: bool, tmux_available: bool, wt_available: bool) -> Self {
        Self {
            bare_repo_path,
            worktrees: Vec::new(),
            sessions: HashMap::new(),
            next_session_id: 1,
            focus: FocusTarget::Sidebar,
            input_mode: InputMode::Normal,
            active_session_id: None,
            active_worktree_idx: None,
            worktree_status: None,
            info_panel_section: 0,
            info_panel_cursor: 0,
            dialog: None,
            loading_message: None,
            pending_action: None,
            should_quit: false,
            sidebar_selected: 0,
            sidebar_hovered: None,
            terminal_panel_hovered: None,
            sidebar_items: Vec::new(),
            event_tx,
            status_message: None,
            status_message_time: None,
            status_severity: StatusSeverity::Info,
            sidebar_visible: true,
            repo_detected,
            regular_repo_path: None,
            tmux_available,
            wt_available,
            terminal_scroll: 0,
            pending_merge: None,
            prompt_queue_visible: false,
            prompt_queues: HashMap::new(),
            prompt_queue_selected: 0,
            prompt_queue_input: String::new(),
            prompt_queue_cursor: 0,
            prompt_queue_editing: None,
            prompt_queue_last_send: None,
            prompt_queue_last_check: None,
            show_help: false,
            help_tab: 0,
            help_scroll: 0,
            spinner_frame: 0,
            worktree_statuses: HashMap::new(),
            next_status_refresh: None,
            request_status_fetch: None,
            screen_mode: ScreenMode::Normal,
            mini: MiniModeState::default(),
            saved_prompts: Vec::new(),
            agent_summaries: HashMap::new(),
            mini_drilldown_session: None,
            mouse_captured: true,
            mouse_capture_disabled_at: None,
            text_selection: None,
            url_cache: url::UrlCache::default(),
            url_cache_dirty: true,
            claude_usage: HashMap::new(),
            global_usage: None,
            terminal_ids: Vec::new(),
            terminal_panel_selected: 0,
            sidebar_panel: SidebarPanel::Worktrees,
            terminal_panel_items: Vec::new(),
            areas: LayoutAreas::default(),
            update_available: None,
            git_lock: std::sync::Arc::new(std::sync::Mutex::new(())),
        }
    }

    /// Toggle focus: Sidebar → TerminalPane → PromptQueue (if visible) → Sidebar.
    pub fn toggle_focus(&mut self) {
        match self.focus {
            FocusTarget::Sidebar => {
                if self.active_session_id.is_some() {
                    self.focus = FocusTarget::TerminalPane;
                    self.input_mode = InputMode::Terminal;
                } else if self.active_worktree_idx.is_some() {
                    self.focus = FocusTarget::TerminalPane;
                    // Stay in Normal mode — info panel handles keys differently
                }
            }
            FocusTarget::TerminalPane => {
                if self.prompt_queue_visible && self.active_session_id.is_some() {
                    self.focus = FocusTarget::PromptQueue;
                    self.input_mode = InputMode::Normal;
                } else {
                    self.focus = FocusTarget::Sidebar;
                    self.input_mode = InputMode::Normal;
                }
            }
            FocusTarget::PromptQueue => {
                self.focus = FocusTarget::Sidebar;
                self.input_mode = InputMode::Normal;
            }
        }
    }

    /// Escape from terminal mode back to sidebar.
    pub fn escape_to_sidebar(&mut self) {
        self.focus = FocusTarget::Sidebar;
        self.input_mode = InputMode::Normal;
    }

    /// Rebuild the flat sidebar_items list from worktrees and sessions.
    pub fn rebuild_sidebar_items(&mut self) {
        self.sidebar_items.clear();
        for (wi, wt) in self.worktrees.iter().enumerate() {
            self.sidebar_items.push(SidebarItem::Worktree(wi));
            if wt.expanded {
                for (si, _sid) in wt.session_ids.iter().enumerate() {
                    self.sidebar_items.push(SidebarItem::Session(wi, si));
                }
            }
        }
        if !self.sidebar_items.is_empty() && self.sidebar_selected >= self.sidebar_items.len() {
            self.sidebar_selected = self.sidebar_items.len() - 1;
        }
        self.rebuild_terminal_panel_items();
    }

    /// Rebuild the terminal panel items list from terminal_ids.
    pub fn rebuild_terminal_panel_items(&mut self) {
        self.terminal_panel_items.clear();
        for (ti, _sid) in self.terminal_ids.iter().enumerate() {
            self.terminal_panel_items.push(SidebarItem::Terminal(ti));
        }
        if self.terminal_panel_items.is_empty() {
            // Fall back to Worktrees panel if no terminals
            if self.sidebar_panel == SidebarPanel::Terminals {
                self.sidebar_panel = SidebarPanel::Worktrees;
            }
        }
        if !self.terminal_panel_items.is_empty() && self.terminal_panel_selected >= self.terminal_panel_items.len() {
            self.terminal_panel_selected = self.terminal_panel_items.len() - 1;
        }
    }

    pub fn selected_sidebar_item(&self) -> Option<SidebarItem> {
        match self.sidebar_panel {
            SidebarPanel::Worktrees => self.sidebar_items.get(self.sidebar_selected).copied(),
            SidebarPanel::Terminals => self.terminal_panel_items.get(self.terminal_panel_selected).copied(),
        }
    }

    pub fn sidebar_up(&mut self) {
        match self.sidebar_panel {
            SidebarPanel::Worktrees => {
                if self.sidebar_selected > 0 {
                    self.sidebar_selected -= 1;
                }
            }
            SidebarPanel::Terminals => {
                if self.terminal_panel_selected > 0 {
                    self.terminal_panel_selected -= 1;
                } else {
                    // At top of Terminals panel, move to bottom of Worktrees panel
                    self.sidebar_panel = SidebarPanel::Worktrees;
                    if !self.sidebar_items.is_empty() {
                        self.sidebar_selected = self.sidebar_items.len() - 1;
                    }
                }
            }
        }
    }

    pub fn sidebar_down(&mut self) {
        match self.sidebar_panel {
            SidebarPanel::Worktrees => {
                if self.sidebar_selected + 1 < self.sidebar_items.len() {
                    self.sidebar_selected += 1;
                } else if !self.terminal_panel_items.is_empty() {
                    // At bottom of Worktrees panel, move to top of Terminals panel
                    self.sidebar_panel = SidebarPanel::Terminals;
                    self.terminal_panel_selected = 0;
                }
            }
            SidebarPanel::Terminals => {
                if self.terminal_panel_selected + 1 < self.terminal_panel_items.len() {
                    self.terminal_panel_selected += 1;
                }
            }
        }
    }

    pub fn sidebar_jump_top(&mut self) {
        self.sidebar_panel = SidebarPanel::Worktrees;
        self.sidebar_selected = 0;
    }

    pub fn sidebar_jump_bottom(&mut self) {
        if !self.terminal_panel_items.is_empty() {
            self.sidebar_panel = SidebarPanel::Terminals;
            self.terminal_panel_selected = self.terminal_panel_items.len() - 1;
        } else if !self.sidebar_items.is_empty() {
            self.sidebar_panel = SidebarPanel::Worktrees;
            self.sidebar_selected = self.sidebar_items.len() - 1;
        }
    }

    pub fn collapse_all(&mut self) {
        for wt in &mut self.worktrees {
            wt.expanded = false;
        }
        self.rebuild_sidebar_items();
    }

    pub fn expand_all(&mut self) {
        for wt in &mut self.worktrees {
            wt.expanded = true;
        }
        self.rebuild_sidebar_items();
    }

    pub fn toggle_expand(&mut self) {
        if let Some(SidebarItem::Worktree(wi)) = self.selected_sidebar_item() {
            if let Some(wt) = self.worktrees.get_mut(wi) {
                wt.expanded = !wt.expanded;
                self.rebuild_sidebar_items();
            }
        }
    }

    pub fn activate_selected(&mut self) {
        self.text_selection = None;
        match self.selected_sidebar_item() {
            Some(SidebarItem::Session(wi, si)) => {
                if let Some(wt) = self.worktrees.get(wi) {
                    if let Some(&sid) = wt.session_ids.get(si) {
                        self.active_session_id = Some(sid);
                        self.active_worktree_idx = None;
                        self.worktree_status = None;
                        self.terminal_scroll = 0;
                        self.focus = FocusTarget::TerminalPane;
                        self.input_mode = InputMode::Terminal;
                        self.url_cache_dirty = true;
                    }
                }
            }
            Some(SidebarItem::Worktree(wi)) => {
                if let Some(wt) = self.worktrees.get(wi) {
                    let wt_path = wt.path.clone();
                    // Show worktree info panel
                    self.active_session_id = None;
                    self.active_worktree_idx = Some(wi);
                    self.info_panel_section = 0;
                    self.info_panel_cursor = 0;
                    self.terminal_scroll = 0;
                    // Use cached status immediately if available (no blocking)
                    self.worktree_status = self.worktree_statuses.get(&wt_path).cloned();
                    // Request a background fetch for fresh data
                    self.request_status_fetch = Some(wt_path);
                    self.clamp_info_panel_cursor();
                    // Expand if collapsed
                    if let Some(wt) = self.worktrees.get_mut(wi) {
                        if !wt.expanded {
                            wt.expanded = true;
                            self.rebuild_sidebar_items();
                        }
                    }
                }
            }
            Some(SidebarItem::Terminal(ti)) => {
                if let Some(&sid) = self.terminal_ids.get(ti) {
                    self.active_session_id = Some(sid);
                    self.active_worktree_idx = None;
                    self.worktree_status = None;
                    self.terminal_scroll = 0;
                    self.focus = FocusTarget::TerminalPane;
                    self.input_mode = InputMode::Terminal;
                    self.url_cache_dirty = true;
                }
            }
            None => {}
        }
    }

    /// Refresh the cached worktree status if one is being viewed.
    /// Updates both the active display and the background cache.
    /// Clamps the info panel cursor to valid bounds.
    pub fn refresh_worktree_status(&mut self) {
        if let Some(wi) = self.active_worktree_idx {
            if let Ok(status) = worktree::fetch_worktree_status(self, wi) {
                // Update the per-worktree cache
                if let Some(wt) = self.worktrees.get(wi) {
                    self.worktree_statuses.insert(wt.path.clone(), status.clone());
                }
                self.worktree_status = Some(status);
            }
            self.clamp_info_panel_cursor();
        }
    }

    /// Queue a blocking action with a loading message. The main loop will
    /// draw the loading overlay, execute the action, then clear it.
    pub fn queue_action(&mut self, message: impl Into<String>, action: PendingAction) {
        self.loading_message = Some(message.into());
        self.pending_action = Some(action);
    }

    /// Returns true if the info panel is focused (TerminalPane focus with a worktree selected, no terminal session).
    pub fn info_panel_focused(&self) -> bool {
        self.focus == FocusTarget::TerminalPane
            && self.active_worktree_idx.is_some()
            && self.active_session_id.is_none()
    }

    /// Get the split unstaged/staged file lists from the cached worktree status.
    #[allow(clippy::type_complexity)]
    pub fn info_panel_file_lists(&self) -> (Vec<(char, String)>, Vec<(char, String)>) {
        let mut unstaged = Vec::new();
        let mut staged = Vec::new();
        if let Some(ref status) = self.worktree_status {
            for c in &status.files {
                if c.index_status != ' ' && c.index_status != '?' {
                    staged.push((c.index_status, c.path.clone()));
                }
                if c.work_status != ' ' || c.index_status == '?' {
                    let s = if c.index_status == '?' { '?' } else { c.work_status };
                    unstaged.push((s, c.path.clone()));
                }
            }
        }
        (unstaged, staged)
    }

    /// Clamp info panel cursor after a status refresh.
    pub fn clamp_info_panel_cursor(&mut self) {
        let (unstaged, staged) = self.info_panel_file_lists();
        // If current section is empty, switch to the other
        if self.info_panel_section == 0 && unstaged.is_empty() && !staged.is_empty() {
            self.info_panel_section = 1;
        } else if self.info_panel_section == 1 && staged.is_empty() && !unstaged.is_empty() {
            self.info_panel_section = 0;
        }
        let len = if self.info_panel_section == 0 { unstaged.len() } else { staged.len() };
        if len == 0 {
            self.info_panel_cursor = 0;
        } else if self.info_panel_cursor >= len {
            self.info_panel_cursor = len - 1;
        }
    }

    pub fn open_dialog(&mut self, dialog: Dialog) {
        self.dialog = Some(dialog);
        self.input_mode = InputMode::Dialog;
    }

    pub fn close_dialog(&mut self) {
        self.dialog = None;
        self.input_mode = match self.focus {
            FocusTarget::Sidebar => InputMode::Normal,
            FocusTarget::TerminalPane => {
                if self.active_worktree_idx.is_some() && self.active_session_id.is_none() {
                    InputMode::Normal // info panel stays in Normal mode
                } else {
                    InputMode::Terminal
                }
            }
            FocusTarget::PromptQueue => InputMode::Normal,
        };
    }

    pub fn set_status(&mut self, msg: impl Into<String>) {
        self.status_message = Some(msg.into());
        self.status_message_time = Some(Instant::now());
        self.status_severity = StatusSeverity::Info;
    }

    pub fn set_status_with(&mut self, severity: StatusSeverity, msg: impl Into<String>) {
        self.status_message = Some(msg.into());
        self.status_message_time = Some(Instant::now());
        self.status_severity = severity;
    }

    /// Get the prompt queue for the active session (immutable).
    pub fn active_prompt_queue(&self) -> &[String] {
        self.active_session_id
            .and_then(|sid| self.prompt_queues.get(&sid))
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    /// Returns true if the prompt queue panel is focused.
    pub fn prompt_queue_focused(&self) -> bool {
        self.focus == FocusTarget::PromptQueue
    }

    /// Height of the queue panel in rows (including borders).
    pub fn queue_panel_height(&self) -> u16 {
        let queue_len = self.active_prompt_queue().len();
        // 2 for borders + 1 for input line + queue items (min 1 for empty state)
        let content_rows = 1 + queue_len.max(1);
        (content_rows as u16 + 2).min(12) // cap at 12 rows
    }

    /// Path to the prompt queues persistence file.
    fn prompt_queues_path(&self) -> PathBuf {
        self.bare_repo_path.join(".prompt_queues.json")
    }

    /// Save all prompt queues to disk, keyed by tmux session name.
    pub fn save_prompt_queues(&self) {
        let mut data: HashMap<String, Vec<String>> = HashMap::new();
        for (sid, queue) in &self.prompt_queues {
            if queue.is_empty() {
                continue;
            }
            // Use tmux session name as stable key; skip non-tmux sessions
            if let Some(session) = self.sessions.get(sid) {
                if let Some(ref tmux_name) = session.tmux_session_name {
                    data.insert(tmux_name.clone(), queue.clone());
                }
            }
        }
        let path = self.prompt_queues_path();
        if data.is_empty() {
            // Remove file if no queues to save
            let _ = std::fs::remove_file(&path);
            return;
        }
        match serde_json::to_string_pretty(&data) {
            Ok(json) => {
                if let Err(e) = std::fs::write(&path, json) {
                    tracing::warn!("Failed to save prompt queues: {}", e);
                }
            }
            Err(e) => {
                tracing::warn!("Failed to serialize prompt queues: {}", e);
            }
        }
    }

    /// Rebuild the tree item list for mini mode from all worktrees and sessions.
    pub fn rebuild_mini_agent_list(&mut self) {
        self.mini.items.clear();
        for (wi, wt) in self.worktrees.iter().enumerate() {
            self.mini.items.push(SidebarItem::Worktree(wi));
            if wt.expanded {
                for (si, _sid) in wt.session_ids.iter().enumerate() {
                    self.mini.items.push(SidebarItem::Session(wi, si));
                }
            }
        }
        if !self.mini.items.is_empty() && self.mini.selected >= self.mini.items.len() {
            self.mini.selected = self.mini.items.len() - 1;
        }
    }

    /// Path to the saved prompts persistence file.
    fn saved_prompts_path(&self) -> PathBuf {
        self.bare_repo_path.join(".agent_prompts.json")
    }

    /// Load saved prompt templates from disk.
    pub fn load_saved_prompts(&mut self) {
        let path = self.saved_prompts_path();
        let json = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(_) => return,
        };
        match serde_json::from_str(&json) {
            Ok(prompts) => self.saved_prompts = prompts,
            Err(e) => tracing::warn!("Failed to parse saved prompts: {}", e),
        }
    }

    /// Save prompt templates to disk.
    pub fn save_saved_prompts(&self) {
        let path = self.saved_prompts_path();
        if self.saved_prompts.is_empty() {
            let _ = std::fs::remove_file(&path);
            return;
        }
        match serde_json::to_string_pretty(&self.saved_prompts) {
            Ok(json) => {
                if let Err(e) = std::fs::write(&path, json) {
                    tracing::warn!("Failed to save prompts: {}", e);
                }
            }
            Err(e) => tracing::warn!("Failed to serialize prompts: {}", e),
        }
    }

    /// Load prompt queues from disk for all current sessions (by tmux name).
    pub fn load_prompt_queues(&mut self) {
        let path = self.prompt_queues_path();
        let json = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(_) => return, // No file, nothing to load
        };
        let data: HashMap<String, Vec<String>> = match serde_json::from_str(&json) {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!("Failed to parse prompt queues file: {}", e);
                return;
            }
        };
        if data.is_empty() {
            return;
        }
        // Match tmux session names to current session IDs
        for (sid, session) in &self.sessions {
            if let Some(ref tmux_name) = session.tmux_session_name {
                if let Some(queue) = data.get(tmux_name) {
                    // Trim whitespace/newlines that may have accumulated
                    let cleaned: Vec<String> = queue
                        .iter()
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect();
                    if !cleaned.is_empty() {
                        self.prompt_queues.insert(*sid, cleaned);
                    }
                }
            }
        }
    }
}

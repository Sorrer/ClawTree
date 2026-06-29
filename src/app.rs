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
}

/// Computed agent status for sidebar display.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentStatus {
    Working,
    Idle,
    NeedsInput,
    /// Claude Code is stuck on a transient server-side rate limit (the
    /// "API Error: Server is temporarily limiting requests" message), with
    /// nothing else running. Cleared automatically once it retries successfully
    /// or the user moves on (the message leaves the visible screen).
    RateLimited,
    Exited,
}

/// A saved prompt template for quick agent creation.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SavedPrompt {
    pub name: String,
    pub prompt: String,
}

/// Expand a user-typed path the way a shell roughly would, since dialog text
/// input has no shell to do it for us. Handles:
///   - surrounding single/double quotes (e.g. a pasted `"~/my folder"`)
///   - a leading `~` / `~/...` → `$HOME`
///   - environment variables `$VAR` and `${VAR}` (unset vars are left literal)
///
/// Relative paths (`./`, `../`) are intentionally left alone — `canonicalize`
/// already resolves them against the current directory.
pub fn expand_path(input: &str) -> PathBuf {
    let mut s = input.trim();

    // Strip one layer of matching surrounding quotes.
    if s.len() >= 2 {
        let bytes = s.as_bytes();
        let first = bytes[0];
        if (first == b'"' || first == b'\'') && bytes[bytes.len() - 1] == first {
            s = &s[1..s.len() - 1];
        }
    }

    // Expand environment variables: $VAR and ${VAR}.
    let mut expanded = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '$' {
            expanded.push(c);
            continue;
        }
        // Braced form: ${VAR}
        if chars.peek() == Some(&'{') {
            chars.next(); // consume '{'
            let mut name = String::new();
            let mut closed = false;
            for nc in chars.by_ref() {
                if nc == '}' {
                    closed = true;
                    break;
                }
                name.push(nc);
            }
            match (closed, std::env::var(&name)) {
                (true, Ok(val)) => expanded.push_str(&val),
                // Unset or unterminated: keep the original text literal.
                (true, Err(_)) => {
                    expanded.push_str("${");
                    expanded.push_str(&name);
                    expanded.push('}');
                }
                (false, _) => {
                    expanded.push_str("${");
                    expanded.push_str(&name);
                }
            }
            continue;
        }
        // Bare form: $VAR (alphanumeric + underscore)
        let mut name = String::new();
        while let Some(&nc) = chars.peek() {
            if nc.is_alphanumeric() || nc == '_' {
                name.push(nc);
                chars.next();
            } else {
                break;
            }
        }
        match std::env::var(&name) {
            Ok(val) if !name.is_empty() => expanded.push_str(&val),
            _ => {
                // Unset var or lone `$`: keep literal.
                expanded.push('$');
                expanded.push_str(&name);
            }
        }
    }

    // Expand a leading tilde.
    if expanded == "~" || expanded.starts_with("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(home).join(expanded.strip_prefix("~/").unwrap_or(""));
        }
    }

    PathBuf::from(expanded)
}

/// An extra (non-worktree) directory where the user wants to launch Claude sessions.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ExtraLocation {
    pub path: PathBuf,
    pub name: Option<String>,
    #[serde(skip)]
    pub session_ids: Vec<u64>,
    #[serde(skip)]
    pub expanded: bool,
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
    RemoveLocation,
}

/// Build a list of context actions appropriate for the given sidebar item.
pub fn context_actions_for_item(item: &SidebarItem, wt_available: bool) -> Vec<ContextAction> {
    match item {
        SidebarItem::Project => {
            vec![
                ContextAction {
                    label: "New Claude".into(),
                    hotkey: "c".into(),
                    kind: ContextActionKind::NewClaude,
                },
                ContextAction {
                    label: "New Terminal".into(),
                    hotkey: "t".into(),
                    kind: ContextActionKind::NewTerminal,
                },
                ContextAction {
                    label: "New Worktree".into(),
                    hotkey: "n".into(),
                    kind: ContextActionKind::NewWorktree,
                },
            ]
        }
        SidebarItem::Worktree(_) => {
            let mut actions = vec![
                ContextAction {
                    label: "New Claude".into(),
                    hotkey: "c".into(),
                    kind: ContextActionKind::NewClaude,
                },
                ContextAction {
                    label: "New Claude (YOLO)".into(),
                    hotkey: "C".into(),
                    kind: ContextActionKind::NewClaudeYolo,
                },
                ContextAction {
                    label: "New Terminal".into(),
                    hotkey: "t".into(),
                    kind: ContextActionKind::NewTerminal,
                },
                ContextAction {
                    label: "Stage & Commit".into(),
                    hotkey: "s".into(),
                    kind: ContextActionKind::StageCommit,
                },
                ContextAction {
                    label: "Stage All + AI Commit".into(),
                    hotkey: "^s".into(),
                    kind: ContextActionKind::StageAllClaudeCommit,
                },
                ContextAction {
                    label: "Push".into(),
                    hotkey: "p".into(),
                    kind: ContextActionKind::Push,
                },
                ContextAction {
                    label: "Pull".into(),
                    hotkey: "P".into(),
                    kind: ContextActionKind::Pull,
                },
                ContextAction {
                    label: "Merge Branch".into(),
                    hotkey: "m".into(),
                    kind: ContextActionKind::MergeBranch,
                },
                ContextAction {
                    label: "New Worktree".into(),
                    hotkey: "n".into(),
                    kind: ContextActionKind::NewWorktree,
                },
                ContextAction {
                    label: "Copy Last URL".into(),
                    hotkey: "u".into(),
                    kind: ContextActionKind::CopyLastUrl,
                },
                ContextAction {
                    label: "Open Last URL".into(),
                    hotkey: "U".into(),
                    kind: ContextActionKind::OpenLastUrl,
                },
                ContextAction {
                    label: "Delete Worktree".into(),
                    hotkey: "d".into(),
                    kind: ContextActionKind::DeleteWorktree,
                },
                ContextAction {
                    label: "Force Delete Worktree".into(),
                    hotkey: "D".into(),
                    kind: ContextActionKind::ForceDeleteWorktree,
                },
            ];
            if wt_available {
                actions.insert(
                    actions.len() - 2,
                    ContextAction {
                        label: "Windows Terminal".into(),
                        hotkey: "w".into(),
                        kind: ContextActionKind::OpenWindowsTerminal,
                    },
                );
                actions.insert(
                    actions.len() - 2,
                    ContextAction {
                        label: "Win Terminal + Claude".into(),
                        hotkey: "W".into(),
                        kind: ContextActionKind::OpenWindowsTerminalClaude,
                    },
                );
            }
            actions
        }
        SidebarItem::ProjectSession(_)
        | SidebarItem::Session(_, _)
        | SidebarItem::Terminal(_)
        | SidebarItem::LocationSession(_, _) => {
            vec![
                ContextAction {
                    label: "Rename Session".into(),
                    hotkey: "r".into(),
                    kind: ContextActionKind::RenameSession,
                },
                ContextAction {
                    label: "Copy Last URL".into(),
                    hotkey: "u".into(),
                    kind: ContextActionKind::CopyLastUrl,
                },
                ContextAction {
                    label: "Open Last URL".into(),
                    hotkey: "U".into(),
                    kind: ContextActionKind::OpenLastUrl,
                },
                ContextAction {
                    label: "Delete Session".into(),
                    hotkey: "d".into(),
                    kind: ContextActionKind::DeleteSession,
                },
            ]
        }
        SidebarItem::Location(_) => {
            vec![
                ContextAction {
                    label: "New Claude".into(),
                    hotkey: "c".into(),
                    kind: ContextActionKind::NewClaude,
                },
                ContextAction {
                    label: "New Claude (YOLO)".into(),
                    hotkey: "C".into(),
                    kind: ContextActionKind::NewClaudeYolo,
                },
                ContextAction {
                    label: "Remove Location".into(),
                    hotkey: "d".into(),
                    kind: ContextActionKind::RemoveLocation,
                },
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
    RenameSession { session_id: u64, input: String },
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
    MergeSuccess { message: String },
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
        selected: usize, // 0=Claude, 1=Claude (skip perms), 2=Dismiss
    },
    /// Git authentication failed — show friendly instructions.
    AuthError {
        operation: String, // "push", "pull", "clone"
        message: String,   // the raw git error
        selected: usize,   // 0 = Dismiss
    },
    /// Right-click / "+" context menu for sidebar items.
    ContextMenu {
        item: SidebarItem,
        selected: Option<usize>,
        actions: Vec<ContextAction>,
    },
    /// A newer version is available — offer to update in-place.
    UpdateAvailable {
        latest_version: String,
        selected: usize, // 0=Update now, 1=Dismiss, 2=Skip this version
        phase: UpdatePhase,
    },
    /// Add an extra location directory.
    AddLocation {
        path_input: String,
        /// Inline validation error shown inside the dialog (e.g. missing dir).
        error: Option<String>,
    },
    /// Interactive staging and commit UI.
    GitCommit {
        worktree_idx: usize,
        unstaged: Vec<(char, String)>, // (status_char, path)
        staged: Vec<(char, String)>,   // (status_char, path)
        section: usize,                // 0=unstaged, 1=staged
        selected: usize,               // index within current section
        phase: CommitPhase,
        commit_message: String,
        cursor_pos: usize, // character offset into commit_message
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

/// Number of options in UpdateAvailable dialog (Prompt phase).
pub const UPDATE_AVAILABLE_OPTION_COUNT: usize = 3;

/// Number of options in UpdateAvailable dialog (Complete phase): Restart now / Later.
pub const UPDATE_COMPLETE_OPTION_COUNT: usize = 2;

/// Phase of the in-app update dialog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdatePhase {
    /// Showing the prompt with three options.
    Prompt,
    /// Downloading and verifying the new binary.
    Downloading,
    /// Replacing the current binary.
    Replacing,
    /// Update installed — prompt the user to restart now or later.
    Complete,
    /// An error occurred during download or replace.
    Failed(String),
}

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
    RemoveLocation(usize),
}

/// A blocking git action queued for execution by the main loop.
/// The main loop draws a loading overlay first, then runs the action.
#[derive(Debug)]
pub enum PendingAction {
    StageFile {
        worktree_idx: usize,
        file: String,
    },
    UnstageFile {
        worktree_idx: usize,
        file: String,
    },
    StageAll {
        worktree_idx: usize,
        then_claude: bool,
    },
    Commit {
        worktree_idx: usize,
        message: String,
    },
    CommitAndPush {
        worktree_idx: usize,
        message: String,
    },
    RefreshWorktreeStatus,
    OpenStageCommit {
        worktree_idx: usize,
    },
    StageAllAndCommitClaude {
        worktree_idx: usize,
    },
    MergeExecute {
        source_worktree_idx: usize,
        target_branch: String,
    },
}

/// Whether to auto-open the init or convert dialog on first tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoInitKind {
    /// Auto-open the convert dialog (regular .git repo detected).
    Convert,
    /// Auto-open the init dialog (no repo detected at all).
    Init,
}

/// Sidebar item types for the tree view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidebarItem {
    /// Global project overview (always first in the sidebar).
    Project,
    /// A session spawned at the project root (index into app.project_session_ids).
    ProjectSession(usize),
    Worktree(usize),
    Session(usize, usize), // (worktree_index, session_index within worktree)
    Terminal(usize),       // index into app.terminal_ids
    /// An extra location directory (index into app.locations).
    Location(usize),
    /// A session under an extra location (location_index, session_index).
    LocationSession(usize, usize),
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
    /// Whether the project overview panel is being displayed.
    pub project_overview_active: bool,
    /// Session IDs for sessions spawned at the project root.
    pub project_session_ids: Vec<u64>,
    /// Whether the Project entry is expanded (showing its sessions).
    pub project_expanded: bool,
    pub worktree_status: Option<WorktreeStatus>,
    /// Cursor position for interactive file list in the info panel.
    pub info_panel_section: usize, // 0=unstaged, 1=staged
    pub info_panel_cursor: usize, // index within current section
    /// Scroll offset for the worktree info panel content.
    pub info_panel_scroll: usize,
    pub dialog: Option<Dialog>,
    /// Loading overlay message shown during blocking git operations.
    pub loading_message: Option<String>,
    /// Queued blocking action to execute after drawing the loading overlay.
    pub pending_action: Option<PendingAction>,
    pub should_quit: bool,
    pub sidebar_selected: usize,
    /// First visible item index of the worktrees list (viewport scroll offset).
    /// Driven independently by the mouse wheel; keyboard navigation nudges it
    /// only enough to keep the selected item on screen.
    pub sidebar_scroll: usize,
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
    /// Auto-open init or convert dialog on first tick (consumed once).
    pub auto_init_pending: Option<AutoInitKind>,
    /// Whether tmux is available for session persistence.
    pub tmux_available: bool,
    /// Whether wt.exe (Windows Terminal) is available.
    pub wt_available: bool,
    /// Last known terminal size (cols, rows) for spawning sessions from mouse handlers.
    pub last_terminal_size: (u16, u16),
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
    /// Current screen mode.
    pub screen_mode: ScreenMode,
    /// Saved prompt templates for quick agent creation.
    pub saved_prompts: Vec<SavedPrompt>,
    /// Auto-captured summaries keyed by session id.
    pub agent_summaries: HashMap<u64, String>,
    /// Resolved display names for plain terminal sessions (running command / cwd),
    /// refreshed off the render path by the terminal-name poller.
    pub terminal_names: HashMap<u64, String>,
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
    /// Version string the user chose to permanently skip (loaded from config file).
    pub skipped_update_version: Option<String>,
    /// Whether the update dialog has already been shown this session (dismiss = don't re-show).
    pub update_dialog_shown: bool,
    /// Set when the user chooses "Restart now" after an in-app update; the main
    /// loop exits and re-execs the freshly installed binary.
    pub restart_requested: bool,
    /// Mutex to serialize git operations across threads (status poller, push/pull, etc.).
    pub git_lock: std::sync::Arc<std::sync::Mutex<()>>,
    /// Extra (non-worktree) directories where the user wants to launch Claude sessions.
    pub locations: Vec<ExtraLocation>,
}

impl App {
    pub fn new(
        bare_repo_path: PathBuf,
        event_tx: mpsc::UnboundedSender<AppEvent>,
        repo_detected: bool,
        tmux_available: bool,
        wt_available: bool,
    ) -> Self {
        Self {
            bare_repo_path,
            worktrees: Vec::new(),
            sessions: HashMap::new(),
            next_session_id: 1,
            focus: FocusTarget::Sidebar,
            input_mode: InputMode::Normal,
            active_session_id: None,
            active_worktree_idx: None,
            project_overview_active: false,
            project_session_ids: Vec::new(),
            project_expanded: true,
            worktree_status: None,
            info_panel_section: 0,
            info_panel_cursor: 0,
            info_panel_scroll: 0,
            dialog: None,
            loading_message: None,
            pending_action: None,
            should_quit: false,
            sidebar_selected: 0,
            sidebar_scroll: 0,
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
            auto_init_pending: None,
            tmux_available,
            wt_available,
            last_terminal_size: crossterm::terminal::size().unwrap_or((80, 24)),
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
            saved_prompts: Vec::new(),
            agent_summaries: HashMap::new(),
            mouse_captured: true,
            mouse_capture_disabled_at: None,
            text_selection: None,
            url_cache: url::UrlCache::default(),
            url_cache_dirty: true,
            claude_usage: HashMap::new(),
            terminal_names: HashMap::new(),
            global_usage: None,
            terminal_ids: Vec::new(),
            terminal_panel_selected: 0,
            sidebar_panel: SidebarPanel::Worktrees,
            terminal_panel_items: Vec::new(),
            areas: LayoutAreas::default(),
            update_available: None,
            skipped_update_version: None,
            update_dialog_shown: false,
            restart_requested: false,
            git_lock: std::sync::Arc::new(std::sync::Mutex::new(())),
            locations: Vec::new(),
        }
    }

    /// Toggle focus: Sidebar → TerminalPane → PromptQueue (if visible) → Sidebar.
    pub fn toggle_focus(&mut self) {
        match self.focus {
            FocusTarget::Sidebar => {
                if self.active_session_id.is_some() {
                    self.focus = FocusTarget::TerminalPane;
                    self.input_mode = InputMode::Terminal;
                } else if self.active_worktree_idx.is_some() || self.project_overview_active {
                    self.focus = FocusTarget::TerminalPane;
                    // Stay in Normal mode — info/overview panel handles keys differently
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
        // Project overview is always the first item
        self.sidebar_items.push(SidebarItem::Project);
        if self.project_expanded {
            for (si, _sid) in self.project_session_ids.iter().enumerate() {
                self.sidebar_items.push(SidebarItem::ProjectSession(si));
            }
        }
        for (wi, wt) in self.worktrees.iter().enumerate() {
            self.sidebar_items.push(SidebarItem::Worktree(wi));
            if wt.expanded {
                for (si, _sid) in wt.session_ids.iter().enumerate() {
                    self.sidebar_items.push(SidebarItem::Session(wi, si));
                }
            }
        }
        for (li, loc) in self.locations.iter().enumerate() {
            self.sidebar_items.push(SidebarItem::Location(li));
            if loc.expanded {
                for (si, _sid) in loc.session_ids.iter().enumerate() {
                    self.sidebar_items
                        .push(SidebarItem::LocationSession(li, si));
                }
            }
        }
        if !self.sidebar_items.is_empty() && self.sidebar_selected >= self.sidebar_items.len() {
            self.sidebar_selected = self.sidebar_items.len() - 1;
        }
        // Keep the scroll offset valid so render and mouse hit-testing agree even
        // after the list shrinks (e.g. collapsing a worktree).
        let max_scroll = self.sidebar_max_scroll();
        if self.sidebar_scroll > max_scroll {
            self.sidebar_scroll = max_scroll;
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
        if !self.terminal_panel_items.is_empty()
            && self.terminal_panel_selected >= self.terminal_panel_items.len()
        {
            self.terminal_panel_selected = self.terminal_panel_items.len() - 1;
        }
    }

    pub fn selected_sidebar_item(&self) -> Option<SidebarItem> {
        match self.sidebar_panel {
            SidebarPanel::Worktrees => self.sidebar_items.get(self.sidebar_selected).copied(),
            SidebarPanel::Terminals => self
                .terminal_panel_items
                .get(self.terminal_panel_selected)
                .copied(),
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
        self.ensure_sidebar_selected_visible();
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
        self.ensure_sidebar_selected_visible();
    }

    pub fn sidebar_jump_top(&mut self) {
        self.sidebar_panel = SidebarPanel::Worktrees;
        self.sidebar_selected = 0;
        self.ensure_sidebar_selected_visible();
    }

    pub fn sidebar_jump_bottom(&mut self) {
        if !self.terminal_panel_items.is_empty() {
            self.sidebar_panel = SidebarPanel::Terminals;
            self.terminal_panel_selected = self.terminal_panel_items.len() - 1;
        } else if !self.sidebar_items.is_empty() {
            self.sidebar_panel = SidebarPanel::Worktrees;
            self.sidebar_selected = self.sidebar_items.len() - 1;
        }
        self.ensure_sidebar_selected_visible();
    }

    /// Visible height (in rows) of the worktrees list viewport, derived from the
    /// inner area recorded during the last render. Zero before the first frame.
    fn sidebar_viewport_height(&self) -> usize {
        self.areas.sidebar_inner.get().height as usize
    }

    /// Largest valid scroll offset so the last item can sit at the bottom of the
    /// viewport without leaving an empty gap below it.
    fn sidebar_max_scroll(&self) -> usize {
        let vh = self.sidebar_viewport_height();
        self.sidebar_items.len().saturating_sub(vh)
    }

    /// Scroll the worktrees list viewport by `delta` rows. This moves the visible
    /// window only — the selection is left untouched (the mouse wheel scrolls the
    /// list, it does not move the cursor).
    pub fn sidebar_scroll_by(&mut self, delta: isize) {
        let max = self.sidebar_max_scroll();
        let new = (self.sidebar_scroll as isize + delta).clamp(0, max as isize);
        self.sidebar_scroll = new as usize;
    }

    /// Nudge the scroll offset just enough to keep the selected item visible after
    /// a keyboard move. Items are all one row tall, so the offset maps directly to
    /// an item index.
    pub fn ensure_sidebar_selected_visible(&mut self) {
        let vh = self.sidebar_viewport_height();
        if vh == 0 {
            return;
        }
        if self.sidebar_selected < self.sidebar_scroll {
            self.sidebar_scroll = self.sidebar_selected;
        } else if self.sidebar_selected >= self.sidebar_scroll + vh {
            self.sidebar_scroll = self.sidebar_selected + 1 - vh;
        }
        // Clamp in case the list shrank since the offset was last set.
        let max = self.sidebar_max_scroll();
        if self.sidebar_scroll > max {
            self.sidebar_scroll = max;
        }
    }

    pub fn collapse_all(&mut self) {
        for wt in &mut self.worktrees {
            wt.expanded = false;
        }
        for loc in &mut self.locations {
            loc.expanded = false;
        }
        self.rebuild_sidebar_items();
    }

    pub fn expand_all(&mut self) {
        for wt in &mut self.worktrees {
            wt.expanded = true;
        }
        for loc in &mut self.locations {
            loc.expanded = true;
        }
        self.rebuild_sidebar_items();
    }

    pub fn toggle_expand(&mut self) {
        match self.selected_sidebar_item() {
            Some(SidebarItem::Project) | Some(SidebarItem::ProjectSession(_)) => {
                self.project_expanded = !self.project_expanded;
                self.rebuild_sidebar_items();
            }
            Some(SidebarItem::Worktree(wi)) => {
                if let Some(wt) = self.worktrees.get_mut(wi) {
                    wt.expanded = !wt.expanded;
                    self.rebuild_sidebar_items();
                }
            }
            Some(SidebarItem::Location(li)) | Some(SidebarItem::LocationSession(li, _)) => {
                if let Some(loc) = self.locations.get_mut(li) {
                    loc.expanded = !loc.expanded;
                    self.rebuild_sidebar_items();
                }
            }
            _ => {}
        }
    }

    pub fn activate_selected(&mut self) {
        self.text_selection = None;
        match self.selected_sidebar_item() {
            Some(SidebarItem::Project) => {
                self.active_session_id = None;
                self.active_worktree_idx = None;
                self.worktree_status = None;
                self.project_overview_active = true;
                self.terminal_scroll = 0;
            }
            Some(SidebarItem::ProjectSession(si)) => {
                self.project_overview_active = false;
                if let Some(&sid) = self.project_session_ids.get(si) {
                    self.active_session_id = Some(sid);
                    self.active_worktree_idx = None;
                    self.worktree_status = None;
                    self.terminal_scroll = 0;
                    self.focus = FocusTarget::TerminalPane;
                    self.input_mode = InputMode::Terminal;
                    self.url_cache_dirty = true;
                }
            }
            Some(SidebarItem::Session(wi, si)) => {
                self.project_overview_active = false;
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
                self.project_overview_active = false;
                if let Some(wt) = self.worktrees.get(wi) {
                    let wt_path = wt.path.clone();
                    // Show worktree info panel
                    self.active_session_id = None;
                    self.active_worktree_idx = Some(wi);
                    self.info_panel_section = 0;
                    self.info_panel_cursor = 0;
                    self.info_panel_scroll = 0;
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
                self.project_overview_active = false;
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
            Some(SidebarItem::Location(_li)) => {
                self.project_overview_active = false;
                self.active_session_id = None;
                self.active_worktree_idx = None;
                self.worktree_status = None;
                self.terminal_scroll = 0;
            }
            Some(SidebarItem::LocationSession(li, si)) => {
                self.project_overview_active = false;
                if let Some(loc) = self.locations.get(li) {
                    if let Some(&sid) = loc.session_ids.get(si) {
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
            None => {}
        }
        // Viewing a session marks it read.
        if let Some(sid) = self.active_session_id {
            if let Some(session) = self.sessions.get_mut(&sid) {
                session.unread = false;
            }
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
                    self.worktree_statuses
                        .insert(wt.path.clone(), status.clone());
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
                    let s = if c.index_status == '?' {
                        '?'
                    } else {
                        c.work_status
                    };
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
        let len = if self.info_panel_section == 0 {
            unstaged.len()
        } else {
            staged.len()
        };
        if len == 0 {
            self.info_panel_cursor = 0;
        } else if self.info_panel_cursor >= len {
            self.info_panel_cursor = len - 1;
        }
    }

    /// Compute the line index of the current cursor position in the info panel
    /// content (matching the line vector built in `draw_worktree_info`).
    pub fn info_panel_cursor_line(&self) -> usize {
        let (unstaged, staged) = self.info_panel_file_lists();
        let has_files = self
            .worktree_status
            .as_ref()
            .is_some_and(|s| !s.files.is_empty());
        if !has_files {
            return 0;
        }
        // Fixed header: Branch, HEAD, Refresh, blank = 4 lines
        // Then Unstaged header = line 4
        let unstaged_start = 5; // first unstaged file line
        if self.info_panel_section == 0 {
            // Cursor is in the unstaged section
            let item_count = if unstaged.is_empty() {
                1
            } else {
                unstaged.len()
            };
            return unstaged_start + self.info_panel_cursor.min(item_count.saturating_sub(1));
        }
        // Section 1 (staged): skip unstaged items + blank + staged header
        let unstaged_lines = if unstaged.is_empty() {
            1
        } else {
            unstaged.len()
        };
        let staged_start = unstaged_start + unstaged_lines + 2; // blank + staged header
        staged_start + self.info_panel_cursor.min(staged.len().saturating_sub(1))
    }

    /// Adjust `info_panel_scroll` so the cursor line is visible within `visible_height` rows.
    pub fn ensure_info_cursor_visible(&mut self, visible_height: usize) {
        if visible_height == 0 {
            return;
        }
        let cursor_line = self.info_panel_cursor_line();
        // Scroll up if cursor is above viewport
        if cursor_line < self.info_panel_scroll {
            self.info_panel_scroll = cursor_line;
        }
        // Scroll down if cursor is below viewport
        if cursor_line >= self.info_panel_scroll + visible_height {
            self.info_panel_scroll = cursor_line + 1 - visible_height;
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

    /// Path to the skip-update-version config file.
    fn skip_update_path() -> PathBuf {
        let config_dir = std::env::var("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                let home = std::env::var("HOME")
                    .map(PathBuf::from)
                    .unwrap_or_else(|_| PathBuf::from("."));
                home.join(".config")
            })
            .join("clawtree");
        config_dir.join("skip-update-version")
    }

    /// Load the skipped update version from the config file.
    pub fn load_skipped_update_version(&mut self) {
        let path = Self::skip_update_path();
        if let Ok(contents) = std::fs::read_to_string(&path) {
            let version = contents.trim().to_string();
            if !version.is_empty() {
                self.skipped_update_version = Some(version);
            }
        }
    }

    /// Save a version to skip permanently.
    pub fn save_skipped_update_version(&self, version: &str) {
        let path = Self::skip_update_path();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&path, version);
    }

    /// Path to the global locations persistence file.
    fn locations_path() -> PathBuf {
        let home = std::env::var("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("."));
        home.join(".clawtree").join("locations.json")
    }

    /// Load extra locations from disk.
    pub fn load_locations(&mut self) {
        let path = Self::locations_path();
        let json = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(_) => return,
        };
        match serde_json::from_str::<Vec<ExtraLocation>>(&json) {
            Ok(locs) => self.locations = locs,
            Err(e) => tracing::warn!("Failed to parse locations: {}", e),
        }
        self.rebuild_sidebar_items();
    }

    /// Save extra locations to disk.
    pub fn save_locations(&self) {
        let path = Self::locations_path();
        if self.locations.is_empty() {
            let _ = std::fs::remove_file(&path);
            return;
        }
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match serde_json::to_string_pretty(&self.locations) {
            Ok(json) => {
                if let Err(e) = std::fs::write(&path, json) {
                    tracing::warn!("Failed to save locations: {}", e);
                }
            }
            Err(e) => tracing::warn!("Failed to serialize locations: {}", e),
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

#[cfg(test)]
mod expand_path_tests {
    use super::expand_path;
    use std::path::PathBuf;

    #[test]
    fn expands_tilde() {
        std::env::set_var("HOME", "/home/tester");
        assert_eq!(expand_path("~/test"), PathBuf::from("/home/tester/test"));
        assert_eq!(expand_path("~"), PathBuf::from("/home/tester"));
    }

    #[test]
    fn strips_surrounding_quotes() {
        std::env::set_var("HOME", "/home/tester");
        assert_eq!(
            expand_path("\"~/my folder\""),
            PathBuf::from("/home/tester/my folder")
        );
        assert_eq!(expand_path("'/tmp/x'"), PathBuf::from("/tmp/x"));
    }

    #[test]
    fn expands_env_vars() {
        std::env::set_var("CLAWTREE_TEST_DIR", "/opt/data");
        assert_eq!(
            expand_path("$CLAWTREE_TEST_DIR/sub"),
            PathBuf::from("/opt/data/sub")
        );
        assert_eq!(
            expand_path("${CLAWTREE_TEST_DIR}/sub"),
            PathBuf::from("/opt/data/sub")
        );
    }

    #[test]
    fn leaves_unset_vars_and_relative_literal() {
        std::env::remove_var("CLAWTREE_NOPE");
        assert_eq!(
            expand_path("$CLAWTREE_NOPE/x"),
            PathBuf::from("$CLAWTREE_NOPE/x")
        );
        assert_eq!(expand_path("./rel"), PathBuf::from("./rel"));
        assert_eq!(expand_path("../up"), PathBuf::from("../up"));
    }
}

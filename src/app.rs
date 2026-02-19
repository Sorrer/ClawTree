use std::collections::HashMap;
use std::path::PathBuf;
use tokio::sync::mpsc;

use crate::event::AppEvent;
use crate::session::Session;
use crate::worktree::{Worktree, WorktreeStatus};

/// Which pane has keyboard focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusTarget {
    Sidebar,
    TerminalPane,
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
        selected: usize,              // 0=Commit, 1=Claude, 2=Cancel
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
    },
}

/// Number of conflict resolution options in MergeConflict dialog.
pub const CONFLICT_RESOLVER_COUNT: usize = 5;

/// Number of options in DirtyWorktree dialog.
pub const DIRTY_WORKTREE_OPTION_COUNT: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommitPhase {
    Staging,  // navigating files, staging/unstaging
    Message,  // typing commit message
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
    StageAll { worktree_idx: usize },
    Commit { worktree_idx: usize, message: String },
    RefreshWorktreeStatus,
    FetchWorktreeStatus { worktree_idx: usize },
    OpenStageCommit { worktree_idx: usize },
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
    /// Commit message input for inline commit in the info panel.
    pub info_panel_commit_msg: Option<String>,
    pub dialog: Option<Dialog>,
    /// Loading overlay message shown during blocking git operations.
    pub loading_message: Option<String>,
    /// Queued blocking action to execute after drawing the loading overlay.
    pub pending_action: Option<PendingAction>,
    pub should_quit: bool,
    pub sidebar_selected: usize,
    pub sidebar_items: Vec<SidebarItem>,
    pub event_tx: mpsc::UnboundedSender<AppEvent>,
    pub status_message: Option<String>,
    pub sidebar_visible: bool,
    /// Whether a valid bare repo was detected at startup.
    pub repo_detected: bool,
    /// Whether tmux is available for session persistence.
    pub tmux_available: bool,
    /// Scrollback offset for the active terminal (0 = live view, >0 = scrolled up).
    pub terminal_scroll: usize,
    /// Merge to retry after a commit completes (set when target worktree was dirty).
    pub pending_merge: Option<PendingAction>,
}

impl App {
    pub fn new(bare_repo_path: PathBuf, event_tx: mpsc::UnboundedSender<AppEvent>, repo_detected: bool, tmux_available: bool) -> Self {
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
            info_panel_commit_msg: None,
            dialog: None,
            loading_message: None,
            pending_action: None,
            should_quit: false,
            sidebar_selected: 0,
            sidebar_items: Vec::new(),
            event_tx,
            status_message: None,
            sidebar_visible: true,
            repo_detected,
            tmux_available,
            terminal_scroll: 0,
            pending_merge: None,
        }
    }

    /// Toggle focus between sidebar and terminal pane.
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
                self.focus = FocusTarget::Sidebar;
                self.input_mode = InputMode::Normal;
                self.info_panel_commit_msg = None;
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
    }

    pub fn selected_sidebar_item(&self) -> Option<SidebarItem> {
        self.sidebar_items.get(self.sidebar_selected).copied()
    }

    pub fn sidebar_up(&mut self) {
        if self.sidebar_selected > 0 {
            self.sidebar_selected -= 1;
        }
    }

    pub fn sidebar_down(&mut self) {
        if self.sidebar_selected + 1 < self.sidebar_items.len() {
            self.sidebar_selected += 1;
        }
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
                    }
                }
            }
            Some(SidebarItem::Worktree(wi)) => {
                if self.worktrees.get(wi).is_some() {
                    // Show worktree info panel
                    self.active_session_id = None;
                    self.active_worktree_idx = Some(wi);
                    self.worktree_status = None;
                    self.info_panel_section = 0;
                    self.info_panel_cursor = 0;
                    self.info_panel_commit_msg = None;
                    self.terminal_scroll = 0;
                    self.queue_action("Loading...", PendingAction::FetchWorktreeStatus { worktree_idx: wi });
                    // Expand if collapsed
                    if let Some(wt) = self.worktrees.get_mut(wi) {
                        if !wt.expanded {
                            wt.expanded = true;
                            self.rebuild_sidebar_items();
                        }
                    }
                }
            }
            None => {}
        }
    }

    /// Refresh the cached worktree status if one is being viewed.
    /// Clamps the info panel cursor to valid bounds.
    pub fn refresh_worktree_status(&mut self) {
        if let Some(wi) = self.active_worktree_idx {
            self.worktree_status = crate::worktree::fetch_worktree_status(self, wi).ok();
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
        };
    }

    pub fn set_status(&mut self, msg: impl Into<String>) {
        self.status_message = Some(msg.into());
    }
}

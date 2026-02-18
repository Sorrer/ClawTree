use std::collections::HashMap;
use std::path::PathBuf;
use tokio::sync::mpsc;

use crate::event::AppEvent;
use crate::session::Session;
use crate::worktree::Worktree;

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
        /// 0 = branch field
        focused_field: usize,
    },
    MergeBranch {
        /// Index of the target worktree (merge INTO this one)
        target_worktree_idx: usize,
        /// Available source branches
        branches: Vec<String>,
        /// Currently selected branch index
        selected: usize,
    },
    Confirm {
        message: String,
        on_confirm: ConfirmAction,
    },
}

#[derive(Debug, Clone)]
pub enum ConfirmAction {
    DeleteSession(u64),
    DeleteWorktree(PathBuf),
    ForceDeleteWorktree(PathBuf),
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
    pub dialog: Option<Dialog>,
    pub should_quit: bool,
    pub sidebar_selected: usize,
    pub sidebar_items: Vec<SidebarItem>,
    pub event_tx: mpsc::UnboundedSender<AppEvent>,
    pub status_message: Option<String>,
    pub sidebar_visible: bool,
}

impl App {
    pub fn new(bare_repo_path: PathBuf, event_tx: mpsc::UnboundedSender<AppEvent>) -> Self {
        Self {
            bare_repo_path,
            worktrees: Vec::new(),
            sessions: HashMap::new(),
            next_session_id: 1,
            focus: FocusTarget::Sidebar,
            input_mode: InputMode::Normal,
            active_session_id: None,
            dialog: None,
            should_quit: false,
            sidebar_selected: 0,
            sidebar_items: Vec::new(),
            event_tx,
            status_message: None,
            sidebar_visible: true,
        }
    }

    /// Toggle focus between sidebar and terminal pane.
    pub fn toggle_focus(&mut self) {
        match self.focus {
            FocusTarget::Sidebar => {
                if self.active_session_id.is_some() {
                    self.focus = FocusTarget::TerminalPane;
                    self.input_mode = InputMode::Terminal;
                }
            }
            FocusTarget::TerminalPane => {
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
                        self.focus = FocusTarget::TerminalPane;
                        self.input_mode = InputMode::Terminal;
                    }
                }
            }
            Some(SidebarItem::Worktree(wi)) => {
                if let Some(wt) = self.worktrees.get(wi) {
                    if wt.expanded && !wt.session_ids.is_empty() {
                        let sid = wt.session_ids[0];
                        self.active_session_id = Some(sid);
                        self.focus = FocusTarget::TerminalPane;
                        self.input_mode = InputMode::Terminal;
                    } else {
                        self.toggle_expand();
                    }
                }
            }
            None => {}
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
            FocusTarget::TerminalPane => InputMode::Terminal,
        };
    }

    pub fn set_status(&mut self, msg: impl Into<String>) {
        self.status_message = Some(msg.into());
    }
}

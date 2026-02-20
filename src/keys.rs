use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::app::{App, CommitPhase, ConfirmAction, Dialog, FocusTarget, InputMode, MiniModeFocus, PendingAction, ScreenMode, SidebarItem, SavedPrompt};
use crate::session;
use crate::ui::terminal_pane;
use crate::worktree;

// ── Keybinding registry ─────────────────────────────────────────────

/// A single keybinding entry for help display: (key_display, description).
pub type KeyEntry = (&'static str, &'static str);

/// Context categories mapping to help overlay tabs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyContext {
    Global,
    Sidebar,
    Terminal,
    Queue,
    InfoPanel,
    MiniMode,
}

pub const KEY_CONTEXT_COUNT: usize = 6;

impl KeyContext {
    pub const ALL: [KeyContext; KEY_CONTEXT_COUNT] = [
        KeyContext::Global,
        KeyContext::Sidebar,
        KeyContext::Terminal,
        KeyContext::Queue,
        KeyContext::InfoPanel,
        KeyContext::MiniMode,
    ];

    pub const fn label(&self) -> &'static str {
        match self {
            KeyContext::Global => "Global",
            KeyContext::Sidebar => "Sidebar",
            KeyContext::Terminal => "Terminal",
            KeyContext::Queue => "Queue",
            KeyContext::InfoPanel => "Info Panel",
            KeyContext::MiniMode => "Mini Mode",
        }
    }

    pub const fn keys(&self) -> &'static [KeyEntry] {
        match self {
            KeyContext::Global => GLOBAL_KEYS,
            KeyContext::Sidebar => SIDEBAR_KEYS,
            KeyContext::Terminal => TERMINAL_KEYS,
            KeyContext::Queue => QUEUE_KEYS,
            KeyContext::InfoPanel => INFO_PANEL_KEYS,
            KeyContext::MiniMode => MINI_MODE_KEYS,
        }
    }

    /// Returns additional platform-specific keys for this context.
    pub fn extra_keys(&self, wt_available: bool) -> &'static [KeyEntry] {
        match self {
            KeyContext::Sidebar if wt_available => SIDEBAR_KEYS_WT,
            _ => &[],
        }
    }
}

const GLOBAL_KEYS: &[KeyEntry] = &[
    ("Ctrl+Q",  "Quit application"),
    ("Ctrl+B",  "Toggle sidebar"),
    ("Ctrl+P",  "Toggle prompt queue"),
    ("F2",      "Toggle Mini Mode"),
    ("?",       "Show/hide this help"),
    ("Click",   "Enable text selection (any key restores scroll)"),
];

const SIDEBAR_KEYS: &[KeyEntry] = &[
    ("Tab",         "Focus terminal / info panel"),
    ("j / Down",    "Navigate down"),
    ("k / Up",      "Navigate up"),
    ("Enter",       "Activate selected item"),
    ("Space",       "Toggle expand/collapse"),
    ("c",           "New Claude session"),
    ("C",           "Claude (--dangerously-skip-permissions)"),
    ("t",           "New terminal session"),
    ("n",           "New worktree"),
    ("d",           "Delete session/worktree"),
    ("D",           "Force-delete worktree"),
    ("m",           "Merge branch"),
    ("s",           "Stage & commit"),
    ("p",           "Push branch to remote"),
    ("r",           "Rename/nickname session"),
    ("G",           "Jump to bottom"),
    ("Home / End",  "Jump to top / bottom"),
    ("z / Z",       "Collapse / expand all worktrees"),
    ("F5 / ^R",     "Refresh worktrees"),
    ("PgUp/PgDn",   "Scroll terminal"),
];

/// Additional sidebar keys shown only when wt.exe is available.
const SIDEBAR_KEYS_WT: &[KeyEntry] = &[
    ("w",           "Open Windows Terminal tab"),
    ("W",           "Windows Terminal + Claude"),
];

const TERMINAL_KEYS: &[KeyEntry] = &[
    ("Tab",         "Back to sidebar / prompt queue"),
    ("PgUp/PgDn",   "Scroll through history"),
    ("(all keys)",  "Sent directly to Claude session"),
];

const QUEUE_KEYS: &[KeyEntry] = &[
    ("Tab",         "Back to sidebar"),
    ("Esc",         "Cancel edit / back to sidebar"),
    ("Enter",       "Add item / save edit / load for editing"),
    ("Up / Down",   "Navigate queue items"),
    ("d / Delete",  "Delete selected item"),
    ("(type)",      "Input text for new/editing prompt"),
    ("Backspace",   "Delete character"),
];

const MINI_MODE_KEYS: &[KeyEntry] = &[
    ("j / Down",    "Navigate tree"),
    ("k / Up",      "Navigate tree"),
    ("Tab/Enter",   "Focus detail input (on agent)"),
    ("Enter",       "Toggle expand (on worktree)"),
    ("Space",       "Toggle expand/collapse worktree"),
    ("o",           "Open full terminal (drilldown)"),
    ("a",           "Create new agent"),
    ("d",           "Kill agent / remove worktree"),
    ("r",           "Rename agent"),
    ("s",           "Browse saved prompts"),
    ("z / Z",       "Collapse / expand all"),
    ("Esc",         "Return to normal mode"),
    ("(detail)",    "Type + Enter: send to agent"),
];

const INFO_PANEL_KEYS: &[KeyEntry] = &[
    ("j / Down",    "Navigate files"),
    ("k / Up",      "Navigate files"),
    ("Tab",         "Switch unstaged/staged section"),
    ("Esc",         "Back to sidebar"),
    ("Space/Enter", "Stage/unstage selected file"),
    ("a",           "Stage all files"),
    ("c",           "Enter commit message mode"),
    ("C",           "New Claude session"),
    ("n",           "New worktree"),
    ("d",           "Delete worktree"),
    ("m",           "Merge branch"),
    ("p",           "Push branch"),
    ("F5 / ^R",     "Refresh"),
];

/// Handle a key event based on current input mode.
pub fn handle_key(app: &mut App, key: KeyEvent, terminal_size: (u16, u16)) {
    // ── Help overlay intercepts all keys when visible ──────────────
    if app.show_help {
        handle_help_key(app, key);
        return;
    }

    // ── Global keybindings (work in ALL modes) ─────────────────────
    if key.modifiers == KeyModifiers::CONTROL && key.code == KeyCode::Char('q') {
        app.should_quit = true;
        return;
    }

    // F2 — toggle between Normal and Mini mode
    if key.modifiers.is_empty() && key.code == KeyCode::F(2) {
        match app.screen_mode {
            ScreenMode::Normal => {
                app.screen_mode = ScreenMode::Mini;
                app.mini.focus = MiniModeFocus::AgentList;
                app.input_mode = InputMode::Normal;
                app.rebuild_mini_agent_list();
            }
            ScreenMode::Mini | ScreenMode::MiniDrilldown => {
                app.screen_mode = ScreenMode::Normal;
                app.input_mode = match app.focus {
                    FocusTarget::TerminalPane if app.active_session_id.is_some() => InputMode::Terminal,
                    _ => InputMode::Normal,
                };
            }
        }
        session::resize_all(app, terminal_size.1, terminal_size.0);
        return;
    }

    // ── Mini mode key handling ─────────────────────────────────────
    if app.screen_mode == ScreenMode::Mini {
        if app.input_mode == InputMode::Dialog {
            handle_dialog_key(app, key, terminal_size);
        } else {
            handle_mini_mode_key(app, key, terminal_size);
        }
        return;
    }
    if app.screen_mode == ScreenMode::MiniDrilldown {
        handle_mini_drilldown_key(app, key, terminal_size);
        return;
    }

    if key.modifiers == KeyModifiers::CONTROL && key.code == KeyCode::Char('b') {
        app.sidebar_visible = !app.sidebar_visible;
        session::resize_all(app, terminal_size.1, terminal_size.0);
        return;
    }
    if key.modifiers == KeyModifiers::CONTROL && key.code == KeyCode::Char('p') {
        if app.active_session_id.is_some() {
            app.prompt_queue_visible = !app.prompt_queue_visible;
            session::resize_all(app, terminal_size.1, terminal_size.0);
            if app.prompt_queue_visible {
                // Focus the queue panel
                app.focus = FocusTarget::PromptQueue;
                app.input_mode = InputMode::Normal;
            } else if app.focus == FocusTarget::PromptQueue {
                // Was focused on queue, go back to terminal
                app.focus = FocusTarget::TerminalPane;
                app.input_mode = InputMode::Terminal;
            }
        }
        return;
    }

    // ? — toggle help overlay (Normal mode only, not in dialogs/terminal)
    if key.modifiers.is_empty() && key.code == KeyCode::Char('?') && app.input_mode == InputMode::Normal {
        // Don't open help if we're typing in prompt queue or info panel commit message
        if !app.prompt_queue_focused() && app.info_panel_commit_msg.is_none() {
            app.show_help = true;
            app.help_tab = 0;
            return;
        }
    }

    match app.input_mode {
        InputMode::Normal => handle_normal_key(app, key, terminal_size),
        InputMode::Terminal => handle_terminal_key(app, key),
        InputMode::Dialog => handle_dialog_key(app, key, terminal_size),
    }
}

/// Handle keys while the help overlay is open.
fn handle_help_key(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Esc | KeyCode::Char('?') => {
            app.show_help = false;
        }
        KeyCode::Left | KeyCode::Char('h') => {
            if app.help_tab > 0 {
                app.help_tab -= 1;
                app.help_scroll = 0; // reset scroll on tab change
            }
        }
        KeyCode::Right | KeyCode::Char('l') => {
            if app.help_tab + 1 < KEY_CONTEXT_COUNT {
                app.help_tab += 1;
                app.help_scroll = 0; // reset scroll on tab change
            }
        }
        KeyCode::Char('1') => { app.help_tab = 0; app.help_scroll = 0; }
        KeyCode::Char('2') => { app.help_tab = 1; app.help_scroll = 0; }
        KeyCode::Char('3') => { app.help_tab = 2; app.help_scroll = 0; }
        KeyCode::Char('4') => { app.help_tab = 3; app.help_scroll = 0; }
        KeyCode::Char('5') => { app.help_tab = 4; app.help_scroll = 0; }
        KeyCode::Char('6') => { app.help_tab = 5; app.help_scroll = 0; }
        // Scroll within key list
        KeyCode::Down | KeyCode::Char('j') => {
            let ctx = KeyContext::ALL[app.help_tab];
            let total = ctx.keys().len() + ctx.extra_keys(app.wt_available).len();
            if app.help_scroll + 1 < total {
                app.help_scroll += 1;
            }
        }
        KeyCode::Up | KeyCode::Char('k') => {
            if app.help_scroll > 0 {
                app.help_scroll -= 1;
            }
        }
        _ => {}
    }
}

fn handle_normal_key(app: &mut App, key: KeyEvent, terminal_size: (u16, u16)) {
    // Prompt queue focused — handle queue keys
    if app.prompt_queue_focused() {
        handle_prompt_queue_key(app, key, terminal_size);
        return;
    }

    // Info panel focused — handle file navigation/staging keys
    if app.info_panel_focused() {
        handle_info_panel_key(app, key, terminal_size);
        return;
    }

    // If no repo detected, only allow init or convert
    if !app.repo_detected {
        match key.code {
            KeyCode::Char('i') => {
                app.open_dialog(Dialog::InitRepo {
                    url_input: String::new(),
                    branch_input: "main".to_string(),
                    focused_field: 0,
                });
            }
            KeyCode::Char('c') => {
                if let Some(ref repo_path) = app.regular_repo_path {
                    let branch = worktree::git::current_branch_name(repo_path)
                        .unwrap_or_else(|_| "main".to_string());
                    let source = repo_path.clone();
                    app.open_dialog(Dialog::ConvertRepo {
                        mode: 0,
                        target_path_input: String::new(),
                        branch_name: branch,
                        focused_field: 0,
                        source_repo_path: source,
                    });
                }
            }
            _ => {}
        }
        return;
    }

    match (key.modifiers, key.code) {
        // Tab — toggle focus to terminal / info panel
        (_, KeyCode::Tab) => {
            app.toggle_focus();
        }

        (_, KeyCode::F(5)) | (KeyModifiers::CONTROL, KeyCode::Char('r')) => {
            if let Err(e) = worktree::refresh_worktrees(app) {
                app.set_status(format!("Error refreshing: {}", e));
            }
            app.queue_action("Refreshing...", PendingAction::RefreshWorktreeStatus);
        }

        // Terminal scrollback (works from sidebar too)
        (_, KeyCode::PageUp) => {
            app.terminal_scroll = app.terminal_scroll.saturating_add(SCROLL_PAGE);
        }
        (_, KeyCode::PageDown) => {
            app.terminal_scroll = app.terminal_scroll.saturating_sub(SCROLL_PAGE);
        }

        // Sidebar navigation
        (_, KeyCode::Char('j')) | (_, KeyCode::Down) => app.sidebar_down(),
        (_, KeyCode::Char('k')) | (_, KeyCode::Up) => app.sidebar_up(),
        (_, KeyCode::Home) => app.sidebar_jump_top(),
        (_, KeyCode::End) | (KeyModifiers::SHIFT, KeyCode::Char('G')) => app.sidebar_jump_bottom(),
        (_, KeyCode::Enter) => app.activate_selected(),
        (_, KeyCode::Char(' ')) => app.toggle_expand(),
        // z — collapse all worktrees, Z — expand all
        (_, KeyCode::Char('z')) => app.collapse_all(),
        (KeyModifiers::SHIFT, KeyCode::Char('Z')) => app.expand_all(),

        // c — new Claude session
        (_, KeyCode::Char('c')) => {
            spawn_claude_for_selected(app, terminal_size, false);
        }
        // C — new Claude session with --dangerously-skip-permissions
        (KeyModifiers::SHIFT, KeyCode::Char('C')) => {
            spawn_claude_for_selected(app, terminal_size, true);
        }

        // t — new terminal session
        (_, KeyCode::Char('t')) => {
            spawn_terminal_for_selected(app, terminal_size);
        }

        // n — new worktree
        (_, KeyCode::Char('n')) => {
            let base = selected_worktree_branch(app);
            app.open_dialog(Dialog::CreateWorktree {
                branch_input: String::new(),
                base_branch: base,
                focused_field: 0,
            });
        }

        // d — delete selected (session or worktree)
        (_, KeyCode::Char('d')) => {
            handle_delete(app);
        }
        // D — force-delete worktree
        (KeyModifiers::SHIFT, KeyCode::Char('D')) => {
            handle_force_delete(app);
        }

        // m — merge branch into selected worktree
        (_, KeyCode::Char('m')) => {
            handle_merge(app);
        }

        // s — stage/commit (open GitCommit dialog for selected worktree)
        (_, KeyCode::Char('s')) => {
            handle_stage_commit(app);
        }

        // p — push branch to remote
        (_, KeyCode::Char('p')) => {
            handle_push(app);
        }

        // r — rename/nickname a session
        (_, KeyCode::Char('r')) => {
            if let Some(SidebarItem::Session(wi, si)) = app.selected_sidebar_item() {
                if let Some(wt) = app.worktrees.get(wi) {
                    if let Some(&sid) = wt.session_ids.get(si) {
                        let current = app.sessions.get(&sid)
                            .and_then(|s| s.nickname.clone())
                            .unwrap_or_default();
                        app.open_dialog(Dialog::RenameSession {
                            session_id: sid,
                            input: current,
                        });
                    }
                }
            }
        }

        // w — open new Windows Terminal tab in worktree directory (WSL only)
        (_, KeyCode::Char('w')) if app.wt_available => {
            open_wsl_window(app, false);
        }
        // W — open new Windows Terminal tab with claude in worktree directory (WSL only)
        (KeyModifiers::SHIFT, KeyCode::Char('W')) if app.wt_available => {
            open_wsl_window(app, true);
        }

        _ => {}
    }
}

fn handle_info_panel_key(app: &mut App, key: KeyEvent, terminal_size: (u16, u16)) {
    // If typing a commit message, handle text input
    if app.info_panel_commit_msg.is_some() {
        match key.code {
            KeyCode::Esc => {
                app.info_panel_commit_msg = None;
            }
            KeyCode::Enter => {
                let msg = app.info_panel_commit_msg.take().unwrap_or_default();
                if msg.is_empty() {
                    app.set_status("Commit message cannot be empty");
                    app.info_panel_commit_msg = Some(msg);
                    return;
                }
                if let Some(wi) = app.active_worktree_idx {
                    app.queue_action("Committing...", PendingAction::Commit {
                        worktree_idx: wi,
                        message: msg,
                    });
                }
            }
            KeyCode::Char(c) => {
                if let Some(ref mut msg) = app.info_panel_commit_msg {
                    msg.push(c);
                }
            }
            KeyCode::Backspace => {
                if let Some(ref mut msg) = app.info_panel_commit_msg {
                    msg.pop();
                }
            }
            _ => {}
        }
        return;
    }

    let (unstaged, staged) = app.info_panel_file_lists();

    match key.code {
        // Navigation
        KeyCode::Char('j') | KeyCode::Down => {
            let len = if app.info_panel_section == 0 { unstaged.len() } else { staged.len() };
            if len > 0 && app.info_panel_cursor + 1 < len {
                app.info_panel_cursor += 1;
            }
        }
        KeyCode::Char('k') | KeyCode::Up => {
            if app.info_panel_cursor > 0 {
                app.info_panel_cursor -= 1;
            }
        }
        // Switch between unstaged/staged sections
        KeyCode::Tab => {
            if app.info_panel_section == 0 && !staged.is_empty() {
                app.info_panel_section = 1;
                app.info_panel_cursor = app.info_panel_cursor.min(staged.len().saturating_sub(1));
            } else if app.info_panel_section == 1 && !unstaged.is_empty() {
                app.info_panel_section = 0;
                app.info_panel_cursor = app.info_panel_cursor.min(unstaged.len().saturating_sub(1));
            } else {
                // No other section to switch to — go back to sidebar
                app.escape_to_sidebar();
            }
        }
        KeyCode::Esc => {
            app.escape_to_sidebar();
        }
        // Space or Enter — stage/unstage selected file
        KeyCode::Char(' ') | KeyCode::Enter => {
            if let Some(wi) = app.active_worktree_idx {
                if app.info_panel_section == 0 {
                    if let Some((_, path)) = unstaged.get(app.info_panel_cursor) {
                        app.queue_action("Staging...", PendingAction::StageFile {
                            worktree_idx: wi,
                            file: path.clone(),
                        });
                    }
                } else {
                    if let Some((_, path)) = staged.get(app.info_panel_cursor) {
                        app.queue_action("Unstaging...", PendingAction::UnstageFile {
                            worktree_idx: wi,
                            file: path.clone(),
                        });
                    }
                }
            }
        }
        // a — stage all
        KeyCode::Char('a') => {
            if let Some(wi) = app.active_worktree_idx {
                app.queue_action("Staging all...", PendingAction::StageAll { worktree_idx: wi });
            }
        }
        // c — enter commit message mode (matching GitCommit dialog)
        KeyCode::Char('c') => {
            if staged.is_empty() {
                app.set_status("No staged files to commit");
            } else {
                app.info_panel_commit_msg = Some(String::new());
            }
        }
        KeyCode::Char('C') if key.modifiers == KeyModifiers::SHIFT => {
            spawn_claude_for_selected(app, terminal_size, true);
        }
        KeyCode::Char('n') => {
            let base = selected_worktree_branch(app);
            app.open_dialog(Dialog::CreateWorktree {
                branch_input: String::new(),
                base_branch: base,
                focused_field: 0,
            });
        }
        KeyCode::Char('d') => {
            handle_delete(app);
        }
        KeyCode::Char('m') => {
            handle_merge(app);
        }
        KeyCode::Char('p') => {
            handle_push(app);
        }
        KeyCode::F(5) => {
            if let Err(e) = worktree::refresh_worktrees(app) {
                app.set_status(format!("Error refreshing: {}", e));
            }
            app.queue_action("Refreshing...", PendingAction::RefreshWorktreeStatus);
        }
        KeyCode::Char('r') if key.modifiers == KeyModifiers::CONTROL => {
            if let Err(e) = worktree::refresh_worktrees(app) {
                app.set_status(format!("Error refreshing: {}", e));
            }
            app.queue_action("Refreshing...", PendingAction::RefreshWorktreeStatus);
        }
        _ => {}
    }
}

/// Get the branch name of the currently selected worktree (or "main" as default).
fn selected_worktree_branch(app: &App) -> String {
    let wi = match app.selected_sidebar_item() {
        Some(SidebarItem::Worktree(wi)) => Some(wi),
        Some(SidebarItem::Session(wi, _)) => Some(wi),
        None => None,
    };
    wi.and_then(|i| app.worktrees.get(i))
        .map(|wt| wt.branch.clone())
        .unwrap_or_else(|| "main".to_string())
}

fn open_wsl_window(app: &mut App, with_claude: bool) {
    let wt_idx = match app.selected_sidebar_item() {
        Some(SidebarItem::Worktree(wi)) => Some(wi),
        Some(SidebarItem::Session(wi, _)) => Some(wi),
        None => None,
    };
    if let Some(wi) = wt_idx {
        if let Some(wt) = app.worktrees.get(wi) {
            let path = wt.path.clone();
            let branch = wt.branch.clone();
            std::thread::Builder::new()
                .name("wsl-window".into())
                .spawn(move || {
                    // Convert Linux path to Windows path
                    let win_path = match std::process::Command::new("wslpath")
                        .args(["-w", &path.to_string_lossy()])
                        .output()
                    {
                        Ok(o) if o.status.success() => {
                            String::from_utf8_lossy(&o.stdout).trim().to_string()
                        }
                        _ => return,
                    };

                    if with_claude {
                        // Write a temp rcfile that sources bashrc then runs clauded
                        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
                        let init_file = std::path::PathBuf::from(home).join(".clauded_init");
                        let _ = std::fs::write(
                            &init_file,
                            "source ~/.bashrc\nclauded\n",
                        );
                        let _ = std::process::Command::new("wt.exe")
                            .args([
                                "-w", "0", "nt", "--title", &branch,
                                "--", "wsl.exe", "--cd", &win_path,
                                "bash", "--rcfile", &init_file.to_string_lossy(),
                            ])
                            .spawn();
                    } else {
                        let _ = std::process::Command::new("wt.exe")
                            .args([
                                "-w", "0", "nt", "--title", &branch,
                                "--", "wsl.exe", "--cd", &win_path,
                            ])
                            .spawn();
                    }
                })
                .ok();

            let action = if with_claude { "claude tab" } else { "terminal tab" };
            app.set_status(format!("Opening {} for '{}'", action, app.worktrees[wi].branch));
        }
    }
}

fn spawn_claude_for_selected(app: &mut App, terminal_size: (u16, u16), skip_permissions: bool) {
    let wt_idx = match app.selected_sidebar_item() {
        Some(SidebarItem::Worktree(wi)) => Some(wi),
        Some(SidebarItem::Session(wi, _)) => Some(wi),
        None => None,
    };
    if let Some(wi) = wt_idx {
        match session::spawn_session(app, wi, terminal_size, skip_permissions, None) {
            Ok(sid) => {
                app.active_session_id = Some(sid);
                app.focus = FocusTarget::TerminalPane;
                app.input_mode = InputMode::Terminal;
                app.rebuild_sidebar_items();
            }
            Err(e) => {
                app.set_status(format!("Failed to spawn session: {}", e));
            }
        }
    }
}

fn spawn_terminal_for_selected(app: &mut App, terminal_size: (u16, u16)) {
    let wt_idx = match app.selected_sidebar_item() {
        Some(SidebarItem::Worktree(wi)) => Some(wi),
        Some(SidebarItem::Session(wi, _)) => Some(wi),
        None => None,
    };
    if let Some(wi) = wt_idx {
        match session::spawn_terminal_session(app, wi, terminal_size) {
            Ok(sid) => {
                app.active_session_id = Some(sid);
                app.focus = FocusTarget::TerminalPane;
                app.input_mode = InputMode::Terminal;
                app.rebuild_sidebar_items();
            }
            Err(e) => {
                app.set_status(format!("Failed to spawn terminal: {}", e));
            }
        }
    }
}

fn handle_delete(app: &mut App) {
    match app.selected_sidebar_item() {
        Some(SidebarItem::Session(wi, si)) => {
            if let Some(wt) = app.worktrees.get(wi) {
                if let Some(&sid) = wt.session_ids.get(si) {
                    app.open_dialog(Dialog::Confirm {
                        message: format!("Kill session {}?", session::session_label(app, sid)),
                        on_confirm: ConfirmAction::DeleteSession(sid),
                    });
                }
            }
        }
        Some(SidebarItem::Worktree(wi)) => {
            if let Some(wt) = app.worktrees.get(wi) {
                let path = wt.path.clone();
                let has_sessions = !wt.session_ids.is_empty();
                let msg = if has_sessions {
                    format!(
                        "DELETE worktree '{}' and kill {} session(s)",
                        wt.branch,
                        wt.session_ids.len()
                    )
                } else {
                    format!("DELETE worktree '{}'", wt.branch)
                };
                app.open_dialog(Dialog::ConfirmDangerous {
                    message: msg,
                    input: String::new(),
                    on_confirm: ConfirmAction::DeleteWorktree(path),
                });
            }
        }
        None => {}
    }
}

fn handle_force_delete(app: &mut App) {
    if let Some(SidebarItem::Worktree(wi)) | Some(SidebarItem::Session(wi, _)) =
        app.selected_sidebar_item()
    {
        if let Some(wt) = app.worktrees.get(wi) {
            let path = wt.path.clone();
            app.open_dialog(Dialog::ConfirmDangerous {
                message: format!(
                    "FORCE DELETE worktree '{}' (even if dirty)",
                    wt.branch
                ),
                input: String::new(),
                on_confirm: ConfirmAction::ForceDeleteWorktree(path),
            });
        }
    }
}

fn handle_stage_commit(app: &mut App) {
    let wt_idx = match app.selected_sidebar_item() {
        Some(SidebarItem::Worktree(wi)) => Some(wi),
        Some(SidebarItem::Session(wi, _)) => Some(wi),
        None => None,
    };
    if let Some(wi) = wt_idx {
        app.queue_action("Loading status...", PendingAction::OpenStageCommit { worktree_idx: wi });
    }
}

fn handle_push(app: &mut App) {
    let wt_idx = match app.selected_sidebar_item() {
        Some(SidebarItem::Worktree(wi)) => Some(wi),
        Some(SidebarItem::Session(wi, _)) => Some(wi),
        None => None,
    };
    if let Some(wi) = wt_idx {
        if let Some(wt) = app.worktrees.get(wi) {
            let path = wt.path.clone();
            let branch = wt.branch.clone();
            let tx = app.event_tx.clone();
            app.set_status(format!("Pushing '{}'...", branch));

            std::thread::Builder::new()
                .name("git-push".into())
                .spawn(move || {
                    let result = worktree::git::push_branch(&path, &branch);
                    let error = result.err().map(|e| format!("{}", e));
                    let _ = tx.send(crate::event::AppEvent::PushComplete { branch, error });
                })
                .ok();
        }
    }
}

fn handle_merge(app: &mut App) {
    let wt_idx = match app.selected_sidebar_item() {
        Some(SidebarItem::Worktree(wi)) => Some(wi),
        Some(SidebarItem::Session(wi, _)) => Some(wi),
        None => None,
    };
    if let Some(wi) = wt_idx {
        match worktree::available_branches(app) {
            Ok(branches) => {
                if branches.is_empty() {
                    app.set_status("No branches found");
                    return;
                }
                // Filter out the source worktree's own branch
                let source_branch = app.worktrees.get(wi).map(|w| w.branch.as_str());
                let filtered: Vec<String> = branches
                    .into_iter()
                    .filter(|b| source_branch.map_or(true, |sb| b != sb))
                    .collect();
                if filtered.is_empty() {
                    app.set_status("No other branches to merge into");
                    return;
                }
                // Default selection to "main" if available
                let default_idx = filtered.iter().position(|b| b == "main").unwrap_or(0);
                app.open_dialog(Dialog::MergeBranch {
                    source_worktree_idx: wi,
                    branches: filtered,
                    selected: default_idx,
                });
            }
            Err(e) => {
                app.set_status(format!("Failed to list branches: {}", e));
            }
        }
    }
}

fn handle_prompt_queue_key(app: &mut App, key: KeyEvent, terminal_size: (u16, u16)) {
    let sid = match app.active_session_id {
        Some(sid) => sid,
        None => return,
    };

    match key.code {
        // Tab — toggle focus back to sidebar
        KeyCode::Tab => {
            app.toggle_focus();
        }
        KeyCode::Esc => {
            if app.prompt_queue_editing.is_some() {
                // Cancel editing
                app.prompt_queue_editing = None;
                app.prompt_queue_input.clear();
                app.prompt_queue_cursor = 0;
            } else {
                // Go back to sidebar
                app.focus = FocusTarget::Sidebar;
                app.input_mode = InputMode::Normal;
            }
        }
        KeyCode::Enter => {
            if let Some(edit_idx) = app.prompt_queue_editing.take() {
                // Save edited item back to queue
                if !app.prompt_queue_input.is_empty() {
                    let input = app.prompt_queue_input.drain(..).collect::<String>();
                    if let Some(queue) = app.prompt_queues.get_mut(&sid) {
                        if edit_idx < queue.len() {
                            queue[edit_idx] = input;
                        }
                    }
                    app.save_prompt_queues();
                }
                app.prompt_queue_cursor = 0;
            } else if !app.prompt_queue_input.is_empty() {
                // Add new item to queue
                let input = app.prompt_queue_input.drain(..).collect::<String>();
                app.prompt_queues.entry(sid).or_default().push(input);
                app.save_prompt_queues();
                app.prompt_queue_cursor = 0;
            } else {
                // Input empty + item selected → load for editing
                let queue_len = app.active_prompt_queue().len();
                if queue_len > 0 && app.prompt_queue_selected < queue_len {
                    let text = app.active_prompt_queue()[app.prompt_queue_selected].clone();
                    app.prompt_queue_cursor = text.len();
                    app.prompt_queue_input = text;
                    app.prompt_queue_editing = Some(app.prompt_queue_selected);
                }
            }
        }
        KeyCode::Left => {
            if app.prompt_queue_cursor > 0 {
                // Walk back to previous char boundary
                let mut pos = app.prompt_queue_cursor - 1;
                while pos > 0 && !app.prompt_queue_input.is_char_boundary(pos) {
                    pos -= 1;
                }
                app.prompt_queue_cursor = pos;
            }
        }
        KeyCode::Right => {
            if app.prompt_queue_cursor < app.prompt_queue_input.len() {
                // Walk forward to next char boundary
                let mut pos = app.prompt_queue_cursor + 1;
                while pos < app.prompt_queue_input.len() && !app.prompt_queue_input.is_char_boundary(pos) {
                    pos += 1;
                }
                app.prompt_queue_cursor = pos;
            }
        }
        KeyCode::Home => {
            app.prompt_queue_cursor = 0;
        }
        KeyCode::End => {
            app.prompt_queue_cursor = app.prompt_queue_input.len();
        }
        KeyCode::Up => {
            if app.prompt_queue_input.is_empty() && app.prompt_queue_editing.is_none() {
                if app.prompt_queue_selected > 0 {
                    app.prompt_queue_selected -= 1;
                }
            }
        }
        KeyCode::Down => {
            if app.prompt_queue_input.is_empty() && app.prompt_queue_editing.is_none() {
                let queue_len = app.active_prompt_queue().len();
                if queue_len > 0 && app.prompt_queue_selected + 1 < queue_len {
                    app.prompt_queue_selected += 1;
                }
            }
        }
        KeyCode::Char('d') if app.prompt_queue_input.is_empty() && app.prompt_queue_editing.is_none() => {
            let queue_len = app.active_prompt_queue().len();
            if queue_len > 0 && app.prompt_queue_selected < queue_len {
                if let Some(queue) = app.prompt_queues.get_mut(&sid) {
                    queue.remove(app.prompt_queue_selected);
                    if app.prompt_queue_selected >= queue.len() && app.prompt_queue_selected > 0 {
                        app.prompt_queue_selected -= 1;
                    }
                }
                app.save_prompt_queues();
            }
        }
        KeyCode::Delete if app.prompt_queue_input.is_empty() && app.prompt_queue_editing.is_none() => {
            let queue_len = app.active_prompt_queue().len();
            if queue_len > 0 && app.prompt_queue_selected < queue_len {
                if let Some(queue) = app.prompt_queues.get_mut(&sid) {
                    queue.remove(app.prompt_queue_selected);
                    if app.prompt_queue_selected >= queue.len() && app.prompt_queue_selected > 0 {
                        app.prompt_queue_selected -= 1;
                    }
                }
                app.save_prompt_queues();
            }
        }
        KeyCode::Backspace => {
            if app.prompt_queue_cursor > 0 {
                // Find the previous char boundary
                let mut prev = app.prompt_queue_cursor - 1;
                while prev > 0 && !app.prompt_queue_input.is_char_boundary(prev) {
                    prev -= 1;
                }
                app.prompt_queue_input.drain(prev..app.prompt_queue_cursor);
                app.prompt_queue_cursor = prev;
            }
        }
        KeyCode::Char(c) if c != '\n' && c != '\r' => {
            app.prompt_queue_input.insert(app.prompt_queue_cursor, c);
            app.prompt_queue_cursor += c.len_utf8();
        }
        _ => {}
    }

    // Suppress unused warning for terminal_size — we accept it for consistency with other handlers
    let _ = terminal_size;
}

fn handle_terminal_key(app: &mut App, key: KeyEvent) {
    // Tab — toggle focus back to sidebar (or prompt queue if visible)
    if key.code == KeyCode::Tab {
        app.terminal_scroll = 0;
        app.toggle_focus();
        return;
    }

    // PgUp / PgDown scroll through history
    match key.code {
        KeyCode::PageUp => {
            app.terminal_scroll = app.terminal_scroll.saturating_add(SCROLL_PAGE);
            clamp_terminal_scroll(app);
            return;
        }
        KeyCode::PageDown => {
            app.terminal_scroll = app.terminal_scroll.saturating_sub(SCROLL_PAGE);
            return;
        }
        _ => {}
    }

    // Any other key snaps back to live view and sends to PTY
    app.terminal_scroll = 0;

    if let Some(sid) = app.active_session_id {
        if let Some(session) = app.sessions.get(&sid) {
            let bytes = key_to_bytes(key, session.application_cursor_mode());
            if !bytes.is_empty() {
                let _ = session.write_tx.send(bytes::Bytes::from(bytes));
            }
        }
    }
}

/// Handle a bracketed paste event. Sends the entire pasted text to the PTY
/// in one write (instead of character-by-character), or inserts into the
/// active dialog/queue input field.
pub fn handle_paste(app: &mut App, data: String) {
    match app.input_mode {
        InputMode::Terminal => {
            app.terminal_scroll = 0;
            if let Some(sid) = app.active_session_id {
                if let Some(session) = app.sessions.get(&sid) {
                    let _ = session.write_tx.send(bytes::Bytes::from(data));
                }
            }
        }
        InputMode::Dialog => {
            // Insert pasted text into whichever dialog input field is active
            match app.dialog {
                Some(Dialog::CreateWorktree { ref mut branch_input, ref mut base_branch, focused_field, .. }) => {
                    let field = if focused_field == 0 { branch_input } else { base_branch };
                    field.push_str(&data);
                }
                Some(Dialog::InitRepo { ref mut url_input, ref mut branch_input, focused_field, .. }) => {
                    let field = if focused_field == 0 { url_input } else { branch_input };
                    field.push_str(&data);
                }
                Some(Dialog::RenameSession { ref mut input, .. }) => {
                    input.push_str(&data);
                }
                Some(Dialog::GitCommit { ref mut commit_message, phase, .. }) if phase == CommitPhase::Message => {
                    commit_message.push_str(&data);
                }
                _ => {}
            }
        }
        InputMode::Normal => {
            // Mini mode detail input
            if app.screen_mode == ScreenMode::Mini && app.mini.focus == MiniModeFocus::DetailInput {
                app.mini.detail_input.push_str(&data);
            } else if app.screen_mode == ScreenMode::Mini && app.mini.focus == MiniModeFocus::PromptInput {
                app.mini.prompt_input.push_str(&data);
            } else if app.focus == FocusTarget::PromptQueue {
                // Prompt queue input
                app.prompt_queue_input.push_str(&data);
            }
        }
    }
}

const SCROLL_LINES: usize = 3;
const SCROLL_PAGE: usize = 20;

/// Handle mouse wheel scroll events. Works regardless of focus.
pub fn handle_scroll(app: &mut App, up: bool) {
    if app.active_session_id.is_none() {
        return;
    }
    if up {
        app.terminal_scroll = app.terminal_scroll.saturating_add(SCROLL_LINES);
        clamp_terminal_scroll(app);
    } else {
        app.terminal_scroll = app.terminal_scroll.saturating_sub(SCROLL_LINES);
    }
}

/// Clamp terminal_scroll to the actual tmux history size so it can't
/// overshoot past the top, which would cause a "lag" when scrolling back down.
fn clamp_terminal_scroll(app: &mut App) {
    if let Some(sid) = app.active_session_id {
        if let Some(session) = app.sessions.get(&sid) {
            if let Some(ref tmux_name) = session.tmux_session_name {
                let history = terminal_pane::tmux_history_size(tmux_name);
                if app.terminal_scroll > history {
                    app.terminal_scroll = history;
                }
            }
        }
    }
}

fn handle_dialog_key(app: &mut App, key: KeyEvent, terminal_size: (u16, u16)) {
    // GitCommit-specific keys need to be handled before the general match
    // because Space, 'a', 'c' have special meaning in staging phase
    if let Some(Dialog::GitCommit { ref phase, .. }) = app.dialog {
        if *phase == CommitPhase::Staging {
            match key.code {
                KeyCode::Char(' ') => {
                    handle_git_commit_space(app);
                    return;
                }
                KeyCode::Char('a') => {
                    handle_git_commit_stage_all(app);
                    return;
                }
                KeyCode::Char('c') => {
                    handle_git_commit_enter_message(app);
                    return;
                }
                _ => {} // fall through to general handler
            }
        }
    }

    match key.code {
        KeyCode::Esc => {
            // GitCommit message phase: go back to staging
            if let Some(Dialog::GitCommit { ref mut phase, .. }) = app.dialog {
                if *phase == CommitPhase::Message {
                    *phase = CommitPhase::Staging;
                    return;
                }
            }
            // Clear any pending merge if cancelling a commit or dirty worktree dialog
            if matches!(app.dialog, Some(Dialog::GitCommit { .. }) | Some(Dialog::DirtyWorktree { .. })) {
                app.pending_merge = None;
            }
            app.close_dialog();
        }
        KeyCode::Enter => {
            let dialog = app.dialog.take();
            match dialog {
                Some(Dialog::InitRepo { url_input, branch_input, .. }) => {
                    let branch = if branch_input.is_empty() { "main".to_string() } else { branch_input };
                    let bare_path = app.bare_repo_path.clone();
                    let tx = app.event_tx.clone();
                    app.set_status(if url_input.is_empty() {
                        format!("Initializing bare repo (branch '{}')...", branch)
                    } else {
                        format!("Cloning into bare repo...")
                    });
                    app.close_dialog();

                    // Run in background thread so UI stays responsive
                    std::thread::Builder::new()
                        .name("init-repo".into())
                        .spawn(move || {
                            let result = if url_input.is_empty() {
                                worktree::git::init_bare_repo(&bare_path, &branch)
                            } else {
                                worktree::git::clone_bare_repo(&bare_path, &url_input, &branch)
                            };
                            let error = result.err().map(|e| format!("{}", e));
                            let _ = tx.send(crate::event::AppEvent::InitRepoComplete {
                                error,
                            });
                        })
                        .ok();
                }
                Some(Dialog::ConvertRepo { mode, target_path_input, branch_name, source_repo_path, .. }) => {
                    let tx = app.event_tx.clone();
                    let branch = branch_name;
                    if mode == 0 {
                        // In-place conversion
                        let repo_path = source_repo_path.clone();
                        app.set_status("Converting repo in-place...");
                        app.close_dialog();
                        let bare_path = repo_path.clone();
                        std::thread::Builder::new()
                            .name("convert-repo".into())
                            .spawn(move || {
                                let result = worktree::git::convert_repo_in_place(&repo_path, &branch);
                                let error = result.err().map(|e| format!("{}", e));
                                let _ = tx.send(crate::event::AppEvent::ConvertRepoComplete {
                                    bare_repo_path: bare_path,
                                    error,
                                });
                            })
                            .ok();
                    } else {
                        // Different location
                        if target_path_input.is_empty() {
                            app.set_status("Target path cannot be empty");
                            app.dialog = Some(Dialog::ConvertRepo {
                                mode, target_path_input, branch_name: branch,
                                focused_field: 1, source_repo_path,
                            });
                            return;
                        }
                        let target = std::path::PathBuf::from(&target_path_input);
                        let source = source_repo_path;
                        let bare_path = target.clone();
                        app.set_status("Converting repo to new location...");
                        app.close_dialog();
                        std::thread::Builder::new()
                            .name("convert-repo".into())
                            .spawn(move || {
                                let result = worktree::git::convert_repo_to_location(&source, &target, &branch);
                                let error = result.err().map(|e| format!("{}", e));
                                let _ = tx.send(crate::event::AppEvent::ConvertRepoComplete {
                                    bare_repo_path: bare_path,
                                    error,
                                });
                            })
                            .ok();
                    }
                }
                Some(Dialog::CreateWorktree { branch_input, base_branch, .. }) => {
                    if !branch_input.is_empty() {
                        let bare_path = app.bare_repo_path.clone();
                        let branch = branch_input.clone();
                        let base = base_branch.clone();
                        let tx = app.event_tx.clone();
                        app.set_status(format!("Creating worktree '{}'...", branch));
                        app.close_dialog();

                        // Run in background thread so UI stays responsive
                        std::thread::Builder::new()
                            .name("create-worktree".into())
                            .spawn(move || {
                                let path = branch.clone();
                                let result = worktree::git::create_worktree(&bare_path, &branch, &path, &base);
                                let error = result.err().map(|e| format!("{}", e));
                                let _ = tx.send(crate::event::AppEvent::WorktreeCreated {
                                    branch,
                                    error,
                                });
                            })
                            .ok();
                    } else {
                        app.close_dialog();
                    }
                }
                Some(Dialog::MergeBranch {
                    source_worktree_idx,
                    branches,
                    selected,
                }) => {
                    if let Some(target_branch) = branches.get(selected) {
                        let target_branch = target_branch.clone();
                        let source_name = app
                            .worktrees
                            .get(source_worktree_idx)
                            .map(|w| w.branch.clone())
                            .unwrap_or_default();

                        // Quick check: source worktree clean?
                        match worktree::is_worktree_clean(app, source_worktree_idx) {
                            Ok(false) => {
                                // Remember the merge so we can retry after commit
                                app.pending_merge = Some(PendingAction::MergeExecute {
                                    source_worktree_idx,
                                    target_branch: target_branch.clone(),
                                });
                                match worktree::status_porcelain(app, source_worktree_idx) {
                                    Ok(changes) => {
                                        let files: Vec<(String, String)> = changes
                                            .iter()
                                            .map(|c| {
                                                (
                                                    format!("{}{}", c.index_status, c.work_status),
                                                    c.path.clone(),
                                                )
                                            })
                                            .collect();
                                        app.dialog = Some(Dialog::DirtyWorktree {
                                            worktree_idx: source_worktree_idx,
                                            files,
                                            selected: 0,
                                        });
                                    }
                                    Err(e) => {
                                        app.pending_merge = None;
                                        app.set_status(format!("Failed to get status: {}", e));
                                        app.close_dialog();
                                    }
                                }
                                return;
                            }
                            Err(e) => {
                                app.set_status(format!("Failed to check status of '{}': {}", source_name, e));
                                app.close_dialog();
                                return;
                            }
                            Ok(true) => {}
                        }

                        // Queue the heavy merge operation (worktree creation, merge, etc.)
                        app.close_dialog();
                        app.queue_action(
                            format!("Merging '{}' into '{}'...", source_name, target_branch),
                            PendingAction::MergeExecute {
                                source_worktree_idx,
                                target_branch,
                            },
                        );
                    } else {
                        app.close_dialog();
                    }
                }
                Some(Dialog::RenameSession { session_id, input }) => {
                    if let Some(session) = app.sessions.get_mut(&session_id) {
                        if input.is_empty() {
                            session.nickname = None;
                        } else {
                            session.nickname = Some(input);
                        }
                    }
                    app.close_dialog();
                }
                Some(Dialog::MergeConflict {
                    worktree_idx,
                    source_branch,
                    selected,
                }) => {
                    match selected {
                        0 => {
                            // VS Code
                            if let Some(wt) = app.worktrees.get(worktree_idx) {
                                let path = wt.path.clone();
                                std::thread::spawn(move || {
                                    let _ = std::process::Command::new("code")
                                        .arg(&path)
                                        .spawn();
                                });
                                app.set_status("Opening VS Code to resolve conflicts...");
                            }
                            app.close_dialog();
                        }
                        1 => {
                            // JetBrains
                            if let Some(wt) = app.worktrees.get(worktree_idx) {
                                let path = wt.path.clone();
                                std::thread::spawn(move || {
                                    // Try common JetBrains IDEs in order
                                    for cmd in ["idea", "webstorm", "goland", "pycharm", "clion", "rider"] {
                                        if std::process::Command::new(cmd)
                                            .arg(&path)
                                            .spawn()
                                            .is_ok()
                                        {
                                            return;
                                        }
                                    }
                                });
                                app.set_status("Opening JetBrains IDE to resolve conflicts...");
                            }
                            app.close_dialog();
                        }
                        2 | 3 => {
                            // Claude — spawn a session with merge prompt
                            let skip_perms = selected == 3;
                            let target_branch = app.worktrees.get(worktree_idx)
                                .map(|w| w.branch.clone())
                                .unwrap_or_default();
                            // Get source branch HEAD commit info for context
                            let source_head = app.worktrees.get(worktree_idx)
                                .and_then(|wt| worktree::git::branch_head_oneline(&wt.path, &source_branch).ok())
                                .unwrap_or_else(|| source_branch.clone());
                            let prompt = format!(
                                "Resolve the merge conflicts in this repository. Branch '{}' (HEAD: {}) was being merged into '{}'. Resolve all conflicts and create a commit.",
                                source_branch, source_head, target_branch
                            );
                            app.close_dialog();
                            if app.worktrees.get(worktree_idx).is_some() {
                                match session::spawn_session(app, worktree_idx, terminal_size, skip_perms, Some(&prompt)) {
                                    Ok(sid) => {
                                        app.active_session_id = Some(sid);
                                        app.focus = FocusTarget::TerminalPane;
                                        app.input_mode = InputMode::Terminal;
                                        app.rebuild_sidebar_items();
                                        app.set_status("Claude session opened — resolve merge conflicts");
                                    }
                                    Err(e) => {
                                        app.set_status(format!("Failed to spawn Claude: {}", e));
                                    }
                                }
                            }
                        }
                        4 => {
                            // Abort merge
                            match worktree::merge_abort(app, worktree_idx) {
                                Ok(()) => {
                                    app.set_status("Merge aborted");
                                    let _ = worktree::refresh_worktrees(app);
                                    app.refresh_worktree_status();
                                }
                                Err(e) => {
                                    app.set_status(format!("Failed to abort merge: {}", e));
                                }
                            }
                            app.close_dialog();
                        }
                        _ => {
                            app.close_dialog();
                        }
                    }
                }
                Some(Dialog::DirtyWorktree {
                    worktree_idx,
                    selected,
                    ..
                }) => {
                    match selected {
                        0 => {
                            // Commit changes — open GitCommit dialog
                            match worktree::status_porcelain(app, worktree_idx) {
                                Ok(changes) => {
                                    let mut unstaged = Vec::new();
                                    let mut staged = Vec::new();
                                    for c in &changes {
                                        if c.index_status != ' ' && c.index_status != '?' {
                                            staged.push((c.index_status, c.path.clone()));
                                        }
                                        if c.work_status != ' ' || c.index_status == '?' {
                                            let status = if c.index_status == '?' { '?' } else { c.work_status };
                                            unstaged.push((status, c.path.clone()));
                                        }
                                    }
                                    app.dialog = Some(Dialog::GitCommit {
                                        worktree_idx,
                                        unstaged,
                                        staged,
                                        section: 0,
                                        selected: 0,
                                        phase: CommitPhase::Staging,
                                        commit_message: String::new(),
                                    });
                                }
                                Err(e) => {
                                    app.set_status(format!("Failed to get status: {}", e));
                                    app.close_dialog();
                                }
                            }
                        }
                        1 => {
                            // Open with Claude
                            let wi = worktree_idx;
                            app.close_dialog();
                            if app.worktrees.get(wi).is_some() {
                                match session::spawn_session(app, wi, terminal_size, false, Some("Commit the uncommitted changes in this repository.")) {
                                    Ok(sid) => {
                                        app.active_session_id = Some(sid);
                                        app.focus = FocusTarget::TerminalPane;
                                        app.input_mode = InputMode::Terminal;
                                        app.rebuild_sidebar_items();
                                        app.set_status("Claude session opened — commit changes");
                                    }
                                    Err(e) => {
                                        app.set_status(format!("Failed to spawn Claude: {}", e));
                                    }
                                }
                            }
                        }
                        _ => {
                            // Cancel
                            app.pending_merge = None;
                            app.close_dialog();
                        }
                    }
                }
                Some(Dialog::GitCommit {
                    worktree_idx,
                    unstaged,
                    staged,
                    section,
                    selected,
                    phase,
                    commit_message,
                }) => {
                    match phase {
                        CommitPhase::Staging => {
                            // Enter does nothing in staging phase — put dialog back
                            app.dialog = Some(Dialog::GitCommit {
                                worktree_idx,
                                unstaged,
                                staged,
                                section,
                                selected,
                                phase,
                                commit_message,
                            });
                        }
                        CommitPhase::Message => {
                            if commit_message.is_empty() {
                                app.set_status("Commit message cannot be empty");
                                app.dialog = Some(Dialog::GitCommit {
                                    worktree_idx,
                                    unstaged,
                                    staged,
                                    section,
                                    selected,
                                    phase,
                                    commit_message,
                                });
                                return;
                            }
                            app.close_dialog();
                            app.queue_action("Committing...", PendingAction::Commit {
                                worktree_idx,
                                message: commit_message,
                            });
                        }
                    }
                }
                Some(Dialog::Confirm { on_confirm, .. }) => {
                    match on_confirm {
                        ConfirmAction::DeleteSession(sid) => {
                            session::kill_session(app, sid);
                        }
                        ConfirmAction::DeleteWorktree(_) | ConfirmAction::ForceDeleteWorktree(_) => {
                            // Should not happen — worktree deletion uses ConfirmDangerous now
                        }
                    }
                    app.close_dialog();
                }
                Some(Dialog::ConfirmDangerous { input, on_confirm, .. }) => {
                    if input.trim().eq_ignore_ascii_case("yes") {
                        match on_confirm {
                            ConfirmAction::DeleteWorktree(path) => {
                                match worktree::remove_worktree(app, &path) {
                                    Ok(_) => {
                                        app.set_status("Worktree removed");
                                        let _ = worktree::refresh_worktrees(app);
                                    }
                                    Err(e) => {
                                        let msg = format!("{}", e);
                                        if msg.contains("dirty") || msg.contains("untracked") || msg.contains("changes") {
                                            app.open_dialog(Dialog::ConfirmDangerous {
                                                message: "Worktree is dirty. FORCE DELETE?".to_string(),
                                                input: String::new(),
                                                on_confirm: ConfirmAction::ForceDeleteWorktree(path),
                                            });
                                            return;
                                        }
                                        app.set_status(format!("Error: {}", e));
                                    }
                                }
                            }
                            ConfirmAction::ForceDeleteWorktree(path) => {
                                match worktree::force_remove_worktree(app, &path) {
                                    Ok(_) => {
                                        app.set_status("Worktree force-removed");
                                        let _ = worktree::refresh_worktrees(app);
                                    }
                                    Err(e) => {
                                        app.set_status(format!("Error: {}", e));
                                    }
                                }
                            }
                            _ => {}
                        }
                        app.close_dialog();
                    }
                    // If input != "yes", Enter does nothing
                }
                None => {
                    app.close_dialog();
                }
            }
        }

        // Navigation within dialogs
        KeyCode::Up | KeyCode::Char('k') => {
            match app.dialog {
                Some(Dialog::MergeBranch { ref mut selected, .. }) => {
                    if *selected > 0 {
                        *selected -= 1;
                    }
                }
                Some(Dialog::MergeConflict { ref mut selected, .. }) => {
                    if *selected > 0 {
                        *selected -= 1;
                    }
                }
                Some(Dialog::DirtyWorktree { ref mut selected, .. }) => {
                    if *selected > 0 {
                        *selected -= 1;
                    }
                }
                Some(Dialog::GitCommit { ref phase, ref mut selected, ref unstaged, ref staged, ref section, .. }) => {
                    if *phase == CommitPhase::Staging {
                        if *selected > 0 {
                            *selected -= 1;
                        } else if *section == 0 && !unstaged.is_empty() {
                            // already at top of unstaged
                        } else if *section == 1 && !staged.is_empty() {
                            // already at top of staged
                        }
                    }
                }
                _ => {}
            }
        }
        KeyCode::Down | KeyCode::Char('j') => {
            match app.dialog {
                Some(Dialog::MergeBranch { ref mut selected, ref branches, .. }) => {
                    if *selected + 1 < branches.len() {
                        *selected += 1;
                    }
                }
                Some(Dialog::MergeConflict { ref mut selected, .. }) => {
                    if *selected + 1 < crate::app::CONFLICT_RESOLVER_COUNT {
                        *selected += 1;
                    }
                }
                Some(Dialog::DirtyWorktree { ref mut selected, .. }) => {
                    if *selected + 1 < crate::app::DIRTY_WORKTREE_OPTION_COUNT {
                        *selected += 1;
                    }
                }
                Some(Dialog::GitCommit { ref phase, ref mut selected, ref unstaged, ref staged, ref section, .. }) => {
                    if *phase == CommitPhase::Staging {
                        let len = if *section == 0 { unstaged.len() } else { staged.len() };
                        if len > 0 && *selected + 1 < len {
                            *selected += 1;
                        }
                    }
                }
                _ => {}
            }
        }

        KeyCode::Tab => {
            match app.dialog {
                Some(Dialog::CreateWorktree { ref mut focused_field, .. }) => {
                    *focused_field = (*focused_field + 1) % 2;
                }
                Some(Dialog::InitRepo { ref mut focused_field, .. }) => {
                    *focused_field = (*focused_field + 1) % 2;
                }
                Some(Dialog::ConvertRepo { ref mode, ref mut focused_field, .. }) => {
                    if *mode == 0 {
                        // In-place: skip field 1 (target path), cycle 0→2→0
                        *focused_field = if *focused_field == 0 { 2 } else { 0 };
                    } else {
                        // Different location: cycle 0→1→2→0
                        *focused_field = (*focused_field + 1) % 3;
                    }
                }
                Some(Dialog::GitCommit { ref phase, ref mut section, ref mut selected, ref unstaged, ref staged, .. }) => {
                    if *phase == CommitPhase::Staging {
                        *section = 1 - *section;
                        let len = if *section == 0 { unstaged.len() } else { staged.len() };
                        if len == 0 {
                            *selected = 0;
                        } else if *selected >= len {
                            *selected = len - 1;
                        }
                    }
                }
                _ => {}
            }
        }
        KeyCode::Char(c) => {
            match &mut app.dialog {
                Some(Dialog::CreateWorktree { ref mut branch_input, ref mut base_branch, focused_field, .. }) => {
                    match *focused_field {
                        0 => branch_input.push(c),
                        _ => base_branch.push(c),
                    }
                }
                Some(Dialog::InitRepo { ref mut url_input, ref mut branch_input, focused_field, .. }) => {
                    match *focused_field {
                        0 => url_input.push(c),
                        _ => branch_input.push(c),
                    }
                }
                Some(Dialog::ConvertRepo { ref mut target_path_input, ref mut branch_name, focused_field, .. }) => {
                    match *focused_field {
                        1 => target_path_input.push(c),
                        2 => branch_name.push(c),
                        _ => {} // field 0 is mode selector, no char input
                    }
                }
                Some(Dialog::RenameSession { ref mut input, .. }) => {
                    input.push(c);
                }
                Some(Dialog::ConfirmDangerous { ref mut input, .. }) => {
                    input.push(c);
                }
                Some(Dialog::GitCommit { ref phase, ref mut commit_message, .. }) => {
                    if *phase == CommitPhase::Message {
                        commit_message.push(c);
                    }
                }
                _ => {}
            }
        }
        KeyCode::Backspace => {
            match &mut app.dialog {
                Some(Dialog::CreateWorktree { ref mut branch_input, ref mut base_branch, focused_field, .. }) => {
                    match *focused_field {
                        0 => { branch_input.pop(); }
                        _ => { base_branch.pop(); }
                    }
                }
                Some(Dialog::InitRepo { ref mut url_input, ref mut branch_input, focused_field, .. }) => {
                    match *focused_field {
                        0 => { url_input.pop(); }
                        _ => { branch_input.pop(); }
                    }
                }
                Some(Dialog::ConvertRepo { ref mut target_path_input, ref mut branch_name, focused_field, .. }) => {
                    match *focused_field {
                        1 => { target_path_input.pop(); }
                        2 => { branch_name.pop(); }
                        _ => {}
                    }
                }
                Some(Dialog::RenameSession { ref mut input, .. }) => {
                    input.pop();
                }
                Some(Dialog::ConfirmDangerous { ref mut input, .. }) => {
                    input.pop();
                }
                Some(Dialog::GitCommit { ref phase, ref mut commit_message, .. }) => {
                    if *phase == CommitPhase::Message {
                        commit_message.pop();
                    }
                }
                _ => {}
            }
        }
        KeyCode::Left | KeyCode::Right => {
            if let Some(Dialog::ConvertRepo { ref mut mode, ref mut focused_field, .. }) = app.dialog {
                if *focused_field == 0 {
                    *mode = if *mode == 0 { 1 } else { 0 };
                }
            }
        }
        _ => {}
    }
}

/// Stage or unstage the selected file in GitCommit dialog.
fn handle_git_commit_space(app: &mut App) {
    let (worktree_idx, section, selected) = match &app.dialog {
        Some(Dialog::GitCommit { worktree_idx, section, selected, .. }) => {
            (*worktree_idx, *section, *selected)
        }
        _ => return,
    };

    if section == 0 {
        // Unstaged → stage
        let file = match &app.dialog {
            Some(Dialog::GitCommit { unstaged, .. }) => {
                unstaged.get(selected).map(|(_, p)| p.clone())
            }
            _ => None,
        };
        if let Some(file) = file {
            app.queue_action("Staging...", PendingAction::StageFile {
                worktree_idx,
                file,
            });
        }
    } else {
        // Staged → unstage
        let file = match &app.dialog {
            Some(Dialog::GitCommit { staged, .. }) => {
                staged.get(selected).map(|(_, p)| p.clone())
            }
            _ => None,
        };
        if let Some(file) = file {
            app.queue_action("Unstaging...", PendingAction::UnstageFile {
                worktree_idx,
                file,
            });
        }
    }
}

/// Stage all files in GitCommit dialog.
fn handle_git_commit_stage_all(app: &mut App) {
    let worktree_idx = match &app.dialog {
        Some(Dialog::GitCommit { worktree_idx, .. }) => *worktree_idx,
        _ => return,
    };

    app.queue_action("Staging all...", PendingAction::StageAll { worktree_idx });
}

/// Switch to commit message phase if staged files exist.
fn handle_git_commit_enter_message(app: &mut App) {
    if let Some(Dialog::GitCommit { ref staged, ref mut phase, .. }) = app.dialog {
        if staged.is_empty() {
            app.set_status("No staged files to commit");
        } else {
            *phase = CommitPhase::Message;
        }
    }
}

// ── Mini mode key handlers ─────────────────────────────────────────

fn handle_mini_mode_key(app: &mut App, key: KeyEvent, terminal_size: (u16, u16)) {
    match app.mini.focus {
        MiniModeFocus::AgentList => handle_mini_agent_list_key(app, key, terminal_size),
        MiniModeFocus::DetailInput => handle_mini_detail_input_key(app, key, terminal_size),
        MiniModeFocus::WorktreeSelector => handle_mini_worktree_selector_key(app, key),
        MiniModeFocus::PromptInput => handle_mini_prompt_input_key(app, key, terminal_size),
        MiniModeFocus::SavedPrompts => handle_mini_saved_prompts_key(app, key),
    }
}

fn handle_mini_agent_list_key(app: &mut App, key: KeyEvent, terminal_size: (u16, u16)) {
    // ? — help overlay
    if key.modifiers.is_empty() && key.code == KeyCode::Char('?') {
        app.show_help = true;
        app.help_tab = 5; // Mini Mode tab
        return;
    }

    match key.code {
        KeyCode::Char('j') | KeyCode::Down => {
            if !app.mini.items.is_empty() && app.mini.selected + 1 < app.mini.items.len() {
                app.mini.selected += 1;
            }
        }
        KeyCode::Char('k') | KeyCode::Up => {
            if app.mini.selected > 0 {
                app.mini.selected -= 1;
            }
        }
        KeyCode::Home => {
            app.mini.selected = 0;
        }
        KeyCode::End | KeyCode::Char('G') if key.modifiers == KeyModifiers::SHIFT || key.code == KeyCode::End => {
            if !app.mini.items.is_empty() {
                app.mini.selected = app.mini.items.len() - 1;
            }
        }
        KeyCode::Tab | KeyCode::Enter => {
            // Session → focus detail input; Worktree → toggle expand (Enter) or no-op (Tab)
            match app.mini.items.get(app.mini.selected).copied() {
                Some(SidebarItem::Session(_, _)) => {
                    app.mini.focus = MiniModeFocus::DetailInput;
                }
                Some(SidebarItem::Worktree(wi)) if key.code == KeyCode::Enter => {
                    if let Some(wt) = app.worktrees.get_mut(wi) {
                        wt.expanded = !wt.expanded;
                        app.rebuild_mini_agent_list();
                        app.rebuild_sidebar_items();
                    }
                }
                _ => {}
            }
        }
        KeyCode::Char('o') => {
            // Open full terminal drilldown for selected session
            if let Some(SidebarItem::Session(wi, si)) = app.mini.items.get(app.mini.selected).copied() {
                if let Some(wt) = app.worktrees.get(wi) {
                    if let Some(&sid) = wt.session_ids.get(si) {
                        app.mini_drilldown_session = Some(sid);
                        app.active_session_id = Some(sid);
                        app.screen_mode = ScreenMode::MiniDrilldown;
                        app.input_mode = InputMode::Terminal;
                        app.focus = FocusTarget::TerminalPane;
                        app.terminal_scroll = 0;
                        session::resize_all(app, terminal_size.1, terminal_size.0);
                    }
                }
            }
        }
        KeyCode::Char(' ') => {
            // Toggle expand on worktree
            if let Some(SidebarItem::Worktree(wi)) = app.mini.items.get(app.mini.selected).copied() {
                if let Some(wt) = app.worktrees.get_mut(wi) {
                    wt.expanded = !wt.expanded;
                    app.rebuild_mini_agent_list();
                    app.rebuild_sidebar_items();
                }
            }
        }
        KeyCode::Char('a') => {
            // Create new agent — determine target worktree from selection
            if app.worktrees.is_empty() {
                app.set_status("No worktrees available");
                return;
            }
            let target_wi = match app.mini.items.get(app.mini.selected).copied() {
                Some(SidebarItem::Worktree(wi)) => wi,
                Some(SidebarItem::Session(wi, _)) => wi,
                None => 0,
            };
            app.mini.target_worktree_idx = target_wi;
            // If only one worktree, skip selection and go straight to prompt
            if app.worktrees.len() == 1 {
                app.mini.prompt_input.clear();
                app.mini.focus = MiniModeFocus::PromptInput;
            } else {
                app.mini.focus = MiniModeFocus::WorktreeSelector;
            }
        }
        KeyCode::Char('d') => {
            // Kill selected agent or delete worktree
            match app.mini.items.get(app.mini.selected).copied() {
                Some(SidebarItem::Session(wi, si)) => {
                    if let Some(wt) = app.worktrees.get(wi) {
                        if let Some(&sid) = wt.session_ids.get(si) {
                            app.open_dialog(Dialog::Confirm {
                                message: format!("Kill agent {}?", session::session_label(app, sid)),
                                on_confirm: ConfirmAction::DeleteSession(sid),
                            });
                        }
                    }
                }
                Some(SidebarItem::Worktree(wi)) => {
                    if let Some(wt) = app.worktrees.get(wi) {
                        let path = wt.path.clone();
                        let has_sessions = !wt.session_ids.is_empty();
                        let msg = if has_sessions {
                            format!("DELETE worktree '{}' and kill {} session(s)", wt.branch, wt.session_ids.len())
                        } else {
                            format!("DELETE worktree '{}'", wt.branch)
                        };
                        app.open_dialog(Dialog::ConfirmDangerous {
                            message: msg,
                            input: String::new(),
                            on_confirm: ConfirmAction::DeleteWorktree(path),
                        });
                    }
                }
                None => {}
            }
        }
        KeyCode::Char('r') => {
            // Rename selected agent
            if let Some(SidebarItem::Session(wi, si)) = app.mini.items.get(app.mini.selected).copied() {
                if let Some(wt) = app.worktrees.get(wi) {
                    if let Some(&sid) = wt.session_ids.get(si) {
                        let current = app.sessions.get(&sid)
                            .and_then(|s| s.nickname.clone())
                            .unwrap_or_default();
                        app.open_dialog(Dialog::RenameSession {
                            session_id: sid,
                            input: current,
                        });
                    }
                }
            }
        }
        KeyCode::Char('s') => {
            // Open saved prompts browser
            app.mini.saved_prompt_selected = 0;
            app.mini.focus = MiniModeFocus::SavedPrompts;
        }
        KeyCode::Char('z') => {
            // Collapse all
            for wt in &mut app.worktrees {
                wt.expanded = false;
            }
            app.rebuild_mini_agent_list();
            app.rebuild_sidebar_items();
        }
        KeyCode::Char('Z') if key.modifiers == KeyModifiers::SHIFT => {
            // Expand all
            for wt in &mut app.worktrees {
                wt.expanded = true;
            }
            app.rebuild_mini_agent_list();
            app.rebuild_sidebar_items();
        }
        KeyCode::Esc => {
            // Return to normal mode
            app.screen_mode = ScreenMode::Normal;
            app.input_mode = InputMode::Normal;
        }
        _ => {}
    }
}

/// Instruction appended to every message sent from mini mode (both initial prompts
/// and follow-up messages). Tells Claude to wrap important summary output in XML
/// tags so extract_summary() can parse it reliably.
const CLAWTREE_INSTRUCTION: &str = "\n\nCRITICAL SYSTEM INSTRUCTION: You MUST end your response with a summary wrapped in XML tags like this:\n<IMPORTANT_CLAWTREE_OUTPUT>\n[your 1-2 sentence summary of what you did and the result]\n</IMPORTANT_CLAWTREE_OUTPUT>\nThis is required for every response. Do not skip this.";

fn handle_mini_detail_input_key(app: &mut App, key: KeyEvent, _terminal_size: (u16, u16)) {
    // Alt+Enter or Shift+Enter → insert newline
    if key.code == KeyCode::Enter
        && key.modifiers.intersects(KeyModifiers::ALT | KeyModifiers::SHIFT)
    {
        app.mini.detail_input.push('\n');
        return;
    }

    match key.code {
        KeyCode::Enter => {
            if app.mini.detail_input.is_empty() {
                return;
            }
            // Send the typed text + CLAWTREE instruction + Enter to the selected agent's PTY
            let sid = match app.mini.items.get(app.mini.selected).copied() {
                Some(SidebarItem::Session(wi, si)) => {
                    app.worktrees.get(wi).and_then(|wt| wt.session_ids.get(si)).copied()
                }
                _ => None,
            };
            if let Some(sid) = sid {
                if let Some(session) = app.sessions.get(&sid) {
                    let user_text: String = app.mini.detail_input.drain(..).collect();
                    let full_text = format!("{}{}", user_text, CLAWTREE_INSTRUCTION);

                    if let Some(ref tmux_name) = session.tmux_session_name {
                        let tmux = tmux_name.clone();
                        std::thread::spawn(move || {
                            // Use tmux set-buffer + paste-buffer for multi-line safety.
                            // send-keys -l would interpret \n as Enter presses, submitting
                            // each line separately. paste-buffer triggers bracketed paste
                            // mode so the terminal receives the full text as a single paste.
                            let _ = std::process::Command::new("tmux")
                                .args(["set-buffer", &full_text])
                                .output();
                            let _ = std::process::Command::new("tmux")
                                .args(["paste-buffer", "-t", &tmux])
                                .output();
                            std::thread::sleep(std::time::Duration::from_millis(100));
                            let _ = std::process::Command::new("tmux")
                                .args(["send-keys", "-t", &tmux, "Enter"])
                                .output();
                        });
                    } else {
                        // Non-tmux fallback: write text + CR directly to PTY
                        let mut payload = full_text.into_bytes();
                        payload.push(b'\r');
                        let _ = session.write_tx.send(bytes::Bytes::from(payload));
                    }
                    app.set_status("Sent to agent");
                }
            }
        }
        KeyCode::Tab | KeyCode::Esc => {
            // Back to tree sidebar
            app.mini.focus = MiniModeFocus::AgentList;
        }
        KeyCode::Backspace => {
            app.mini.detail_input.pop();
        }
        KeyCode::Char(c) if c != '\n' && c != '\r' => {
            app.mini.detail_input.push(c);
        }
        _ => {}
    }
}

fn handle_mini_worktree_selector_key(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Char('j') | KeyCode::Down => {
            if !app.worktrees.is_empty() && app.mini.target_worktree_idx + 1 < app.worktrees.len() {
                app.mini.target_worktree_idx += 1;
            }
        }
        KeyCode::Char('k') | KeyCode::Up => {
            if app.mini.target_worktree_idx > 0 {
                app.mini.target_worktree_idx -= 1;
            }
        }
        KeyCode::Enter => {
            // Select worktree, advance to prompt input
            app.mini.prompt_input.clear();
            app.mini.focus = MiniModeFocus::PromptInput;
        }
        KeyCode::Esc => {
            app.mini.focus = MiniModeFocus::AgentList;
        }
        _ => {}
    }
}

fn handle_mini_prompt_input_key(app: &mut App, key: KeyEvent, terminal_size: (u16, u16)) {
    // Alt+Enter or Shift+Enter → insert newline
    if key.code == KeyCode::Enter
        && key.modifiers.intersects(KeyModifiers::ALT | KeyModifiers::SHIFT)
    {
        app.mini.prompt_input.push('\n');
        return;
    }

    match key.code {
        KeyCode::Enter => {
            if app.mini.prompt_input.is_empty() {
                app.set_status("Prompt cannot be empty");
                return;
            }
            let prompt = format!("{}{}", app.mini.prompt_input, CLAWTREE_INSTRUCTION);
            let wi = app.mini.target_worktree_idx;
            app.mini.prompt_input.clear();
            app.mini.focus = MiniModeFocus::AgentList;

            // Spawn the agent
            match session::spawn_session(app, wi, terminal_size, false, Some(&prompt)) {
                Ok(_sid) => {
                    app.rebuild_sidebar_items();
                    app.rebuild_mini_agent_list();
                    // Select the newly created agent (last in list)
                    if !app.mini.items.is_empty() {
                        app.mini.selected = app.mini.items.len() - 1;
                    }
                    app.set_status("Agent spawned");
                }
                Err(e) => {
                    app.set_status(format!("Failed to spawn agent: {}", e));
                }
            }
        }
        KeyCode::Tab => {
            // Switch to saved prompts picker
            app.mini.saved_prompt_selected = 0;
            app.mini.focus = MiniModeFocus::SavedPrompts;
        }
        KeyCode::Esc => {
            app.mini.prompt_input.clear();
            app.mini.focus = MiniModeFocus::AgentList;
        }
        KeyCode::Backspace => {
            app.mini.prompt_input.pop();
        }
        KeyCode::Char(c) if c != '\n' && c != '\r' => {
            app.mini.prompt_input.push(c);
        }
        _ => {}
    }
}

fn handle_mini_saved_prompts_key(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Char('j') | KeyCode::Down => {
            if !app.saved_prompts.is_empty() && app.mini.saved_prompt_selected + 1 < app.saved_prompts.len() {
                app.mini.saved_prompt_selected += 1;
            }
        }
        KeyCode::Char('k') | KeyCode::Up => {
            if app.mini.saved_prompt_selected > 0 {
                app.mini.saved_prompt_selected -= 1;
            }
        }
        KeyCode::Enter => {
            // Load selected prompt into input field
            if let Some(sp) = app.saved_prompts.get(app.mini.saved_prompt_selected) {
                app.mini.prompt_input = sp.prompt.clone();
                app.mini.focus = MiniModeFocus::PromptInput;
            }
        }
        KeyCode::Char('a') => {
            // Save current input as new template
            if app.mini.prompt_input.is_empty() {
                app.set_status("Type a prompt first, then save it");
                return;
            }
            let name = format!("Prompt {}", app.saved_prompts.len() + 1);
            app.saved_prompts.push(SavedPrompt {
                name,
                prompt: app.mini.prompt_input.clone(),
            });
            app.save_saved_prompts();
            app.set_status("Prompt saved");
        }
        KeyCode::Char('d') => {
            // Delete selected template
            if !app.saved_prompts.is_empty() && app.mini.saved_prompt_selected < app.saved_prompts.len() {
                app.saved_prompts.remove(app.mini.saved_prompt_selected);
                if app.mini.saved_prompt_selected >= app.saved_prompts.len() && app.mini.saved_prompt_selected > 0 {
                    app.mini.saved_prompt_selected -= 1;
                }
                app.save_saved_prompts();
                app.set_status("Prompt deleted");
            }
        }
        KeyCode::Esc => {
            app.mini.focus = MiniModeFocus::PromptInput;
        }
        _ => {}
    }
}

fn handle_mini_drilldown_key(app: &mut App, key: KeyEvent, terminal_size: (u16, u16)) {
    // Tab — return to mini mode agent list
    if key.code == KeyCode::Tab {
        app.screen_mode = ScreenMode::Mini;
        app.input_mode = InputMode::Normal;
        app.mini.focus = MiniModeFocus::AgentList;
        app.terminal_scroll = 0;
        app.rebuild_mini_agent_list();
        session::resize_all(app, terminal_size.1, terminal_size.0);
        return;
    }

    // All other keys forwarded to terminal
    handle_terminal_key(app, key);
}

/// Convert a KeyEvent into raw bytes suitable for writing to a PTY.
fn key_to_bytes(key: KeyEvent, app_cursor: bool) -> Vec<u8> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);

    match key.code {
        KeyCode::Char(c) => {
            if ctrl {
                let byte = (c.to_ascii_lowercase() as u8).wrapping_sub(b'a').wrapping_add(1);
                if alt {
                    vec![0x1b, byte]
                } else {
                    vec![byte]
                }
            } else if alt {
                let mut buf = vec![0x1b];
                let mut char_buf = [0u8; 4];
                buf.extend_from_slice(c.encode_utf8(&mut char_buf).as_bytes());
                buf
            } else {
                let mut char_buf = [0u8; 4];
                c.encode_utf8(&mut char_buf).as_bytes().to_vec()
            }
        }
        KeyCode::Enter => vec![b'\r'],
        KeyCode::Backspace => vec![0x7f],
        KeyCode::Tab => vec![b'\t'],
        KeyCode::BackTab => vec![0x1b, b'[', b'Z'],
        KeyCode::Esc => vec![0x1b],
        KeyCode::Up => {
            if app_cursor { vec![0x1b, b'O', b'A'] } else { vec![0x1b, b'[', b'A'] }
        }
        KeyCode::Down => {
            if app_cursor { vec![0x1b, b'O', b'B'] } else { vec![0x1b, b'[', b'B'] }
        }
        KeyCode::Right => {
            if app_cursor { vec![0x1b, b'O', b'C'] } else { vec![0x1b, b'[', b'C'] }
        }
        KeyCode::Left => {
            if app_cursor { vec![0x1b, b'O', b'D'] } else { vec![0x1b, b'[', b'D'] }
        }
        KeyCode::Home => vec![0x1b, b'[', b'H'],
        KeyCode::End => vec![0x1b, b'[', b'F'],
        KeyCode::PageUp => vec![0x1b, b'[', b'5', b'~'],
        KeyCode::PageDown => vec![0x1b, b'[', b'6', b'~'],
        KeyCode::Insert => vec![0x1b, b'[', b'2', b'~'],
        KeyCode::Delete => vec![0x1b, b'[', b'3', b'~'],
        KeyCode::F(n) => f_key_bytes(n),
        _ => vec![],
    }
}

fn f_key_bytes(n: u8) -> Vec<u8> {
    match n {
        1 => vec![0x1b, b'O', b'P'],
        2 => vec![0x1b, b'O', b'Q'],
        3 => vec![0x1b, b'O', b'R'],
        4 => vec![0x1b, b'O', b'S'],
        5 => vec![0x1b, b'[', b'1', b'5', b'~'],
        6 => vec![0x1b, b'[', b'1', b'7', b'~'],
        7 => vec![0x1b, b'[', b'1', b'8', b'~'],
        8 => vec![0x1b, b'[', b'1', b'9', b'~'],
        9 => vec![0x1b, b'[', b'2', b'0', b'~'],
        10 => vec![0x1b, b'[', b'2', b'1', b'~'],
        11 => vec![0x1b, b'[', b'2', b'3', b'~'],
        12 => vec![0x1b, b'[', b'2', b'4', b'~'],
        _ => vec![],
    }
}

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::app::{App, CommitPhase, ConfirmAction, Dialog, FocusTarget, InputMode, MiniModeFocus, PendingAction, ScreenMode, SidebarItem, SavedPrompt, StatusSeverity};
use crate::session;
use crate::ui::terminal_pane;
use crate::url;
use crate::worktree;

// ── Keybinding registry ─────────────────────────────────────────────

/// A single keybinding entry for help display: (key_display, description).
/// When `key_display` is empty (`""`), the entry is a section header and
/// `description` holds the group title.
pub type KeyEntry = (&'static str, &'static str);

/// Create a section-header sentinel entry for the help overlay.
pub const fn section(label: &'static str) -> KeyEntry {
    ("", label)
}

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

    /// Count display rows including section headers and blank separator lines.
    /// Each section header adds a blank line before it (except the first) plus
    /// the header line itself.
    pub fn display_row_count(&self, wt_available: bool) -> usize {
        let mut rows: usize = 0;
        let mut first_section = true;
        for entry in self.keys().iter().chain(self.extra_keys(wt_available).iter()) {
            if entry.0.is_empty() {
                // section header: blank line before (except first) + header line
                if first_section {
                    rows += 1;
                    first_section = false;
                } else {
                    rows += 2;
                }
            } else {
                rows += 1;
            }
        }
        rows
    }
}

const GLOBAL_KEYS: &[KeyEntry] = &[
    section("Application"),
    ("Ctrl + Q",    "Quit application"),
    ("?",           "Show/hide this help"),
    section("Panels"),
    ("Ctrl + B",    "Toggle sidebar"),
    ("Ctrl + P",    "Toggle prompt queue"),
    ("F2",          "Toggle Mini Mode"),
    section("Mouse"),
    ("Click",       "Enable text selection (any key restores scroll)"),
];

const SIDEBAR_KEYS: &[KeyEntry] = &[
    section("Navigation"),
    ("Tab",                     "Focus terminal / info panel"),
    ("j / Down",                "Navigate down"),
    ("k / Up",                  "Navigate up"),
    ("Shift + G",               "Jump to bottom"),
    ("Home / End",              "Jump to top / bottom"),
    ("PgUp / PgDn",             "Scroll terminal"),
    section("Selection"),
    ("Enter",                   "Activate selected item"),
    ("Space",                   "Toggle expand/collapse"),
    ("z / Shift + Z",           "Collapse / expand all worktrees"),
    section("Sessions & Agents"),
    ("c",                       "claude"),
    ("Shift + c",               "claude (YOLO)"),
    ("t",                       "New terminal session"),
    ("r",                       "Rename/nickname session"),
    section("Worktree Management"),
    ("n",                       "New worktree"),
    ("d",                       "Delete session/worktree"),
    ("Shift + D",               "Force-delete worktree"),
    ("F5 / Ctrl + R",           "Refresh worktrees"),
    section("Git Operations"),
    ("m",                       "Merge branch"),
    ("s",                       "Stage & commit"),
    ("Ctrl + s",                "Stage all & commit with Claude"),
    ("p",                       "Push branch to remote"),
    ("Shift + p",               "Pull branch from remote"),
    section("URLs"),
    ("u",                       "Copy last URL to clipboard"),
    ("Shift + U",               "Open last URL in browser"),
];

/// Additional sidebar keys shown only when wt.exe is available.
const SIDEBAR_KEYS_WT: &[KeyEntry] = &[
    section("Windows Terminal"),
    ("w",                       "Open Windows Terminal tab"),
    ("Shift + W",               "Windows Terminal + Claude"),
];

const TERMINAL_KEYS: &[KeyEntry] = &[
    section("Navigation"),
    ("Tab",                     "Back to sidebar / prompt queue"),
    ("PgUp / PgDn",             "Scroll through history"),
    section("URLs"),
    ("Ctrl + U",                "Copy last URL to clipboard"),
    section("Input"),
    ("(all keys)",              "Sent directly to Claude session"),
];

const QUEUE_KEYS: &[KeyEntry] = &[
    section("Navigation"),
    ("Tab",         "Back to sidebar"),
    ("Esc",         "Cancel edit / back to sidebar"),
    ("Up / Down",   "Navigate queue items"),
    section("Editing"),
    ("Enter",       "Add item / save edit / load for editing"),
    ("d / Delete",  "Delete selected item"),
    ("(type)",      "Input text for new/editing prompt"),
    ("Backspace",   "Delete character"),
];

const MINI_MODE_KEYS: &[KeyEntry] = &[
    section("Navigation"),
    ("j / Down",                "Navigate tree"),
    ("k / Up",                  "Navigate tree"),
    ("Esc",                     "Return to normal mode"),
    section("Selection & Expand"),
    ("Tab / Enter",             "Focus detail input (on agent)"),
    ("Enter",                   "Toggle expand (on worktree)"),
    ("Space",                   "Toggle expand/collapse worktree"),
    ("z / Shift + Z",           "Collapse / expand all"),
    section("Actions"),
    ("o",                       "Open full terminal (drilldown)"),
    ("a",                       "Create new agent"),
    ("d",                       "Kill agent / remove worktree"),
    ("r",                       "Rename agent"),
    ("s",                       "Browse saved prompts"),
    section("Detail Pane"),
    ("(detail)",                "Type + Enter: send to agent"),
];

const INFO_PANEL_KEYS: &[KeyEntry] = &[
    section("Navigation"),
    ("j / Down",                "Navigate files"),
    ("k / Up",                  "Navigate files"),
    ("Tab",                     "Switch unstaged/staged section"),
    ("Esc",                     "Back to sidebar"),
    section("Git Operations"),
    ("s",                       "Stage & commit"),
    ("Ctrl + s",                "Stage all & commit with Claude"),
    ("Shift + c",               "claude"),
    ("n",                       "New worktree"),
    ("d",                       "Delete worktree"),
    ("m",                       "Merge branch"),
    ("p",                       "Push branch"),
    ("Shift + p",               "Pull branch"),
    ("F5 / Ctrl + R",           "Refresh"),
];

/// Handle a key event based on current input mode.
pub fn handle_key(app: &mut App, key: KeyEvent, terminal_size: (u16, u16)) {
    // ── Help overlay intercepts all keys when visible ──────────────
    if app.show_help {
        handle_help_key(app, key, terminal_size);
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
    if key.modifiers == KeyModifiers::CONTROL && key.code == KeyCode::Char('p') && app.dialog.is_none() {
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
        // Don't open help if we're typing in prompt queue
        if !app.prompt_queue_focused() {
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
fn handle_help_key(app: &mut App, key: KeyEvent, terminal_size: (u16, u16)) {
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
            let total = ctx.display_row_count(app.wt_available);
            let visible = help_visible_rows(total, terminal_size.1);
            let max_scroll = total.saturating_sub(visible);
            if app.help_scroll < max_scroll {
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

/// Compute how many key-list rows are visible in the help popup.
/// Mirrors the layout logic in ui/help.rs.
fn help_visible_rows(display_rows: usize, term_height: u16) -> usize {
    let max_height = (term_height as f32 * 0.8) as u16;
    // tabs(1) + separator(1) + display_rows + separator(1) + help(1) + borders(2)
    let content_height = (display_rows as u16) + 6;
    let height = content_height.min(max_height).max(10);
    // inner height = height - 2 (borders), key list = inner - 4 (tabs + 2 seps + footer)
    (height.saturating_sub(6)) as usize
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
                        confirmed: false,
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

        // Ctrl+S — stage all + generate commit message with Claude
        (KeyModifiers::CONTROL, KeyCode::Char('s')) => {
            handle_stage_all_commit_claude(app);
        }

        // s — stage/commit (open GitCommit dialog for selected worktree)
        (_, KeyCode::Char('s')) => {
            handle_stage_commit(app);
        }

        // p — push branch to remote
        (_, KeyCode::Char('p')) => {
            handle_push(app);
        }

        // P — pull branch from remote
        (KeyModifiers::SHIFT, KeyCode::Char('P')) => {
            handle_pull(app);
        }

        // r — rename/nickname a session
        (_, KeyCode::Char('r')) => {
            let sid = match app.selected_sidebar_item() {
                Some(SidebarItem::Session(wi, si)) => {
                    app.worktrees.get(wi).and_then(|wt| wt.session_ids.get(si).copied())
                }
                Some(SidebarItem::Terminal(ti)) => {
                    app.terminal_ids.get(ti).copied()
                }
                _ => None,
            };
            if let Some(sid) = sid {
                let current = app.sessions.get(&sid)
                    .and_then(|s| s.nickname.clone())
                    .unwrap_or_default();
                app.open_dialog(Dialog::RenameSession {
                    session_id: sid,
                    input: current,
                });
            }
        }

        // u — copy last URL to clipboard
        (_, KeyCode::Char('u')) => {
            handle_url_copy(app);
        }
        // U — open last URL in browser
        (KeyModifiers::SHIFT, KeyCode::Char('U')) => {
            handle_url_open(app);
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
        KeyCode::Char('s') if key.modifiers == KeyModifiers::CONTROL => {
            handle_stage_all_commit_claude(app);
        }
        KeyCode::Char('s') => {
            handle_stage_commit(app);
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
        KeyCode::Char('P') if key.modifiers == KeyModifiers::SHIFT => {
            handle_pull(app);
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

/// Get the worktree index for the currently selected sidebar item.
/// For Terminal items, resolves via the session's worktree_path.
fn selected_worktree_idx(app: &App) -> Option<usize> {
    match app.selected_sidebar_item() {
        Some(SidebarItem::Worktree(wi)) => Some(wi),
        Some(SidebarItem::Session(wi, _)) => Some(wi),
        Some(SidebarItem::Terminal(ti)) => {
            let sid = app.terminal_ids.get(ti)?;
            let session = app.sessions.get(sid)?;
            app.worktrees.iter().position(|wt| wt.path == session.worktree_path)
        }
        None => None,
    }
}

/// Get the branch name of the currently selected worktree (or "main" as default).
fn selected_worktree_branch(app: &App) -> String {
    selected_worktree_idx(app)
        .and_then(|i| app.worktrees.get(i))
        .map(|wt| wt.branch.clone())
        .unwrap_or_else(|| "main".to_string())
}

fn open_wsl_window(app: &mut App, with_claude: bool) {
    if let Some(wi) = selected_worktree_idx(app) {
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
    if let Some(wi) = selected_worktree_idx(app) {
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
    if let Some(wi) = selected_worktree_idx(app) {
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
        Some(SidebarItem::Terminal(ti)) => {
            if let Some(&sid) = app.terminal_ids.get(ti) {
                app.open_dialog(Dialog::Confirm {
                    message: format!("Kill terminal {}?", session::session_label(app, sid)),
                    on_confirm: ConfirmAction::DeleteSession(sid),
                });
            }
        }
        Some(SidebarItem::Worktree(wi)) => {
            if let Some(wt) = app.worktrees.get(wi) {
                let path = wt.path.clone();
                let session_count = wt.session_ids.len();
                let terminal_count = app.terminal_ids.iter().filter(|&&tid| {
                    app.sessions.get(&tid)
                        .map(|s| s.worktree_path == wt.path)
                        .unwrap_or(false)
                }).count();
                let total = session_count + terminal_count;
                let msg = if total > 0 {
                    format!(
                        "DELETE worktree '{}' and kill {} session(s)",
                        wt.branch,
                        total
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
    if let Some(wi) = selected_worktree_idx(app) {
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
    if let Some(wi) = selected_worktree_idx(app) {
        app.queue_action("Loading status...", PendingAction::OpenStageCommit { worktree_idx: wi });
    }
}

fn handle_stage_all_commit_claude(app: &mut App) {
    if let Some(wi) = selected_worktree_idx(app) {
        app.queue_action("Staging all & generating commit message...", PendingAction::StageAllAndCommitClaude { worktree_idx: wi });
    }
}

fn handle_push(app: &mut App) {
    if let Some(wi) = selected_worktree_idx(app) {
        if let Some(wt) = app.worktrees.get(wi) {
            let path = wt.path.clone();
            let branch = wt.branch.clone();
            let tx = app.event_tx.clone();
            let git_lock = std::sync::Arc::clone(&app.git_lock);
            app.set_status(format!("Pushing '{}'...", branch));

            std::thread::Builder::new()
                .name("git-push".into())
                .spawn(move || {
                    let _guard = git_lock.lock().unwrap_or_else(|e| e.into_inner());
                    let result = worktree::git::push_branch(&path, &branch);
                    let error = result.err().map(|e| format!("{}", e));
                    let _ = tx.send(crate::event::AppEvent::PushComplete { branch, error });
                })
                .ok();
        }
    }
}

fn handle_pull(app: &mut App) {
    if let Some(wi) = selected_worktree_idx(app) {
        if let Some(wt) = app.worktrees.get(wi) {
            let path = wt.path.clone();
            let branch = wt.branch.clone();
            let tx = app.event_tx.clone();
            let git_lock = std::sync::Arc::clone(&app.git_lock);
            app.set_status(format!("Pulling '{}'...", branch));

            std::thread::Builder::new()
                .name("git-pull".into())
                .spawn(move || {
                    let _guard = git_lock.lock().unwrap_or_else(|e| e.into_inner());
                    match worktree::git::pull_branch(&path) {
                        Ok(worktree::git::PullResult::Success) => {
                            let _ = tx.send(crate::event::AppEvent::PullComplete {
                                branch,
                                worktree_idx: wi,
                                error: None,
                                has_conflicts: false,
                            });
                        }
                        Ok(worktree::git::PullResult::Conflict(msg)) => {
                            let _ = tx.send(crate::event::AppEvent::PullComplete {
                                branch,
                                worktree_idx: wi,
                                error: Some(msg),
                                has_conflicts: true,
                            });
                        }
                        Err(e) => {
                            let _ = tx.send(crate::event::AppEvent::PullComplete {
                                branch,
                                worktree_idx: wi,
                                error: Some(format!("{}", e)),
                                has_conflicts: false,
                            });
                        }
                    }
                })
                .ok();
        }
    }
}

fn handle_merge(app: &mut App) {
    if let Some(wi) = selected_worktree_idx(app) {
        // If this worktree already has an unresolved merge, show the conflict dialog
        if let Some(source_branch) = worktree::merge_in_progress(app, wi) {
            app.set_status("Worktree has an unresolved merge — resolve or abort first");
            app.open_dialog(Dialog::MergeConflict {
                worktree_idx: wi,
                source_branch,
                selected: 0,
            });
            return;
        }
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
                    .filter(|b| source_branch.is_none_or(|sb| b != sb))
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
            if app.prompt_queue_input.is_empty() && app.prompt_queue_editing.is_none()
                && app.prompt_queue_selected > 0 {
                app.prompt_queue_selected -= 1;
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

    // Ctrl+U — copy last URL to clipboard (intercepted before PTY forwarding)
    if key.modifiers == KeyModifiers::CONTROL && key.code == KeyCode::Char('u') {
        handle_url_copy(app);
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
                Some(Dialog::GitCommit { ref mut commit_message, phase: CommitPhase::Message, ref mut cursor_pos, .. }) => {
                    let byte_pos = commit_message.char_indices()
                        .nth(*cursor_pos)
                        .map(|(i, _)| i)
                        .unwrap_or(commit_message.len());
                    commit_message.insert_str(byte_pos, &data);
                    *cursor_pos += data.chars().count();
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

pub(crate) const SCROLL_LINES: usize = 3;
const SCROLL_PAGE: usize = 20;

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

/// Copy the last detected URL to the clipboard.
fn handle_url_copy(app: &mut App) {
    if let Some(last) = app.url_cache.urls.last() {
        let u = last.url.clone();
        match url::copy_to_clipboard(&u) {
            Ok(()) => app.set_status(format!("Copied: {}", u)),
            Err(e) => app.set_status_with(StatusSeverity::Error, format!("Clipboard error: {}", e)),
        }
    } else {
        app.set_status("No URLs detected");
    }
}

/// Open the last detected URL in the browser.
fn handle_url_open(app: &mut App) {
    if let Some(last) = app.url_cache.urls.last() {
        let u = last.url.clone();
        match url::open_url_in_browser(&u) {
            Ok(()) => app.set_status(format!("Opened: {}", u)),
            Err(e) => app.set_status_with(StatusSeverity::Error, format!("Failed to open URL: {}", e)),
        }
    } else {
        app.set_status("No URLs detected");
    }
}

fn handle_dialog_key(app: &mut App, key: KeyEvent, terminal_size: (u16, u16)) {
    // GitCommit-specific keys need to be handled before the general match
    // because Space, 'a', 'c' have special meaning in staging phase
    if let Some(Dialog::GitCommit { ref phase, .. }) = app.dialog {
        if *phase == CommitPhase::Staging {
            // Ctrl+C — generate commit message with Claude (skips manual message phase)
            // Crossterm sends Ctrl+C as Char('\x03') without keyboard enhancement.
            let is_ctrl_c = (key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c'))
                || key.code == KeyCode::Char('\x03');
            if is_ctrl_c {
                handle_git_commit_claude_message(app);
                return;
            }
            // Ctrl+A — stage all + generate commit message with Claude
            // Crossterm sends Ctrl+A as Char('\x01') without keyboard enhancement.
            let is_ctrl_a = (key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('a'))
                || key.code == KeyCode::Char('\x01');
            if is_ctrl_a {
                if let Some(Dialog::GitCommit { worktree_idx, .. }) = &app.dialog {
                    let wi = *worktree_idx;
                    app.queue_action("Staging all & generating commit message...", PendingAction::StageAll { worktree_idx: wi, then_claude: true });
                }
                return;
            }
            match (key.modifiers, key.code) {
                (_, KeyCode::Char(' ')) => {
                    handle_git_commit_space(app);
                    return;
                }
                (_, KeyCode::Char('a')) => {
                    handle_git_commit_stage_all(app);
                    return;
                }
                (_, KeyCode::Char('c')) => {
                    handle_git_commit_enter_message(app);
                    return;
                }
                _ => {} // fall through to general handler
            }
        }
    }

    // GitCommit message phase: Ctrl+G to generate AI commit message
    // Crossterm may report Ctrl+G as Char('\x07') (ASCII BEL) without keyboard enhancement.
    if let Some(Dialog::GitCommit { ref phase, .. }) = app.dialog {
        if *phase == CommitPhase::Message {
            let is_ctrl_g = (key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('g'))
                || key.code == KeyCode::Char('\x07');
            if is_ctrl_g {
                handle_git_commit_claude_message(app);
                return;
            }
        }
    }

    // GitCommit message phase: Ctrl+P to commit and push
    if let Some(Dialog::GitCommit { ref phase, .. }) = app.dialog {
        if *phase == CommitPhase::Message {
            let is_ctrl_p = (key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('p'))
                || key.code == KeyCode::Char('\x10');
            if is_ctrl_p {
                handle_git_commit_and_push(app);
                return;
            }
        }
    }

    // GitCommit message phase: Ctrl+L to insert newline
    if let Some(Dialog::GitCommit { ref phase, ref mut commit_message, ref mut cursor_pos, .. }) = app.dialog {
        if *phase == CommitPhase::Message {
            let is_ctrl_l = (key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('l'))
                || key.code == KeyCode::Char('\x0c');
            if is_ctrl_l {
                let byte_pos = commit_message.char_indices()
                    .nth(*cursor_pos)
                    .map(|(i, _)| i)
                    .unwrap_or(commit_message.len());
                commit_message.insert(byte_pos, '\n');
                *cursor_pos += 1;
                return;
            }
        }
    }

    // ── Text input priority ──────────────────────────────────────────
    // Dialogs with text input fields must receive all character keys
    // (including j/k) before navigation handlers can intercept them.
    match key.code {
        KeyCode::Char(c) => {
            if dialog_insert_char(app, c) {
                return;
            }
        }
        KeyCode::Backspace => {
            if dialog_backspace(app) {
                return;
            }
        }
        _ => {}
    }

    match key.code {
        KeyCode::Esc => {
            // GitCommit message phase: go back to staging
            if let Some(Dialog::GitCommit { ref mut phase, .. }) = app.dialog {
                if *phase == CommitPhase::Message {
                    *phase = CommitPhase::Staging;
                    return;
                }
                // GeneratingMessage: cancel generation, go back to message
                if *phase == CommitPhase::GeneratingMessage {
                    *phase = CommitPhase::Message;
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
            // Alt+Enter or Shift+Enter: insert newline in commit message
            if key.modifiers.intersects(KeyModifiers::ALT | KeyModifiers::SHIFT) {
                if let Some(Dialog::GitCommit { ref phase, ref mut commit_message, ref mut cursor_pos, .. }) = app.dialog {
                    if *phase == CommitPhase::Message {
                        let byte_pos = commit_message.char_indices()
                            .nth(*cursor_pos)
                            .map(|(i, _)| i)
                            .unwrap_or(commit_message.len());
                        commit_message.insert(byte_pos, '\n');
                        *cursor_pos += 1;
                        return;
                    }
                }
            }
            let dialog = app.dialog.take();
            match dialog {
                Some(Dialog::InitRepo { url_input, branch_input, .. }) => {
                    let branch = if branch_input.is_empty() { "main".to_string() } else { branch_input };
                    let bare_path = app.bare_repo_path.clone();
                    let tx = app.event_tx.clone();
                    app.set_status(if url_input.is_empty() {
                        format!("Initializing bare repo (branch '{}')...", branch)
                    } else {
                        "Cloning into bare repo...".to_string()
                    });
                    app.close_dialog();

                    // Run in background thread so UI stays responsive
                    let git_lock = std::sync::Arc::clone(&app.git_lock);
                    std::thread::Builder::new()
                        .name("init-repo".into())
                        .spawn(move || {
                            let _guard = git_lock.lock().unwrap_or_else(|e| e.into_inner());
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
                Some(Dialog::ConvertRepo { mode, target_path_input, branch_name, source_repo_path, confirmed, .. }) => {
                    // First Enter press: show warning and ask for confirmation
                    if !confirmed {
                        if mode == 1 && target_path_input.is_empty() {
                            app.set_status("Target path cannot be empty");
                            app.dialog = Some(Dialog::ConvertRepo {
                                mode, target_path_input, branch_name,
                                focused_field: 1, source_repo_path, confirmed: false,
                            });
                            return;
                        }
                        app.dialog = Some(Dialog::ConvertRepo {
                            mode, target_path_input, branch_name,
                            focused_field: 0, source_repo_path, confirmed: true,
                        });
                        return;
                    }

                    // Second Enter press: user confirmed, proceed with conversion
                    let tx = app.event_tx.clone();
                    let branch = branch_name;
                    let git_lock = std::sync::Arc::clone(&app.git_lock);
                    if mode == 0 {
                        // In-place conversion
                        let repo_path = source_repo_path.clone();
                        app.set_status("Converting repo in-place...");
                        app.close_dialog();
                        let bare_path = repo_path.clone();
                        std::thread::Builder::new()
                            .name("convert-repo".into())
                            .spawn(move || {
                                let _guard = git_lock.lock().unwrap_or_else(|e| e.into_inner());
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
                        let target = std::path::PathBuf::from(&target_path_input);
                        let source = source_repo_path;
                        let bare_path = target.clone();
                        app.set_status("Converting repo to new location...");
                        app.close_dialog();
                        std::thread::Builder::new()
                            .name("convert-repo".into())
                            .spawn(move || {
                                let _guard = git_lock.lock().unwrap_or_else(|e| e.into_inner());
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
                        let git_lock = std::sync::Arc::clone(&app.git_lock);
                        std::thread::Builder::new()
                            .name("create-worktree".into())
                            .spawn(move || {
                                let _guard = git_lock.lock().unwrap_or_else(|e| e.into_inner());
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

                        // Queue immediately — all checks run inside execute_pending_action
                        // behind the loading overlay to avoid UI freezes.
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
                            // Commit changes — use queue_action to avoid UI freeze
                            app.close_dialog();
                            app.queue_action("Loading status...", PendingAction::OpenStageCommit { worktree_idx });
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
                        2 => {
                            // Ignore uncommitted changes — proceed with merge
                            if let Some(action) = app.pending_merge.take() {
                                let source_name = app
                                    .worktrees
                                    .get(worktree_idx)
                                    .map(|w| w.branch.clone())
                                    .unwrap_or_default();
                                if let PendingAction::MergeExecute { ref target_branch, .. } = action {
                                    let msg = format!("Merging '{}' into '{}'...", source_name, target_branch);
                                    app.close_dialog();
                                    app.queue_action(msg, action);
                                } else {
                                    app.close_dialog();
                                }
                            } else {
                                app.close_dialog();
                            }
                        }
                        _ => {
                            // Cancel
                            app.pending_merge = None;
                            app.close_dialog();
                        }
                    }
                }
                Some(Dialog::PullError {
                    worktree_idx,
                    error_message,
                    selected,
                }) => {
                    match selected {
                        0 | 1 => {
                            // Claude (0 = normal, 1 = skip perms)
                            let skip_perms = selected == 1;
                            let branch = app.worktrees.get(worktree_idx)
                                .map(|w| w.branch.clone())
                                .unwrap_or_default();
                            let prompt = format!(
                                "A git pull on branch '{}' failed with the following error:\n\n{}\n\nPlease investigate and fix the issue so the pull can succeed.",
                                branch, error_message
                            );
                            let wi = worktree_idx;
                            app.close_dialog();
                            if app.worktrees.get(wi).is_some() {
                                match session::spawn_session(app, wi, terminal_size, skip_perms, Some(&prompt)) {
                                    Ok(sid) => {
                                        app.active_session_id = Some(sid);
                                        app.focus = FocusTarget::TerminalPane;
                                        app.input_mode = InputMode::Terminal;
                                        app.rebuild_sidebar_items();
                                        app.set_status("Claude session opened — fix pull error");
                                    }
                                    Err(e) => {
                                        app.set_status(format!("Failed to spawn Claude: {}", e));
                                    }
                                }
                            }
                        }
                        _ => {
                            // Dismiss
                            app.close_dialog();
                        }
                    }
                }
                Some(Dialog::AuthError { .. }) => {
                    // Only option is Dismiss
                    app.close_dialog();
                }
                Some(Dialog::MergeSuccess { .. }) => {
                    app.close_dialog();
                }
                Some(Dialog::GitCommit {
                    worktree_idx,
                    unstaged,
                    staged,
                    section,
                    selected,
                    phase,
                    commit_message,
                    cursor_pos,
                }) => {
                    match phase {
                        CommitPhase::Staging | CommitPhase::GeneratingMessage => {
                            // Enter does nothing in staging/generating phase — put dialog back
                            app.dialog = Some(Dialog::GitCommit {
                                worktree_idx,
                                unstaged,
                                staged,
                                section,
                                selected,
                                phase,
                                commit_message,
                                cursor_pos,
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
                                    cursor_pos,
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
                Some(Dialog::ConfirmDangerous { input, on_confirm, message }) => {
                    if input.trim().eq_ignore_ascii_case("yes") {
                        match on_confirm {
                            ConfirmAction::DeleteWorktree(path) => {
                                match worktree::remove_worktree(app, &path) {
                                    Ok(_) => {
                                        app.set_status("Worktree removed");
                                    }
                                    Err(e) => {
                                        let msg = format!("{}", e);
                                        if msg.contains("dirty") || msg.contains("untracked") || msg.contains("changes") || msg.contains("submodule") {
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
                                let _ = worktree::refresh_worktrees(app);
                            }
                            ConfirmAction::ForceDeleteWorktree(path) => {
                                match worktree::force_remove_worktree(app, &path) {
                                    Ok(_) => {
                                        app.set_status("Worktree force-removed");
                                    }
                                    Err(e) => {
                                        app.set_status(format!("Error: {}", e));
                                    }
                                }
                                let _ = worktree::refresh_worktrees(app);
                            }
                            _ => {}
                        }
                        app.close_dialog();
                    } else {
                        // Put the dialog back — user hasn't typed "yes" yet
                        app.dialog = Some(Dialog::ConfirmDangerous { input, on_confirm, message });
                    }
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
                Some(Dialog::PullError { ref mut selected, .. }) => {
                    if *selected > 0 {
                        *selected -= 1;
                    }
                }
                Some(Dialog::AuthError { ref mut selected, .. }) => {
                    if *selected > 0 {
                        *selected -= 1;
                    }
                }
                Some(Dialog::GitCommit { ref phase, ref mut selected, ref unstaged, ref staged, ref section, ref commit_message, ref mut cursor_pos, .. }) => {
                    if *phase == CommitPhase::Staging {
                        if *selected > 0 {
                            *selected -= 1;
                        } else if *section == 0 && !unstaged.is_empty() {
                            // already at top of unstaged
                        } else if *section == 1 && !staged.is_empty() {
                            // already at top of staged
                        }
                    } else if *phase == CommitPhase::Message {
                        *cursor_pos = commit_msg_cursor_up(commit_message, *cursor_pos);
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
                Some(Dialog::PullError { ref mut selected, .. }) => {
                    if *selected + 1 < crate::app::PULL_ERROR_OPTION_COUNT {
                        *selected += 1;
                    }
                }
                Some(Dialog::AuthError { ref mut selected, .. }) => {
                    if *selected + 1 < crate::app::AUTH_ERROR_OPTION_COUNT {
                        *selected += 1;
                    }
                }
                Some(Dialog::GitCommit { ref phase, ref mut selected, ref unstaged, ref staged, ref section, ref commit_message, ref mut cursor_pos, .. }) => {
                    if *phase == CommitPhase::Staging {
                        let len = if *section == 0 { unstaged.len() } else { staged.len() };
                        if len > 0 && *selected + 1 < len {
                            *selected += 1;
                        }
                    } else if *phase == CommitPhase::Message {
                        *cursor_pos = commit_msg_cursor_down(commit_message, *cursor_pos);
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
                Some(Dialog::GitCommit { ref phase, ref mut commit_message, ref mut cursor_pos, .. }) => {
                    if *phase == CommitPhase::Message {
                        let byte_pos = commit_message.char_indices()
                            .nth(*cursor_pos)
                            .map(|(i, _)| i)
                            .unwrap_or(commit_message.len());
                        commit_message.insert(byte_pos, c);
                        *cursor_pos += 1;
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
                Some(Dialog::GitCommit { ref phase, ref mut commit_message, ref mut cursor_pos, .. }) => {
                    if *phase == CommitPhase::Message && *cursor_pos > 0 {
                        let byte_pos = commit_message.char_indices()
                            .nth(*cursor_pos - 1)
                            .map(|(i, _)| i)
                            .unwrap_or(0);
                        commit_message.remove(byte_pos);
                        *cursor_pos -= 1;
                    }
                }
                _ => {}
            }
        }
        KeyCode::Left => {
            match &mut app.dialog {
                Some(Dialog::ConvertRepo { ref mut mode, ref mut focused_field, .. }) => {
                    if *focused_field == 0 {
                        *mode = if *mode == 0 { 1 } else { 0 };
                    }
                }
                Some(Dialog::GitCommit { ref phase, ref mut cursor_pos, .. }) => {
                    if *phase == CommitPhase::Message {
                        *cursor_pos = cursor_pos.saturating_sub(1);
                    }
                }
                _ => {}
            }
        }
        KeyCode::Right => {
            match &mut app.dialog {
                Some(Dialog::ConvertRepo { ref mut mode, ref mut focused_field, .. }) => {
                    if *focused_field == 0 {
                        *mode = if *mode == 0 { 1 } else { 0 };
                    }
                }
                Some(Dialog::GitCommit { ref phase, ref commit_message, ref mut cursor_pos, .. }) => {
                    if *phase == CommitPhase::Message {
                        let char_count = commit_message.chars().count();
                        *cursor_pos = (*cursor_pos + 1).min(char_count);
                    }
                }
                _ => {}
            }
        }
        KeyCode::Home => {
            if let Some(Dialog::GitCommit { ref phase, ref commit_message, ref mut cursor_pos, .. }) = app.dialog {
                if *phase == CommitPhase::Message {
                    *cursor_pos = commit_msg_line_start(commit_message, *cursor_pos);
                }
            }
        }
        KeyCode::End => {
            if let Some(Dialog::GitCommit { ref phase, ref commit_message, ref mut cursor_pos, .. }) = app.dialog {
                if *phase == CommitPhase::Message {
                    *cursor_pos = commit_msg_line_end(commit_message, *cursor_pos);
                }
            }
        }
        KeyCode::Delete => {
            if let Some(Dialog::GitCommit { ref phase, ref mut commit_message, ref cursor_pos, .. }) = app.dialog {
                if *phase == CommitPhase::Message {
                    let char_count = commit_message.chars().count();
                    if *cursor_pos < char_count {
                        let byte_pos = commit_message.char_indices()
                            .nth(*cursor_pos)
                            .map(|(i, _)| i)
                            .unwrap_or(commit_message.len());
                        commit_message.remove(byte_pos);
                    }
                }
            }
        }
        _ => {}
    }
}

/// Try to insert a character into the current dialog's text input field.
/// Returns true if the character was consumed (dialog has active text input).
fn dialog_insert_char(app: &mut App, c: char) -> bool {
    match &mut app.dialog {
        Some(Dialog::CreateWorktree { ref mut branch_input, ref mut base_branch, focused_field, .. }) => {
            match *focused_field {
                0 => branch_input.push(c),
                _ => base_branch.push(c),
            }
            true
        }
        Some(Dialog::InitRepo { ref mut url_input, ref mut branch_input, focused_field, .. }) => {
            match *focused_field {
                0 => url_input.push(c),
                _ => branch_input.push(c),
            }
            true
        }
        Some(Dialog::ConvertRepo { ref mut target_path_input, ref mut branch_name, focused_field, .. }) => {
            match *focused_field {
                1 => { target_path_input.push(c); true }
                2 => { branch_name.push(c); true }
                _ => false // field 0 is mode selector, no char input
            }
        }
        Some(Dialog::RenameSession { ref mut input, .. }) => {
            input.push(c);
            true
        }
        Some(Dialog::ConfirmDangerous { ref mut input, .. }) => {
            input.push(c);
            true
        }
        Some(Dialog::GitCommit { ref phase, ref mut commit_message, ref mut cursor_pos, .. }) => {
            if *phase == CommitPhase::Message && !c.is_control() {
                let byte_pos = commit_message.char_indices()
                    .nth(*cursor_pos)
                    .map(|(i, _)| i)
                    .unwrap_or(commit_message.len());
                commit_message.insert(byte_pos, c);
                *cursor_pos += 1;
                true
            } else {
                false
            }
        }
        _ => false
    }
}

/// Try to delete a character from the current dialog's text input field.
/// Returns true if the backspace was consumed (dialog has active text input).
fn dialog_backspace(app: &mut App) -> bool {
    match &mut app.dialog {
        Some(Dialog::CreateWorktree { ref mut branch_input, ref mut base_branch, focused_field, .. }) => {
            match *focused_field {
                0 => { branch_input.pop(); }
                _ => { base_branch.pop(); }
            }
            true
        }
        Some(Dialog::InitRepo { ref mut url_input, ref mut branch_input, focused_field, .. }) => {
            match *focused_field {
                0 => { url_input.pop(); }
                _ => { branch_input.pop(); }
            }
            true
        }
        Some(Dialog::ConvertRepo { ref mut target_path_input, ref mut branch_name, focused_field, .. }) => {
            match *focused_field {
                1 => { target_path_input.pop(); true }
                2 => { branch_name.pop(); true }
                _ => false
            }
        }
        Some(Dialog::RenameSession { ref mut input, .. }) => {
            input.pop();
            true
        }
        Some(Dialog::ConfirmDangerous { ref mut input, .. }) => {
            input.pop();
            true
        }
        Some(Dialog::GitCommit { ref phase, ref mut commit_message, ref mut cursor_pos, .. }) => {
            if *phase == CommitPhase::Message && *cursor_pos > 0 {
                let byte_pos = commit_message.char_indices()
                    .nth(*cursor_pos - 1)
                    .map(|(i, _)| i)
                    .unwrap_or(0);
                commit_message.remove(byte_pos);
                *cursor_pos -= 1;
                true
            } else {
                false
            }
        }
        _ => false
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

    app.queue_action("Staging all...", PendingAction::StageAll { worktree_idx, then_claude: false });
}

/// Move cursor up one line in a multi-line string, preserving column.
fn commit_msg_cursor_up(text: &str, cursor_pos: usize) -> usize {
    let chars: Vec<char> = text.chars().collect();
    // Find start of current line
    let mut current_line_start = cursor_pos;
    while current_line_start > 0 && chars[current_line_start - 1] != '\n' {
        current_line_start -= 1;
    }
    if current_line_start == 0 {
        return 0; // already on first line, go to start
    }
    let col = cursor_pos - current_line_start;
    // Find start of previous line
    let mut prev_line_start = current_line_start - 1; // skip the \n
    while prev_line_start > 0 && chars[prev_line_start - 1] != '\n' {
        prev_line_start -= 1;
    }
    let prev_line_len = current_line_start - 1 - prev_line_start;
    prev_line_start + col.min(prev_line_len)
}

/// Move cursor down one line in a multi-line string, preserving column.
fn commit_msg_cursor_down(text: &str, cursor_pos: usize) -> usize {
    let chars: Vec<char> = text.chars().collect();
    let len = chars.len();
    // Find start of current line
    let mut current_line_start = cursor_pos;
    while current_line_start > 0 && chars[current_line_start - 1] != '\n' {
        current_line_start -= 1;
    }
    let col = cursor_pos - current_line_start;
    // Find end of current line (next \n)
    let mut next_newline = cursor_pos;
    while next_newline < len && chars[next_newline] != '\n' {
        next_newline += 1;
    }
    if next_newline >= len {
        return len; // no next line, go to end
    }
    let next_line_start = next_newline + 1;
    // Find length of next line
    let mut next_line_end = next_line_start;
    while next_line_end < len && chars[next_line_end] != '\n' {
        next_line_end += 1;
    }
    let next_line_len = next_line_end - next_line_start;
    next_line_start + col.min(next_line_len)
}

/// Move cursor to start of current line.
fn commit_msg_line_start(text: &str, cursor_pos: usize) -> usize {
    let chars: Vec<char> = text.chars().collect();
    let mut pos = cursor_pos;
    while pos > 0 && chars[pos - 1] != '\n' {
        pos -= 1;
    }
    pos
}

/// Move cursor to end of current line.
fn commit_msg_line_end(text: &str, cursor_pos: usize) -> usize {
    let chars: Vec<char> = text.chars().collect();
    let mut pos = cursor_pos;
    while pos < chars.len() && chars[pos] != '\n' {
        pos += 1;
    }
    pos
}

/// Strip markdown code fences, Co-Authored-By/Signed-off-by trailers,
/// and other artifacts from AI-generated commit messages.
fn clean_commit_message(msg: &str) -> String {
    let mut s = msg.trim().to_string();
    // Strip leading markdown code fence (```commit, ```text, ```, etc.)
    if s.starts_with("```") {
        if let Some(nl) = s.find('\n') {
            s = s[nl + 1..].to_string();
        } else {
            s = s[3..].to_string();
        }
    }
    // Strip trailing markdown code fence
    if s.ends_with("```") {
        s = s[..s.len() - 3].to_string();
    }
    // Remove Co-Authored-By and Signed-off-by trailer lines
    let lines: Vec<&str> = s.lines().collect();
    let mut result: Vec<&str> = Vec::new();
    for line in &lines {
        let trimmed = line.trim_start();
        if trimmed.starts_with("Co-Authored-By:") || trimmed.starts_with("Co-authored-by:")
            || trimmed.starts_with("Signed-off-by:") || trimmed.starts_with("Signed-Off-By:")
        {
            continue;
        }
        result.push(line);
    }
    // Remove trailing blank lines
    while result.last().is_some_and(|l| l.trim().is_empty()) {
        result.pop();
    }
    result.join("\n")
}

/// Commit with the current message and push to remote.
fn handle_git_commit_and_push(app: &mut App) {
    let (worktree_idx, commit_message) = match &app.dialog {
        Some(Dialog::GitCommit { worktree_idx, commit_message, phase, .. })
            if *phase == CommitPhase::Message => (*worktree_idx, commit_message.clone()),
        _ => return,
    };
    if commit_message.is_empty() {
        app.set_status("Commit message cannot be empty");
        return;
    }
    app.close_dialog();
    app.queue_action("Committing & pushing...", PendingAction::CommitAndPush {
        worktree_idx,
        message: commit_message,
    });
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

/// Generate a commit message using Claude for the staged diff.
pub fn handle_git_commit_claude_message(app: &mut App) {
    let (worktree_idx, staged_empty, already_generating) = match &app.dialog {
        Some(Dialog::GitCommit { worktree_idx, staged, phase, .. }) => {
            (*worktree_idx, staged.is_empty(), *phase == CommitPhase::GeneratingMessage)
        }
        _ => return,
    };

    if staged_empty {
        app.set_status("No staged files — stage files first");
        return;
    }
    if already_generating {
        return;
    }

    // Get staged diff
    let diff = match worktree::diff_staged(app, worktree_idx) {
        Ok(d) => d,
        Err(e) => {
            app.set_status(format!("Failed to get diff: {}", e));
            return;
        }
    };

    if diff.trim().is_empty() {
        app.set_status("Staged diff is empty");
        return;
    }

    // Truncate to ~8000 chars to avoid overwhelming the model
    let diff_truncated = if diff.len() > 8000 {
        format!("{}...\n[truncated]", &diff[..8000])
    } else {
        diff
    };

    // Set phase to generating
    if let Some(Dialog::GitCommit { ref mut phase, .. }) = app.dialog {
        *phase = CommitPhase::GeneratingMessage;
    }

    let tx = app.event_tx.clone();
    std::thread::Builder::new()
        .name("claude-commit-msg".into())
        .spawn(move || {
            use std::io::Write;
            let mut child = match std::process::Command::new("claude")
                .args([
                    "-p",
                    "--model", "haiku",
                    "--no-session-persistence",
                    "--system-prompt",
                    "Your entire response must be a git commit message and nothing else. No preamble, no analysis. Focus on WHY the change was made. Imperative mood summary (max 72 chars), then a blank line and concise bullet points (- prefix) explaining the motivation. No markdown fences, no Co-Authored-By.",
                ])
                .env("CLAUDECODE", "")
                .env("CLAUDE_CODE_ENTRYPOINT", "")
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .spawn()
            {
                Ok(child) => child,
                Err(e) => {
                    let _ = tx.send(crate::event::AppEvent::ClaudeCommitMessageReady {
                        worktree_idx,
                        message: Err(format!("Failed to run claude: {}", e)),
                    });
                    return;
                }
            };

            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(b"Write a commit message for this diff:\n\n");
                let _ = stdin.write_all(diff_truncated.as_bytes());
            }

            let output = match child.wait_with_output() {
                Ok(out) => out,
                Err(e) => {
                    let _ = tx.send(crate::event::AppEvent::ClaudeCommitMessageReady {
                        worktree_idx,
                        message: Err(format!("claude process error: {}", e)),
                    });
                    return;
                }
            };

            if output.status.success() {
                let raw = String::from_utf8_lossy(&output.stdout).trim().to_string();
                let msg = clean_commit_message(&raw);
                if msg.is_empty() {
                    let _ = tx.send(crate::event::AppEvent::ClaudeCommitMessageReady {
                        worktree_idx,
                        message: Err("Claude returned empty message".to_string()),
                    });
                } else {
                    let _ = tx.send(crate::event::AppEvent::ClaudeCommitMessageReady {
                        worktree_idx,
                        message: Ok(msg),
                    });
                }
            } else {
                let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                let _ = tx.send(crate::event::AppEvent::ClaudeCommitMessageReady {
                    worktree_idx,
                    message: Err(format!("claude failed: {}", if stderr.is_empty() { "unknown error".to_string() } else { stderr })),
                });
            }
        })
        .ok();
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
                Some(SidebarItem::Terminal(_)) | None => 0,
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
                Some(SidebarItem::Terminal(_)) | None => {}
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
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);

    // xterm modifier parameter: 1 + (shift?1:0) + (alt?2:0) + (ctrl?4:0)
    // Used for modified arrow keys, Home/End, etc.  0 means no modifiers.
    let modifier: u8 = {
        let m = if shift { 1u8 } else { 0 }
              + if alt   { 2 }   else { 0 }
              + if ctrl  { 4 }   else { 0 };
        if m > 0 { m + 1 } else { 0 }
    };

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
        KeyCode::Up    => modified_csi_key(b'A', modifier, app_cursor),
        KeyCode::Down  => modified_csi_key(b'B', modifier, app_cursor),
        KeyCode::Right => modified_csi_key(b'C', modifier, app_cursor),
        KeyCode::Left  => modified_csi_key(b'D', modifier, app_cursor),
        KeyCode::Home  => modified_csi_key(b'H', modifier, false),
        KeyCode::End   => modified_csi_key(b'F', modifier, false),
        KeyCode::PageUp   => modified_tilde_key(5, modifier),
        KeyCode::PageDown => modified_tilde_key(6, modifier),
        KeyCode::Insert   => modified_tilde_key(2, modifier),
        KeyCode::Delete   => modified_tilde_key(3, modifier),
        KeyCode::F(n) => f_key_bytes(n, modifier),
        _ => vec![],
    }
}

/// Generate a CSI-style key sequence with optional modifier.
///   No modifier:           \x1b[{final}  or  \x1bO{final} (app cursor mode)
///   With modifier (2..=8): \x1b[1;{mod}{final}
fn modified_csi_key(final_byte: u8, modifier: u8, app_cursor: bool) -> Vec<u8> {
    if modifier > 0 {
        vec![0x1b, b'[', b'1', b';', modifier + b'0', final_byte]
    } else if app_cursor {
        vec![0x1b, b'O', final_byte]
    } else {
        vec![0x1b, b'[', final_byte]
    }
}

/// Generate a tilde-style key sequence with optional modifier.
///   No modifier:           \x1b[{n}~
///   With modifier (2..=8): \x1b[{n};{mod}~
fn modified_tilde_key(n: u8, modifier: u8) -> Vec<u8> {
    if modifier > 0 {
        if n >= 10 {
            vec![0x1b, b'[', (n / 10) + b'0', (n % 10) + b'0', b';', modifier + b'0', b'~']
        } else {
            vec![0x1b, b'[', n + b'0', b';', modifier + b'0', b'~']
        }
    } else if n >= 10 {
        vec![0x1b, b'[', (n / 10) + b'0', (n % 10) + b'0', b'~']
    } else {
        vec![0x1b, b'[', n + b'0', b'~']
    }
}

fn f_key_bytes(n: u8, modifier: u8) -> Vec<u8> {
    // F1-F4 use SS3 (unmodified) or CSI 1;mod P/Q/R/S (modified)
    match n {
        1..=4 => {
            let final_byte = b'P' + n - 1;
            if modifier > 0 {
                vec![0x1b, b'[', b'1', b';', modifier + b'0', final_byte]
            } else {
                vec![0x1b, b'O', final_byte]
            }
        }
        // F5-F12 use tilde encoding
        5  => modified_tilde_key(15, modifier),
        6  => modified_tilde_key(17, modifier),
        7  => modified_tilde_key(18, modifier),
        8  => modified_tilde_key(19, modifier),
        9  => modified_tilde_key(20, modifier),
        10 => modified_tilde_key(21, modifier),
        11 => modified_tilde_key(23, modifier),
        12 => modified_tilde_key(24, modifier),
        _ => vec![],
    }
}

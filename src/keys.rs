use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::app::{App, CommitPhase, ConfirmAction, Dialog, FocusTarget, InputMode, PendingAction, SidebarItem};
use crate::session;
use crate::worktree;

/// Handle a key event based on current input mode.
pub fn handle_key(app: &mut App, key: KeyEvent, terminal_size: (u16, u16)) {
    // ── Global keybindings (work in ALL modes) ─────────────────────
    if key.modifiers == KeyModifiers::CONTROL && key.code == KeyCode::Char('q') {
        app.should_quit = true;
        return;
    }
    if key.modifiers == KeyModifiers::CONTROL && key.code == KeyCode::Char('b') {
        app.sidebar_visible = !app.sidebar_visible;
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

    match app.input_mode {
        InputMode::Normal => handle_normal_key(app, key, terminal_size),
        InputMode::Terminal => handle_terminal_key(app, key),
        InputMode::Dialog => handle_dialog_key(app, key, terminal_size),
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

    // If no repo detected, only allow init
    if !app.repo_detected {
        match key.code {
            KeyCode::Char('i') => {
                app.open_dialog(Dialog::InitRepo {
                    url_input: String::new(),
                    branch_input: "main".to_string(),
                    focused_field: 0,
                });
            }
            _ => {}
        }
        return;
    }

    match (key.modifiers, key.code) {
        (_, KeyCode::Tab) => {
            app.toggle_focus();
        }
        (_, KeyCode::F(5)) => {
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
        (_, KeyCode::Enter) => app.activate_selected(),
        (_, KeyCode::Char(' ')) => app.toggle_expand(),

        // c — new Claude session
        (_, KeyCode::Char('c')) => {
            spawn_claude_for_selected(app, terminal_size, false);
        }
        // C — new Claude session with --dangerously-skip-permissions
        (KeyModifiers::SHIFT, KeyCode::Char('C')) => {
            spawn_claude_for_selected(app, terminal_size, true);
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

        // w — open new Windows Terminal tab in worktree directory
        (_, KeyCode::Char('w')) => {
            open_wsl_window(app, false);
        }
        // W — open new Windows Terminal tab with claude in worktree directory
        (KeyModifiers::SHIFT, KeyCode::Char('W')) => {
            open_wsl_window(app, true);
        }

        // q — toggle prompt queue panel
        (_, KeyCode::Char('q')) => {
            if app.active_session_id.is_some() {
                app.prompt_queue_visible = !app.prompt_queue_visible;
                // Trigger PTY resize since available height changed
                let size = terminal_size;
                session::resize_all(app, size.1, size.0);
            }
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
        // Space — stage/unstage selected file
        KeyCode::Char(' ') => {
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
        // Enter — commit staged files (enter commit message mode)
        KeyCode::Enter => {
            if staged.is_empty() {
                app.set_status("No staged files to commit");
            } else {
                app.info_panel_commit_msg = Some(String::new());
            }
        }
        // Pass through keys that should still work from the info panel
        KeyCode::Char('c') => {
            spawn_claude_for_selected(app, terminal_size, false);
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
                        "Remove worktree '{}' and kill {} session(s)?",
                        wt.branch,
                        wt.session_ids.len()
                    )
                } else {
                    format!("Remove worktree '{}'?", wt.branch)
                };
                app.open_dialog(Dialog::Confirm {
                    message: msg,
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
            app.open_dialog(Dialog::Confirm {
                message: format!(
                    "FORCE remove worktree '{}' (even if dirty)?",
                    wt.branch
                ),
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
        KeyCode::Esc => {
            if app.prompt_queue_editing.is_some() {
                // Cancel editing
                app.prompt_queue_editing = None;
                app.prompt_queue_input.clear();
            } else {
                // Go back to terminal pane
                app.focus = FocusTarget::TerminalPane;
                app.input_mode = InputMode::Terminal;
            }
        }
        KeyCode::Tab => {
            // Escape to sidebar
            app.focus = FocusTarget::Sidebar;
            app.input_mode = InputMode::Normal;
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
            } else if !app.prompt_queue_input.is_empty() {
                // Add new item to queue
                let input = app.prompt_queue_input.drain(..).collect::<String>();
                app.prompt_queues.entry(sid).or_default().push(input);
                app.save_prompt_queues();
            } else {
                // Input empty + item selected → load for editing
                let queue_len = app.active_prompt_queue().len();
                if queue_len > 0 && app.prompt_queue_selected < queue_len {
                    let text = app.active_prompt_queue()[app.prompt_queue_selected].clone();
                    app.prompt_queue_input = text;
                    app.prompt_queue_editing = Some(app.prompt_queue_selected);
                }
            }
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
            app.prompt_queue_input.pop();
        }
        KeyCode::Char(c) if c != '\n' && c != '\r' => {
            app.prompt_queue_input.push(c);
        }
        _ => {}
    }

    // Suppress unused warning for terminal_size — we accept it for consistency with other handlers
    let _ = terminal_size;
}

fn handle_terminal_key(app: &mut App, key: KeyEvent) {
    // Tab: go to PromptQueue if visible, else escape to sidebar
    if key.code == KeyCode::Tab && key.modifiers.is_empty() {
        app.terminal_scroll = 0;
        if app.prompt_queue_visible && app.active_session_id.is_some() {
            app.focus = FocusTarget::PromptQueue;
            app.input_mode = InputMode::Normal;
        } else {
            app.escape_to_sidebar();
        }
        return;
    }

    // PgUp / PgDown scroll through history
    match key.code {
        KeyCode::PageUp => {
            app.terminal_scroll = app.terminal_scroll.saturating_add(SCROLL_PAGE);
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

const SCROLL_LINES: usize = 3;
const SCROLL_PAGE: usize = 20;

/// Handle mouse wheel scroll events. Works regardless of focus.
pub fn handle_scroll(app: &mut App, up: bool) {
    if app.active_session_id.is_none() {
        return;
    }
    if up {
        app.terminal_scroll = app.terminal_scroll.saturating_add(SCROLL_LINES);
    } else {
        app.terminal_scroll = app.terminal_scroll.saturating_sub(SCROLL_LINES);
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
                        ConfirmAction::DeleteWorktree(path) => {
                            match worktree::remove_worktree(app, &path) {
                                Ok(_) => {
                                    app.set_status("Worktree removed");
                                    let _ = worktree::refresh_worktrees(app);
                                }
                                Err(e) => {
                                    let msg = format!("{}", e);
                                    if msg.contains("dirty") || msg.contains("untracked") || msg.contains("changes") {
                                        // Offer force-delete
                                        app.open_dialog(Dialog::Confirm {
                                            message: format!("Worktree is dirty. Force remove?"),
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
                    }
                    app.close_dialog();
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
                Some(Dialog::RenameSession { ref mut input, .. }) => {
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
                Some(Dialog::RenameSession { ref mut input, .. }) => {
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

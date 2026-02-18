use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::app::{App, ConfirmAction, Dialog, FocusTarget, InputMode, SidebarItem};
use crate::session;
use crate::worktree;

/// Handle a key event based on current input mode.
pub fn handle_key(app: &mut App, key: KeyEvent, terminal_size: (u16, u16)) {
    // ── Global keybindings (work in ALL modes) ─────────────────────
    // Ctrl+q — quit
    if key.modifiers == KeyModifiers::CONTROL && key.code == KeyCode::Char('q') {
        app.should_quit = true;
        return;
    }

    // Ctrl+b — toggle sidebar visibility (works from any mode)
    if key.modifiers == KeyModifiers::CONTROL && key.code == KeyCode::Char('b') {
        app.sidebar_visible = !app.sidebar_visible;
        return;
    }

    match app.input_mode {
        InputMode::Normal => handle_normal_key(app, key, terminal_size),
        InputMode::Terminal => handle_terminal_key(app, key),
        InputMode::Dialog => handle_dialog_key(app, key),
    }
}

fn handle_normal_key(app: &mut App, key: KeyEvent, terminal_size: (u16, u16)) {
    match (key.modifiers, key.code) {
        (_, KeyCode::Tab) => {
            app.toggle_focus();
        }
        (_, KeyCode::F(5)) => {
            if let Err(e) = worktree::refresh_worktrees(app) {
                app.set_status(format!("Error refreshing: {}", e));
            }
        }

        // Sidebar navigation
        (_, KeyCode::Char('j')) | (_, KeyCode::Down) => {
            app.sidebar_down();
        }
        (_, KeyCode::Char('k')) | (_, KeyCode::Up) => {
            app.sidebar_up();
        }
        (_, KeyCode::Enter) => {
            app.activate_selected();
        }
        (_, KeyCode::Char(' ')) => {
            app.toggle_expand();
        }

        // c — new Claude session (normal mode)
        (_, KeyCode::Char('c')) => {
            spawn_claude_for_selected(app, terminal_size, false);
        }
        // C (shift+c) — new Claude session with --dangerously-skip-permissions
        (KeyModifiers::SHIFT, KeyCode::Char('C')) => {
            spawn_claude_for_selected(app, terminal_size, true);
        }

        (_, KeyCode::Char('n')) => {
            app.open_dialog(Dialog::CreateWorktree {
                branch_input: String::new(),
                path_input: String::new(),
                focused_field: 0,
            });
        }
        (_, KeyCode::Char('d')) => {
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
                                "Delete worktree '{}' and kill {} running session(s)?",
                                wt.branch,
                                wt.session_ids.len()
                            )
                        } else {
                            format!("Delete worktree '{}'?", wt.branch)
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

        _ => {}
    }
}

fn spawn_claude_for_selected(app: &mut App, terminal_size: (u16, u16), skip_permissions: bool) {
    let wt_idx = match app.selected_sidebar_item() {
        Some(SidebarItem::Worktree(wi)) => Some(wi),
        Some(SidebarItem::Session(wi, _)) => Some(wi),
        None => None,
    };
    if let Some(wi) = wt_idx {
        match session::spawn_session(app, wi, terminal_size, skip_permissions) {
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

fn handle_terminal_key(app: &mut App, key: KeyEvent) {
    // Escape — return to sidebar
    if key.code == KeyCode::Esc && key.modifiers.is_empty() {
        app.escape_to_sidebar();
        return;
    }

    // Tab — toggle focus back to sidebar
    if key.code == KeyCode::Tab && key.modifiers.is_empty() {
        app.escape_to_sidebar();
        return;
    }

    // Pass key to active session's PTY
    if let Some(sid) = app.active_session_id {
        if let Some(session) = app.sessions.get(&sid) {
            let bytes = key_to_bytes(key, session.application_cursor_mode());
            if !bytes.is_empty() {
                let _ = session.write_tx.send(bytes::Bytes::from(bytes));
            }
        }
    }
}

fn handle_dialog_key(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Esc => {
            app.close_dialog();
        }
        KeyCode::Enter => {
            let dialog = app.dialog.take();
            match dialog {
                Some(Dialog::CreateWorktree {
                    branch_input,
                    path_input,
                    ..
                }) => {
                    if !branch_input.is_empty() {
                        let path = if path_input.is_empty() {
                            branch_input.clone()
                        } else {
                            path_input
                        };
                        match worktree::create_worktree(app, &branch_input, &path) {
                            Ok(_) => {
                                app.set_status(format!("Created worktree '{}'", branch_input));
                                let _ = worktree::refresh_worktrees(app);
                            }
                            Err(e) => {
                                app.set_status(format!("Error: {}", e));
                            }
                        }
                    }
                    app.close_dialog();
                }
                Some(Dialog::Confirm { on_confirm, .. }) => {
                    match on_confirm {
                        ConfirmAction::DeleteSession(sid) => {
                            session::kill_session(app, sid);
                        }
                        ConfirmAction::DeleteWorktree(path) => {
                            match worktree::remove_worktree(app, &path) {
                                Ok(_) => {
                                    app.set_status("Worktree removed".to_string());
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
        KeyCode::Tab => {
            if let Some(Dialog::CreateWorktree {
                ref mut focused_field,
                ..
            }) = app.dialog
            {
                *focused_field = (*focused_field + 1) % 2;
            }
        }
        KeyCode::BackTab => {
            if let Some(Dialog::CreateWorktree {
                ref mut focused_field,
                ..
            }) = app.dialog
            {
                *focused_field = if *focused_field == 0 { 1 } else { 0 };
            }
        }
        KeyCode::Char(c) => {
            if let Some(Dialog::CreateWorktree {
                ref mut branch_input,
                ref mut path_input,
                focused_field,
                ..
            }) = app.dialog
            {
                match focused_field {
                    0 => branch_input.push(c),
                    1 => path_input.push(c),
                    _ => {}
                }
            }
        }
        KeyCode::Backspace => {
            if let Some(Dialog::CreateWorktree {
                ref mut branch_input,
                ref mut path_input,
                focused_field,
                ..
            }) = app.dialog
            {
                match focused_field {
                    0 => {
                        branch_input.pop();
                    }
                    1 => {
                        path_input.pop();
                    }
                    _ => {}
                }
            }
        }
        _ => {}
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
            if app_cursor {
                vec![0x1b, b'O', b'A']
            } else {
                vec![0x1b, b'[', b'A']
            }
        }
        KeyCode::Down => {
            if app_cursor {
                vec![0x1b, b'O', b'B']
            } else {
                vec![0x1b, b'[', b'B']
            }
        }
        KeyCode::Right => {
            if app_cursor {
                vec![0x1b, b'O', b'C']
            } else {
                vec![0x1b, b'[', b'C']
            }
        }
        KeyCode::Left => {
            if app_cursor {
                vec![0x1b, b'O', b'D']
            } else {
                vec![0x1b, b'[', b'D']
            }
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

pub mod pty;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

use crate::app::App;

/// A Claude Code session running in a PTY.
pub struct Session {
    pub id: u64,
    pub worktree_path: PathBuf,
    pub label: String,
    pub parser: Arc<RwLock<pty::ClaudeParser>>,
    pub write_tx: mpsc::UnboundedSender<bytes::Bytes>,
    pub master_pty: Box<dyn portable_pty::MasterPty + Send>,
    pub exited: Arc<AtomicBool>,
    pub last_output: Arc<RwLock<Instant>>,
    /// If backed by tmux, the tmux session name for reconnection.
    pub tmux_session_name: Option<String>,
    /// User-assigned nickname, displayed instead of terminal title when set.
    pub nickname: Option<String>,
}

impl Session {
    /// Check if the PTY is in application cursor mode.
    pub fn application_cursor_mode(&self) -> bool {
        match self.parser.try_read() {
            Ok(guard) => guard.screen().application_cursor(),
            Err(_) => false,
        }
    }

    /// Get the terminal title set by Claude Code (e.g. task description).
    pub fn terminal_title(&self) -> Option<String> {
        match self.parser.try_read() {
            Ok(guard) => {
                let title = &guard.callbacks().title;
                if title.is_empty() {
                    None
                } else {
                    Some(title.clone())
                }
            }
            Err(_) => None,
        }
    }

    /// Returns true if Claude is actively working, detected by the braille
    /// dot spinner characters (⠂ U+2802 / ⠐ U+2810) that Claude Code sets
    /// in the terminal title while processing.
    pub fn is_active(&self) -> bool {
        if self.exited.load(Ordering::Relaxed) {
            return false;
        }
        match self.parser.try_read() {
            Ok(guard) => {
                let title = &guard.callbacks().title;
                title.starts_with('⠂') || title.starts_with('⠐')
            }
            Err(_) => false,
        }
    }
}

/// Spawn a new Claude session in the given worktree.
/// If `initial_prompt` is Some, it is passed as a CLI argument so Claude starts working on it immediately.
pub fn spawn_session(app: &mut App, worktree_idx: usize, terminal_size: (u16, u16), skip_permissions: bool, initial_prompt: Option<&str>) -> anyhow::Result<u64> {
    let wt = app.worktrees.get(worktree_idx)
        .ok_or_else(|| anyhow::anyhow!("Invalid worktree index"))?;

    let session_id = app.next_session_id;
    app.next_session_id += 1;

    let pane_pct = if app.sidebar_visible { 0.7 } else { 1.0 };
    let cols = (terminal_size.0 as f32 * pane_pct) as u16 - 2;
    let rows = terminal_size.1 - 3;

    let handle = if app.tmux_available {
        let tmux_name = pty::tmux_session_name(&wt.branch, session_id);
        pty::spawn_claude_pty_tmux(
            &wt.path,
            session_id,
            app.event_tx.clone(),
            rows,
            cols,
            skip_permissions,
            &tmux_name,
            initial_prompt,
        )?
    } else {
        pty::spawn_claude_pty(
            &wt.path,
            session_id,
            app.event_tx.clone(),
            rows,
            cols,
            skip_permissions,
            initial_prompt,
        )?
    };

    let label = "Initializing...".to_string();

    let session = Session {
        id: session_id,
        worktree_path: wt.path.clone(),
        label,
        parser: handle.parser,
        write_tx: handle.write_tx,
        master_pty: handle.master_pty,
        exited: handle.exited,
        last_output: handle.last_output,
        tmux_session_name: handle.tmux_session_name,
        nickname: None,
    };

    app.sessions.insert(session_id, session);

    if let Some(wt) = app.worktrees.get_mut(worktree_idx) {
        wt.session_ids.push(session_id);
        wt.expanded = true;
    }

    Ok(session_id)
}

/// Kill a session and clean up. Also kills the tmux session if applicable.
pub fn kill_session(app: &mut App, session_id: u64) {
    if let Some(session) = app.sessions.get(&session_id) {
        // Kill the tmux session if present
        if let Some(ref tmux_name) = session.tmux_session_name {
            let _ = std::process::Command::new("tmux")
                .args(["kill-session", "-t", tmux_name])
                .output();
        }
    }

    app.sessions.remove(&session_id);

    for wt in &mut app.worktrees {
        wt.session_ids.retain(|&id| id != session_id);
    }

    if app.active_session_id == Some(session_id) {
        app.active_session_id = None;
        app.escape_to_sidebar();
    }

    app.rebuild_sidebar_items();
}

/// Get a session label for display.
pub fn session_label(app: &App, session_id: u64) -> String {
    app.sessions
        .get(&session_id)
        .map(|s| s.label.clone())
        .unwrap_or_else(|| format!("session-{}", session_id))
}

/// Mark a session as exited and rebuild sidebar to update counts.
pub fn mark_exited(app: &mut App, session_id: u64) {
    if let Some(session) = app.sessions.get_mut(&session_id) {
        session.exited.store(true, Ordering::SeqCst);
        if !session.label.contains("[exited]") {
            session.label = format!("{} [exited]", session.label);
        }
    }
    app.rebuild_sidebar_items();
}

/// Apply title updates received from the background tmux poller.
/// Uses a blocking write so updates are never silently dropped.
pub fn apply_tmux_title_updates(app: &App, updates: Vec<(u64, String)>) {
    for (session_id, title) in updates {
        if let Some(session) = app.sessions.get(&session_id) {
            if let Ok(mut parser) = session.parser.write() {
                parser.callbacks_mut().title = title;
            }
        }
    }
}

/// Snapshot of tmux session info needed by the poller thread.
#[derive(Clone)]
pub struct TmuxSessionInfo {
    pub session_id: u64,
    pub tmux_name: String,
    pub current_title: String,
    pub exited: bool,
}

/// Collect tmux session info from the app for the background poller.
pub fn collect_tmux_session_info(app: &App) -> Vec<TmuxSessionInfo> {
    app.sessions
        .values()
        .filter_map(|s| {
            s.tmux_session_name.as_ref().map(|name| TmuxSessionInfo {
                session_id: s.id,
                tmux_name: name.clone(),
                current_title: s.parser
                    .try_read()
                    .ok()
                    .map(|p| p.callbacks().title.clone())
                    .unwrap_or_default(),
                exited: s.exited.load(Ordering::SeqCst),
            })
        })
        .collect()
}

/// Spawn a background thread that continuously polls tmux for pane title
/// changes and sends events when titles update.
pub fn spawn_tmux_title_poller(
    event_tx: tokio::sync::mpsc::UnboundedSender<crate::event::AppEvent>,
    session_info: std::sync::Arc<std::sync::Mutex<Vec<TmuxSessionInfo>>>,
) {
    std::thread::Builder::new()
        .name("tmux-title-poller".into())
        .spawn(move || {
            loop {
                std::thread::sleep(Duration::from_millis(500));

                let sessions = {
                    match session_info.lock() {
                        Ok(guard) => guard.clone(),
                        Err(_) => continue,
                    }
                };

                if sessions.is_empty() {
                    continue;
                }

                let mut updates = Vec::new();
                for info in &sessions {
                    if info.exited {
                        continue;
                    }
                    if let Some(title) = pty::query_tmux_pane_title(&info.tmux_name) {
                        if title != info.current_title {
                            updates.push((info.session_id, title));
                        }
                    }
                }

                if !updates.is_empty() {
                    if event_tx
                        .send(crate::event::AppEvent::TmuxTitlesChanged { updates })
                        .is_err()
                    {
                        break; // channel closed, app shutting down
                    }
                }
            }
        })
        .ok();
}

/// Resize all active sessions to match new terminal dimensions.
pub fn resize_all(app: &App, rows: u16, cols: u16) {
    let pane_pct = if app.sidebar_visible { 0.7 } else { 1.0 };
    let pane_cols = (cols as f32 * pane_pct) as u16 - 2;
    let pane_rows = rows - 3;

    for session in app.sessions.values() {
        if !session.exited.load(Ordering::SeqCst) {
            let _ = session.master_pty.resize(portable_pty::PtySize {
                rows: pane_rows,
                cols: pane_cols,
                pixel_width: 0,
                pixel_height: 0,
            });

            if let Ok(mut p) = session.parser.write() {
                p.screen_mut().set_size(pane_rows, pane_cols);
            }
        }
    }
}

/// Scan for existing tmux sessions from a previous TUI run and reattach them.
/// Returns the number of sessions reconnected.
pub fn reconnect_tmux_sessions(app: &mut App, terminal_size: (u16, u16)) -> usize {
    if !app.tmux_available {
        return 0;
    }

    let tmux_sessions = pty::list_tmux_sessions();
    if tmux_sessions.is_empty() {
        return 0;
    }

    let pane_pct = if app.sidebar_visible { 0.7 } else { 1.0 };
    let cols = (terminal_size.0 as f32 * pane_pct) as u16 - 2;
    let rows = terminal_size.1 - 3;

    let mut count = 0;

    for (tmux_name, wt_path) in tmux_sessions {
        // Find which worktree this session belongs to
        let wt_idx = app.worktrees.iter().position(|wt| {
            wt.path == wt_path || wt_path.starts_with(&wt.path)
        });

        let wt_idx = match wt_idx {
            Some(i) => i,
            None => continue, // Orphaned tmux session, skip
        };

        let session_id = app.next_session_id;
        app.next_session_id += 1;

        // Query the pane title before attaching — this is the name Claude set
        let pane_title = pty::query_tmux_pane_title(&tmux_name);

        let handle = match pty::attach_tmux_session(
            &tmux_name,
            session_id,
            app.event_tx.clone(),
            rows,
            cols,
        ) {
            Ok(h) => h,
            Err(e) => {
                tracing::warn!("Failed to reattach tmux session '{}': {}", tmux_name, e);
                continue;
            }
        };

        // Use the pane title Claude set, fall back to tmux session name
        let label = pane_title.unwrap_or_else(|| tmux_name.clone());

        let session = Session {
            id: session_id,
            worktree_path: wt_path,
            label,
            parser: handle.parser,
            write_tx: handle.write_tx,
            master_pty: handle.master_pty,
            exited: handle.exited,
            last_output: handle.last_output,
            tmux_session_name: handle.tmux_session_name,
            nickname: None,
        };

        app.sessions.insert(session_id, session);

        if let Some(wt) = app.worktrees.get_mut(wt_idx) {
            wt.session_ids.push(session_id);
            wt.expanded = true;
        }

        count += 1;
    }

    if count > 0 {
        app.rebuild_sidebar_items();
    }

    count
}

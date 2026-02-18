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

    /// Returns true if the session has produced output recently (within the
    /// last 2 seconds), indicating Claude is actively working.
    pub fn is_active(&self) -> bool {
        if self.exited.load(Ordering::Relaxed) {
            return false;
        }
        match self.last_output.try_read() {
            Ok(t) => t.elapsed() < Duration::from_secs(2),
            Err(_) => false,
        }
    }
}

/// Spawn a new Claude session in the given worktree.
pub fn spawn_session(app: &mut App, worktree_idx: usize, terminal_size: (u16, u16), skip_permissions: bool) -> anyhow::Result<u64> {
    let wt = app.worktrees.get(worktree_idx)
        .ok_or_else(|| anyhow::anyhow!("Invalid worktree index"))?;

    let session_id = app.next_session_id;
    app.next_session_id += 1;

    let pane_pct = if app.sidebar_visible { 0.7 } else { 1.0 };
    let cols = (terminal_size.0 as f32 * pane_pct) as u16 - 2;
    let rows = terminal_size.1 - 3;

    let handle = pty::spawn_claude_pty(
        &wt.path,
        session_id,
        app.event_tx.clone(),
        rows,
        cols,
        skip_permissions,
    )?;

    let existing_count = wt.session_ids.len();
    let label = if skip_permissions {
        format!("claude-{} [yolo]", existing_count + 1)
    } else {
        format!("claude-{}", existing_count + 1)
    };

    let session = Session {
        id: session_id,
        worktree_path: wt.path.clone(),
        label,
        parser: handle.parser,
        write_tx: handle.write_tx,
        master_pty: handle.master_pty,
        exited: handle.exited,
        last_output: handle.last_output,
    };

    app.sessions.insert(session_id, session);

    if let Some(wt) = app.worktrees.get_mut(worktree_idx) {
        wt.session_ids.push(session_id);
        wt.expanded = true;
    }

    Ok(session_id)
}

/// Kill a session and clean up.
pub fn kill_session(app: &mut App, session_id: u64) {
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

/// Mark a session as exited.
pub fn mark_exited(app: &mut App, session_id: u64) {
    if let Some(session) = app.sessions.get_mut(&session_id) {
        if !session.label.contains("[exited]") {
            session.label = format!("{} [exited]", session.label);
        }
    }
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

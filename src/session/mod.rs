pub mod pty;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

use crate::app::{AgentStatus, App};

/// Claude Code context window usage data parsed from debug logs.
#[derive(Debug, Clone)]
pub struct ClaudeUsage {
    pub tokens_used: usize,
    pub effective_window: usize,
    pub threshold: usize,
}

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
    /// The prompt used to create this agent (None for reconnected sessions).
    pub initial_prompt: Option<String>,
    /// Whether the agent was active on the previous tick (for transition detection).
    pub was_active: bool,
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

    /// Returns true if the session's terminal is at Claude Code's input prompt (`>`).
    /// Scans the bottom of the VT100 screen for a line containing the `>` prompt,
    /// handling both plain prompts and prompts inside box-drawing characters (│ > │).
    pub fn is_at_input_prompt(&self) -> bool {
        if self.exited.load(Ordering::Relaxed) {
            return false;
        }

        // For tmux sessions, capture the full visible pane content
        if let Some(ref tmux_name) = self.tmux_session_name {
            if let Ok(output) = std::process::Command::new("tmux")
                .args(["capture-pane", "-t", tmux_name, "-p"])
                .output()
            {
                if output.status.success() {
                    let content = String::from_utf8_lossy(&output.stdout);
                    return Self::content_has_input_prompt(&content);
                }
            }
        }

        // Fallback: scan VT100 screen contents
        match self.parser.try_read() {
            Ok(guard) => {
                let screen = guard.screen();
                let contents = screen.contents();
                Self::content_has_input_prompt(&contents)
            }
            Err(_) => false,
        }
    }

    /// Check if terminal content contains Claude Code's idle input prompt.
    /// Claude Code uses `❯` (U+276F) as the prompt character, rendered inside
    /// box-drawing borders: `│ ❯ │` or `│ ❯ placeholder text │`.
    ///
    /// The input prompt area is separated from the output by a horizontal
    /// separator (`├───┤`). Selection/choice menus (e.g. "Yes/No" for clearing
    /// context) also use `❯` as the cursor but appear inline in the content
    /// area without a separator above them.
    ///
    /// Detection logic:
    /// - `❯` with no text after it → idle input prompt (always true)
    /// - `❯` with text after it → only idle if a `├───┤` separator is directly
    ///   above (meaning we're in the input area, not a selection menu)
    fn content_has_input_prompt(content: &str) -> bool {
        let bottom_lines: Vec<&str> = content
            .lines()
            .rev()
            .filter(|l| !l.trim().is_empty())
            .take(8)
            .collect();

        // Find the first ❯ line from the bottom
        let mut prompt_idx: Option<usize> = None;
        let mut prompt_has_text = false;

        for (i, line) in bottom_lines.iter().enumerate() {
            let trimmed = line.trim().trim_matches('\u{a0}').trim();
            if trimmed.is_empty() { continue; }

            let stripped = trimmed
                .trim_start_matches(|c: char| c == '│' || c == '┃' || c == '|')
                .trim_end_matches(|c: char| c == '│' || c == '┃' || c == '|')
                .trim()
                .trim_matches('\u{a0}')
                .trim();
            if stripped.is_empty() { continue; }

            if Self::starts_with_prompt(stripped) {
                prompt_idx = Some(i);
                let after = stripped
                    .trim_start_matches(|c: char| c == '❯' || c == '›')
                    .trim();
                prompt_has_text = !after.is_empty();
                break;
            }
        }

        let idx = match prompt_idx {
            Some(i) => i,
            None => return false,
        };

        // ❯ with no text after it → definitely the idle input prompt
        if !prompt_has_text {
            return true;
        }

        // ❯ followed by text → could be user input in the prompt area, or a
        // selection menu option. Check for a horizontal separator (├───┤) above
        // it which marks the boundary between output and input areas.
        for j in (idx + 1)..bottom_lines.len() {
            let above = bottom_lines[j].trim();
            if above.is_empty() { continue; }

            // Skip lines that are empty inside their borders
            let inner = above
                .trim_start_matches(|c: char| c == '│' || c == '┃' || c == '|')
                .trim_end_matches(|c: char| c == '│' || c == '┃' || c == '|')
                .trim()
                .trim_matches('\u{a0}')
                .trim();
            if inner.is_empty() { continue; }

            // Horizontal separator: line containing ├ or ┣
            if above.contains('├') || above.contains('┣') {
                return true;
            }

            // Hit non-separator content → not in the input area
            break;
        }

        false
    }

    /// Returns true if the string starts with Claude Code's prompt character.
    /// Only matches `❯` (U+276F) and `›` (U+203A) — NOT plain `>` which appears
    /// too frequently in build output, error messages, and shell commands.
    fn starts_with_prompt(s: &str) -> bool {
        matches!(s.chars().next(), Some('❯') | Some('›'))
    }

    /// Compute the agent status for mini mode display.
    /// - Working: spinner chars in title (actively processing)
    /// - Idle: at the input prompt (Claude finished, waiting for next command)
    /// - NeedsInput: not active, not at prompt, not exited (Claude asked a question
    ///   or is waiting for user input outside the normal prompt)
    /// - Exited: process terminated
    pub fn agent_status(&self) -> AgentStatus {
        if self.exited.load(Ordering::Relaxed) {
            return AgentStatus::Exited;
        }
        if self.is_active() {
            return AgentStatus::Working;
        }
        if self.is_at_input_prompt() {
            return AgentStatus::Idle;
        }
        AgentStatus::NeedsInput
    }

    /// Get the visible terminal content (via tmux capture-pane or vt100 parser).
    pub fn get_visible_content(&self) -> String {
        if let Some(ref tmux_name) = self.tmux_session_name {
            if let Ok(output) = std::process::Command::new("tmux")
                .args(["capture-pane", "-t", tmux_name, "-p"])
                .output()
            {
                if output.status.success() {
                    return String::from_utf8_lossy(&output.stdout).to_string();
                }
            }
        }
        match self.parser.try_read() {
            Ok(guard) => guard.screen().contents(),
            Err(_) => String::new(),
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

/// Extract a summary from the session's visible terminal output.
///
/// First tries to find content wrapped in `<IMPORTANT_CLAWTREE_OUTPUT>` XML tags
/// (which Claude produces when instructed by the mini mode pre-input instruction).
/// Falls back to capturing the last non-empty output block above the prompt
/// (up to 5 lines), stripping box-drawing characters.
pub fn extract_summary(session: &Session) -> Option<String> {
    let content = session.get_visible_content();
    if content.is_empty() {
        return None;
    }

    // Try to extract from <IMPORTANT_CLAWTREE_OUTPUT> tags first.
    // Look for the *last* occurrence in case there are multiple.
    if let Some(tagged) = extract_clawtree_tagged_output(&content) {
        return Some(tagged);
    }

    let lines: Vec<&str> = content.lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .collect();

    if lines.is_empty() {
        return None;
    }

    // Find the last non-empty lines, skipping prompt lines and box-drawing borders
    let mut summary_lines: Vec<String> = Vec::new();
    for line in lines.iter().rev() {
        // Skip prompt lines
        let stripped = line
            .trim_start_matches(|c: char| c == '│' || c == '┃' || c == '|')
            .trim_end_matches(|c: char| c == '│' || c == '┃' || c == '|')
            .trim();
        if stripped.is_empty() {
            continue;
        }
        if Session::starts_with_prompt(stripped) {
            continue;
        }
        // Skip pure box-drawing lines (borders)
        if stripped.chars().all(|c| "─━┌┐└┘├┤┬┴┼╭╮╰╯".contains(c) || c == ' ') {
            continue;
        }
        // Clean box-drawing chars from the line
        let clean: String = stripped
            .trim_start_matches(|c: char| "│┃|─━ ".contains(c))
            .trim_end_matches(|c: char| "│┃|─━ ".contains(c))
            .trim()
            .to_string();
        if !clean.is_empty() {
            summary_lines.push(clean);
        }
        if summary_lines.len() >= 5 {
            break;
        }
    }

    summary_lines.reverse();
    if summary_lines.is_empty() {
        None
    } else {
        Some(summary_lines.join("\n"))
    }
}

/// Extract content from the last `<IMPORTANT_CLAWTREE_OUTPUT>...</IMPORTANT_CLAWTREE_OUTPUT>`
/// block in the terminal output. The tags may be split across lines and may have
/// box-drawing characters around them (since they're rendered inside Claude's TUI).
fn extract_clawtree_tagged_output(content: &str) -> Option<String> {
    const OPEN_TAG: &str = "<IMPORTANT_CLAWTREE_OUTPUT>";
    const CLOSE_TAG: &str = "</IMPORTANT_CLAWTREE_OUTPUT>";

    // Strip box-drawing chars from each line before searching for tags,
    // since the terminal wraps everything in │ ... │ borders.
    let cleaned: String = content
        .lines()
        .map(|l| {
            l.trim()
                .trim_start_matches(|c: char| c == '│' || c == '┃' || c == '|')
                .trim_end_matches(|c: char| c == '│' || c == '┃' || c == '|')
                .trim()
        })
        .collect::<Vec<_>>()
        .join("\n");

    // Find the last occurrence of the tags
    let open_pos = cleaned.rfind(OPEN_TAG)?;
    let after_open = open_pos + OPEN_TAG.len();
    let close_pos = cleaned[after_open..].rfind(CLOSE_TAG)?;
    let inner = &cleaned[after_open..after_open + close_pos];

    let trimmed = inner.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Spawn a new Claude session in the given worktree.
/// If `initial_prompt` is Some, it is passed as a CLI argument so Claude starts working on it immediately.
pub fn spawn_session(app: &mut App, worktree_idx: usize, terminal_size: (u16, u16), skip_permissions: bool, initial_prompt: Option<&str>) -> anyhow::Result<u64> {
    let wt = app.worktrees.get(worktree_idx)
        .ok_or_else(|| anyhow::anyhow!("Invalid worktree index"))?;

    let session_id = app.next_session_id;
    app.next_session_id += 1;

    let (rows, cols) = calculate_pane_size(app, terminal_size.1, terminal_size.0);

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
        initial_prompt: initial_prompt.map(|s| s.to_string()),
        was_active: false,
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
    app.prompt_queues.remove(&session_id);
    app.agent_summaries.remove(&session_id);

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
    pub worktree_path: String,
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
                worktree_path: s.worktree_path.to_string_lossy().to_string(),
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

/// Calculate PTY pane dimensions to match the ratatui layout exactly.
/// Uses integer arithmetic matching ratatui's Percentage constraint.
fn calculate_pane_size(app: &App, terminal_rows: u16, terminal_cols: u16) -> (u16, u16) {
    use crate::app::ScreenMode;

    match app.screen_mode {
        ScreenMode::MiniDrilldown => {
            // Drilldown layout: 1-row header + terminal pane (with borders) + 1-row status bar
            let pane_cols = terminal_cols.saturating_sub(2);
            let pane_rows = terminal_rows.saturating_sub(4); // 1 header + 1 status + 2 border
            (pane_rows, pane_cols)
        }
        _ => {
            let sidebar_width = if app.sidebar_visible && app.screen_mode == ScreenMode::Normal {
                (terminal_cols as u32 * 30 / 100) as u16
            } else {
                0
            };
            let pane_cols = terminal_cols.saturating_sub(sidebar_width).saturating_sub(2);

            let queue_height = if app.prompt_queue_visible && app.active_session_id.is_some() {
                app.queue_panel_height()
            } else {
                0
            };
            let pane_rows = terminal_rows.saturating_sub(3).saturating_sub(queue_height);

            (pane_rows, pane_cols)
        }
    }
}

/// Resize all active sessions to match new terminal dimensions.
pub fn resize_all(app: &App, rows: u16, cols: u16) {
    let (pane_rows, pane_cols) = calculate_pane_size(app, rows, cols);

    for session in app.sessions.values() {
        if !session.exited.load(Ordering::SeqCst) {
            // Skip resize if parser already has the target dimensions.
            // Unnecessary resize sends SIGWINCH to Claude (via tmux) even
            // when the size hasn't changed, causing Claude to redraw and
            // push content into scrollback — the cumulative "extra newline" bug.
            let already_correct = session.parser.try_read()
                .map(|p| p.screen().size() == (pane_rows, pane_cols))
                .unwrap_or(false);

            if already_correct {
                continue;
            }

            tracing::info!(
                "RESIZE-ALL session {} resizing to {}x{}",
                session.id, pane_cols, pane_rows
            );

            // Resize parser first so it's ready for new-size output before
            // the PTY resize triggers tmux to re-render
            if let Ok(mut p) = session.parser.write() {
                p.screen_mut().set_size(pane_rows, pane_cols);
            }

            let _ = session.master_pty.resize(portable_pty::PtySize {
                rows: pane_rows,
                cols: pane_cols,
                pixel_width: 0,
                pixel_height: 0,
            });

            // Explicitly resize the tmux window so the inner pane matches.
            // Relying solely on the PTY resize propagating through the
            // `tmux attach` client is unreliable — tmux may ignore the
            // client size change depending on window-size policy or when
            // multiple clients are attached.
            if let Some(ref tmux_name) = session.tmux_session_name {
                let _ = std::process::Command::new("tmux")
                    .args([
                        "resize-window",
                        "-t", tmux_name,
                        "-x", &pane_cols.to_string(),
                        "-y", &pane_rows.to_string(),
                    ])
                    .output();
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

    let (rows, cols) = calculate_pane_size(app, terminal_size.1, terminal_size.0);

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
            initial_prompt: None,
            was_active: false,
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

/// Spawn a background thread that polls Claude Code debug logs for context
/// window usage using pure file I/O (no subprocess calls).
///
/// Discovery strategy (for unclaimed sessions only):
/// 1. Scan /proc for processes with .claude/tasks/<uuid> fds (pure file I/O).
/// 2. Match the process CWD to the session's worktree path.
/// 3. Use the UUID to find the corresponding debug log file.
///
/// Once discovered, debug file paths are cached and subsequent polls just
/// read the last 8KB of the cached file (microseconds per session).
pub fn spawn_claude_usage_poller(
    event_tx: tokio::sync::mpsc::UnboundedSender<crate::event::AppEvent>,
    session_info: std::sync::Arc<std::sync::Mutex<Vec<TmuxSessionInfo>>>,
) {
    std::thread::Builder::new()
        .name("claude-usage-poller".into())
        .spawn(move || {
            let mut path_cache: std::collections::HashMap<u64, String> =
                std::collections::HashMap::new();

            // Brief startup delay so the TUI renders immediately
            std::thread::sleep(Duration::from_secs(2));

            loop {
                std::thread::sleep(Duration::from_secs(2));

                let sessions = match session_info.lock() {
                    Ok(guard) => guard.clone(),
                    Err(_) => continue,
                };

                if sessions.is_empty() {
                    continue;
                }

                // Prune cache: remove sessions that no longer exist
                let active_ids: std::collections::HashSet<u64> =
                    sessions.iter().map(|s| s.session_id).collect();
                path_cache.retain(|id, _| active_ids.contains(id));

                // Find sessions that don't have a cached debug file yet
                let unclaimed: Vec<&TmuxSessionInfo> = sessions
                    .iter()
                    .filter(|s| !s.exited && !path_cache.contains_key(&s.session_id))
                    .collect();

                // Discovery: scan /proc for Claude task UUIDs, match by worktree path
                if !unclaimed.is_empty() {
                    let task_map = discover_claude_tasks();
                    for info in &unclaimed {
                        for (cwd, debug_path) in &task_map {
                            if cwd == &info.worktree_path
                                || cwd.starts_with(&format!("{}/", info.worktree_path))
                            {
                                if parse_last_autocompact(debug_path).is_some() {
                                    path_cache.insert(info.session_id, debug_path.clone());
                                    break;
                                }
                            }
                        }
                    }

                    // Fallback for any still-unclaimed: mtime-based scan of ~/.claude/debug/
                    let still_unclaimed: Vec<u64> = unclaimed
                        .iter()
                        .filter(|s| !path_cache.contains_key(&s.session_id))
                        .map(|s| s.session_id)
                        .collect();

                    if !still_unclaimed.is_empty() {
                        let claimed_paths: std::collections::HashSet<String> =
                            path_cache.values().cloned().collect();
                        if let Ok(home) = std::env::var("HOME") {
                            let debug_dir = format!("{}/.claude/debug", home);
                            if let Ok(entries) = std::fs::read_dir(&debug_dir) {
                                let now = std::time::SystemTime::now();
                                let max_age = Duration::from_secs(600);

                                let mut candidates: Vec<(std::time::SystemTime, String)> = entries
                                    .filter_map(|e| e.ok())
                                    .filter(|e| {
                                        e.path()
                                            .extension()
                                            .map(|ext| ext == "txt")
                                            .unwrap_or(false)
                                    })
                                    .filter_map(|e| {
                                        let path = e.path().to_string_lossy().to_string();
                                        if claimed_paths.contains(&path) {
                                            return None;
                                        }
                                        let modified = e.metadata().ok()?.modified().ok()?;
                                        if now.duration_since(modified).unwrap_or(max_age) < max_age
                                        {
                                            Some((modified, path))
                                        } else {
                                            None
                                        }
                                    })
                                    .collect();

                                candidates.sort_by(|a, b| b.0.cmp(&a.0));

                                let mut candidate_iter = candidates.into_iter();
                                for sid in &still_unclaimed {
                                    loop {
                                        match candidate_iter.next() {
                                            Some((_, path)) => {
                                                if parse_last_autocompact(&path).is_some() {
                                                    path_cache.insert(*sid, path);
                                                    break;
                                                }
                                            }
                                            None => break,
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                // Read all cached files (pure file I/O — microseconds each)
                let mut updates = Vec::new();
                for info in sessions.iter().filter(|s| !s.exited) {
                    if let Some(path) = path_cache.get(&info.session_id) {
                        if let Some(usage) = parse_last_autocompact(path) {
                            updates.push((info.session_id, usage));
                        }
                    }
                }

                if !updates.is_empty() {
                    if event_tx
                        .send(crate::event::AppEvent::ClaudeUsageUpdated { updates })
                        .is_err()
                    {
                        break;
                    }
                }
            }
        })
        .ok();
}

/// Discover active Claude task UUIDs by scanning /proc for processes with
/// .claude/tasks/<uuid> file descriptors. Returns a map of CWD → debug_file_path.
/// Pure file I/O — no subprocess calls. Typically completes in ~5-10ms.
fn discover_claude_tasks() -> std::collections::HashMap<String, String> {
    let mut results = std::collections::HashMap::new();

    let home = match std::env::var("HOME") {
        Ok(h) => h,
        Err(_) => return results,
    };

    let proc_dir = match std::fs::read_dir("/proc") {
        Ok(d) => d,
        Err(_) => return results,
    };

    for entry in proc_dir.flatten() {
        let name = entry.file_name();
        let pid_str = name.to_string_lossy();
        if !pid_str.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }

        let fd_dir = format!("/proc/{}/fd", pid_str);
        let fds = match std::fs::read_dir(&fd_dir) {
            Ok(d) => d,
            Err(_) => continue,
        };

        for fd_entry in fds.flatten() {
            if let Ok(target) = std::fs::read_link(fd_entry.path()) {
                let target_str = target.to_string_lossy();
                if let Some(pos) = target_str.find(".claude/tasks/") {
                    let after = &target_str[pos + ".claude/tasks/".len()..];
                    let uuid = after.split('/').next().unwrap_or("");
                    if !uuid.is_empty() && uuid.len() >= 32 {
                        let cwd_path = format!("/proc/{}/cwd", pid_str);
                        if let Ok(cwd) = std::fs::read_link(&cwd_path) {
                            let debug_path =
                                format!("{}/.claude/debug/{}.txt", home, uuid);
                            if std::path::Path::new(&debug_path).exists() {
                                results.insert(
                                    cwd.to_string_lossy().to_string(),
                                    debug_path,
                                );
                            }
                        }
                        break; // found UUID for this PID, move on
                    }
                }
            }
        }
    }

    results
}

/// Global account-level usage data from the Anthropic API.
#[derive(Debug, Clone)]
pub struct GlobalUsage {
    pub five_hour_pct: f64,
    pub five_hour_reset: String,
    pub seven_day_pct: f64,
    pub seven_day_reset: String,
}

/// Spawn a background thread that polls the Anthropic API for global usage data.
/// Polls every 30 seconds. Reads OAuth token from ~/.claude/.credentials.json.
/// Initial poll is deferred by 2 seconds so the TUI renders immediately without lag.
pub fn spawn_global_usage_poller(
    event_tx: tokio::sync::mpsc::UnboundedSender<crate::event::AppEvent>,
) {
    std::thread::Builder::new()
        .name("global-usage-poller".into())
        .spawn(move || {
            // Brief delay so the TUI starts up without waiting on the first curl
            std::thread::sleep(Duration::from_secs(2));
            loop {
                if let Some(usage) = poll_global_usage() {
                    if event_tx
                        .send(crate::event::AppEvent::GlobalUsageUpdated { usage })
                        .is_err()
                    {
                        break;
                    }
                }
                std::thread::sleep(Duration::from_secs(30));
            }
        })
        .ok();
}

/// Read the OAuth access token from ~/.claude/.credentials.json.
fn read_oauth_token() -> Option<String> {
    let home = std::env::var("HOME").ok()?;
    let path = format!("{}/.claude/.credentials.json", home);
    let content = std::fs::read_to_string(path).ok()?;
    let json: serde_json::Value = serde_json::from_str(&content).ok()?;
    json.get("claudeAiOauth")
        .and_then(|o| o.get("accessToken"))
        .and_then(|t| t.as_str())
        .map(|s| s.to_string())
}

/// Poll the Anthropic API for global usage data.
fn poll_global_usage() -> Option<GlobalUsage> {
    let token = read_oauth_token()?;

    let output = std::process::Command::new("curl")
        .args([
            "-s",
            "--max-time", "5",
            "-H", &format!("Authorization: Bearer {}", token),
            "-H", "anthropic-beta: oauth-2025-04-20",
            "https://api.anthropic.com/api/oauth/usage",
        ])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let body = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value = serde_json::from_str(&body).ok()?;

    let five_hour = json.get("five_hour")?;
    let seven_day = json.get("seven_day")?;

    Some(GlobalUsage {
        five_hour_pct: five_hour.get("utilization")?.as_f64()?,
        five_hour_reset: five_hour.get("resets_at")?.as_str()?.to_string(),
        seven_day_pct: seven_day.get("utilization")?.as_f64()?,
        seven_day_reset: seven_day.get("resets_at")?.as_str()?.to_string(),
    })
}

/// Parse the last `autocompact:` line from a Claude debug log file.
/// Reads the file from the end for efficiency.
fn parse_last_autocompact(path: &str) -> Option<ClaudeUsage> {
    use std::io::{Read, Seek, SeekFrom};

    let mut file = std::fs::File::open(path).ok()?;
    let file_len = file.metadata().ok()?.len();

    // Read the last 8KB (autocompact lines are short, this is plenty)
    let read_size = 8192u64.min(file_len);
    if read_size == 0 {
        return None;
    }

    file.seek(SeekFrom::End(-(read_size as i64))).ok()?;
    let mut buf = vec![0u8; read_size as usize];
    file.read_exact(&mut buf).ok()?;
    let content = String::from_utf8_lossy(&buf);

    // Find the last autocompact line
    let mut last_line = None;
    for line in content.lines() {
        if line.contains("autocompact:") {
            last_line = Some(line);
        }
    }

    let line = last_line?;
    parse_autocompact_line(line)
}

/// Parse an autocompact log line: `autocompact: tokens=78536 threshold=167000 effectiveWindow=180000`
fn parse_autocompact_line(line: &str) -> Option<ClaudeUsage> {
    let after = line.split("autocompact:").nth(1)?;

    let mut tokens = None;
    let mut threshold = None;
    let mut effective_window = None;

    for part in after.split_whitespace() {
        if let Some(val) = part.strip_prefix("tokens=") {
            tokens = val.parse().ok();
        } else if let Some(val) = part.strip_prefix("threshold=") {
            threshold = val.parse().ok();
        } else if let Some(val) = part.strip_prefix("effectiveWindow=") {
            effective_window = val.parse().ok();
        }
    }

    Some(ClaudeUsage {
        tokens_used: tokens?,
        effective_window: effective_window?,
        threshold: threshold?,
    })
}

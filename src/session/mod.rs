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
}

impl ClaudeUsage {
    /// Context usage percentage relative to the effective context window.
    /// The `effectiveWindow` is the actual usable context — the buffer above
    /// it is reserved by Claude Code for the autocompact process itself.
    pub fn usage_pct(&self) -> usize {
        if self.effective_window > 0 {
            (self.tokens_used as f64 / self.effective_window as f64 * 100.0) as usize
        } else {
            0
        }
    }
}

/// A session running in a PTY — either Claude Code or a plain terminal shell.
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
    /// Whether the agent was active on the previous tick (for transition detection).
    pub was_active: bool,
    /// True if this is a plain terminal session (not Claude Code).
    pub is_terminal: bool,
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
                .trim_start_matches(['│', '┃', '|'])
                .trim_end_matches(['│', '┃', '|'])
                .trim()
                .trim_matches('\u{a0}')
                .trim();
            if stripped.is_empty() { continue; }

            if Self::starts_with_prompt(stripped) {
                prompt_idx = Some(i);
                let after = stripped
                    .trim_start_matches(['❯', '›'])
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
        // selection menu option. Check for a horizontal separator above it
        // which marks the boundary between output and input areas.
        // Claude Code uses either `├───┤` (box-drawing) or a solid line of
        // `─` characters as the input area separator.
        for line in &bottom_lines[(idx + 1)..] {
            let above = line.trim();
            if above.is_empty() { continue; }

            // Skip lines that are empty inside their borders
            let inner = above
                .trim_start_matches(['│', '┃', '|'])
                .trim_end_matches(['│', '┃', '|'])
                .trim()
                .trim_matches('\u{a0}')
                .trim();
            if inner.is_empty() { continue; }

            // Horizontal separator: box-drawing T-junctions
            if above.contains('├') || above.contains('┣') {
                return true;
            }

            // Horizontal separator: solid line of ─ (Claude Code input boundary)
            if Self::is_horizontal_rule(inner) {
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

    /// Returns true if the string is a horizontal rule made of box-drawing
    /// horizontal characters (─, ━, ╌, ═). Requires at least 10 such chars
    /// and that they make up the majority of the line.
    fn is_horizontal_rule(s: &str) -> bool {
        let total = s.chars().count();
        if total < 10 {
            return false;
        }
        let horiz = s.chars().filter(|&c| c == '─' || c == '━' || c == '╌' || c == '═').count();
        horiz > total / 2
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
        // Plain terminal sessions are either alive or exited — they don't
        // have Claude's spinner/prompt detection, so skip those checks.
        if self.is_terminal {
            return AgentStatus::Idle;
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

    /// Returns true if Claude Code is in plan mode, detected by the ⏸ (U+23F8)
    /// character that Claude Code displays in the bottom bar when plan mode is on.
    /// The bottom bar shows "⏸ plan mode on" when active.
    pub fn is_in_plan_mode(&self) -> bool {
        if self.exited.load(Ordering::Relaxed) || self.is_terminal {
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
                    return Self::content_has_plan_mode(&content);
                }
            }
        }

        // Fallback: scan VT100 screen contents
        match self.parser.try_read() {
            Ok(guard) => {
                let screen = guard.screen();
                let contents = screen.contents();
                Self::content_has_plan_mode(&contents)
            }
            Err(_) => false,
        }
    }

    /// Check if terminal content contains Claude Code's plan mode indicator.
    /// Claude Code shows "⏸ plan mode on" in the bottom bar when plan mode is active.
    fn content_has_plan_mode(content: &str) -> bool {
        content
            .lines()
            .rev()
            .filter(|l| !l.trim().is_empty())
            .take(6)
            .any(|line| line.contains('\u{23F8}'))
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
            .trim_start_matches(['│', '┃', '|'])
            .trim_end_matches(['│', '┃', '|'])
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
                .trim_start_matches(['│', '┃', '|'])
                .trim_end_matches(['│', '┃', '|'])
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

/// Spawn a one-shot `claude -p` process to summarize terminal output.
///
/// Captures the tmux scrollback for the given session and sends it to
/// a haiku model with a summarization system prompt. The result is sent
/// back via the event channel as `SummaryReady`.
pub fn generate_oneshot_summary(
    session_id: u64,
    tmux_session_name: &str,
    event_tx: mpsc::UnboundedSender<crate::event::AppEvent>,
) {
    let tmux_name = tmux_session_name.to_string();
    std::thread::spawn(move || {
        // Capture last 200 lines of tmux scrollback (visible + history)
        let capture = std::process::Command::new("tmux")
            .args(["capture-pane", "-t", &tmux_name, "-p", "-S", "-200"])
            .output();
        let content = match capture {
            Ok(out) if out.status.success() => {
                String::from_utf8_lossy(&out.stdout).to_string()
            }
            _ => return,
        };

        let trimmed = content.trim();
        if trimmed.is_empty() {
            return;
        }

        // Pipe the terminal content to claude -p with a summarization prompt
        use std::io::Write;
        let mut child = match std::process::Command::new("claude")
            .args([
                "-p",
                "--model", "haiku",
                "--no-session-persistence",
                "--system-prompt",
                "You are a concise summarizer. You will receive terminal output from a Claude Code coding session. Extract a 1-3 sentence summary of what the agent accomplished, what it changed, or what it is requesting. Output ONLY the summary text, nothing else. Do not ask questions or continue the conversation.",
            ])
            .env("CLAUDECODE", "")
            .env("CLAUDE_CODE_ENTRYPOINT", "")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            Ok(child) => child,
            Err(_) => return,
        };

        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(trimmed.as_bytes());
            // stdin is dropped here, closing the pipe
        }

        let output = match child.wait_with_output() {
            Ok(out) => out,
            Err(_) => return,
        };

        if output.status.success() {
            let summary = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !summary.is_empty() {
                let _ = event_tx.send(crate::event::AppEvent::SummaryReady {
                    session_id,
                    summary,
                });
            }
        }
    });
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
        was_active: false,
        is_terminal: false,
    };

    app.sessions.insert(session_id, session);

    if let Some(wt) = app.worktrees.get_mut(worktree_idx) {
        wt.session_ids.push(session_id);
        wt.expanded = true;
    }

    Ok(session_id)
}

/// Spawn a plain terminal shell session in the given worktree (tmux only).
pub fn spawn_terminal_session(app: &mut App, worktree_idx: usize, terminal_size: (u16, u16)) -> anyhow::Result<u64> {
    if !app.tmux_available {
        anyhow::bail!("tmux is required for terminal sessions");
    }

    let wt = app.worktrees.get(worktree_idx)
        .ok_or_else(|| anyhow::anyhow!("Invalid worktree index"))?;

    let session_id = app.next_session_id;
    app.next_session_id += 1;

    let (rows, cols) = calculate_pane_size(app, terminal_size.1, terminal_size.0);

    let tmux_name = pty::tmux_terminal_session_name(&wt.branch, session_id);
    let handle = pty::spawn_terminal_pty_tmux(
        &wt.path,
        session_id,
        app.event_tx.clone(),
        rows,
        cols,
        &tmux_name,
    )?;

    let label = wt.path.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "terminal".to_string());

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
        was_active: false,
        is_terminal: true,
    };

    app.sessions.insert(session_id, session);

    // Terminals go into the dedicated terminal panel, not under worktrees
    app.terminal_ids.push(session_id);

    Ok(session_id)
}

/// Spawn a Claude session at the bare repo root (for the Project overview).
pub fn spawn_root_session(app: &mut App, terminal_size: (u16, u16), skip_permissions: bool) -> anyhow::Result<u64> {
    if !app.tmux_available {
        anyhow::bail!("tmux is required for root sessions");
    }

    let session_id = app.next_session_id;
    app.next_session_id += 1;

    let (rows, cols) = calculate_pane_size(app, terminal_size.1, terminal_size.0);

    let tmux_name = pty::tmux_session_name("root", session_id);
    let handle = pty::spawn_claude_pty_tmux(
        &app.bare_repo_path,
        session_id,
        app.event_tx.clone(),
        rows,
        cols,
        skip_permissions,
        &tmux_name,
        None,
    )?;

    let label = "root".to_string();

    let session = Session {
        id: session_id,
        worktree_path: app.bare_repo_path.clone(),
        label,
        parser: handle.parser,
        write_tx: handle.write_tx,
        master_pty: handle.master_pty,
        exited: handle.exited,
        last_output: handle.last_output,
        tmux_session_name: handle.tmux_session_name,
        nickname: None,
        was_active: false,
        is_terminal: false,
    };

    app.sessions.insert(session_id, session);
    app.project_session_ids.push(session_id);
    app.project_expanded = true;

    Ok(session_id)
}

/// Spawn a plain terminal at the bare repo root (for the Project overview).
pub fn spawn_root_terminal_session(app: &mut App, terminal_size: (u16, u16)) -> anyhow::Result<u64> {
    if !app.tmux_available {
        anyhow::bail!("tmux is required for terminal sessions");
    }

    let session_id = app.next_session_id;
    app.next_session_id += 1;

    let (rows, cols) = calculate_pane_size(app, terminal_size.1, terminal_size.0);

    let tmux_name = pty::tmux_terminal_session_name("root", session_id);
    let handle = pty::spawn_terminal_pty_tmux(
        &app.bare_repo_path,
        session_id,
        app.event_tx.clone(),
        rows,
        cols,
        &tmux_name,
    )?;

    let label = "root-terminal".to_string();

    let session = Session {
        id: session_id,
        worktree_path: app.bare_repo_path.clone(),
        label,
        parser: handle.parser,
        write_tx: handle.write_tx,
        master_pty: handle.master_pty,
        exited: handle.exited,
        last_output: handle.last_output,
        tmux_session_name: handle.tmux_session_name,
        nickname: None,
        was_active: false,
        is_terminal: true,
    };

    app.sessions.insert(session_id, session);
    app.project_session_ids.push(session_id);
    app.project_expanded = true;

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
    app.project_session_ids.retain(|&id| id != session_id);
    app.terminal_ids.retain(|&id| id != session_id);

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

                if !updates.is_empty()
                    && event_tx
                        .send(crate::event::AppEvent::TmuxTitlesChanged { updates })
                        .is_err()
                {
                    break; // channel closed, app shutting down
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
                crate::ui::theme::SIDEBAR_MAX_WIDTH.min(terminal_cols)
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
            // Skip parser + PTY resize if already at the target dimensions.
            // Unnecessary PTY resize sends SIGWINCH to Claude (via tmux) even
            // when the size hasn't changed, causing Claude to redraw and
            // push content into scrollback — the cumulative "extra newline" bug.
            let already_correct = session.parser.try_read()
                .map(|p| p.screen().size() == (pane_rows, pane_cols))
                .unwrap_or(false);

            if !already_correct {
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
            }

            // Always sync the tmux window size — the tmux session may have
            // been detached and reattached on a different-sized terminal
            // (e.g. moved to a larger monitor). The parser was recreated at
            // the correct size on attach, so the `already_correct` check
            // above would skip the PTY resize, but the tmux window itself
            // is still at the old dimensions.
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

        // Check if this is a root/project session (path matches bare repo)
        let is_root = wt_idx.is_none() && (wt_path == app.bare_repo_path || wt_path.starts_with(&app.bare_repo_path));

        if wt_idx.is_none() && !is_root {
            continue; // Orphaned tmux session, skip
        }

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

        // Detect whether this is a terminal session by checking for the -term- infix
        let is_terminal = tmux_name.contains("-term-");

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
            was_active: false,
            is_terminal,
        };

        app.sessions.insert(session_id, session);

        if is_root {
            // Root sessions go under the Project entry
            app.project_session_ids.push(session_id);
            app.project_expanded = true;
        } else if is_terminal {
            // Terminals go into the dedicated terminal panel
            app.terminal_ids.push(session_id);
        } else if let Some(wt) = app.worktrees.get_mut(wt_idx.unwrap()) {
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

                // Discovery: for each unclaimed session, get the tmux pane PID,
                // then match by /proc FD lookup or process start timestamp.
                // One tmux call per unclaimed session (transient, typically 0).
                if !unclaimed.is_empty() {
                    let home = std::env::var("HOME").unwrap_or_default();
                    let mut claimed_paths: std::collections::HashSet<String> =
                        path_cache.values().cloned().collect();
                    for info in &unclaimed {
                        if let Some(debug_path) = discover_debug_file_for_session(
                            &info.tmux_name,
                            &home,
                            &claimed_paths,
                        ) {
                            claimed_paths.insert(debug_path.clone());
                            path_cache.insert(info.session_id, debug_path);
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

                if !updates.is_empty()
                    && event_tx
                        .send(crate::event::AppEvent::ClaudeUsageUpdated { updates })
                        .is_err()
                {
                    break;
                }
            }
        })
        .ok();
}

/// Discover the debug file for a specific tmux session by matching its process
/// start time to the first-line timestamp in debug log files.
/// One tmux subprocess call + file I/O. Only called for unclaimed sessions.
fn discover_debug_file_for_session(
    tmux_name: &str,
    home: &str,
    claimed_paths: &std::collections::HashSet<String>,
) -> Option<String> {
    // Get the pane PID from tmux (one lightweight subprocess call)
    let output = std::process::Command::new("tmux")
        .args(["display-message", "-t", tmux_name, "-p", "#{pane_pid}"])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let pane_pid: u32 = String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse()
        .ok()?;

    // First try: direct /proc FD lookup (works for actively processing sessions)
    if let Some(path) = find_task_uuid_in_proc(pane_pid, home) {
        if !claimed_paths.contains(&path) {
            return Some(path);
        }
    }

    // Second try: match process start time to debug file first-line timestamp.
    // Each Claude process creates its debug file within ~1 second of starting.
    let proc_start_epoch = get_process_start_epoch(pane_pid)?;
    match_debug_file_by_timestamp(home, proc_start_epoch, claimed_paths)
}

/// Get a process's start time as epoch seconds from /proc/<pid>.
fn get_process_start_epoch(pid: u32) -> Option<i64> {
    let proc_path = format!("/proc/{}", pid);
    let metadata = std::fs::metadata(&proc_path).ok()?;
    let modified = metadata.modified().ok()?;
    let epoch = modified
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs() as i64;
    Some(epoch)
}

/// Find a debug file whose first-line timestamp matches the given epoch (within 5 seconds).
/// Reads only the first 256 bytes of each candidate file for efficiency.
fn match_debug_file_by_timestamp(
    home: &str,
    target_epoch: i64,
    claimed_paths: &std::collections::HashSet<String>,
) -> Option<String> {
    let debug_dir = format!("{}/.claude/debug", home);
    let entries = std::fs::read_dir(&debug_dir).ok()?;

    let mut best: Option<(i64, String)> = None; // (diff, path)

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().map(|e| e != "txt").unwrap_or(true) {
            continue;
        }

        let path_str = path.to_string_lossy().to_string();
        if claimed_paths.contains(&path_str) {
            continue;
        }

        // Quick filter: skip files last modified more than 24 hours before the target
        if let Ok(meta) = entry.metadata() {
            if let Ok(mtime) = meta.modified() {
                let file_mtime = mtime
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs() as i64;
                if file_mtime < target_epoch - 86400 {
                    continue;
                }
            }
        }

        // Read first 256 bytes to parse the timestamp
        if let Some(file_epoch) = read_first_line_epoch(&path_str) {
            let diff = file_epoch - target_epoch;
            // Must be within 0-5 seconds after process start
            if (0..=5).contains(&diff)
                && best.as_ref().map(|(d, _)| diff < *d).unwrap_or(true)
            {
                best = Some((diff, path_str));
            }
        }
    }

    best.map(|(_, path)| path)
}

/// Read the first line of a debug log and parse the ISO 8601 timestamp to epoch seconds.
fn read_first_line_epoch(path: &str) -> Option<i64> {
    use std::io::Read;
    let mut file = std::fs::File::open(path).ok()?;
    let mut buf = [0u8; 256];
    let n = file.read(&mut buf).ok()?;
    let content = std::str::from_utf8(&buf[..n]).ok()?;
    let first_line = content.lines().next()?;

    // Parse: "2026-02-19T22:28:25.843Z [DEBUG] ..."
    if first_line.len() < 19 {
        return None;
    }
    let ts_str = &first_line[..19]; // "2026-02-19T22:28:25"

    let year: i32 = ts_str.get(0..4)?.parse().ok()?;
    let month: u32 = ts_str.get(5..7)?.parse().ok()?;
    let day: u32 = ts_str.get(8..10)?.parse().ok()?;
    let hour: u32 = ts_str.get(11..13)?.parse().ok()?;
    let min: u32 = ts_str.get(14..16)?.parse().ok()?;
    let sec: u32 = ts_str.get(17..19)?.parse().ok()?;

    let days = days_from_civil(year, month, day);
    Some(days as i64 * 86400 + hour as i64 * 3600 + min as i64 * 60 + sec as i64)
}

/// Convert a civil date to days since Unix epoch.
/// Algorithm from http://howardhinnant.github.io/date_algorithms.html
fn days_from_civil(y: i32, m: u32, d: u32) -> i32 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u32;
    let m_adj = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * m_adj + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe as i32 - 719468
}

/// Check a specific PID's /proc/<pid>/fd for .claude/tasks/<uuid> and
/// return the corresponding debug file path.
fn find_task_uuid_in_proc(pid: u32, home: &str) -> Option<String> {
    let fd_dir = format!("/proc/{}/fd", pid);
    let fds = std::fs::read_dir(&fd_dir).ok()?;

    for fd_entry in fds.flatten() {
        if let Ok(target) = std::fs::read_link(fd_entry.path()) {
            let target_str = target.to_string_lossy();
            if let Some(pos) = target_str.find(".claude/tasks/") {
                let after = &target_str[pos + ".claude/tasks/".len()..];
                let uuid = after.split('/').next().unwrap_or("");
                if !uuid.is_empty() && uuid.len() >= 32 {
                    let debug_path = format!("{}/.claude/debug/{}.txt", home, uuid);
                    if std::path::Path::new(&debug_path).exists() {
                        return Some(debug_path);
                    }
                }
            }
        }
    }
    None
}

/// Global account-level usage data from the Anthropic API.
#[derive(Debug, Clone)]
pub struct GlobalUsage {
    pub five_hour_pct: f64,
    pub five_hour_reset: String,
    pub seven_day_pct: f64,
    pub seven_day_reset: String,
    pub sonnet_7d_pct: Option<f64>,
    pub sonnet_7d_reset: Option<String>,
}

/// Spawn a background thread that polls the Anthropic API for global usage data.
/// Polls every 120 seconds with exponential backoff on errors (up to 10 minutes).
/// Initial poll is deferred by 2 seconds so the TUI renders immediately without lag.
pub fn spawn_global_usage_poller(
    event_tx: tokio::sync::mpsc::UnboundedSender<crate::event::AppEvent>,
) {
    const BASE_INTERVAL_SECS: u64 = 120;
    const MAX_BACKOFF_SECS: u64 = 600; // 10 minutes

    std::thread::Builder::new()
        .name("global-usage-poller".into())
        .spawn(move || {
            // Brief delay so the TUI starts up without waiting on the first curl
            std::thread::sleep(Duration::from_secs(2));
            let mut current_interval = BASE_INTERVAL_SECS;
            loop {
                match poll_global_usage() {
                    Ok(usage) => {
                        current_interval = BASE_INTERVAL_SECS;
                        if event_tx
                            .send(crate::event::AppEvent::GlobalUsageUpdated { usage })
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(_) => {
                        // Exponential backoff: double the interval on each
                        // consecutive failure, capped at MAX_BACKOFF_SECS.
                        current_interval = (current_interval * 2).min(MAX_BACKOFF_SECS);
                    }
                }
                std::thread::sleep(Duration::from_secs(current_interval));
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
/// Returns `Err(())` on any failure (network, auth, rate limit, parse) so the
/// caller can apply backoff.
fn poll_global_usage() -> Result<GlobalUsage, ()> {
    let token = read_oauth_token().ok_or(())?;

    let output = std::process::Command::new("curl")
        .args([
            "-s",
            "--max-time", "10",
            "-H", &format!("Authorization: Bearer {}", token),
            "-H", "anthropic-beta: oauth-2025-04-20",
            "https://api.anthropic.com/api/oauth/usage",
        ])
        .output()
        .map_err(|_| ())?;

    if !output.status.success() {
        return Err(());
    }

    let body = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value = serde_json::from_str(&body).map_err(|_| ())?;

    // Detect API-level errors (rate limits, auth failures, etc.)
    if json.get("error").is_some() {
        return Err(());
    }

    let five_hour = json.get("five_hour").ok_or(())?;
    let seven_day = json.get("seven_day").ok_or(())?;

    // seven_day_sonnet may be null when unused
    let (sonnet_7d_pct, sonnet_7d_reset) = json.get("seven_day_sonnet")
        .and_then(|v| v.as_object())
        .map(|snt| {
            let pct = snt.get("utilization").and_then(|v| v.as_f64());
            let reset = snt.get("resets_at").and_then(|v| v.as_str()).map(|s| s.to_string());
            (pct, reset)
        })
        .unwrap_or((None, None));

    Ok(GlobalUsage {
        five_hour_pct: five_hour.get("utilization").and_then(|v| v.as_f64()).ok_or(())?,
        five_hour_reset: five_hour.get("resets_at").and_then(|v| v.as_str()).ok_or(())?.to_string(),
        seven_day_pct: seven_day.get("utilization").and_then(|v| v.as_f64()).ok_or(())?,
        seven_day_reset: seven_day.get("resets_at").and_then(|v| v.as_str()).ok_or(())?.to_string(),
        sonnet_7d_pct,
        sonnet_7d_reset,
    })
}

/// Parse the last `autocompact:` line from a Claude debug log file.
/// Reads the file from the end in expanding chunks until an `autocompact:`
/// line is found. Debug logs can contain heavy spam (e.g. "Fast mode
/// unavailable" repeated hundreds of times), so a fixed-size tail is not
/// enough — the last autocompact line can easily be 32KB+ from the end.
fn parse_last_autocompact(path: &str) -> Option<ClaudeUsage> {
    use std::io::{Read, Seek, SeekFrom};

    let mut file = std::fs::File::open(path).ok()?;
    let file_len = file.metadata().ok()?.len();
    if file_len == 0 {
        return None;
    }

    // Start with 64KB and double up to the full file size.
    let mut read_size = (64 * 1024u64).min(file_len);
    loop {
        file.seek(SeekFrom::End(-(read_size as i64))).ok()?;
        let mut buf = vec![0u8; read_size as usize];
        file.read_exact(&mut buf).ok()?;
        let content = String::from_utf8_lossy(&buf);

        // Find the last autocompact line in this chunk
        let mut last_line = None;
        for line in content.lines() {
            if line.contains("autocompact:") {
                last_line = Some(line.to_owned());
            }
        }

        if let Some(line) = last_line {
            return parse_autocompact_line(&line);
        }

        // Already read the whole file — give up
        if read_size >= file_len {
            return None;
        }

        // Double the window and try again
        read_size = (read_size * 2).min(file_len);
    }
}

/// Parse an autocompact log line: `autocompact: tokens=78536 threshold=167000 effectiveWindow=180000`
fn parse_autocompact_line(line: &str) -> Option<ClaudeUsage> {
    let after = line.split("autocompact:").nth(1)?;

    let mut tokens = None;
    let mut effective_window = None;

    for part in after.split_whitespace() {
        if let Some(val) = part.strip_prefix("tokens=") {
            tokens = val.parse().ok();
        } else if let Some(val) = part.strip_prefix("effectiveWindow=") {
            effective_window = val.parse().ok();
        }
    }

    Some(ClaudeUsage {
        tokens_used: tokens?,
        effective_window: effective_window?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── content_has_input_prompt tests ──────────────────────────────────

    #[test]
    fn test_content_has_input_prompt_bare_chevron() {
        // ❯ with no text after it → idle prompt
        let content = "Some output\n│ ❯ │\n";
        assert!(Session::content_has_input_prompt(content));
    }

    #[test]
    fn test_content_has_input_prompt_with_separator() {
        // ❯ with text, preceded by separator → input area
        let content = "Some output\n├──────────────────┤\n│ ❯ type here │\n";
        assert!(Session::content_has_input_prompt(content));
    }

    #[test]
    fn test_content_has_input_prompt_selection_menu() {
        // ❯ with text, no separator → selection menu, NOT idle prompt
        let content = "Choose an option:\n❯ Yes\n  No\n";
        assert!(!Session::content_has_input_prompt(content));
    }

    #[test]
    fn test_content_has_input_prompt_empty() {
        assert!(!Session::content_has_input_prompt(""));
    }

    // ── starts_with_prompt tests ───────────────────────────────────────

    #[test]
    fn test_starts_with_prompt_heavy_chevron() {
        assert!(Session::starts_with_prompt("❯"));
        assert!(Session::starts_with_prompt("❯ some text"));
    }

    #[test]
    fn test_starts_with_prompt_single_angle() {
        assert!(Session::starts_with_prompt("› prompt"));
    }

    #[test]
    fn test_starts_with_prompt_plain_gt() {
        // Plain > should NOT match (too common in build output)
        assert!(!Session::starts_with_prompt("> not a prompt"));
    }

    #[test]
    fn test_starts_with_prompt_regular_text() {
        assert!(!Session::starts_with_prompt("Hello world"));
        assert!(!Session::starts_with_prompt(""));
    }

    // ── is_horizontal_rule tests ───────────────────────────────────────

    #[test]
    fn test_is_horizontal_rule_long_dashes() {
        assert!(Session::is_horizontal_rule("──────────────────────"));
    }

    #[test]
    fn test_is_horizontal_rule_too_short() {
        assert!(!Session::is_horizontal_rule("─────"));
    }

    #[test]
    fn test_is_horizontal_rule_mixed_text() {
        // Majority must be horizontal box chars
        assert!(!Session::is_horizontal_rule("── hello world ──────"));
    }

    #[test]
    fn test_is_horizontal_rule_all_text() {
        assert!(!Session::is_horizontal_rule("this is just text content"));
    }

    // ── extract_clawtree_tagged_output tests ───────────────────────────

    #[test]
    fn test_extract_tagged_output_simple() {
        let content = "some output\n<IMPORTANT_CLAWTREE_OUTPUT>\nHello summary\n</IMPORTANT_CLAWTREE_OUTPUT>\nmore output\n";
        let result = extract_clawtree_tagged_output(content);
        assert_eq!(result, Some("Hello summary".to_string()));
    }

    #[test]
    fn test_extract_tagged_output_with_borders() {
        let content = "│ <IMPORTANT_CLAWTREE_OUTPUT> │\n│ Summary here │\n│ </IMPORTANT_CLAWTREE_OUTPUT> │\n";
        let result = extract_clawtree_tagged_output(content);
        assert_eq!(result, Some("Summary here".to_string()));
    }

    #[test]
    fn test_extract_tagged_output_empty_tags() {
        let content = "<IMPORTANT_CLAWTREE_OUTPUT>\n</IMPORTANT_CLAWTREE_OUTPUT>\n";
        let result = extract_clawtree_tagged_output(content);
        assert!(result.is_none());
    }

    #[test]
    fn test_extract_tagged_output_no_tags() {
        let content = "just regular output with no tags";
        let result = extract_clawtree_tagged_output(content);
        assert!(result.is_none());
    }
}

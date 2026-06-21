use anyhow::{Context, Result};
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use std::io::{Read, Write};
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Instant;
use tokio::sync::mpsc;

use crate::event::AppEvent;

/// Callbacks that capture the window title set by Claude Code.
pub struct TitleCallbacks {
    pub title: String,
}

impl TitleCallbacks {
    pub fn new() -> Self {
        Self {
            title: String::new(),
        }
    }
}

impl vt100::Callbacks for TitleCallbacks {
    fn set_window_title(&mut self, _screen: &mut vt100::Screen, title: &[u8]) {
        if let Ok(s) = std::str::from_utf8(title) {
            self.title = s.to_string();
        }
    }

    fn set_window_icon_name(&mut self, _screen: &mut vt100::Screen, name: &[u8]) {
        // Some apps set the icon name instead of / in addition to the title
        if let Ok(s) = std::str::from_utf8(name) {
            if self.title.is_empty() {
                self.title = s.to_string();
            }
        }
    }
}

/// The parser type with our callbacks.
pub type ClaudeParser = vt100::Parser<TitleCallbacks>;

/// Holds the PTY state for a session.
pub struct PtyHandle {
    pub parser: Arc<RwLock<ClaudeParser>>,
    pub write_tx: mpsc::UnboundedSender<bytes::Bytes>,
    pub master_pty: Box<dyn portable_pty::MasterPty + Send>,
    pub exited: Arc<AtomicBool>,
    pub last_output: Arc<RwLock<Instant>>,
    /// If this session is backed by tmux, the tmux session name.
    pub tmux_session_name: Option<String>,
}

/// Prefix for all tmux sessions we manage.
const TMUX_PREFIX: &str = "clawtree";

/// Configure the tmux server for truecolor passthrough (called once per process).
///
/// tmux decides whether to pass 24-bit RGB colors through to the outer terminal
/// (our PTY) or downconvert them to the 256-color palette.  It checks the `Tc`
/// capability in `terminal-overrides` (tmux < 3.2) and the `RGB` flag in
/// `terminal-features` (tmux >= 3.2).  We need both to cover all tmux versions.
///
/// We also set `default-terminal` to `tmux-256color` so programs *inside* tmux
/// (e.g. Claude Code) see a TERM that advertises full color, rather than the
/// default `screen` which only declares 8 colors.
fn ensure_tmux_truecolor() {
    use std::sync::Once;
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        // 1. Wildcard Tc — tells tmux that ANY outer terminal supports truecolor.
        //    Uses `-sa` (server-append) so we don't clobber user overrides.
        let _ = Command::new("tmux")
            .args(["set", "-sa", "terminal-overrides", ",*:Tc"])
            .output();

        // 2. terminal-features RGB — the modern equivalent for tmux >= 3.2.
        //    Harmless on older tmux (unrecognized option is silently ignored).
        let _ = Command::new("tmux")
            .args(["set", "-sa", "terminal-features", ",xterm-256color:RGB"])
            .output();

        // 3. default-terminal — controls what TERM is set inside tmux panes.
        //    `tmux-256color` advertises 256-color + tmux-specific capabilities.
        //    Programs like Claude Code check TERM to decide their color output level.
        let _ = Command::new("tmux")
            .args(["set", "-g", "default-terminal", "tmux-256color"])
            .output();
    });
}

/// Return all prefixes we should scan for when listing/reconnecting tmux sessions.
/// Always includes TMUX_PREFIX. Additional legacy prefixes can be supplied via the
/// `CLAWTREE_LEGACY_PREFIXES` environment variable (comma-separated, e.g. "wctui,oldname").
fn all_tmux_prefixes() -> Vec<&'static str> {
    use std::sync::OnceLock;
    static PREFIXES: OnceLock<Vec<String>> = OnceLock::new();
    let prefixes = PREFIXES.get_or_init(|| {
        let mut v = vec![TMUX_PREFIX.to_string()];
        if let Ok(val) = std::env::var("CLAWTREE_LEGACY_PREFIXES") {
            for p in val.split(',') {
                let p = p.trim();
                if !p.is_empty() && p != TMUX_PREFIX {
                    v.push(p.to_string());
                }
            }
        }
        v
    });
    // SAFETY: OnceLock guarantees the Vec lives for 'static, so we can hand out &str refs.
    prefixes.iter().map(|s| s.as_str()).collect()
}

/// Check if tmux is available on the system.
pub fn tmux_available() -> bool {
    Command::new("tmux")
        .arg("-V")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Sanitize a string for use in a tmux session name.
fn sanitize_tmux_name(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

/// Generate a unique tmux session name for a plain terminal in a worktree.
pub fn tmux_terminal_session_name(branch: &str, session_id: u64) -> String {
    let base = format!(
        "{}-term-{}-{}",
        TMUX_PREFIX,
        sanitize_tmux_name(branch),
        session_id
    );
    if tmux_session_exists(&base) {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        format!("{}-{}", base, ts % 10000)
    } else {
        base
    }
}

/// Generate a unique tmux session name for a worktree branch + session id.
pub fn tmux_session_name(branch: &str, session_id: u64) -> String {
    let base = format!(
        "{}-{}-{}",
        TMUX_PREFIX,
        sanitize_tmux_name(branch),
        session_id
    );
    // If name already taken, append a suffix
    if tmux_session_exists(&base) {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        format!("{}-{}", base, ts % 10000)
    } else {
        base
    }
}

/// Check if a tmux session with the given name exists.
fn tmux_session_exists(name: &str) -> bool {
    Command::new("tmux")
        .args(["has-session", "-t", name])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Environment variable we set inside tmux to track the original worktree path.
const TMUX_WORKTREE_ENV: &str = "CLAWTREE_WORKTREE";

/// List all tmux sessions that belong to us (prefixed with TMUX_PREFIX).
/// Returns (session_name, worktree_path) pairs sorted by creation time (oldest first).
pub fn list_tmux_sessions() -> Vec<(String, std::path::PathBuf)> {
    // Request session_created alongside the name so we can sort by creation time
    let output = match Command::new("tmux")
        .args(["list-sessions", "-F", "#{session_created}\t#{session_name}"])
        .output()
    {
        Ok(o) if o.status.success() => o,
        _ => return Vec::new(),
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let prefixes = all_tmux_prefixes();
    let mut our_sessions: Vec<(u64, String)> = stdout
        .lines()
        .filter_map(|line| {
            let (ts_str, name) = line.split_once('\t')?;
            if prefixes.iter().any(|pfx| name.starts_with(pfx)) {
                let ts = ts_str.parse::<u64>().unwrap_or(0);
                Some((ts, name.to_string()))
            } else {
                None
            }
        })
        .collect();
    our_sessions.sort_by_key(|(ts, _)| *ts);

    let mut results = Vec::new();
    for (_ts, name) in our_sessions {
        // First try our env var (reliable — set at creation time)
        let env_output = Command::new("tmux")
            .args(["show-environment", "-t", &name, TMUX_WORKTREE_ENV])
            .output();

        let path = if let Ok(o) = &env_output {
            if o.status.success() {
                // Output is "CLAWTREE_WORKTREE=/path/to/worktree\n"
                let line = String::from_utf8_lossy(&o.stdout);
                line.trim()
                    .strip_prefix(&format!("{}=", TMUX_WORKTREE_ENV))
                    .map(|p| p.to_string())
            } else {
                None
            }
        } else {
            None
        };

        // Fall back to pane_current_path if env var not set (old sessions)
        let path = path.or_else(|| {
            Command::new("tmux")
                .args(["display-message", "-t", &name, "-p", "#{pane_current_path}"])
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        });

        if let Some(p) = path {
            if !p.is_empty() {
                results.push((name, std::path::PathBuf::from(p)));
            }
        }
    }

    results
}

/// List just the names of all tmux sessions that belong to us.
/// Cheaper than `list_tmux_sessions()` — skips worktree path resolution.
pub fn list_tmux_session_names() -> Vec<String> {
    let output = match Command::new("tmux")
        .args(["list-sessions", "-F", "#{session_name}"])
        .output()
    {
        Ok(o) if o.status.success() => o,
        _ => return Vec::new(),
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let prefixes = all_tmux_prefixes();
    stdout
        .lines()
        .filter(|name| prefixes.iter().any(|pfx| name.starts_with(pfx)))
        .map(|s| s.to_string())
        .collect()
}

/// Kill a tmux session by name. Returns true if the kill command succeeded.
pub fn kill_tmux_session(name: &str) -> bool {
    Command::new("tmux")
        .args(["kill-session", "-t", name])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Spawn a `claude` process inside a tmux session, then attach to it via PTY.
#[allow(clippy::too_many_arguments)]
pub fn spawn_claude_pty_tmux(
    working_dir: &Path,
    session_id: u64,
    event_tx: mpsc::UnboundedSender<AppEvent>,
    rows: u16,
    cols: u16,
    skip_permissions: bool,
    tmux_name: &str,
    initial_prompt: Option<&str>,
) -> Result<PtyHandle> {
    // Create the tmux session running claude
    let mut tmux_args = vec![
        "new-session".to_string(),
        "-d".to_string(),
        "-s".to_string(),
        tmux_name.to_string(),
        "-x".to_string(),
        cols.to_string(),
        "-y".to_string(),
        rows.to_string(),
    ];
    // Run claude with COLORTERM=truecolor so it emits 24-bit RGB colors
    tmux_args.push("env".to_string());
    tmux_args.push("COLORTERM=truecolor".to_string());
    tmux_args.push("claude".to_string());
    if skip_permissions {
        tmux_args.push("--dangerously-skip-permissions".to_string());
    }
    if let Some(prompt) = initial_prompt {
        tmux_args.push(prompt.to_string());
    }

    // Ensure the tmux server is configured for truecolor passthrough (once)
    ensure_tmux_truecolor();

    let output = Command::new("tmux")
        .args(&tmux_args)
        .current_dir(working_dir)
        .env("TERM", "xterm-256color")
        .output()
        .context("Failed to create tmux session")?;

    if !output.status.success() {
        anyhow::bail!(
            "tmux new-session failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    // Disable the tmux status bar so the pane fills the full PTY dimensions.
    // This eliminates the 1-row discrepancy between PTY size and pane size
    // that causes content reflow / extra blank lines on resize.
    let _ = Command::new("tmux")
        .args(["set-option", "-t", tmux_name, "status", "off"])
        .output();

    // Store the worktree path and COLORTERM in the tmux session environment
    let _ = Command::new("tmux")
        .args([
            "set-environment",
            "-t",
            tmux_name,
            TMUX_WORKTREE_ENV,
            &working_dir.to_string_lossy(),
        ])
        .output();
    let _ = Command::new("tmux")
        .args(["set-environment", "-t", tmux_name, "COLORTERM", "truecolor"])
        .output();

    // Allow passthrough of escape sequences that tmux normally strips
    // (OSC for hyperlinks, styled underlines, kitty graphics, etc.)
    let _ = Command::new("tmux")
        .args(["set-option", "-t", tmux_name, "allow-passthrough", "on"])
        .output();

    // Clear the default pane title (usually username@host) so it doesn't
    // show up before Claude sets its own title
    let _ = Command::new("tmux")
        .args(["select-pane", "-t", tmux_name, "-T", ""])
        .output();

    // Now attach to that tmux session via a PTY
    attach_tmux_session(tmux_name, session_id, event_tx, rows, cols)
}

/// Spawn a plain shell inside a tmux session, then attach to it via PTY.
pub fn spawn_terminal_pty_tmux(
    working_dir: &Path,
    session_id: u64,
    event_tx: mpsc::UnboundedSender<AppEvent>,
    rows: u16,
    cols: u16,
    tmux_name: &str,
) -> Result<PtyHandle> {
    let tmux_args = vec![
        "new-session".to_string(),
        "-d".to_string(),
        "-s".to_string(),
        tmux_name.to_string(),
        "-x".to_string(),
        cols.to_string(),
        "-y".to_string(),
        rows.to_string(),
    ];

    ensure_tmux_truecolor();

    let output = Command::new("tmux")
        .args(&tmux_args)
        .current_dir(working_dir)
        .env("TERM", "xterm-256color")
        .output()
        .context("Failed to create tmux terminal session")?;

    if !output.status.success() {
        anyhow::bail!(
            "tmux new-session failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let _ = Command::new("tmux")
        .args(["set-option", "-t", tmux_name, "status", "off"])
        .output();

    let _ = Command::new("tmux")
        .args([
            "set-environment",
            "-t",
            tmux_name,
            TMUX_WORKTREE_ENV,
            &working_dir.to_string_lossy(),
        ])
        .output();
    let _ = Command::new("tmux")
        .args(["set-environment", "-t", tmux_name, "COLORTERM", "truecolor"])
        .output();

    let _ = Command::new("tmux")
        .args(["set-option", "-t", tmux_name, "allow-passthrough", "on"])
        .output();

    let _ = Command::new("tmux")
        .args(["select-pane", "-t", tmux_name, "-T", ""])
        .output();

    attach_tmux_session(tmux_name, session_id, event_tx, rows, cols)
}

/// Query the current working directory of a tmux session's pane.
pub fn query_tmux_pane_cwd(tmux_name: &str) -> Option<String> {
    let output = Command::new("tmux")
        .args([
            "display-message",
            "-t",
            tmux_name,
            "-p",
            "#{pane_current_path}",
        ])
        .output()
        .ok()?;
    if output.status.success() {
        let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !path.is_empty() {
            return Some(path);
        }
    }
    None
}

/// Interactive shells. When one of these is the foreground process *and* it is
/// the pane's own login shell, the pane is idle at a prompt. When a shell of the
/// same name appears as a child (running a script), we surface the script name.
const SHELL_COMMANDS: &[&str] = &["bash", "zsh", "fish", "sh", "dash"];

/// Query the command the user is actively running in a tmux pane, e.g. "vim",
/// "node", or the basename of a shell script like "deploy.sh".
///
/// Returns `None` when the pane is sitting idle at its shell prompt — callers
/// fall back to the working directory in that case.
///
/// `#{pane_current_command}` alone can't tell an idle shell from one running a
/// script (both report "bash"), so we inspect the pane's tty: the interactive
/// shell holds the foreground process group only when idle. Any *other*
/// foreground process is the command the user is running.
pub fn query_tmux_pane_foreground(tmux_name: &str) -> Option<String> {
    // The pid tmux launched (the interactive shell) and the pane's tty.
    let output = Command::new("tmux")
        .args([
            "display-message",
            "-t",
            tmux_name,
            "-p",
            "#{pane_pid}|#{pane_tty}",
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let (pid_str, tty) = stdout.trim().split_once('|')?;
    let shell_pid: i32 = pid_str.trim().parse().ok()?;
    let tty = tty.trim().strip_prefix("/dev/").unwrap_or(tty.trim());
    if tty.is_empty() {
        return None;
    }

    // List every process attached to the pane's tty. STAT contains '+' for the
    // foreground process group; PID lets us exclude the idle interactive shell.
    let ps = Command::new("ps")
        .args(["-t", tty, "-o", "stat=,pid=,comm=,args="])
        .output()
        .ok()?;
    if !ps.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&ps.stdout);

    for line in text.lines() {
        let line = line.trim_start();
        let mut parts = line.splitn(2, char::is_whitespace);
        let stat = parts.next()?;
        if !stat.contains('+') {
            continue; // not in the foreground process group
        }
        let rest = parts.next().unwrap_or("").trim_start();
        let mut p = rest.splitn(2, char::is_whitespace);
        let pid: i32 = p.next().and_then(|s| s.parse().ok())?;
        if pid == shell_pid {
            continue; // the idle interactive shell itself → not a running command
        }
        let rest = p.next().unwrap_or("").trim_start();
        let mut p = rest.splitn(2, char::is_whitespace);
        let comm = p.next().unwrap_or("").trim();
        let args = p.next().unwrap_or("").trim();
        if comm.is_empty() {
            continue;
        }
        return Some(friendly_command_name(comm, args));
    }
    None
}

/// Turn a foreground process into a human-friendly label. For a shell running a
/// script (`/bin/bash ./deploy.sh`), surface the script's basename; otherwise
/// use the process name.
fn friendly_command_name(comm: &str, args: &str) -> String {
    if SHELL_COMMANDS.contains(&comm) {
        // First non-flag argument after the shell path is the script.
        for tok in args.split_whitespace().skip(1) {
            if tok.starts_with('-') {
                continue;
            }
            if let Some(name) = std::path::Path::new(tok).file_name() {
                return name.to_string_lossy().to_string();
            }
        }
    }
    comm.to_string()
}

/// Query the pane title from an existing tmux session.
pub fn query_tmux_pane_title(tmux_name: &str) -> Option<String> {
    let output = Command::new("tmux")
        .args(["display-message", "-t", tmux_name, "-p", "#{pane_title}"])
        .output()
        .ok()?;
    if output.status.success() {
        let title = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !title.is_empty() {
            return Some(title);
        }
    }
    None
}

/// Attach to an existing tmux session via a PTY.
pub fn attach_tmux_session(
    tmux_name: &str,
    session_id: u64,
    event_tx: mpsc::UnboundedSender<AppEvent>,
    rows: u16,
    cols: u16,
) -> Result<PtyHandle> {
    // Ensure truecolor is configured (idempotent — runs once per process)
    ensure_tmux_truecolor();

    // Ensure status bar is off so pane fills the full PTY dimensions
    let _ = Command::new("tmux")
        .args(["set-option", "-t", tmux_name, "status", "off"])
        .output();

    // NOTE: We intentionally do NOT call `tmux resize-window` here.
    // The tmux session already has the correct size from the previous run,
    // and resize-window can send SIGWINCH to Claude even when the size
    // hasn't changed, causing content to scroll into the scrollback buffer.

    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .context("Failed to open PTY")?;

    let mut cmd = CommandBuilder::new("tmux");
    cmd.args(["attach-session", "-t", tmux_name]);
    cmd.env("TERM", "xterm-256color");
    cmd.env("COLORTERM", "truecolor");

    // Query existing pane title before attaching (so it's preserved on reconnect)
    let existing_title = query_tmux_pane_title(tmux_name);

    let _child = pair
        .slave
        .spawn_command(cmd)
        .context("Failed to attach to tmux session")?;

    drop(pair.slave);

    setup_pty_threads(
        pair.master,
        session_id,
        event_tx,
        rows,
        cols,
        Some(tmux_name.to_string()),
        existing_title,
    )
}

/// Spawn a `claude` process directly in a PTY (no tmux).
pub fn spawn_claude_pty(
    working_dir: &Path,
    session_id: u64,
    event_tx: mpsc::UnboundedSender<AppEvent>,
    rows: u16,
    cols: u16,
    skip_permissions: bool,
    initial_prompt: Option<&str>,
) -> Result<PtyHandle> {
    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .context("Failed to open PTY")?;

    let mut cmd = CommandBuilder::new("claude");
    if skip_permissions {
        cmd.arg("--dangerously-skip-permissions");
    }
    if let Some(prompt) = initial_prompt {
        cmd.arg(prompt);
    }
    cmd.cwd(working_dir);
    cmd.env("TERM", "xterm-256color");
    cmd.env("COLORTERM", "truecolor");

    let _child = pair
        .slave
        .spawn_command(cmd)
        .context("Failed to spawn claude CLI")?;

    drop(pair.slave);

    setup_pty_threads(pair.master, session_id, event_tx, rows, cols, None, None)
}

/// Common PTY reader/writer thread setup.
fn setup_pty_threads(
    master: Box<dyn portable_pty::MasterPty + Send>,
    session_id: u64,
    event_tx: mpsc::UnboundedSender<AppEvent>,
    rows: u16,
    cols: u16,
    tmux_session_name: Option<String>,
    initial_title: Option<String>,
) -> Result<PtyHandle> {
    let mut callbacks = TitleCallbacks::new();
    if let Some(title) = initial_title {
        callbacks.title = title;
    }
    let parser = Arc::new(RwLock::new(vt100::Parser::new_with_callbacks(
        rows, cols, 1000, callbacks,
    )));
    let exited = Arc::new(AtomicBool::new(false));
    let last_output = Arc::new(RwLock::new(Instant::now()));

    // ── Reader thread (plain OS thread) ────────────────────────────
    let reader_parser = Arc::clone(&parser);
    let reader_exited = Arc::clone(&exited);
    let reader_tx = event_tx.clone();
    let reader_last_output = Arc::clone(&last_output);
    let mut reader = master
        .try_clone_reader()
        .context("Failed to clone PTY reader")?;

    std::thread::Builder::new()
        .name(format!("pty-reader-{}", session_id))
        .spawn(move || {
            let mut buf = [0u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => {
                        reader_exited.store(true, Ordering::SeqCst);
                        let _ = reader_tx.send(AppEvent::PtyExited { session_id });
                        break;
                    }
                    Ok(n) => {
                        if let Ok(mut p) = reader_parser.write() {
                            p.process(&buf[..n]);
                        }
                        if let Ok(mut t) = reader_last_output.write() {
                            *t = Instant::now();
                        }
                        let _ = reader_tx.send(AppEvent::PtyOutput);
                    }
                    Err(_) => {
                        reader_exited.store(true, Ordering::SeqCst);
                        let _ = reader_tx.send(AppEvent::PtyExited { session_id });
                        break;
                    }
                }
            }
        })
        .context("Failed to spawn PTY reader thread")?;

    // ── Writer thread (plain OS thread) ────────────────────────────
    let (write_tx, mut write_rx) = mpsc::unbounded_channel::<bytes::Bytes>();
    let mut writer = master.take_writer().context("Failed to take PTY writer")?;

    std::thread::Builder::new()
        .name(format!("pty-writer-{}", session_id))
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .build()
                .expect("failed to build writer runtime");

            rt.block_on(async {
                while let Some(data) = write_rx.recv().await {
                    if writer.write_all(&data).is_err() {
                        break;
                    }
                    let _ = writer.flush();
                }
            });
        })
        .context("Failed to spawn PTY writer thread")?;

    Ok(PtyHandle {
        parser,
        write_tx,
        master_pty: master,
        exited,
        last_output,
        tmux_session_name,
    })
}

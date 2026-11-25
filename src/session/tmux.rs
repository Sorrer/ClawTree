// Low-level tmux helpers: one place that builds every `tmux` command (so the
// `CLAWTREE_TMUX_SOCKET` override reaches all of them) and the functions that
// create clawtree's tmux sessions without attaching to them.
//
// Kept free of other crate modules so `lib.rs` can include it for integration
// tests alongside `layout.rs`.

use std::path::Path;
use std::process::Command;

/// Prefix for all tmux sessions we manage.
pub const TMUX_PREFIX: &str = "clawtree";
/// tmux session env var holding the worktree (or root / location) path.
pub const WORKTREE_ENV: &str = "CLAWTREE_WORKTREE";
/// tmux session env var set to `1` when Claude runs with `--dangerously-skip-permissions`.
pub const SKIP_PERMISSIONS_ENV: &str = "CLAWTREE_SKIP_PERMISSIONS";
/// tmux session env var holding the Claude conversation id the session was started with.
pub const CLAUDE_SESSION_ENV: &str = "CLAWTREE_CLAUDE_SESSION_ID";
/// Process env var: run every tmux command against `tmux -L <name>` instead of
/// the default server. Used by tests and manual crash simulations.
pub const SOCKET_ENV: &str = "CLAWTREE_TMUX_SOCKET";

/// The private tmux socket name from `CLAWTREE_TMUX_SOCKET`, if set.
pub fn socket_name() -> Option<String> {
    std::env::var(SOCKET_ENV).ok().filter(|s| !s.is_empty())
}

/// Arguments that select the tmux server (`-L <socket>`), or none.
pub fn socket_args() -> Vec<String> {
    match socket_name() {
        Some(s) => vec!["-L".to_string(), s],
        None => Vec::new(),
    }
}

/// A `tmux` command aimed at clawtree's tmux server. Every tmux invocation
/// goes through here.
pub fn tmux_cmd() -> Command {
    let mut cmd = Command::new("tmux");
    cmd.args(socket_args());
    cmd
}

/// Configure the tmux server for truecolor passthrough.
///
/// tmux decides whether to pass 24-bit RGB colors through to the outer terminal
/// (our PTY) or downconvert them to the 256-color palette.  It checks the `Tc`
/// capability in `terminal-overrides` (tmux < 3.2) and the `RGB` flag in
/// `terminal-features` (tmux >= 3.2).  We need both to cover all tmux versions.
///
/// We also set `default-terminal` to `tmux-256color` so programs *inside* tmux
/// (e.g. Claude Code) see a TERM that advertises full color.
///
/// Runs until it succeeds once per process: with no tmux server running the
/// `set` commands fail, so callers invoke it again after creating a session.
pub fn ensure_truecolor() {
    use std::sync::atomic::{AtomicBool, Ordering};
    static DONE: AtomicBool = AtomicBool::new(false);
    if DONE.load(Ordering::Relaxed) {
        return;
    }
    let ok = tmux_cmd()
        .args(["set", "-sa", "terminal-overrides", ",*:Tc"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !ok {
        return; // no server yet; retried after the next new-session
    }
    let _ = tmux_cmd()
        .args(["set", "-sa", "terminal-features", ",xterm-256color:RGB"])
        .output();
    let _ = tmux_cmd()
        .args(["set", "-g", "default-terminal", "tmux-256color"])
        .output();
    DONE.store(true, Ordering::Relaxed);
}

/// Whether a tmux session with this exact name exists.
pub fn session_exists(name: &str) -> bool {
    // `=name` forces an exact match; plain `-t name` also matches prefixes.
    tmux_cmd()
        .args(["has-session", "-t", &format!("={}", name)])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Read one variable from a tmux session's environment.
pub fn show_env(name: &str, var: &str) -> Option<String> {
    let o = tmux_cmd()
        .args(["show-environment", "-t", name, var])
        .output()
        .ok()?;
    if !o.status.success() {
        return None;
    }
    let line = String::from_utf8_lossy(&o.stdout);
    line.trim()
        .strip_prefix(&format!("{}=", var))
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty())
}

/// Set one variable in a tmux session's environment.
pub fn set_env(name: &str, var: &str, value: &str) {
    let _ = tmux_cmd()
        .args(["set-environment", "-t", name, var, value])
        .output();
}

/// How to start `claude` in a new session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaudeLaunch {
    /// A new conversation, optionally with a pre-assigned id (`--session-id`)
    /// and a first prompt.
    New {
        session_id: Option<String>,
        initial_prompt: Option<String>,
    },
    /// `--resume <id>`: reopen a known conversation, optionally in the
    /// permission mode it was last using (`--permission-mode`).
    Resume {
        id: String,
        permission_mode: Option<String>,
    },
    /// `--continue`: the most recent conversation in the directory.
    Continue,
}

impl ClaudeLaunch {
    /// The conversation id this launch is known to use, if any.
    pub fn known_session_id(&self) -> Option<&str> {
        match self {
            ClaudeLaunch::New { session_id, .. } => session_id.as_deref(),
            ClaudeLaunch::Resume { id, .. } => Some(id),
            ClaudeLaunch::Continue => None,
        }
    }

    /// Arguments passed to `claude` after any permission flag.
    pub fn claude_args(&self) -> Vec<String> {
        match self {
            ClaudeLaunch::New {
                session_id,
                initial_prompt,
            } => {
                let mut v = Vec::new();
                if let Some(id) = session_id {
                    v.push("--session-id".to_string());
                    v.push(id.clone());
                }
                if let Some(p) = initial_prompt {
                    v.push(p.clone());
                }
                v
            }
            ClaudeLaunch::Resume {
                id,
                permission_mode,
            } => {
                let mut v = vec!["--resume".to_string(), id.clone()];
                if let Some(mode) = permission_mode {
                    v.push("--permission-mode".to_string());
                    v.push(mode.clone());
                }
                v
            }
            ClaudeLaunch::Continue => vec!["--continue".to_string()],
        }
    }
}

fn run_new_session(
    tmux_name: &str,
    start_dir: &Path,
    rows: u16,
    cols: u16,
    command: &[String],
) -> anyhow::Result<()> {
    ensure_truecolor();
    let mut args = vec![
        "new-session".to_string(),
        "-d".to_string(),
        "-s".to_string(),
        tmux_name.to_string(),
        "-x".to_string(),
        cols.to_string(),
        "-y".to_string(),
        rows.to_string(),
        "-c".to_string(),
        start_dir.to_string_lossy().to_string(),
    ];
    args.extend(command.iter().cloned());
    let output = tmux_cmd()
        .args(&args)
        .current_dir(start_dir)
        .env("TERM", "xterm-256color")
        .output()
        .map_err(|e| anyhow::anyhow!("Failed to run tmux: {}", e))?;
    if !output.status.success() {
        anyhow::bail!(
            "tmux new-session failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    // A fresh server only exists now; apply the server options if they failed earlier.
    ensure_truecolor();
    Ok(())
}

/// Options every clawtree session gets after creation.
fn configure_session(tmux_name: &str, worktree: &Path) {
    // Disable the tmux status bar so the pane fills the full PTY dimensions.
    let _ = tmux_cmd()
        .args(["set-option", "-t", tmux_name, "status", "off"])
        .output();
    set_env(tmux_name, WORKTREE_ENV, &worktree.to_string_lossy());
    set_env(tmux_name, "COLORTERM", "truecolor");
    // Allow passthrough of escape sequences that tmux normally strips
    // (OSC for hyperlinks, styled underlines, kitty graphics, etc.)
    let _ = tmux_cmd()
        .args(["set-option", "-t", tmux_name, "allow-passthrough", "on"])
        .output();
    // Clear the default pane title (usually username@host) so it doesn't
    // show up before Claude sets its own title.
    let _ = tmux_cmd()
        .args(["select-pane", "-t", tmux_name, "-T", ""])
        .output();
}

/// Create a detached tmux session running `claude` in `working_dir`.
#[allow(clippy::too_many_arguments)]
pub fn create_claude_session(
    working_dir: &Path,
    rows: u16,
    cols: u16,
    skip_permissions: bool,
    tmux_name: &str,
    launch: &ClaudeLaunch,
    extra_env: Option<(&str, &str)>,
) -> anyhow::Result<()> {
    // Run claude with COLORTERM=truecolor so it emits 24-bit RGB colors
    let mut cmd = vec!["env".to_string(), "COLORTERM=truecolor".to_string()];
    if let Some((k, v)) = extra_env {
        cmd.push(format!("{}={}", k, v));
    }
    cmd.push("claude".to_string());
    if skip_permissions {
        cmd.push("--dangerously-skip-permissions".to_string());
    }
    cmd.extend(launch.claude_args());

    run_new_session(tmux_name, working_dir, rows, cols, &cmd)?;
    configure_session(tmux_name, working_dir);
    if skip_permissions {
        set_env(tmux_name, SKIP_PERMISSIONS_ENV, "1");
    }
    if let Some(id) = launch.known_session_id() {
        set_env(tmux_name, CLAUDE_SESSION_ENV, id);
    }
    Ok(())
}

/// Create a detached tmux session running the user's shell. `worktree` is
/// recorded as the session's home; the shell starts in `start_dir`.
pub fn create_terminal_session(
    worktree: &Path,
    start_dir: &Path,
    rows: u16,
    cols: u16,
    tmux_name: &str,
) -> anyhow::Result<()> {
    run_new_session(tmux_name, start_dir, rows, cols, &[])?;
    configure_session(tmux_name, worktree);
    Ok(())
}

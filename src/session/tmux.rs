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
/// tmux session env var holding the agent conversation id the session was started with
/// (the name predates other agents).
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

/// tmux session env var naming the coding agent a session runs (see
/// [`Agent::id`]). Unset means Claude Code (sessions from before agents).
pub const AGENT_ENV: &str = "CLAWTREE_AGENT";

/// Hidden `clawtree` subcommand agy runs as its title command: reads agy's
/// status JSON on stdin and prints the pane title (see [`agy_title`]).
pub const AGY_TITLE_SUBCOMMAND: &str = "__agy-title";

/// The coding agent a session runs.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Default, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum Agent {
    /// Claude Code (`claude`).
    #[default]
    Claude,
    /// Google's Antigravity CLI (`agy`), the successor of Gemini CLI.
    Antigravity,
}

impl Agent {
    pub const ALL: &'static [Agent] = &[Agent::Claude, Agent::Antigravity];

    /// The executable to run.
    pub fn binary(self) -> &'static str {
        match self {
            Agent::Claude => "claude",
            Agent::Antigravity => "agy",
        }
    }

    /// Stable id stored in tmux ([`AGENT_ENV`]) and the layout manifest.
    pub fn id(self) -> &'static str {
        match self {
            Agent::Claude => "claude",
            Agent::Antigravity => "antigravity",
        }
    }

    pub fn from_id(s: &str) -> Option<Agent> {
        Agent::ALL.iter().copied().find(|a| a.id() == s)
    }

    /// Short name for the UI.
    pub fn name(self) -> &'static str {
        self.binary()
    }

    pub fn is_claude(&self) -> bool {
        *self == Agent::Claude
    }

    /// Whether clawtree can choose the conversation id at launch
    /// (`claude --session-id`). agy picks its own; the poller finds it.
    pub fn assigns_conversation_id(self) -> bool {
        self == Agent::Claude
    }

    /// The agent whose binary `arg0` (a path or bare name) is, if any.
    pub fn from_program(arg0: &str) -> Option<Agent> {
        let base = arg0.rsplit('/').next().unwrap_or(arg0);
        Agent::ALL.iter().copied().find(|a| a.binary() == base)
    }
}

/// Modes `agy --mode` accepts. agy's fourth mode, "default" (everything asks),
/// has no flag value; clawtree passes it as a session-only setting.
pub const AGY_MODES: &[&str] = &["accept-edits", "auto", "plan"];

/// How to start an agent in a new session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentLaunch {
    /// A new conversation, optionally with a pre-assigned id (`--session-id`,
    /// Claude only) and a first prompt.
    New {
        session_id: Option<String>,
        initial_prompt: Option<String>,
    },
    /// Reopen a known conversation, optionally in the permission mode it was
    /// last using.
    Resume {
        id: String,
        permission_mode: Option<String>,
    },
    /// The most recent conversation in the directory.
    Continue,
}

impl AgentLaunch {
    /// The conversation id this launch is known to use, if any.
    pub fn known_session_id(&self) -> Option<&str> {
        match self {
            AgentLaunch::New { session_id, .. } => session_id.as_deref(),
            AgentLaunch::Resume { id, .. } => Some(id),
            AgentLaunch::Continue => None,
        }
    }

    /// Arguments passed to `claude` after any permission flag.
    pub fn claude_args(&self) -> Vec<String> {
        match self {
            AgentLaunch::New {
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
            AgentLaunch::Resume {
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
            AgentLaunch::Continue => vec!["--continue".to_string()],
        }
    }

    /// Arguments passed to `agy` after any permission flag. `title_command`
    /// is installed as agy's title command for this session only
    /// (`--settings` is never saved), so the pane title carries agy's state.
    /// With `allow_mcp`, the clawtree MCP tools run without an approval prompt.
    pub fn agy_args(&self, title_command: Option<&str>, allow_mcp: bool) -> Vec<String> {
        let mut v = Vec::new();
        let mut settings = serde_json::Map::new();
        if let Some(cmd) = title_command {
            settings.insert(
                "title".into(),
                serde_json::json!({ "type": "command", "command": cmd }),
            );
        }
        if allow_mcp {
            settings.insert(
                "permissions".into(),
                serde_json::json!({
                    "allow": MCP_TOOLS
                        .iter()
                        .map(|t| format!("mcp({}/{})", MCP_SERVER_NAME, t))
                        .collect::<Vec<_>>()
                }),
            );
        }
        match self {
            AgentLaunch::New { initial_prompt, .. } => {
                if let Some(p) = initial_prompt {
                    v.push("-i".to_string());
                    v.push(p.clone());
                }
            }
            AgentLaunch::Resume {
                id,
                permission_mode,
            } => {
                v.push("--conversation".to_string());
                v.push(id.clone());
                match permission_mode.as_deref() {
                    Some(m) if AGY_MODES.contains(&m) => {
                        v.push("--mode".to_string());
                        v.push(m.to_string());
                    }
                    Some("default") => {
                        settings.insert("agentMode".into(), serde_json::json!(""));
                    }
                    _ => {}
                }
            }
            AgentLaunch::Continue => v.push("--continue".to_string()),
        }
        if !settings.is_empty() {
            v.push("--settings".to_string());
            v.push(serde_json::Value::Object(settings).to_string());
        }
        v
    }

    /// Arguments for `agent` after any permission flag.
    pub fn args(&self, agent: Agent, title_command: Option<&str>) -> Vec<String> {
        match agent {
            Agent::Claude => self.claude_args(),
            Agent::Antigravity => self.agy_args(title_command, mcp_enabled()),
        }
    }
}

/// The running clawtree executable, for commands agents call back into.
/// After an in-place update the old inode is gone and Linux reports
/// `<path> (deleted)`; the path itself then holds the new binary.
pub fn clawtree_exe() -> Option<std::path::PathBuf> {
    let p = std::env::current_exe().ok()?;
    let s = p.to_string_lossy();
    Some(match s.strip_suffix(" (deleted)") {
        Some(stripped) => std::path::PathBuf::from(stripped),
        None => p,
    })
}

/// The name of clawtree's MCP server (`clawtree mcp`) in agents' configs.
pub const MCP_SERVER_NAME: &str = "clawtree";
/// The tools `clawtree mcp` offers (agy's allow rules name each one).
pub const MCP_TOOLS: &[&str] = &["list_sessions", "send_message"];
/// Process env var: set to `1` to start agents without the clawtree MCP server.
pub const NO_MCP_ENV: &str = "CLAWTREE_NO_MCP";

pub fn mcp_enabled() -> bool {
    std::env::var(NO_MCP_ENV).map_or(true, |v| v.is_empty() || v == "0")
}

/// Claude Code flags that load the clawtree MCP server for this session only
/// and let its tools run without a prompt. `--mcp-config` takes several
/// values, so it is followed by another flag (`--settings`) to end the list.
pub fn claude_mcp_args() -> Vec<String> {
    let Some(exe) = clawtree_exe().filter(|_| mcp_enabled()) else {
        return Vec::new();
    };
    let config = serde_json::json!({
        "mcpServers": { MCP_SERVER_NAME: { "command": exe, "args": ["mcp"] } }
    });
    let settings = serde_json::json!({
        "permissions": { "allow": [format!("mcp__{}", MCP_SERVER_NAME)] }
    });
    vec![
        "--mcp-config".to_string(),
        config.to_string(),
        "--settings".to_string(),
        settings.to_string(),
    ]
}

/// agy has no per-session MCP setting, so register `clawtree mcp` in its
/// config (`agy mcp add`, which also updates a moved binary) when it isn't
/// there yet. Runs in the background; a failure just leaves agy without it.
pub fn ensure_agy_mcp_registered(home: &Path) {
    let Some(exe) = clawtree_exe().filter(|_| mcp_enabled()) else {
        return;
    };
    let exe = exe.to_string_lossy().to_string();
    let config = home.join(".gemini").join("config").join("mcp_config.json");
    let registered = std::fs::read(&config)
        .ok()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
        .and_then(|v| v.get("mcpServers")?.get(MCP_SERVER_NAME).cloned())
        .is_some_and(|s| {
            s.get("command").and_then(|c| c.as_str()) == Some(exe.as_str())
                && s.get("args") == Some(&serde_json::json!(["mcp"]))
        });
    if registered {
        return;
    }
    std::thread::spawn(move || {
        let _ = Command::new(Agent::Antigravity.binary())
            .args(["mcp", "add", MCP_SERVER_NAME, &exe, "mcp"])
            .stdin(std::process::Stdio::null())
            .output();
    });
}

/// The shell command agy runs to title the pane: `'<clawtree>' __agy-title`.
pub fn agy_title_command() -> Option<String> {
    let exe = clawtree_exe()?;
    let quoted = exe.to_string_lossy().replace('\'', "'\\''");
    Some(format!("'{}' {}", quoted, AGY_TITLE_SUBCOMMAND))
}

/// agy `agent_state` values that mean a turn is in progress.
const AGY_BUSY_STATES: &[&str] = &["working", "tool_use", "reviewing", "initializing"];

/// The pane title for agy's status JSON (its title/statusline payload):
/// `"<glyph> <conversation title>"`, using Claude Code's title glyphs — a
/// spinner while working, `✳` otherwise — so the sidebar's title handling
/// works unchanged. A pending tool approval is not "working": the screen
/// check then reports that it needs input.
pub fn agy_title(status: &serde_json::Value) -> String {
    let state = status
        .get("agent_state")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let confirm = status
        .get("tool_confirmation_pending")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let busy = AGY_BUSY_STATES.contains(&state) && !confirm;
    let glyph = if busy { '\u{25D0}' } else { '\u{2733}' };
    let title = status
        .get("conversation_title")
        .and_then(|v| v.as_str())
        .map(|t| t.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| "agy".to_string());
    format!("{} {}", glyph, title)
}

/// Whether `program` resolves to an executable on `PATH`.
pub fn program_on_path(program: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| {
        std::fs::metadata(dir.join(program))
            .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    })
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

/// Create a detached tmux session running `agent` in `working_dir`.
#[allow(clippy::too_many_arguments)]
pub fn create_agent_session(
    agent: Agent,
    working_dir: &Path,
    rows: u16,
    cols: u16,
    skip_permissions: bool,
    tmux_name: &str,
    launch: &AgentLaunch,
    extra_env: Option<(&str, &str)>,
) -> anyhow::Result<()> {
    // Run the agent with COLORTERM=truecolor so it emits 24-bit RGB colors
    let mut cmd = vec!["env".to_string(), "COLORTERM=truecolor".to_string()];
    if let Some((k, v)) = extra_env {
        cmd.push(format!("{}={}", k, v));
    }
    cmd.push(agent.binary().to_string());
    match agent {
        Agent::Claude => cmd.extend(claude_mcp_args()),
        Agent::Antigravity => {
            ensure_agy_mcp_registered(Path::new(&std::env::var("HOME").unwrap_or_default()))
        }
    }
    if skip_permissions {
        // Both agents use the same flag name.
        cmd.push("--dangerously-skip-permissions".to_string());
    }
    let title_command = match agent {
        Agent::Antigravity => agy_title_command(),
        Agent::Claude => None,
    };
    cmd.extend(launch.args(agent, title_command.as_deref()));

    run_new_session(tmux_name, working_dir, rows, cols, &cmd)?;
    configure_session(tmux_name, working_dir);
    if !agent.is_claude() {
        set_env(tmux_name, AGENT_ENV, agent.id());
    }
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

#[cfg(test)]
mod agent_tests {
    use super::*;

    #[test]
    fn agent_ids_round_trip() {
        for &a in Agent::ALL {
            assert_eq!(Agent::from_id(a.id()), Some(a));
        }
        assert_eq!(
            Agent::from_program("/home/u/.local/bin/agy"),
            Some(Agent::Antigravity)
        );
        assert_eq!(Agent::from_program("claude"), Some(Agent::Claude));
        assert_eq!(Agent::from_program("bash"), None);
        assert!(Agent::Claude.assigns_conversation_id());
        assert!(!Agent::Antigravity.assigns_conversation_id());
    }

    #[test]
    fn agy_launch_args() {
        let title = Some("'/x/clawtree' __agy-title");
        let settings_title =
            r#"{"title":{"command":"'/x/clawtree' __agy-title","type":"command"}}"#;
        let new = AgentLaunch::New {
            session_id: Some("ignored".into()),
            initial_prompt: Some("fix the bug".into()),
        };
        assert_eq!(
            new.agy_args(title, false),
            vec!["-i", "fix the bug", "--settings", settings_title]
        );
        let resume = |mode: Option<&str>| AgentLaunch::Resume {
            id: "abc".into(),
            permission_mode: mode.map(|m| m.to_string()),
        };
        assert_eq!(
            resume(Some("plan")).agy_args(None, false),
            vec!["--conversation", "abc", "--mode", "plan"]
        );
        assert_eq!(
            resume(Some("default")).agy_args(None, false),
            vec!["--conversation", "abc", "--settings", r#"{"agentMode":""}"#]
        );
        // Claude's mode names mean nothing to agy.
        assert_eq!(
            resume(Some("bypassPermissions")).agy_args(None, false),
            vec!["--conversation", "abc"]
        );
        assert_eq!(AgentLaunch::Continue.agy_args(None, false), vec!["--continue"]);
        assert_eq!(
            AgentLaunch::Continue.agy_args(None, true),
            vec![
                "--continue",
                "--settings",
                r#"{"permissions":{"allow":["mcp(clawtree/list_sessions)","mcp(clawtree/send_message)"]}}"#
            ]
        );
        // Claude's arguments are unchanged.
        assert_eq!(
            resume(Some("plan")).args(Agent::Claude, title),
            vec!["--resume", "abc", "--permission-mode", "plan"]
        );
    }

    #[test]
    fn agy_title_uses_claude_glyphs() {
        let v = |j: &str| serde_json::from_str::<serde_json::Value>(j).unwrap();
        assert_eq!(
            agy_title(&v(
                r#"{"agent_state":"working","conversation_title":"Fix  login\nbug"}"#
            )),
            "\u{25D0} Fix login bug"
        );
        assert_eq!(agy_title(&v(r#"{"agent_state":"idle"}"#)), "\u{2733} agy");
        assert_eq!(
            agy_title(&v(
                r#"{"agent_state":"tool_use","tool_confirmation_pending":true}"#
            )),
            "\u{2733} agy"
        );
        assert_eq!(agy_title(&serde_json::Value::Null), "\u{2733} agy");
    }

    #[test]
    fn title_command_quotes_the_path() {
        let cmd = agy_title_command().unwrap();
        assert!(
            cmd.starts_with('\'') && cmd.ends_with(AGY_TITLE_SUBCOMMAND),
            "{}",
            cmd
        );
    }
}

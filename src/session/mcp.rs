//! `clawtree mcp`: a stdio MCP server that lets agent sessions (Claude Code,
//! agy) find each other and exchange messages.
//!
//! Every agent session clawtree starts launches its own copy (Claude through
//! `--mcp-config`, agy through its MCP config), and learns which session it
//! is from `TMUX_PANE`. It offers two tools:
//!
//! - `list_sessions`: every clawtree agent session on this tmux server, with
//!   its nickname, agent, folder and live status;
//! - `send_message`: drop a message in the target's mailbox
//!   (`~/.clawtree/mailbox/<tmux name>/`).
//!
//! A message is typed into the target's prompt once the target is idle, the
//! way the prompt queue does it. The target's own server delivers its mailbox
//! (it holds `receiver.lock` while running); mail for a session without one
//! (started before messaging existed) is delivered by any running server.
//! `deliver.lock` keeps two servers from typing into a pane at once.

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{json, Value};

use super::layout::{self, LoadOutcome};
use super::{screen_status, strip_title_status_glyph, tmux};
use crate::app::AgentStatus;

/// `clawtree mcp`
pub const SUBCOMMAND: &str = "mcp";
/// The name agents know the server by (tools appear as `mcp__clawtree__…`).
pub const SERVER_NAME: &str = "clawtree";
const PROTOCOL_VERSION: &str = "2025-06-18";
/// How often a server checks whether it can deliver waiting messages.
const DELIVERY_POLL: Duration = Duration::from_secs(2);
/// Two screen captures this far apart must match before a message is typed,
/// so a session that only just went quiet isn't interrupted mid-redraw.
const SETTLE: Duration = Duration::from_millis(1500);

fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_default())
}

fn mailbox_root(home: &Path) -> PathBuf {
    home.join(".clawtree").join("mailbox")
}

/// tmux names clawtree generates are `[A-Za-z0-9._-]`; anything else could
/// escape the mailbox folder.
fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

fn mailbox_dir(home: &Path, name: &str) -> Option<PathBuf> {
    valid_name(name).then(|| mailbox_root(home).join(name))
}

// ── Sessions ─────────────────────────────────────────────────────────────

/// A clawtree agent session, as seen from tmux.
#[derive(Debug, Clone)]
struct Peer {
    name: String,
    pane: String,
    agent: tmux::Agent,
    title: String,
    path: Option<PathBuf>,
    nickname: Option<String>,
    project: Option<PathBuf>,
}

impl Peer {
    fn label(&self) -> String {
        strip_title_status_glyph(&self.title).trim().to_string()
    }

    fn folder(&self) -> Option<String> {
        self.path
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().to_string())
    }
}

fn tmux_out(args: &[&str]) -> Option<String> {
    let o = tmux::tmux_cmd().args(args).output().ok()?;
    o.status
        .success()
        .then(|| String::from_utf8_lossy(&o.stdout).to_string())
}

/// The tmux session this server runs in.
fn own_session_name() -> Option<String> {
    let pane = std::env::var("TMUX_PANE").ok().filter(|p| !p.is_empty())?;
    tmux_out(&["display-message", "-p", "-t", &pane, "#{session_name}"])
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Nicknames and projects from every layout manifest, by tmux name.
fn manifest_info() -> HashMap<String, (Option<String>, PathBuf)> {
    let mut out = HashMap::new();
    for path in layout::all_manifest_paths() {
        if let LoadOutcome::Loaded(m) = layout::load_from(&path) {
            for e in m.entries {
                out.insert(e.tmux_name, (e.nickname, m.bare_repo_path.clone()));
            }
        }
    }
    out
}

/// Every clawtree agent session (terminals excluded).
fn list_peers() -> Vec<Peer> {
    let Some(text) = tmux_out(&[
        "list-panes",
        "-a",
        "-F",
        "#{session_name}\t#{pane_id}\t#{pane_pid}\t#{pane_title}",
    ]) else {
        return Vec::new();
    };
    let info = manifest_info();
    let mut seen = HashSet::new();
    let mut peers = Vec::new();
    for line in text.lines() {
        let mut parts = line.splitn(4, '\t');
        let (Some(name), Some(pane), Some(pid), title) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        if !name.starts_with(&format!("{}-", tmux::TMUX_PREFIX))
            || name.contains("-term-")
            || !seen.insert(name.to_string())
        {
            continue;
        }
        let env = tmux_out(&["show-environment", "-t", name]).unwrap_or_default();
        let var = |k: &str| {
            env.lines()
                .find_map(|l| l.strip_prefix(&format!("{}=", k)))
                .map(|v| v.to_string())
        };
        let agent = var(tmux::AGENT_ENV)
            .and_then(|a| tmux::Agent::from_id(&a))
            .or_else(|| {
                pid.parse::<u32>()
                    .ok()
                    .and_then(super::process_program)
                    .and_then(|p| tmux::Agent::from_program(&p))
            })
            .unwrap_or_default();
        let (nickname, project) = match info.get(name) {
            Some((n, p)) => (n.clone(), Some(p.clone())),
            None => (None, None),
        };
        peers.push(Peer {
            name: name.to_string(),
            pane: pane.to_string(),
            agent,
            title: title.unwrap_or("").to_string(),
            path: var(tmux::WORKTREE_ENV).map(PathBuf::from),
            nickname,
            project,
        });
    }
    peers
}

fn capture(pane: &str) -> Option<String> {
    tmux_out(&["capture-pane", "-p", "-t", pane])
}

fn status_of(peer: &Peer) -> AgentStatus {
    match capture(&peer.pane) {
        Some(screen) => screen_status(peer.agent, &peer.title, &screen),
        None => AgentStatus::Exited,
    }
}

fn status_label(s: AgentStatus) -> &'static str {
    match s {
        AgentStatus::Working => "working",
        AgentStatus::AwaitingAgents => "waiting_on_subagents",
        AgentStatus::Idle => "idle",
        AgentStatus::NeedsInput => "needs_input",
        AgentStatus::RateLimited => "rate_limited",
        AgentStatus::Exited => "exited",
    }
}

/// Whether some server is running for `name` (it holds `receiver.lock`).
fn has_receiver(home: &Path, name: &str) -> bool {
    let Some(dir) = mailbox_dir(home, name) else {
        return false;
    };
    // Taking the lock succeeds only when nobody holds it.
    matches!(layout::try_lock(&dir.join("receiver")), Ok(None))
}

/// Find the session `to` names: its tmux name, else a nickname or title
/// (case-insensitive, must be unique).
fn resolve<'a>(peers: &'a [Peer], to: &str) -> Result<&'a Peer, String> {
    let to = to.trim();
    if let Some(p) = peers.iter().find(|p| p.name == to) {
        return Ok(p);
    }
    let low = to.to_lowercase();
    let keys: [fn(&Peer) -> Option<String>; 2] = [
        |p| p.nickname.clone().map(|n| n.to_lowercase()),
        |p| Some(p.label().to_lowercase()),
    ];
    for key in keys {
        let hits: Vec<&Peer> = peers
            .iter()
            .filter(|p| key(p).as_deref() == Some(low.as_str()))
            .collect();
        match hits.len() {
            1 => return Ok(hits[0]),
            0 => {}
            _ => {
                return Err(format!(
                    "\"{}\" matches several sessions ({}); use the session name",
                    to,
                    hits.iter().map(|p| p.name.as_str()).collect::<Vec<_>>().join(", ")
                ))
            }
        }
    }
    Err(format!(
        "no clawtree session named \"{}\"; call list_sessions for the names",
        to
    ))
}

// ── Mailbox ──────────────────────────────────────────────────────────────

/// Write a message into `to`'s mailbox (atomically: temp file, then rename).
fn post(home: &Path, to: &str, message: &Value) -> std::io::Result<PathBuf> {
    use std::sync::atomic::{AtomicU32, Ordering};
    static SEQ: AtomicU32 = AtomicU32::new(0);
    let dir = mailbox_dir(home, to)
        .ok_or_else(|| std::io::Error::other("invalid session name"))?;
    std::fs::create_dir_all(&dir)?;
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let stem = format!(
        "{:013}-{}-{}",
        millis,
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    );
    let tmp = dir.join(format!(".{}.tmp", stem));
    let path = dir.join(format!("{}.json", stem));
    std::fs::write(&tmp, serde_json::to_vec(message)?)?;
    std::fs::rename(&tmp, &path)?;
    Ok(path)
}

/// Waiting messages for `name`, oldest first.
fn pending(home: &Path, name: &str) -> Vec<PathBuf> {
    let Some(dir) = mailbox_dir(home, name) else {
        return Vec::new();
    };
    let mut v: Vec<PathBuf> = std::fs::read_dir(&dir)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|x| x == "json"))
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

/// The line typed into the receiving session.
fn render(message: &Value) -> String {
    let s = |k: &str| message.get(k).and_then(|v| v.as_str()).unwrap_or("");
    let from = s("from");
    let mut who = if from.is_empty() {
        "outside clawtree".to_string()
    } else {
        from.to_string()
    };
    let about: Vec<&str> = [s("from_agent"), s("from_folder")]
        .into_iter()
        .filter(|x| !x.is_empty())
        .collect();
    if !about.is_empty() {
        who = format!("{} ({})", who, about.join(" · "));
    }
    // Sessions started before messaging have no send_message tool, but every
    // agent can run a shell command.
    let reply = if from.is_empty() {
        String::new()
    } else {
        format!(
            " — reply with the clawtree send_message tool (to: \"{0}\"), or run: clawtree send {0} \"<reply>\"",
            from
        )
    };
    format!("[clawtree message from {}{}] {}", who, reply, s("text").trim())
}

/// The screen with its digits removed: the running-agent panel's clocks
/// (`7h 25m 54s`) tick every second even when nothing else is happening.
fn settle_key(screen: &str) -> String {
    screen.chars().filter(|c| !c.is_ascii_digit()).collect()
}

/// Text on one captured line (`capture-pane -e`) with, per character,
/// whether it is drawn as ghost text: dim (Claude's suggestions and
/// placeholder) or grey (agy's placeholder).
fn styled_chars(line: &str) -> Vec<(char, bool)> {
    let mut out = Vec::new();
    let (mut dim, mut grey) = (false, false);
    let mut it = line.chars().peekable();
    while let Some(c) = it.next() {
        if c != '\u{1b}' {
            out.push((c, dim || grey));
            continue;
        }
        if it.peek() != Some(&'[') {
            continue;
        }
        it.next();
        let mut params = String::new();
        let mut fin = ' ';
        for d in it.by_ref() {
            if d.is_ascii_alphabetic() || d == '~' {
                fin = d;
                break;
            }
            params.push(d);
        }
        if fin != 'm' {
            continue;
        }
        let codes: Vec<u32> = params.split(';').map(|p| p.parse().unwrap_or(0)).collect();
        let mut i = 0;
        while i < codes.len() {
            match codes[i] {
                0 => (dim, grey) = (false, false),
                2 => dim = true,
                22 => dim = false,
                90 => grey = true,
                30..=37 | 39 | 91..=97 => grey = false,
                38 => {
                    match codes.get(i + 1) {
                        Some(5) => {
                            let n = codes.get(i + 2).copied().unwrap_or(0);
                            grey = n == 8 || (240..=250).contains(&n);
                            i += 2;
                        }
                        Some(2) => {
                            grey = false;
                            i += 4;
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
            i += 1;
        }
    }
    out
}

/// Whether the prompt holds text the user typed (as opposed to empty, or a
/// dim/grey suggestion or placeholder). A message typed now would be glued
/// onto their draft and submitted with it.
fn has_draft(agent: tmux::Agent, styled_screen: &str) -> bool {
    let prompt = match agent {
        tmux::Agent::Claude => '\u{276f}', // ❯
        tmux::Agent::Antigravity => '>',
    };
    for line in styled_screen.lines().rev().take(40) {
        let chars = styled_chars(line);
        let start = chars
            .iter()
            .position(|(c, _)| !c.is_whitespace() && !matches!(c, '│' | '┃' | '|' | '\u{a0}'));
        let Some(start) = start else { continue };
        if chars[start].0 != prompt {
            continue;
        }
        return chars[start + 1..]
            .iter()
            .any(|&(c, ghost)| !ghost && !c.is_whitespace() && !matches!(c, '│' | '┃' | '\u{a0}'));
    }
    false
}

/// Ready to be typed into: idle (or only waiting on its sub-agents), not
/// scrolled back, no draft in its prompt, and its screen not changing.
fn ready_for_message(peer: &Peer) -> bool {
    let in_mode = tmux_out(&["display-message", "-p", "-t", &peer.pane, "#{pane_in_mode}"]);
    if in_mode.as_deref().map(str::trim) != Some("0") {
        return false;
    }
    let Some(before) = capture(&peer.pane) else {
        return false;
    };
    if !matches!(
        screen_status(peer.agent, &peer.title, &before),
        AgentStatus::Idle | AgentStatus::AwaitingAgents
    ) {
        return false;
    }
    let styled = tmux_out(&["capture-pane", "-e", "-p", "-t", &peer.pane]).unwrap_or_default();
    if has_draft(peer.agent, &styled) {
        return false;
    }
    std::thread::sleep(SETTLE);
    capture(&peer.pane).map(|after| settle_key(&after)) == Some(settle_key(&before))
}

/// Type `text` into `pane` and submit it, like the prompt queue: literal
/// keys (multi-line text as a bracketed paste), then Enter.
fn type_into(pane: &str, text: &str) -> bool {
    let ok = if text.contains('\n') {
        let buffer = format!("clawtree-msg-{}", std::process::id());
        let loaded = tmux::tmux_cmd()
            .args(["load-buffer", "-b", &buffer, "-"])
            .stdin(std::process::Stdio::piped())
            .spawn()
            .and_then(|mut child| {
                if let Some(mut stdin) = child.stdin.take() {
                    stdin.write_all(text.as_bytes())?;
                }
                child.wait()
            })
            .map(|s| s.success())
            .unwrap_or(false);
        loaded
            && tmux_out(&["paste-buffer", "-p", "-d", "-b", &buffer, "-t", pane]).is_some()
    } else {
        tmux_out(&["send-keys", "-t", pane, "-l", text]).is_some()
    };
    if !ok {
        return false;
    }
    std::thread::sleep(Duration::from_millis(300));
    tmux_out(&["send-keys", "-t", pane, "Enter"]).is_some()
}

/// Deliver the oldest waiting message for `name` if it can take one now.
/// Returns how many messages are still waiting.
fn deliver_one(home: &Path, name: &str) -> usize {
    let waiting = pending(home, name);
    if waiting.is_empty() {
        return 0;
    }
    let Some(dir) = mailbox_dir(home, name) else {
        return 0;
    };
    let Ok(Some(_lock)) = layout::try_lock(&dir.join("deliver")) else {
        return waiting.len(); // someone else is delivering
    };
    let Some(peer) = list_peers().into_iter().find(|p| p.name == name) else {
        return waiting.len(); // not running right now; kept for when it is
    };
    if !ready_for_message(&peer) {
        return waiting.len();
    }
    // Re-list under the lock: another server may have just delivered.
    let Some(first) = pending(home, name).into_iter().next() else {
        return 0;
    };
    let message: Value = std::fs::read(&first)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or(Value::Null);
    if message.is_null() {
        let _ = std::fs::remove_file(&first); // unreadable: drop it
    } else if type_into(&peer.pane, &render(&message)) {
        let _ = std::fs::remove_file(&first);
    }
    pending(home, name).len()
}

// ── Tools ────────────────────────────────────────────────────────────────

struct Server {
    home: PathBuf,
    me: Option<String>,
}

fn tool_specs() -> Value {
    json!([
        {
            "name": "list_sessions",
            "description": "List the other coding-agent sessions (Claude Code, agy) running in clawtree on this machine, with their name, nickname, agent, folder and status (idle, working, needs_input, waiting_on_subagents). Use a name from here with send_message.",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "send_message",
            "description": "Send a message to another clawtree agent session. It is typed into that session's prompt as soon as the session is idle (it waits while the session is working), prefixed with who it is from so the other agent can reply with send_message. Returns immediately; replies arrive as new prompts in this session.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "to": { "type": "string", "description": "Session name from list_sessions (or its nickname)." },
                    "message": { "type": "string", "description": "What to tell the other agent." }
                },
                "required": ["to", "message"]
            }
        }
    ])
}

impl Server {
    fn list_sessions(&self) -> Result<String, String> {
        let peers = list_peers();
        let sessions: Vec<Value> = peers
            .iter()
            .map(|p| {
                json!({
                    "name": p.name,
                    "you": self.me.as_deref() == Some(p.name.as_str()),
                    "agent": p.agent.name(),
                    "nickname": p.nickname,
                    "title": p.label(),
                    "status": status_label(status_of(p)),
                    "folder": p.path,
                    "project": p.project,
                    "messages_waiting": pending(&self.home, &p.name).len(),
                })
            })
            .collect();
        serde_json::to_string_pretty(&json!({ "you": self.me, "sessions": sessions }))
            .map_err(|e| e.to_string())
    }

    fn send_message(&self, args: &Value) -> Result<String, String> {
        let to = args.get("to").and_then(|v| v.as_str()).unwrap_or("");
        let text = args.get("message").and_then(|v| v.as_str()).unwrap_or("");
        if text.trim().is_empty() {
            return Err("message is empty".into());
        }
        let peers = list_peers();
        let target = resolve(&peers, to)?;
        if self.me.as_deref() == Some(target.name.as_str()) {
            return Err("that is this session".into());
        }
        let me = self
            .me
            .as_deref()
            .and_then(|n| peers.iter().find(|p| p.name == n));
        let message = json!({
            "from": self.me,
            "from_agent": me.map(|p| p.agent.name()),
            "from_folder": me.and_then(|p| p.folder()),
            "text": text,
            "sent_at": layout::now_secs(),
        });
        post(&self.home, &target.name, &message).map_err(|e| e.to_string())?;
        let status = status_label(status_of(target));
        let when = match status {
            "idle" | "waiting_on_subagents" => "it will be typed into its prompt in a few seconds",
            _ => "it will be typed into its prompt once it is idle",
        };
        Ok(format!(
            "Queued for {} ({}, currently {}): {}.",
            target.name,
            target.agent.name(),
            status,
            when
        ))
    }

    fn call(&self, name: &str, args: &Value) -> Value {
        let result = match name {
            "list_sessions" => self.list_sessions(),
            "send_message" => self.send_message(args),
            other => Err(format!("unknown tool {}", other)),
        };
        match result {
            Ok(text) => json!({ "content": [{ "type": "text", "text": text }] }),
            Err(text) => json!({ "content": [{ "type": "text", "text": text }], "isError": true }),
        }
    }

    fn handle(&self, msg: &Value) -> Option<Value> {
        let id = msg.get("id")?.clone(); // notifications get no reply
        let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        let result = match method {
            "initialize" => {
                let version = params
                    .get("protocolVersion")
                    .and_then(|v| v.as_str())
                    .unwrap_or(PROTOCOL_VERSION);
                json!({
                    "protocolVersion": version,
                    "capabilities": { "tools": {} },
                    "serverInfo": { "name": SERVER_NAME, "version": env!("CARGO_PKG_VERSION") },
                    "instructions": "Talk to the other coding-agent sessions running in clawtree: list_sessions shows them, send_message delivers a message to one. Messages from other sessions arrive as prompts starting with [clawtree message from …].",
                })
            }
            "ping" => json!({}),
            "tools/list" => json!({ "tools": tool_specs() }),
            "tools/call" => {
                let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
                let args = params.get("arguments").cloned().unwrap_or(json!({}));
                self.call(name, &args)
            }
            _ => {
                return Some(json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": { "code": -32601, "message": format!("method not found: {}", method) },
                }))
            }
        };
        Some(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
    }
}

/// Mailboxes with waiting messages whose session has no server of its own.
fn orphaned_mailboxes(home: &Path, me: Option<&str>) -> Vec<String> {
    std::fs::read_dir(mailbox_root(home))
        .map(|rd| {
            rd.flatten()
                .filter_map(|e| e.file_name().into_string().ok())
                .filter(|n| Some(n.as_str()) != me && valid_name(n))
                .filter(|n| !pending(home, n).is_empty() && !has_receiver(home, n))
                .collect()
        })
        .unwrap_or_default()
}

/// Background delivery: this session's own mailbox, plus mail for sessions
/// that have no server (started before messaging; the deliver lock keeps
/// servers from doubling up).
fn spawn_delivery(home: PathBuf, me: Option<String>) {
    std::thread::Builder::new()
        .name("mcp-delivery".into())
        .spawn(move || {
            // Held for the life of the process: tells senders we deliver our own mail.
            let _receiver = me
                .as_deref()
                .and_then(|n| mailbox_dir(&home, n))
                .and_then(|d| layout::try_lock(&d.join("receiver")).ok().flatten());
            loop {
                if let Some(n) = &me {
                    deliver_one(&home, n);
                }
                for t in orphaned_mailboxes(&home, me.as_deref()) {
                    deliver_one(&home, &t);
                }
                std::thread::sleep(DELIVERY_POLL);
            }
        })
        .ok();
}

/// A message waiting this long is delivered by the clawtree window even if
/// its session has a server (one from an older build may be stuck).
const STALE_MAIL_SECS: u64 = 60;

/// Mailboxes whose oldest message has waited more than `STALE_MAIL_SECS`.
fn stale_mailboxes(home: &Path) -> Vec<String> {
    let now = layout::now_secs();
    std::fs::read_dir(mailbox_root(home))
        .map(|rd| {
            rd.flatten()
                .filter_map(|e| e.file_name().into_string().ok())
                .filter(|n| valid_name(n))
                .filter(|n| {
                    pending(home, n).first().is_some_and(|f| {
                        std::fs::metadata(f)
                            .and_then(|m| m.modified())
                            .ok()
                            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                            .is_some_and(|t| now.saturating_sub(t.as_secs()) > STALE_MAIL_SECS)
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// For the clawtree window: deliver mail for sessions that have no MCP
/// server of their own (started before messaging, or with it turned off),
/// and any mail that has waited over a minute. Any number of windows can run
/// this; the deliver lock keeps them apart.
pub fn spawn_window_courier() {
    if !tmux::mcp_enabled() {
        return;
    }
    let home = home();
    std::thread::Builder::new()
        .name("mailbox-courier".into())
        .spawn(move || loop {
            std::thread::sleep(DELIVERY_POLL);
            let mut targets = orphaned_mailboxes(&home, None);
            for t in stale_mailboxes(&home) {
                if !targets.contains(&t) {
                    targets.push(t);
                }
            }
            for t in targets {
                deliver_one(&home, &t);
            }
        })
        .ok();
}

/// `clawtree send <to> <message…>`: what `send_message` does, for agents
/// without the MCP tool (sessions started before messaging) or scripts.
/// Delivery is left to the target's server, or any other running one.
pub fn send_cli(args: &[String]) -> i32 {
    let (Some(to), rest) = (args.first(), args.get(1..).unwrap_or_default()) else {
        eprintln!("usage: clawtree send <session> <message>");
        return 2;
    };
    let message = rest.join(" ");
    let server = Server {
        home: home(),
        me: own_session_name(),
    };
    match server.send_message(&json!({ "to": to, "message": message })) {
        Ok(text) => {
            println!("{}", text);
            0
        }
        Err(e) => {
            eprintln!("clawtree send: {}", e);
            1
        }
    }
}

/// Run the server on stdin/stdout until stdin closes.
pub fn run() -> anyhow::Result<()> {
    let home = home();
    let me = own_session_name();
    spawn_delivery(home.clone(), me.clone());
    let server = Server { home, me };

    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            Ok(msg) => server.handle(&msg),
            Err(e) => Some(json!({
                "jsonrpc": "2.0",
                "id": Value::Null,
                "error": { "code": -32700, "message": format!("parse error: {}", e) },
            })),
        };
        if let Some(r) = reply {
            writeln!(stdout, "{}", r)?;
            stdout.flush()?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peer(name: &str, nick: Option<&str>, title: &str) -> Peer {
        Peer {
            name: name.into(),
            pane: "%1".into(),
            agent: tmux::Agent::Claude,
            title: title.into(),
            path: Some(PathBuf::from("/p/main")),
            nickname: nick.map(|s| s.to_string()),
            project: None,
        }
    }

    #[test]
    fn resolves_by_name_then_nickname_then_title() {
        let peers = vec![
            peer("clawtree-main-1", Some("Reviewer"), "\u{2733} Fix login"),
            peer("clawtree-main-2", None, "\u{25D0} Write docs"),
        ];
        assert_eq!(resolve(&peers, "clawtree-main-2").unwrap().name, "clawtree-main-2");
        assert_eq!(resolve(&peers, "reviewer").unwrap().name, "clawtree-main-1");
        assert_eq!(resolve(&peers, "write docs").unwrap().name, "clawtree-main-2");
        assert!(resolve(&peers, "nobody").is_err());
        let twins = vec![peer("a", Some("x"), ""), peer("b", Some("X"), "")];
        assert!(resolve(&twins, "x").unwrap_err().contains("several"));
    }

    #[test]
    fn mailbox_names_cannot_escape() {
        let home = Path::new("/h");
        assert!(mailbox_dir(home, "clawtree-main-1").is_some());
        for bad in ["", "..", "../x", "a/b", ".hidden", "a b"] {
            assert!(mailbox_dir(home, bad).is_none(), "{:?}", bad);
        }
    }

    #[test]
    fn messages_queue_in_order_and_render_with_sender() {
        let home = tempfile::tempdir().unwrap();
        for i in 0..3 {
            post(home.path(), "clawtree-main-1", &json!({ "text": format!("m{}", i) })).unwrap();
        }
        let files = pending(home.path(), "clawtree-main-1");
        assert_eq!(files.len(), 3);
        let first: Value = serde_json::from_slice(&std::fs::read(&files[0]).unwrap()).unwrap();
        assert_eq!(first["text"], "m0");
        assert_eq!(
            render(&json!({ "from": "clawtree-main-2", "from_agent": "agy", "from_folder": "main", "text": " hi " })),
            "[clawtree message from clawtree-main-2 (agy · main) — reply with the clawtree send_message tool (to: \"clawtree-main-2\"), or run: clawtree send clawtree-main-2 \"<reply>\"] hi"
        );
        assert_eq!(
            render(&json!({ "text": "hi" })),
            "[clawtree message from outside clawtree] hi"
        );
    }

    #[test]
    fn receiver_lock_marks_a_live_server() {
        let home = tempfile::tempdir().unwrap();
        assert!(!has_receiver(home.path(), "s"));
        let dir = mailbox_dir(home.path(), "s").unwrap();
        let _held = layout::try_lock(&dir.join("receiver")).unwrap().unwrap();
        assert!(has_receiver(home.path(), "s"));
    }

    #[test]
    fn json_rpc_handshake_and_tools() {
        let s = Server {
            home: PathBuf::from("/nonexistent"),
            me: None,
        };
        let init = s
            .handle(&json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2026-07-28"}}))
            .unwrap();
        assert_eq!(init["result"]["protocolVersion"], "2026-07-28");
        assert_eq!(init["result"]["serverInfo"]["name"], "clawtree");
        assert!(s.handle(&json!({"jsonrpc":"2.0","method":"notifications/initialized"})).is_none());
        let tools = s.handle(&json!({"jsonrpc":"2.0","id":2,"method":"tools/list"})).unwrap();
        let names: Vec<&str> = tools["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, tmux::MCP_TOOLS);
        let unknown = s.handle(&json!({"jsonrpc":"2.0","id":3,"method":"server/discover"})).unwrap();
        assert_eq!(unknown["error"]["code"], -32601);
        let empty = s
            .handle(&json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"send_message","arguments":{"to":"x","message":" "}}}))
            .unwrap();
        assert_eq!(empty["result"]["isError"], true);
    }

    #[test]
    fn ticking_agent_clocks_dont_block_delivery() {
        let a = "  ◯ general-purpose  Polling build.log   7h 25m 54s · ↓ 329.9k tokens";
        let b = "  ◯ general-purpose  Polling build.log   7h 25m 56s · ↓ 329.9k tokens";
        assert_eq!(settle_key(a), settle_key(b));
        assert_ne!(settle_key("❯ "), settle_key("❯ hello"));
    }

    #[test]
    fn drafts_are_told_apart_from_suggestions() {
        // Captured with `tmux capture-pane -e` from Claude Code 2.1.332 and agy 1.5.17.
        let claude_suggestion = "\x1b[39m\u{276f}\u{a0}\x1b[2madd the missing tests\x1b[0m\x1b[39m\x1b[49m";
        let claude_placeholder = "\x1b[39m\u{276f}\u{a0}\x1b[2mTry \"how do I log an error?\"\x1b[0m\x1b[39m\x1b[49m";
        let claude_draft = "\x1b[39m\u{276f}\u{a0}hello my draft";
        let claude_empty = "\x1b[39m\u{276f}\u{a0}";
        assert!(!has_draft(tmux::Agent::Claude, claude_suggestion));
        assert!(!has_draft(tmux::Agent::Claude, claude_placeholder));
        assert!(has_draft(tmux::Agent::Claude, claude_draft));
        assert!(!has_draft(tmux::Agent::Claude, claude_empty));
        let agy_placeholder = "\x1b[94m>\x1b[39m \x1b[90mAuto mode: edits & safe commands auto-approved (shift+tab to cycle)\x1b[39m";
        let agy_draft = "\x1b[94m>\x1b[39m hello my draft";
        assert!(!has_draft(tmux::Agent::Antigravity, agy_placeholder));
        assert!(has_draft(tmux::Agent::Antigravity, agy_draft));
        // Only the prompt line counts, not transcript lines below it.
        let screen = format!("{}\n  ⏵⏵ bypass permissions on · ← 2 agents", claude_suggestion);
        assert!(!has_draft(tmux::Agent::Claude, &screen));
    }

    #[test]
    fn mail_for_sessions_without_a_server_is_picked_up() {
        let home = tempfile::tempdir().unwrap();
        post(home.path(), "old-session", &json!({ "text": "hi" })).unwrap();
        post(home.path(), "live-session", &json!({ "text": "hi" })).unwrap();
        let dir = mailbox_dir(home.path(), "live-session").unwrap();
        let _held = layout::try_lock(&dir.join("receiver")).unwrap().unwrap();
        assert_eq!(orphaned_mailboxes(home.path(), None), vec!["old-session".to_string()]);
        assert!(orphaned_mailboxes(home.path(), Some("old-session")).is_empty());
    }

    #[test]
    fn old_mail_counts_as_stale() {
        let home = tempfile::tempdir().unwrap();
        let fresh = post(home.path(), "fresh", &json!({ "text": "hi" })).unwrap();
        let old = post(home.path(), "old", &json!({ "text": "hi" })).unwrap();
        let past = std::time::SystemTime::now() - Duration::from_secs(STALE_MAIL_SECS + 30);
        std::fs::File::options()
            .write(true)
            .open(&old)
            .unwrap()
            .set_modified(past)
            .unwrap();
        assert!(fresh.exists());
        assert_eq!(stale_mailboxes(home.path()), vec!["old".to_string()]);
    }
}

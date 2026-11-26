// Session layout manifest: clawtree's on-disk record of which sessions a
// project had, so they can be rebuilt after the tmux server dies (WSL VM
// crash, reboot, `tmux kill-server`).
//
// One file per project at `~/.clawtree/state/<key>.json`. This module is pure
// data + file I/O (no `App`), so `lib.rs` can include it for integration
// tests; the App-facing glue lives in `session/recovery.rs`.

use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use super::agy;
use super::tmux::{self, Agent, AgentLaunch};

/// Current manifest schema version. Files with a newer version are left alone.
pub const MANIFEST_VERSION: u32 = 1;

/// Process env var that turns layout persistence off entirely (test hook).
pub const SKIP_LAYOUT_ENV: &str = "CLAWTREE_SKIP_LAYOUT";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EntryKind {
    Claude,
    Terminal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EntryScope {
    Worktree,
    Root,
    Location,
}

/// One session in the manifest. `tmux_name` is the primary key: in-memory
/// session ids are reassigned on every launch, tmux names are not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LayoutEntry {
    pub tmux_name: String,
    pub kind: EntryKind,
    pub scope: EntryScope,
    /// The worktree, project root or location the session belongs to.
    pub path: PathBuf,
    /// Last known pane cwd (terminals may `cd` away from `path`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
    /// Informational; the branch of the worktree at save time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nickname: Option<String>,
    /// Which agent an agent (`kind: claude`) entry runs. Absent = Claude
    /// Code, so manifests written before other agents still read the same.
    #[serde(default, skip_serializing_if = "Agent::is_claude")]
    pub agent: Agent,
    /// The agent's conversation id (agent sessions only; the name predates
    /// other agents).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claude_session_id: Option<String>,
    /// agy only: its agent mode at the last save (`auto`, `accept-edits`,
    /// `plan`, `default`). Claude's mode is read from its transcript instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_mode: Option<String>,
    #[serde(default)]
    pub skip_permissions: bool,
    /// Position within its parent in the sidebar.
    #[serde(default)]
    pub order: usize,
    /// Unix seconds.
    #[serde(default)]
    pub created_at: u64,
    /// The process exited; never offered for restore.
    #[serde(default)]
    pub exited: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LayoutManifest {
    pub version: u32,
    pub bare_repo_path: PathBuf,
    /// Unix seconds.
    pub updated_at: u64,
    pub clawtree_version: String,
    /// The session focused at the last save.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_tmux_name: Option<String>,
    #[serde(default)]
    pub entries: Vec<LayoutEntry>,
}

impl LayoutManifest {
    pub fn new(bare_repo_path: &Path) -> Self {
        Self {
            version: MANIFEST_VERSION,
            bare_repo_path: bare_repo_path.to_path_buf(),
            updated_at: now_secs(),
            clawtree_version: env!("CARGO_PKG_VERSION").to_string(),
            active_tmux_name: None,
            entries: Vec::new(),
        }
    }
}

pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Whether layout persistence is disabled for this process.
pub fn layout_disabled() -> bool {
    std::env::var(SKIP_LAYOUT_ENV)
        .map(|v| !v.is_empty() && v != "0")
        .unwrap_or(false)
}

// ── Paths ────────────────────────────────────────────────────────────────

/// 32-bit FNV-1a. Written out by hand because `DefaultHasher` is not stable
/// across Rust versions and the key must never change.
pub fn fnv1a32(bytes: &[u8]) -> u32 {
    let mut hash: u32 = 0x811c_9dc5;
    for b in bytes {
        hash ^= *b as u32;
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash
}

/// File-name key for a project: the path with non-alphanumerics replaced by
/// `-` (readable), plus the FNV-1a hash of the full path (collision-proof).
pub fn manifest_key(bare_repo_path: &Path) -> String {
    let canonical =
        std::fs::canonicalize(bare_repo_path).unwrap_or_else(|_| bare_repo_path.to_path_buf());
    let s = canonical.to_string_lossy();
    let readable: String = s
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    // Keep the file name well under NAME_MAX for deep paths.
    let readable: String = if readable.len() > 120 {
        readable[readable.len() - 120..].to_string()
    } else {
        readable
    };
    format!("{}-{:08x}", readable, fnv1a32(s.as_bytes()))
}

fn home_dir() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
}

/// `~/.clawtree/state`.
pub fn state_dir() -> PathBuf {
    home_dir().join(".clawtree").join("state")
}

pub fn manifest_path_in(state_dir: &Path, bare_repo_path: &Path) -> PathBuf {
    state_dir.join(format!("{}.json", manifest_key(bare_repo_path)))
}

/// `~/.clawtree/state/<key>.json` for this project.
pub fn manifest_path(bare_repo_path: &Path) -> PathBuf {
    manifest_path_in(&state_dir(), bare_repo_path)
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut s = path.as_os_str().to_os_string();
    s.push(suffix);
    PathBuf::from(s)
}

// ── Load / save ──────────────────────────────────────────────────────────

#[derive(Debug)]
pub enum LoadOutcome {
    Missing,
    Loaded(LayoutManifest),
    /// The file didn't parse; it was moved to this path.
    Corrupt(PathBuf),
    /// Written by a newer clawtree; left untouched.
    UnsupportedVersion(u32),
}

pub fn load_from(path: &Path) -> LoadOutcome {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(_) => return LoadOutcome::Missing,
    };
    // Check the version before the full schema, so a future format that
    // changed shape is refused rather than treated as corrupt.
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
        if let Some(ver) = v.get("version").and_then(|x| x.as_u64()) {
            if ver > MANIFEST_VERSION as u64 {
                tracing::warn!(
                    "Layout manifest {} has version {}; this clawtree understands {}. Leaving it alone.",
                    path.display(),
                    ver,
                    MANIFEST_VERSION
                );
                return LoadOutcome::UnsupportedVersion(ver as u32);
            }
        }
    }
    match serde_json::from_str::<LayoutManifest>(&text) {
        Ok(m) => LoadOutcome::Loaded(m),
        Err(e) => {
            let dest = with_suffix(path, &format!(".corrupt-{}", now_secs()));
            tracing::warn!(
                "Layout manifest {} failed to parse ({}); moving it to {}",
                path.display(),
                e,
                dest.display()
            );
            let _ = std::fs::rename(path, &dest);
            LoadOutcome::Corrupt(dest)
        }
    }
}

/// Load this project's manifest, if there is a readable one.
pub fn load(bare_repo_path: &Path) -> Option<LayoutManifest> {
    match load_from(&manifest_path(bare_repo_path)) {
        LoadOutcome::Loaded(m) => Some(m),
        _ => None,
    }
}

/// Write `m` to `path` so a crash at any point leaves either the old or the
/// new file: write a temp file, fsync it, rename it over, fsync the directory.
pub fn save_atomic_to(path: &Path, m: &LayoutManifest) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = with_suffix(path, ".tmp");
    let json = serde_json::to_vec_pretty(m).map_err(std::io::Error::other)?;
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(&json)?;
        f.write_all(b"\n")?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    if let Some(dir) = path.parent() {
        if let Ok(d) = std::fs::File::open(dir) {
            let _ = d.sync_all();
        }
    }
    Ok(())
}

/// An exclusive advisory lock on `<manifest>.lock`, released on drop.
pub struct ManifestLock {
    _file: std::fs::File,
}

/// Take the manifest lock without blocking. `Ok(None)` means another clawtree
/// holds it right now.
pub fn try_lock(manifest: &Path) -> std::io::Result<Option<ManifestLock>> {
    use std::os::unix::io::AsRawFd;
    if let Some(dir) = manifest.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(with_suffix(manifest, ".lock"))?;
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc == 0 {
        Ok(Some(ManifestLock { _file: file }))
    } else {
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::EWOULDBLOCK) {
            Ok(None)
        } else {
            Err(err)
        }
    }
}

// ── Snapshot merge ───────────────────────────────────────────────────────

/// Combine what this instance knows (`live`) with what the file already
/// holds (`prev`):
///
/// > entries = live ∪ (prev entries not in `closed`, not superseded by a live
/// > entry — same tmux name or same conversation — and not exited)
///
/// so a second instance, or a launch where reconnect skipped some sessions,
/// can never erase another session's record. Only an explicit close does.
pub fn merge(
    prev: Option<&LayoutManifest>,
    bare_repo_path: &Path,
    mut live: Vec<LayoutEntry>,
    closed: &HashSet<String>,
    active_tmux_name: Option<String>,
) -> LayoutManifest {
    let live_names: HashSet<String> = live.iter().map(|e| e.tmux_name.clone()).collect();
    if let Some(prev) = prev {
        // Keep facts the live side hasn't learned (yet) for the same session.
        for e in live.iter_mut() {
            if let Some(old) = prev.entries.iter().find(|o| o.tmux_name == e.tmux_name) {
                if e.claude_session_id.is_none() {
                    e.claude_session_id = old.claude_session_id.clone();
                }
                if e.created_at == 0 {
                    e.created_at = old.created_at;
                }
            }
        }
        // A conversation restored under a new tmux name leaves its old
        // entry behind; keep one entry per conversation so the old one
        // isn't offered (and started a second time) after the next crash.
        let mut ids: HashSet<String> = live
            .iter()
            .filter_map(|e| e.claude_session_id.clone())
            .collect();
        for old in &prev.entries {
            if live_names.contains(&old.tmux_name) || closed.contains(&old.tmux_name) || old.exited
            {
                continue;
            }
            if let Some(id) = &old.claude_session_id {
                if !ids.insert(id.clone()) {
                    continue;
                }
            }
            live.push(old.clone());
        }
    }
    let mut m = LayoutManifest::new(bare_repo_path);
    m.active_tmux_name = active_tmux_name
        .or_else(|| prev.and_then(|p| p.active_tmux_name.clone()))
        .filter(|n| live.iter().any(|e| &e.tmux_name == n));
    m.entries = live;
    m
}

#[derive(Debug)]
pub enum SaveOutcome {
    Saved(LayoutManifest),
    /// Another instance holds the lock; try again later.
    Busy,
    /// The file is from a newer clawtree, or writing failed.
    Refused,
}

/// Read-modify-write this project's manifest under the lock.
pub fn write_merged_to(
    path: &Path,
    bare_repo_path: &Path,
    live: Vec<LayoutEntry>,
    closed: &HashSet<String>,
    active_tmux_name: Option<String>,
) -> SaveOutcome {
    let _lock = match try_lock(path) {
        Ok(Some(l)) => l,
        Ok(None) => return SaveOutcome::Busy,
        Err(e) => {
            tracing::warn!("Layout manifest lock failed: {}", e);
            return SaveOutcome::Refused;
        }
    };
    let prev = match load_from(path) {
        LoadOutcome::Loaded(m) => Some(m),
        LoadOutcome::UnsupportedVersion(_) => return SaveOutcome::Refused,
        LoadOutcome::Missing | LoadOutcome::Corrupt(_) => None,
    };
    let merged = merge(
        prev.as_ref(),
        bare_repo_path,
        live,
        closed,
        active_tmux_name,
    );
    // A project that has never had a session needs no file.
    if prev.is_none() && merged.entries.is_empty() {
        return SaveOutcome::Saved(merged);
    }
    // Nothing changed (e.g. a worktree was only expanded): skip the fsync'd write.
    if let Some(p) = &prev {
        if p.entries == merged.entries && p.active_tmux_name == merged.active_tmux_name {
            return SaveOutcome::Saved(merged);
        }
    }
    match save_atomic_to(path, &merged) {
        Ok(()) => SaveOutcome::Saved(merged),
        Err(e) => {
            tracing::warn!("Failed to write layout manifest {}: {}", path.display(), e);
            SaveOutcome::Refused
        }
    }
}

pub fn write_merged(
    bare_repo_path: &Path,
    live: Vec<LayoutEntry>,
    closed: &HashSet<String>,
    active_tmux_name: Option<String>,
) -> SaveOutcome {
    write_merged_to(
        &manifest_path(bare_repo_path),
        bare_repo_path,
        live,
        closed,
        active_tmux_name,
    )
}

/// Drop entries by tmux name (e.g. after `clawtree --cleanup` killed them).
pub fn forget_in(path: &Path, names: &HashSet<String>) {
    let Ok(Some(_lock)) = try_lock(path) else {
        return;
    };
    if let LoadOutcome::Loaded(mut m) = load_from(path) {
        let before = m.entries.len();
        m.entries.retain(|e| !names.contains(&e.tmux_name));
        if m.entries.len() != before {
            m.updated_at = now_secs();
            let _ = save_atomic_to(path, &m);
        }
    }
}

/// Every manifest file in the state directory.
pub fn all_manifest_paths() -> Vec<PathBuf> {
    std::fs::read_dir(state_dir())
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|x| x == "json"))
                .collect()
        })
        .unwrap_or_default()
}

// ── Last-chance flush (panic hook) ───────────────────────────────────────

struct PendingSave {
    bare_repo_path: PathBuf,
    live: Vec<LayoutEntry>,
    closed: HashSet<String>,
    active: Option<String>,
}

static PENDING: Mutex<Option<PendingSave>> = Mutex::new(None);

/// Remember a snapshot that hasn't been written yet (cheap, in memory), so a
/// panic before the debounce fires can still save it.
pub fn stash_pending(
    bare_repo_path: &Path,
    live: Vec<LayoutEntry>,
    closed: &HashSet<String>,
    active: Option<String>,
) {
    if let Ok(mut p) = PENDING.try_lock() {
        *p = Some(PendingSave {
            bare_repo_path: bare_repo_path.to_path_buf(),
            live,
            closed: closed.clone(),
            active,
        });
    }
}

pub fn clear_pending() {
    if let Ok(mut p) = PENDING.try_lock() {
        *p = None;
    }
}

/// Best effort, from the panic hook: write the stashed snapshot if any.
pub fn flush_pending() {
    let pending = match PENDING.try_lock() {
        Ok(mut p) => p.take(),
        Err(_) => None,
    };
    if let Some(p) = pending {
        let _ = write_merged(&p.bare_repo_path, p.live, &p.closed, p.active);
    }
}

// ── Diff ─────────────────────────────────────────────────────────────────

#[derive(Debug, Default)]
pub struct LayoutDiff {
    /// Missing from tmux and their folder/parent still exists.
    pub restorable: Vec<LayoutEntry>,
    /// Missing from tmux and their worktree/location is gone.
    pub stale: Vec<LayoutEntry>,
    /// A tmux session with this name exists (attached here or elsewhere).
    pub live: Vec<LayoutEntry>,
    /// Their conversation is already open (under another tmux name, or in
    /// a Claude started outside clawtree), or an earlier entry for the same
    /// conversation was already counted. Restoring one would run a second
    /// claude on the same transcript.
    pub duplicates: Vec<LayoutEntry>,
}

/// Sort a manifest's entries against the tmux sessions that exist now.
///
/// `root_available` is false when the project folder itself is missing (an
/// unmounted drive): then nothing is restorable *or* stale, it's "not now".
/// `can_restore` decides whether an entry's parent still exists.
/// `open_conversations` are conversation ids running right now (see
/// [`open_conversations`]); entries for those are duplicates, not restorable.
pub fn diff(
    m: &LayoutManifest,
    live_tmux_names: &HashSet<String>,
    open_conversations: &HashSet<String>,
    root_available: bool,
    can_restore: impl Fn(&LayoutEntry) -> bool,
) -> LayoutDiff {
    let mut d = LayoutDiff::default();
    let mut taken: HashSet<&str> = open_conversations.iter().map(|s| s.as_str()).collect();
    for e in &m.entries {
        if !e.exited && live_tmux_names.contains(&e.tmux_name) {
            if let Some(id) = &e.claude_session_id {
                taken.insert(id);
            }
        }
    }
    for e in &m.entries {
        if e.exited {
            continue;
        }
        if live_tmux_names.contains(&e.tmux_name) {
            d.live.push(e.clone());
        } else if e
            .claude_session_id
            .as_deref()
            .is_some_and(|id| !taken.insert(id))
        {
            d.duplicates.push(e.clone());
        } else if !root_available {
            continue;
        } else if can_restore(e) {
            d.restorable.push(e.clone());
        } else {
            d.stale.push(e.clone());
        }
    }
    d
}

// ── Claude conversations ─────────────────────────────────────────────────

/// Claude Code's project-folder name for a cwd: every non-alphanumeric
/// character becomes `-`. Lossy, but we only ever mangle a known path.
pub fn mangle_project_dir(path: &Path) -> String {
    path.to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

pub fn claude_projects_dir(home: &Path) -> PathBuf {
    home.join(".claude").join("projects")
}

/// The transcript `<id>.jsonl` for this conversation: first in the cwd's
/// project folder, then in any project folder (Claude shortens very long
/// folder names, so the mangled guess can miss).
pub fn transcript_path(home: &Path, cwd: &Path, id: &str) -> Option<PathBuf> {
    let projects = claude_projects_dir(home);
    let file = format!("{}.jsonl", id);
    let direct = projects.join(mangle_project_dir(cwd)).join(&file);
    if direct.is_file() {
        return Some(direct);
    }
    std::fs::read_dir(&projects).ok()?.flatten().find_map(|d| {
        let p = d.path().join(&file);
        p.is_file().then_some(p)
    })
}

pub fn transcript_exists(home: &Path, cwd: &Path, id: &str) -> bool {
    transcript_path(home, cwd, id).is_some()
}

/// Permission modes `claude --permission-mode` accepts.
pub const PERMISSION_MODES: &[&str] = &[
    "acceptEdits",
    "auto",
    "bypassPermissions",
    "manual",
    "dontAsk",
    "plan",
];

/// The permission mode a conversation was last in: Claude Code stamps
/// `"permissionMode"` on each message it writes to the transcript (Shift+Tab
/// changes it mid-session). Reads the tail first; transcripts can be large.
pub fn last_permission_mode(transcript: &Path) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};
    const KEY: &str = "\"permissionMode\":\"";
    const TAIL: u64 = 512 * 1024;
    const MAX_FULL: u64 = 64 * 1024 * 1024;
    let find_last = |text: &str| -> Option<String> {
        let mut rest = text;
        let mut last = None;
        while let Some(i) = rest.find(KEY) {
            rest = &rest[i + KEY.len()..];
            if let Some(end) = rest.find('"') {
                last = Some(rest[..end].to_string());
            }
        }
        last.filter(|m| PERMISSION_MODES.contains(&m.as_str()))
    };
    let mut f = std::fs::File::open(transcript).ok()?;
    let len = f.metadata().ok()?.len();
    if len > TAIL {
        f.seek(SeekFrom::Start(len - TAIL)).ok()?;
        let mut buf = Vec::new();
        f.read_to_end(&mut buf).ok()?;
        if let Some(m) = find_last(&String::from_utf8_lossy(&buf)) {
            return Some(m);
        }
        if len > MAX_FULL {
            return None;
        }
        f.seek(SeekFrom::Start(0)).ok()?;
    }
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).ok()?;
    find_last(&String::from_utf8_lossy(&buf))
}

/// Conversation ids from top-level `*.jsonl` transcripts in `cwd`'s project
/// folder modified after `newer_than` (unix seconds), newest first, skipping
/// `claimed` ids. Nested folders (`subagents/` etc.) are ignored.
pub fn transcript_candidates(
    home: &Path,
    cwd: &Path,
    newer_than: u64,
    claimed: &HashSet<String>,
) -> Vec<String> {
    let dir = claude_projects_dir(home).join(mangle_project_dir(cwd));
    let mut found: Vec<(u64, String)> = std::fs::read_dir(&dir)
        .map(|rd| {
            rd.flatten()
                .filter_map(|e| {
                    let meta = e.metadata().ok()?;
                    if !meta.is_file() {
                        return None;
                    }
                    let path = e.path();
                    if path.extension()? != "jsonl" {
                        return None;
                    }
                    let id = path.file_stem()?.to_string_lossy().to_string();
                    if claimed.contains(&id) {
                        return None;
                    }
                    let mtime = meta
                        .modified()
                        .ok()?
                        .duration_since(std::time::UNIX_EPOCH)
                        .ok()?
                        .as_secs();
                    (mtime > newer_than).then_some((mtime, id))
                })
                .collect()
        })
        .unwrap_or_default();
    found.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    found.into_iter().map(|(_, id)| id).collect()
}

fn any_transcript(home: &Path, cwd: &Path) -> bool {
    std::fs::read_dir(claude_projects_dir(home).join(mangle_project_dir(cwd)))
        .map(|rd| {
            rd.flatten()
                .any(|e| e.path().extension().is_some_and(|x| x == "jsonl"))
        })
        .unwrap_or(false)
}

/// Where a restored session's conversation comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResumeSource {
    /// A plain terminal: nothing to resume.
    Terminal,
    /// The recorded conversation id, with a transcript on disk.
    SessionId(String),
    /// No id was recorded; guessed from transcripts newer than the session.
    /// `pick` indexes `candidates` (the user can cycle through them).
    Guessed {
        candidates: Vec<String>,
        pick: usize,
    },
    /// `claude --continue`: the most recent conversation in the folder.
    Continue,
    /// Start a new conversation (the session never got a transcript).
    Fresh,
}

impl ResumeSource {
    pub fn describe(&self) -> String {
        match self {
            ResumeSource::Terminal => "terminal".into(),
            ResumeSource::SessionId(id) => format!("session id {}", short_id(id)),
            ResumeSource::Guessed { candidates, pick } => {
                let id = candidates.get(*pick).map(|s| short_id(s)).unwrap_or("?");
                if candidates.len() > 1 {
                    format!(
                        "guessed: {} ({}/{}, c: next)",
                        id,
                        pick + 1,
                        candidates.len()
                    )
                } else {
                    format!("guessed: {}", id)
                }
            }
            ResumeSource::Continue => "--continue (most recent conversation)".into(),
            ResumeSource::Fresh => "new conversation (no transcript)".into(),
        }
    }

    /// The conversation id this source resumes, if it names one.
    pub fn resumed_id(&self) -> Option<&str> {
        match self {
            ResumeSource::SessionId(id) => Some(id),
            ResumeSource::Guessed { candidates, pick } => candidates.get(*pick).map(|s| s.as_str()),
            _ => None,
        }
    }

    /// How to launch claude for this source. `new_id` is used for `Fresh`;
    /// `permission_mode` is passed along when resuming a known conversation.
    pub fn launch(&self, new_id: Option<String>, permission_mode: Option<String>) -> AgentLaunch {
        match self {
            ResumeSource::SessionId(_) | ResumeSource::Guessed { .. } => match self.resumed_id() {
                Some(id) => AgentLaunch::Resume {
                    id: id.to_string(),
                    permission_mode,
                },
                None => AgentLaunch::Continue,
            },
            ResumeSource::Continue => AgentLaunch::Continue,
            ResumeSource::Terminal | ResumeSource::Fresh => AgentLaunch::New {
                session_id: new_id,
                initial_prompt: None,
            },
        }
    }
}

/// One row of the restore dialog / `clawtree restore` plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreRow {
    pub entry: LayoutEntry,
    pub checked: bool,
    pub source: ResumeSource,
    /// The mode the resumed conversation was last in, from its transcript.
    pub permission_mode: Option<String>,
}

impl RestoreRow {
    pub fn new(home: &Path, entry: LayoutEntry, source: ResumeSource) -> Self {
        let mut row = Self {
            entry,
            checked: true,
            source,
            permission_mode: None,
        };
        row.refresh_permission_mode(home);
        row
    }

    /// Cycle a guessed conversation to the next candidate.
    pub fn next_candidate(&mut self, home: &Path) {
        if let ResumeSource::Guessed { candidates, pick } = &mut self.source {
            if !candidates.is_empty() {
                *pick = (*pick + 1) % candidates.len();
            }
        }
        self.refresh_permission_mode(home);
    }

    fn refresh_permission_mode(&mut self, home: &Path) {
        self.permission_mode = match self.entry.agent {
            Agent::Claude => self
                .source
                .resumed_id()
                .and_then(|id| transcript_path(home, &self.entry.path, id))
                .and_then(|p| last_permission_mode(&p)),
            Agent::Antigravity => self.source.resumed_id().and(self.entry.agent_mode.clone()),
        };
    }

    /// Where this row's conversation is already open, if it is: either the
    /// one it would resume or the one its entry recorded (a row planned as a
    /// fresh start still stands for that recorded session).
    pub fn open_elsewhere(&self, home: &Path) -> Option<String> {
        let mut open = open_conversations(home);
        [self.source.resumed_id(), self.entry.claude_session_id.as_deref()]
            .into_iter()
            .flatten()
            .find_map(|id| open.remove(id))
    }

    /// Bypass mode comes back as clawtree's YOLO flag: either the session was
    /// started that way, or its conversation was last in bypassPermissions
    /// (sessions started before clawtree recorded the flag).
    pub fn skip_permissions(&self) -> bool {
        self.entry.kind == EntryKind::Claude
            && (self.entry.skip_permissions
                || self.permission_mode.as_deref() == Some("bypassPermissions"))
    }

    /// How to start the agent for this row. Bypass is carried by
    /// `skip_permissions()`; any other mode is passed along (`--permission-mode`
    /// for Claude, `--mode` for agy). `new_id` is ignored for agents that pick
    /// their own conversation id.
    pub fn claude_launch(&self, new_id: Option<String>) -> AgentLaunch {
        let new_id = new_id.filter(|_| self.entry.agent.assigns_conversation_id());
        let mode = self
            .permission_mode
            .clone()
            .filter(|m| m != "bypassPermissions");
        self.source.launch(new_id, mode)
    }

    /// Resume source plus the mode, for lists.
    pub fn summary(&self) -> String {
        match self.mode_label() {
            Some(m) => format!("{} · {}", self.source.describe(), m),
            None => self.source.describe(),
        }
    }

    /// Short text for the mode the session comes back in.
    pub fn mode_label(&self) -> Option<String> {
        if self.entry.kind != EntryKind::Claude {
            return None;
        }
        match (self.skip_permissions(), self.permission_mode.as_deref()) {
            (_, Some(m)) if m != "bypassPermissions" => Some(m.to_string()),
            (true, _) => Some("bypassPermissions".to_string()),
            _ => None,
        }
    }
}

/// Build restore rows for `restorable`, resolving each conversation once.
pub fn plan_rows(home: &Path, restorable: &[LayoutEntry], all: &[LayoutEntry]) -> Vec<RestoreRow> {
    resolve_all(home, restorable, all)
        .into_iter()
        .zip(restorable.iter().cloned())
        .map(|(source, entry)| RestoreRow::new(home, entry, source))
        .collect()
}

pub fn short_id(id: &str) -> &str {
    id.get(..8).unwrap_or(id)
}

/// Decide how to resume an entry. `claimed` holds conversation ids that
/// belong to other entries and must not be guessed for this one.
pub fn resolve_resume(home: &Path, e: &LayoutEntry, claimed: &HashSet<String>) -> ResumeSource {
    if e.kind == EntryKind::Terminal {
        return ResumeSource::Terminal;
    }
    if e.agent == Agent::Antigravity {
        // agy conversations are found from the live process, so an entry
        // without an id never started one.
        return match &e.claude_session_id {
            Some(id) if agy::conversation_exists(home, id) => ResumeSource::SessionId(id.clone()),
            _ => ResumeSource::Fresh,
        };
    }
    if let Some(id) = &e.claude_session_id {
        return if transcript_exists(home, &e.path, id) {
            ResumeSource::SessionId(id.clone())
        } else {
            // Assigned at spawn but never used: there's nothing to resume.
            ResumeSource::Fresh
        };
    }
    let candidates = transcript_candidates(home, &e.path, e.created_at, claimed);
    if !candidates.is_empty() {
        ResumeSource::Guessed {
            candidates,
            pick: 0,
        }
    } else if any_transcript(home, &e.path) {
        ResumeSource::Continue
    } else {
        ResumeSource::Fresh
    }
}

/// Resolve every entry in order, so two entries never guess the same id.
pub fn resolve_all(home: &Path, entries: &[LayoutEntry], all: &[LayoutEntry]) -> Vec<ResumeSource> {
    let mut claimed: HashSet<String> = all
        .iter()
        .filter_map(|e| e.claude_session_id.clone())
        .collect();
    entries
        .iter()
        .map(|e| {
            let src = resolve_resume(home, e, &claimed);
            if let Some(id) = src.resumed_id() {
                claimed.insert(id.to_string());
            }
            src
        })
        .collect()
}

/// A random (v4) UUID from the kernel RNG, for `claude --session-id`.
pub fn new_uuid_v4() -> String {
    use std::io::Read;
    let mut b = [0u8; 16];
    let ok = std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut b))
        .is_ok();
    if !ok {
        // Fallback: mix time and pid. Uniqueness, not secrecy, is what matters.
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let seed = t ^ ((std::process::id() as u128) << 64);
        b.copy_from_slice(&seed.to_le_bytes());
    }
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h: String = b.iter().map(|x| format!("{:02x}", x)).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &h[0..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..32]
    )
}

/// Whether the installed `claude` accepts `--session-id`, once known.
/// `probe_claude_capabilities` fills it in from `claude --help`; until it has,
/// this returns false and the poller discovers ids instead.
static SUPPORTS_SESSION_ID: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

pub fn claude_supports_session_id() -> bool {
    SUPPORTS_SESSION_ID.get().copied().unwrap_or(false)
}

/// Run `claude --help` once and cache whether it lists `--session-id`.
pub fn probe_claude_capabilities() -> bool {
    *SUPPORTS_SESSION_ID.get_or_init(|| {
        std::process::Command::new("claude")
            .arg("--help")
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).contains("--session-id"))
            .unwrap_or(false)
    })
}

/// A new conversation id to launch with, if `claude` supports assigning one.
pub fn assign_session_id() -> Option<String> {
    claude_supports_session_id().then(new_uuid_v4)
}

/// Read the conversation id Claude Code records for a running process in
/// `~/.claude/sessions/<pid>.json`, checking `procStart` against
/// `/proc/<pid>/stat` so a recycled pid can't hand back a stale id.
pub fn claude_session_id_for_pid(home: &Path, pid: u32) -> Option<String> {
    let text = std::fs::read_to_string(
        home.join(".claude")
            .join("sessions")
            .join(format!("{}.json", pid)),
    )
    .ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    if let Some(recorded) = v.get("procStart").and_then(|x| x.as_str()) {
        let stat = std::fs::read_to_string(format!("/proc/{}/stat", pid)).ok()?;
        // Field 22 (starttime); fields after the parenthesised comm are space-separated.
        let after = stat.rsplit_once(')')?.1;
        let start = after.split_whitespace().nth(19)?;
        if start != recorded {
            return None;
        }
    }
    v.get("sessionId")
        .and_then(|x| x.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
}

/// Every conversation open right now, with where it's open: the id recorded
/// on each tmux session (set the moment clawtree creates one, so this sees a
/// restore by another instance that just happened), and each running Claude
/// process Claude Code knows about — including ones started outside clawtree.
pub fn open_conversations(home: &Path) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for name in tmux::list_session_names() {
        if let Some(id) = tmux::show_env(&name, tmux::CLAUDE_SESSION_ENV) {
            if !id.is_empty() {
                out.entry(id)
                    .or_insert_with(|| format!("tmux session {}", name));
            }
        }
    }
    let dir = home.join(".claude").join("sessions");
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return out;
    };
    for f in rd.flatten() {
        let name = f.file_name();
        let Some(pid) = name
            .to_str()
            .and_then(|n| n.strip_suffix(".json"))
            .and_then(|n| n.parse::<u32>().ok())
        else {
            continue;
        };
        // Most files belong to processes long gone; skip those cheaply.
        if !Path::new(&format!("/proc/{}", pid)).exists() {
            continue;
        }
        if let Some(id) = claude_session_id_for_pid(home, pid) {
            out.entry(id)
                .or_insert_with(|| format!("claude process {}", pid));
        }
    }
    out
}

/// [`open_conversations`] without the locations.
pub fn open_conversation_ids(home: &Path) -> HashSet<String> {
    open_conversations(home).into_keys().collect()
}

// ── Recreating sessions without the TUI ──────────────────────────────────

/// Create (but don't attach) the tmux session for a manifest entry.
pub fn create_tmux_for_entry(
    e: &LayoutEntry,
    tmux_name: &str,
    launch: &AgentLaunch,
    skip_permissions: bool,
    rows: u16,
    cols: u16,
    claude_env: Option<(&str, &str)>,
) -> anyhow::Result<()> {
    match e.kind {
        EntryKind::Claude => tmux::create_agent_session(
            e.agent,
            &e.path,
            rows,
            cols,
            skip_permissions,
            tmux_name,
            launch,
            claude_env,
        ),
        EntryKind::Terminal => {
            let start = e.cwd.as_deref().filter(|c| c.is_dir()).unwrap_or(&e.path);
            tmux::create_terminal_session(&e.path, start, rows, cols, tmux_name)
        }
    }
}

/// One-line description of an entry for lists.
pub fn entry_label(e: &LayoutEntry) -> String {
    let parent = match e.scope {
        EntryScope::Root => "project root".to_string(),
        _ => e
            .branch
            .clone()
            .or_else(|| e.path.file_name().map(|n| n.to_string_lossy().to_string()))
            .unwrap_or_else(|| e.path.display().to_string()),
    };
    match &e.nickname {
        Some(n) => format!("{} · {}", parent, n),
        None => format!("{} · {}", parent, e.tmux_name),
    }
}

/// What an entry runs, for lists: `claude`, `agy` or `terminal`.
pub fn kind_label(e: &LayoutEntry) -> &'static str {
    match e.kind {
        EntryKind::Claude => e.agent.name(),
        EntryKind::Terminal => "terminal",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, kind: EntryKind, path: &str) -> LayoutEntry {
        LayoutEntry {
            tmux_name: name.into(),
            kind,
            scope: EntryScope::Worktree,
            path: PathBuf::from(path),
            cwd: None,
            branch: Some("main".into()),
            nickname: None,
            agent: Agent::Claude,
            claude_session_id: None,
            agent_mode: None,
            skip_permissions: false,
            order: 0,
            created_at: 100,
            exited: false,
        }
    }

    fn manifest(entries: Vec<LayoutEntry>) -> LayoutManifest {
        let mut m = LayoutManifest::new(Path::new("/p"));
        m.entries = entries;
        m
    }

    #[test]
    fn round_trips_through_json() {
        let mut e = entry("clawtree-main-3", EntryKind::Claude, "/p/main");
        e.nickname = Some("Barrens revamp".into());
        e.claude_session_id = Some("a178e180-a9fc-4516-9879-f07a4b0b86bb".into());
        e.skip_permissions = true;
        let mut m = manifest(vec![
            e,
            entry("clawtree-term-main-4", EntryKind::Terminal, "/p/main"),
        ]);
        m.active_tmux_name = Some("clawtree-main-3".into());
        let json = serde_json::to_string(&m).unwrap();
        assert!(json.contains("\"kind\":\"claude\""));
        assert!(json.contains("\"scope\":\"worktree\""));
        let back: LayoutManifest = serde_json::from_str(&json).unwrap();
        assert_eq!(back, m);
    }

    #[test]
    fn refuses_future_version_and_leaves_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.json");
        let text = r#"{"version": 99, "whatever": true}"#;
        std::fs::write(&path, text).unwrap();
        assert!(matches!(
            load_from(&path),
            LoadOutcome::UnsupportedVersion(99)
        ));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
        // A save must not overwrite it either.
        let out = write_merged_to(&path, Path::new("/p"), vec![], &HashSet::new(), None);
        assert!(matches!(out, SaveOutcome::Refused));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
    }

    #[test]
    fn unchanged_layout_is_not_rewritten() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.json");
        let live = vec![entry("a", EntryKind::Claude, "/p/main")];
        let none = HashSet::new();
        write_merged_to(&path, Path::new("/p"), live.clone(), &none, None);
        // Make a rewrite detectable: a real write would drop this extra space.
        let marked = std::fs::read_to_string(&path)
            .unwrap()
            .replace("\"version\": 1", "\"version\":  1");
        std::fs::write(&path, &marked).unwrap();
        write_merged_to(&path, Path::new("/p"), live, &none, None);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), marked);
    }

    #[test]
    fn corrupt_file_is_moved_aside() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.json");
        std::fs::write(&path, "{ not json").unwrap();
        match load_from(&path) {
            LoadOutcome::Corrupt(dest) => {
                assert!(dest.to_string_lossy().contains(".json.corrupt-"));
                assert_eq!(std::fs::read_to_string(dest).unwrap(), "{ not json");
            }
            other => panic!("expected corrupt, got {:?}", other),
        }
        assert!(!path.exists());
    }

    #[test]
    fn atomic_save_leaves_no_tmp() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state").join("x.json");
        let m = manifest(vec![entry("a", EntryKind::Claude, "/p/main")]);
        save_atomic_to(&path, &m).unwrap();
        save_atomic_to(&path, &m).unwrap();
        assert!(!with_suffix(&path, ".tmp").exists());
        assert!(matches!(load_from(&path), LoadOutcome::Loaded(ref l) if *l == m));
    }

    #[test]
    fn key_is_readable_and_stable() {
        let k = manifest_key(Path::new("/nonexistent/roblox/boss.battles"));
        assert!(k.starts_with("-nonexistent-roblox-boss-battles-"), "{}", k);
        assert_eq!(k.len(), "-nonexistent-roblox-boss-battles-".len() + 8);
        assert_eq!(
            k,
            manifest_key(Path::new("/nonexistent/roblox/boss.battles"))
        );
        // Same readable form, different path → different key.
        assert_ne!(
            k,
            manifest_key(Path::new("/nonexistent/roblox/boss-battles"))
        );
        // FNV-1a reference value.
        assert_eq!(fnv1a32(b"a"), 0xe40c292c);
    }

    #[test]
    fn merge_keeps_unclosed_history_and_drops_closed() {
        let mut old_live = entry("live", EntryKind::Claude, "/p/main");
        old_live.claude_session_id = Some("old-id".into());
        old_live.nickname = Some("old nick".into());
        let prev = manifest(vec![
            old_live,
            entry("kept", EntryKind::Claude, "/p/main"),
            entry("closed", EntryKind::Claude, "/p/main"),
            LayoutEntry {
                exited: true,
                ..entry("exited", EntryKind::Claude, "/p/main")
            },
        ]);
        let mut now_live = entry("live", EntryKind::Claude, "/p/main");
        now_live.nickname = Some("new nick".into());
        let closed: HashSet<String> = ["closed".to_string()].into();
        let m = merge(Some(&prev), Path::new("/p"), vec![now_live], &closed, None);
        let names: Vec<&str> = m.entries.iter().map(|e| e.tmux_name.as_str()).collect();
        assert_eq!(names, vec!["live", "kept"]);
        // The live copy wins, but inherits an id it hadn't learned yet.
        assert_eq!(m.entries[0].nickname.as_deref(), Some("new nick"));
        assert_eq!(m.entries[0].claude_session_id.as_deref(), Some("old-id"));
    }

    fn convo(name: &str, id: &str) -> LayoutEntry {
        LayoutEntry {
            claude_session_id: Some(id.into()),
            ..entry(name, EntryKind::Claude, "/p/main")
        }
    }

    #[test]
    fn merge_drops_an_entry_restored_under_a_new_name() {
        // The crash-restore came back as "new-19" because "old-1" was taken;
        // the old entry must not survive to be offered again.
        let prev = manifest(vec![convo("old-1", "conv-a"), convo("other", "conv-b")]);
        let m = merge(
            Some(&prev),
            Path::new("/p"),
            vec![convo("new-19", "conv-a")],
            &HashSet::new(),
            None,
        );
        let names: Vec<&str> = m.entries.iter().map(|e| e.tmux_name.as_str()).collect();
        assert_eq!(names, vec!["new-19", "other"]);
    }

    #[test]
    fn merge_keeps_one_entry_per_dead_conversation() {
        let prev = manifest(vec![
            convo("a-1", "conv-a"),
            convo("a-2", "conv-a"),
            entry("term-1", EntryKind::Terminal, "/p/main"),
            entry("term-2", EntryKind::Terminal, "/p/main"),
        ]);
        let m = merge(Some(&prev), Path::new("/p"), vec![], &HashSet::new(), None);
        let names: Vec<&str> = m.entries.iter().map(|e| e.tmux_name.as_str()).collect();
        // Terminals have no conversation and are never collapsed.
        assert_eq!(names, vec!["a-1", "term-1", "term-2"]);
    }

    #[test]
    fn merge_keeps_two_live_sessions_on_one_conversation() {
        // Both are running; recording both is the truth, not a duplicate.
        let m = merge(
            None,
            Path::new("/p"),
            vec![convo("x", "conv-a"), convo("y", "conv-a")],
            &HashSet::new(),
            None,
        );
        assert_eq!(m.entries.len(), 2);
    }

    #[test]
    fn diff_never_offers_a_conversation_that_is_already_open() {
        let m = manifest(vec![
            convo("old-1", "conv-a"), // conv-a is live as new-19
            convo("new-19", "conv-a"),
            convo("solo", "conv-b"),   // open in a claude outside clawtree
            convo("twin-1", "conv-c"), // two dead entries, one conversation
            convo("twin-2", "conv-c"),
            entry("term", EntryKind::Terminal, "/p/main"),
        ]);
        let live: HashSet<String> = ["new-19".to_string()].into();
        let open: HashSet<String> = ["conv-b".to_string()].into();
        let d = diff(&m, &live, &open, true, |_| true);
        let names =
            |v: &Vec<LayoutEntry>| v.iter().map(|e| e.tmux_name.clone()).collect::<Vec<_>>();
        assert_eq!(names(&d.live), vec!["new-19"]);
        assert_eq!(names(&d.restorable), vec!["twin-1", "term"]);
        assert_eq!(names(&d.duplicates), vec!["old-1", "solo", "twin-2"]);
    }

    #[test]
    fn diff_partitions_entries() {
        let m = manifest(vec![
            entry("alive", EntryKind::Claude, "/p/main"),
            entry("missing", EntryKind::Claude, "/p/main"),
            entry("gone", EntryKind::Terminal, "/p/removed"),
            LayoutEntry {
                exited: true,
                ..entry("done", EntryKind::Claude, "/p/main")
            },
        ]);
        let live: HashSet<String> = ["alive".to_string()].into();
        let d = diff(&m, &live, &HashSet::new(), true, |e| {
            e.path == Path::new("/p/main")
        });
        let names =
            |v: &Vec<LayoutEntry>| v.iter().map(|e| e.tmux_name.clone()).collect::<Vec<_>>();
        assert_eq!(names(&d.live), vec!["alive"]);
        assert_eq!(names(&d.restorable), vec!["missing"]);
        assert_eq!(names(&d.stale), vec!["gone"]);

        // Unmounted project root: nothing is restorable or stale.
        let d = diff(&m, &live, &HashSet::new(), false, |_| true);
        assert!(d.restorable.is_empty() && d.stale.is_empty());
        assert_eq!(names(&d.live), vec!["alive"]);
    }

    #[test]
    fn mangles_like_claude_code() {
        assert_eq!(
            mangle_project_dir(Path::new("/home/u/a.b/c-d")),
            "-home-u-a-b-c-d"
        );
        assert_eq!(mangle_project_dir(Path::new("/x/.bare")), "-x--bare");
    }

    fn touch(path: &Path, mtime: u64) {
        std::fs::write(path, "{}\n").unwrap();
        let t = std::time::UNIX_EPOCH + std::time::Duration::from_secs(mtime);
        std::fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(t)
            .unwrap();
    }

    #[test]
    fn transcript_fallback_picks_newest_unclaimed() {
        let home = tempfile::tempdir().unwrap();
        let cwd = Path::new("/home/u/proj/main");
        let dir = claude_projects_dir(home.path()).join(mangle_project_dir(cwd));
        std::fs::create_dir_all(dir.join("subagents")).unwrap();
        touch(&dir.join("old.jsonl"), 50); // before created_at
        touch(&dir.join("mid.jsonl"), 200);
        touch(&dir.join("new.jsonl"), 300);
        touch(&dir.join("claimed.jsonl"), 400);
        touch(&dir.join("subagents").join("nested.jsonl"), 500);
        touch(&dir.join("notes.txt"), 600);
        let claimed: HashSet<String> = ["claimed".to_string()].into();
        let got = transcript_candidates(home.path(), cwd, 100, &claimed);
        assert_eq!(got, vec!["new", "mid"]);

        let mut e = entry("s", EntryKind::Claude, "/home/u/proj/main");
        e.created_at = 100;
        let src = resolve_resume(home.path(), &e, &claimed);
        assert_eq!(
            src,
            ResumeSource::Guessed {
                candidates: vec!["new".into(), "mid".into()],
                pick: 0
            }
        );
        // A recorded id with a transcript resumes it; without one, start fresh.
        e.claude_session_id = Some("mid".into());
        assert_eq!(
            resolve_resume(home.path(), &e, &claimed),
            ResumeSource::SessionId("mid".into())
        );
        e.claude_session_id = Some("never-used".into());
        assert_eq!(
            resolve_resume(home.path(), &e, &claimed),
            ResumeSource::Fresh
        );

        // Two id-less entries in the same folder never share a guess.
        let a = entry("a", EntryKind::Claude, "/home/u/proj/main");
        let b = entry("b", EntryKind::Claude, "/home/u/proj/main");
        let all = vec![a.clone(), b.clone()];
        let srcs = resolve_all(home.path(), &all, &all);
        assert_eq!(srcs[0].resumed_id(), Some("claimed"));
        assert_eq!(srcs[1].resumed_id(), Some("new"));
    }

    #[test]
    fn permission_mode_comes_from_the_transcript() {
        let home = tempfile::tempdir().unwrap();
        let cwd = Path::new("/home/u/proj/main");
        let dir = claude_projects_dir(home.path()).join(mangle_project_dir(cwd));
        std::fs::create_dir_all(&dir).unwrap();
        let line = |mode: &str| format!("{{\"type\":\"user\",\"permissionMode\":\"{}\"}}\n", mode);
        // Shift+Tab'd from default into plan: the last message wins.
        std::fs::write(dir.join("p.jsonl"), line("auto") + &line("plan")).unwrap();
        std::fs::write(dir.join("y.jsonl"), line("bypassPermissions")).unwrap();
        std::fs::write(dir.join("x.jsonl"), line("bogus")).unwrap();
        assert_eq!(last_permission_mode(&dir.join("x.jsonl")), None);

        let mut e = entry("s", EntryKind::Claude, "/home/u/proj/main");
        e.claude_session_id = Some("p".into());
        let row = RestoreRow::new(home.path(), e.clone(), ResumeSource::SessionId("p".into()));
        assert_eq!(row.permission_mode.as_deref(), Some("plan"));
        assert!(!row.skip_permissions());
        assert_eq!(
            row.claude_launch(None).claude_args(),
            vec!["--resume", "p", "--permission-mode", "plan"]
        );

        // A conversation last in bypass mode comes back YOLO, even if the
        // session predates clawtree recording the flag.
        let row = RestoreRow::new(home.path(), e, ResumeSource::SessionId("y".into()));
        assert!(row.skip_permissions());
        assert_eq!(row.claude_launch(None).claude_args(), vec!["--resume", "y"]);
        assert_eq!(row.mode_label().as_deref(), Some("bypassPermissions"));
    }

    #[test]
    fn uuid_has_v4_shape() {
        let u = new_uuid_v4();
        assert_eq!(u.len(), 36);
        assert_eq!(&u[14..15], "4");
        assert!(matches!(&u[19..20], "8" | "9" | "a" | "b"));
        assert_ne!(u, new_uuid_v4());
    }

    #[test]
    fn lock_is_exclusive() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.json");
        let held = try_lock(&path).unwrap();
        assert!(held.is_some());
        // flock locks are per open file description, so a second open conflicts.
        assert!(try_lock(&path).unwrap().is_none());
        drop(held);
        assert!(try_lock(&path).unwrap().is_some());
    }

    #[test]
    fn manifests_without_agent_read_as_claude() {
        let json = r#"{"tmux_name":"a","kind":"claude","scope":"worktree","path":"/p/main"}"#;
        let e: LayoutEntry = serde_json::from_str(json).unwrap();
        assert_eq!(e.agent, Agent::Claude);
        // ... and Claude entries are written without the field.
        assert!(!serde_json::to_string(&e).unwrap().contains("agent"));
        let mut g = e.clone();
        g.agent = Agent::Antigravity;
        g.agent_mode = Some("plan".into());
        let text = serde_json::to_string(&g).unwrap();
        assert!(text.contains(r#""agent":"antigravity""#), "{}", text);
        assert_eq!(serde_json::from_str::<LayoutEntry>(&text).unwrap(), g);
        assert_eq!(kind_label(&g), "agy");
        assert_eq!(kind_label(&e), "claude");
    }

    #[test]
    fn agy_entries_resume_their_conversation_in_its_mode() {
        let home = tempfile::tempdir().unwrap();
        let dir = agy::data_dir(home.path()).join("conversations");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("conv-1.db"), b"").unwrap();
        let mut e = entry("g", EntryKind::Claude, "/p/main");
        e.agent = Agent::Antigravity;
        e.agent_mode = Some("accept-edits".into());
        e.skip_permissions = true;

        e.claude_session_id = Some("conv-1".into());
        let row = RestoreRow::new(
            home.path(),
            e.clone(),
            resolve_resume(home.path(), &e, &HashSet::new()),
        );
        assert_eq!(row.source, ResumeSource::SessionId("conv-1".into()));
        assert!(row.skip_permissions());
        assert_eq!(
            row.claude_launch(Some("new".into())),
            AgentLaunch::Resume {
                id: "conv-1".into(),
                permission_mode: Some("accept-edits".into())
            }
        );

        // No recorded id (or a missing file): a fresh conversation, with no
        // Claude-style assigned id.
        e.claude_session_id = Some("gone".into());
        assert_eq!(
            resolve_resume(home.path(), &e, &HashSet::new()),
            ResumeSource::Fresh
        );
        e.claude_session_id = None;
        let row = RestoreRow::new(
            home.path(),
            e.clone(),
            resolve_resume(home.path(), &e, &HashSet::new()),
        );
        assert_eq!(row.source, ResumeSource::Fresh);
        assert_eq!(
            row.claude_launch(Some("new".into())),
            AgentLaunch::New {
                session_id: None,
                initial_prompt: None
            }
        );
    }
}

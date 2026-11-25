//! App-facing side of the layout manifest: turning the live `App` into
//! manifest entries, debounced writes, and rebuilding sessions after a crash.
//! The file format and pure logic live in `layout.rs`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use super::layout::{
    self, EntryKind, EntryScope, LayoutEntry, LayoutManifest, RestoreRow, SaveOutcome,
};
use super::{pty, spawn_in, SpawnOpts, SpawnParent};
use crate::app::{App, FocusTarget, InputMode, SidebarItem};

/// Minimum gap between manifest writes; bursts collapse into one write.
const WRITE_DEBOUNCE: Duration = Duration::from_millis(500);

fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_default())
}

impl App {
    /// Note that the session layout changed. No I/O: this runs on UI paths.
    pub fn mark_layout_dirty(&mut self) {
        self.layout_dirty = true;
    }

    fn active_tmux_name(&self) -> Option<String> {
        self.active_session_id
            .and_then(|sid| self.sessions.get(&sid))
            .and_then(|s| s.tmux_session_name.clone())
    }

    /// Write the manifest if the layout is dirty and the debounce allows.
    /// Called from the main loop every iteration.
    pub fn flush_layout_if_due(&mut self, now: Instant) {
        if !self.layout_enabled || !self.repo_detected {
            return;
        }
        let active = self.active_tmux_name();
        if active != self.layout_saved_active {
            // Focus changes are worth recording, but let the debounce absorb them.
            self.layout_dirty = true;
        }
        if !self.layout_dirty {
            return;
        }
        let entries = snapshot(self);
        let due = self
            .layout_last_write
            .is_none_or(|t| now.duration_since(t) >= WRITE_DEBOUNCE);
        if !due {
            // Keep the latest snapshot in memory for the panic hook.
            layout::stash_pending(
                &self.bare_repo_path,
                entries,
                &self.closed_tmux_names,
                active,
            );
            return;
        }
        self.write_layout(entries, active, now);
    }

    /// Write now, regardless of the debounce (clean quit).
    pub fn flush_layout_now(&mut self) {
        if !self.layout_enabled || !self.repo_detected {
            return;
        }
        let active = self.active_tmux_name();
        let entries = snapshot(self);
        self.write_layout(entries, active, Instant::now());
    }

    fn write_layout(&mut self, entries: Vec<LayoutEntry>, active: Option<String>, now: Instant) {
        self.layout_last_write = Some(now);
        match layout::write_merged(
            &self.bare_repo_path,
            entries,
            &self.closed_tmux_names,
            active.clone(),
        ) {
            SaveOutcome::Saved(m) => {
                self.layout_dirty = false;
                self.layout_saved_active = active;
                self.layout_reserved_names = m.entries.into_iter().map(|e| e.tmux_name).collect();
                layout::clear_pending();
            }
            // Another instance is writing; stay dirty and retry after the debounce.
            SaveOutcome::Busy => {}
            SaveOutcome::Refused => {
                self.layout_dirty = false;
                self.layout_saved_active = active;
            }
        }
    }
}

/// The live layout as manifest entries, in sidebar order: worktrees, then
/// project-root sessions, then locations, then the terminal panel. Sessions
/// without tmux (the non-tmux fallback) can't be reattached and are skipped.
pub fn snapshot(app: &App) -> Vec<LayoutEntry> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let mut push =
        |sid: u64, order: usize, scope: EntryScope, path: &Path, branch: Option<String>| {
            let Some(s) = app.sessions.get(&sid) else {
                return;
            };
            let Some(name) = s.tmux_session_name.clone() else {
                return;
            };
            if !seen.insert(name.clone()) {
                return;
            }
            out.push(LayoutEntry {
                tmux_name: name,
                kind: if s.is_terminal {
                    EntryKind::Terminal
                } else {
                    EntryKind::Claude
                },
                scope,
                path: path.to_path_buf(),
                cwd: if s.is_terminal {
                    app.terminal_cwds.get(&sid).cloned()
                } else {
                    None
                },
                branch,
                nickname: s.nickname.clone(),
                claude_session_id: s.claude_session_id.clone(),
                skip_permissions: s.skip_permissions,
                order,
                created_at: s.created_at,
                // Lost with the tmux server ≠ exited: those stay restorable.
                exited: s.exited.load(Ordering::Relaxed) && !s.lost_with_server,
            });
        };
    for wt in &app.worktrees {
        for (i, &sid) in wt.session_ids.iter().enumerate() {
            push(
                sid,
                i,
                EntryScope::Worktree,
                &wt.path,
                Some(wt.branch.clone()),
            );
        }
    }
    for (i, &sid) in app.project_session_ids.iter().enumerate() {
        push(sid, i, EntryScope::Root, &app.bare_repo_path, None);
    }
    for loc in &app.locations {
        for (i, &sid) in loc.session_ids.iter().enumerate() {
            push(sid, i, EntryScope::Location, &loc.path, loc.name.clone());
        }
    }
    for (i, &sid) in app.terminal_ids.iter().enumerate() {
        if let Some(s) = app.sessions.get(&sid) {
            let path = s.worktree_path.clone();
            let scope = if path == app.bare_repo_path {
                EntryScope::Root
            } else if app.locations.iter().any(|l| l.path == path) {
                EntryScope::Location
            } else {
                EntryScope::Worktree
            };
            push(sid, i, scope, &path, None);
        }
    }
    out
}

/// Where an entry would go in the current sidebar, if its parent still exists.
pub fn parent_for_entry(app: &App, e: &LayoutEntry) -> Option<SpawnParent> {
    match e.scope {
        EntryScope::Worktree => app
            .worktrees
            .iter()
            .position(|w| w.path == e.path)
            .map(SpawnParent::Worktree),
        EntryScope::Root => (e.path == app.bare_repo_path).then_some(SpawnParent::Root),
        EntryScope::Location => app
            .locations
            .iter()
            .position(|l| l.path == e.path)
            .map(SpawnParent::Location),
    }
}

/// Fill in what tmux can't tell reconnect (nicknames; ids and creation
/// times it didn't record) from the manifest, and remember its order.
pub fn apply_manifest(app: &mut App, m: &LayoutManifest) {
    let by_name: HashMap<&str, &LayoutEntry> = m
        .entries
        .iter()
        .map(|e| (e.tmux_name.as_str(), e))
        .collect();
    for s in app.sessions.values_mut() {
        let Some(e) = s.tmux_session_name.as_deref().and_then(|n| by_name.get(n)) else {
            continue;
        };
        if s.nickname.is_none() {
            s.nickname = e.nickname.clone();
        }
        if s.claude_session_id.is_none() && !s.is_terminal {
            s.claude_session_id = e.claude_session_id.clone();
        }
        if !s.is_terminal && e.skip_permissions {
            s.skip_permissions = true;
        }
        if e.created_at > 0 && (s.created_at == 0 || e.created_at < s.created_at) {
            s.created_at = e.created_at;
        }
    }
    app.layout_initial_order = m
        .entries
        .iter()
        .map(|e| (e.tmux_name.clone(), e.order))
        .collect();
    app.layout_reserved_names = m.entries.iter().map(|e| e.tmux_name.clone()).collect();
    sort_by_manifest_order(app);
}

/// Stable-sort each parent's sessions by their recorded order; sessions the
/// manifest doesn't know keep their relative order at the end.
pub fn sort_by_manifest_order(app: &mut App) {
    let order = &app.layout_initial_order;
    let sessions = &app.sessions;
    let key = |sid: &u64| {
        sessions
            .get(sid)
            .and_then(|s| s.tmux_session_name.as_ref())
            .and_then(|n| order.get(n))
            .copied()
            .unwrap_or(usize::MAX)
    };
    for wt in &mut app.worktrees {
        wt.session_ids.sort_by_key(key);
    }
    app.project_session_ids.sort_by_key(key);
    for loc in &mut app.locations {
        loc.session_ids.sort_by_key(key);
    }
    app.rebuild_sidebar_items();
}

/// What the startup check found: rows to offer and stale entries to report.
pub struct RestorePlan {
    pub rows: Vec<RestoreRow>,
    pub stale: Vec<LayoutEntry>,
    pub active_tmux_name: Option<String>,
}

/// Compare the manifest with the tmux sessions that exist now (on any
/// instance — a session attached elsewhere is not missing).
pub fn plan_restore(app: &App, m: &LayoutManifest) -> RestorePlan {
    let live: HashSet<String> = pty::list_tmux_session_names().into_iter().collect();
    let d = layout::diff(m, &live, app.bare_repo_path.is_dir(), |e| {
        e.path.is_dir() && parent_for_entry(app, e).is_some()
    });
    RestorePlan {
        rows: layout::plan_rows(&home(), &d.restorable, &m.entries),
        stale: d.stale,
        active_tmux_name: m.active_tmux_name.clone(),
    }
}

/// Recreate one session from a restore row. Returns its new session id.
pub fn restore_row(
    app: &mut App,
    row: &RestoreRow,
    terminal_size: (u16, u16),
) -> anyhow::Result<u64> {
    let e = &row.entry;
    let parent = parent_for_entry(app, e)
        .ok_or_else(|| anyhow::anyhow!("its folder is no longer in the sidebar"))?;
    let is_terminal = e.kind == EntryKind::Terminal;
    let launch = (!is_terminal).then(|| row.claude_launch(layout::assign_session_id()));
    let opts = SpawnOpts {
        preferred_tmux_name: Some(e.tmux_name.clone()),
        launch,
        nickname: e.nickname.clone(),
        start_dir: e.cwd.clone(),
        created_at: (e.created_at > 0).then_some(e.created_at),
    };
    spawn_in(
        app,
        parent,
        is_terminal,
        terminal_size,
        row.skip_permissions(),
        None,
        opts,
    )
}

/// Mark entries as explicitly closed so the next write drops them. Never
/// touches transcripts.
pub fn discard(app: &mut App, names: impl IntoIterator<Item = String>) {
    for n in names {
        app.layout_reserved_names.remove(&n);
        app.closed_tmux_names.insert(n);
    }
    app.mark_layout_dirty();
}

/// After a restore: put sessions back in their recorded order and focus the
/// session that was focused before the crash.
pub fn finish_restore(app: &mut App, focus: Option<&str>) {
    sort_by_manifest_order(app);
    let Some(name) = focus else {
        return;
    };
    let Some(sid) = app
        .sessions
        .values()
        .find(|s| s.tmux_session_name.as_deref() == Some(name))
        .map(|s| s.id)
    else {
        return;
    };
    if let Some(idx) = app.sidebar_items.iter().position(|item| match *item {
        SidebarItem::ProjectSession(si) => app.project_session_ids.get(si) == Some(&sid),
        SidebarItem::Session(wi, si) => {
            app.worktrees.get(wi).and_then(|w| w.session_ids.get(si)) == Some(&sid)
        }
        SidebarItem::LocationSession(li, si) => {
            app.locations.get(li).and_then(|l| l.session_ids.get(si)) == Some(&sid)
        }
        _ => false,
    }) {
        app.sidebar_selected = idx;
    }
    app.active_session_id = Some(sid);
    app.project_overview_active = false;
    app.active_worktree_idx = None;
    app.worktree_status = None;
    app.terminal_scroll = 0;
    app.focus = FocusTarget::TerminalPane;
    app.input_mode = InputMode::Terminal;
    app.url_cache_dirty = true;
}

/// Stale entries as printable lines (name + conversation id for a manual
/// `claude --resume`).
pub fn describe_stale(e: &LayoutEntry) -> String {
    match &e.claude_session_id {
        Some(id) => format!(
            "{} — {} — claude --resume {}",
            layout::entry_label(e),
            e.path.display(),
            id
        ),
        None => format!("{} — {}", layout::entry_label(e), e.path.display()),
    }
}

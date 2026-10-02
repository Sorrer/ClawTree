pub mod git;

use anyhow::Result;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::mpsc;

use crate::app::App;
use crate::event::AppEvent;

/// Cached git status info for display in the worktree info panel.
#[derive(Debug, Clone)]
pub struct WorktreeStatus {
    pub files: Vec<git::FileChange>,
    pub recent_commits: Vec<String>,
    pub head_subject: String,
    pub unpushed_commits: Vec<String>,
}

/// A worktree with its associated sessions.
#[derive(Debug, Clone)]
pub struct Worktree {
    pub path: PathBuf,
    pub branch: String,
    pub commit_hash: String,
    pub session_ids: Vec<u64>,
    pub expanded: bool,
}

/// Refresh the worktree list from git.
pub fn refresh_worktrees(app: &mut App) -> Result<()> {
    let entries = git::list_worktrees(&app.bare_repo_path)?;

    // Preserve existing session associations
    let old_sessions: std::collections::HashMap<PathBuf, Vec<u64>> = app
        .worktrees
        .iter()
        .map(|wt| (wt.path.clone(), wt.session_ids.clone()))
        .collect();

    let old_expanded: std::collections::HashMap<PathBuf, bool> = app
        .worktrees
        .iter()
        .map(|wt| (wt.path.clone(), wt.expanded))
        .collect();

    app.worktrees = entries
        .into_iter()
        .filter(|e| !e.is_bare) // Don't show the bare repo itself
        .map(|e| {
            let session_ids = old_sessions.get(&e.path).cloned().unwrap_or_default();
            let expanded = old_expanded
                .get(&e.path)
                .copied()
                .unwrap_or(!session_ids.is_empty());
            Worktree {
                path: e.path,
                branch: e.branch.unwrap_or_else(|| "detached".to_string()),
                commit_hash: if e.head.len() > 8 {
                    e.head[..8].to_string()
                } else {
                    e.head
                },
                session_ids,
                expanded,
            }
        })
        .collect();

    app.rebuild_sidebar_items();
    Ok(())
}

/// Remove a worktree. Kills any associated sessions and terminals first.
pub fn remove_worktree(app: &mut App, worktree_path: &Path) -> Result<()> {
    kill_worktree_sessions(app, worktree_path);
    git::remove_worktree(&app.bare_repo_path, worktree_path)
}

/// Force-remove a worktree (even if dirty). Kills sessions and terminals first.
pub fn force_remove_worktree(app: &mut App, worktree_path: &Path) -> Result<()> {
    kill_worktree_sessions(app, worktree_path);
    git::force_remove_worktree(&app.bare_repo_path, worktree_path)
}

/// Kill all sessions (agents and terminals) associated with a worktree path.
fn kill_worktree_sessions(app: &mut App, worktree_path: &Path) {
    // Kill agent sessions under this worktree
    if let Some(wt) = app.worktrees.iter().find(|w| w.path == worktree_path) {
        let sids: Vec<u64> = wt.session_ids.clone();
        for sid in sids {
            crate::session::kill_session(app, sid);
        }
    }
    // Kill terminal sessions whose worktree_path matches
    let terminal_sids: Vec<u64> = app
        .terminal_ids
        .iter()
        .filter(|&&tid| {
            app.sessions
                .get(&tid)
                .map(|s| s.worktree_path == worktree_path)
                .unwrap_or(false)
        })
        .copied()
        .collect();
    for sid in terminal_sids {
        crate::session::kill_session(app, sid);
    }
}

/// Check if a worktree's working tree is clean (all changes committed).
pub fn is_worktree_clean(app: &App, worktree_idx: usize) -> Result<bool> {
    let wt = app
        .worktrees
        .get(worktree_idx)
        .ok_or_else(|| anyhow::anyhow!("Invalid worktree index"))?;
    git::is_worktree_clean(&wt.path)
}

/// Find the worktree index for a given branch name, if one exists.
pub fn find_worktree_for_branch(app: &App, branch: &str) -> Option<usize> {
    app.worktrees.iter().position(|wt| wt.branch == branch)
}

/// Merge a source branch into a target worktree's branch.
pub fn merge_into_worktree(
    app: &App,
    worktree_idx: usize,
    source_branch: &str,
) -> Result<git::MergeResult> {
    let wt = app
        .worktrees
        .get(worktree_idx)
        .ok_or_else(|| anyhow::anyhow!("Invalid worktree index"))?;
    git::merge_branch(&wt.path, source_branch)
}

/// Check if a worktree has an in-progress merge. Returns the source branch name if so.
pub fn merge_in_progress(app: &App, worktree_idx: usize) -> Option<String> {
    let wt = app.worktrees.get(worktree_idx)?;
    git::merge_in_progress(&wt.path)
}

/// Abort a merge in progress on the given worktree.
pub fn merge_abort(app: &App, worktree_idx: usize) -> Result<()> {
    let wt = app
        .worktrees
        .get(worktree_idx)
        .ok_or_else(|| anyhow::anyhow!("Invalid worktree index"))?;
    git::merge_abort(&wt.path)
}

/// Fetch worktree status (file changes, recent commits, HEAD subject) for display.
pub fn fetch_worktree_status(app: &App, worktree_idx: usize) -> Result<WorktreeStatus> {
    let wt = app
        .worktrees
        .get(worktree_idx)
        .ok_or_else(|| anyhow::anyhow!("Invalid worktree index"))?;
    let files = git::status_porcelain(&wt.path)?;
    let recent_commits = git::log_oneline(&wt.path, 10).unwrap_or_default();
    let head_subject = git::head_subject(&wt.path).unwrap_or_default();
    let unpushed_commits = git::unpushed_commits(&wt.path);
    Ok(WorktreeStatus {
        files,
        recent_commits,
        head_subject,
        unpushed_commits,
    })
}

/// Fetch worktree status by path (no App reference needed).
/// Used by the background status poller thread.
pub fn fetch_worktree_status_by_path(path: &Path) -> Result<WorktreeStatus> {
    let files = git::status_porcelain(path)?;
    let recent_commits = git::log_oneline(path, 10).unwrap_or_default();
    let head_subject = git::head_subject(path).unwrap_or_default();
    let unpushed_commits = git::unpushed_commits(path);
    Ok(WorktreeStatus {
        files,
        recent_commits,
        head_subject,
        unpushed_commits,
    })
}

/// Background refresh interval for worktree status polling.
pub const STATUS_REFRESH_INTERVAL: Duration = Duration::from_secs(10);

/// Delay between fetching each individual worktree's status (stagger to avoid lag).
const STATUS_STAGGER_DELAY: Duration = Duration::from_millis(300);

/// Spawn a background thread that periodically fetches git status for all worktrees.
/// Each worktree is fetched with a small stagger delay to avoid system lag.
/// Results are sent back via AppEvent::WorktreeStatusReady.
pub fn spawn_status_poller(
    event_tx: mpsc::UnboundedSender<AppEvent>,
    worktree_paths: Arc<Mutex<Vec<PathBuf>>>,
    git_lock: Arc<Mutex<()>>,
) {
    std::thread::Builder::new()
        .name("status-poller".into())
        .spawn(move || {
            loop {
                let paths: Vec<PathBuf> =
                    worktree_paths.lock().map(|p| p.clone()).unwrap_or_default();

                if paths.is_empty() {
                    std::thread::sleep(Duration::from_secs(1));
                    continue;
                }

                let cycle_start = Instant::now();
                let next_refresh_at = cycle_start + STATUS_REFRESH_INTERVAL;

                for path in &paths {
                    // Use try_lock so we skip this worktree if a user-initiated
                    // git operation (commit, merge, push, pull, etc.) is running.
                    // This prevents "index.lock exists" errors from concurrent access.
                    if let Ok(_guard) = git_lock.try_lock() {
                        if let Ok(status) = fetch_worktree_status_by_path(path) {
                            if event_tx
                                .send(AppEvent::WorktreeStatusReady {
                                    worktree_path: path.clone(),
                                    status,
                                    next_refresh_at,
                                })
                                .is_err()
                            {
                                return; // channel closed, app shutting down
                            }
                        }
                    }
                    // Stagger between worktrees to avoid I/O burst
                    std::thread::sleep(STATUS_STAGGER_DELAY);
                }

                // Sleep until next cycle
                let elapsed = cycle_start.elapsed();
                if elapsed < STATUS_REFRESH_INTERVAL {
                    std::thread::sleep(STATUS_REFRESH_INTERVAL - elapsed);
                }
            }
        })
        .ok();
}

/// How often the worktree list watcher checks for filesystem changes.
const WORKTREE_WATCH_INTERVAL: Duration = Duration::from_secs(2);

/// Modification times of the directories whose changes signal that worktrees
/// may have been added or removed outside clawtree: the repo root (where
/// worktree folders usually live) and the git dir's `worktrees/` registry
/// (where `git worktree add/remove/prune` record every worktree).
fn worktree_dirs_fingerprint(bare_repo_path: &Path) -> Vec<Option<std::time::SystemTime>> {
    let dot_bare = bare_repo_path.join(".bare");
    let git_dir = if dot_bare.is_dir() {
        dot_bare
    } else {
        bare_repo_path.to_path_buf()
    };
    [bare_repo_path.to_path_buf(), git_dir.join("worktrees")]
        .iter()
        .map(|dir| std::fs::metadata(dir).and_then(|m| m.modified()).ok())
        .collect()
}

/// Spawn a background thread that watches the repo root and git worktree
/// registry for changes and sends AppEvent::WorktreeDirsChanged when they
/// move, so the main loop can re-list worktrees created by other tools.
/// Only stats two directories per cycle; git is not invoked here.
pub fn spawn_worktree_list_watcher(event_tx: mpsc::UnboundedSender<AppEvent>, bare_repo_path: PathBuf) {
    std::thread::Builder::new()
        .name("worktree-watcher".into())
        .spawn(move || {
            let mut last = worktree_dirs_fingerprint(&bare_repo_path);
            loop {
                std::thread::sleep(WORKTREE_WATCH_INTERVAL);
                let current = worktree_dirs_fingerprint(&bare_repo_path);
                if current != last {
                    last = current;
                    if event_tx.send(AppEvent::WorktreeDirsChanged).is_err() {
                        return; // channel closed, app shutting down
                    }
                }
            }
        })
        .ok();
}

/// Re-list worktrees from git and apply the result only if it differs from
/// what is shown. Index-based UI state (active worktree, sidebar selection) is
/// carried over by worktree path so an insertion or removal doesn't shift the
/// user onto a different worktree. Returns true if the list changed.
pub fn sync_worktrees(app: &mut App) -> Result<bool> {
    let entries = git::list_worktrees(&app.bare_repo_path)?;
    let fresh: Vec<(PathBuf, String)> = entries
        .into_iter()
        .filter(|e| !e.is_bare)
        .map(|e| (e.path, e.branch.unwrap_or_else(|| "detached".to_string())))
        .collect();
    let unchanged = fresh.len() == app.worktrees.len()
        && fresh
            .iter()
            .zip(&app.worktrees)
            .all(|((path, branch), wt)| *path == wt.path && *branch == wt.branch);
    if unchanged {
        return Ok(false);
    }

    use crate::app::SidebarItem;
    let path_of = |app: &App, wi: usize| app.worktrees.get(wi).map(|wt| wt.path.clone());
    let active_path = app.active_worktree_idx.and_then(|wi| path_of(app, wi));
    let selected = app.sidebar_items.get(app.sidebar_selected).copied();
    let selected_path = match selected {
        Some(SidebarItem::Worktree(wi)) | Some(SidebarItem::Session(wi, _)) => path_of(app, wi),
        _ => None,
    };

    refresh_worktrees(app)?;

    let index_of = |app: &App, path: &Path| app.worktrees.iter().position(|wt| wt.path == path);
    if let Some(path) = active_path {
        app.active_worktree_idx = index_of(app, &path);
        if app.active_worktree_idx.is_none() {
            // The worktree being viewed was removed externally.
            app.worktree_status = None;
            app.project_overview_active = app.active_session_id.is_none();
        }
    }
    let new_selected = match (selected, selected_path) {
        (Some(SidebarItem::Worktree(_)), Some(path)) => {
            index_of(app, &path).map(SidebarItem::Worktree)
        }
        (Some(SidebarItem::Session(_, si)), Some(path)) => {
            index_of(app, &path).map(|wi| SidebarItem::Session(wi, si))
        }
        (other, _) => other,
    };
    app.sidebar_selected = new_selected
        .and_then(|item| app.sidebar_items.iter().position(|i| *i == item))
        .unwrap_or(0);
    app.ensure_sidebar_selected_visible();
    app.clamp_info_panel_cursor();
    Ok(true)
}

/// Collect worktree paths for the status poller's shared state.
pub fn collect_worktree_paths(app: &App) -> Vec<PathBuf> {
    app.worktrees.iter().map(|wt| wt.path.clone()).collect()
}

/// List branches available for merging.
pub fn available_branches(app: &App) -> Result<Vec<String>> {
    git::list_branches(&app.bare_repo_path)
}

/// Get file status for a worktree (porcelain format).
pub fn status_porcelain(app: &App, worktree_idx: usize) -> Result<Vec<git::FileChange>> {
    let wt = app
        .worktrees
        .get(worktree_idx)
        .ok_or_else(|| anyhow::anyhow!("Invalid worktree index"))?;
    git::status_porcelain(&wt.path)
}

/// Stage a single file in a worktree.
pub fn stage_file(app: &App, worktree_idx: usize, file: &str) -> Result<()> {
    let wt = app
        .worktrees
        .get(worktree_idx)
        .ok_or_else(|| anyhow::anyhow!("Invalid worktree index"))?;
    git::stage_file(&wt.path, file)
}

/// Unstage a single file in a worktree.
pub fn unstage_file(app: &App, worktree_idx: usize, file: &str) -> Result<()> {
    let wt = app
        .worktrees
        .get(worktree_idx)
        .ok_or_else(|| anyhow::anyhow!("Invalid worktree index"))?;
    git::unstage_file(&wt.path, file)
}

/// Stage all files in a worktree.
pub fn stage_all(app: &App, worktree_idx: usize) -> Result<()> {
    let wt = app
        .worktrees
        .get(worktree_idx)
        .ok_or_else(|| anyhow::anyhow!("Invalid worktree index"))?;
    git::stage_all(&wt.path)
}

/// Get the diff of staged changes in a worktree.
pub fn diff_staged(app: &App, worktree_idx: usize) -> Result<String> {
    let wt = app
        .worktrees
        .get(worktree_idx)
        .ok_or_else(|| anyhow::anyhow!("Invalid worktree index"))?;
    git::diff_staged(&wt.path)
}

/// Commit staged changes in a worktree.
pub fn commit(app: &App, worktree_idx: usize, message: &str) -> Result<()> {
    let wt = app
        .worktrees
        .get(worktree_idx)
        .ok_or_else(|| anyhow::anyhow!("Invalid worktree index"))?;
    git::commit(&wt.path, message)
}

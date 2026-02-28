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
            let session_ids = old_sessions
                .get(&e.path)
                .cloned()
                .unwrap_or_default();
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
    let terminal_sids: Vec<u64> = app.terminal_ids.iter()
        .filter(|&&tid| {
            app.sessions.get(&tid)
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
    let wt = app.worktrees.get(worktree_idx)
        .ok_or_else(|| anyhow::anyhow!("Invalid worktree index"))?;
    git::is_worktree_clean(&wt.path)
}

/// Find the worktree index for a given branch name, if one exists.
pub fn find_worktree_for_branch(app: &App, branch: &str) -> Option<usize> {
    app.worktrees.iter().position(|wt| wt.branch == branch)
}

/// Merge a source branch into a target worktree's branch.
pub fn merge_into_worktree(app: &App, worktree_idx: usize, source_branch: &str) -> Result<git::MergeResult> {
    let wt = app.worktrees.get(worktree_idx)
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
    let wt = app.worktrees.get(worktree_idx)
        .ok_or_else(|| anyhow::anyhow!("Invalid worktree index"))?;
    git::merge_abort(&wt.path)
}

/// Fetch worktree status (file changes, recent commits, HEAD subject) for display.
pub fn fetch_worktree_status(app: &App, worktree_idx: usize) -> Result<WorktreeStatus> {
    let wt = app.worktrees.get(worktree_idx)
        .ok_or_else(|| anyhow::anyhow!("Invalid worktree index"))?;
    let files = git::status_porcelain(&wt.path)?;
    let recent_commits = git::log_oneline(&wt.path, 10).unwrap_or_default();
    let head_subject = git::head_subject(&wt.path).unwrap_or_default();
    let unpushed_commits = git::unpushed_commits(&wt.path);
    Ok(WorktreeStatus { files, recent_commits, head_subject, unpushed_commits })
}

/// Fetch worktree status by path (no App reference needed).
/// Used by the background status poller thread.
pub fn fetch_worktree_status_by_path(path: &Path) -> Result<WorktreeStatus> {
    let files = git::status_porcelain(path)?;
    let recent_commits = git::log_oneline(path, 10).unwrap_or_default();
    let head_subject = git::head_subject(path).unwrap_or_default();
    let unpushed_commits = git::unpushed_commits(path);
    Ok(WorktreeStatus { files, recent_commits, head_subject, unpushed_commits })
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
) {
    std::thread::Builder::new()
        .name("status-poller".into())
        .spawn(move || {
            loop {
                let paths: Vec<PathBuf> = worktree_paths
                    .lock()
                    .map(|p| p.clone())
                    .unwrap_or_default();

                if paths.is_empty() {
                    std::thread::sleep(Duration::from_secs(1));
                    continue;
                }

                let cycle_start = Instant::now();
                let next_refresh_at = cycle_start + STATUS_REFRESH_INTERVAL;

                for path in &paths {
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
    let wt = app.worktrees.get(worktree_idx)
        .ok_or_else(|| anyhow::anyhow!("Invalid worktree index"))?;
    git::status_porcelain(&wt.path)
}

/// Stage a single file in a worktree.
pub fn stage_file(app: &App, worktree_idx: usize, file: &str) -> Result<()> {
    let wt = app.worktrees.get(worktree_idx)
        .ok_or_else(|| anyhow::anyhow!("Invalid worktree index"))?;
    git::stage_file(&wt.path, file)
}

/// Unstage a single file in a worktree.
pub fn unstage_file(app: &App, worktree_idx: usize, file: &str) -> Result<()> {
    let wt = app.worktrees.get(worktree_idx)
        .ok_or_else(|| anyhow::anyhow!("Invalid worktree index"))?;
    git::unstage_file(&wt.path, file)
}

/// Stage all files in a worktree.
pub fn stage_all(app: &App, worktree_idx: usize) -> Result<()> {
    let wt = app.worktrees.get(worktree_idx)
        .ok_or_else(|| anyhow::anyhow!("Invalid worktree index"))?;
    git::stage_all(&wt.path)
}

/// Get the diff of staged changes in a worktree.
pub fn diff_staged(app: &App, worktree_idx: usize) -> Result<String> {
    let wt = app.worktrees.get(worktree_idx)
        .ok_or_else(|| anyhow::anyhow!("Invalid worktree index"))?;
    git::diff_staged(&wt.path)
}

/// Commit staged changes in a worktree.
pub fn commit(app: &App, worktree_idx: usize, message: &str) -> Result<()> {
    let wt = app.worktrees.get(worktree_idx)
        .ok_or_else(|| anyhow::anyhow!("Invalid worktree index"))?;
    git::commit(&wt.path, message)
}

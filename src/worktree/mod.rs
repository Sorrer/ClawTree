pub mod git;

use anyhow::Result;
use std::path::{Path, PathBuf};

use crate::app::App;

/// Cached git status info for display in the worktree info panel.
#[derive(Debug, Clone)]
pub struct WorktreeStatus {
    pub files: Vec<git::FileChange>,
    pub recent_commits: Vec<String>,
    pub head_subject: String,
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

/// Remove a worktree. Kills any associated sessions first.
pub fn remove_worktree(app: &mut App, worktree_path: &Path) -> Result<()> {
    // Kill sessions associated with this worktree
    if let Some(wt) = app.worktrees.iter().find(|w| w.path == worktree_path) {
        let sids: Vec<u64> = wt.session_ids.clone();
        for sid in sids {
            crate::session::kill_session(app, sid);
        }
    }

    git::remove_worktree(&app.bare_repo_path, worktree_path)
}

/// Force-remove a worktree (even if dirty). Kills sessions first.
pub fn force_remove_worktree(app: &mut App, worktree_path: &Path) -> Result<()> {
    if let Some(wt) = app.worktrees.iter().find(|w| w.path == worktree_path) {
        let sids: Vec<u64> = wt.session_ids.clone();
        for sid in sids {
            crate::session::kill_session(app, sid);
        }
    }

    git::force_remove_worktree(&app.bare_repo_path, worktree_path)
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
    Ok(WorktreeStatus { files, recent_commits, head_subject })
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

/// Commit staged changes in a worktree.
pub fn commit(app: &App, worktree_idx: usize, message: &str) -> Result<()> {
    let wt = app.worktrees.get(worktree_idx)
        .ok_or_else(|| anyhow::anyhow!("Invalid worktree index"))?;
    git::commit(&wt.path, message)
}

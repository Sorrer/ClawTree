pub mod git;

use anyhow::Result;
use std::path::{Path, PathBuf};

use crate::app::App;

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

/// Create a new worktree.
pub fn create_worktree(app: &App, branch: &str, rel_path: &str) -> Result<()> {
    git::create_worktree(&app.bare_repo_path, branch, rel_path)
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

/// Merge a source branch into a target worktree's branch.
pub fn merge_into_worktree(app: &App, worktree_idx: usize, source_branch: &str) -> Result<String> {
    let wt = app.worktrees.get(worktree_idx)
        .ok_or_else(|| anyhow::anyhow!("Invalid worktree index"))?;
    git::merge_branch(&wt.path, source_branch)
}

/// List branches available for merging.
pub fn available_branches(app: &App) -> Result<Vec<String>> {
    git::list_branches(&app.bare_repo_path)
}

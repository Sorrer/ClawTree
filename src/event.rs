use std::path::PathBuf;
use std::time::Instant;

use crossterm::event::Event as CrosstermEvent;

use crate::session::{ClaudeUsage, GlobalUsage};
use crate::worktree::WorktreeStatus;

/// Unified event type for the application.
pub enum AppEvent {
    /// A crossterm input event (key press, mouse, resize).
    Input(CrosstermEvent),
    /// PTY output is available.
    PtyOutput,
    /// PTY process has exited for the given session.
    PtyExited { session_id: u64 },
    /// Periodic tick for UI refresh.
    Tick,
    /// Background worktree creation completed.
    WorktreeCreated {
        branch: String,
        error: Option<String>,
    },
    /// Background repo init completed.
    InitRepoComplete {
        error: Option<String>,
    },
    /// Background repo conversion completed.
    ConvertRepoComplete {
        bare_repo_path: PathBuf,
        error: Option<String>,
    },
    /// Background git push completed.
    PushComplete {
        branch: String,
        error: Option<String>,
    },
    /// Background git pull completed.
    PullComplete {
        branch: String,
        worktree_idx: usize,
        /// None = success, Some = error message.
        error: Option<String>,
        /// True if the error is a merge conflict (vs other errors).
        has_conflicts: bool,
    },
    /// Tmux title(s) changed — maps session_id to new title.
    TmuxTitlesChanged {
        updates: Vec<(u64, String)>,
    },
    /// Background git status fetch completed for a worktree.
    WorktreeStatusReady {
        worktree_path: PathBuf,
        status: WorktreeStatus,
        next_refresh_at: Instant,
    },
    /// Claude Code context usage data updated from debug logs.
    ClaudeUsageUpdated {
        updates: Vec<(u64, ClaudeUsage)>,
    },
    /// Global account-level usage data updated from the Anthropic API.
    GlobalUsageUpdated {
        usage: GlobalUsage,
    },
    /// A one-shot Claude summary completed for a session.
    SummaryReady {
        session_id: u64,
        summary: String,
    },
    /// Claude-generated commit message is ready.
    ClaudeCommitMessageReady {
        worktree_idx: usize,
        message: Result<String, String>,
    },
    /// A newer version is available on GitHub Releases.
    UpdateAvailable {
        latest_version: String,
    },
}

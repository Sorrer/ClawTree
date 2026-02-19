use crossterm::event::Event as CrosstermEvent;

/// Unified event type for the application.
#[derive(Debug)]
pub enum AppEvent {
    /// A crossterm input event (key press, mouse, resize).
    Input(CrosstermEvent),
    /// PTY output is available for the given session.
    PtyOutput { session_id: u64 },
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
    /// Background git push completed.
    PushComplete {
        branch: String,
        error: Option<String>,
    },
    /// Tmux title(s) changed — maps session_id to new title.
    TmuxTitlesChanged {
        updates: Vec<(u64, String)>,
    },
}

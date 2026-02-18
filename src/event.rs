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
}

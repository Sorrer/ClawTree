pub mod dialogs;
pub mod sidebar;
pub mod status_bar;
pub mod terminal_pane;

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout};

use crate::app::App;

/// Draw the full UI.
pub fn draw(f: &mut Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),    // main area
            Constraint::Length(1), // status bar
        ])
        .split(f.area());

    if app.sidebar_visible {
        let main_chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage(30), // sidebar
                Constraint::Percentage(70), // terminal pane
            ])
            .split(chunks[0]);

        sidebar::draw(f, app, main_chunks[0]);
        terminal_pane::draw(f, app, main_chunks[1]);
    } else {
        terminal_pane::draw(f, app, chunks[0]);
    }

    status_bar::draw(f, app, chunks[1]);

    // Draw dialog on top if present
    if app.dialog.is_some() {
        dialogs::draw(f, app);
    }
}

pub mod dialogs;
pub mod sidebar;
pub mod status_bar;
pub mod terminal_pane;
pub mod welcome;

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

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

    if !app.repo_detected {
        welcome::draw(f, app, chunks[0]);
    } else if app.sidebar_visible {
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

    // Draw loading overlay on top of everything
    if let Some(ref msg) = app.loading_message {
        draw_loading_overlay(f, msg);
    }
}

fn draw_loading_overlay(f: &mut Frame, message: &str) {
    let width = (message.len() as u16 + 6).max(20).min(f.area().width);
    let area = centered_rect(width, 3, f.area());
    f.render_widget(Clear, area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow));

    let inner = block.inner(area);
    f.render_widget(block, area);

    let line = Line::from(vec![
        Span::styled("  ", Style::default()),
        Span::styled(message, Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
    ]);
    f.render_widget(Paragraph::new(line), inner);
}

fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let x = area.x + area.width.saturating_sub(width) / 2;
    let y = area.y + area.height.saturating_sub(height) / 2;
    Rect::new(x, y, width.min(area.width), height.min(area.height))
}

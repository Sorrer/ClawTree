pub mod dialogs;
pub mod help;
pub mod mini_mode;
pub mod prompt_queue;
pub mod sidebar;
pub mod status_bar;
pub mod terminal_pane;
pub mod theme;
pub mod welcome;

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

use crate::app::{App, ScreenMode};

/// Draw the full UI, dispatching by screen mode.
pub fn draw(f: &mut Frame, app: &App) {
    // Reset all layout areas so hidden panels don't have stale hit regions
    app.areas.reset();

    match app.screen_mode {
        ScreenMode::Normal => draw_normal(f, app),
        ScreenMode::Mini => mini_mode::draw(f, app),
        ScreenMode::MiniDrilldown => mini_mode::draw_drilldown(f, app),
    }
}

/// Draw the normal (full sidebar + terminal) mode.
fn draw_normal(f: &mut Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),    // main area
            Constraint::Length(1), // status bar
        ])
        .split(f.area());

    let show_queue = app.prompt_queue_visible && app.active_session_id.is_some();

    if !app.repo_detected {
        welcome::draw(f, app, chunks[0]);
    } else if app.sidebar_visible {
        let main_chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Max(theme::SIDEBAR_MAX_WIDTH), // sidebar
                Constraint::Min(0),                        // terminal pane
            ])
            .split(chunks[0]);

        // Record sidebar area for mouse hit-testing
        app.areas.sidebar.set(main_chunks[0]);

        // Split sidebar area for terminal panel and global usage panel
        {
            let has_terminals = !app.terminal_ids.is_empty();
            let has_usage = app.global_usage.is_some();
            let terminal_panel_height = if has_terminals {
                (app.terminal_ids.len() as u16 + 2).min(8)
            } else {
                0
            };

            match (has_terminals, has_usage) {
                (true, true) => {
                    let sidebar_chunks = Layout::default()
                        .direction(Direction::Vertical)
                        .constraints([
                            Constraint::Min(5),                        // worktree list
                            Constraint::Length(terminal_panel_height), // terminal panel
                            Constraint::Length(5),                     // global usage
                        ])
                        .split(main_chunks[0]);
                    sidebar::draw(f, app, sidebar_chunks[0]);
                    sidebar::draw_terminal_panel(f, app, sidebar_chunks[1]);
                    sidebar::draw_global_usage(f, app, sidebar_chunks[2]);
                }
                (true, false) => {
                    let sidebar_chunks = Layout::default()
                        .direction(Direction::Vertical)
                        .constraints([
                            Constraint::Min(5),                        // worktree list
                            Constraint::Length(terminal_panel_height), // terminal panel
                        ])
                        .split(main_chunks[0]);
                    sidebar::draw(f, app, sidebar_chunks[0]);
                    sidebar::draw_terminal_panel(f, app, sidebar_chunks[1]);
                }
                (false, true) => {
                    let sidebar_chunks = Layout::default()
                        .direction(Direction::Vertical)
                        .constraints([
                            Constraint::Min(5),    // worktree list
                            Constraint::Length(5), // global usage
                        ])
                        .split(main_chunks[0]);
                    sidebar::draw(f, app, sidebar_chunks[0]);
                    sidebar::draw_global_usage(f, app, sidebar_chunks[1]);
                }
                (false, false) => {
                    sidebar::draw(f, app, main_chunks[0]);
                }
            }
        }

        if show_queue {
            let pane_chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Min(5),                           // terminal pane
                    Constraint::Length(app.queue_panel_height()), // prompt queue
                ])
                .split(main_chunks[1]);

            app.areas.terminal_pane.set(pane_chunks[0]);
            app.areas.prompt_queue.set(pane_chunks[1]);
            terminal_pane::draw(f, app, pane_chunks[0]);
            prompt_queue::draw(f, app, pane_chunks[1]);
        } else {
            app.areas.terminal_pane.set(main_chunks[1]);
            terminal_pane::draw(f, app, main_chunks[1]);
        }
    } else if show_queue {
        let pane_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(5),
                Constraint::Length(app.queue_panel_height()),
            ])
            .split(chunks[0]);

        app.areas.terminal_pane.set(pane_chunks[0]);
        app.areas.prompt_queue.set(pane_chunks[1]);
        terminal_pane::draw(f, app, pane_chunks[0]);
        prompt_queue::draw(f, app, pane_chunks[1]);
    } else {
        app.areas.terminal_pane.set(chunks[0]);
        terminal_pane::draw(f, app, chunks[0]);
    }

    app.areas.status_bar.set(chunks[1]);
    status_bar::draw(f, app, chunks[1]);

    // Draw dialog on top if present
    if app.dialog.is_some() {
        dialogs::draw(f, app);
    }

    // Draw loading overlay on top of everything
    if let Some(ref msg) = app.loading_message {
        draw_loading_overlay(f, msg);
    }

    // Draw help overlay on top of everything
    if app.show_help {
        help::draw(f, app);
    }
}

pub(crate) fn draw_loading_overlay(f: &mut Frame, message: &str) {
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
        Span::styled(
            message,
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
    ]);
    f.render_widget(Paragraph::new(line), inner);
}

pub(crate) fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let x = area.x + area.width.saturating_sub(width) / 2;
    let y = area.y + area.height.saturating_sub(height) / 2;
    Rect::new(x, y, width.min(area.width), height.min(area.height))
}

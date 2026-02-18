use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::app::{App, Dialog};

pub fn draw(f: &mut Frame, app: &App) {
    let dialog = match &app.dialog {
        Some(d) => d,
        None => return,
    };

    match dialog {
        Dialog::CreateWorktree {
            branch_input,
            path_input,
            focused_field,
        } => draw_create_worktree(f, branch_input, path_input, *focused_field),
        Dialog::Confirm { message, .. } => draw_confirm(f, message),
    }
}

fn centered_rect(percent_x: u16, height: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length((r.height.saturating_sub(height)) / 2),
            Constraint::Length(height),
            Constraint::Min(0),
        ])
        .split(r);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}

fn draw_create_worktree(f: &mut Frame, branch: &str, path: &str, focused: usize) {
    let area = centered_rect(50, 8, f.area());
    f.render_widget(Clear, area);

    let block = Block::default()
        .title(" New Worktree ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow));

    let inner = block.inner(area);
    f.render_widget(block, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(inner);

    let branch_style = if focused == 0 {
        Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::White)
    };
    let path_style = if focused == 1 {
        Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::White)
    };

    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("Branch: ", Style::default().fg(Color::Gray)),
            Span::styled(
                format!("{}_", branch),
                branch_style,
            ),
        ])),
        chunks[1],
    );

    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("Path:   ", Style::default().fg(Color::Gray)),
            Span::styled(
                format!("{}{}",
                    path,
                    if focused == 1 { "_" } else { "" }
                ),
                path_style,
            ),
        ])),
        chunks[2],
    );

    f.render_widget(
        Paragraph::new("Enter: create  Esc: cancel  Tab: next field")
            .style(Style::default().fg(Color::DarkGray)),
        chunks[4],
    );
}

fn draw_confirm(f: &mut Frame, message: &str) {
    let area = centered_rect(50, 5, f.area());
    f.render_widget(Clear, area);

    let block = Block::default()
        .title(" Confirm ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Red));

    let inner = block.inner(area);
    f.render_widget(block, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(inner);

    f.render_widget(
        Paragraph::new(message).style(Style::default().fg(Color::White)),
        chunks[0],
    );

    f.render_widget(
        Paragraph::new("Enter: yes  Esc: no")
            .style(Style::default().fg(Color::DarkGray)),
        chunks[2],
    );
}

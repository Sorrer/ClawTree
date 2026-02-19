use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::app::App;

pub fn draw(f: &mut Frame, app: &App, area: Rect) {
    let has_regular_repo = app.regular_repo_path.is_some();

    let block = Block::default()
        .title(" Worktree Claude TUI ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));

    let inner = block.inner(area);
    f.render_widget(block, area);

    // Center the content vertically
    let content_height = if has_regular_repo { 14u16 } else { 12u16 };
    let v_pad = inner.height.saturating_sub(content_height) / 2;

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(v_pad),
            Constraint::Length(content_height),
            Constraint::Min(0),
        ])
        .split(inner);

    let lines_area = chunks[1];

    let dir_display = app.bare_repo_path.display().to_string();

    let header = if has_regular_repo {
        "Regular git repo detected"
    } else {
        "No git bare repo detected"
    };

    let mut lines = vec![
        Line::from(Span::styled(
            header,
            Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(vec![
            Span::styled("Directory: ", Style::default().fg(Color::Gray)),
            Span::styled(&dir_display, Style::default().fg(Color::White)),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            "This tool manages git worktrees with a .bare repo layout:",
            Style::default().fg(Color::Gray),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "  project/",
            Style::default().fg(Color::DarkGray),
        )),
        Line::from(Span::styled(
            "  +-- .bare/          (bare git repository)",
            Style::default().fg(Color::DarkGray),
        )),
        Line::from(Span::styled(
            "  +-- .git            (gitdir pointer to .bare)",
            Style::default().fg(Color::DarkGray),
        )),
        Line::from(Span::styled(
            "  +-- main/           (worktree for main branch)",
            Style::default().fg(Color::DarkGray),
        )),
        Line::from(""),
        Line::from(vec![
            Span::styled("Press ", Style::default().fg(Color::Gray)),
            Span::styled("i", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
            Span::styled(" to initialize a new bare repo workflow", Style::default().fg(Color::Gray)),
        ]),
    ];

    if has_regular_repo {
        lines.push(Line::from(""));
        lines.push(Line::from(vec![
            Span::styled("Press ", Style::default().fg(Color::Gray)),
            Span::styled("c", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)),
            Span::styled(" to convert existing repo to bare worktree layout", Style::default().fg(Color::Gray)),
        ]));
    }

    let paragraph = Paragraph::new(lines).alignment(Alignment::Center);
    f.render_widget(paragraph, lines_area);
}

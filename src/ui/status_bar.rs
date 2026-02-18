use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::{App, FocusTarget, InputMode};

pub fn draw(f: &mut Frame, app: &App, area: Rect) {
    let mode_text = match app.input_mode {
        InputMode::Normal => "NORMAL",
        InputMode::Terminal => "TERMINAL",
        InputMode::Dialog => "DIALOG",
    };

    let focus_text = match app.focus {
        FocusTarget::Sidebar => "Sidebar",
        FocusTarget::TerminalPane => "Terminal",
    };

    let session_text = match app.active_session_id {
        Some(sid) => app
            .sessions
            .get(&sid)
            .map(|s| s.label.clone())
            .unwrap_or_else(|| format!("session-{}", sid)),
        None => "no session".to_string(),
    };

    let help_text = match app.input_mode {
        InputMode::Normal => "Tab:focus j/k:nav Enter:select c:claude C:claude-yolo n:new-wt d:del ^b:sidebar ^q:quit",
        InputMode::Terminal => "Esc/Tab:sidebar ^b:sidebar ^q:quit",
        InputMode::Dialog => "Enter:confirm Esc:cancel Tab:next-field",
    };

    let status = if let Some(ref msg) = app.status_message {
        msg.as_str()
    } else {
        ""
    };

    let line = Line::from(vec![
        Span::styled(
            format!(" {} ", mode_text),
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled(focus_text, Style::default().fg(Color::White)),
        Span::styled(" | ", Style::default().fg(Color::DarkGray)),
        Span::styled(&session_text, Style::default().fg(Color::Green)),
        Span::styled(" | ", Style::default().fg(Color::DarkGray)),
        Span::styled(help_text, Style::default().fg(Color::DarkGray)),
        if !status.is_empty() {
            Span::styled(
                format!(" | {}", status),
                Style::default().fg(Color::Yellow),
            )
        } else {
            Span::raw("")
        },
    ]);

    let paragraph = Paragraph::new(line)
        .style(Style::default().bg(Color::Rgb(30, 30, 30)));
    f.render_widget(paragraph, area);
}

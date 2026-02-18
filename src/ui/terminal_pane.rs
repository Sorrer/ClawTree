use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::app::{App, FocusTarget};

pub fn draw(f: &mut Frame, app: &App, area: Rect) {
    let is_focused = app.focus == FocusTarget::TerminalPane;

    let border_style = if is_focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let title = match app.active_session_id {
        Some(sid) => {
            let session = app.sessions.get(&sid);
            let label = session.map(|s| s.label.as_str()).unwrap_or("???");
            let term_title = session.and_then(|s| s.terminal_title());
            match term_title {
                Some(t) => format!(" {} - {} ", label, t),
                None => format!(" {} ", label),
            }
        }
        None => " Terminal ".to_string(),
    };

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(border_style);

    if let Some(sid) = app.active_session_id {
        if let Some(session) = app.sessions.get(&sid) {
            match session.parser.try_read() {
                Ok(guard) => {
                    let screen = guard.screen();
                    let pseudo_term = tui_term::widget::PseudoTerminal::new(screen)
                        .block(block);
                    f.render_widget(pseudo_term, area);
                    return;
                }
                Err(_) => {
                    let placeholder =
                        Paragraph::new("Rendering...").block(block);
                    f.render_widget(placeholder, area);
                    return;
                }
            }
        }
    }

    let help = Paragraph::new("Select a worktree and press 'c' to start a Claude session")
        .style(Style::default().fg(Color::DarkGray))
        .block(block);
    f.render_widget(help, area);
}

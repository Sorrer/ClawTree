use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::app::App;
use super::theme;

pub fn draw(f: &mut Frame, app: &App, area: Rect) {
    let has_regular_repo = app.regular_repo_path.is_some();
    let auto_pending = app.auto_init_pending.is_some();

    let block = Block::default()
        .title(" clawtree ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::get().brand_claw));

    let inner = block.inner(area);
    f.render_widget(block, area);

    // Logo (3 lines) + blank + content
    // When auto-init is pending, we hide the manual key prompts (fewer lines)
    let content_height = if auto_pending {
        16u16
    } else if has_regular_repo {
        20u16
    } else {
        18u16
    };
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

    let header = if auto_pending {
        match app.auto_init_pending {
            Some(crate::app::AutoInitKind::Convert) => "Converting existing repo...",
            Some(crate::app::AutoInitKind::Init) => "Setting up new repository...",
            None => "No git bare repo detected",
        }
    } else if has_regular_repo {
        "Regular git repo detected"
    } else {
        "No git bare repo detected"
    };

    let claw_style = Style::default()
        .fg(theme::get().brand_claw)
        .add_modifier(Modifier::BOLD);
    let text_style = Style::default().fg(theme::get().brand_name);

    let mut lines: Vec<Line> = Vec::new();

    // ── Big ASCII logo ──────────────────────────────────────────
    // Each line has claw marks (green bold) + letter forms (darker green)
    for logo_line in theme::LOGO_BIG {
        // Split at the boundary between claw marks and text.
        // Claw marks are the leading `╱ ╱ ╱` portion (first 10 chars).
        let (claw_part, text_part) = if logo_line.len() > 10 {
            let boundary = logo_line
                .char_indices()
                .take_while(|(_, c)| *c == '╱' || *c == ' ')
                .last()
                .map(|(i, c)| i + c.len_utf8())
                .unwrap_or(0);
            (&logo_line[..boundary], &logo_line[boundary..])
        } else {
            (*logo_line, "")
        };

        lines.push(Line::from(vec![
            Span::styled(claw_part, claw_style),
            Span::styled(text_part, text_style),
        ]));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(""));

    // ── Info content ────────────────────────────────────────────
    lines.push(Line::from(Span::styled(
        header,
        Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled("Directory: ", Style::default().fg(Color::Gray)),
        Span::styled(&dir_display, Style::default().fg(Color::White)),
    ]));
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "This tool manages git worktrees with a .bare repo layout:",
        Style::default().fg(Color::Gray),
    )));
    lines.push(Line::from(""));
    let tree_lines = [
        "  project/",
        "  +-- .bare/          (bare git repository)",
        "  +-- .git            (gitdir pointer to .bare)",
        "  +-- main/           (worktree for main branch)",
    ];
    let max_w = tree_lines.iter().map(|l| l.len()).max().unwrap_or(0);
    for tl in &tree_lines {
        lines.push(Line::from(Span::styled(
            format!("{:<width$}", tl, width = max_w),
            Style::default().fg(Color::DarkGray),
        )));
    }

    // Only show manual key prompts when auto-init is NOT pending
    if !auto_pending {
        lines.push(Line::from(""));
        lines.push(Line::from(vec![
            Span::styled("Press ", Style::default().fg(Color::Gray)),
            Span::styled("i", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
            Span::styled(" to initialize a new bare repo workflow", Style::default().fg(Color::Gray)),
        ]));

        if has_regular_repo {
            lines.push(Line::from(""));
            lines.push(Line::from(vec![
                Span::styled("Press ", Style::default().fg(Color::Gray)),
                Span::styled("c", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)),
                Span::styled(" to convert existing repo to bare worktree layout", Style::default().fg(Color::Gray)),
            ]));
        }
    }

    let paragraph = Paragraph::new(lines).alignment(Alignment::Center);
    f.render_widget(paragraph, lines_area);
}

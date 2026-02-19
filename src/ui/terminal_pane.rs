use ansi_to_tui::IntoText as _;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::app::{App, FocusTarget};

pub fn draw(f: &mut Frame, app: &App, area: Rect) {
    let is_focused = app.focus == FocusTarget::TerminalPane;

    let border_style = if is_focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let mut title = match app.active_session_id {
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

    if app.terminal_scroll > 0 {
        title = format!("{}[+{}] ", title, app.terminal_scroll);
    }

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(border_style);

    if let Some(sid) = app.active_session_id {
        if let Some(session) = app.sessions.get(&sid) {
            // When scrolled and session is tmux-backed, capture history from tmux
            if app.terminal_scroll > 0 {
                if let Some(ref tmux_name) = session.tmux_session_name {
                    let inner = block.inner(area);
                    let visible_rows = inner.height as usize;

                    // Query tmux history size to clamp scroll
                    let history = tmux_history_size(tmux_name);
                    let effective_scroll = app.terminal_scroll.min(history);

                    if effective_scroll > 0 {
                        let start = -(effective_scroll as i64);
                        let end = start + visible_rows as i64 - 1;
                        if let Some(content) = capture_tmux_pane(tmux_name, start, end) {
                            // Render block, then clear inner area to remove PseudoTerminal artifacts
                            let inner_area = block.inner(area);
                            f.render_widget(block, area);
                            f.render_widget(Clear, inner_area);

                            // Parse ANSI escape sequences into styled ratatui text
                            let text = content.as_bytes().into_text().unwrap_or_default();
                            let lines: Vec<Line> = text.lines
                                .into_iter()
                                .take(visible_rows)
                                .collect();
                            let para = Paragraph::new(lines);
                            f.render_widget(para, inner_area);

                            // Draw scrollbar
                            let total = history + visible_rows;
                            let scrollbar_area = Rect {
                                x: inner_area.x + inner_area.width.saturating_sub(1),
                                y: inner_area.y,
                                width: 1,
                                height: inner_area.height,
                            };
                            draw_scrollbar(f, scrollbar_area, total, effective_scroll, visible_rows);
                            return;
                        }
                    }
                }
            }

            // Live view: render from vt100 parser
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

    // Priority 2: Worktree info panel
    if let Some(wi) = app.active_worktree_idx {
        if let Some(wt) = app.worktrees.get(wi) {
            let branch = &wt.branch;
            let block = Block::default()
                .title(format!(" {} ", branch))
                .borders(Borders::ALL)
                .border_style(border_style);
            let inner = block.inner(area);
            f.render_widget(block, area);
            draw_worktree_info(f, app, wi, inner);
            return;
        }
    }

    let help = Paragraph::new("Select a worktree and press 'c' to start a Claude session")
        .style(Style::default().fg(Color::DarkGray))
        .block(block);
    f.render_widget(help, area);
}

/// Draw the worktree info panel content.
fn draw_worktree_info(f: &mut Frame, app: &App, wi: usize, area: Rect) {
    let wt = match app.worktrees.get(wi) {
        Some(wt) => wt,
        None => return,
    };

    let focused = app.info_panel_focused();
    let (unstaged, staged) = app.info_panel_file_lists();
    let mut lines: Vec<Line> = Vec::new();

    // Branch and HEAD info
    lines.push(Line::from(vec![
        Span::styled("  Branch: ", Style::default().fg(Color::DarkGray)),
        Span::styled(&wt.branch, Style::default().fg(Color::Cyan)),
    ]));

    if let Some(ref status) = app.worktree_status {
        lines.push(Line::from(vec![
            Span::styled("  HEAD:   ", Style::default().fg(Color::DarkGray)),
            Span::styled(&wt.commit_hash, Style::default().fg(Color::Yellow)),
            Span::raw(" "),
            Span::raw(&status.head_subject),
        ]));
    } else {
        lines.push(Line::from(vec![
            Span::styled("  HEAD:   ", Style::default().fg(Color::DarkGray)),
            Span::styled(&wt.commit_hash, Style::default().fg(Color::Yellow)),
        ]));
    }

    lines.push(Line::raw(""));

    if let Some(ref status) = app.worktree_status {
        if status.files.is_empty() {
            lines.push(Line::styled(
                "  Working tree clean",
                Style::default().fg(Color::Green),
            ));
        } else {
            // Unstaged section
            let unstaged_header = format!("  ── Unstaged ({}) ", unstaged.len());
            let unstaged_header_style = if focused && app.info_panel_section == 0 {
                Style::default().fg(Color::White)
            } else {
                Style::default().fg(Color::DarkGray)
            };
            lines.push(Line::styled(unstaged_header, unstaged_header_style));

            if unstaged.is_empty() {
                lines.push(Line::styled("    (none)", Style::default().fg(Color::DarkGray)));
            } else {
                for (i, (status_char, path)) in unstaged.iter().enumerate() {
                    let is_selected = focused && app.info_panel_section == 0 && app.info_panel_cursor == i;
                    let color = file_status_color(*status_char);
                    let prefix = if is_selected { " > " } else { "   " };
                    let status_str = format!("{}{} ", prefix, status_char);
                    if is_selected {
                        lines.push(Line::from(vec![
                            Span::styled(status_str, Style::default().fg(Color::White).bg(Color::DarkGray)),
                            Span::styled(path.as_str(), Style::default().fg(Color::White).bg(Color::DarkGray)),
                        ]));
                    } else {
                        lines.push(Line::from(vec![
                            Span::styled(status_str, Style::default().fg(color)),
                            Span::styled(path.as_str(), Style::default().fg(color)),
                        ]));
                    }
                }
            }

            lines.push(Line::raw(""));

            // Staged section
            let staged_header = format!("  ── Staged ({}) ", staged.len());
            let staged_header_style = if focused && app.info_panel_section == 1 {
                Style::default().fg(Color::White)
            } else {
                Style::default().fg(Color::DarkGray)
            };
            lines.push(Line::styled(staged_header, staged_header_style));

            if staged.is_empty() {
                lines.push(Line::styled("    (none)", Style::default().fg(Color::DarkGray)));
            } else {
                for (i, (status_char, path)) in staged.iter().enumerate() {
                    let is_selected = focused && app.info_panel_section == 1 && app.info_panel_cursor == i;
                    let prefix = if is_selected { " > " } else { "   " };
                    let status_str = format!("{}{} ", prefix, status_char);
                    if is_selected {
                        lines.push(Line::from(vec![
                            Span::styled(status_str, Style::default().fg(Color::White).bg(Color::DarkGray)),
                            Span::styled(path.as_str(), Style::default().fg(Color::White).bg(Color::DarkGray)),
                        ]));
                    } else {
                        lines.push(Line::from(vec![
                            Span::styled(status_str, Style::default().fg(Color::Green)),
                            Span::styled(path.as_str(), Style::default().fg(Color::Green)),
                        ]));
                    }
                }
            }
        }

        lines.push(Line::raw(""));

        // Recent commits section
        if !status.recent_commits.is_empty() {
            lines.push(Line::styled(
                "  ── Recent Commits ",
                Style::default().fg(Color::DarkGray),
            ));
            for commit in &status.recent_commits {
                lines.push(Line::styled(
                    format!("  {}", commit),
                    Style::default().fg(Color::DarkGray),
                ));
            }
        }
    }

    // Commit message input (when active)
    if let Some(ref msg) = app.info_panel_commit_msg {
        lines.push(Line::raw(""));
        lines.push(Line::styled(
            "  ── Commit Message ──",
            Style::default().fg(Color::Yellow),
        ));
        lines.push(Line::from(vec![
            Span::styled("  > ", Style::default().fg(Color::Yellow)),
            Span::raw(msg.as_str()),
            Span::styled("_", Style::default().fg(Color::Yellow)),
        ]));
        lines.push(Line::styled(
            "  Enter: commit  Esc: cancel",
            Style::default().fg(Color::DarkGray),
        ));
    }

    // Keybinding hints at bottom
    let hints = if focused && app.info_panel_commit_msg.is_none() {
        Line::from(vec![
            Span::styled("  Space", Style::default().fg(Color::Cyan)),
            Span::styled(": stage/unstage  ", Style::default().fg(Color::DarkGray)),
            Span::styled("a", Style::default().fg(Color::Cyan)),
            Span::styled(": stage all  ", Style::default().fg(Color::DarkGray)),
            Span::styled("Enter", Style::default().fg(Color::Cyan)),
            Span::styled(": commit  ", Style::default().fg(Color::DarkGray)),
            Span::styled("Tab", Style::default().fg(Color::Cyan)),
            Span::styled(": switch section  ", Style::default().fg(Color::DarkGray)),
            Span::styled("Esc", Style::default().fg(Color::Cyan)),
            Span::styled(": back", Style::default().fg(Color::DarkGray)),
        ])
    } else if app.info_panel_commit_msg.is_none() {
        Line::from(vec![
            Span::styled("  Tab", Style::default().fg(Color::Cyan)),
            Span::styled(": focus files  ", Style::default().fg(Color::DarkGray)),
            Span::styled("c", Style::default().fg(Color::Cyan)),
            Span::styled(": claude  ", Style::default().fg(Color::DarkGray)),
            Span::styled("p", Style::default().fg(Color::Cyan)),
            Span::styled(": push  ", Style::default().fg(Color::DarkGray)),
            Span::styled("s", Style::default().fg(Color::Cyan)),
            Span::styled(": stage/commit  ", Style::default().fg(Color::DarkGray)),
            Span::styled("m", Style::default().fg(Color::Cyan)),
            Span::styled(": merge  ", Style::default().fg(Color::DarkGray)),
            Span::styled("n", Style::default().fg(Color::Cyan)),
            Span::styled(": new  ", Style::default().fg(Color::DarkGray)),
            Span::styled("d", Style::default().fg(Color::Cyan)),
            Span::styled(": delete", Style::default().fg(Color::DarkGray)),
        ])
    } else {
        Line::raw("") // hints already shown inline with commit message
    };

    // Place hints at the bottom of the area
    let content_len = lines.len();
    let available = area.height as usize;
    if available > content_len + 2 {
        for _ in 0..(available - content_len - 1) {
            lines.push(Line::raw(""));
        }
    } else {
        lines.push(Line::raw(""));
    }
    lines.push(hints);

    let paragraph = Paragraph::new(lines);
    f.render_widget(paragraph, area);
}

/// Color for a file status character.
fn file_status_color(status: char) -> Color {
    match status {
        '?' => Color::Blue,
        'M' => Color::Yellow,
        'D' => Color::Red,
        'A' => Color::Green,
        _ => Color::White,
    }
}

/// Capture tmux pane content for a range of lines. Returns None on failure.
fn capture_tmux_pane(tmux_name: &str, start: i64, end: i64) -> Option<String> {
    let output = std::process::Command::new("tmux")
        .args([
            "capture-pane", "-t", tmux_name,
            "-p", "-e",
            "-S", &start.to_string(),
            "-E", &end.to_string(),
        ])
        .output()
        .ok()?;

    if output.status.success() {
        Some(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
        None
    }
}

/// Query the number of lines in the tmux pane's scrollback history.
fn tmux_history_size(tmux_name: &str) -> usize {
    std::process::Command::new("tmux")
        .args(["display-message", "-t", tmux_name, "-p", "#{history_size}"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| {
            String::from_utf8_lossy(&o.stdout)
                .trim()
                .parse::<usize>()
                .ok()
        })
        .unwrap_or(0)
}

/// Draw a scrollbar in the given 1-column area.
fn draw_scrollbar(f: &mut Frame, area: Rect, total_lines: usize, scroll_offset: usize, visible: usize) {
    if area.height == 0 || total_lines == 0 {
        return;
    }

    let bar_h = area.height as usize;

    // Thumb size: proportional to visible / total, minimum 1
    let thumb_size = ((visible as f64 / total_lines as f64) * bar_h as f64)
        .ceil()
        .max(1.0) as usize;
    let thumb_size = thumb_size.min(bar_h);

    // Thumb position: 0 = bottom (live), max = top (oldest)
    // Invert so scroll_offset=0 puts thumb at bottom
    let max_offset = total_lines.saturating_sub(visible);
    let travel = bar_h.saturating_sub(thumb_size);
    let thumb_top = if max_offset > 0 {
        ((scroll_offset as f64 / max_offset as f64) * travel as f64).round() as usize
    } else {
        0
    };
    // Invert: high scroll = thumb at top
    let thumb_top_y = travel.saturating_sub(thumb_top);

    let buf = f.buffer_mut();
    for row in 0..bar_h {
        let y = area.y + row as u16;
        let x = area.x;
        if x < buf.area().right() && y < buf.area().bottom() {
            let in_thumb = row >= thumb_top_y && row < thumb_top_y + thumb_size;
            let cell = &mut buf[(x, y)];
            if in_thumb {
                cell.set_char('█');
                cell.set_fg(Color::DarkGray);
            } else {
                cell.set_char('│');
                cell.set_fg(Color::Rgb(40, 40, 40));
            }
        }
    }
}

use std::time::Instant;

use ansi_to_tui::IntoText as _;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use ratatui::Frame;

use super::theme;
use crate::app::{App, FocusTarget, TextSelection};
use crate::url;

pub fn draw(f: &mut Frame, app: &App, area: Rect) {
    let is_focused = app.focus == FocusTarget::TerminalPane;

    let border_style = if is_focused {
        Style::default()
            .fg(theme::get().border_focused_terminal)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme::BORDER_UNFOCUSED)
    };
    let border_type = if is_focused {
        BorderType::Thick
    } else {
        BorderType::Plain
    };

    let focus_indicator = if is_focused { " ▸" } else { "" };
    let mut title = match app.active_session_id {
        Some(sid) => {
            let session = app.sessions.get(&sid);
            let is_terminal = session.map(|s| s.is_terminal).unwrap_or(false);
            if is_terminal {
                // Plain terminal: name after running command / cwd (or nickname).
                let name = session
                    .and_then(|s| s.nickname.clone())
                    .unwrap_or_else(|| super::sidebar::terminal_display_name(session));
                format!("{} {} ", focus_indicator, name)
            } else {
                let term_title = session.and_then(|s| s.terminal_title());
                let label = session.map(|s| s.label.as_str()).unwrap_or("???");
                // Prefer terminal title (set by Claude); fall back to session label
                match term_title {
                    Some(t) => format!("{} {} ", focus_indicator, t),
                    None => format!("{} {} ", focus_indicator, label),
                }
            }
        }
        None => format!("{} Terminal ", focus_indicator),
    };

    if app.terminal_scroll > 0 {
        title = format!("{}[+{}] ", title, app.terminal_scroll);
    }

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_type(border_type)
        .border_style(border_style);

    // Record inner area for mouse hit-testing
    app.areas.terminal_pane_inner.set(block.inner(area));

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
                            let lines: Vec<Line> =
                                text.lines.into_iter().take(visible_rows).collect();
                            let para = Paragraph::new(lines);
                            f.render_widget(para, inner_area);

                            // Selection overlay for scrolled content
                            if let Some(sel) = app.text_selection.as_ref() {
                                let ((sr, sc), (er, ec)) = sel.ordered();
                                let buf = f.buffer_mut();
                                for row in sr..=er {
                                    if row >= inner_area.height {
                                        break;
                                    }
                                    let col_start = if row == sr { sc } else { 0 };
                                    let col_end = if row == er {
                                        ec
                                    } else {
                                        inner_area.width.saturating_sub(1)
                                    };
                                    for col in col_start..=col_end {
                                        if col >= inner_area.width {
                                            break;
                                        }
                                        let buf_x = inner_area.x + col;
                                        let buf_y = inner_area.y + row;
                                        if buf_x >= buf.area().right()
                                            || buf_y >= buf.area().bottom()
                                        {
                                            continue;
                                        }
                                        let buf_cell = &mut buf[(buf_x, buf_y)];
                                        let mut style = buf_cell.style();
                                        style = style.add_modifier(Modifier::REVERSED);
                                        buf_cell.set_style(style);
                                    }
                                }
                            }

                            // Draw scrollbar
                            let total = history + visible_rows;
                            let scrollbar_area = Rect {
                                x: inner_area.x + inner_area.width.saturating_sub(1),
                                y: inner_area.y,
                                width: 1,
                                height: inner_area.height,
                            };
                            draw_scrollbar(
                                f,
                                scrollbar_area,
                                total,
                                effective_scroll,
                                visible_rows,
                            );
                            return;
                        }
                    }
                }
            }

            // Live view: render from vt100 parser
            match session.parser.try_read() {
                Ok(guard) => {
                    let screen = guard.screen();
                    render_vt100_screen(
                        f,
                        screen,
                        block,
                        area,
                        &app.url_cache,
                        app.text_selection.as_ref(),
                        is_focused,
                    );
                    return;
                }
                Err(_) => {
                    let placeholder = Paragraph::new("Rendering...").block(block);
                    f.render_widget(placeholder, area);
                    return;
                }
            }
        }
    }

    // Priority 2: Project overview panel
    if app.project_overview_active {
        let project_name = app
            .bare_repo_path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "Project".to_string());
        let block = Block::default()
            .title(format!("{} {} ", focus_indicator, project_name))
            .borders(Borders::ALL)
            .border_type(border_type)
            .border_style(border_style);
        let inner = block.inner(area);
        f.render_widget(block, area);
        draw_project_overview(f, app, inner);
        return;
    }

    // Priority 3: Worktree info panel
    if let Some(wi) = app.active_worktree_idx {
        if let Some(wt) = app.worktrees.get(wi) {
            let branch = &wt.branch;
            let block = Block::default()
                .title(format!("{} {} ", focus_indicator, branch))
                .borders(Borders::ALL)
                .border_type(border_type)
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

/// Draw the project overview panel showing repo-wide stats and quick-start guide.
fn draw_project_overview(f: &mut Frame, app: &App, area: Rect) {
    use crate::keys::QUICK_START_KEYS;
    use crate::worktree::git;
    use std::sync::atomic::Ordering;

    let project_name = app
        .bare_repo_path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "Project".to_string());

    let label_style = Style::default().fg(Color::Gray);
    let value_style = Style::default().fg(Color::White);
    let header_style = Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD);
    let dim_style = Style::default().fg(Color::DarkGray);
    let section_style = Style::default()
        .fg(Color::Yellow)
        .add_modifier(Modifier::BOLD);
    let key_style = Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD);

    let mut lines: Vec<Line> = Vec::new();

    // Project name and path
    lines.push(Line::from(vec![
        Span::styled("◆ ", header_style),
        Span::styled(&project_name, header_style),
    ]));
    lines.push(Line::from(vec![
        Span::styled("  ", Style::default()),
        Span::styled(app.bare_repo_path.display().to_string(), dim_style),
    ]));
    lines.push(Line::from(""));

    // Compact stats line
    let wt_count = app.worktrees.len();
    let active_sessions = app
        .sessions
        .values()
        .filter(|s| !s.exited.load(Ordering::Relaxed))
        .count();
    let total_sessions = app.sessions.len();
    let mut stats_spans = vec![
        Span::styled("  ", Style::default()),
        Span::styled(format!("{}", wt_count), value_style),
        Span::styled(" worktrees  ", label_style),
        Span::styled(
            format!("{}/{}", active_sessions, total_sessions),
            value_style,
        ),
        Span::styled(" sessions", label_style),
    ];
    if let Some(url) = git::remote_url(&app.bare_repo_path) {
        stats_spans.push(Span::styled("  ", Style::default()));
        stats_spans.push(Span::styled(url, dim_style));
    }
    lines.push(Line::from(stats_spans));
    lines.push(Line::from(""));

    // Quick start guide from shared key registry
    lines.push(Line::from(Span::styled("  Quick Start", header_style)));
    lines.push(Line::from(""));

    let mut first_section = true;
    for entry in QUICK_START_KEYS {
        if entry.0.is_empty() {
            // Section header
            if !first_section {
                lines.push(Line::from(""));
            }
            first_section = false;
            lines.push(Line::from(vec![
                Span::styled("  ", Style::default()),
                Span::styled(entry.1.to_string(), section_style),
            ]));
        } else {
            // Key entry
            lines.push(Line::from(vec![
                Span::styled(format!("    {:16}", entry.0), key_style),
                Span::styled(entry.1.to_string(), label_style),
            ]));
        }
    }

    let paragraph = Paragraph::new(lines);
    f.render_widget(paragraph, area);
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

    // Show next refresh countdown
    let refresh_text = match app.next_status_refresh {
        Some(next) => {
            let now = Instant::now();
            if next > now {
                let secs = (next - now).as_secs();
                format!("  \u{27F3} Refresh in {}s", secs)
            } else {
                "  \u{27F3} Refreshing...".to_string()
            }
        }
        None => "  \u{27F3} Fetching...".to_string(),
    };
    lines.push(Line::styled(
        refresh_text,
        Style::default().fg(Color::DarkGray),
    ));

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
                lines.push(Line::styled(
                    "    (none)",
                    Style::default().fg(Color::DarkGray),
                ));
            } else {
                for (i, (status_char, path)) in unstaged.iter().enumerate() {
                    let is_selected =
                        focused && app.info_panel_section == 0 && app.info_panel_cursor == i;
                    let color = file_status_color(*status_char);
                    let prefix = if is_selected { " > " } else { "   " };
                    let status_str = format!("{}{} ", prefix, status_char);
                    if is_selected {
                        lines.push(Line::from(vec![
                            Span::styled(
                                status_str,
                                Style::default().fg(Color::White).bg(Color::DarkGray),
                            ),
                            Span::styled(
                                path.as_str(),
                                Style::default().fg(Color::White).bg(Color::DarkGray),
                            ),
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
                lines.push(Line::styled(
                    "    (none)",
                    Style::default().fg(Color::DarkGray),
                ));
            } else {
                for (i, (status_char, path)) in staged.iter().enumerate() {
                    let is_selected =
                        focused && app.info_panel_section == 1 && app.info_panel_cursor == i;
                    let prefix = if is_selected { " > " } else { "   " };
                    let status_str = format!("{}{} ", prefix, status_char);
                    if is_selected {
                        lines.push(Line::from(vec![
                            Span::styled(
                                status_str,
                                Style::default().fg(Color::White).bg(Color::DarkGray),
                            ),
                            Span::styled(
                                path.as_str(),
                                Style::default().fg(Color::White).bg(Color::DarkGray),
                            ),
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

        // Unpushed commits section
        if !status.unpushed_commits.is_empty() {
            lines.push(Line::styled(
                format!("  ── Unpushed Commits ({}) ", status.unpushed_commits.len()),
                Style::default().fg(Color::Yellow),
            ));
            for commit in &status.unpushed_commits {
                lines.push(Line::styled(
                    format!("  {}", commit),
                    Style::default().fg(Color::Yellow),
                ));
            }
            lines.push(Line::raw(""));
        }

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

    // Keybinding hints at bottom
    let hints = if focused {
        Line::from(vec![
            Span::styled("  j/k", Style::default().fg(Color::Cyan)),
            Span::styled(": navigate  ", Style::default().fg(Color::DarkGray)),
            Span::styled("Tab", Style::default().fg(Color::Cyan)),
            Span::styled(": section  ", Style::default().fg(Color::DarkGray)),
            Span::styled("s", Style::default().fg(Color::Cyan)),
            Span::styled(": stage/commit  ", Style::default().fg(Color::DarkGray)),
            Span::styled("^s", Style::default().fg(Color::Cyan)),
            Span::styled(": AI commit  ", Style::default().fg(Color::DarkGray)),
            Span::styled("p", Style::default().fg(Color::Cyan)),
            Span::styled(": push  ", Style::default().fg(Color::DarkGray)),
            Span::styled("P", Style::default().fg(Color::Cyan)),
            Span::styled(": pull  ", Style::default().fg(Color::DarkGray)),
            Span::styled("Esc", Style::default().fg(Color::Cyan)),
            Span::styled(": back", Style::default().fg(Color::DarkGray)),
        ])
    } else {
        Line::from(vec![
            Span::styled("  c", Style::default().fg(Color::Cyan)),
            Span::styled(": claude  ", Style::default().fg(Color::DarkGray)),
            Span::styled("p", Style::default().fg(Color::Cyan)),
            Span::styled(": push  ", Style::default().fg(Color::DarkGray)),
            Span::styled("P", Style::default().fg(Color::Cyan)),
            Span::styled(": pull  ", Style::default().fg(Color::DarkGray)),
            Span::styled("s", Style::default().fg(Color::Cyan)),
            Span::styled(": stage/commit  ", Style::default().fg(Color::DarkGray)),
            Span::styled("^s", Style::default().fg(Color::Cyan)),
            Span::styled(": AI commit  ", Style::default().fg(Color::DarkGray)),
            Span::styled("m", Style::default().fg(Color::Cyan)),
            Span::styled(": merge  ", Style::default().fg(Color::DarkGray)),
            Span::styled("n", Style::default().fg(Color::Cyan)),
            Span::styled(": new  ", Style::default().fg(Color::DarkGray)),
            Span::styled("d", Style::default().fg(Color::Cyan)),
            Span::styled(": delete", Style::default().fg(Color::DarkGray)),
        ])
    };

    // Split area: content on top, hints pinned at bottom
    let content_height = area.height.saturating_sub(1);
    let content_area = Rect {
        height: content_height,
        ..area
    };
    let hints_area = Rect {
        y: area.y + content_height,
        height: 1,
        ..area
    };

    // Render scrollable content
    let total_lines = lines.len();
    let visible = content_area.height as usize;
    // Clamp scroll so we don't scroll past the content
    let max_scroll = total_lines.saturating_sub(visible);
    let scroll = app.info_panel_scroll.min(max_scroll);

    let paragraph = Paragraph::new(lines).scroll((scroll as u16, 0));
    f.render_widget(paragraph, content_area);

    // Render hints pinned at bottom
    let hints_paragraph = Paragraph::new(hints);
    f.render_widget(hints_paragraph, hints_area);
}

/// Color for a file status character.
fn file_status_color(status: char) -> Color {
    match status {
        '?' => theme::FILE_UNTRACKED,
        'M' => theme::FILE_MODIFIED,
        'D' => theme::FILE_DELETED,
        'A' => theme::FILE_ADDED,
        _ => theme::FILE_DEFAULT,
    }
}

/// Capture tmux pane content for a range of lines (with ANSI escapes). Returns None on failure.
fn capture_tmux_pane(tmux_name: &str, start: i64, end: i64) -> Option<String> {
    let output = std::process::Command::new("tmux")
        .args([
            "capture-pane",
            "-t",
            tmux_name,
            "-p",
            "-e",
            "-S",
            &start.to_string(),
            "-E",
            &end.to_string(),
        ])
        .output()
        .ok()?;

    if output.status.success() {
        Some(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
        None
    }
}

/// Capture tmux pane content as plain text (no ANSI escapes).
pub fn capture_tmux_pane_plain(tmux_name: &str, start: i64, end: i64) -> Option<String> {
    let output = std::process::Command::new("tmux")
        .args([
            "capture-pane",
            "-t",
            tmux_name,
            "-p",
            "-S",
            &start.to_string(),
            "-E",
            &end.to_string(),
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
pub fn tmux_history_size(tmux_name: &str) -> usize {
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
fn draw_scrollbar(
    f: &mut Frame,
    area: Rect,
    total_lines: usize,
    scroll_offset: usize,
    visible: usize,
) {
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
                cell.set_fg(theme::SCROLLBAR_THUMB);
            } else {
                cell.set_char('│');
                cell.set_fg(theme::get().scrollbar_track);
            }
        }
    }
}

/// Render a vt100 screen into a ratatui frame with full attribute support.
///
/// This replaces `tui_term::widget::PseudoTerminal` which drops the `dim`
/// attribute (SGR 2).  Claude Code uses dim for placeholder/hint text, so
/// without this the placeholders render at full intensity — the same color
/// as regular typed text.
fn render_vt100_screen(
    f: &mut Frame,
    screen: &vt100::Screen,
    block: Block,
    area: Rect,
    url_cache: &url::UrlCache,
    selection: Option<&TextSelection>,
    is_focused: bool,
) {
    let inner = block.inner(area);
    f.render_widget(block, area);

    let (screen_rows, screen_cols) = screen.size();
    let buf = f.buffer_mut();

    for row in 0..inner.height.min(screen_rows) {
        for col in 0..inner.width.min(screen_cols) {
            if let Some(cell) = screen.cell(row, col) {
                // Skip the trailing half of wide (CJK) characters
                if cell.is_wide_continuation() {
                    continue;
                }

                let buf_x = inner.x + col;
                let buf_y = inner.y + row;
                if buf_x >= buf.area().right() || buf_y >= buf.area().bottom() {
                    continue;
                }

                let buf_cell = &mut buf[(buf_x, buf_y)];

                if cell.has_contents() {
                    buf_cell.set_symbol(cell.contents());
                }

                let fg = convert_vt100_color(cell.fgcolor());
                let bg = convert_vt100_color(cell.bgcolor());

                let mut style = Style::reset();
                if cell.bold() {
                    style = style.add_modifier(Modifier::BOLD);
                }
                if cell.dim() {
                    style = style.add_modifier(Modifier::DIM);
                }
                if cell.italic() {
                    style = style.add_modifier(Modifier::ITALIC);
                }
                if cell.underline() {
                    style = style.add_modifier(Modifier::UNDERLINED);
                }
                if cell.inverse() {
                    style = style.add_modifier(Modifier::REVERSED);
                }

                buf_cell.set_style(style);
                buf_cell.set_fg(fg);
                buf_cell.set_bg(bg);
            }
        }
    }

    // Overlay URL highlighting on detected URLs
    for (i, detected) in url_cache.urls.iter().enumerate() {
        let is_hovered = url_cache.hovered == Some(i);
        for span in &detected.spans {
            let row = span.row;
            if row >= inner.height.min(screen_rows) {
                continue;
            }
            for col in span.col_start..span.col_end {
                if col >= inner.width.min(screen_cols) {
                    break;
                }
                let buf_x = inner.x + col;
                let buf_y = inner.y + row;
                if buf_x >= buf.area().right() || buf_y >= buf.area().bottom() {
                    continue;
                }
                let buf_cell = &mut buf[(buf_x, buf_y)];
                let mut style = buf_cell.style();
                style = style.add_modifier(Modifier::UNDERLINED);
                if is_hovered {
                    style = style.fg(Color::Cyan).add_modifier(Modifier::BOLD);
                }
                buf_cell.set_style(style);
            }
        }
    }

    // Overlay selection highlighting (inverse video)
    if let Some(sel) = selection {
        let ((sr, sc), (er, ec)) = sel.ordered();
        for row in sr..=er {
            if row >= inner.height.min(screen_rows) {
                break;
            }
            let col_start = if row == sr { sc } else { 0 };
            let col_end = if row == er {
                ec
            } else {
                inner.width.min(screen_cols).saturating_sub(1)
            };
            for col in col_start..=col_end {
                if col >= inner.width.min(screen_cols) {
                    break;
                }
                let buf_x = inner.x + col;
                let buf_y = inner.y + row;
                if buf_x >= buf.area().right() || buf_y >= buf.area().bottom() {
                    continue;
                }
                let buf_cell = &mut buf[(buf_x, buf_y)];
                let mut style = buf_cell.style();
                style = style.add_modifier(Modifier::REVERSED);
                buf_cell.set_style(style);
            }
        }
    }

    // Position the hardware cursor at the embedded terminal's cursor location.
    // ratatui hides the cursor every frame unless `set_cursor_position` is
    // called during draw, so without this the cursor is invisible inside the
    // pane (e.g. typing into Claude Code via tmux). Only show it when the pane
    // is focused and the program hasn't hidden the cursor itself.
    if is_focused && !screen.hide_cursor() {
        let (cur_row, cur_col) = screen.cursor_position();
        if cur_row < inner.height.min(screen_rows) && cur_col < inner.width.min(screen_cols) {
            f.set_cursor_position((inner.x + cur_col, inner.y + cur_row));
        }
    }
}

/// Convert a vt100 color to a ratatui color.
fn convert_vt100_color(color: vt100::Color) -> Color {
    match color {
        vt100::Color::Default => Color::Reset,
        vt100::Color::Idx(i) => Color::Indexed(i),
        vt100::Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
    }
}

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, List, ListItem};
use std::sync::atomic::Ordering;

use crate::app::{App, FocusTarget, SidebarItem};
use super::theme;

/// Get the current spinner character based on the app's frame counter.
fn spinner_char(app: &App) -> char {
    // spinner_frame increments every tick (~33ms). Advance spinner every 3rd tick for ~10fps.
    let idx = (app.spinner_frame / 3) % theme::SPINNER_FRAMES.len();
    theme::SPINNER_FRAMES[idx]
}

pub fn draw(f: &mut Frame, app: &App, area: Rect) {
    let is_focused = app.focus == FocusTarget::Sidebar;

    let border_style = if is_focused {
        Style::default().fg(theme::BORDER_FOCUSED).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme::BORDER_UNFOCUSED)
    };

    let border_type = if is_focused { BorderType::Thick } else { BorderType::Plain };

    let title = if is_focused { " ▸ Worktrees " } else { " Worktrees " };
    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_type(border_type)
        .border_style(border_style);

    let inner_width = area.width.saturating_sub(2) as usize;

    let items: Vec<ListItem> = app
        .sidebar_items
        .iter()
        .enumerate()
        .map(|(idx, item)| {
            let is_selected = idx == app.sidebar_selected;
            match item {
                SidebarItem::Worktree(wi) => {
                    render_worktree(app, *wi, is_selected, inner_width)
                }
                SidebarItem::Session(wi, si) => {
                    render_session(app, *wi, *si, is_selected, inner_width)
                }
            }
        })
        .collect();

    let list = List::new(items).block(block);
    f.render_widget(list, area);
}

fn render_worktree(app: &App, wi: usize, is_selected: bool, inner_width: usize) -> ListItem<'static> {
    let wt = &app.worktrees[wi];
    let icon = if wt.expanded { "▼" } else { "▶" };

    let total = wt.session_ids.len();
    let alive = wt.session_ids.iter().filter(|sid| {
        app.sessions.get(sid)
            .map(|s| !s.exited.load(Ordering::SeqCst))
            .unwrap_or(false)
    }).count();
    let working = wt.session_ids.iter().filter(|sid| {
        app.sessions.get(sid)
            .map(|s| s.is_active())
            .unwrap_or(false)
    }).count();

    let bg = if is_selected { theme::SIDEBAR_SEL_BG } else { Color::Reset };
    let bold = if is_selected { Modifier::BOLD } else { Modifier::empty() };

    let branch_style = Style::default()
        .fg(Color::White)
        .bg(bg)
        .add_modifier(bold);

    let mut spans = vec![
        Span::styled(format!("{} {}", icon, wt.branch), branch_style),
    ];

    if total > 0 {
        // Alive (non-exited) count in gray — how many instances spawned
        spans.push(Span::styled(
            format!(" {}", alive),
            Style::default().fg(Color::DarkGray).bg(bg),
        ));

        if working > 0 {
            // Actively processing count in yellow
            spans.push(Span::styled(
                format!(" {}", working),
                Style::default().fg(Color::Yellow).bg(bg),
            ));
        }
    }

    // Pad to full width so the highlight covers the whole row
    let text_len: usize = spans.iter().map(|s| s.content.len()).sum();
    if is_selected && text_len < inner_width {
        spans.push(Span::styled(
            " ".repeat(inner_width - text_len),
            Style::default().bg(bg),
        ));
    }

    ListItem::new(Line::from(spans))
}

fn render_session(app: &App, wi: usize, si: usize, is_selected: bool, inner_width: usize) -> ListItem<'static> {
    let wt = &app.worktrees[wi];
    let sid = wt.session_ids[si];
    let session = app.sessions.get(&sid);
    let is_active_session = app.active_session_id == Some(sid);
    let is_exited = session
        .map(|s| s.exited.load(Ordering::SeqCst))
        .unwrap_or(true);
    let is_working = session
        .map(|s| s.is_active())
        .unwrap_or(false);
    let nickname = session.and_then(|s| s.nickname.clone());
    let title = session.and_then(|s| s.terminal_title());

    // Prefer nickname, then terminal title (stripped of Claude status chars), then label.
    let display_name = nickname.unwrap_or_else(|| {
        title
            .unwrap_or_else(|| {
                session
                    .map(|s| s.label.clone())
                    .unwrap_or_else(|| "???".to_string())
            })
            .trim_start_matches('✳')
            .trim_start_matches('⠂')
            .trim_start_matches('⠐')
            .trim_start()
            .to_string()
    });

    // Status indicator — only reflects working state, not active selection
    let status_icon: String = if is_exited {
        "✗".to_string()
    } else if is_working {
        spinner_char(app).to_string()
    } else {
        "○".to_string()
    };

    // Status color — only reflects working state
    let fg = if is_exited {
        Color::DarkGray
    } else if is_working {
        Color::Yellow
    } else {
        Color::Gray
    };

    // Background: selection highlight and active session highlight are independent
    let has_bg = is_selected || is_active_session;
    let bg = match (is_selected, is_active_session) {
        (true, true) => theme::SIDEBAR_SEL_ACTIVE_BG,
        (true, false) => theme::SIDEBAR_SEL_BG,
        (false, true) => theme::SIDEBAR_ACTIVE_BG,
        (false, false) => Color::Reset,
    };
    let bold = if is_selected { Modifier::BOLD } else { Modifier::empty() };

    // When highlighted, bump dim text to lighter so it's readable
    let sel_fg = if has_bg {
        match fg {
            Color::DarkGray => Color::Gray,
            Color::Gray => Color::White,
            other => other,
        }
    } else {
        fg
    };

    // Truncate to fit sidebar width
    let prefix = format!("  {} ", status_icon);
    let max_name = inner_width.saturating_sub(prefix.len());
    let truncated = if display_name.len() > max_name && max_name > 1 {
        format!("{}…", &display_name[..max_name.saturating_sub(1)])
    } else {
        display_name
    };

    let text = format!("{}{}", prefix, truncated);

    let mut spans = vec![
        Span::styled(text, Style::default().fg(sel_fg).bg(bg).add_modifier(bold)),
    ];

    // Pad to full width so the highlight covers the whole row
    let text_len: usize = spans.iter().map(|s| s.content.len()).sum();
    if has_bg && text_len < inner_width {
        spans.push(Span::styled(
            " ".repeat(inner_width - text_len),
            Style::default().bg(bg),
        ));
    }

    ListItem::new(Line::from(spans))
}

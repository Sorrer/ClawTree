use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem};
use std::sync::atomic::Ordering;

use crate::app::{App, FocusTarget, SidebarItem};

pub fn draw(f: &mut Frame, app: &App, area: Rect) {
    let is_focused = app.focus == FocusTarget::Sidebar;

    let border_style = if is_focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let block = Block::default()
        .title(" Worktrees ")
        .borders(Borders::ALL)
        .border_style(border_style);

    // Available width inside the block (minus borders)
    let inner_width = area.width.saturating_sub(2) as usize;

    let items: Vec<ListItem> = app
        .sidebar_items
        .iter()
        .enumerate()
        .map(|(idx, item)| {
            let is_selected = idx == app.sidebar_selected;
            match item {
                SidebarItem::Worktree(wi) => {
                    let wt = &app.worktrees[*wi];
                    let icon = if wt.expanded { "▼" } else { "▶" };
                    let session_count = if wt.session_ids.is_empty() {
                        String::new()
                    } else {
                        format!(" ({})", wt.session_ids.len())
                    };
                    let text = format!("{} {}{}", icon, wt.branch, session_count);
                    let style = if is_selected {
                        Style::default()
                            .fg(Color::White)
                            .bg(Color::DarkGray)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(Color::White)
                    };
                    ListItem::new(Line::from(Span::styled(text, style)))
                }
                SidebarItem::Session(wi, si) => {
                    let wt = &app.worktrees[*wi];
                    let sid = wt.session_ids[*si];
                    let session = app.sessions.get(&sid);
                    let label = session
                        .map(|s| s.label.as_str())
                        .unwrap_or("???");
                    let is_active_session = app.active_session_id == Some(sid);
                    let is_exited = session
                        .map(|s| s.exited.load(Ordering::Relaxed))
                        .unwrap_or(true);
                    let is_working = session
                        .map(|s| s.is_active())
                        .unwrap_or(false);
                    let title = session.and_then(|s| s.terminal_title());

                    // Status indicator
                    let status_icon = if is_exited {
                        "✗"
                    } else if is_working {
                        "⟳"
                    } else if is_active_session {
                        "●"
                    } else {
                        "○"
                    };

                    // Color
                    let color = if is_exited {
                        Color::DarkGray
                    } else if is_working {
                        Color::Yellow
                    } else if is_active_session {
                        Color::Green
                    } else {
                        Color::Gray
                    };

                    let bg = if is_selected {
                        Color::DarkGray
                    } else {
                        Color::Reset
                    };

                    let bold = if is_selected {
                        Modifier::BOLD
                    } else {
                        Modifier::empty()
                    };

                    // Build the line: "  ● label"  then title underneath if present
                    let header = format!("  {} {}", status_icon, label);

                    if let Some(ref title_str) = title {
                        // Truncate title to fit
                        let max_title = inner_width.saturating_sub(4);
                        let truncated = if title_str.len() > max_title {
                            format!("{}…", &title_str[..max_title.saturating_sub(1)])
                        } else {
                            title_str.clone()
                        };

                        let lines = vec![
                            Line::from(Span::styled(
                                header,
                                Style::default().fg(color).bg(bg).add_modifier(bold),
                            )),
                            Line::from(Span::styled(
                                format!("    {}", truncated),
                                Style::default().fg(Color::Rgb(120, 120, 120)).bg(bg),
                            )),
                        ];
                        ListItem::new(lines)
                    } else {
                        ListItem::new(Line::from(Span::styled(
                            header,
                            Style::default().fg(color).bg(bg).add_modifier(bold),
                        )))
                    }
                }
            }
        })
        .collect();

    let list = List::new(items).block(block);
    f.render_widget(list, area);
}

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, List, ListItem};
use std::sync::atomic::Ordering;

use crate::app::{AgentStatus, App, FocusTarget, SidebarItem, SidebarPanel};
use super::theme;

/// Get the current spinner character based on the app's frame counter.
fn spinner_char(app: &App) -> char {
    // spinner_frame increments every tick (~33ms). Advance spinner every 3rd tick for ~10fps.
    let idx = (app.spinner_frame / 3) % theme::SPINNER_FRAMES.len();
    theme::SPINNER_FRAMES[idx]
}

pub fn draw(f: &mut Frame, app: &App, area: Rect) {
    let is_focused = app.focus == FocusTarget::Sidebar;
    let in_worktrees_panel = app.sidebar_panel == SidebarPanel::Worktrees;
    let has_terminals = !app.terminal_ids.is_empty();

    let border_style = if is_focused {
        Style::default().fg(theme::BORDER_FOCUSED_SIDEBAR).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme::BORDER_UNFOCUSED)
    };

    let border_type = if is_focused { BorderType::Thick } else { BorderType::Plain };

    // Only show ▸ indicator when this sub-panel is active
    let title = if is_focused && in_worktrees_panel { " ▸ Worktrees " } else { " Worktrees " };

    // Remove bottom border when terminals panel is below (they share the edge)
    let borders = if has_terminals {
        Borders::TOP | Borders::LEFT | Borders::RIGHT
    } else {
        Borders::ALL
    };

    let block = Block::default()
        .title(title)
        .borders(borders)
        .border_type(border_type)
        .border_style(border_style);

    let inner_width = area.width.saturating_sub(2) as usize;

    let items: Vec<ListItem> = app
        .sidebar_items
        .iter()
        .enumerate()
        .map(|(idx, item)| {
            let is_selected = in_worktrees_panel && idx == app.sidebar_selected;
            let is_hovered = app.sidebar_hovered == Some(idx) && !is_selected;
            match item {
                SidebarItem::Worktree(wi) => {
                    render_worktree(app, *wi, is_selected, is_hovered, inner_width)
                }
                SidebarItem::Session(wi, si) => {
                    render_session(app, *wi, *si, is_selected, is_hovered, inner_width)
                }
                SidebarItem::Terminal(_) => {
                    // Terminals are rendered in the terminal panel, not here
                    ListItem::new(Line::from(vec![]))
                }
            }
        })
        .collect();

    // Record inner area for mouse hit-testing
    app.areas.sidebar_inner.set(block.inner(area));

    let list = List::new(items).block(block);
    f.render_widget(list, area);
}

fn render_worktree(app: &App, wi: usize, is_selected: bool, is_hovered: bool, inner_width: usize) -> ListItem<'static> {
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
    let needs_input = wt.session_ids.iter().filter(|sid| {
        app.sessions.get(sid)
            .map(|s| s.agent_status() == AgentStatus::NeedsInput)
            .unwrap_or(false)
    }).count();

    let has_bg = is_selected || is_hovered;
    let t = theme::get();
    let bg = if is_selected { t.sidebar_sel_bg } else if is_hovered { t.sidebar_hover_bg } else { Color::Reset };
    let bold = if is_selected { Modifier::BOLD } else { Modifier::empty() };

    let branch_style = Style::default()
        .fg(Color::White)
        .bg(bg)
        .add_modifier(bold);

    let mut spans = vec![
        Span::styled(format!("{} {}", icon, wt.branch), branch_style),
    ];

    // Show yellow asterisk if worktree has uncommitted or unpushed changes
    if let Some(status) = app.worktree_statuses.get(&wt.path) {
        if !status.files.is_empty() || !status.unpushed_commits.is_empty() {
            spans.push(Span::styled("*", Style::default().fg(Color::Yellow).bg(bg)));
        }
    }

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

        if needs_input > 0 {
            // Needs input count in blue
            spans.push(Span::styled(
                format!(" {}", needs_input),
                Style::default().fg(Color::Blue).bg(bg),
            ));
        }
    }

    // Pad to full width so the highlight covers the whole row
    let text_len: usize = spans.iter().map(|s| s.content.len()).sum();
    if has_bg && text_len < inner_width {
        // Reserve space for "+" button on hover
        let pad = inner_width - text_len;
        if is_hovered && pad >= 2 {
            spans.push(Span::styled(
                " ".repeat(pad - 2),
                Style::default().bg(bg),
            ));
            spans.push(Span::styled(
                " +",
                Style::default().fg(Color::Cyan).bg(bg).add_modifier(Modifier::BOLD),
            ));
        } else {
            spans.push(Span::styled(
                " ".repeat(pad),
                Style::default().bg(bg),
            ));
        }
    }

    ListItem::new(Line::from(spans))
}

fn render_session(app: &App, wi: usize, si: usize, is_selected: bool, is_hovered: bool, inner_width: usize) -> ListItem<'static> {
    let wt = &app.worktrees[wi];
    let sid = wt.session_ids[si];
    let session = app.sessions.get(&sid);
    let is_active_session = app.active_session_id == Some(sid);
    let is_terminal = session.map(|s| s.is_terminal).unwrap_or(false);

    // Background: selection highlight, active session highlight, and hover are independent
    let has_bg = is_selected || is_active_session || is_hovered;
    let t = theme::get();
    let bg = match (is_selected, is_active_session, is_hovered) {
        (true, true, _) => t.sidebar_sel_active_bg,
        (true, false, _) => t.sidebar_sel_bg,
        (false, true, _) => t.sidebar_active_bg,
        (false, false, true) => t.sidebar_hover_bg,
        (false, false, false) => Color::Reset,
    };
    let bold = if is_selected { Modifier::BOLD } else { Modifier::empty() };

    if is_terminal {
        return render_terminal_session(app, session, sid, has_bg, is_hovered, bg, bold, inner_width);
    }

    let status = session
        .map(|s| s.agent_status())
        .unwrap_or(AgentStatus::Exited);
    let in_plan_mode = session.map(|s| s.is_in_plan_mode()).unwrap_or(false);
    let nickname = session.and_then(|s| s.nickname.clone());
    let title = session.and_then(|s| s.terminal_title());

    // Prefer nickname, then terminal title (stripped of Claude status chars), then label.
    let base_name = nickname.unwrap_or_else(|| {
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

    // Prepend plan mode icon when Claude Code is in plan mode
    let display_name = if in_plan_mode {
        format!("{} {}", theme::PLAN_MODE_ICON, base_name)
    } else {
        base_name
    };

    // Status indicator and color based on agent status
    let (status_icon, fg): (String, Color) = match status {
        AgentStatus::Exited => ("✗".to_string(), Color::DarkGray),
        AgentStatus::Working => (spinner_char(app).to_string(), Color::Yellow),
        AgentStatus::NeedsInput => ("●".to_string(), theme::AGENT_NEEDS_INPUT),
        AgentStatus::Idle => ("○".to_string(), Color::Gray),
    };

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

    // Build context usage suffix if available (percentage display, greyed text)
    let usage_suffix = app.claude_usage.get(&sid).map(|u| {
        (format!(" {}%", u.usage_pct()), Color::DarkGray)
    });

    // Truncate to fit sidebar width (account for usage suffix)
    let prefix = format!("  {} ", status_icon);
    let suffix_len = usage_suffix.as_ref().map(|(s, _)| s.len()).unwrap_or(0);
    let max_name = inner_width.saturating_sub(prefix.len()).saturating_sub(suffix_len);
    let truncated = if display_name.len() > max_name && max_name > 1 {
        let mut end = max_name.saturating_sub(1);
        // Walk back to a valid char boundary
        while end > 0 && !display_name.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &display_name[..end])
    } else {
        display_name
    };

    let text = format!("{}{}", prefix, truncated);

    let mut spans = vec![
        Span::styled(text, Style::default().fg(sel_fg).bg(bg).add_modifier(bold)),
    ];

    // Append usage indicator
    if let Some((usage_text, usage_color)) = usage_suffix {
        spans.push(Span::styled(
            usage_text,
            Style::default().fg(usage_color).bg(bg),
        ));
    }

    // Pad to full width so the highlight covers the whole row
    let text_len: usize = spans.iter().map(|s| s.content.len()).sum();
    if has_bg && text_len < inner_width {
        let pad = inner_width - text_len;
        if is_hovered && pad >= 2 {
            spans.push(Span::styled(
                " ".repeat(pad - 2),
                Style::default().bg(bg),
            ));
            spans.push(Span::styled(
                " +",
                Style::default().fg(Color::Cyan).bg(bg).add_modifier(Modifier::BOLD),
            ));
        } else {
            spans.push(Span::styled(
                " ".repeat(pad),
                Style::default().bg(bg),
            ));
        }
    }

    ListItem::new(Line::from(spans))
}

/// Render a plain terminal session item with [terminal] tag and cwd as the name.
#[allow(clippy::too_many_arguments)]
fn render_terminal_session(
    _app: &App,
    session: Option<&crate::session::Session>,
    _sid: u64,
    has_bg: bool,
    is_hovered: bool,
    bg: Color,
    bold: Modifier,
    inner_width: usize,
) -> ListItem<'static> {
    let is_exited = session
        .map(|s| s.exited.load(Ordering::SeqCst))
        .unwrap_or(true);

    // Get current working directory from tmux pane
    let display_name = if is_exited {
        "[exited]".to_string()
    } else {
        session
            .and_then(|s| s.tmux_session_name.as_deref())
            .and_then(crate::session::pty::query_tmux_pane_cwd)
            .map(|cwd| {
                // Truncate to just the last directory component
                std::path::Path::new(&cwd)
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or(cwd)
            })
            .unwrap_or_else(|| {
                session
                    .map(|s| s.label.clone())
                    .unwrap_or_else(|| "terminal".to_string())
            })
    };

    let tag = "[terminal]";
    let tag_fg = if is_exited { Color::DarkGray } else { Color::Green };
    let name_fg = if has_bg {
        if is_exited { Color::Gray } else { Color::White }
    } else if is_exited { Color::DarkGray } else { Color::Gray };

    let prefix = format!("  {} ", tag);
    let max_name = inner_width.saturating_sub(prefix.len());
    let truncated = if display_name.len() > max_name && max_name > 1 {
        let mut end = max_name.saturating_sub(1);
        while end > 0 && !display_name.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &display_name[..end])
    } else {
        display_name
    };

    let mut spans = vec![
        Span::styled("  ", Style::default().bg(bg)),
        Span::styled(tag.to_string(), Style::default().fg(tag_fg).bg(bg).add_modifier(bold)),
        Span::styled(" ", Style::default().bg(bg)),
        Span::styled(truncated, Style::default().fg(name_fg).bg(bg).add_modifier(bold)),
    ];

    // Pad to full width so the highlight covers the whole row
    let text_len: usize = spans.iter().map(|s| s.content.len()).sum();
    if has_bg && text_len < inner_width {
        let pad = inner_width - text_len;
        if is_hovered && pad >= 2 {
            spans.push(Span::styled(
                " ".repeat(pad - 2),
                Style::default().bg(bg),
            ));
            spans.push(Span::styled(
                " +",
                Style::default().fg(Color::Cyan).bg(bg).add_modifier(Modifier::BOLD),
            ));
        } else {
            spans.push(Span::styled(
                " ".repeat(pad),
                Style::default().bg(bg),
            ));
        }
    }

    ListItem::new(Line::from(spans))
}

/// Draw the terminal panel below the worktree list.
pub fn draw_terminal_panel(f: &mut Frame, app: &App, area: Rect) {
    let is_focused = app.focus == FocusTarget::Sidebar;
    let in_terminal_panel = app.sidebar_panel == SidebarPanel::Terminals;

    // Use same border style as the Worktrees panel for visual consistency
    let border_style = if is_focused {
        Style::default().fg(theme::BORDER_FOCUSED_SIDEBAR).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme::BORDER_UNFOCUSED)
    };

    // Only show ▸ indicator when this sub-panel is active
    let title = if is_focused && in_terminal_panel { " ▸ Terminals " } else { " Terminals " };

    // Use T-junction corners to visually connect with the Worktrees panel above
    let mut border_set = if is_focused {
        symbols::border::THICK
    } else {
        symbols::border::PLAIN
    };
    border_set.top_left = if is_focused { "┣" } else { "├" };
    border_set.top_right = if is_focused { "┫" } else { "┤" };

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_set(border_set)
        .border_style(border_style);

    let inner_width = area.width.saturating_sub(2) as usize;

    let items: Vec<ListItem> = app
        .terminal_panel_items
        .iter()
        .enumerate()
        .map(|(idx, item)| {
            let is_selected = in_terminal_panel && idx == app.terminal_panel_selected;
            let is_hovered = app.terminal_panel_hovered == Some(idx) && !is_selected;
            match item {
                SidebarItem::Terminal(ti) => {
                    if let Some(&sid) = app.terminal_ids.get(*ti) {
                        let session = app.sessions.get(&sid);
                        let is_active_session = app.active_session_id == Some(sid);
                        let has_bg = is_selected || is_active_session || is_hovered;
                        let t = theme::get();
                        let bg = match (is_selected, is_active_session, is_hovered) {
                            (true, true, _) => t.sidebar_sel_active_bg,
                            (true, false, _) => t.sidebar_sel_bg,
                            (false, true, _) => t.sidebar_active_bg,
                            (false, false, true) => t.sidebar_hover_bg,
                            (false, false, false) => Color::Reset,
                        };
                        let bold = if is_selected { Modifier::BOLD } else { Modifier::empty() };
                        render_terminal_session(app, session, sid, has_bg, is_hovered, bg, bold, inner_width)
                    } else {
                        ListItem::new(Line::from(vec![]))
                    }
                }
                _ => ListItem::new(Line::from(vec![])),
            }
        })
        .collect();

    // Store areas for mouse hit-testing
    app.areas.sidebar_terminal_panel.set(area);
    let tp_inner = block.inner(area);
    app.areas.sidebar_terminal_panel_inner.set(tp_inner);

    let list = List::new(items).block(block);
    f.render_widget(list, area);
}

/// Draw the global usage panel below the sidebar worktree list.
pub fn draw_global_usage(f: &mut Frame, app: &App, area: Rect) {
    use ratatui::widgets::Paragraph;

    let usage = match &app.global_usage {
        Some(u) => u,
        None => return,
    };

    let block = Block::default()
        .title(" Usage ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::BORDER_UNFOCUSED));

    let inner = block.inner(area);
    f.render_widget(block, area);

    if inner.height < 2 || inner.width < 10 {
        return;
    }

    let bar_width: usize = 6;
    let inner_w = inner.width as usize;

    let five_hour_line = format_usage_line("5h", usage.five_hour_pct, &usage.five_hour_reset, bar_width, inner_w);
    let seven_day_line = format_usage_line("7d", usage.seven_day_pct, &usage.seven_day_reset, bar_width, inner_w);

    let mut lines = vec![five_hour_line];
    if inner.height >= 2 {
        lines.push(seven_day_line);
    }
    if inner.height >= 3 {
        if let (Some(pct), Some(ref reset)) = (usage.sonnet_7d_pct, &usage.sonnet_7d_reset) {
            lines.push(format_usage_line("SN", pct, reset, bar_width, inner_w));
        }
    }

    f.render_widget(Paragraph::new(lines), inner);
}

/// Format a single usage line with label, percentage, mini bar, and reset datetime.
fn format_usage_line(label: &str, pct: f64, reset_iso: &str, bar_width: usize, inner_w: usize) -> Line<'static> {
    let pct_clamped = pct.clamp(0.0, 100.0);
    let filled = ((pct_clamped / 100.0) * bar_width as f64).round() as usize;
    let empty = bar_width.saturating_sub(filled);

    let bar_filled: String = "\u{2588}".repeat(filled);
    let bar_empty: String = "\u{2591}".repeat(empty);

    let bar_color = if pct_clamped >= 80.0 {
        Color::Red
    } else if pct_clamped >= 50.0 {
        Color::Yellow
    } else {
        Color::Green
    };

    let reset_display = format_reset_datetime(reset_iso);
    let grey = Style::default().fg(Color::DarkGray);

    // Build the core spans, then conditionally append reset time if it fits
    let core_text_len = 1 + label.len() + 2 + 4 + 1 + bar_width; // " 5h: 45% ██████"
    let reset_fits = core_text_len + 1 + reset_display.len() <= inner_w;

    let mut spans = vec![
        Span::styled(format!(" {}: ", label), grey),
        Span::styled(format!("{:>2}%", pct_clamped as u32), grey),
        Span::raw(" "),
        Span::styled(bar_filled, Style::default().fg(bar_color)),
        Span::styled(bar_empty, Style::default().fg(Color::DarkGray)),
    ];

    if reset_fits {
        spans.push(Span::styled(format!(" {}", reset_display), grey));
    }

    Line::from(spans)
}

/// Format an ISO 8601 reset timestamp in the system's local timezone.
/// e.g., "2026-02-19T21:00:00+00:00" → "Feb 19 16:00 EST"
fn format_reset_datetime(iso: &str) -> String {
    use chrono::{DateTime, FixedOffset, Local};

    let utc: DateTime<FixedOffset> = match iso.parse() {
        Ok(dt) => dt,
        Err(_) => return "?".to_string(),
    };
    let local = utc.with_timezone(&Local);

    // chrono's %Z on Linux returns the offset ("-05:00") instead of the
    // abbreviation ("EST"). Use libc::localtime_r to get tm_zone instead.
    let epoch = local.timestamp() as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let tz = if !unsafe { libc::localtime_r(&epoch, &mut tm) }.is_null() && !tm.tm_zone.is_null() {
        unsafe { std::ffi::CStr::from_ptr(tm.tm_zone) }
            .to_str().unwrap_or("??").to_string()
    } else {
        local.format("%Z").to_string()
    };

    format!("{} {}", local.format("%b %-d %H:%M"), tz)
}


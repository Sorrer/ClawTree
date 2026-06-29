use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, List, ListItem, ListState};
use ratatui::Frame;
use std::sync::atomic::Ordering;

use super::theme;
use crate::app::{AgentStatus, App, FocusTarget, SidebarItem, SidebarPanel};

/// Get the current spinner character based on the app's frame counter.
fn spinner_char(app: &App) -> char {
    // spinner_frame increments every tick (~33ms). Advance spinner every 3rd tick for ~10fps.
    let idx = (app.spinner_frame / 3) % theme::SPINNER_FRAMES.len();
    theme::SPINNER_FRAMES[idx]
}

/// Display name for a plain terminal session: the command the user is actively
/// running (e.g. vim, node, deploy.sh) or the cwd folder name, falling back to
/// the session label.
///
/// Reads the cache populated off the render path by `spawn_terminal_name_poller`.
/// It must NOT shell out (`query_tmux_pane_*`) here — doing so per terminal per
/// frame blocks the render thread on tmux's single server thread and freezes the
/// UI on instances with many sessions.
pub(crate) fn terminal_display_name(app: &App, session: Option<&crate::session::Session>) -> String {
    match session {
        Some(s) => app
            .terminal_names
            .get(&s.id)
            .filter(|n| !n.is_empty())
            .cloned()
            .unwrap_or_else(|| s.label.clone()),
        None => "terminal".to_string(),
    }
}

pub fn draw(f: &mut Frame, app: &App, area: Rect) {
    let is_focused = app.focus == FocusTarget::Sidebar;
    let in_worktrees_panel = app.sidebar_panel == SidebarPanel::Worktrees;
    let has_terminals = !app.terminal_ids.is_empty();

    let border_style = if is_focused {
        Style::default()
            .fg(theme::BORDER_FOCUSED_SIDEBAR)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme::BORDER_UNFOCUSED)
    };

    let border_type = if is_focused {
        BorderType::Thick
    } else {
        BorderType::Plain
    };

    // Only show ▸ indicator when this sub-panel is active
    let title = if is_focused && in_worktrees_panel {
        " ▸ Worktrees "
    } else {
        " Worktrees "
    };

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
            let is_hovered = app.sidebar_hovered == Some(idx);
            match item {
                SidebarItem::Project => render_project(app, is_selected, is_hovered, inner_width),
                SidebarItem::Worktree(wi) => {
                    render_worktree(app, *wi, is_selected, is_hovered, inner_width)
                }
                SidebarItem::ProjectSession(si) => {
                    render_project_session(app, *si, is_selected, is_hovered, inner_width)
                }
                SidebarItem::Session(wi, si) => {
                    render_session(app, *wi, *si, is_selected, is_hovered, inner_width)
                }
                SidebarItem::Terminal(_) => {
                    // Terminals are rendered in the terminal panel, not here
                    ListItem::new(Line::from(vec![]))
                }
                SidebarItem::Location(li) => {
                    render_location(app, *li, is_selected, is_hovered, inner_width)
                }
                SidebarItem::LocationSession(li, si) => {
                    render_location_session(app, *li, *si, is_selected, is_hovered, inner_width)
                }
            }
        })
        .collect();

    // Record inner area for mouse hit-testing
    let inner = block.inner(area);
    app.areas.sidebar_inner.set(inner);

    let list = List::new(items).block(block);

    // Render as a stateful widget so the worktrees list scrolls once it grows
    // taller than the available area. We drive the viewport via the scroll offset
    // (not the selection) so the mouse wheel scrolls the list as a whole; keyboard
    // navigation keeps the selection visible by nudging this same offset. The
    // selection highlight is painted by the per-item render fns, so we don't set
    // `selected` here (doing so would make ratatui snap the offset back to the
    // cursor and fight free-scrolling). Clamp defensively against a stale offset.
    let max_offset = app
        .sidebar_items
        .len()
        .saturating_sub(inner.height as usize);
    let mut state = ListState::default().with_offset(app.sidebar_scroll.min(max_offset));
    f.render_stateful_widget(list, area, &mut state);
}

fn render_project(
    app: &App,
    is_selected: bool,
    is_hovered: bool,
    inner_width: usize,
) -> ListItem<'static> {
    let project_name = app
        .bare_repo_path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "Project".to_string());

    let has_bg = is_selected || is_hovered;
    let t = theme::get();
    let bg = if is_selected {
        t.sidebar_sel_bg
    } else if is_hovered {
        t.sidebar_hover_bg
    } else {
        Color::Reset
    };
    let bold = Modifier::BOLD;

    let label = format!("◆ {}", project_name);
    let mut spans = vec![Span::styled(
        label,
        Style::default().fg(t.brand_claw).bg(bg).add_modifier(bold),
    )];

    // Pad to full width
    let text_len: usize = spans.iter().map(|s| s.width()).sum();
    if has_bg && text_len < inner_width {
        let pad = inner_width - text_len;
        if is_hovered && pad >= 2 {
            spans.push(Span::styled(" ".repeat(pad - 2), Style::default().bg(bg)));
            spans.push(Span::styled(
                "+ ",
                Style::default()
                    .fg(Color::Cyan)
                    .bg(bg)
                    .add_modifier(Modifier::BOLD),
            ));
        } else {
            spans.push(Span::styled(" ".repeat(pad), Style::default().bg(bg)));
        }
    }

    ListItem::new(Line::from(spans))
}

fn render_project_session(
    app: &App,
    si: usize,
    is_selected: bool,
    is_hovered: bool,
    inner_width: usize,
) -> ListItem<'static> {
    let sid = app.project_session_ids[si];
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
    let bold = if is_selected {
        Modifier::BOLD
    } else {
        Modifier::empty()
    };

    let status = session
        .map(|s| s.agent_status())
        .unwrap_or(AgentStatus::Exited);
    let is_terminal = session.map(|s| s.is_terminal).unwrap_or(false);
    let in_plan_mode = session.map(|s| s.is_in_plan_mode()).unwrap_or(false);
    let nickname = session.and_then(|s| s.nickname.clone());
    let title = session.and_then(|s| s.terminal_title());

    // Terminals show a square icon and are named after their running command or
    // current directory; Claude sessions show a status dot and their title.
    let (display_name, status_icon, fg): (String, String, Color) = if is_terminal {
        let is_exited = session
            .map(|s| s.exited.load(Ordering::SeqCst))
            .unwrap_or(true);
        let name = nickname.unwrap_or_else(|| {
            if is_exited {
                "[exited]".to_string()
            } else {
                terminal_display_name(app, session)
            }
        });
        let color = if is_exited { Color::DarkGray } else { Color::Green };
        (name, "\u{25aa}".to_string(), color)
    } else {
        let name = nickname.unwrap_or_else(|| {
            title
                .unwrap_or_else(|| {
                    session
                        .map(|s| s.label.clone())
                        .unwrap_or_else(|| "???".to_string())
                })
                .trim_start_matches('\u{2733}')
                .trim_start_matches('\u{2802}')
                .trim_start_matches('\u{2810}')
                .trim_start()
                .to_string()
        });
        let (icon, color): (String, Color) = match status {
            AgentStatus::Exited => ("\u{2717}".to_string(), Color::DarkGray),
            AgentStatus::Working => (spinner_char(app).to_string(), Color::Yellow),
            AgentStatus::NeedsInput => ("\u{25cf}".to_string(), theme::AGENT_NEEDS_INPUT),
            AgentStatus::RateLimited => ("\u{2298}".to_string(), theme::AGENT_RATE_LIMITED),
            AgentStatus::Idle => ("\u{25cb}".to_string(), Color::Gray),
        };
        (name, icon, color)
    };

    let sel_fg = if has_bg {
        match fg {
            Color::DarkGray => Color::Gray,
            Color::Gray => Color::White,
            other => other,
        }
    } else {
        fg
    };

    let usage_suffix = app
        .claude_usage
        .get(&sid)
        .map(|u| (format!(" {}%", u.usage_pct()), Color::DarkGray));

    let prefix = format!("  {} ", status_icon);
    let suffix_len = usage_suffix.as_ref().map(|(s, _)| s.len()).unwrap_or(0);
    let max_name = inner_width
        .saturating_sub(prefix.len())
        .saturating_sub(suffix_len);
    let truncated = if display_name.len() > max_name && max_name > 1 {
        let mut end = max_name.saturating_sub(1);
        while end > 0 && !display_name.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &display_name[..end])
    } else {
        display_name
    };

    let name_fg = if in_plan_mode {
        theme::AGENT_PLANNING
    } else {
        sel_fg
    };

    let mut spans = vec![
        Span::styled(
            prefix,
            Style::default().fg(sel_fg).bg(bg).add_modifier(bold),
        ),
        Span::styled(
            truncated,
            Style::default().fg(name_fg).bg(bg).add_modifier(bold),
        ),
    ];

    if let Some((usage_text, usage_color)) = usage_suffix {
        spans.push(Span::styled(
            usage_text,
            Style::default().fg(usage_color).bg(bg),
        ));
    }

    let text_len: usize = spans.iter().map(|s| s.width()).sum();
    if has_bg && text_len < inner_width {
        let pad = inner_width - text_len;
        if is_hovered && pad >= 2 {
            spans.push(Span::styled(" ".repeat(pad - 2), Style::default().bg(bg)));
            spans.push(Span::styled(
                "+ ",
                Style::default()
                    .fg(Color::Cyan)
                    .bg(bg)
                    .add_modifier(Modifier::BOLD),
            ));
        } else {
            spans.push(Span::styled(" ".repeat(pad), Style::default().bg(bg)));
        }
    }

    ListItem::new(Line::from(spans))
}

fn render_worktree(
    app: &App,
    wi: usize,
    is_selected: bool,
    is_hovered: bool,
    inner_width: usize,
) -> ListItem<'static> {
    let wt = &app.worktrees[wi];
    let icon = if wt.expanded { "▼" } else { "▶" };

    let total = wt.session_ids.len();
    let alive = wt
        .session_ids
        .iter()
        .filter(|sid| {
            app.sessions
                .get(sid)
                .map(|s| !s.exited.load(Ordering::SeqCst))
                .unwrap_or(false)
        })
        .count();
    let working = wt
        .session_ids
        .iter()
        .filter(|sid| {
            app.sessions
                .get(sid)
                .map(|s| s.is_active())
                .unwrap_or(false)
        })
        .count();
    let needs_input = wt
        .session_ids
        .iter()
        .filter(|sid| {
            app.sessions
                .get(sid)
                .map(|s| s.agent_status() == AgentStatus::NeedsInput)
                .unwrap_or(false)
        })
        .count();
    let rate_limited = wt
        .session_ids
        .iter()
        .filter(|sid| {
            app.sessions
                .get(sid)
                .map(|s| s.is_rate_limited())
                .unwrap_or(false)
        })
        .count();

    let has_bg = is_selected || is_hovered;
    let t = theme::get();
    let bg = if is_selected {
        t.sidebar_sel_bg
    } else if is_hovered {
        t.sidebar_hover_bg
    } else {
        Color::Reset
    };
    let bold = if is_selected {
        Modifier::BOLD
    } else {
        Modifier::empty()
    };

    let branch_style = Style::default().fg(Color::White).bg(bg).add_modifier(bold);

    let mut spans = vec![Span::styled(
        format!("{} {}", icon, wt.branch),
        branch_style,
    )];

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

        if rate_limited > 0 {
            // Rate-limited count in red so a collapsed worktree still surfaces it
            spans.push(Span::styled(
                format!(" {}", rate_limited),
                Style::default()
                    .fg(theme::AGENT_RATE_LIMITED)
                    .bg(bg),
            ));
        }
    }

    // Pad to full width so the highlight covers the whole row
    let text_len: usize = spans.iter().map(|s| s.width()).sum();
    if has_bg && text_len < inner_width {
        // Reserve space for "+" button on hover
        let pad = inner_width - text_len;
        if is_hovered && pad >= 2 {
            spans.push(Span::styled(" ".repeat(pad - 2), Style::default().bg(bg)));
            spans.push(Span::styled(
                "+ ",
                Style::default()
                    .fg(Color::Cyan)
                    .bg(bg)
                    .add_modifier(Modifier::BOLD),
            ));
        } else {
            spans.push(Span::styled(" ".repeat(pad), Style::default().bg(bg)));
        }
    }

    ListItem::new(Line::from(spans))
}

fn render_session(
    app: &App,
    wi: usize,
    si: usize,
    is_selected: bool,
    is_hovered: bool,
    inner_width: usize,
) -> ListItem<'static> {
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
    let bold = if is_selected {
        Modifier::BOLD
    } else {
        Modifier::empty()
    };

    if is_terminal {
        return render_terminal_session(
            app,
            session,
            sid,
            has_bg,
            is_hovered,
            bg,
            bold,
            inner_width,
        );
    }

    let status = session
        .map(|s| s.agent_status())
        .unwrap_or(AgentStatus::Exited);
    let in_plan_mode = session.map(|s| s.is_in_plan_mode()).unwrap_or(false);
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

    // Status indicator and color based on agent status
    let (status_icon, fg): (String, Color) = match status {
        AgentStatus::Exited => ("✗".to_string(), Color::DarkGray),
        AgentStatus::Working => (spinner_char(app).to_string(), Color::Yellow),
        AgentStatus::NeedsInput => ("●".to_string(), theme::AGENT_NEEDS_INPUT),
        AgentStatus::RateLimited => ("⊘".to_string(), theme::AGENT_RATE_LIMITED),
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
    let usage_suffix = app
        .claude_usage
        .get(&sid)
        .map(|u| (format!(" {}%", u.usage_pct()), Color::DarkGray));

    // Truncate to fit sidebar width (account for usage suffix)
    let prefix = format!("  {} ", status_icon);
    let suffix_len = usage_suffix.as_ref().map(|(s, _)| s.len()).unwrap_or(0);
    let max_name = inner_width
        .saturating_sub(prefix.len())
        .saturating_sub(suffix_len);
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

    // Tint session name purple when in plan mode
    let name_fg = if in_plan_mode {
        theme::AGENT_PLANNING
    } else {
        sel_fg
    };

    let mut spans = vec![
        Span::styled(
            prefix,
            Style::default().fg(sel_fg).bg(bg).add_modifier(bold),
        ),
        Span::styled(
            truncated,
            Style::default().fg(name_fg).bg(bg).add_modifier(bold),
        ),
    ];

    // Append usage indicator
    if let Some((usage_text, usage_color)) = usage_suffix {
        spans.push(Span::styled(
            usage_text,
            Style::default().fg(usage_color).bg(bg),
        ));
    }

    // Pad to full width so the highlight covers the whole row
    let text_len: usize = spans.iter().map(|s| s.width()).sum();
    if has_bg && text_len < inner_width {
        let pad = inner_width - text_len;
        if is_hovered && pad >= 2 {
            spans.push(Span::styled(" ".repeat(pad - 2), Style::default().bg(bg)));
            spans.push(Span::styled(
                "+ ",
                Style::default()
                    .fg(Color::Cyan)
                    .bg(bg)
                    .add_modifier(Modifier::BOLD),
            ));
        } else {
            spans.push(Span::styled(" ".repeat(pad), Style::default().bg(bg)));
        }
    }

    ListItem::new(Line::from(spans))
}

/// Render a plain terminal session item with [terminal] tag and cwd as the name.
#[allow(clippy::too_many_arguments)]
fn render_terminal_session(
    app: &App,
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

    // Prefer a user-assigned nickname (set via rename); otherwise show the
    // pane's running command or current working directory.
    let display_name = if let Some(nick) = session.and_then(|s| s.nickname.clone()) {
        nick
    } else if is_exited {
        "[exited]".to_string()
    } else {
        terminal_display_name(app, session)
    };

    let tag = "[terminal]";
    let tag_fg = if is_exited {
        Color::DarkGray
    } else {
        Color::Green
    };
    let name_fg = if has_bg {
        if is_exited {
            Color::Gray
        } else {
            Color::White
        }
    } else if is_exited {
        Color::DarkGray
    } else {
        Color::Gray
    };

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
        Span::styled(
            tag.to_string(),
            Style::default().fg(tag_fg).bg(bg).add_modifier(bold),
        ),
        Span::styled(" ", Style::default().bg(bg)),
        Span::styled(
            truncated,
            Style::default().fg(name_fg).bg(bg).add_modifier(bold),
        ),
    ];

    // Pad to full width so the highlight covers the whole row
    let text_len: usize = spans.iter().map(|s| s.width()).sum();
    if has_bg && text_len < inner_width {
        let pad = inner_width - text_len;
        if is_hovered && pad >= 2 {
            spans.push(Span::styled(" ".repeat(pad - 2), Style::default().bg(bg)));
            spans.push(Span::styled(
                "+ ",
                Style::default()
                    .fg(Color::Cyan)
                    .bg(bg)
                    .add_modifier(Modifier::BOLD),
            ));
        } else {
            spans.push(Span::styled(" ".repeat(pad), Style::default().bg(bg)));
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
        Style::default()
            .fg(theme::BORDER_FOCUSED_SIDEBAR)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme::BORDER_UNFOCUSED)
    };

    // Only show ▸ indicator when this sub-panel is active
    let title = if is_focused && in_terminal_panel {
        " ▸ Terminals "
    } else {
        " Terminals "
    };

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
            let is_hovered = app.terminal_panel_hovered == Some(idx);
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
                        let bold = if is_selected {
                            Modifier::BOLD
                        } else {
                            Modifier::empty()
                        };
                        render_terminal_session(
                            app,
                            session,
                            sid,
                            has_bg,
                            is_hovered,
                            bg,
                            bold,
                            inner_width,
                        )
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

    let five_hour_line = format_usage_line(
        "5h",
        usage.five_hour_pct,
        &usage.five_hour_reset,
        bar_width,
        inner_w,
    );
    let seven_day_line = format_usage_line(
        "7d",
        usage.seven_day_pct,
        &usage.seven_day_reset,
        bar_width,
        inner_w,
    );

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
fn format_usage_line(
    label: &str,
    pct: f64,
    reset_iso: &str,
    bar_width: usize,
    inner_w: usize,
) -> Line<'static> {
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
            .to_str()
            .unwrap_or("??")
            .to_string()
    } else {
        local.format("%Z").to_string()
    };

    format!("{} {}", local.format("%b %-d %H:%M"), tz)
}

fn render_location(
    app: &App,
    li: usize,
    is_selected: bool,
    is_hovered: bool,
    inner_width: usize,
) -> ListItem<'static> {
    let loc = &app.locations[li];
    let icon = if loc.expanded { "▼" } else { "▶" };

    let display_name = loc.name.clone().unwrap_or_else(|| {
        loc.path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| loc.path.to_string_lossy().to_string())
    });

    let total = loc.session_ids.len();
    let alive = loc
        .session_ids
        .iter()
        .filter(|sid| {
            app.sessions
                .get(sid)
                .map(|s| !s.exited.load(Ordering::SeqCst))
                .unwrap_or(false)
        })
        .count();
    let working = loc
        .session_ids
        .iter()
        .filter(|sid| {
            app.sessions
                .get(sid)
                .map(|s| s.is_active())
                .unwrap_or(false)
        })
        .count();

    let has_bg = is_selected || is_hovered;
    let t = theme::get();
    let bg = if is_selected {
        t.sidebar_sel_bg
    } else if is_hovered {
        t.sidebar_hover_bg
    } else {
        Color::Reset
    };
    let bold = if is_selected {
        Modifier::BOLD
    } else {
        Modifier::empty()
    };

    let label_style = Style::default().fg(Color::Cyan).bg(bg).add_modifier(bold);

    let mut spans = vec![Span::styled(
        format!("{} ◇ {}", icon, display_name),
        label_style,
    )];

    if total > 0 {
        spans.push(Span::styled(
            format!(" {}", alive),
            Style::default().fg(Color::DarkGray).bg(bg),
        ));
        if working > 0 {
            spans.push(Span::styled(
                format!(" {}", working),
                Style::default().fg(Color::Yellow).bg(bg),
            ));
        }
    }

    let text_len: usize = spans.iter().map(|s| s.width()).sum();
    if has_bg && text_len < inner_width {
        let pad = inner_width - text_len;
        if is_hovered && pad >= 2 {
            spans.push(Span::styled(" ".repeat(pad - 2), Style::default().bg(bg)));
            spans.push(Span::styled(
                "+ ",
                Style::default()
                    .fg(Color::Cyan)
                    .bg(bg)
                    .add_modifier(Modifier::BOLD),
            ));
        } else {
            spans.push(Span::styled(" ".repeat(pad), Style::default().bg(bg)));
        }
    }

    ListItem::new(Line::from(spans))
}

fn render_location_session(
    app: &App,
    li: usize,
    si: usize,
    is_selected: bool,
    is_hovered: bool,
    inner_width: usize,
) -> ListItem<'static> {
    let loc = &app.locations[li];
    let sid = loc.session_ids[si];
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
    let bold = if is_selected {
        Modifier::BOLD
    } else {
        Modifier::empty()
    };

    let status = session
        .map(|s| s.agent_status())
        .unwrap_or(AgentStatus::Exited);
    let in_plan_mode = session.map(|s| s.is_in_plan_mode()).unwrap_or(false);
    let nickname = session.and_then(|s| s.nickname.clone());
    let title = session.and_then(|s| s.terminal_title());

    let display_name = nickname.unwrap_or_else(|| {
        title
            .unwrap_or_else(|| {
                session
                    .map(|s| s.label.clone())
                    .unwrap_or_else(|| "???".to_string())
            })
            .trim_start_matches('\u{2733}')
            .trim_start_matches('\u{2802}')
            .trim_start_matches('\u{2810}')
            .trim_start()
            .to_string()
    });

    let (status_icon, fg): (String, Color) = match status {
        AgentStatus::Exited => ("\u{2717}".to_string(), Color::DarkGray),
        AgentStatus::Working => (spinner_char(app).to_string(), Color::Yellow),
        AgentStatus::NeedsInput => ("\u{25cf}".to_string(), theme::AGENT_NEEDS_INPUT),
        AgentStatus::RateLimited => ("\u{2298}".to_string(), theme::AGENT_RATE_LIMITED),
        AgentStatus::Idle => ("\u{25cb}".to_string(), Color::Gray),
    };

    let sel_fg = if has_bg {
        match fg {
            Color::DarkGray => Color::Gray,
            Color::Gray => Color::White,
            other => other,
        }
    } else {
        fg
    };

    let usage_suffix = app
        .claude_usage
        .get(&sid)
        .map(|u| (format!(" {}%", u.usage_pct()), Color::DarkGray));

    let prefix = format!("  {} ", status_icon);
    let suffix_len = usage_suffix.as_ref().map(|(s, _)| s.len()).unwrap_or(0);
    let max_name = inner_width
        .saturating_sub(prefix.len())
        .saturating_sub(suffix_len);
    let truncated = if display_name.len() > max_name && max_name > 1 {
        let mut end = max_name.saturating_sub(1);
        while end > 0 && !display_name.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &display_name[..end])
    } else {
        display_name
    };

    let name_fg = if in_plan_mode {
        theme::AGENT_PLANNING
    } else {
        sel_fg
    };

    let mut spans = vec![
        Span::styled(
            prefix,
            Style::default().fg(sel_fg).bg(bg).add_modifier(bold),
        ),
        Span::styled(
            truncated,
            Style::default().fg(name_fg).bg(bg).add_modifier(bold),
        ),
    ];

    if let Some((usage_text, usage_color)) = usage_suffix {
        spans.push(Span::styled(
            usage_text,
            Style::default().fg(usage_color).bg(bg),
        ));
    }

    let text_len: usize = spans.iter().map(|s| s.width()).sum();
    if has_bg && text_len < inner_width {
        let pad = inner_width - text_len;
        if is_hovered && pad >= 2 {
            spans.push(Span::styled(" ".repeat(pad - 2), Style::default().bg(bg)));
            spans.push(Span::styled(
                "+ ",
                Style::default()
                    .fg(Color::Cyan)
                    .bg(bg)
                    .add_modifier(Modifier::BOLD),
            ));
        } else {
            spans.push(Span::styled(" ".repeat(pad), Style::default().bg(bg)));
        }
    }

    ListItem::new(Line::from(spans))
}

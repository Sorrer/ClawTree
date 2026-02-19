use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, List, ListItem, Paragraph};
use std::sync::atomic::Ordering;

use crate::app::{AgentStatus, App, MiniModeFocus, SidebarItem};
use super::theme;

/// Get the current spinner character based on the app's frame counter.
fn spinner_char(app: &App) -> char {
    let idx = (app.spinner_frame / 3) % theme::SPINNER_FRAMES.len();
    theme::SPINNER_FRAMES[idx]
}

/// Draw the mini mode agent list view.
pub fn draw(f: &mut Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),    // main area
            Constraint::Length(1), // status bar
        ])
        .split(f.area());

    draw_main(f, app, chunks[0]);
    super::status_bar::draw(f, app, chunks[1]);

    // Draw dialog on top if present
    if app.dialog.is_some() {
        super::dialogs::draw(f, app);
    }
    if let Some(ref msg) = app.loading_message {
        super::draw_loading_overlay(f, msg);
    }
    if app.show_help {
        super::help::draw(f, app);
    }
}

/// Draw the mini mode main content area.
fn draw_main(f: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .title(" Mini Mode ")
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Magenta));

    let inner = block.inner(area);
    f.render_widget(block, area);

    if inner.height < 3 {
        return;
    }

    match app.mini.focus {
        MiniModeFocus::AgentList => draw_agent_list_view(f, app, inner),
        MiniModeFocus::WorktreeSelector => draw_worktree_selector(f, app, inner),
        MiniModeFocus::PromptInput => draw_prompt_input(f, app, inner),
        MiniModeFocus::SavedPrompts => draw_saved_prompts(f, app, inner),
    }
}

/// Draw the tree-structured agent list with summary panel.
fn draw_agent_list_view(f: &mut Frame, app: &App, area: Rect) {
    let has_items = !app.mini.items.is_empty();
    let tree_height = if has_items {
        (app.mini.items.len() as u16).min(area.height.saturating_sub(6))
    } else {
        3
    };

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(tree_height), // tree list
            Constraint::Min(3),              // summary panel
            Constraint::Length(1),           // hints
        ])
        .split(area);

    // ── Tree list ──
    let inner_width = area.width as usize;

    if app.mini.items.is_empty() {
        let lines = vec![
            Line::styled(
                "  No worktrees. Press 'a' to create an agent.",
                Style::default().fg(Color::DarkGray),
            ),
        ];
        f.render_widget(Paragraph::new(lines), chunks[0]);
    } else {
        let items: Vec<ListItem> = app.mini.items
            .iter()
            .enumerate()
            .map(|(idx, item)| {
                let is_selected = idx == app.mini.selected;
                match item {
                    SidebarItem::Worktree(wi) => render_worktree(app, *wi, is_selected, inner_width),
                    SidebarItem::Session(wi, si) => render_session(app, *wi, *si, is_selected, inner_width),
                }
            })
            .collect();

        let list = List::new(items);
        f.render_widget(list, chunks[0]);
    }

    // ── Summary panel ──
    let summary_area = chunks[1];
    let mut summary_lines: Vec<Line> = Vec::new();
    summary_lines.push(Line::styled(
        format!("  {} Summary {}",
            "──",
            "─".repeat((summary_area.width as usize).saturating_sub(14).max(0))
        ),
        Style::default().fg(Color::DarkGray),
    ));

    // Show summary for the selected session (if a session is selected)
    let selected_sid = match app.mini.items.get(app.mini.selected) {
        Some(SidebarItem::Session(wi, si)) => {
            app.worktrees.get(*wi).and_then(|wt| wt.session_ids.get(*si)).copied()
        }
        _ => None,
    };

    if let Some(sid) = selected_sid {
        if let Some(summary) = app.agent_summaries.get(&sid) {
            for line in summary.lines().take(summary_area.height.saturating_sub(1) as usize) {
                summary_lines.push(Line::styled(
                    format!("  {}", line),
                    Style::default().fg(Color::White),
                ));
            }
        } else {
            summary_lines.push(Line::styled(
                "  (no summary yet)",
                Style::default().fg(Color::DarkGray),
            ));
        }
    } else {
        // Worktree selected — show worktree info
        if let Some(SidebarItem::Worktree(wi)) = app.mini.items.get(app.mini.selected) {
            if let Some(wt) = app.worktrees.get(*wi) {
                let total = wt.session_ids.len();
                let working = wt.session_ids.iter().filter(|sid| {
                    app.sessions.get(sid).map(|s| s.is_active()).unwrap_or(false)
                }).count();
                let idle = wt.session_ids.iter().filter(|sid| {
                    app.sessions.get(sid).map(|s| {
                        let status = s.agent_status();
                        status == AgentStatus::Idle
                    }).unwrap_or(false)
                }).count();
                summary_lines.push(Line::from(vec![
                    Span::styled(format!("  {} agents", total), Style::default().fg(Color::White)),
                    Span::styled(format!("  {} working", working), Style::default().fg(theme::AGENT_WORKING)),
                    Span::styled(format!("  {} idle", idle), Style::default().fg(theme::AGENT_IDLE)),
                ]));
            }
        }
    }

    f.render_widget(Paragraph::new(summary_lines), summary_area);

    // ── Hints bar ──
    let hints = Line::from(vec![
        Span::styled(" F2", Style::default().fg(Color::Yellow)),
        Span::styled(":normal ", Style::default().fg(Color::DarkGray)),
        Span::styled("Enter", Style::default().fg(Color::Yellow)),
        Span::styled(":open ", Style::default().fg(Color::DarkGray)),
        Span::styled("a", Style::default().fg(Color::Yellow)),
        Span::styled(":new ", Style::default().fg(Color::DarkGray)),
        Span::styled("d", Style::default().fg(Color::Yellow)),
        Span::styled(":kill ", Style::default().fg(Color::DarkGray)),
        Span::styled("r", Style::default().fg(Color::Yellow)),
        Span::styled(":rename ", Style::default().fg(Color::DarkGray)),
        Span::styled("Space", Style::default().fg(Color::Yellow)),
        Span::styled(":expand ", Style::default().fg(Color::DarkGray)),
        Span::styled("s", Style::default().fg(Color::Yellow)),
        Span::styled(":prompts ", Style::default().fg(Color::DarkGray)),
        Span::styled("?", Style::default().fg(Color::Yellow)),
        Span::styled(":help", Style::default().fg(Color::DarkGray)),
    ]);
    f.render_widget(Paragraph::new(hints), chunks[2]);
}

/// Render a worktree row in the mini mode tree.
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

    let bg = if is_selected { theme::MINI_SELECTED_BG } else { Color::Reset };
    let bold = if is_selected { Modifier::BOLD } else { Modifier::empty() };

    let branch_style = Style::default()
        .fg(Color::White)
        .bg(bg)
        .add_modifier(bold);

    let mut spans = vec![
        Span::styled(format!(" {} {}", icon, wt.branch), branch_style),
    ];

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

    // Pad to full width so highlight covers the whole row
    let text_len: usize = spans.iter().map(|s| s.content.len()).sum();
    if is_selected && text_len < inner_width {
        spans.push(Span::styled(
            " ".repeat(inner_width - text_len),
            Style::default().bg(bg),
        ));
    }

    ListItem::new(Line::from(spans))
}

/// Render a session row in the mini mode tree with status badge and summary snippet.
fn render_session(app: &App, wi: usize, si: usize, is_selected: bool, inner_width: usize) -> ListItem<'static> {
    let wt = &app.worktrees[wi];
    let sid = wt.session_ids[si];
    let session = app.sessions.get(&sid);

    let is_exited = session
        .map(|s| s.exited.load(Ordering::SeqCst))
        .unwrap_or(true);
    let is_working = session
        .map(|s| s.is_active())
        .unwrap_or(false);

    let nickname = session.and_then(|s| s.nickname.clone());
    let title = session.and_then(|s| s.terminal_title());

    // Prefer nickname, then terminal title (stripped of status chars), then label
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

    // Status indicator
    let status_icon: String = if is_exited {
        "✗".to_string()
    } else if is_working {
        spinner_char(app).to_string()
    } else {
        "○".to_string()
    };

    // Agent status for badge
    let status = session
        .map(|s| s.agent_status())
        .unwrap_or(AgentStatus::Exited);

    let (status_text, status_color) = match status {
        AgentStatus::Working => ("Working", theme::AGENT_WORKING),
        AgentStatus::Idle => ("Idle", theme::AGENT_IDLE),
        AgentStatus::NeedsInput => ("Needs Input", theme::AGENT_NEEDS_INPUT),
        AgentStatus::Exited => ("Exited", theme::AGENT_EXITED),
    };

    // Status color for the icon
    let fg = if is_exited {
        Color::DarkGray
    } else if is_working {
        Color::Yellow
    } else {
        Color::Gray
    };

    let bg = if is_selected { theme::MINI_SELECTED_BG } else { Color::Reset };
    let bold = if is_selected { Modifier::BOLD } else { Modifier::empty() };

    // When highlighted, bump dim text to lighter so it's readable
    let sel_fg = if is_selected {
        match fg {
            Color::DarkGray => Color::Gray,
            Color::Gray => Color::White,
            other => other,
        }
    } else {
        fg
    };

    // Build context usage badge if available
    let usage_badge = app.claude_usage.get(&sid).map(|u| {
        let pct = if u.effective_window > 0 {
            (u.tokens_used as f64 / u.effective_window as f64 * 100.0) as usize
        } else {
            0
        };
        let tokens_k = u.tokens_used / 1000;
        let window_k = u.effective_window / 1000;
        let color = if pct >= 80 {
            Color::Red
        } else if pct >= 50 {
            Color::Yellow
        } else {
            Color::Green
        };
        (format!(" {}k/{}k", tokens_k, window_k), color)
    });

    // Summary snippet (truncated)
    let summary_snippet = app.agent_summaries.get(&sid)
        .map(|s| {
            let first_line = s.lines().next().unwrap_or("");
            if first_line.len() > 40 {
                format!("\"{}...\"", &first_line[..37])
            } else {
                format!("\"{}\"", first_line)
            }
        })
        .unwrap_or_default();

    // Truncate name to fit
    let prefix = format!("  {} ", status_icon);
    let status_badge = format!(" {}", status_text);
    let usage_len = usage_badge.as_ref().map(|(s, _)| s.len()).unwrap_or(0);
    let available = inner_width.saturating_sub(prefix.len() + status_badge.len() + usage_len + 2);
    let truncated_name = if display_name.len() > available && available > 1 {
        format!("{}…", &display_name[..available.saturating_sub(1)])
    } else {
        display_name
    };

    let mut spans = vec![
        Span::styled(
            prefix,
            Style::default().fg(sel_fg).bg(bg),
        ),
        Span::styled(
            truncated_name,
            Style::default().fg(if is_selected { Color::White } else { sel_fg }).bg(bg).add_modifier(bold),
        ),
        Span::styled(
            status_badge,
            Style::default().fg(status_color).bg(bg).add_modifier(
                if status == AgentStatus::NeedsInput { Modifier::BOLD } else { Modifier::empty() }
            ),
        ),
    ];

    // Append usage indicator
    if let Some((usage_text, usage_color)) = usage_badge {
        spans.push(Span::styled(
            usage_text,
            Style::default().fg(usage_color).bg(bg),
        ));
    }

    if !summary_snippet.is_empty() {
        spans.push(Span::styled(
            format!(" {}", summary_snippet),
            Style::default().fg(Color::DarkGray).bg(bg),
        ));
    }

    // Pad to full width so highlight covers the whole row
    let text_len: usize = spans.iter().map(|s| s.content.len()).sum();
    if is_selected && text_len < inner_width {
        spans.push(Span::styled(
            " ".repeat(inner_width - text_len),
            Style::default().bg(bg),
        ));
    }

    ListItem::new(Line::from(spans))
}

/// Draw the worktree selector for agent creation.
fn draw_worktree_selector(f: &mut Frame, app: &App, area: Rect) {
    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(vec![
        Span::styled("  SELECT WORKTREE", Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
    ]));
    lines.push(Line::styled(
        format!("  {}", "─".repeat((area.width as usize).saturating_sub(4))),
        Style::default().fg(Color::DarkGray),
    ));

    if app.worktrees.is_empty() {
        lines.push(Line::styled("  No worktrees available.", Style::default().fg(Color::DarkGray)));
    } else {
        for (i, wt) in app.worktrees.iter().enumerate() {
            let is_selected = i == app.mini.target_worktree_idx;
            let selector = if is_selected { " > " } else { "   " };
            let session_count = wt.session_ids.len();
            let count_text = if session_count > 0 {
                format!(" ({} agents)", session_count)
            } else {
                String::new()
            };

            let line = Line::from(vec![
                Span::styled(
                    selector,
                    Style::default().fg(if is_selected { Color::White } else { Color::DarkGray }),
                ),
                Span::styled(
                    wt.branch.clone(),
                    Style::default().fg(if is_selected { Color::Cyan } else { Color::White }),
                ),
                Span::styled(count_text, Style::default().fg(Color::DarkGray)),
            ]);
            lines.push(line);
        }
    }

    lines.push(Line::raw(""));
    lines.push(Line::from(vec![
        Span::styled("  Enter", Style::default().fg(Color::Yellow)),
        Span::styled(": select  ", Style::default().fg(Color::DarkGray)),
        Span::styled("Esc", Style::default().fg(Color::Yellow)),
        Span::styled(": cancel", Style::default().fg(Color::DarkGray)),
    ]));

    f.render_widget(Paragraph::new(lines), area);
}

/// Draw the prompt input for agent creation.
fn draw_prompt_input(f: &mut Frame, app: &App, area: Rect) {
    let wt_name = app.worktrees.get(app.mini.target_worktree_idx)
        .map(|wt| wt.branch.as_str())
        .unwrap_or("???");

    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(vec![
        Span::styled("  NEW AGENT", Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
        Span::styled(format!(" in {}", wt_name), Style::default().fg(Color::Cyan)),
    ]));
    lines.push(Line::styled(
        format!("  {}", "─".repeat((area.width as usize).saturating_sub(4))),
        Style::default().fg(Color::DarkGray),
    ));
    lines.push(Line::styled("  Enter a prompt for the agent:", Style::default().fg(Color::DarkGray)));
    lines.push(Line::raw(""));
    lines.push(Line::from(vec![
        Span::styled("  > ", Style::default().fg(Color::Yellow)),
        Span::raw(app.mini.prompt_input.clone()),
        Span::styled("_", Style::default().fg(Color::Yellow)),
    ]));
    lines.push(Line::raw(""));
    lines.push(Line::from(vec![
        Span::styled("  Enter", Style::default().fg(Color::Yellow)),
        Span::styled(": spawn  ", Style::default().fg(Color::DarkGray)),
        Span::styled("Tab", Style::default().fg(Color::Yellow)),
        Span::styled(": saved prompts  ", Style::default().fg(Color::DarkGray)),
        Span::styled("Esc", Style::default().fg(Color::Yellow)),
        Span::styled(": cancel", Style::default().fg(Color::DarkGray)),
    ]));

    f.render_widget(Paragraph::new(lines), area);
}

/// Draw the saved prompts browser.
fn draw_saved_prompts(f: &mut Frame, app: &App, area: Rect) {
    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(vec![
        Span::styled("  SAVED PROMPTS", Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
    ]));
    lines.push(Line::styled(
        format!("  {}", "─".repeat((area.width as usize).saturating_sub(4))),
        Style::default().fg(Color::DarkGray),
    ));

    if app.saved_prompts.is_empty() {
        lines.push(Line::styled("  No saved prompts. Press 'a' to save the current input.", Style::default().fg(Color::DarkGray)));
    } else {
        for (i, sp) in app.saved_prompts.iter().enumerate() {
            let is_selected = i == app.mini.saved_prompt_selected;
            let selector = if is_selected { " > " } else { "   " };
            let prompt_preview = if sp.prompt.len() > 50 {
                format!("{}...", &sp.prompt[..47])
            } else {
                sp.prompt.clone()
            };

            let line = Line::from(vec![
                Span::styled(
                    selector,
                    Style::default().fg(if is_selected { Color::White } else { Color::DarkGray }),
                ),
                Span::styled(
                    format!("[{}] ", sp.name),
                    Style::default().fg(Color::Yellow),
                ),
                Span::styled(
                    prompt_preview,
                    Style::default().fg(if is_selected { Color::White } else { Color::DarkGray }),
                ),
            ]);
            lines.push(line);
        }
    }

    lines.push(Line::raw(""));
    lines.push(Line::from(vec![
        Span::styled("  Enter", Style::default().fg(Color::Yellow)),
        Span::styled(": load  ", Style::default().fg(Color::DarkGray)),
        Span::styled("a", Style::default().fg(Color::Yellow)),
        Span::styled(": save current  ", Style::default().fg(Color::DarkGray)),
        Span::styled("d", Style::default().fg(Color::Yellow)),
        Span::styled(": delete  ", Style::default().fg(Color::DarkGray)),
        Span::styled("Esc", Style::default().fg(Color::Yellow)),
        Span::styled(": back", Style::default().fg(Color::DarkGray)),
    ]));

    f.render_widget(Paragraph::new(lines), area);
}

/// Draw the mini mode drilldown view (thin header + full terminal).
pub fn draw_drilldown(f: &mut Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // drilldown header
            Constraint::Min(1),    // terminal pane
            Constraint::Length(1), // status bar
        ])
        .split(f.area());

    // ── Header bar ──
    let sid = app.mini_drilldown_session.unwrap_or(0);
    let session = app.sessions.get(&sid);
    let agent_name = session
        .and_then(|s| s.nickname.clone())
        .or_else(|| session.and_then(|s| s.terminal_title()))
        .unwrap_or_else(|| format!("Agent-{}", sid));

    let status = session
        .map(|s| s.agent_status())
        .unwrap_or(AgentStatus::Exited);

    let (status_text, status_color) = match status {
        AgentStatus::Working => ("Working", theme::AGENT_WORKING),
        AgentStatus::Idle => ("Idle", theme::AGENT_IDLE),
        AgentStatus::NeedsInput => ("Needs Input", theme::AGENT_NEEDS_INPUT),
        AgentStatus::Exited => ("Exited", theme::AGENT_EXITED),
    };

    let header = Line::from(vec![
        Span::styled(
            " MINI ",
            Style::default().fg(Color::Black).bg(theme::MODE_MINI_BG).add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled(&agent_name, Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
        Span::styled(" | ", Style::default().fg(Color::DarkGray)),
        Span::styled(status_text, Style::default().fg(status_color)),
        Span::styled(" | ", Style::default().fg(Color::DarkGray)),
        Span::styled("Tab", Style::default().fg(Color::Yellow)),
        Span::styled(":back to agents  ", Style::default().fg(Color::DarkGray)),
        Span::styled("F2", Style::default().fg(Color::Yellow)),
        Span::styled(":normal", Style::default().fg(Color::DarkGray)),
    ]);

    f.render_widget(
        Paragraph::new(header).style(Style::default().bg(theme::MINI_DRILLDOWN_HEADER_BG)),
        chunks[0],
    );

    // ── Terminal pane (reuse existing draw) ──
    super::terminal_pane::draw(f, app, chunks[1]);

    // ── Status bar ──
    super::status_bar::draw(f, app, chunks[2]);

    // Overlays
    if app.dialog.is_some() {
        super::dialogs::draw(f, app);
    }
    if let Some(ref msg) = app.loading_message {
        super::draw_loading_overlay(f, msg);
    }
    if app.show_help {
        super::help::draw(f, app);
    }
}

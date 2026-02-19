use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, List, ListItem, Paragraph, Wrap};
use std::sync::atomic::Ordering;

use crate::app::{AgentStatus, App, MiniModeFocus, SidebarItem};
use super::theme;

/// Get the current spinner character based on the app's frame counter.
fn spinner_char(app: &App) -> char {
    let idx = (app.spinner_frame / 3) % theme::SPINNER_FRAMES.len();
    theme::SPINNER_FRAMES[idx]
}

/// Draw the mini mode view (sidebar + detail pane).
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

/// Draw the mini mode main area: sidebar tree (left) + detail pane (right).
fn draw_main(f: &mut Frame, app: &App, area: Rect) {
    // Split horizontally: 30% sidebar, 70% detail pane (matching normal mode proportions)
    let h_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(30),
            Constraint::Percentage(70),
        ])
        .split(area);

    draw_tree_sidebar(f, app, h_chunks[0]);

    // Right pane content depends on focus
    match app.mini.focus {
        MiniModeFocus::AgentList | MiniModeFocus::DetailInput => {
            draw_detail_pane(f, app, h_chunks[1]);
        }
        MiniModeFocus::WorktreeSelector => draw_worktree_selector(f, app, h_chunks[1]),
        MiniModeFocus::PromptInput => draw_prompt_input(f, app, h_chunks[1]),
        MiniModeFocus::SavedPrompts => draw_saved_prompts(f, app, h_chunks[1]),
    }
}

// ── Tree sidebar (left pane) ───────────────────────────────────────

fn draw_tree_sidebar(f: &mut Frame, app: &App, area: Rect) {
    let is_focused = app.mini.focus == MiniModeFocus::AgentList;

    let border_style = if is_focused {
        Style::default().fg(theme::BORDER_FOCUSED_SIDEBAR).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme::BORDER_UNFOCUSED)
    };
    let border_type = if is_focused { BorderType::Thick } else { BorderType::Plain };
    let title = if is_focused { " ▸ Agents " } else { " Agents " };

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_type(border_type)
        .border_style(border_style);

    let inner_width = area.width.saturating_sub(2) as usize;

    if app.mini.items.is_empty() {
        let empty = Paragraph::new(Line::styled(
            " No agents. 'a' to create.",
            Style::default().fg(Color::DarkGray),
        ))
        .block(block);
        f.render_widget(empty, area);
        return;
    }

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

    let list = List::new(items).block(block);
    f.render_widget(list, area);
}

// ── Detail pane (right side) ───────────────────────────────────────

fn draw_detail_pane(f: &mut Frame, app: &App, area: Rect) {
    let is_focused = app.mini.focus == MiniModeFocus::DetailInput;

    let border_style = if is_focused {
        Style::default().fg(theme::BORDER_FOCUSED_TERMINAL).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme::BORDER_UNFOCUSED)
    };
    let border_type = if is_focused { BorderType::Thick } else { BorderType::Plain };

    // Determine what's selected
    match app.mini.items.get(app.mini.selected).copied() {
        Some(SidebarItem::Session(wi, si)) => {
            draw_session_detail(f, app, area, wi, si, border_style, border_type, is_focused);
        }
        Some(SidebarItem::Worktree(wi)) => {
            draw_worktree_detail(f, app, area, wi, border_style, border_type);
        }
        None => {
            let block = Block::default()
                .title(" Details ")
                .borders(Borders::ALL)
                .border_type(border_type)
                .border_style(border_style);
            let msg = Paragraph::new(Line::styled(
                " Select an agent to view details.",
                Style::default().fg(Color::DarkGray),
            ))
            .block(block);
            f.render_widget(msg, area);
        }
    }
}

fn draw_session_detail(
    f: &mut Frame,
    app: &App,
    area: Rect,
    wi: usize,
    si: usize,
    border_style: Style,
    border_type: BorderType,
    is_focused: bool,
) {
    let wt = match app.worktrees.get(wi) {
        Some(wt) => wt,
        None => return,
    };
    let sid = match wt.session_ids.get(si) {
        Some(&sid) => sid,
        None => return,
    };
    let session = app.sessions.get(&sid);

    // Agent display name
    let display_name = session
        .and_then(|s| s.nickname.clone())
        .or_else(|| session.and_then(|s| s.terminal_title()))
        .unwrap_or_else(|| format!("Agent-{}", sid));
    let clean_name = display_name
        .trim_start_matches('✳')
        .trim_start_matches('⠂')
        .trim_start_matches('⠐')
        .trim_start()
        .to_string();

    let title = format!(" {} ", clean_name);
    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_type(border_type)
        .border_style(border_style);

    let inner = block.inner(area);
    f.render_widget(block, area);

    if inner.height < 2 {
        return;
    }

    let status = session
        .map(|s| s.agent_status())
        .unwrap_or(AgentStatus::Exited);

    let (status_text, status_color) = match status {
        AgentStatus::Working => ("Working", theme::AGENT_WORKING),
        AgentStatus::Idle => ("Idle", theme::AGENT_IDLE),
        AgentStatus::NeedsInput => ("Needs Input", theme::AGENT_NEEDS_INPUT),
        AgentStatus::Exited => ("Exited", theme::AGENT_EXITED),
    };

    // Status icon
    let is_exited = session.map(|s| s.exited.load(Ordering::SeqCst)).unwrap_or(true);
    let is_working = session.map(|s| s.is_active()).unwrap_or(false);
    let status_icon: String = if is_exited {
        "✗".to_string()
    } else if is_working {
        spinner_char(app).to_string()
    } else {
        "○".to_string()
    };

    // Usage info
    let usage_text = app.claude_usage.get(&sid).map(|u| {
        if u.effective_window > 0 {
            let pct = (u.tokens_used as f64 / u.effective_window as f64 * 100.0) as usize;
            format!("Context: {}%", pct)
        } else {
            String::new()
        }
    }).unwrap_or_default();

    // Build content lines
    let mut lines: Vec<Line> = Vec::new();

    // Status row
    let mut status_spans = vec![
        Span::styled(
            format!(" {} ", status_icon),
            Style::default().fg(status_color),
        ),
        Span::styled(
            status_text,
            Style::default().fg(status_color).add_modifier(
                if status == AgentStatus::NeedsInput { Modifier::BOLD } else { Modifier::empty() }
            ),
        ),
        Span::styled(
            format!("  {}", wt.branch),
            Style::default().fg(Color::DarkGray),
        ),
    ];
    if !usage_text.is_empty() {
        status_spans.push(Span::styled(
            format!("  {}", usage_text),
            Style::default().fg(Color::DarkGray),
        ));
    }
    lines.push(Line::from(status_spans));

    // Separator
    let sep = "─".repeat(inner.width.saturating_sub(2) as usize);
    lines.push(Line::styled(format!(" {}", sep), Style::default().fg(Color::DarkGray)));

    // Summary section
    lines.push(Line::raw(""));
    if let Some(summary) = app.agent_summaries.get(&sid) {
        for line in summary.lines() {
            lines.push(Line::styled(
                format!(" {}", line),
                Style::default().fg(Color::White),
            ));
        }
    } else if status == AgentStatus::Working {
        lines.push(Line::styled(
            " Agent is working...",
            Style::default().fg(Color::DarkGray),
        ));
    } else {
        lines.push(Line::styled(
            " (no summary yet)",
            Style::default().fg(Color::DarkGray),
        ));
    }

    // Calculate space for summary vs input
    let input_height: u16 = if status == AgentStatus::Idle || status == AgentStatus::NeedsInput {
        4 // base height (separator + hints + 1 input line + blank)
    } else {
        0
    };

    // Input section (only when agent is idle or needs input)
    // Calculate input height dynamically based on line count
    let input_line_count = if input_height > 0 {
        let text_lines = app.mini.detail_input.split('\n').count().max(1);
        // 1 separator + 1 hints + text_lines (min 1) + 1 blank
        (text_lines as u16) + 3
    } else {
        0
    };
    let effective_input_height = if input_height > 0 { input_line_count } else { 0 };

    let summary_area = Rect {
        x: inner.x,
        y: inner.y,
        width: inner.width,
        height: inner.height.saturating_sub(effective_input_height),
    };
    f.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }),
        summary_area,
    );

    if effective_input_height > 0 && inner.height > effective_input_height + 2 {
        let input_y = inner.y + inner.height.saturating_sub(effective_input_height);
        let input_area = Rect {
            x: inner.x,
            y: input_y,
            width: inner.width,
            height: effective_input_height,
        };

        let sep2 = "─".repeat(inner.width.saturating_sub(2) as usize);
        let cursor_style = if is_focused {
            Style::default().fg(Color::Yellow).add_modifier(Modifier::RAPID_BLINK)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        let prompt_color = if is_focused { Color::Cyan } else { Color::DarkGray };

        let hint_text = if is_focused {
            " Tab:back  Enter:send  Alt+Enter:newline"
        } else {
            " Tab:focus input"
        };

        let mut input_lines = vec![
            Line::styled(format!(" {}", sep2), Style::default().fg(Color::DarkGray)),
            Line::from(vec![
                Span::styled(hint_text, Style::default().fg(Color::DarkGray)),
            ]),
        ];

        // Render multi-line input: first line gets the prompt, rest are indented
        let text_parts: Vec<&str> = app.mini.detail_input.split('\n').collect();
        for (i, part) in text_parts.iter().enumerate() {
            if i == 0 {
                input_lines.push(Line::from(vec![
                    Span::styled(" ❯ ", Style::default().fg(prompt_color)),
                    Span::raw(part.to_string()),
                    if i == text_parts.len() - 1 {
                        Span::styled("█", cursor_style)
                    } else {
                        Span::raw("")
                    },
                ]));
            } else {
                input_lines.push(Line::from(vec![
                    Span::raw("   "),
                    Span::raw(part.to_string()),
                    if i == text_parts.len() - 1 {
                        Span::styled("█", cursor_style)
                    } else {
                        Span::raw("")
                    },
                ]));
            }
        }

        f.render_widget(Paragraph::new(input_lines), input_area);
    }
}

fn draw_worktree_detail(
    f: &mut Frame,
    app: &App,
    area: Rect,
    wi: usize,
    border_style: Style,
    border_type: BorderType,
) {
    let wt = match app.worktrees.get(wi) {
        Some(wt) => wt,
        None => return,
    };

    let title = format!(" {} ", wt.branch);
    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_type(border_type)
        .border_style(border_style);

    let inner = block.inner(area);
    f.render_widget(block, area);

    if inner.height < 2 {
        return;
    }

    let total = wt.session_ids.len();
    let alive = wt.session_ids.iter().filter(|sid| {
        app.sessions.get(sid).map(|s| !s.exited.load(Ordering::SeqCst)).unwrap_or(false)
    }).count();
    let working = wt.session_ids.iter().filter(|sid| {
        app.sessions.get(sid).map(|s| s.is_active()).unwrap_or(false)
    }).count();
    let idle = total.saturating_sub(working).saturating_sub(total.saturating_sub(alive));

    let mut lines: Vec<Line> = Vec::new();

    lines.push(Line::from(vec![
        Span::styled(" Agents: ", Style::default().fg(Color::DarkGray)),
        Span::styled(format!("{}", total), Style::default().fg(Color::White)),
    ]));

    if total > 0 {
        lines.push(Line::from(vec![
            Span::styled("   ", Style::default()),
            Span::styled(format!("{} working", working), Style::default().fg(theme::AGENT_WORKING)),
            Span::styled("  ", Style::default()),
            Span::styled(format!("{} idle", idle), Style::default().fg(theme::AGENT_IDLE)),
        ]));
    }

    let sep = "─".repeat(inner.width.saturating_sub(2) as usize);
    lines.push(Line::styled(format!(" {}", sep), Style::default().fg(Color::DarkGray)));
    lines.push(Line::raw(""));

    // Show brief summary for each agent in this worktree
    for &sid in &wt.session_ids {
        let session = app.sessions.get(&sid);
        let name = session
            .and_then(|s| s.nickname.clone())
            .or_else(|| session.and_then(|s| s.terminal_title()))
            .unwrap_or_else(|| format!("Agent-{}", sid));
        let clean = name.trim_start_matches('✳')
            .trim_start_matches('⠂')
            .trim_start_matches('⠐')
            .trim_start();

        let status = session.map(|s| s.agent_status()).unwrap_or(AgentStatus::Exited);
        let (status_text, status_color) = match status {
            AgentStatus::Working => ("working", theme::AGENT_WORKING),
            AgentStatus::Idle => ("idle", theme::AGENT_IDLE),
            AgentStatus::NeedsInput => ("needs input", theme::AGENT_NEEDS_INPUT),
            AgentStatus::Exited => ("exited", theme::AGENT_EXITED),
        };

        lines.push(Line::from(vec![
            Span::styled(format!(" {}", clean), Style::default().fg(Color::White)),
            Span::styled(format!(" ({})", status_text), Style::default().fg(status_color)),
        ]));

        if let Some(summary) = app.agent_summaries.get(&sid) {
            let first_line = summary.lines().next().unwrap_or("");
            let truncated = if first_line.len() > (inner.width as usize).saturating_sub(4) {
                format!("{}...", &first_line[..(inner.width as usize).saturating_sub(7).max(3)])
            } else {
                first_line.to_string()
            };
            lines.push(Line::styled(
                format!("   {}", truncated),
                Style::default().fg(Color::DarkGray),
            ));
        }
    }

    if total == 0 {
        lines.push(Line::styled(
            " No agents. Press 'a' to create one.",
            Style::default().fg(Color::DarkGray),
        ));
    }

    f.render_widget(Paragraph::new(lines), inner);
}

// ── Tree item renderers ────────────────────────────────────────────

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

    // Pad to full width
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

    let is_exited = session
        .map(|s| s.exited.load(Ordering::SeqCst))
        .unwrap_or(true);
    let is_working = session
        .map(|s| s.is_active())
        .unwrap_or(false);

    let nickname = session.and_then(|s| s.nickname.clone());
    let title = session.and_then(|s| s.terminal_title());

    let display_name = nickname.unwrap_or_else(|| {
        title
            .unwrap_or_else(|| {
                session.map(|s| s.label.clone()).unwrap_or_else(|| "???".to_string())
            })
            .trim_start_matches('✳')
            .trim_start_matches('⠂')
            .trim_start_matches('⠐')
            .trim_start()
            .to_string()
    });

    let status_icon: String = if is_exited {
        "✗".to_string()
    } else if is_working {
        spinner_char(app).to_string()
    } else {
        "○".to_string()
    };

    let status = session.map(|s| s.agent_status()).unwrap_or(AgentStatus::Exited);

    let fg = if is_exited {
        Color::DarkGray
    } else if is_working {
        Color::Yellow
    } else {
        Color::Gray
    };

    let bg = if is_selected { theme::SIDEBAR_SEL_BG } else { Color::Reset };
    let bold = if is_selected { Modifier::BOLD } else { Modifier::empty() };

    let sel_fg = if is_selected {
        match fg {
            Color::DarkGray => Color::Gray,
            Color::Gray => Color::White,
            other => other,
        }
    } else {
        fg
    };

    // Usage percentage
    let usage_str = app.claude_usage.get(&sid).and_then(|u| {
        if u.effective_window > 0 {
            Some(format!(" {}%", (u.tokens_used as f64 / u.effective_window as f64 * 100.0) as usize))
        } else {
            None
        }
    });

    let prefix = format!("  {} ", status_icon);
    let usage_len = usage_str.as_ref().map(|s| s.len()).unwrap_or(0);
    let max_name = inner_width.saturating_sub(prefix.len() + usage_len);
    let truncated = if display_name.len() > max_name && max_name > 1 {
        format!("{}…", &display_name[..max_name.saturating_sub(1)])
    } else {
        display_name
    };

    let text = format!("{}{}", prefix, truncated);

    let mut spans = vec![
        Span::styled(text, Style::default().fg(sel_fg).bg(bg).add_modifier(bold)),
    ];

    if let Some(usage) = usage_str {
        let usage_color = if status == AgentStatus::NeedsInput {
            theme::AGENT_NEEDS_INPUT
        } else {
            Color::DarkGray
        };
        spans.push(Span::styled(usage, Style::default().fg(usage_color).bg(bg)));
    }

    // Pad to full width
    let text_len: usize = spans.iter().map(|s| s.content.len()).sum();
    if is_selected && text_len < inner_width {
        spans.push(Span::styled(
            " ".repeat(inner_width - text_len),
            Style::default().bg(bg),
        ));
    }

    ListItem::new(Line::from(spans))
}

// ── Agent creation sub-views (shown in right pane) ─────────────────

fn draw_worktree_selector(f: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .title(" Select Worktree ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let mut lines: Vec<Line> = Vec::new();

    if app.worktrees.is_empty() {
        lines.push(Line::styled(" No worktrees available.", Style::default().fg(Color::DarkGray)));
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

            lines.push(Line::from(vec![
                Span::styled(selector, Style::default().fg(if is_selected { Color::White } else { Color::DarkGray })),
                Span::styled(wt.branch.clone(), Style::default().fg(if is_selected { Color::Cyan } else { Color::White })),
                Span::styled(count_text, Style::default().fg(Color::DarkGray)),
            ]));
        }
    }

    lines.push(Line::raw(""));
    lines.push(Line::from(vec![
        Span::styled(" Enter", Style::default().fg(Color::Yellow)),
        Span::styled(": select  ", Style::default().fg(Color::DarkGray)),
        Span::styled("Esc", Style::default().fg(Color::Yellow)),
        Span::styled(": cancel", Style::default().fg(Color::DarkGray)),
    ]));

    f.render_widget(Paragraph::new(lines), inner);
}

fn draw_prompt_input(f: &mut Frame, app: &App, area: Rect) {
    let wt_name = app.worktrees.get(app.mini.target_worktree_idx)
        .map(|wt| wt.branch.as_str())
        .unwrap_or("???");

    let block = Block::default()
        .title(format!(" New Agent in {} ", wt_name))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Green));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let mut lines = vec![
        Line::styled(" Enter a prompt for the agent:", Style::default().fg(Color::DarkGray)),
        Line::raw(""),
    ];

    // Render multi-line prompt input
    let cursor_style = Style::default().fg(Color::Yellow).add_modifier(Modifier::RAPID_BLINK);
    let text_parts: Vec<&str> = app.mini.prompt_input.split('\n').collect();
    for (i, part) in text_parts.iter().enumerate() {
        if i == 0 {
            lines.push(Line::from(vec![
                Span::styled(" ❯ ", Style::default().fg(Color::Yellow)),
                Span::raw(part.to_string()),
                if i == text_parts.len() - 1 {
                    Span::styled("█", cursor_style)
                } else {
                    Span::raw("")
                },
            ]));
        } else {
            lines.push(Line::from(vec![
                Span::raw("   "),
                Span::raw(part.to_string()),
                if i == text_parts.len() - 1 {
                    Span::styled("█", cursor_style)
                } else {
                    Span::raw("")
                },
            ]));
        }
    }

    lines.push(Line::raw(""));
    lines.push(Line::from(vec![
        Span::styled(" Enter", Style::default().fg(Color::Yellow)),
        Span::styled(": spawn  ", Style::default().fg(Color::DarkGray)),
        Span::styled("Alt+Enter", Style::default().fg(Color::Yellow)),
        Span::styled(": newline  ", Style::default().fg(Color::DarkGray)),
        Span::styled("Tab", Style::default().fg(Color::Yellow)),
        Span::styled(": saved prompts  ", Style::default().fg(Color::DarkGray)),
        Span::styled("Esc", Style::default().fg(Color::Yellow)),
        Span::styled(": cancel", Style::default().fg(Color::DarkGray)),
    ]));

    f.render_widget(Paragraph::new(lines), inner);
}

fn draw_saved_prompts(f: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .title(" Saved Prompts ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let mut lines: Vec<Line> = Vec::new();

    if app.saved_prompts.is_empty() {
        lines.push(Line::styled(" No saved prompts. Press 'a' to save current input.", Style::default().fg(Color::DarkGray)));
    } else {
        for (i, sp) in app.saved_prompts.iter().enumerate() {
            let is_selected = i == app.mini.saved_prompt_selected;
            let selector = if is_selected { " > " } else { "   " };
            let max_len = inner.width.saturating_sub(10) as usize;
            let preview = if sp.prompt.len() > max_len {
                format!("{}...", &sp.prompt[..max_len.saturating_sub(3)])
            } else {
                sp.prompt.clone()
            };

            lines.push(Line::from(vec![
                Span::styled(selector, Style::default().fg(if is_selected { Color::White } else { Color::DarkGray })),
                Span::styled(format!("[{}] ", sp.name), Style::default().fg(Color::Yellow)),
                Span::styled(preview, Style::default().fg(if is_selected { Color::White } else { Color::DarkGray })),
            ]));
        }
    }

    lines.push(Line::raw(""));
    lines.push(Line::from(vec![
        Span::styled(" Enter", Style::default().fg(Color::Yellow)),
        Span::styled(": load  ", Style::default().fg(Color::DarkGray)),
        Span::styled("a", Style::default().fg(Color::Yellow)),
        Span::styled(": save  ", Style::default().fg(Color::DarkGray)),
        Span::styled("d", Style::default().fg(Color::Yellow)),
        Span::styled(": delete  ", Style::default().fg(Color::DarkGray)),
        Span::styled("Esc", Style::default().fg(Color::Yellow)),
        Span::styled(": back", Style::default().fg(Color::DarkGray)),
    ]));

    f.render_widget(Paragraph::new(lines), inner);
}

// ── Drilldown view ─────────────────────────────────────────────────

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

    // ── Terminal pane ──
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

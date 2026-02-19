use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};

use crate::app::{AgentStatus, App, MiniModeFocus};
use super::theme;

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

/// Draw the agent list with summary panel and quick prompts.
fn draw_agent_list_view(f: &mut Frame, app: &App, area: Rect) {
    // Split: agent list top, summary middle, hints bottom
    let has_agents = !app.mini.agent_list.is_empty();
    let agent_list_height = if has_agents {
        (app.mini.agent_list.len() as u16 + 2).min(area.height.saturating_sub(6))
    } else {
        3
    };

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(agent_list_height), // agent list
            Constraint::Min(3),                     // summary panel
            Constraint::Length(1),                  // hints
        ])
        .split(area);

    // ── Agent list ──
    let mut lines: Vec<Line> = Vec::new();
    let header = Line::from(vec![
        Span::styled(
            format!("  AGENTS ({})", app.mini.agent_list.len()),
            Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
        ),
    ]);
    lines.push(header);
    lines.push(Line::styled(
        "  ".to_string() + &"─".repeat((area.width as usize).saturating_sub(4)),
        Style::default().fg(Color::DarkGray),
    ));

    if app.mini.agent_list.is_empty() {
        lines.push(Line::styled(
            "  No agents running. Press 'a' to create one.",
            Style::default().fg(Color::DarkGray),
        ));
    } else {
        for (i, &(sid, wi)) in app.mini.agent_list.iter().enumerate() {
            let is_selected = i == app.mini.selected;
            let wt_name = app.worktrees.get(wi)
                .map(|wt| wt.branch.as_str())
                .unwrap_or("???");

            let session = app.sessions.get(&sid);
            let display_name = session
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

            let selector = if is_selected { " > " } else { "   " };

            let mut spans = vec![
                Span::styled(
                    selector,
                    Style::default().fg(if is_selected { Color::White } else { Color::DarkGray }),
                ),
                Span::styled(
                    format!("{}/", wt_name),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled(
                    format!("{:<20}", display_name),
                    Style::default().fg(Color::White),
                ),
                Span::styled(
                    format!(" {:<12}", status_text),
                    Style::default().fg(status_color).add_modifier(
                        if status == AgentStatus::NeedsInput { Modifier::BOLD } else { Modifier::empty() }
                    ),
                ),
            ];

            if !summary_snippet.is_empty() {
                spans.push(Span::styled(
                    format!(" {}", summary_snippet),
                    Style::default().fg(Color::DarkGray),
                ));
            }

            let line = Line::from(spans);
            if is_selected {
                // Render with highlight background
                let para = Paragraph::new(line)
                    .style(Style::default().bg(theme::MINI_SELECTED_BG));
                let row_area = Rect {
                    x: area.x,
                    y: chunks[0].y + 2 + i as u16,
                    width: area.width,
                    height: 1,
                };
                if row_area.y < chunks[0].y + chunks[0].height {
                    f.render_widget(para, row_area);
                }
            } else {
                lines.push(line);
            }
        }
    }

    // Render lines that aren't the selected one (which is rendered separately with bg)
    if app.mini.agent_list.is_empty() || app.mini.selected >= app.mini.agent_list.len() {
        let para = Paragraph::new(lines);
        f.render_widget(para, chunks[0]);
    } else {
        // We need to render all lines, inserting a placeholder for the selected row
        let mut all_lines: Vec<Line> = Vec::new();
        let mut agent_idx = 0;
        for (line_idx, line) in lines.into_iter().enumerate() {
            if line_idx >= 2 { // After header lines
                if agent_idx == app.mini.selected {
                    all_lines.push(Line::raw("")); // placeholder, rendered separately
                    agent_idx += 1;
                }
                agent_idx += 1;
            }
            all_lines.push(line);
        }
        // If selected was after all pushed lines
        if app.mini.selected >= app.mini.agent_list.len().saturating_sub(1) {
            // Already handled
        }
        let para = Paragraph::new(all_lines);
        f.render_widget(para, chunks[0]);

        // Render selected row with highlight
        let selected_row_y = chunks[0].y + 2 + app.mini.selected as u16;
        if selected_row_y < chunks[0].y + chunks[0].height {
            let &(sid, wi) = &app.mini.agent_list[app.mini.selected];
            let row_line = build_agent_row(app, sid, wi, true);
            let para = Paragraph::new(row_line)
                .style(Style::default().bg(theme::MINI_SELECTED_BG));
            let row_area = Rect {
                x: area.x,
                y: selected_row_y,
                width: area.width,
                height: 1,
            };
            f.render_widget(para, row_area);
        }
    }

    // ── Summary panel ──
    let summary_area = chunks[1];
    let mut summary_lines: Vec<Line> = Vec::new();
    summary_lines.push(Line::styled(
        format!("  {} Summary {}", "──", "─".repeat((summary_area.width as usize).saturating_sub(14).max(0))),
        Style::default().fg(Color::DarkGray),
    ));

    if let Some(&(sid, _)) = app.mini.agent_list.get(app.mini.selected) {
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
        Span::styled("s", Style::default().fg(Color::Yellow)),
        Span::styled(":prompts ", Style::default().fg(Color::DarkGray)),
        Span::styled("?", Style::default().fg(Color::Yellow)),
        Span::styled(":help", Style::default().fg(Color::DarkGray)),
    ]);
    f.render_widget(Paragraph::new(hints), chunks[2]);
}

/// Build a single agent row as a Line.
fn build_agent_row(app: &App, sid: u64, wi: usize, is_selected: bool) -> Line<'static> {
    let wt_name = app.worktrees.get(wi)
        .map(|wt| wt.branch.clone())
        .unwrap_or_else(|| "???".to_string());

    let session = app.sessions.get(&sid);
    let display_name = session
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

    let selector = if is_selected { " > " } else { "   " };

    let mut spans = vec![
        Span::styled(
            selector.to_string(),
            Style::default().fg(if is_selected { Color::White } else { Color::DarkGray }),
        ),
        Span::styled(
            format!("{}/", wt_name),
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(
            format!("{:<20}", display_name),
            Style::default().fg(Color::White),
        ),
        Span::styled(
            format!(" {:<12}", status_text),
            Style::default().fg(status_color).add_modifier(
                if status == AgentStatus::NeedsInput { Modifier::BOLD } else { Modifier::empty() }
            ),
        ),
    ];

    if !summary_snippet.is_empty() {
        spans.push(Span::styled(
            format!(" {}", summary_snippet),
            Style::default().fg(Color::DarkGray),
        ));
    }

    Line::from(spans)
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

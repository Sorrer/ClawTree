use std::path::Path;

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph};

use crate::app::{App, CommitPhase, ContextAction, ContextActionKind, Dialog, SidebarItem};
use super::theme;

pub fn draw(f: &mut Frame, app: &App) {
    let dialog = match &app.dialog {
        Some(d) => d,
        None => return,
    };

    match dialog {
        Dialog::CreateWorktree {
            branch_input,
            base_branch,
            focused_field,
        } => draw_create_worktree(f, app, branch_input, base_branch, *focused_field),
        Dialog::MergeBranch {
            source_worktree_idx,
            branches,
            selected,
        } => {
            let source_name = app
                .worktrees
                .get(*source_worktree_idx)
                .map(|w| w.branch.as_str())
                .unwrap_or("???");
            draw_merge(f, app, source_name, branches, *selected);
        }
        Dialog::Confirm { message, .. } => draw_confirm(f, app, message),
        Dialog::ConfirmDangerous { message, input, .. } => draw_confirm_dangerous(f, app, message, input),
        Dialog::InitRepo {
            url_input,
            branch_input,
            focused_field,
        } => draw_init_repo(f, app, url_input, branch_input, *focused_field),
        Dialog::RenameSession { input, .. } => draw_rename_session(f, app, input),
        Dialog::MergeConflict {
            worktree_idx,
            selected,
            ..
        } => {
            let branch_name = app
                .worktrees
                .get(*worktree_idx)
                .map(|w| w.branch.as_str())
                .unwrap_or("???");
            draw_merge_conflict(f, app, branch_name, *selected);
        }
        Dialog::DirtyWorktree {
            worktree_idx,
            files,
            selected,
        } => {
            let branch_name = app
                .worktrees
                .get(*worktree_idx)
                .map(|w| w.branch.as_str())
                .unwrap_or("???");
            draw_dirty_worktree(f, app, branch_name, files, *selected);
        }
        Dialog::MergeSuccess { message } => {
            draw_merge_success(f, app, message);
        }
        Dialog::GitCommit {
            worktree_idx,
            unstaged,
            staged,
            section,
            selected,
            phase,
            commit_message,
            cursor_pos,
        } => {
            let branch_name = app
                .worktrees
                .get(*worktree_idx)
                .map(|w| w.branch.as_str())
                .unwrap_or("???");
            draw_git_commit(f, app, branch_name, unstaged, staged, *section, *selected, *phase, commit_message, *cursor_pos);
        }
        Dialog::PullError {
            worktree_idx,
            error_message,
            selected,
        } => {
            let branch_name = app
                .worktrees
                .get(*worktree_idx)
                .map(|w| w.branch.as_str())
                .unwrap_or("???");
            draw_pull_error(f, app, branch_name, error_message, *selected);
        }
        Dialog::AuthError {
            operation,
            message,
            selected,
        } => {
            draw_auth_error(f, app, operation, message, *selected);
        }
        Dialog::ContextMenu {
            item,
            selected,
            actions,
        } => {
            draw_context_menu(f, app, item, *selected, actions);
        }
        Dialog::ConvertRepo {
            mode,
            target_path_input,
            branch_name,
            focused_field,
            source_repo_path,
            confirmed,
        } => {
            draw_convert_repo(f, app, source_repo_path, *mode, target_path_input, branch_name, *focused_field, *confirmed);
        }
    }
}

fn centered_rect(percent_x: u16, height: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length((r.height.saturating_sub(height)) / 2),
            Constraint::Length(height),
            Constraint::Min(0),
        ])
        .split(r);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}

fn draw_create_worktree(f: &mut Frame, app: &App, branch: &str, base: &str, focused: usize) {
    let area = centered_rect(50, 9, f.area());
    app.areas.dialog.set(area);
    f.render_widget(Clear, area);

    let block = Block::default()
        .title(" New Worktree ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::DIALOG_CREATION));

    let inner = block.inner(area);
    f.render_widget(block, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // branch label
            Constraint::Length(1), // branch input
            Constraint::Length(1), // spacer
            Constraint::Length(1), // base label
            Constraint::Length(1), // base input
            Constraint::Length(1), // spacer
            Constraint::Length(1), // help
        ])
        .split(inner);

    let branch_label_style = if focused == 0 {
        Style::default().fg(Color::White)
    } else {
        Style::default().fg(Color::Gray)
    };
    f.render_widget(
        Paragraph::new("Branch name (also used as directory name):")
            .style(branch_label_style),
        chunks[0],
    );

    let branch_style = if focused == 0 {
        Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Cyan)
    };
    let branch_display = if focused == 0 {
        format!("{}_", branch)
    } else {
        branch.to_string()
    };
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(branch_display, branch_style))),
        chunks[1],
    );

    let base_label_style = if focused == 1 {
        Style::default().fg(Color::White)
    } else {
        Style::default().fg(Color::Gray)
    };
    f.render_widget(
        Paragraph::new("Base branch:")
            .style(base_label_style),
        chunks[3],
    );

    let base_style = if focused == 1 {
        Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Cyan)
    };
    let base_display = if focused == 1 {
        format!("{}_", base)
    } else {
        base.to_string()
    };
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(base_display, base_style))),
        chunks[4],
    );

    f.render_widget(
        Paragraph::new("Enter: create  Tab: switch field  Esc: cancel")
            .style(Style::default().fg(Color::DarkGray)),
        chunks[6],
    );
}

fn draw_merge(f: &mut Frame, app: &App, source: &str, branches: &[String], selected: usize) {
    // Height: title + target info + separator + branches (max 10) + separator + help
    let visible_count = branches.len().min(10);
    let height = (visible_count as u16) + 5;
    let area = centered_rect(50, height, f.area());
    app.areas.dialog.set(area);
    f.render_widget(Clear, area);

    let block = Block::default()
        .title(" Merge Branch ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::DIALOG_NEUTRAL));

    let inner = block.inner(area);
    f.render_widget(block, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // target info
            Constraint::Length(1), // separator
            Constraint::Min(1),   // branch list
            Constraint::Length(1), // help
        ])
        .split(inner);

    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("Merge ", Style::default().fg(Color::Gray)),
            Span::styled(source, Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
            Span::styled(" into:", Style::default().fg(Color::Gray)),
        ])),
        chunks[0],
    );

    f.render_widget(
        Paragraph::new("Select target branch:")
            .style(Style::default().fg(Color::Gray)),
        chunks[1],
    );

    // Scrolling: keep selected in view
    let list_height = chunks[2].height as usize;
    let scroll_offset = if selected >= list_height {
        selected - list_height + 1
    } else {
        0
    };

    let items: Vec<ListItem> = branches
        .iter()
        .enumerate()
        .skip(scroll_offset)
        .take(list_height)
        .map(|(idx, branch)| {
            let is_sel = idx == selected;
            let marker = if is_sel { ">" } else { " " };
            let style = if is_sel {
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            };
            ListItem::new(Line::from(Span::styled(
                format!(" {} {}", marker, branch),
                style,
            )))
        })
        .collect();

    let list = List::new(items);
    f.render_widget(list, chunks[2]);

    f.render_widget(
        Paragraph::new("j/k: select  Enter: merge  Esc: cancel")
            .style(Style::default().fg(Color::DarkGray)),
        chunks[3],
    );
}

fn draw_confirm(f: &mut Frame, app: &App, message: &str) {
    let area = centered_rect(50, 5, f.area());
    app.areas.dialog.set(area);
    f.render_widget(Clear, area);

    let block = Block::default()
        .title(" Confirm ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::DIALOG_DESTRUCTIVE));

    let inner = block.inner(area);
    f.render_widget(block, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(inner);

    f.render_widget(
        Paragraph::new(message).style(Style::default().fg(Color::White)),
        chunks[0],
    );

    f.render_widget(
        Paragraph::new("Enter: yes  Esc: no")
            .style(Style::default().fg(Color::DarkGray)),
        chunks[2],
    );
}

fn draw_confirm_dangerous(f: &mut Frame, app: &App, message: &str, input: &str) {
    let area = centered_rect(55, 8, f.area());
    app.areas.dialog.set(area);
    f.render_widget(Clear, area);

    let block = Block::default()
        .title(" ⚠ WARNING ⚠ ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::DIALOG_DESTRUCTIVE).add_modifier(Modifier::BOLD));

    let inner = block.inner(area);
    f.render_widget(block, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // message
            Constraint::Length(1), // spacer
            Constraint::Length(1), // "Type 'yes' to confirm:"
            Constraint::Length(1), // input field
            Constraint::Length(1), // spacer
            Constraint::Length(1), // help text
        ])
        .split(inner);

    f.render_widget(
        Paragraph::new(message)
            .style(Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)),
        chunks[0],
    );

    f.render_widget(
        Paragraph::new("Type 'yes' to confirm:")
            .style(Style::default().fg(Color::Yellow)),
        chunks[2],
    );

    let input_display = format!("> {}_", input);
    f.render_widget(
        Paragraph::new(input_display)
            .style(Style::default().fg(Color::White)),
        chunks[3],
    );

    f.render_widget(
        Paragraph::new("Esc: cancel")
            .style(Style::default().fg(Color::DarkGray)),
        chunks[5],
    );
}

fn draw_init_repo(f: &mut Frame, app: &App, url: &str, branch: &str, focused: usize) {
    let area = centered_rect(60, 10, f.area());
    app.areas.dialog.set(area);
    f.render_widget(Clear, area);

    let block = Block::default()
        .title(" Initialize Bare Repo ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::DIALOG_CREATION));

    let inner = block.inner(area);
    f.render_widget(block, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // URL label
            Constraint::Length(1), // URL input
            Constraint::Length(1), // spacer
            Constraint::Length(1), // branch label
            Constraint::Length(1), // branch input
            Constraint::Length(1), // spacer
            Constraint::Length(1), // help
            Constraint::Min(0),
        ])
        .split(inner);

    let url_style = if focused == 0 {
        Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::White)
    };
    let branch_style = if focused == 1 {
        Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::White)
    };

    let url_label_style = if focused == 0 {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::Gray)
    };
    let branch_label_style = if focused == 1 {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::Gray)
    };

    f.render_widget(
        Paragraph::new("Remote URL (leave empty for local init):").style(url_label_style),
        chunks[0],
    );

    let url_display = if url.is_empty() && focused == 0 {
        "_".to_string()
    } else if focused == 0 {
        format!("{}_", url)
    } else {
        url.to_string()
    };
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(url_display, url_style))),
        chunks[1],
    );

    f.render_widget(
        Paragraph::new("Initial branch name:").style(branch_label_style),
        chunks[3],
    );

    let branch_display = if focused == 1 {
        format!("{}_", branch)
    } else {
        branch.to_string()
    };
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(branch_display, branch_style))),
        chunks[4],
    );

    f.render_widget(
        Paragraph::new("Tab: switch field  Enter: create  Esc: cancel")
            .style(Style::default().fg(Color::DarkGray)),
        chunks[6],
    );
}

fn draw_merge_conflict(f: &mut Frame, app: &App, branch: &str, selected: usize) {
    let options = ["VS Code", "JetBrains", "Claude", "Claude (skip perms)", "Abort merge"];
    let height = (options.len() as u16) + 6;
    let area = centered_rect(50, height, f.area());
    app.areas.dialog.set(area);
    f.render_widget(Clear, area);

    let block = Block::default()
        .title(" Merge Conflict ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::DIALOG_DESTRUCTIVE));

    let inner = block.inner(area);
    f.render_widget(block, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // conflict message
            Constraint::Length(1), // separator
            Constraint::Min(1),   // option list
            Constraint::Length(1), // help
        ])
        .split(inner);

    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("Conflicts in ", Style::default().fg(Color::Yellow)),
            Span::styled(
                branch,
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" — resolve with:", Style::default().fg(Color::Yellow)),
        ])),
        chunks[0],
    );

    let items: Vec<ListItem> = options
        .iter()
        .enumerate()
        .map(|(idx, label)| {
            let is_sel = idx == selected;
            let marker = if is_sel { ">" } else { " " };
            let style = if idx == options.len() - 1 {
                // Abort option in red
                if is_sel {
                    Style::default()
                        .fg(Color::Red)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::Red)
                }
            } else if is_sel {
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            };
            ListItem::new(Line::from(Span::styled(
                format!(" {} {}", marker, label),
                style,
            )))
        })
        .collect();

    let list = List::new(items);
    f.render_widget(list, chunks[2]);

    f.render_widget(
        Paragraph::new("j/k: select  Enter: open  Esc: close")
            .style(Style::default().fg(Color::DarkGray)),
        chunks[3],
    );
}

fn draw_pull_error(f: &mut Frame, app: &App, branch: &str, error_message: &str, selected: usize) {
    let options = ["Claude", "Claude (skip perms)", "Dismiss"];
    // Calculate error display lines (wrap at ~56 chars to fit in dialog)
    let wrap_width = 56usize;
    let error_lines: Vec<&str> = error_message.lines().collect();
    let mut wrapped_count = 0usize;
    for line in &error_lines {
        if line.is_empty() {
            wrapped_count += 1;
        } else {
            wrapped_count += line.len().div_ceil(wrap_width);
        }
    }
    let error_display_lines = wrapped_count.min(8); // cap error display at 8 lines
    let height = (options.len() as u16) + (error_display_lines as u16) + 7;
    let area = centered_rect(60, height, f.area());
    app.areas.dialog.set(area);
    f.render_widget(Clear, area);

    let block = Block::default()
        .title(" Pull Error ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::DIALOG_DESTRUCTIVE));

    let inner = block.inner(area);
    f.render_widget(block, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),                           // header
            Constraint::Length(1),                           // separator
            Constraint::Length(error_display_lines as u16),  // error message
            Constraint::Length(1),                           // separator
            Constraint::Min(1),                              // option list
            Constraint::Length(1),                            // help
        ])
        .split(inner);

    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("Pull failed on ", Style::default().fg(Color::Red)),
            Span::styled(
                branch,
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
        ])),
        chunks[0],
    );

    // Render error message (truncated to fit)
    let display_text: String = error_lines
        .iter()
        .take(8)
        .map(|l| if l.len() > wrap_width { &l[..wrap_width] } else { l })
        .collect::<Vec<&str>>()
        .join("\n");
    f.render_widget(
        Paragraph::new(display_text)
            .style(Style::default().fg(Color::Yellow)),
        chunks[2],
    );

    let items: Vec<ListItem> = options
        .iter()
        .enumerate()
        .map(|(idx, label)| {
            let is_sel = idx == selected;
            let marker = if is_sel { ">" } else { " " };
            let style = if idx == options.len() - 1 {
                // Dismiss option in gray
                if is_sel {
                    Style::default()
                        .fg(Color::DarkGray)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::DarkGray)
                }
            } else if is_sel {
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            };
            ListItem::new(Line::from(Span::styled(
                format!(" {} {}", marker, label),
                style,
            )))
        })
        .collect();

    let list = List::new(items);
    f.render_widget(list, chunks[4]);

    f.render_widget(
        Paragraph::new("j/k: select  Enter: confirm  Esc: dismiss")
            .style(Style::default().fg(Color::DarkGray)),
        chunks[5],
    );
}

fn draw_auth_error(f: &mut Frame, app: &App, operation: &str, error_message: &str, selected: usize) {
    // Calculate error display lines (wrap at ~56 chars to fit in dialog)
    let wrap_width = 56usize;
    let error_lines: Vec<&str> = error_message.lines().collect();
    let mut wrapped_count = 0usize;
    for line in &error_lines {
        if line.is_empty() {
            wrapped_count += 1;
        } else {
            wrapped_count += line.len().div_ceil(wrap_width);
        }
    }
    let error_display_lines = wrapped_count.min(8);

    // header(1) + sep(1) + error + sep(1) + instructions(1) + hints(3) + sep(1) + option(1) + help(1)
    let height = (error_display_lines as u16) + 12;
    let area = centered_rect(60, height, f.area());
    app.areas.dialog.set(area);
    f.render_widget(Clear, area);

    let block = Block::default()
        .title(" Authentication Required ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::DIALOG_WARNING));

    let inner = block.inner(area);
    f.render_widget(block, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),                           // header
            Constraint::Length(1),                           // separator
            Constraint::Length(error_display_lines as u16),  // error message
            Constraint::Length(1),                           // separator
            Constraint::Length(1),                           // instructions
            Constraint::Length(3),                           // auth hints
            Constraint::Length(1),                           // separator
            Constraint::Length(1),                           // dismiss option
            Constraint::Length(1),                           // help
        ])
        .split(inner);

    let op_capitalized = match operation {
        "push" => "Push",
        "pull" => "Pull",
        "clone" => "Clone",
        _ => operation,
    };
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                format!("{} failed", op_capitalized),
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                " — git authentication needed",
                Style::default().fg(Color::Yellow),
            ),
        ])),
        chunks[0],
    );

    // Render error message (truncated to fit)
    let display_text: String = error_lines
        .iter()
        .take(8)
        .map(|l| if l.len() > wrap_width { &l[..wrap_width] } else { l })
        .collect::<Vec<&str>>()
        .join("\n");
    f.render_widget(
        Paragraph::new(display_text)
            .style(Style::default().fg(Color::DarkGray)),
        chunks[2],
    );

    f.render_widget(
        Paragraph::new("Authenticate outside ClawTree, then retry:")
            .style(Style::default().fg(Color::White)),
        chunks[4],
    );

    let hints = vec![
        Line::from(Span::styled(
            "  - gh auth login (GitHub CLI)",
            Style::default().fg(Color::Gray),
        )),
        Line::from(Span::styled(
            "  - SSH keys: ssh-add ~/.ssh/id_*",
            Style::default().fg(Color::Gray),
        )),
        Line::from(Span::styled(
            "  - git credential helper / personal access token",
            Style::default().fg(Color::Gray),
        )),
    ];
    f.render_widget(Paragraph::new(hints), chunks[5]);

    // Dismiss option
    let is_sel = selected == 0;
    let marker = if is_sel { ">" } else { " " };
    let style = if is_sel {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::White)
    };
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            format!(" {} Dismiss", marker),
            style,
        ))),
        chunks[7],
    );

    f.render_widget(
        Paragraph::new("Enter: dismiss  Esc: dismiss")
            .style(Style::default().fg(Color::DarkGray)),
        chunks[8],
    );
}

fn draw_rename_session(f: &mut Frame, app: &App, name: &str) {
    let area = centered_rect(50, 6, f.area());
    app.areas.dialog.set(area);
    f.render_widget(Clear, area);

    let block = Block::default()
        .title(" Rename Session ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::DIALOG_NEUTRAL));

    let inner = block.inner(area);
    f.render_widget(block, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(inner);

    f.render_widget(
        Paragraph::new("Nickname (empty to clear):")
            .style(Style::default().fg(Color::Gray)),
        chunks[0],
    );

    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            format!("{}_", name),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ))),
        chunks[1],
    );

    f.render_widget(
        Paragraph::new("Enter: save  Esc: cancel")
            .style(Style::default().fg(Color::DarkGray)),
        chunks[3],
    );
}

fn draw_dirty_worktree(
    f: &mut Frame,
    app: &App,
    branch: &str,
    files: &[(String, String)],
    selected: usize,
) {
    let options = ["Commit changes", "Open with Claude", "Ignore", "Cancel"];
    let visible_files = files.len().min(8);
    // title(1) + info(1) + blank(1) + files + blank(1) + options(3) + blank(1) + help(1)
    let height = (visible_files as u16) + (options.len() as u16) + 6;
    let area = centered_rect(55, height, f.area());
    app.areas.dialog.set(area);
    f.render_widget(Clear, area);

    let block = Block::default()
        .title(" Uncommitted Changes ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::DIALOG_WARNING));

    let inner = block.inner(area);
    f.render_widget(block, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),                     // info line
            Constraint::Length(1),                     // blank
            Constraint::Length(visible_files as u16),  // file list
            Constraint::Length(1),                     // blank
            Constraint::Length(options.len() as u16),  // options
            Constraint::Length(1),                     // help
        ])
        .split(inner);

    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("'", Style::default().fg(Color::Gray)),
            Span::styled(
                branch,
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("' has uncommitted changes:", Style::default().fg(Color::Gray)),
        ])),
        chunks[0],
    );

    // File list (read-only)
    let file_items: Vec<ListItem> = files
        .iter()
        .take(visible_files)
        .map(|(status, path)| {
            ListItem::new(Line::from(vec![
                Span::styled(
                    format!(" {}  ", status),
                    Style::default().fg(Color::Yellow),
                ),
                Span::styled(path.as_str(), Style::default().fg(Color::White)),
            ]))
        })
        .collect();
    f.render_widget(List::new(file_items), chunks[2]);

    // Options
    let option_items: Vec<ListItem> = options
        .iter()
        .enumerate()
        .map(|(idx, label)| {
            let is_sel = idx == selected;
            let marker = if is_sel { ">" } else { " " };
            let style = if idx == options.len() - 1 {
                if is_sel {
                    Style::default()
                        .fg(Color::Red)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::Red)
                }
            } else if is_sel {
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            };
            ListItem::new(Line::from(Span::styled(
                format!(" {} {}", marker, label),
                style,
            )))
        })
        .collect();
    f.render_widget(List::new(option_items), chunks[4]);

    f.render_widget(
        Paragraph::new("j/k: select  Enter: confirm  Esc: cancel")
            .style(Style::default().fg(Color::DarkGray)),
        chunks[5],
    );
}

fn draw_merge_success(f: &mut Frame, app: &App, message: &str) {
    // title(1) + message(1) + blank(1) + ok(1) + help(1)
    let height = 5;
    let area = centered_rect(50, height, f.area());
    app.areas.dialog.set(area);
    f.render_widget(Clear, area);

    let block = Block::default()
        .title(" Merge Complete ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Green));

    let inner = block.inner(area);
    f.render_widget(block, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // message
            Constraint::Length(1), // blank
            Constraint::Length(1), // ok
            Constraint::Length(1), // help
        ])
        .split(inner);

    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            message,
            Style::default().fg(Color::White),
        ))),
        chunks[0],
    );

    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            " > OK",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ))),
        chunks[2],
    );

    f.render_widget(
        Paragraph::new("Enter/Esc: dismiss")
            .style(Style::default().fg(Color::DarkGray)),
        chunks[3],
    );
}

#[allow(clippy::too_many_arguments)]
fn draw_git_commit(
    f: &mut Frame,
    app: &App,
    branch: &str,
    unstaged: &[(char, String)],
    staged: &[(char, String)],
    section: usize,
    selected: usize,
    phase: CommitPhase,
    commit_message: &str,
    cursor_pos: usize,
) {
    match phase {
        CommitPhase::Staging => {
            draw_git_commit_staging(f, app, branch, unstaged, staged, section, selected);
        }
        CommitPhase::GeneratingMessage => {
            draw_git_commit_generating(f, app, branch, staged);
        }
        CommitPhase::Message => {
            draw_git_commit_message(f, app, branch, staged, commit_message, cursor_pos);
        }
    }
}

fn draw_git_commit_staging(
    f: &mut Frame,
    app: &App,
    branch: &str,
    unstaged: &[(char, String)],
    staged: &[(char, String)],
    section: usize,
    selected: usize,
) {
    // Use up to 80% of terminal height, leaving room for surrounding UI
    let max_height = (f.area().height as f32 * 0.8) as u16;
    // Fixed rows: unstaged header(1) + staged header(1) + help(1) + borders(2) = 5
    let fixed_rows: u16 = 5;
    let available_for_files = max_height.saturating_sub(fixed_rows);

    // Split available space between the two sections
    let unstaged_visible = if unstaged.is_empty() {
        0
    } else if staged.is_empty() {
        (unstaged.len() as u16).min(available_for_files)
    } else {
        // Give each section at least 1 row, then split remaining proportionally
        let half = available_for_files / 2;
        (unstaged.len() as u16).min(half.max(3))
    };
    let staged_visible = if staged.is_empty() {
        0
    } else {
        let remaining = available_for_files.saturating_sub(unstaged_visible);
        (staged.len() as u16).min(remaining.max(3))
    };

    // Reclaim unused space from whichever section didn't need it all
    let unstaged_visible = {
        let staged_unused = available_for_files.saturating_sub(unstaged_visible).saturating_sub(staged_visible);
        (unstaged.len() as u16).min(unstaged_visible + staged_unused)
    };

    let height = unstaged_visible + staged_visible + 3 + 2; // +3 for headers+help, +2 for borders
    let area = centered_rect(60, height, f.area());
    app.areas.dialog.set(area);
    f.render_widget(Clear, area);

    let block = Block::default()
        .title(format!(" Commit — {} ", branch))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::DIALOG_CREATION));

    let inner = block.inner(area);
    f.render_widget(block, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),                       // unstaged header
            Constraint::Length(unstaged_visible),        // unstaged files
            Constraint::Length(1),                       // staged header
            Constraint::Length(staged_visible),          // staged files
            Constraint::Length(1),                       // help
        ])
        .split(inner);

    // Scroll offset for the active section
    let unstaged_scroll = if section == 0 && unstaged_visible > 0 {
        scroll_offset_for(selected, unstaged_visible as usize)
    } else {
        0
    };
    let staged_scroll = if section == 1 && staged_visible > 0 {
        scroll_offset_for(selected, staged_visible as usize)
    } else {
        0
    };

    // Unstaged header
    let unstaged_hdr_style = if section == 0 {
        Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let unstaged_scroll_hint = if unstaged.len() as u16 > unstaged_visible {
        format!(" [{}/{}]", (selected + 1).min(unstaged.len()), unstaged.len())
    } else {
        String::new()
    };
    f.render_widget(
        Paragraph::new(format!("── Unstaged ({}) ──{}", unstaged.len(), if section == 0 { &unstaged_scroll_hint } else { "" }))
            .style(unstaged_hdr_style),
        chunks[0],
    );

    // Unstaged files (with scroll)
    let unstaged_items: Vec<ListItem> = unstaged
        .iter()
        .enumerate()
        .skip(unstaged_scroll)
        .take(unstaged_visible as usize)
        .map(|(idx, (status, path))| {
            let is_sel = section == 0 && idx == selected;
            let marker = if is_sel { ">" } else { " " };
            let style = if is_sel {
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            };
            ListItem::new(Line::from(Span::styled(
                format!(" {} {} {}", marker, status, path),
                style,
            )))
        })
        .collect();
    f.render_widget(List::new(unstaged_items), chunks[1]);

    // Staged header
    let staged_hdr_style = if section == 1 {
        Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let staged_scroll_hint = if staged.len() as u16 > staged_visible {
        format!(" [{}/{}]", (selected + 1).min(staged.len()), staged.len())
    } else {
        String::new()
    };
    f.render_widget(
        Paragraph::new(format!("── Staged ({}) ──{}", staged.len(), if section == 1 { &staged_scroll_hint } else { "" }))
            .style(staged_hdr_style),
        chunks[2],
    );

    // Staged files (with scroll)
    let staged_items: Vec<ListItem> = staged
        .iter()
        .enumerate()
        .skip(staged_scroll)
        .take(staged_visible as usize)
        .map(|(idx, (status, path))| {
            let is_sel = section == 1 && idx == selected;
            let marker = if is_sel { ">" } else { " " };
            let style = if is_sel {
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            };
            ListItem::new(Line::from(Span::styled(
                format!(" {} {} {}", marker, status, path),
                style,
            )))
        })
        .collect();
    f.render_widget(List::new(staged_items), chunks[3]);

    f.render_widget(
        Paragraph::new("Space: stage/unstage  a: all  c: commit  ^c: AI commit  ^a: all+AI  Esc: cancel")
            .style(Style::default().fg(Color::DarkGray)),
        chunks[4],
    );
}

fn draw_git_commit_generating(
    f: &mut Frame,
    app: &App,
    branch: &str,
    staged: &[(char, String)],
) {
    let max_height = (f.area().height as f32 * 0.6) as u16;
    let fixed_rows = 5; // staged header + blank + spinner + help + borders
    let staged_visible = (staged.len() as u16).min(max_height.saturating_sub(fixed_rows));
    let height = staged_visible + fixed_rows;
    let area = centered_rect(60, height, f.area());
    app.areas.dialog.set(area);
    f.render_widget(Clear, area);

    let block = Block::default()
        .title(format!(" Commit — {} ", branch))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::DIALOG_CREATION));

    let inner = block.inner(area);
    f.render_widget(block, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),                // staged header
            Constraint::Length(staged_visible),   // staged files
            Constraint::Length(1),                // blank
            Constraint::Length(1),                // spinner
            Constraint::Length(1),                // help
        ])
        .split(inner);

    f.render_widget(
        Paragraph::new(format!("── Staged ({}) ──", staged.len()))
            .style(Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)),
        chunks[0],
    );

    let staged_items: Vec<ListItem> = staged
        .iter()
        .take(staged_visible as usize)
        .map(|(status, path)| {
            ListItem::new(Line::from(Span::styled(
                format!("  {} {}", status, path),
                Style::default().fg(Color::White),
            )))
        })
        .collect();
    f.render_widget(List::new(staged_items), chunks[1]);

    let spinner_idx = (app.spinner_frame / 3) % theme::SPINNER_FRAMES.len();
    let spinner = theme::SPINNER_FRAMES[spinner_idx];
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            format!("{} Generating commit message with Claude...", spinner),
            Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        ))),
        chunks[3],
    );

    f.render_widget(
        Paragraph::new("Esc: cancel")
            .style(Style::default().fg(Color::DarkGray)),
        chunks[4],
    );
}

/// Calculate scroll offset to keep `selected` visible within `visible_count` rows.
fn scroll_offset_for(selected: usize, visible_count: usize) -> usize {
    if visible_count == 0 {
        return 0;
    }
    if selected >= visible_count {
        selected - visible_count + 1
    } else {
        0
    }
}

fn draw_git_commit_message(
    f: &mut Frame,
    app: &App,
    branch: &str,
    staged: &[(char, String)],
    commit_message: &str,
    cursor_pos: usize,
) {
    let max_height = (f.area().height as f32 * 0.8) as u16;
    // Count message lines (at least 1 for the cursor).
    // A trailing newline means there's an empty line after it.
    let msg_line_count = if commit_message.is_empty() {
        1
    } else if commit_message.ends_with('\n') {
        commit_message.lines().count() as u16 + 1
    } else {
        commit_message.lines().count().max(1) as u16
    };
    let max_msg_lines = max_height.saturating_sub(7); // reserve for header, staged, label, help, borders
    let msg_visible = msg_line_count.min(max_msg_lines).max(3); // at least 3 lines for the message area
    let fixed_rows = 5; // staged header + blank + label + help + borders
    let staged_visible = (staged.len() as u16).min(max_height.saturating_sub(fixed_rows + msg_visible));
    let height = staged_visible + msg_visible + fixed_rows;
    let area = centered_rect(60, height, f.area());
    app.areas.dialog.set(area);
    f.render_widget(Clear, area);

    let block = Block::default()
        .title(format!(" Commit — {} ", branch))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::DIALOG_CREATION));

    let inner = block.inner(area);
    f.render_widget(block, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),                     // staged header
            Constraint::Length(staged_visible),        // staged files
            Constraint::Length(1),                     // blank/label
            Constraint::Length(msg_visible),           // message area
            Constraint::Length(1),                     // help
        ])
        .split(inner);

    f.render_widget(
        Paragraph::new(format!("── Staged ({}) ──", staged.len()))
            .style(Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)),
        chunks[0],
    );

    let staged_items: Vec<ListItem> = staged
        .iter()
        .take(staged_visible as usize)
        .map(|(status, path)| {
            ListItem::new(Line::from(Span::styled(
                format!("  {} {}", status, path),
                Style::default().fg(Color::White),
            )))
        })
        .collect();
    f.render_widget(List::new(staged_items), chunks[1]);

    f.render_widget(
        Paragraph::new("Commit message (Alt+Enter: newline):")
            .style(Style::default().fg(Color::Gray)),
        chunks[2],
    );

    // Calculate cursor line and column from cursor_pos
    let mut cursor_line: u16 = 0;
    let mut cursor_col: u16 = 0;
    for (i, ch) in commit_message.chars().enumerate() {
        if i == cursor_pos {
            break;
        }
        if ch == '\n' {
            cursor_line += 1;
            cursor_col = 0;
        } else {
            cursor_col += 1;
        }
    }

    // Scroll to keep cursor visible
    let scroll = if cursor_line >= msg_visible {
        cursor_line - msg_visible + 1
    } else {
        0
    };

    // Build message lines
    let msg_lines: Vec<Line> = if commit_message.is_empty() {
        vec![Line::from("")]
    } else {
        let mut lines: Vec<Line> = commit_message
            .split('\n')
            .map(|l| Line::from(Span::styled(l.to_string(), Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))))
            .collect();
        if lines.is_empty() {
            lines.push(Line::from(""));
        }
        lines
    };

    f.render_widget(
        Paragraph::new(msg_lines).scroll((scroll, 0)),
        chunks[3],
    );

    // Place the terminal cursor at the correct position
    let msg_area = chunks[3];
    let screen_line = cursor_line.saturating_sub(scroll);
    if screen_line < msg_visible {
        f.set_cursor_position((
            msg_area.x + cursor_col.min(msg_area.width.saturating_sub(1)),
            msg_area.y + screen_line,
        ));
    }

    f.render_widget(
        Paragraph::new("Enter: commit  ^p: commit+push  ^g: AI msg  ^l: new line  Esc: back")
            .style(Style::default().fg(Color::DarkGray)),
        chunks[4],
    );
}

#[allow(clippy::too_many_arguments)]
fn draw_convert_repo(
    f: &mut Frame,
    app: &App,
    source_path: &Path,
    mode: usize,
    target_path: &str,
    branch: &str,
    focused: usize,
    confirmed: bool,
) {
    // Confirmation screen: show warning and ask user to press Enter again
    if confirmed {
        let area = centered_rect(60, 11, f.area());
        app.areas.dialog.set(area);
        f.render_widget(Clear, area);

        let block = Block::default()
            .title(" Confirm Conversion ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Yellow));
        let inner = block.inner(area);
        f.render_widget(block, area);

        let warn_style = Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD);
        let text_style = Style::default().fg(Color::Gray);
        let key_style = Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD);

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1), // warning icon + title
                Constraint::Length(1), // spacer
                Constraint::Length(1), // line 1
                Constraint::Length(1), // line 2
                Constraint::Length(1), // line 3
                Constraint::Length(1), // spacer
                Constraint::Length(1), // line 4
                Constraint::Length(1), // spacer
                Constraint::Length(1), // help
            ])
            .split(inner);

        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "WARNING: This will restructure your project files.",
                warn_style,
            ))),
            chunks[0],
        );
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "Your working tree will be moved into a subdirectory",
                text_style,
            ))),
            chunks[2],
        );
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!("and checked out as a worktree at ./{}/", branch),
                text_style,
            ))),
            chunks[3],
        );
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "Make sure to backup your project or push your changes.",
                text_style,
            ))),
            chunks[4],
        );
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("Source: ", text_style),
                Span::styled(source_path.display().to_string(), Style::default().fg(Color::White)),
            ])),
            chunks[6],
        );
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("Enter", key_style),
                Span::styled(":confirm  ", text_style),
                Span::styled("Esc", key_style),
                Span::styled(":cancel", text_style),
            ])),
            chunks[8],
        );

        return;
    }

    let height: u16 = if mode == 1 { 15 } else { 12 };
    let area = centered_rect(60, height, f.area());
    app.areas.dialog.set(area);
    f.render_widget(Clear, area);

    let block = Block::default()
        .title(" Convert Existing Repo ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Green));

    let inner = block.inner(area);
    f.render_widget(block, area);

    let mut constraints = vec![
        Constraint::Length(1), // source label
        Constraint::Length(1), // source path
        Constraint::Length(1), // spacer
        Constraint::Length(1), // mode label
        Constraint::Length(1), // mode selector
        Constraint::Length(1), // spacer
    ];

    if mode == 1 {
        constraints.push(Constraint::Length(1)); // target label
        constraints.push(Constraint::Length(1)); // target input
        constraints.push(Constraint::Length(1)); // spacer
    }

    constraints.push(Constraint::Length(1)); // branch label
    constraints.push(Constraint::Length(1)); // branch input
    constraints.push(Constraint::Length(1)); // spacer
    constraints.push(Constraint::Length(1)); // help

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(inner);

    let mut idx = 0;

    // Source path (read-only)
    f.render_widget(
        Paragraph::new("Source repository:")
            .style(Style::default().fg(Color::Gray)),
        chunks[idx],
    );
    idx += 1;

    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            source_path.display().to_string(),
            Style::default().fg(Color::White),
        ))),
        chunks[idx],
    );
    idx += 2; // skip spacer

    let label_style = Style::default().fg(Color::Gray);
    let value_style = Style::default().fg(Color::Cyan);
    let cursor = "> ";
    let no_cursor = "  ";

    // Mode selector
    let mode_prefix = if focused == 0 { cursor } else { no_cursor };
    f.render_widget(
        Paragraph::new(format!("{}Conversion mode (Left/Right to toggle):", mode_prefix))
            .style(label_style),
        chunks[idx],
    );
    idx += 1;

    let inplace_marker = if mode == 0 { "[x]" } else { "[ ]" };
    let location_marker = if mode == 1 { "[x]" } else { "[ ]" };
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            format!("{}{} In-place    {} Different location", no_cursor, inplace_marker, location_marker),
            value_style,
        ))),
        chunks[idx],
    );
    idx += 2; // skip spacer

    // Target path (only if mode == 1)
    if mode == 1 {
        let target_prefix = if focused == 1 { cursor } else { no_cursor };
        f.render_widget(
            Paragraph::new(format!("{}Target directory:", target_prefix))
                .style(label_style),
            chunks[idx],
        );
        idx += 1;

        let target_display = if focused == 1 {
            if target_path.is_empty() { "_".to_string() } else { format!("{}_", target_path) }
        } else {
            target_path.to_string()
        };
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!("{}{}", no_cursor, target_display),
                value_style,
            ))),
            chunks[idx],
        );
        idx += 2; // skip spacer
    }

    // Branch name
    let branch_prefix = if focused == 2 { cursor } else { no_cursor };
    f.render_widget(
        Paragraph::new(format!("{}Branch name:", branch_prefix))
            .style(label_style),
        chunks[idx],
    );
    idx += 1;

    let branch_display = if focused == 2 {
        format!("{}_", branch)
    } else {
        branch.to_string()
    };
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            format!("{}{}", no_cursor, branch_display),
            value_style,
        ))),
        chunks[idx],
    );
    idx += 2; // skip spacer

    // Help line
    f.render_widget(
        Paragraph::new("Tab:next  Left/Right:mode  Enter:convert  Esc:cancel")
            .style(Style::default().fg(Color::DarkGray)),
        chunks[idx],
    );
}

fn draw_context_menu(
    f: &mut Frame,
    app: &App,
    item: &SidebarItem,
    selected: usize,
    actions: &[ContextAction],
) {
    // Determine title from the item
    let title = match item {
        SidebarItem::Worktree(wi) => {
            app.worktrees.get(*wi)
                .map(|wt| format!(" {} ", wt.branch))
                .unwrap_or_else(|| " Actions ".to_string())
        }
        SidebarItem::Session(wi, si) => {
            app.worktrees.get(*wi)
                .and_then(|wt| wt.session_ids.get(*si))
                .and_then(|sid| app.sessions.get(sid))
                .and_then(|s| s.nickname.clone().or_else(|| s.terminal_title()).or_else(|| Some(s.label.clone())))
                .map(|n| format!(" {} ", n))
                .unwrap_or_else(|| " Actions ".to_string())
        }
        SidebarItem::Terminal(ti) => {
            app.terminal_ids.get(*ti)
                .and_then(|sid| app.sessions.get(sid))
                .and_then(|s| s.nickname.clone().or_else(|| Some(s.label.clone())))
                .map(|n| format!(" {} ", n))
                .unwrap_or_else(|| " Actions ".to_string())
        }
    };

    // Height: borders(2) + actions + footer(1)
    let height = (actions.len() as u16) + 4;
    let area = centered_rect(50, height, f.area());
    app.areas.dialog.set(area);
    f.render_widget(Clear, area);

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::DIALOG_NEUTRAL));

    let inner = block.inner(area);
    f.render_widget(block, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),    // action list
            Constraint::Length(1), // help
        ])
        .split(inner);

    let inner_w = chunks[0].width as usize;

    let items: Vec<ListItem> = actions
        .iter()
        .enumerate()
        .map(|(idx, action)| {
            let is_sel = idx == selected;
            let is_destructive = matches!(
                action.kind,
                ContextActionKind::DeleteWorktree
                | ContextActionKind::ForceDeleteWorktree
                | ContextActionKind::DeleteSession
            );

            let marker = if is_sel { " > " } else { "   " };
            let label = &action.label;
            let hotkey = &action.hotkey;

            // Right-align the hotkey hint
            let padding = inner_w.saturating_sub(marker.len() + label.len() + hotkey.len() + 3);
            let padded = " ".repeat(padding);

            let fg = if is_destructive {
                Color::Red
            } else if is_sel {
                Color::Cyan
            } else {
                Color::White
            };
            let bold_mod = if is_sel { Modifier::BOLD } else { Modifier::empty() };

            ListItem::new(Line::from(vec![
                Span::styled(marker.to_string(), Style::default().fg(fg).add_modifier(bold_mod)),
                Span::styled(label.clone(), Style::default().fg(fg).add_modifier(bold_mod)),
                Span::styled(padded, Style::default()),
                Span::styled(format!("[{}]", hotkey), Style::default().fg(Color::DarkGray)),
            ]))
        })
        .collect();

    let list = List::new(items);
    f.render_widget(list, chunks[0]);

    f.render_widget(
        Paragraph::new("j/k: navigate  Enter: select  Esc: close")
            .style(Style::default().fg(Color::DarkGray)),
        chunks[1],
    );
}

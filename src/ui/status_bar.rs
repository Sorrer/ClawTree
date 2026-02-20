use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::{App, InputMode, ScreenMode, StatusSeverity};
use super::theme;

pub fn draw(f: &mut Frame, app: &App, area: Rect) {
    let mode_text = match app.screen_mode {
        ScreenMode::Mini => "MINI",
        ScreenMode::MiniDrilldown => "MINI/DRILL",
        ScreenMode::Normal => match app.input_mode {
            InputMode::Normal => if app.prompt_queue_focused() {
                "Prompt Queue"
            } else {
                "Worktrees"
            },
            InputMode::Terminal => "Claude Code",
            InputMode::Dialog => "DIALOG",
        },
    };

    let wide = area.width > 120;
    let help_text: &str = if app.screen_mode == ScreenMode::Mini && app.mini.focus == crate::app::MiniModeFocus::DetailInput {
        if wide {
            "Tab:back to tree  Enter:send  Alt+Enter:newline  F2:normal  ?:help"
        } else {
            "Tab:tree Enter:send F2:normal ?:help"
        }
    } else if app.screen_mode == ScreenMode::Mini {
        if wide {
            "j/k:navigate  Tab:input  o:terminal  a:new agent  d:kill  r:rename  F2:normal  ?:help"
        } else {
            "j/k:nav Tab:input a:new d:kill F2:normal ?:help"
        }
    } else if app.screen_mode == ScreenMode::MiniDrilldown {
        if wide {
            "Tab:back to agents  F2:normal mode  ^q:quit  (keys forwarded to terminal)"
        } else {
            "Tab:back F2:normal ^q:quit"
        }
    } else if !app.repo_detected && app.input_mode != InputMode::Dialog {
        if app.regular_repo_path.is_some() {
            if wide { "i:init repo  c:convert  ?:help  ^q:quit" } else { "i:init c:convert ?:help ^q:quit" }
        } else {
            if wide { "i:init repo  ?:help  ^q:quit" } else { "i:init-repo ?:help ^q:quit" }
        }
    } else if app.prompt_queue_focused() {
        if wide {
            "Enter:add/edit  d:delete  \u{2191}\u{2193}:navigate  Esc:back  ^p:hide queue"
        } else {
            "Enter:add/edit d:del \u{2191}\u{2193}:nav Esc:back ^p:hide"
        }
    } else {
        match app.input_mode {
            InputMode::Normal => {
                match app.selected_sidebar_item() {
                    Some(crate::app::SidebarItem::Session(_, _)) => {
                        if wide {
                            "Enter:switch session  c:new claude  d:delete  ^p:prompt queue  ?:help  ^q:quit"
                        } else {
                            "Enter:switch c:claude d:del ^p:queue ?:help ^q:quit"
                        }
                    }
                    Some(crate::app::SidebarItem::Worktree(_)) => {
                        if wide {
                            "c:new session  C:yolo  n:new worktree  d:delete  m:merge branch  ?:help  ^q:quit"
                        } else {
                            "c:claude C:yolo n:new-wt d:del m:merge ?:help ^q:quit"
                        }
                    }
                    None => {
                        if wide { "n:new worktree  ^b:sidebar  ?:help  ^q:quit" } else { "n:new-wt ^b:sidebar ?:help ^q:quit" }
                    }
                }
            }
            InputMode::Terminal => {
                if wide {
                    "Tab:escape to sidebar  ^p:prompt queue  ^b:toggle sidebar  ^q:quit"
                } else {
                    "Tab:sidebar ^p:queue ^b:toggle-sidebar ^q:quit"
                }
            }
            InputMode::Dialog => {
                if wide { "Enter:confirm  Esc:cancel  Tab:next field" } else { "Enter:confirm Esc:cancel Tab:next-field" }
            }
        }
    };

    // Status message with severity coloring + auto-fade (dim after 10s, hidden after 30s)
    let severity_style = |sev: StatusSeverity| -> Style {
        match sev {
            StatusSeverity::Info => Style::default().fg(theme::STATUS_INFO),
            StatusSeverity::Success => Style::default().fg(theme::STATUS_SUCCESS),
            StatusSeverity::Warning => Style::default().fg(theme::STATUS_WARNING).add_modifier(Modifier::BOLD),
            StatusSeverity::Error => Style::default().fg(theme::STATUS_ERROR).add_modifier(Modifier::BOLD),
        }
    };
    let (status, status_style) = match (&app.status_message, app.status_message_time) {
        (Some(msg), Some(t)) => {
            let elapsed = t.elapsed().as_secs();
            if elapsed >= 30 {
                ("", Style::default())
            } else if elapsed >= 10 {
                (msg.as_str(), Style::default().fg(theme::STATUS_FADED))
            } else {
                (msg.as_str(), severity_style(app.status_severity))
            }
        }
        (Some(msg), None) => (msg.as_str(), severity_style(app.status_severity)),
        _ => ("", Style::default()),
    };

    let mode_bg = match app.screen_mode {
        ScreenMode::Mini | ScreenMode::MiniDrilldown => theme::MODE_MINI_BG,
        ScreenMode::Normal => match app.input_mode {
            InputMode::Normal => if app.prompt_queue_focused() {
                theme::BORDER_FOCUSED_PROMPT_QUEUE
            } else {
                theme::MODE_NORMAL_BG
            },
            InputMode::Terminal => theme::MODE_TERMINAL_BG,
            InputMode::Dialog => theme::MODE_DIALOG_BG,
        },
    };

    let line = Line::from(vec![
        Span::styled(
            format!(" {} ", theme::LOGO_SMALL),
            Style::default()
                .fg(theme::BRAND_CLAW)
                .bg(theme::STATUS_BAR_BG)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(" {} ", mode_text),
            Style::default()
                .fg(Color::Black)
                .bg(mode_bg)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled(help_text, Style::default().fg(Color::DarkGray)),
        if !status.is_empty() {
            Span::styled(format!(" | {}", status), status_style)
        } else {
            Span::raw("")
        },
    ]);

    let paragraph = Paragraph::new(line)
        .style(Style::default().bg(theme::STATUS_BAR_BG));
    f.render_widget(paragraph, area);
}

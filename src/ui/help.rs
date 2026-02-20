use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Tabs};

use crate::app::App;
use crate::keys::{KeyContext, KeyEntry};

pub fn draw(f: &mut Frame, app: &App) {
    let area = f.area();
    // Use 70% width, up to 80% height
    let width = (area.width as f32 * 0.7) as u16;
    let max_height = (area.height as f32 * 0.8) as u16;

    let ctx = KeyContext::ALL[app.help_tab];
    let keys = ctx.keys();
    let extra = ctx.extra_keys(app.wt_available);
    let display_rows = ctx.display_row_count(app.wt_available);
    // tabs(1) + separator(1) + display_rows + separator(1) + help(1) + borders(2)
    let content_height = (display_rows as u16) + 6;
    let height = content_height.min(max_height).max(10);

    let popup = centered_rect(width, height, area);
    app.areas.help_overlay.set(popup);
    f.render_widget(Clear, popup);

    let block = Block::default()
        .title(" Hotkeys ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));

    let inner = block.inner(popup);
    f.render_widget(block, popup);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // tabs
            Constraint::Length(1), // separator
            Constraint::Min(1),   // key list
            Constraint::Length(1), // separator
            Constraint::Length(1), // footer help
        ])
        .split(inner);

    // Tab bar
    let titles: Vec<Line> = KeyContext::ALL
        .iter()
        .enumerate()
        .map(|(i, kc)| {
            if i == app.help_tab {
                Line::from(Span::styled(
                    format!(" {} ", kc.label()),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
                ))
            } else {
                Line::from(Span::styled(
                    format!(" {} ", kc.label()),
                    Style::default().fg(Color::DarkGray),
                ))
            }
        })
        .collect();

    let tabs = Tabs::new(titles)
        .style(Style::default().fg(Color::White))
        .highlight_style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
        .select(app.help_tab)
        .divider(Span::styled("|", Style::default().fg(Color::DarkGray)));

    f.render_widget(tabs, chunks[0]);

    // Separator line
    let sep = Paragraph::new(Line::from(Span::styled(
        "─".repeat(chunks[1].width as usize),
        Style::default().fg(Color::DarkGray),
    )));
    f.render_widget(sep, chunks[1]);

    // Key list
    let list_area = chunks[2];
    let visible_rows = list_area.height as usize;

    let all_keys: Vec<&KeyEntry> = keys.iter().chain(extra.iter()).collect();
    // key_col_width: skip section headers (empty key string)
    let key_col_width = all_keys
        .iter()
        .filter(|(k, _)| !k.is_empty())
        .map(|(k, _)| k.len())
        .max()
        .unwrap_or(12)
        + 2;

    // Build display rows: section headers get a blank line before them
    // (except the very first entry).
    let mut display_items: Vec<ListItem> = Vec::new();
    let mut first_section = true;
    for (key, desc) in &all_keys {
        if key.is_empty() {
            // Section header
            if first_section {
                first_section = false;
            } else {
                display_items.push(ListItem::new(Line::from("")));
            }
            display_items.push(ListItem::new(Line::from(Span::styled(
                format!("  {}", desc),
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
            ))));
        } else {
            display_items.push(ListItem::new(Line::from(vec![
                Span::styled(
                    format!("  {:width$}", key, width = key_col_width),
                    Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
                ),
                Span::styled(*desc, Style::default().fg(Color::White)),
            ])));
        }
    }

    let scroll = app.help_scroll;
    let visible: Vec<ListItem> = display_items
        .into_iter()
        .skip(scroll)
        .take(visible_rows)
        .collect();

    f.render_widget(List::new(visible), list_area);

    // Separator
    let sep2 = Paragraph::new(Line::from(Span::styled(
        "─".repeat(chunks[3].width as usize),
        Style::default().fg(Color::DarkGray),
    )));
    f.render_widget(sep2, chunks[3]);

    // Footer help
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("  h/l", Style::default().fg(Color::Yellow)),
            Span::styled(": switch tab  ", Style::default().fg(Color::DarkGray)),
            Span::styled("j/k", Style::default().fg(Color::Yellow)),
            Span::styled(": scroll  ", Style::default().fg(Color::DarkGray)),
            Span::styled("1-6", Style::default().fg(Color::Yellow)),
            Span::styled(": jump to tab  ", Style::default().fg(Color::DarkGray)),
            Span::styled("? / Esc", Style::default().fg(Color::Yellow)),
            Span::styled(": close", Style::default().fg(Color::DarkGray)),
        ])),
        chunks[4],
    );
}

fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let x = area.x + area.width.saturating_sub(width) / 2;
    let y = area.y + area.height.saturating_sub(height) / 2;
    Rect::new(x, y, width.min(area.width), height.min(area.height))
}

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};

use crate::app::App;
use super::theme;

pub fn draw(f: &mut Frame, app: &App, area: Rect) {
    let focused = app.prompt_queue_focused();
    let queue = app.active_prompt_queue();
    let queue_len = queue.len();

    let border_color = if focused { theme::BORDER_FOCUSED_PROMPT_QUEUE } else { theme::BORDER_UNFOCUSED };
    let border_mod = if focused { Modifier::BOLD } else { Modifier::empty() };
    let border_type = if focused { BorderType::Thick } else { BorderType::Plain };
    let title = if focused {
        format!(" ▸ Prompt Queue ({}) ", queue_len)
    } else {
        format!(" Prompt Queue ({}) ", queue_len)
    };
    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_type(border_type)
        .border_style(Style::default().fg(border_color).add_modifier(border_mod));

    let inner = block.inner(area);
    f.render_widget(block, area);

    if inner.height == 0 || inner.width == 0 {
        return;
    }

    let mut lines: Vec<Line> = Vec::new();

    // Render queue items (reserve last line for input)
    let max_items = inner.height.saturating_sub(1) as usize;
    let items_to_show = queue_len.min(max_items);
    let scroll_offset = if app.prompt_queue_selected >= items_to_show {
        app.prompt_queue_selected + 1 - items_to_show
    } else {
        0
    };

    for i in 0..items_to_show {
        let idx = scroll_offset + i;
        if idx >= queue_len {
            break;
        }
        let is_selected = idx == app.prompt_queue_selected;
        let is_editing = app.prompt_queue_editing == Some(idx);

        let prefix = if is_selected { "> " } else { "  " };
        let num = format!("{}. ", idx + 1);
        let text = &queue[idx];

        // Truncate to available width
        let avail = inner.width as usize;
        let content = format!("{}{}{}", prefix, num, text);
        let display: String = if content.len() > avail {
            content.chars().take(avail).collect()
        } else {
            content
        };

        let style = if is_editing {
            Style::default().fg(Color::Yellow).add_modifier(Modifier::ITALIC)
        } else if is_selected && focused {
            Style::default().fg(Color::White).bg(theme::SIDEBAR_SEL_BG)
        } else {
            Style::default().fg(Color::Gray)
        };

        lines.push(Line::from(Span::styled(display, style)));
    }

    // Pad empty lines if items don't fill the space
    while lines.len() < max_items {
        lines.push(Line::from(""));
    }

    // Input line at bottom
    if focused {
        let cursor_char = "_";
        let prefix_span = Span::styled("> ", Style::default().fg(Color::Cyan));
        let input_span = Span::styled(
            app.prompt_queue_input.clone(),
            Style::default().fg(Color::White),
        );
        let cursor_span = Span::styled(cursor_char, Style::default().fg(Color::Cyan));
        lines.push(Line::from(vec![prefix_span, input_span, cursor_span]));
    } else {
        let hint = "^p:toggle/focus";
        lines.push(Line::from(Span::styled(
            format!("  {}", hint),
            Style::default().fg(Color::DarkGray),
        )));
    }

    let paragraph = Paragraph::new(lines);
    f.render_widget(paragraph, inner);
}

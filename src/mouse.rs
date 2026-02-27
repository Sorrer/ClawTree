use std::io::Write as _;
use std::process::{Command, Stdio};

use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;

use crate::app::{
    App, FocusTarget, InputMode, MiniModeFocus, ScreenMode, SidebarItem, SidebarPanel,
};
use crate::keys;
use crate::ui::terminal_pane;

/// Convert screen coordinates to vt100-relative (row, col) within the terminal pane.
/// Returns None if the position is outside the terminal pane inner area.
pub fn screen_to_vt100(app: &App, col: u16, row: u16) -> Option<(u16, u16)> {
    let inner = app.areas.terminal_pane_inner.get();
    if inner.width == 0 || inner.height == 0 {
        return None;
    }
    if col < inner.x || col >= inner.x + inner.width
        || row < inner.y || row >= inner.y + inner.height
    {
        return None;
    }
    Some((row - inner.y, col - inner.x))
}

/// Like screen_to_vt100 but clamps to the terminal pane edges instead of
/// returning None.  Used during drag so the selection endpoint tracks even
/// when the mouse leaves the pane.
pub fn screen_to_vt100_clamped(app: &App, col: u16, row: u16) -> Option<(u16, u16)> {
    let inner = app.areas.terminal_pane_inner.get();
    if inner.width == 0 || inner.height == 0 {
        return None;
    }
    let clamped_col = col.max(inner.x).min(inner.x + inner.width - 1) - inner.x;
    let clamped_row = row.max(inner.y).min(inner.y + inner.height - 1) - inner.y;
    Some((clamped_row, clamped_col))
}

/// Extract text from a vt100 screen between two (row, col) positions (inclusive).
/// Handles wide characters, trims trailing whitespace per line, joins with newline.
pub fn extract_text_from_screen(
    screen: &vt100::Screen,
    start: (u16, u16),
    end: (u16, u16),
) -> String {
    let (screen_rows, screen_cols) = screen.size();
    let mut lines: Vec<String> = Vec::new();

    for row in start.0..=end.0 {
        if row >= screen_rows {
            break;
        }
        let col_start = if row == start.0 { start.1 } else { 0 };
        let col_end = if row == end.0 { end.1 } else { screen_cols.saturating_sub(1) };

        let mut line = String::new();
        let mut col = col_start;
        while col <= col_end && col < screen_cols {
            if let Some(cell) = screen.cell(row, col) {
                if cell.is_wide_continuation() {
                    col += 1;
                    continue;
                }
                if cell.has_contents() {
                    line.push_str(cell.contents());
                } else {
                    line.push(' ');
                }
            } else {
                line.push(' ');
            }
            col += 1;
        }
        lines.push(line.trim_end().to_string());
    }

    // Trim trailing empty lines
    while lines.last().map(|l| l.is_empty()).unwrap_or(false) {
        lines.pop();
    }

    lines.join("\n")
}

/// Copy text to the system clipboard via OSC 52 escape sequence.
/// This is instant (just writes to stdout), handles UTF-8 perfectly via
/// base64 encoding, and works in Windows Terminal, iTerm2, Alacritty, kitty, etc.
/// Falls back to external clipboard tools if the write fails.
pub fn copy_to_clipboard(text: &str) -> Result<(), String> {
    // Primary: OSC 52 — supported by all modern terminals
    if copy_via_osc52(text).is_ok() {
        return Ok(());
    }

    // Fallback: external tools
    let candidates: &[&[&str]] = &[
        &["xclip", "-selection", "clipboard"],
        &["xsel", "--clipboard", "--input"],
        &["wl-copy"],
        &["pbcopy"],
        &["win32yank.exe", "-i", "--crlf"],
    ];
    for args in candidates {
        if let Ok(()) = try_clipboard_cmd(args[0], &args[1..], text.as_bytes()) {
            return Ok(());
        }
    }

    Err("No clipboard tool found".to_string())
}

/// Write an OSC 52 escape sequence to set the system clipboard.
/// The terminal emulator intercepts this and copies the base64-decoded text.
fn copy_via_osc52(text: &str) -> Result<(), String> {
    use std::io::Write as _;
    let encoded = base64_encode(text.as_bytes());
    let seq = format!("\x1b]52;c;{}\x07", encoded);
    let mut stdout = std::io::stdout();
    stdout.write_all(seq.as_bytes()).map_err(|e| e.to_string())?;
    stdout.flush().map_err(|e| e.to_string())
}

fn base64_encode(data: &[u8]) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut result = String::with_capacity((data.len() + 2) / 3 * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let triple = (b0 << 16) | (b1 << 8) | b2;
        result.push(CHARS[((triple >> 18) & 0x3F) as usize] as char);
        result.push(CHARS[((triple >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            result.push(CHARS[((triple >> 6) & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
        if chunk.len() > 2 {
            result.push(CHARS[(triple & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
    }
    result
}

fn try_clipboard_cmd(program: &str, args: &[&str], data: &[u8]) -> Result<(), ()> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| ())?;
    if let Some(ref mut stdin) = child.stdin {
        let _ = stdin.write_all(data);
    }
    drop(child.stdin.take()); // close stdin so the child can finish
    let status = child.wait().map_err(|_| ())?;
    if status.success() { Ok(()) } else { Err(()) }
}

/// Extract text from plain-text content (e.g. tmux capture without -e) by
/// row/col coordinates.  Used for scrollback selection where we don't have
/// a vt100::Screen.
pub fn extract_text_from_plain(content: &str, start: (u16, u16), end: (u16, u16)) -> String {
    let lines_vec: Vec<&str> = content.lines().collect();
    let mut result: Vec<String> = Vec::new();

    for row in start.0..=end.0 {
        let row_idx = row as usize;
        if row_idx >= lines_vec.len() {
            break;
        }
        let line = lines_vec[row_idx];
        let chars: Vec<char> = line.chars().collect();
        let col_start = if row == start.0 { start.1 as usize } else { 0 };
        let col_end = if row == end.0 { end.1 as usize } else { chars.len().saturating_sub(1) };
        let col_end = col_end.min(chars.len().saturating_sub(1));

        if col_start <= col_end && col_start < chars.len() {
            let s: String = chars[col_start..=col_end].iter().collect();
            result.push(s.trim_end().to_string());
        } else {
            result.push(String::new());
        }
    }

    // Trim trailing empty lines
    while result.last().map(|l| l.is_empty()).unwrap_or(false) {
        result.pop();
    }

    result.join("\n")
}

/// Check if a screen position overlaps a cached URL, returning its index.
pub fn url_at_position(app: &App, col: u16, row: u16) -> Option<usize> {
    // URLs are only detected in the terminal pane inner area
    let inner = app.areas.terminal_pane_inner.get();
    if inner.width == 0 || inner.height == 0 {
        return None;
    }
    if col < inner.x || col >= inner.x + inner.width || row < inner.y || row >= inner.y + inner.height {
        return None;
    }
    // Convert screen coordinates to terminal-relative coordinates
    let rel_col = col - inner.x;
    let rel_row = row - inner.y;

    for (i, url) in app.url_cache.urls.iter().enumerate() {
        if url.row == rel_row && rel_col >= url.col_start && rel_col < url.col_end {
            return Some(i);
        }
    }
    None
}

/// Check if a point (col, row) is inside a Rect.
fn point_in_rect(col: u16, row: u16, rect: Rect) -> bool {
    rect.width > 0
        && rect.height > 0
        && col >= rect.x
        && col < rect.x + rect.width
        && row >= rect.y
        && row < rect.y + rect.height
}

/// Main entry point for mouse events. Returns true if the event was handled.
pub fn handle_mouse(app: &mut App, event: MouseEvent) -> bool {
    match event.kind {
        MouseEventKind::Down(MouseButton::Left) => handle_left_click(app, event.column, event.row),
        MouseEventKind::ScrollUp => handle_scroll(app, event.column, event.row, true),
        MouseEventKind::ScrollDown => handle_scroll(app, event.column, event.row, false),
        _ => false,
    }
}

/// Check if a click position is in the terminal pane area AND an active session
/// is being displayed (i.e. the area where text selection makes sense).
pub fn is_terminal_session_area(app: &App, col: u16, row: u16) -> bool {
    // Only consider it a "text selection" zone when there's an active terminal session
    if app.active_session_id.is_none() {
        return false;
    }
    // Must not be in an overlay
    if app.show_help || app.dialog.is_some() {
        return false;
    }
    // Must be in normal screen mode
    if app.screen_mode != ScreenMode::Normal {
        return false;
    }
    let terminal_area = app.areas.terminal_pane.get();
    point_in_rect(col, row, terminal_area)
}

// ── Left click dispatch ──────────────────────────────────────────────

fn handle_left_click(app: &mut App, col: u16, row: u16) -> bool {
    // Overlays consume clicks — require keyboard to dismiss
    if app.show_help || app.dialog.is_some() {
        return true;
    }

    match app.screen_mode {
        ScreenMode::Normal => handle_normal_click(app, col, row),
        ScreenMode::Mini => handle_mini_click(app, col, row),
        ScreenMode::MiniDrilldown => handle_drilldown_click(app, col, row),
    }
}

// ── Normal mode clicks ───────────────────────────────────────────────

fn handle_normal_click(app: &mut App, col: u16, row: u16) -> bool {
    let sidebar_area = app.areas.sidebar.get();
    let terminal_area = app.areas.terminal_pane.get();
    let queue_area = app.areas.prompt_queue.get();

    if point_in_rect(col, row, sidebar_area) {
        return handle_sidebar_click(app, col, row);
    }
    if point_in_rect(col, row, queue_area) {
        return handle_prompt_queue_click(app, col, row);
    }
    if point_in_rect(col, row, terminal_area) {
        return handle_terminal_click(app, col, row);
    }
    false
}

fn handle_sidebar_click(app: &mut App, col: u16, row: u16) -> bool {
    // Switch focus to sidebar
    app.focus = FocusTarget::Sidebar;
    app.input_mode = InputMode::Normal;

    // Check if click is in the terminal panel sub-area first
    let tp_inner = app.areas.sidebar_terminal_panel_inner.get();
    if point_in_rect(col, row, tp_inner) {
        let item_row = (row - tp_inner.y) as usize;
        if item_row < app.terminal_panel_items.len() {
            app.sidebar_panel = SidebarPanel::Terminals;
            app.terminal_panel_selected = item_row;
            app.activate_selected();
        }
        return true;
    }

    // Check worktree list area
    let inner = app.areas.sidebar_inner.get();
    if !point_in_rect(col, row, inner) {
        return true; // clicked a border, just focus
    }

    // Map row to item index
    let item_row = (row - inner.y) as usize;

    if item_row < app.sidebar_items.len() {
        app.sidebar_panel = SidebarPanel::Worktrees;
        app.sidebar_selected = item_row;

        // Check if clicking the expand/collapse icon area (first 2 chars)
        if let Some(SidebarItem::Worktree(wi)) = app.sidebar_items.get(item_row).copied() {
            let rel_col = col.saturating_sub(inner.x) as usize;
            if rel_col < 2 {
                // Toggle expand/collapse
                if let Some(wt) = app.worktrees.get_mut(wi) {
                    wt.expanded = !wt.expanded;
                    app.rebuild_sidebar_items();
                }
                return true;
            }
        }

        // Activate the clicked item (open terminal/info panel)
        app.activate_selected();
        return true;
    }

    true
}

fn handle_terminal_click(app: &mut App, col: u16, row: u16) -> bool {
    // If showing info panel (worktree selected, no session)
    if app.active_worktree_idx.is_some() && app.active_session_id.is_none() {
        app.focus = FocusTarget::TerminalPane;
        app.input_mode = InputMode::Normal;

        // Try to select a file in the info panel
        let inner = app.areas.terminal_pane_inner.get();
        if point_in_rect(col, row, inner) {
            handle_info_panel_click(app, row, inner);
        }
        return true;
    }

    // Terminal session — focus and enter terminal mode
    if app.active_session_id.is_some() {
        app.focus = FocusTarget::TerminalPane;
        app.input_mode = InputMode::Terminal;
        app.terminal_scroll = 0; // snap to live view on click
        return true;
    }

    // No active content, just focus
    app.focus = FocusTarget::TerminalPane;
    true
}

fn handle_info_panel_click(app: &mut App, row: u16, inner: Rect) {
    let (unstaged, staged) = app.info_panel_file_lists();
    let rel_row = (row - inner.y) as usize;

    // Layout: Branch(0), HEAD(1), Refresh(2), blank(3), header_unstaged(4), files..., blank, header_staged, files...
    // This is approximate — the exact positions depend on content.
    // header lines: Branch, HEAD, Refresh countdown, blank = 4 lines
    let header_lines = 4;

    // Unstaged section header
    let unstaged_header_row = header_lines;
    let unstaged_start = unstaged_header_row + 1;
    let unstaged_end = unstaged_start + unstaged.len();

    // Staged section: unstaged_end + blank(1) + staged header(1) + files
    let staged_header_row = unstaged_end + 1;
    let staged_start = staged_header_row + 1;
    let staged_end = staged_start + staged.len();

    if rel_row >= unstaged_start && rel_row < unstaged_end {
        app.info_panel_section = 0;
        app.info_panel_cursor = rel_row - unstaged_start;
    } else if rel_row >= staged_start && rel_row < staged_end {
        app.info_panel_section = 1;
        app.info_panel_cursor = rel_row - staged_start;
    }
}

fn handle_prompt_queue_click(app: &mut App, col: u16, row: u16) -> bool {
    app.focus = FocusTarget::PromptQueue;
    app.input_mode = InputMode::Normal;

    let inner = app.areas.prompt_queue_inner.get();
    if !point_in_rect(col, row, inner) {
        return true; // clicked border
    }

    let rel_row = (row - inner.y) as usize;
    let queue = app.active_prompt_queue();
    let queue_len = queue.len();

    // Last row is the input line
    let max_items = inner.height.saturating_sub(1) as usize;
    let items_to_show = queue_len.min(max_items);
    let scroll_offset = if app.prompt_queue_selected >= items_to_show {
        app.prompt_queue_selected + 1 - items_to_show
    } else {
        0
    };

    if rel_row < items_to_show {
        let idx = scroll_offset + rel_row;
        if idx < queue_len {
            app.prompt_queue_selected = idx;
        }
    }
    // If clicked last row, it's the input line — focus is enough

    true
}

// ── Scroll dispatch ──────────────────────────────────────────────────

fn handle_scroll(app: &mut App, col: u16, row: u16, up: bool) -> bool {
    // Overlays consume scroll — require keyboard to interact
    if app.show_help || app.dialog.is_some() {
        return true;
    }

    match app.screen_mode {
        ScreenMode::Normal => handle_normal_scroll(app, col, row, up),
        ScreenMode::Mini => handle_mini_scroll(app, col, row, up),
        ScreenMode::MiniDrilldown => {
            // In drilldown, scroll always goes to terminal
            scroll_terminal(app, up);
            true
        }
    }
}

fn handle_normal_scroll(app: &mut App, col: u16, row: u16, up: bool) -> bool {
    let sidebar_area = app.areas.sidebar.get();
    let terminal_area = app.areas.terminal_pane.get();
    let queue_area = app.areas.prompt_queue.get();

    if point_in_rect(col, row, sidebar_area) {
        if up { app.sidebar_up(); } else { app.sidebar_down(); }
        return true;
    }
    if point_in_rect(col, row, queue_area) {
        scroll_prompt_queue(app, up);
        return true;
    }
    if point_in_rect(col, row, terminal_area) {
        // Info panel: scroll file selection; terminal: scroll history
        if app.active_worktree_idx.is_some() && app.active_session_id.is_none() {
            scroll_info_panel(app, up);
        } else {
            scroll_terminal(app, up);
        }
        return true;
    }

    // Fallback: scroll terminal if active
    scroll_terminal(app, up);
    true
}

fn scroll_terminal(app: &mut App, up: bool) {
    if app.active_session_id.is_none() {
        return;
    }
    if up {
        app.terminal_scroll = app.terminal_scroll.saturating_add(keys::SCROLL_LINES);
        clamp_terminal_scroll(app);
    } else {
        app.terminal_scroll = app.terminal_scroll.saturating_sub(keys::SCROLL_LINES);
    }
}

fn clamp_terminal_scroll(app: &mut App) {
    if let Some(sid) = app.active_session_id {
        if let Some(session) = app.sessions.get(&sid) {
            if let Some(ref tmux_name) = session.tmux_session_name {
                let history = terminal_pane::tmux_history_size(tmux_name);
                if app.terminal_scroll > history {
                    app.terminal_scroll = history;
                }
            }
        }
    }
}

fn scroll_info_panel(app: &mut App, up: bool) {
    let (unstaged, staged) = app.info_panel_file_lists();
    let section_len = if app.info_panel_section == 0 { unstaged.len() } else { staged.len() };
    if section_len == 0 {
        return;
    }
    if up {
        if app.info_panel_cursor > 0 {
            app.info_panel_cursor -= 1;
        }
    } else if app.info_panel_cursor + 1 < section_len {
        app.info_panel_cursor += 1;
    }
}

fn scroll_prompt_queue(app: &mut App, up: bool) {
    let queue_len = app.active_prompt_queue().len();
    if queue_len == 0 {
        return;
    }
    if up {
        if app.prompt_queue_selected > 0 {
            app.prompt_queue_selected -= 1;
        }
    } else if app.prompt_queue_selected + 1 < queue_len {
        app.prompt_queue_selected += 1;
    }
}

// ── Mini mode mouse ──────────────────────────────────────────────────

fn handle_mini_click(app: &mut App, col: u16, row: u16) -> bool {
    let tree_area = app.areas.mini_tree.get();
    let detail_area = app.areas.mini_detail.get();

    if point_in_rect(col, row, tree_area) {
        return handle_mini_tree_click(app, col, row);
    }
    if point_in_rect(col, row, detail_area) {
        // Focus detail input
        if app.mini.focus == MiniModeFocus::AgentList {
            app.mini.focus = MiniModeFocus::DetailInput;
        }
        return true;
    }
    false
}

fn handle_mini_tree_click(app: &mut App, col: u16, row: u16) -> bool {
    app.mini.focus = MiniModeFocus::AgentList;

    let inner = app.areas.mini_tree_inner.get();
    if !point_in_rect(col, row, inner) {
        return true; // clicked border
    }

    let item_row = (row - inner.y) as usize;
    if item_row < app.mini.items.len() {
        app.mini.selected = item_row;

        // Toggle expand/collapse if clicking worktree icon
        if let Some(SidebarItem::Worktree(wi)) = app.mini.items.get(item_row).copied() {
            let rel_col = col.saturating_sub(inner.x) as usize;
            if rel_col < 2 {
                if let Some(wt) = app.worktrees.get_mut(wi) {
                    wt.expanded = !wt.expanded;
                    app.rebuild_mini_agent_list();
                }
                return true;
            }
        }
    }
    true
}

fn handle_mini_scroll(app: &mut App, col: u16, row: u16, up: bool) -> bool {
    let tree_area = app.areas.mini_tree.get();

    if point_in_rect(col, row, tree_area) {
        // Navigate agent list
        if up {
            if app.mini.selected > 0 {
                app.mini.selected -= 1;
            }
        } else if app.mini.selected + 1 < app.mini.items.len() {
            app.mini.selected += 1;
        }
        return true;
    }

    // Default: no-op in detail pane
    true
}

fn handle_drilldown_click(app: &mut App, col: u16, row: u16) -> bool {
    // In drilldown, the entire pane is terminal — just focus it
    let terminal_area = app.areas.terminal_pane.get();
    if point_in_rect(col, row, terminal_area) {
        app.terminal_scroll = 0;
        return true;
    }
    false
}

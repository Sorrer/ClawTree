use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;

use crate::app::{
    App, Dialog, FocusTarget, InputMode, MiniModeFocus, ScreenMode, SidebarItem, SidebarPanel,
    CONFLICT_RESOLVER_COUNT, DIRTY_WORKTREE_OPTION_COUNT, PULL_ERROR_OPTION_COUNT,
};
use crate::keys;
use crate::ui::terminal_pane;

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
    // Overlays take priority
    if app.show_help {
        return handle_help_click(app, col, row);
    }
    if app.dialog.is_some() {
        return handle_dialog_click(app, col, row);
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

    let inner = app.areas.sidebar_inner.get();
    if !point_in_rect(col, row, inner) {
        return true; // clicked the border, just focus
    }

    // Map row to item index
    let item_row = (row - inner.y) as usize;

    // The sidebar_inner only covers the worktree list portion
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
    // Overlays
    if app.show_help {
        return handle_help_scroll(app, up);
    }
    if app.dialog.is_some() {
        return handle_dialog_scroll(app, up);
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

// ── Help overlay mouse ───────────────────────────────────────────────

fn handle_help_click(app: &mut App, col: u16, row: u16) -> bool {
    let area = app.areas.help_overlay.get();
    if !point_in_rect(col, row, area) {
        app.show_help = false;
        return true;
    }
    // Check if clicking in the tab bar area (first inner row)
    // The inner area starts at area.y + 1 (border), and tabs are the first row
    let tab_row = area.y + 1;
    if row == tab_row {
        // Rough tab hit-testing: divide inner width by number of tabs
        let inner_x = area.x + 1;
        let inner_width = area.width.saturating_sub(2);
        let tab_count = keys::KEY_CONTEXT_COUNT as u16;
        if tab_count > 0 && col >= inner_x {
            let rel_x = col - inner_x;
            let tab_width = inner_width / tab_count;
            if tab_width > 0 {
                let clicked_tab = (rel_x / tab_width) as usize;
                if clicked_tab < keys::KEY_CONTEXT_COUNT {
                    app.help_tab = clicked_tab;
                    app.help_scroll = 0;
                }
            }
        }
    }
    true
}

fn handle_help_scroll(app: &mut App, up: bool) -> bool {
    let ctx = keys::KeyContext::ALL[app.help_tab];
    let keys = ctx.keys();
    let extra = ctx.extra_keys(app.wt_available);
    let total = keys.len() + extra.len();
    if up {
        if app.help_scroll > 0 {
            app.help_scroll -= 1;
        }
    } else if app.help_scroll + 1 < total {
        app.help_scroll += 1;
    }
    true
}

// ── Dialog mouse ─────────────────────────────────────────────────────

fn handle_dialog_click(app: &mut App, col: u16, row: u16) -> bool {
    let dialog_area = app.areas.dialog.get();
    if !point_in_rect(col, row, dialog_area) {
        // Click outside dialog — close if not dangerous
        match &app.dialog {
            Some(Dialog::ConfirmDangerous { .. }) => {} // don't close dangerous
            Some(_) => app.close_dialog(),
            None => {}
        }
        return true;
    }

    // For list-based dialogs, try to select item by row
    match &app.dialog {
        Some(Dialog::MergeBranch { branches, .. }) => {
            let inner_y = dialog_area.y + 1; // border
            // Layout: border(1) + target info(1) + separator(1) + list...
            let list_start = inner_y + 2;
            if row >= list_start {
                let idx = (row - list_start) as usize;
                if idx < branches.len() {
                    if let Some(Dialog::MergeBranch { selected, .. }) = &mut app.dialog {
                        *selected = idx;
                    }
                }
            }
        }
        Some(Dialog::MergeConflict { .. }) => {
            let inner_y = dialog_area.y + 1;
            // Layout: conflict msg(1) + separator(1) + list...
            let list_start = inner_y + 2;
            if row >= list_start {
                let idx = (row - list_start) as usize;
                if idx < CONFLICT_RESOLVER_COUNT {
                    if let Some(Dialog::MergeConflict { selected, .. }) = &mut app.dialog {
                        *selected = idx;
                    }
                }
            }
        }
        Some(Dialog::DirtyWorktree { files, .. }) => {
            let inner_y = dialog_area.y + 1;
            let visible_files = files.len().min(8);
            // Layout: info(1) + blank(1) + files + blank(1) + options...
            let options_start = inner_y + 1 + 1 + visible_files as u16 + 1;
            if row >= options_start {
                let idx = (row - options_start) as usize;
                if idx < DIRTY_WORKTREE_OPTION_COUNT {
                    if let Some(Dialog::DirtyWorktree { selected, .. }) = &mut app.dialog {
                        *selected = idx;
                    }
                }
            }
        }
        Some(Dialog::PullError { .. }) => {
            // For simplicity, just detect clicks on the option list
            let inner_y = dialog_area.y + 1;
            let inner_h = dialog_area.height.saturating_sub(2);
            // Options are near the bottom: help(1) at very bottom, then options above it
            let help_row = inner_y + inner_h - 1;
            let options_end = help_row;
            let options_start = options_end.saturating_sub(PULL_ERROR_OPTION_COUNT as u16);
            if row >= options_start && row < options_end {
                let idx = (row - options_start) as usize;
                if idx < PULL_ERROR_OPTION_COUNT {
                    if let Some(Dialog::PullError { selected, .. }) = &mut app.dialog {
                        *selected = idx;
                    }
                }
            }
        }
        Some(Dialog::AuthError { .. }) => {
            // Single dismiss option
            let inner_y = dialog_area.y + 1;
            let inner_h = dialog_area.height.saturating_sub(2);
            let dismiss_row = inner_y + inner_h - 2; // help is last, dismiss above it
            if row == dismiss_row {
                if let Some(Dialog::AuthError { selected, .. }) = &mut app.dialog {
                    *selected = 0;
                }
            }
        }
        _ => {}
    }
    true
}

fn handle_dialog_scroll(app: &mut App, up: bool) -> bool {
    match &app.dialog {
        Some(Dialog::MergeBranch { branches, selected, .. }) => {
            let len = branches.len();
            let sel = *selected;
            if up {
                if sel > 0 {
                    if let Some(Dialog::MergeBranch { selected, .. }) = &mut app.dialog {
                        *selected = sel - 1;
                    }
                }
            } else if sel + 1 < len {
                if let Some(Dialog::MergeBranch { selected, .. }) = &mut app.dialog {
                    *selected = sel + 1;
                }
            }
        }
        Some(Dialog::MergeConflict { selected, .. }) => {
            let sel = *selected;
            if up {
                if sel > 0 {
                    if let Some(Dialog::MergeConflict { selected, .. }) = &mut app.dialog {
                        *selected = sel - 1;
                    }
                }
            } else if sel + 1 < CONFLICT_RESOLVER_COUNT {
                if let Some(Dialog::MergeConflict { selected, .. }) = &mut app.dialog {
                    *selected = sel + 1;
                }
            }
        }
        Some(Dialog::DirtyWorktree { selected, .. }) => {
            let sel = *selected;
            if up {
                if sel > 0 {
                    if let Some(Dialog::DirtyWorktree { selected, .. }) = &mut app.dialog {
                        *selected = sel - 1;
                    }
                }
            } else if sel + 1 < DIRTY_WORKTREE_OPTION_COUNT {
                if let Some(Dialog::DirtyWorktree { selected, .. }) = &mut app.dialog {
                    *selected = sel + 1;
                }
            }
        }
        Some(Dialog::PullError { selected, .. }) => {
            let sel = *selected;
            if up {
                if sel > 0 {
                    if let Some(Dialog::PullError { selected, .. }) = &mut app.dialog {
                        *selected = sel - 1;
                    }
                }
            } else if sel + 1 < PULL_ERROR_OPTION_COUNT {
                if let Some(Dialog::PullError { selected, .. }) = &mut app.dialog {
                    *selected = sel + 1;
                }
            }
        }
        _ => {}
    }
    true
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

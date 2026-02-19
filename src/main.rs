mod app;
mod event;
mod keys;
mod session;
mod ui;
mod worktree;

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use crossterm::event::{DisableMouseCapture, EnableMouseCapture, Event as CrosstermEvent, MouseEventKind};

use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use tracing_subscriber::EnvFilter;

use crate::app::{App, PendingAction, CommitPhase, Dialog};
use crate::event::AppEvent;

/// Global flag set by the signal handler, read by the watchdog thread.
static SIGNAL_RECEIVED: AtomicBool = AtomicBool::new(false);

/// Set the outer terminal's window/tab title via OSC escape sequence.
fn set_terminal_title(title: &str) {
    let _ = crossterm::execute!(
        io::stdout(),
        crossterm::terminal::SetTitle(title)
    );
}

/// Restore the terminal to a usable state. Safe to call multiple times.
fn restore_terminal() {
    // Clear the title we set
    set_terminal_title("");
    let _ = disable_raw_mode();
    let _ = crossterm::execute!(
        io::stdout(),
        LeaveAlternateScreen,
        DisableMouseCapture,
        crossterm::cursor::Show
    );
}

extern "C" fn signal_handler(_sig: libc::c_int) {
    if SIGNAL_RECEIVED.swap(true, Ordering::SeqCst) {
        // Second signal — hard exit immediately, no questions asked.
        restore_terminal();
        std::process::exit(137);
    }
    // First signal — the watchdog thread will pick this up within 50ms.
}

fn main() -> Result<()> {
    // ── Watchdog thread ────────────────────────────────────────────────
    // Runs on a plain OS thread, completely outside tokio. Polls SIGNAL_RECEIVED
    // and force-exits the process if a signal was caught — guarantees we can
    // always exit even if the async runtime or main loop is deadlocked.
    let alive = Arc::new(AtomicBool::new(true));
    let alive_watchdog = Arc::clone(&alive);
    std::thread::Builder::new()
        .name("watchdog".into())
        .spawn(move || {
            loop {
                std::thread::sleep(std::time::Duration::from_millis(50));

                if !alive_watchdog.load(Ordering::Relaxed) {
                    return; // app exited cleanly
                }

                if SIGNAL_RECEIVED.load(Ordering::SeqCst) {
                    restore_terminal();
                    eprintln!("\nSignal received, exiting.");
                    std::thread::sleep(std::time::Duration::from_millis(50));
                    std::process::exit(130);
                }
            }
        })
        .ok();

    // Register signal handlers. These just set SIGNAL_RECEIVED; the watchdog
    // thread does the actual cleanup + exit.
    unsafe {
        libc::signal(libc::SIGINT, signal_handler as libc::sighandler_t);
        libc::signal(libc::SIGTERM, signal_handler as libc::sighandler_t);
        libc::signal(libc::SIGHUP, signal_handler as libc::sighandler_t);
    }

    // Set up file logging (to file, never stdout)
    let log_file = std::fs::File::create("worktree-claude-tui.log")
        .unwrap_or_else(|_| std::fs::File::create("/dev/null").unwrap());
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive("info".parse().unwrap()))
        .with_writer(std::sync::Mutex::new(log_file))
        .with_ansi(false)
        .init();

    // Set up panic hook to restore terminal
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        restore_terminal();
        original_hook(panic_info);
    }));

    // Determine the target directory: first CLI arg, else CWD
    let target_dir = match std::env::args().nth(1) {
        Some(arg) => {
            let p = std::path::PathBuf::from(&arg);
            if p.is_absolute() {
                p
            } else {
                std::env::current_dir()
                    .context("Failed to get current directory")?
                    .join(p)
            }
        }
        None => std::env::current_dir().context("Failed to get current directory")?,
    };

    // Detect bare repo starting from target directory
    let (bare_repo_path, repo_detected) = match worktree::git::detect_bare_repo(&target_dir) {
        Some(path) => {
            tracing::info!("Bare repo detected at {:?}", path);
            (path, true)
        }
        None => {
            tracing::warn!("No bare repo detected at {:?}", target_dir);
            (target_dir.clone(), false)
        }
    };
    tracing::info!("Bare repo path: {:?}, detected: {}", bare_repo_path, repo_detected);

    // Build tokio runtime manually so we control shutdown
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("Failed to create tokio runtime")?;

    // Check tmux availability
    let tmux_available = session::pty::tmux_available();
    tracing::info!("tmux available: {}", tmux_available);

    let result = rt.block_on(async_main(bare_repo_path, repo_detected, tmux_available));

    // Clean exit — tell watchdog to stop, restore terminal
    alive.store(false, Ordering::Relaxed);
    restore_terminal();

    result
}

async fn async_main(bare_repo_path: std::path::PathBuf, repo_detected: bool, tmux_available: bool) -> Result<()> {
    // Initialize terminal
    enable_raw_mode().context("Failed to enable raw mode")?;
    let mut stdout = io::stdout();
    crossterm::execute!(stdout, EnterAlternateScreen, EnableMouseCapture)
        .context("Failed to enter alternate screen")?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).context("Failed to create terminal")?;

    // Create event channel and app
    let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel::<AppEvent>();
    let mut app = App::new(bare_repo_path.clone(), event_tx.clone(), repo_detected, tmux_available);

    // Set the outer terminal title to the repo name
    let repo_name = bare_repo_path
        .file_name()
        .or_else(|| bare_repo_path.parent().and_then(|p| p.file_name()))
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "worktree-tui".to_string());
    set_terminal_title(&format!("wctui — {}", repo_name));

    // ── Input reader — plain OS thread ─────────────────────────────
    let input_tx = event_tx.clone();
    std::thread::Builder::new()
        .name("input-reader".into())
        .spawn(move || {
            loop {
                match crossterm::event::poll(std::time::Duration::from_millis(50)) {
                    Ok(true) => {
                        if let Ok(evt) = crossterm::event::read() {
                            if input_tx.send(AppEvent::Input(evt)).is_err() {
                                break;
                            }
                        }
                    }
                    Ok(false) => {}
                    Err(_) => break,
                }
            }
        })
        .context("Failed to spawn input reader thread")?;

    // ── Tick timer — plain OS thread ───────────────────────────────
    let tick_tx = event_tx.clone();
    std::thread::Builder::new()
        .name("tick-timer".into())
        .spawn(move || {
            loop {
                std::thread::sleep(std::time::Duration::from_millis(33));
                if tick_tx.send(AppEvent::Tick).is_err() {
                    break;
                }
            }
        })
        .context("Failed to spawn tick timer thread")?;

    // Load initial worktree data (only if repo exists)
    if repo_detected {
        if let Err(e) = worktree::refresh_worktrees(&mut app) {
            tracing::error!("Failed to load worktrees: {}", e);
            app.set_status(format!("Failed to load worktrees: {}", e));
        }

        // Reconnect existing tmux sessions from a previous TUI run
        let size = terminal.size()?;
        let reconnected = session::reconnect_tmux_sessions(&mut app, (size.width, size.height));
        if reconnected > 0 {
            app.set_status(format!("Reconnected {} tmux session(s)", reconnected));
            // Restore persisted prompt queues for reconnected sessions
            app.load_prompt_queues();
        }
    }

    // ── Tmux title poller — background thread ──────────────────────
    let tmux_session_info = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    if tmux_available {
        // Seed initial info
        if let Ok(mut info) = tmux_session_info.lock() {
            *info = session::collect_tmux_session_info(&app);
        }
        session::spawn_tmux_title_poller(event_tx.clone(), std::sync::Arc::clone(&tmux_session_info));
    }

    // ── Main event loop ────────────────────────────────────────────
    let mut needs_redraw = true;

    loop {
        if app.should_quit {
            break;
        }

        if needs_redraw {
            terminal.draw(|f| ui::draw(f, &app))?;
            needs_redraw = false;
        }

        // Execute pending actions after drawing the loading overlay
        if let Some(action) = app.pending_action.take() {
            execute_pending_action(&mut app, action);
            app.loading_message = None;
            needs_redraw = true;
            continue; // redraw immediately
        }

        // Timeout ensures we never block forever — always loop back to check should_quit
        match tokio::time::timeout(
            std::time::Duration::from_millis(100),
            event_rx.recv(),
        )
        .await
        {
            Ok(Some(event)) => {
                match event {
                    AppEvent::Input(CrosstermEvent::Key(key)) => {
                        let session_count_before = app.sessions.len();
                        let size = terminal.size()?;
                        keys::handle_key(&mut app, key, (size.width, size.height));
                        // If sessions changed, update the poller's snapshot
                        if tmux_available && app.sessions.len() != session_count_before {
                            if let Ok(mut info) = tmux_session_info.lock() {
                                *info = session::collect_tmux_session_info(&app);
                            }
                        }
                        needs_redraw = true;
                    }
                    AppEvent::Input(CrosstermEvent::Mouse(mouse)) => {
                        match mouse.kind {
                            MouseEventKind::ScrollUp => {
                                keys::handle_scroll(&mut app, true);
                                needs_redraw = true;
                            }
                            MouseEventKind::ScrollDown => {
                                keys::handle_scroll(&mut app, false);
                                needs_redraw = true;
                            }
                            _ => {}
                        }
                    }
                    AppEvent::Input(CrosstermEvent::Resize(w, h)) => {
                        session::resize_all(&app, h, w);
                        needs_redraw = true;
                    }
                    AppEvent::Input(_) => {}
                    AppEvent::PtyOutput { .. } => {
                        needs_redraw = true;
                    }
                    AppEvent::PtyExited { session_id } => {
                        session::mark_exited(&mut app, session_id);
                        if tmux_available {
                            if let Ok(mut info) = tmux_session_info.lock() {
                                *info = session::collect_tmux_session_info(&app);
                            }
                        }
                        needs_redraw = true;
                    }
                    AppEvent::WorktreeCreated { branch, error } => {
                        match error {
                            Some(e) => app.set_status(format!("Error creating '{}': {}", branch, e)),
                            None => {
                                app.set_status(format!("Created worktree '{}'", branch));
                                let _ = worktree::refresh_worktrees(&mut app);
                            }
                        }
                        needs_redraw = true;
                    }
                    AppEvent::PushComplete { branch, error } => {
                        match error {
                            Some(e) => app.set_status(format!("Push '{}' failed: {}", branch, e)),
                            None => {
                                app.set_status(format!("Pushed '{}'", branch));
                            }
                        }
                        needs_redraw = true;
                    }
                    AppEvent::InitRepoComplete { error, .. } => {
                        match error {
                            Some(e) => app.set_status(format!("Init error: {}", e)),
                            None => {
                                app.repo_detected = true;
                                app.set_status("Repository initialized!");
                                let _ = worktree::refresh_worktrees(&mut app);
                            }
                        }
                        needs_redraw = true;
                    }
                    AppEvent::TmuxTitlesChanged { updates } => {
                        session::apply_tmux_title_updates(&app, updates);
                        // Update the poller's snapshot so it knows the current titles
                        if let Ok(mut info) = tmux_session_info.lock() {
                            *info = session::collect_tmux_session_info(&app);
                        }
                        needs_redraw = true;
                    }
                    AppEvent::Tick => {
                        // Auto-send next queued prompt when Claude is idle.
                        // Checks ALL sessions with queues, not just the active one.
                        // Throttled to ~1 check/sec to avoid hammering tmux.
                        if app.dialog.is_none() && app.loading_message.is_none() {
                            let cooldown_ok = app.prompt_queue_last_send
                                .map(|t| t.elapsed() > Duration::from_secs(5))
                                .unwrap_or(true);
                            let check_ok = app.prompt_queue_last_check
                                .map(|t| t.elapsed() > Duration::from_millis(1000))
                                .unwrap_or(true);

                            if cooldown_ok && check_ok {
                                app.prompt_queue_last_check = Some(Instant::now());
                                // Phase 1: find a session ready to receive a prompt (immutable borrows)
                                let candidates: Vec<u64> = app.prompt_queues.iter()
                                    .filter(|(_, q)| !q.is_empty())
                                    .map(|(sid, _)| *sid)
                                    .collect();

                                let mut send_to: Option<(u64, Option<String>, tokio::sync::mpsc::UnboundedSender<bytes::Bytes>)> = None;
                                for sid in &candidates {
                                    if let Some(session) = app.sessions.get(sid) {
                                        if !session.exited.load(Ordering::Relaxed)
                                            && !session.is_active()
                                        {
                                            let settled = session.last_output
                                                .read()
                                                .ok()
                                                .map(|t| t.elapsed() > Duration::from_secs(2))
                                                .unwrap_or(false);
                                            if settled {
                                                send_to = Some((*sid, session.tmux_session_name.clone(), session.write_tx.clone()));
                                                break;
                                            }
                                        }
                                    }
                                }
                                // Phase 2: send the prompt and submit it.
                                if let Some((sid, tmux_name, write_tx)) = send_to {
                                    if let Some(queue) = app.prompt_queues.get_mut(&sid) {
                                        if !queue.is_empty() {
                                            let prompt = queue.remove(0);
                                            if let Some(ref tmux) = tmux_name {
                                                // Use tmux send-keys — the reliable way to
                                                // type into a tmux pane. -l sends literal text,
                                                // then a separate Enter key submits.
                                                let tmux = tmux.clone();
                                                let prompt_clone = prompt.clone();
                                                tokio::spawn(async move {
                                                    let _ = std::process::Command::new("tmux")
                                                        .args(["send-keys", "-t", &tmux, "-l", &prompt_clone])
                                                        .output();
                                                    tokio::time::sleep(Duration::from_millis(100)).await;
                                                    let _ = std::process::Command::new("tmux")
                                                        .args(["send-keys", "-t", &tmux, "Enter"])
                                                        .output();
                                                });
                                            } else {
                                                // Non-tmux fallback: write to PTY directly
                                                let mut payload = prompt.into_bytes();
                                                payload.push(b'\r');
                                                let _ = write_tx.send(bytes::Bytes::from(payload));
                                            }
                                            app.prompt_queue_last_send = Some(Instant::now());
                                            // Clamp selected index if this is the active session
                                            if app.active_session_id == Some(sid) {
                                                if queue.is_empty() {
                                                    app.prompt_queue_selected = 0;
                                                } else if app.prompt_queue_selected >= queue.len() {
                                                    app.prompt_queue_selected = queue.len() - 1;
                                                }
                                            }
                                            app.set_status("Queue: sent prompt");
                                            app.save_prompt_queues();
                                        }
                                    }
                                }
                            }
                        }
                        needs_redraw = true;
                    }
                }
            }
            Ok(None) => break, // all senders dropped
            Err(_) => continue, // timeout, loop back
        }
    }

    Ok(())
}

/// Execute a queued blocking action. Called after the loading overlay is drawn.
fn execute_pending_action(app: &mut App, action: PendingAction) {
    match action {
        PendingAction::StageFile { worktree_idx, file } => {
            if let Err(e) = worktree::stage_file(app, worktree_idx, &file) {
                app.set_status(format!("Failed to stage: {}", e));
            }
            if matches!(app.dialog, Some(Dialog::GitCommit { .. })) {
                refresh_git_commit_dialog(app);
            } else {
                app.refresh_worktree_status();
            }
        }
        PendingAction::UnstageFile { worktree_idx, file } => {
            if let Err(e) = worktree::unstage_file(app, worktree_idx, &file) {
                app.set_status(format!("Failed to unstage: {}", e));
            }
            if matches!(app.dialog, Some(Dialog::GitCommit { .. })) {
                refresh_git_commit_dialog(app);
            } else {
                app.refresh_worktree_status();
            }
        }
        PendingAction::StageAll { worktree_idx } => {
            if let Err(e) = worktree::stage_all(app, worktree_idx) {
                app.set_status(format!("Failed to stage all: {}", e));
            }
            if matches!(app.dialog, Some(Dialog::GitCommit { .. })) {
                refresh_git_commit_dialog(app);
            } else {
                app.refresh_worktree_status();
            }
        }
        PendingAction::Commit { worktree_idx, message } => {
            match worktree::commit(app, worktree_idx, &message) {
                Ok(()) => {
                    let _ = worktree::refresh_worktrees(app);
                    app.refresh_worktree_status();
                    // If a merge was waiting on this commit, re-queue it
                    if let Some(merge_action) = app.pending_merge.take() {
                        app.set_status("Committed — retrying merge...");
                        app.queue_action("Merging...", merge_action);
                    } else {
                        app.set_status("Changes committed successfully");
                    }
                }
                Err(e) => {
                    app.set_status(format!("Commit failed: {}", e));
                }
            }
        }
        PendingAction::RefreshWorktreeStatus => {
            app.refresh_worktree_status();
        }
        PendingAction::FetchWorktreeStatus { worktree_idx } => {
            app.worktree_status = worktree::fetch_worktree_status(app, worktree_idx).ok();
            app.clamp_info_panel_cursor();
        }
        PendingAction::OpenStageCommit { worktree_idx } => {
            match worktree::status_porcelain(app, worktree_idx) {
                Ok(changes) => {
                    if changes.is_empty() {
                        app.set_status("Working tree clean — nothing to commit");
                        return;
                    }
                    let mut unstaged = Vec::new();
                    let mut staged = Vec::new();
                    for c in &changes {
                        if c.index_status != ' ' && c.index_status != '?' {
                            staged.push((c.index_status, c.path.clone()));
                        }
                        if c.work_status != ' ' || c.index_status == '?' {
                            let status = if c.index_status == '?' { '?' } else { c.work_status };
                            unstaged.push((status, c.path.clone()));
                        }
                    }
                    app.open_dialog(Dialog::GitCommit {
                        worktree_idx,
                        unstaged,
                        staged,
                        section: 0,
                        selected: 0,
                        phase: CommitPhase::Staging,
                        commit_message: String::new(),
                    });
                }
                Err(e) => {
                    app.set_status(format!("Failed to get status: {}", e));
                }
            }
        }
        PendingAction::MergeExecute { source_worktree_idx, target_branch } => {
            let source_name = app
                .worktrees
                .get(source_worktree_idx)
                .map(|w| w.branch.clone())
                .unwrap_or_default();

            // Find or create worktree for target branch
            let target_wt_idx = match worktree::find_worktree_for_branch(app, &target_branch) {
                Some(idx) => idx,
                None => {
                    if let Err(e) = worktree::git::create_worktree(
                        &app.bare_repo_path,
                        &target_branch,
                        &target_branch,
                        "",
                    ) {
                        app.set_status(format!("Failed to create worktree for '{}': {}", target_branch, e));
                        return;
                    }
                    if let Err(e) = worktree::refresh_worktrees(app) {
                        app.set_status(format!("Failed to refresh: {}", e));
                        return;
                    }
                    match worktree::find_worktree_for_branch(app, &target_branch) {
                        Some(idx) => idx,
                        None => {
                            app.set_status(format!("Could not find worktree for '{}'", target_branch));
                            return;
                        }
                    }
                }
            };

            // Check target worktree is clean before merging
            match worktree::is_worktree_clean(app, target_wt_idx) {
                Ok(false) => {
                    // Remember the merge so we can retry after commit
                    app.pending_merge = Some(PendingAction::MergeExecute {
                        source_worktree_idx,
                        target_branch: target_branch.clone(),
                    });
                    // Open the commit UI for the dirty target worktree
                    match worktree::status_porcelain(app, target_wt_idx) {
                        Ok(changes) => {
                            let mut unstaged = Vec::new();
                            let mut staged = Vec::new();
                            for c in &changes {
                                if c.index_status != ' ' && c.index_status != '?' {
                                    staged.push((c.index_status, c.path.clone()));
                                }
                                if c.work_status != ' ' || c.index_status == '?' {
                                    let status = if c.index_status == '?' { '?' } else { c.work_status };
                                    unstaged.push((status, c.path.clone()));
                                }
                            }
                            app.set_status(format!(
                                "'{}' has uncommitted changes — commit before merging",
                                target_branch
                            ));
                            app.open_dialog(Dialog::GitCommit {
                                worktree_idx: target_wt_idx,
                                unstaged,
                                staged,
                                section: 0,
                                selected: 0,
                                phase: CommitPhase::Staging,
                                commit_message: String::new(),
                            });
                        }
                        Err(e) => {
                            app.pending_merge = None;
                            app.set_status(format!("Failed to get status of '{}': {}", target_branch, e));
                        }
                    }
                    return;
                }
                Err(e) => {
                    app.set_status(format!("Failed to check status of '{}': {}", target_branch, e));
                    return;
                }
                Ok(true) => {}
            }

            match worktree::merge_into_worktree(app, target_wt_idx, &source_name) {
                Ok(worktree::git::MergeResult::Success(output)) => {
                    app.set_status(format!(
                        "Merged '{}' into '{}': {}",
                        source_name,
                        target_branch,
                        output.lines().next().unwrap_or("ok").trim()
                    ));
                    let _ = worktree::refresh_worktrees(app);
                    app.refresh_worktree_status();
                }
                Ok(worktree::git::MergeResult::Conflict(_)) => {
                    app.set_status(format!(
                        "Merge conflict: '{}' into '{}'",
                        source_name, target_branch
                    ));
                    app.open_dialog(Dialog::MergeConflict {
                        worktree_idx: target_wt_idx,
                        source_branch: source_name.clone(),
                        selected: 0,
                    });
                }
                Err(e) => {
                    app.set_status(format!("Merge error: {}", e));
                }
            }
        }
    }
}

/// Re-fetch file status and rebuild the GitCommit dialog lists.
fn refresh_git_commit_dialog(app: &mut App) {
    let (worktree_idx, old_section, old_selected, phase, commit_message) = match &app.dialog {
        Some(Dialog::GitCommit {
            worktree_idx,
            section,
            selected,
            phase,
            commit_message,
            ..
        }) => (*worktree_idx, *section, *selected, *phase, commit_message.clone()),
        _ => return,
    };

    let changes = match worktree::status_porcelain(app, worktree_idx) {
        Ok(c) => c,
        Err(e) => {
            app.set_status(format!("Failed to refresh status: {}", e));
            return;
        }
    };

    let mut unstaged = Vec::new();
    let mut staged = Vec::new();
    for c in &changes {
        if c.index_status != ' ' && c.index_status != '?' {
            staged.push((c.index_status, c.path.clone()));
        }
        if c.work_status != ' ' || c.index_status == '?' {
            let status = if c.index_status == '?' { '?' } else { c.work_status };
            unstaged.push((status, c.path.clone()));
        }
    }

    // If both lists are empty, the worktree is now clean — close dialog
    if unstaged.is_empty() && staged.is_empty() {
        app.set_status("All changes committed — worktree is clean");
        app.close_dialog();
        return;
    }

    // Clamp selection
    let section = if old_section == 0 && unstaged.is_empty() {
        1
    } else if old_section == 1 && staged.is_empty() {
        0
    } else {
        old_section
    };

    let len = if section == 0 { unstaged.len() } else { staged.len() };
    let selected = if len == 0 { 0 } else { old_selected.min(len - 1) };

    app.dialog = Some(Dialog::GitCommit {
        worktree_idx,
        unstaged,
        staged,
        section,
        selected,
        phase,
        commit_message,
    });
}

mod app;
mod event;
mod keys;
mod mouse;
mod session;
mod ui;
mod worktree;

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use crossterm::event::{DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture, Event as CrosstermEvent, KeyModifiers as EvtKeyModifiers, MouseButton, MouseEventKind};

use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use tracing_subscriber::EnvFilter;

use crate::app::{App, PendingAction, CommitPhase, Dialog, ScreenMode, StatusSeverity};
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
        DisableBracketedPaste,
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

    // Parse CLI arguments: supports --color-mode <mode> and a positional directory
    let mut color_mode_override: Option<ui::theme::ColorMode> = None;
    let mut positional_dir: Option<String> = None;
    {
        let mut args = std::env::args().skip(1);
        while let Some(arg) = args.next() {
            if arg == "--color-mode" {
                if let Some(val) = args.next() {
                    color_mode_override = match val.to_lowercase().as_str() {
                        "truecolor" | "true" | "24bit" => Some(ui::theme::ColorMode::TrueColor),
                        "256" | "256color" => Some(ui::theme::ColorMode::Color256),
                        "basic" | "16" | "ansi" => Some(ui::theme::ColorMode::Basic),
                        _ => {
                            eprintln!("Unknown color mode '{}'. Use: truecolor, 256, basic", val);
                            std::process::exit(1);
                        }
                    };
                }
            } else if positional_dir.is_none() {
                positional_dir = Some(arg);
            }
        }
    }

    // Initialize theme before any UI code runs
    ui::theme::init(color_mode_override);

    // Determine the target directory: first positional arg, else CWD
    let target_dir = match positional_dir {
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
    // Detect regular (non-bare) git repo if no bare repo found
    let regular_repo_path = if !repo_detected {
        worktree::git::detect_regular_repo(&target_dir)
    } else {
        None
    };
    if let Some(ref p) = regular_repo_path {
        tracing::info!("Regular repo detected at {:?}", p);
    }
    tracing::info!("Bare repo path: {:?}, detected: {}", bare_repo_path, repo_detected);

    // Build tokio runtime manually so we control shutdown
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("Failed to create tokio runtime")?;

    // Check tmux availability
    let tmux_available = session::pty::tmux_available();
    tracing::info!("tmux available: {}", tmux_available);

    // Check Windows Terminal availability (use `which` to avoid flashing a console window)
    let wt_available = std::env::var("WSL_DISTRO_NAME").is_ok()
        && std::process::Command::new("which")
            .arg("wt.exe")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
    tracing::info!("wt.exe available: {}", wt_available);

    let result = rt.block_on(async_main(bare_repo_path, repo_detected, regular_repo_path, tmux_available, wt_available));

    // Clean exit — tell watchdog to stop, restore terminal
    alive.store(false, Ordering::Relaxed);
    restore_terminal();

    result
}

async fn async_main(bare_repo_path: std::path::PathBuf, repo_detected: bool, regular_repo_path: Option<std::path::PathBuf>, tmux_available: bool, wt_available: bool) -> Result<()> {
    // Initialize terminal
    enable_raw_mode().context("Failed to enable raw mode")?;
    let mut stdout = io::stdout();
    crossterm::execute!(stdout, EnterAlternateScreen, EnableMouseCapture, EnableBracketedPaste)
        .context("Failed to enter alternate screen")?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).context("Failed to create terminal")?;

    // Create event channel and app
    let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel::<AppEvent>();
    let mut app = App::new(bare_repo_path.clone(), event_tx.clone(), repo_detected, tmux_available, wt_available);
    app.regular_repo_path = regular_repo_path;

    // Set the outer terminal title to the repo name
    let repo_name = bare_repo_path
        .file_name()
        .or_else(|| bare_repo_path.parent().and_then(|p| p.file_name()))
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "worktree-tui".to_string());
    set_terminal_title(&format!("clawtree — {}", repo_name));

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
            // Sync tmux window sizes to the current terminal dimensions —
            // the sessions may have been created on a different-sized monitor.
            session::resize_all(&app, size.height, size.width);
            app.set_status(format!("Reconnected {} tmux session(s)", reconnected));
            // Restore persisted prompt queues for reconnected sessions
            app.load_prompt_queues();
        }

        // Load saved prompt templates for mini mode
        app.load_saved_prompts();
    }

    // ── Tmux title poller — background thread ──────────────────────
    let tmux_session_info = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    if tmux_available {
        // Seed initial info
        if let Ok(mut info) = tmux_session_info.lock() {
            *info = session::collect_tmux_session_info(&app);
        }
        session::spawn_tmux_title_poller(event_tx.clone(), std::sync::Arc::clone(&tmux_session_info));
        session::spawn_claude_usage_poller(event_tx.clone(), std::sync::Arc::clone(&tmux_session_info));
    }

    // ── Global usage poller — background thread ────────────────
    session::spawn_global_usage_poller(event_tx.clone());

    // ── Worktree status poller — background thread ───────────────
    let status_poller_paths = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    if repo_detected {
        // Seed initial worktree paths
        if let Ok(mut paths) = status_poller_paths.lock() {
            *paths = worktree::collect_worktree_paths(&app);
        }
        worktree::spawn_status_poller(event_tx.clone(), std::sync::Arc::clone(&status_poller_paths));
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
            let wt_count_before = app.worktrees.len();
            execute_pending_action(&mut app, action);
            // Only clear loading if the action didn't chain another action
            // (e.g. Commit → MergeExecute sets a new loading_message via queue_action)
            if app.pending_action.is_none() {
                app.loading_message = None;
            }
            // Update status poller if worktrees changed during action
            if repo_detected && app.worktrees.len() != wt_count_before {
                if let Ok(mut paths) = status_poller_paths.lock() {
                    *paths = worktree::collect_worktree_paths(&app);
                }
            }
            needs_redraw = true;
            continue; // redraw immediately
        }

        // Wait for at least one event, then drain all pending events before
        // redrawing. Processing events in batches prevents input lag when
        // many Tick/PtyOutput events are queued between redraws.
        let first_event = match tokio::time::timeout(
            std::time::Duration::from_millis(100),
            event_rx.recv(),
        )
        .await
        {
            Ok(Some(e)) => e,
            Ok(None) => break, // all senders dropped
            Err(_) => continue, // timeout, loop back
        };

        let mut current_event = Some(first_event);
        while let Some(event) = current_event.take().or_else(|| event_rx.try_recv().ok()) {
            if app.should_quit { break; }
                match event {
                    AppEvent::Input(CrosstermEvent::Key(key)) => {
                        // Re-enable mouse capture on any keypress if it was
                        // disabled for text selection
                        if !app.mouse_captured {
                            app.mouse_captured = true;
                            let _ = crossterm::execute!(io::stdout(), EnableMouseCapture);
                        }
                        let session_count_before = app.sessions.len();
                        let worktree_count_before = app.worktrees.len();
                        let size = terminal.size()?;
                        keys::handle_key(&mut app, key, (size.width, size.height));
                        // If sessions changed, update the tmux poller's snapshot
                        if tmux_available && app.sessions.len() != session_count_before {
                            if let Ok(mut info) = tmux_session_info.lock() {
                                *info = session::collect_tmux_session_info(&app);
                            }
                        }
                        // If worktrees changed, update the status poller's snapshot
                        if repo_detected && app.worktrees.len() != worktree_count_before {
                            if let Ok(mut paths) = status_poller_paths.lock() {
                                *paths = worktree::collect_worktree_paths(&app);
                            }
                        }
                        // Handle immediate background status fetch requests
                        if let Some(path) = app.request_status_fetch.take() {
                            let tx = event_tx.clone();
                            let next_refresh = app.next_status_refresh
                                .unwrap_or_else(|| Instant::now() + worktree::STATUS_REFRESH_INTERVAL);
                            tokio::task::spawn_blocking(move || {
                                if let Ok(status) = worktree::fetch_worktree_status_by_path(&path) {
                                    let _ = tx.send(AppEvent::WorktreeStatusReady {
                                        worktree_path: path,
                                        status,
                                        next_refresh_at: next_refresh,
                                    });
                                }
                            });
                        }
                        needs_redraw = true;
                    }
                    AppEvent::Input(CrosstermEvent::Mouse(mouse)) => {
                        match mouse.kind {
                            MouseEventKind::Down(MouseButton::Left) => {
                                if mouse.modifiers.contains(EvtKeyModifiers::SHIFT) {
                                    // Shift+Click → text selection anywhere (escape hatch)
                                    app.mouse_captured = false;
                                    app.mouse_capture_disabled_at = Some(Instant::now());
                                    let _ = crossterm::execute!(io::stdout(), DisableMouseCapture);
                                } else if mouse::is_terminal_session_area(&app, mouse.column, mouse.row) {
                                    // Click in terminal session pane → disable capture immediately
                                    // so the terminal emulator handles text selection natively
                                    // (it gets the Down, Drag, and Up events directly).
                                    // Also process the click to focus the pane.
                                    mouse::handle_mouse(&mut app, mouse);
                                    app.mouse_captured = false;
                                    app.mouse_capture_disabled_at = Some(Instant::now());
                                    let _ = crossterm::execute!(io::stdout(), DisableMouseCapture);
                                } else {
                                    // Click in sidebar/other panels → handle immediately as click.
                                    // No text selection in non-terminal panels.
                                    mouse::handle_mouse(&mut app, mouse);
                                }
                                needs_redraw = true;
                            }
                            MouseEventKind::Down(MouseButton::Middle)
                            | MouseEventKind::Down(MouseButton::Right) => {
                                // Middle/Right click → native text selection
                                app.mouse_captured = false;
                                app.mouse_capture_disabled_at = Some(Instant::now());
                                let _ = crossterm::execute!(io::stdout(), DisableMouseCapture);
                                needs_redraw = true;
                            }
                            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                                mouse::handle_mouse(&mut app, mouse);
                                needs_redraw = true;
                            }
                            _ => {}
                        }
                    }
                    AppEvent::Input(CrosstermEvent::Paste(data)) => {
                        // Re-enable mouse capture if it was disabled for text selection
                        if !app.mouse_captured {
                            app.mouse_captured = true;
                            let _ = crossterm::execute!(io::stdout(), EnableMouseCapture);
                        }
                        keys::handle_paste(&mut app, data);
                        needs_redraw = true;
                    }
                    AppEvent::Input(CrosstermEvent::Resize(w, h)) => {
                        tracing::info!("RESIZE-EVENT from crossterm: {}x{}", w, h);
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
                            Some(e) => app.set_status_with(StatusSeverity::Error, format!("Error creating '{}': {}", branch, e)),
                            None => {
                                app.set_status_with(StatusSeverity::Success, format!("Created worktree '{}'", branch));
                                let _ = worktree::refresh_worktrees(&mut app);
                                // Update status poller paths
                                if let Ok(mut paths) = status_poller_paths.lock() {
                                    *paths = worktree::collect_worktree_paths(&app);
                                }
                            }
                        }
                        needs_redraw = true;
                    }
                    AppEvent::PushComplete { branch, error } => {
                        match error {
                            Some(e) => {
                                if worktree::git::is_auth_error(&e) {
                                    app.set_status_with(StatusSeverity::Error, format!("Push '{}' — authentication needed", branch));
                                    app.open_dialog(Dialog::AuthError {
                                        operation: "push".to_string(),
                                        message: e,
                                        selected: 0,
                                    });
                                } else {
                                    app.set_status_with(StatusSeverity::Error, format!("Push '{}' failed: {}", branch, e));
                                }
                            }
                            None => {
                                app.set_status_with(StatusSeverity::Success, format!("Pushed '{}'", branch));
                            }
                        }
                        needs_redraw = true;
                    }
                    AppEvent::PullComplete { branch, worktree_idx, error, has_conflicts } => {
                        match (error, has_conflicts) {
                            (None, _) => {
                                app.set_status_with(StatusSeverity::Success, format!("Pulled '{}'", branch));
                                let _ = worktree::refresh_worktrees(&mut app);
                                app.refresh_worktree_status();
                            }
                            (Some(_msg), true) => {
                                // Merge conflict from pull — open conflict resolution dialog
                                app.set_status_with(
                                    StatusSeverity::Error,
                                    format!("Pull '{}' has merge conflicts", branch),
                                );
                                app.open_dialog(Dialog::MergeConflict {
                                    worktree_idx,
                                    source_branch: format!("origin/{}", branch),
                                    selected: 0,
                                });
                            }
                            (Some(msg), false) => {
                                if worktree::git::is_auth_error(&msg) {
                                    app.set_status_with(
                                        StatusSeverity::Error,
                                        format!("Pull '{}' — authentication needed", branch),
                                    );
                                    app.open_dialog(Dialog::AuthError {
                                        operation: "pull".to_string(),
                                        message: msg,
                                        selected: 0,
                                    });
                                } else {
                                    // Non-conflict error — open error dialog
                                    app.set_status_with(
                                        StatusSeverity::Error,
                                        format!("Pull '{}' failed", branch),
                                    );
                                    app.open_dialog(Dialog::PullError {
                                        worktree_idx,
                                        error_message: msg,
                                        selected: 0,
                                    });
                                }
                            }
                        }
                        needs_redraw = true;
                    }
                    AppEvent::InitRepoComplete { error, .. } => {
                        match error {
                            Some(e) => {
                                if worktree::git::is_auth_error(&e) {
                                    app.set_status_with(StatusSeverity::Error, "Clone failed — authentication needed");
                                    app.open_dialog(Dialog::AuthError {
                                        operation: "clone".to_string(),
                                        message: e,
                                        selected: 0,
                                    });
                                } else {
                                    app.set_status_with(StatusSeverity::Error, format!("Init error: {}", e));
                                }
                            }
                            None => {
                                app.repo_detected = true;
                                app.set_status_with(StatusSeverity::Success, "Repository initialized!");
                                let _ = worktree::refresh_worktrees(&mut app);
                                // Start the status poller now that we have a repo
                                if let Ok(mut paths) = status_poller_paths.lock() {
                                    *paths = worktree::collect_worktree_paths(&app);
                                }
                                worktree::spawn_status_poller(
                                    event_tx.clone(),
                                    std::sync::Arc::clone(&status_poller_paths),
                                );
                            }
                        }
                        needs_redraw = true;
                    }
                    AppEvent::ConvertRepoComplete { bare_repo_path: new_path, error } => {
                        match error {
                            Some(e) => app.set_status_with(StatusSeverity::Error, format!("Convert error: {}", e)),
                            None => {
                                app.bare_repo_path = new_path;
                                app.repo_detected = true;
                                app.regular_repo_path = None;
                                app.set_status_with(StatusSeverity::Success, "Repository converted to bare worktree layout!");
                                let _ = worktree::refresh_worktrees(&mut app);
                                // Start the status poller now that we have a repo
                                if let Ok(mut paths) = status_poller_paths.lock() {
                                    *paths = worktree::collect_worktree_paths(&app);
                                }
                                worktree::spawn_status_poller(
                                    event_tx.clone(),
                                    std::sync::Arc::clone(&status_poller_paths),
                                );
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
                    AppEvent::ClaudeUsageUpdated { updates } => {
                        for (session_id, usage) in updates {
                            app.claude_usage.insert(session_id, usage);
                        }
                        needs_redraw = true;
                    }
                    AppEvent::GlobalUsageUpdated { usage } => {
                        app.global_usage = Some(usage);
                        needs_redraw = true;
                    }
                    AppEvent::SummaryReady { session_id, summary } => {
                        app.agent_summaries.insert(session_id, summary);
                        needs_redraw = true;
                    }
                    AppEvent::WorktreeStatusReady { worktree_path, status, next_refresh_at } => {
                        // Update the per-worktree cache
                        app.worktree_statuses.insert(worktree_path.clone(), status.clone());
                        app.next_status_refresh = Some(next_refresh_at);
                        // If this is the currently viewed worktree, update the display
                        if let Some(wi) = app.active_worktree_idx {
                            if app.worktrees.get(wi).map(|wt| &wt.path) == Some(&worktree_path) {
                                app.worktree_status = Some(status);
                                app.clamp_info_panel_cursor();
                            }
                        }
                        needs_redraw = true;
                    }
                    AppEvent::Tick => {
                        // Auto-re-enable mouse capture after text selection timeout
                        if let Some(disabled_at) = app.mouse_capture_disabled_at {
                            if !app.mouse_captured && disabled_at.elapsed() > Duration::from_secs(2) {
                                app.mouse_captured = true;
                                app.mouse_capture_disabled_at = None;
                                let _ = crossterm::execute!(io::stdout(), EnableMouseCapture);
                            }
                        }

                        // Increment spinner frame (~30fps from 33ms tick)
                        // We want ~10fps for the spinner, so advance every 3rd tick
                        app.spinner_frame = app.spinner_frame.wrapping_add(1);

                        // ── Mini mode agent tracking (~1/sec, on every 30th tick) ──
                        if app.spinner_frame % 30 == 0 {
                            // Rebuild agent list if in mini mode
                            if app.screen_mode == ScreenMode::Mini {
                                app.rebuild_mini_agent_list();
                            }

                            // Detect Working→Idle transitions and capture summaries
                            let sids: Vec<u64> = app.sessions.keys().copied().collect();
                            for sid in &sids {
                                if let Some(session) = app.sessions.get(sid) {
                                    let currently_active = session.is_active();
                                    let was_active = session.was_active;
                                    let exited = session.exited.load(std::sync::atomic::Ordering::Relaxed);

                                    // Transition: was working, now idle/done
                                    if was_active && !currently_active && !exited {
                                        if let Some(summary) = session::extract_summary(session) {
                                            app.agent_summaries.insert(*sid, summary);
                                        } else if let Some(ref tmux_name) = session.tmux_session_name {
                                            // No XML tags found — fall back to one-shot summary
                                            session::generate_oneshot_summary(
                                                *sid,
                                                tmux_name,
                                                app.event_tx.clone(),
                                            );
                                        }
                                    }
                                }
                            }
                            // Update was_active flags (separate loop to avoid borrow issues)
                            for sid in &sids {
                                if let Some(session) = app.sessions.get_mut(sid) {
                                    session.was_active = session.is_active();
                                }
                            }
                        }

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
    }

    // Clean shutdown: explicitly detach our tmux clients before PTY handles
    // are dropped. Without this, dropping the PTY master sends SIGHUP to the
    // `tmux attach-session` process, which can cause tmux to briefly resize
    // the window or inject artifacts (the cumulative "extra newline" bug).
    // Detaching via server command is clean and immediate.
    for session in app.sessions.values() {
        if let Some(ref tmux_name) = session.tmux_session_name {
            if !session.exited.load(std::sync::atomic::Ordering::SeqCst) {
                let _ = std::process::Command::new("tmux")
                    .args(["detach-client", "-s", tmux_name])
                    .output();
            }
        }
    }
    // Brief pause to let tmux process the detach before PTY handles are dropped
    std::thread::sleep(std::time::Duration::from_millis(50));

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
                        app.set_status_with(StatusSeverity::Success, "Changes committed successfully");
                    }
                }
                Err(e) => {
                    app.set_status_with(StatusSeverity::Error, format!("Commit failed: {}", e));
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

            // Check target worktree for an existing in-progress merge
            if let Some(conflict_branch) = worktree::merge_in_progress(app, target_wt_idx) {
                app.set_status(format!(
                    "'{}' has an unresolved merge — resolve or abort first",
                    target_branch
                ));
                app.open_dialog(Dialog::MergeConflict {
                    worktree_idx: target_wt_idx,
                    source_branch: conflict_branch,
                    selected: 0,
                });
                return;
            }

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
                    let detail = output.lines().next().unwrap_or("ok").trim().to_string();
                    let msg = format!("Merged '{}' into '{}': {}", source_name, target_branch, detail);
                    app.set_status(msg.clone());
                    let _ = worktree::refresh_worktrees(app);
                    app.refresh_worktree_status();
                    app.open_dialog(Dialog::MergeSuccess {
                        message: msg,
                    });
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

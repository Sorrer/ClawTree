mod app;
mod event;
mod keys;
mod mouse;
mod session;
mod ui;
mod update;
mod url;
mod worktree;

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event as CrosstermEvent, KeyModifiers as EvtKeyModifiers, MouseButton, MouseEventKind,
};

use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use tracing_subscriber::EnvFilter;

use crate::app::{
    App, CommitPhase, Dialog, PendingAction, ScreenMode, StatusSeverity, UpdatePhase,
};
use crate::event::AppEvent;

/// Watchdog thread poll interval in milliseconds.
const WATCHDOG_POLL_MS: u64 = 50;
/// Tick rate for UI refresh (~30fps).
const TICK_RATE_MS: u64 = 33;

/// Global flag set by the signal handler, read by the watchdog thread.
static SIGNAL_RECEIVED: AtomicBool = AtomicBool::new(false);

/// Set the outer terminal's window/tab title via OSC escape sequence.
fn set_terminal_title(title: &str) {
    let _ = crossterm::execute!(io::stdout(), crossterm::terminal::SetTitle(title));
}

/// Restore the terminal to a usable state. Safe to call multiple times.
fn restore_terminal() {
    // Clear the title we set
    set_terminal_title("");
    let _ = disable_raw_mode();
    let _ = crossterm::execute!(io::stdout(), crossterm::event::PopKeyboardEnhancementFlags);
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
    // Capture the executable path up front. An in-app update replaces the binary
    // at this path and deletes the old inode, after which `current_exe()` would
    // resolve to a now-deleted path — so we must remember it now to restart into
    // the freshly installed binary later.
    let original_exe = std::env::current_exe();

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
                std::thread::sleep(std::time::Duration::from_millis(WATCHDOG_POLL_MS));

                if !alive_watchdog.load(Ordering::Relaxed) {
                    return; // app exited cleanly
                }

                if SIGNAL_RECEIVED.load(Ordering::SeqCst) {
                    restore_terminal();
                    eprintln!("\nSignal received, exiting.");
                    std::thread::sleep(std::time::Duration::from_millis(WATCHDOG_POLL_MS));
                    std::process::exit(130);
                }
            }
        })
        .ok();

    // Register signal handlers. These just set SIGNAL_RECEIVED; the watchdog
    // thread does the actual cleanup + exit.
    unsafe {
        libc::signal(
            libc::SIGINT,
            signal_handler as *const () as libc::sighandler_t,
        );
        libc::signal(
            libc::SIGTERM,
            signal_handler as *const () as libc::sighandler_t,
        );
        libc::signal(
            libc::SIGHUP,
            signal_handler as *const () as libc::sighandler_t,
        );
    }

    // Set up file logging to ~/.clawtree/logs/clawtree.log
    let log_dir = std::env::var("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from("."))
        .join(".clawtree")
        .join("logs");
    let _ = std::fs::create_dir_all(&log_dir);
    let log_file = std::fs::File::create(log_dir.join("clawtree.log"))
        .or_else(|_| std::fs::File::create("/dev/null"))
        .or_else(|_| std::fs::File::open("/dev/null"))
        .expect("Cannot open any log target");
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

    // Parse CLI arguments: supports --color-mode <mode>, --cleanup, --cleanup-all, and a positional directory
    let mut color_mode_override: Option<ui::theme::ColorMode> = None;
    let mut positional_dir: Option<String> = None;
    let mut do_cleanup = false;
    let mut do_cleanup_all = false;
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
            } else if arg == "--cleanup-all" {
                do_cleanup_all = true;
            } else if arg == "--cleanup" {
                do_cleanup = true;
            } else if positional_dir.is_none() {
                positional_dir = Some(arg);
            }
        }
    }

    // Handle --cleanup / --cleanup-all: kill clawtree tmux sessions and exit
    if do_cleanup || do_cleanup_all {
        let session_names: Vec<String> = if do_cleanup_all {
            // Kill ALL clawtree tmux sessions globally
            session::pty::list_tmux_session_names()
        } else {
            // Scope to the current project: resolve the project root, then
            // only kill sessions whose worktree path lives under it.
            let target = match &positional_dir {
                Some(arg) => {
                    let p = std::path::PathBuf::from(arg);
                    if p.is_absolute() {
                        p
                    } else {
                        std::env::current_dir().unwrap_or_default().join(p)
                    }
                }
                None => std::env::current_dir().unwrap_or_default(),
            };
            let project_root = worktree::git::detect_bare_repo(&target)
                .or_else(|| worktree::git::detect_regular_repo(&target))
                .unwrap_or_else(|| target.clone());

            session::pty::list_tmux_sessions()
                .into_iter()
                .filter(|(_name, wt_path)| wt_path.starts_with(&project_root))
                .map(|(name, _)| name)
                .collect()
        };

        let scope = if do_cleanup_all { "global" } else { "project" };
        if session_names.is_empty() {
            println!("No clawtree tmux sessions found ({}).", scope);
        } else {
            println!(
                "Killing {} clawtree tmux session(s) ({}):",
                session_names.len(),
                scope
            );
            for name in &session_names {
                let ok = session::pty::kill_tmux_session(name);
                let status = if ok { "killed" } else { "failed" };
                println!("  {} ... {}", name, status);
            }
        }
        alive.store(false, Ordering::Relaxed);
        return Ok(());
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
    tracing::info!(
        "Bare repo path: {:?}, detected: {}",
        bare_repo_path,
        repo_detected
    );

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

    let result = rt.block_on(async_main(
        target_dir,
        bare_repo_path,
        repo_detected,
        regular_repo_path,
        tmux_available,
        wt_available,
    ));

    // Clean exit — tell watchdog to stop, restore terminal
    alive.store(false, Ordering::Relaxed);
    restore_terminal();

    // If the user chose "Restart now" after an in-app update, replace this
    // process with the freshly installed binary (preserving the original args).
    // Existing tmux/Claude sessions survive and are reconnected on startup.
    if let Ok(true) = result {
        restart_into_new_binary(original_exe);
    }

    result.map(|_| ())
}

/// Re-exec clawtree with the original arguments after an in-app update.
/// On success this never returns (the process image is replaced); on failure it
/// logs and falls through so the caller exits normally.
///
/// Programs are tried in order:
///   1. `argv[0]` when it's a bare command name (e.g. `clawtree`) — re-resolved
///      through PATH, so a PATH-installed binary picks up the freshly updated
///      file. This is the common case for installed users.
///   2. the absolute path captured at startup (where the update wrote the new
///      binary) — covers launches by path or off PATH (e.g. dev builds).
fn restart_into_new_binary(original_exe: std::io::Result<std::path::PathBuf>) {
    let args: Vec<String> = std::env::args().skip(1).collect();

    let mut candidates: Vec<std::ffi::OsString> = Vec::new();
    if let Some(argv0) = std::env::args_os().next() {
        if !argv0.to_string_lossy().contains('/') {
            candidates.push(argv0);
        }
    }
    if let Ok(path) = original_exe {
        candidates.push(path.into_os_string());
    }

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        for program in &candidates {
            // exec() only returns on failure — fall through to the next candidate.
            let _ = std::process::Command::new(program).args(&args).exec();
        }
    }

    #[cfg(not(unix))]
    {
        for program in &candidates {
            if std::process::Command::new(program)
                .args(&args)
                .spawn()
                .is_ok()
            {
                std::process::exit(0);
            }
        }
    }

    eprintln!("Could not restart automatically; please relaunch clawtree.");
}

async fn async_main(
    target_dir: std::path::PathBuf,
    bare_repo_path: std::path::PathBuf,
    repo_detected: bool,
    regular_repo_path: Option<std::path::PathBuf>,
    tmux_available: bool,
    wt_available: bool,
) -> Result<bool> {
    // Initialize terminal
    enable_raw_mode().context("Failed to enable raw mode")?;
    let mut stdout = io::stdout();
    crossterm::execute!(
        stdout,
        EnterAlternateScreen,
        EnableMouseCapture,
        EnableBracketedPaste
    )
    .context("Failed to enter alternate screen")?;
    // Enable keyboard enhancement if the terminal supports it (kitty protocol).
    // This allows detecting Shift+Enter, Ctrl+key combos, etc. Silently ignored
    // on terminals that don't support it.
    let _ = crossterm::execute!(
        io::stdout(),
        crossterm::event::PushKeyboardEnhancementFlags(
            crossterm::event::KeyboardEnhancementFlags::REPORT_EVENT_TYPES
                | crossterm::event::KeyboardEnhancementFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES,
        )
    );
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).context("Failed to create terminal")?;

    // Create event channel and app
    let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel::<AppEvent>();
    let mut app = App::new(
        bare_repo_path.clone(),
        event_tx.clone(),
        repo_detected,
        tmux_available,
        wt_available,
    );
    app.regular_repo_path = regular_repo_path;

    // Load skipped update version preference
    app.load_skipped_update_version();

    // Load extra locations from global config
    app.load_locations();

    // Auto-detect whether to open init or convert dialog
    if !repo_detected {
        if app.regular_repo_path.is_some() {
            app.auto_init_pending = Some(app::AutoInitKind::Convert);
        } else {
            app.auto_init_pending = Some(app::AutoInitKind::Init);
        }
    }

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
        .spawn(move || loop {
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
        })
        .context("Failed to spawn input reader thread")?;

    // ── Tick timer — plain OS thread ───────────────────────────────
    let tick_tx = event_tx.clone();
    std::thread::Builder::new()
        .name("tick-timer".into())
        .spawn(move || loop {
            std::thread::sleep(std::time::Duration::from_millis(TICK_RATE_MS));
            if tick_tx.send(AppEvent::Tick).is_err() {
                break;
            }
        })
        .context("Failed to spawn tick timer thread")?;

    // Load initial worktree data (only if repo exists)
    if repo_detected {
        if let Err(e) = worktree::refresh_worktrees(&mut app) {
            tracing::error!("Failed to load worktrees: {}", e);
            app.set_status(format!("Failed to load worktrees: {}", e));
        }

        // If the bare repo has no worktrees, try to recover by creating
        // the missing worktree from HEAD's branch. This handles the case
        // where a previous init/conversion created .bare but the worktree
        // add failed or was interrupted.
        if app.worktrees.is_empty() {
            tracing::warn!("Bare repo has no worktrees, attempting recovery");
            let recovered = (|| -> anyhow::Result<bool> {
                let bare_dir = app.bare_repo_path.join(".bare");
                let branch = worktree::git::current_branch_name(&app.bare_repo_path)
                    .or_else(|_| worktree::git::current_branch_name(&bare_dir))
                    .unwrap_or_else(|_| "main".to_string());

                // Ensure there's at least one commit
                let has_commit = std::process::Command::new("git")
                    .args(["rev-parse", "HEAD"])
                    .current_dir(&app.bare_repo_path)
                    .output()
                    .map(|o| o.status.success())
                    .unwrap_or(false);

                if !has_commit {
                    let _ = std::process::Command::new("git")
                        .args([
                            "-c",
                            "user.name=init",
                            "-c",
                            "user.email=init@init",
                            "commit",
                            "--allow-empty",
                            "-m",
                            "Initial commit",
                        ])
                        .env("GIT_DIR", &bare_dir)
                        .env("GIT_WORK_TREE", &app.bare_repo_path)
                        .current_dir(&app.bare_repo_path)
                        .output();
                }

                let output = std::process::Command::new("git")
                    .args(["worktree", "add", &branch])
                    .current_dir(&app.bare_repo_path)
                    .output()?;

                if output.status.success() {
                    worktree::refresh_worktrees(&mut app)?;
                    Ok(!app.worktrees.is_empty())
                } else {
                    Ok(false)
                }
            })()
            .unwrap_or(false);

            if !recovered {
                app.repo_detected = false;
                // Search from the original target dir (not bare_repo_path, which
                // may be a parent directory where detect_bare_repo found .bare/).
                app.regular_repo_path = worktree::git::detect_regular_repo(&target_dir);
                // If no regular repo found either, show init dialog
                if app.regular_repo_path.is_none() {
                    app.auto_init_pending = Some(app::AutoInitKind::Init);
                }
            }
        }

        if !app.worktrees.is_empty() {
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

            // Load saved prompt templates
            app.load_saved_prompts();

            // Auto-select the Project overview on startup
            app.sidebar_selected = 0; // Project is always index 0
            app.project_overview_active = true;
        }
    }

    // ── Tmux title poller — background thread ──────────────────────
    let tmux_session_info = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    if tmux_available {
        // Seed initial info
        if let Ok(mut info) = tmux_session_info.lock() {
            *info = session::collect_tmux_session_info(&app);
        }
        session::spawn_tmux_title_poller(
            event_tx.clone(),
            std::sync::Arc::clone(&tmux_session_info),
        );
        session::spawn_terminal_name_poller(
            event_tx.clone(),
            std::sync::Arc::clone(&tmux_session_info),
        );
        session::spawn_claude_usage_poller(
            event_tx.clone(),
            std::sync::Arc::clone(&tmux_session_info),
        );
    }

    // ── Global usage poller — background thread ────────────────
    // Shared activity timestamp (ms since epoch) the poller reads to decide its
    // cadence. Bumped on every keystroke and PTY output below. Seeded to "now"
    // so the poller treats startup as active and refreshes promptly.
    let last_activity = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(
        session::now_millis(),
    ));
    session::spawn_global_usage_poller(event_tx.clone(), std::sync::Arc::clone(&last_activity));

    // ── Update checker — one-shot background thread ──────────
    update::spawn_update_checker(event_tx.clone());

    // ── Worktree status poller — background thread ───────────────
    let status_poller_paths = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    if repo_detected {
        // Seed initial worktree paths
        if let Ok(mut paths) = status_poller_paths.lock() {
            *paths = worktree::collect_worktree_paths(&app);
        }
        worktree::spawn_status_poller(
            event_tx.clone(),
            std::sync::Arc::clone(&status_poller_paths),
            std::sync::Arc::clone(&app.git_lock),
        );
    }

    // ── Main event loop ────────────────────────────────────────────
    let mut needs_redraw = true;

    // Frame-rate ceiling. Redraws are coalesced so a session streaming a
    // firehose of output can't drive more than ~60 repaints/sec. The vt100
    // parser accumulates every byte regardless, so each repaint shows the
    // latest screen state — we drop only redundant repaints, never content.
    const MIN_FRAME: Duration = Duration::from_millis(16);
    let mut last_draw = Instant::now() - MIN_FRAME;

    // Unread tracking is suppressed during startup: attaching to existing tmux
    // sessions replays their buffered scrollback, firing PtyOutput for every
    // session, which would mark them all unread (green) on launch. We only begin
    // tracking once that initial burst settles — a quiet gap with no PtyOutput —
    // capped so a genuinely busy session can't hold it off forever.
    let started_at = Instant::now();
    let mut last_pty_output = started_at;
    let mut unread_tracking_enabled = false;
    const UNREAD_SETTLE_QUIET: Duration = Duration::from_millis(600);
    const UNREAD_MAX_STARTUP_GRACE: Duration = Duration::from_secs(5);

    // ── Optional frame profiler (CLAWTREE_PROFILE=1) ────────────────
    // Logs redraws/sec, PtyOutput events/sec, and mean draw + URL-scan time
    // to ~/.clawtree/logs/clawtree.log every ~2s. Off by default; zero cost
    // when disabled beyond a couple of branch checks.
    let profile = std::env::var("CLAWTREE_PROFILE").map(|v| v != "0").unwrap_or(false);
    let mut pty_output_events: u64 = 0;
    let mut prof_draws: u64 = 0;
    let mut prof_draw_time = Duration::ZERO;
    let mut prof_scan_time = Duration::ZERO;
    let mut prof_window = Instant::now();

    loop {
        if app.should_quit {
            break;
        }

        // A queued pending action blocks the loop while it runs, and relies on
        // this draw painting its loading overlay first — so it must bypass the
        // frame-rate cap (which would otherwise skip the draw right after the
        // keypress that queued it, leaving a stale screen during the blocking op).
        let force_frame = app.pending_action.is_some();
        if needs_redraw && (force_frame || last_draw.elapsed() >= MIN_FRAME) {
            // Refresh URL cache before rendering if dirty
            if app.url_cache_dirty {
                let scan_start = if profile { Some(Instant::now()) } else { None };
                // Only scan URLs on the live view (scroll == 0); when scrolled
                // the tmux-captured content doesn't match the vt100 screen.
                if app.terminal_scroll == 0 {
                    if let Some(sid) = app.active_session_id {
                        if let Some(session) = app.sessions.get(&sid) {
                            if let Ok(guard) = session.parser.try_read() {
                                let screen = guard.screen();
                                app.url_cache.urls = url::scan_urls_from_screen(screen);
                                app.url_cache.last_scan = Instant::now();
                            }
                        }
                    } else {
                        app.url_cache.urls.clear();
                    }
                } else {
                    app.url_cache.urls.clear();
                    app.url_cache.hovered = None;
                }
                app.url_cache_dirty = false;
                if let Some(s) = scan_start {
                    prof_scan_time += s.elapsed();
                }
            }
            let draw_start = if profile { Some(Instant::now()) } else { None };
            terminal.draw(|f| ui::draw(f, &app))?;
            needs_redraw = false;
            last_draw = Instant::now();
            if let Some(d) = draw_start {
                prof_draw_time += d.elapsed();
                prof_draws += 1;
            }
        }

        // Emit a profiling summary roughly every 2 seconds.
        if profile && prof_window.elapsed() >= Duration::from_secs(2) {
            let secs = prof_window.elapsed().as_secs_f64();
            let mean_draw = if prof_draws > 0 {
                prof_draw_time.as_secs_f64() * 1000.0 / prof_draws as f64
            } else {
                0.0
            };
            let mean_scan = if prof_draws > 0 {
                prof_scan_time.as_secs_f64() * 1000.0 / prof_draws as f64
            } else {
                0.0
            };
            tracing::info!(
                "PROFILE sessions={} draws/s={:.1} pty_out/s={:.1} mean_draw={:.2}ms mean_scan={:.2}ms",
                app.sessions.len(),
                prof_draws as f64 / secs,
                pty_output_events as f64 / secs,
                mean_draw,
                mean_scan,
            );
            pty_output_events = 0;
            prof_draws = 0;
            prof_draw_time = Duration::ZERO;
            prof_scan_time = Duration::ZERO;
            prof_window = Instant::now();
        }

        // Execute pending actions after drawing the loading overlay
        if let Some(action) = app.pending_action.take() {
            let wt_count_before = app.worktrees.len();
            let git_lock = std::sync::Arc::clone(&app.git_lock);
            let _git_guard = git_lock.lock().unwrap_or_else(|e| e.into_inner());
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
            Ok(None) => break,  // all senders dropped
            Err(_) => continue, // timeout, loop back
        };

        let mut current_event = Some(first_event);
        while let Some(event) = current_event.take().or_else(|| event_rx.try_recv().ok()) {
            if app.should_quit {
                break;
            }
            match event {
                AppEvent::Input(CrosstermEvent::Key(key)) => {
                    last_activity.store(session::now_millis(), std::sync::atomic::Ordering::Relaxed);
                    let session_count_before = app.sessions.len();
                    let worktree_count_before = app.worktrees.len();
                    let scroll_before = app.terminal_scroll;
                    let size = terminal.size()?;
                    app.last_terminal_size = (size.width, size.height);
                    keys::handle_key(&mut app, key, (size.width, size.height));
                    if app.terminal_scroll != scroll_before {
                        app.url_cache_dirty = true;
                    }
                    // Re-enable mouse capture on any keypress (user is done
                    // with native text selection if it was active).
                    if !app.mouse_captured {
                        app.mouse_captured = true;
                        app.mouse_capture_disabled_at = None;
                        let _ = crossterm::execute!(io::stdout(), EnableMouseCapture);
                    }
                    // Clear any in-progress text selection on keypress
                    app.text_selection = None;
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
                        let next_refresh = app
                            .next_status_refresh
                            .unwrap_or_else(|| Instant::now() + worktree::STATUS_REFRESH_INTERVAL);
                        let git_lock = std::sync::Arc::clone(&app.git_lock);
                        tokio::task::spawn_blocking(move || {
                            // Use try_lock to avoid blocking if a user operation is running
                            if let Ok(_guard) = git_lock.try_lock() {
                                if let Ok(status) = worktree::fetch_worktree_status_by_path(&path) {
                                    let _ = tx.send(AppEvent::WorktreeStatusReady {
                                        worktree_path: path,
                                        status,
                                        next_refresh_at: next_refresh,
                                    });
                                }
                            }
                        });
                    }
                    needs_redraw = true;
                }
                AppEvent::Input(CrosstermEvent::Mouse(mouse)) => {
                    // An application that enabled mouse reporting (Claude Code's
                    // fullscreen renderer, vim, …) gets clicks and drags itself.
                    if mouse::forward_to_app(&mut app, mouse) {
                        needs_redraw = true;
                        continue;
                    }
                    match mouse.kind {
                        MouseEventKind::Down(MouseButton::Left) => {
                            // Clear any existing selection on new click
                            app.text_selection = None;

                            if mouse.modifiers.contains(EvtKeyModifiers::SHIFT) {
                                // Shift+Click → native text selection (escape hatch)
                                app.mouse_captured = false;
                                app.mouse_capture_disabled_at = Some(Instant::now());
                                let _ = crossterm::execute!(io::stdout(), DisableMouseCapture);
                            } else if mouse::is_terminal_session_area(&app, mouse.column, mouse.row)
                            {
                                // Check if clicking a URL first (only in live view)
                                if app.terminal_scroll == 0 {
                                    if let Some(idx) =
                                        mouse::url_at_position(&app, mouse.column, mouse.row)
                                    {
                                        if let Some(detected) = app.url_cache.urls.get(idx) {
                                            let url = detected.url.clone();
                                            match url::open_url_in_browser(&url) {
                                                Ok(()) => {
                                                    app.set_status(format!("Opened: {}", url))
                                                }
                                                Err(e) => app.set_status_with(
                                                    app::StatusSeverity::Error,
                                                    format!("Failed to open URL: {}", e),
                                                ),
                                            }
                                        }
                                        // URL click handled; skip selection start
                                        needs_redraw = true;
                                        continue;
                                    }
                                }
                                // Start in-app text selection (works in both live and scrollback)
                                if let Some(pos) =
                                    mouse::screen_to_vt100(&app, mouse.column, mouse.row)
                                {
                                    app.text_selection = Some(app::TextSelection {
                                        anchor: pos,
                                        endpoint: pos,
                                    });
                                }
                                // Set focus without resetting scroll
                                app.focus = app::FocusTarget::TerminalPane;
                                app.input_mode = app::InputMode::Terminal;
                            } else {
                                // Click in sidebar/other panels
                                mouse::handle_mouse(&mut app, mouse);
                            }
                            needs_redraw = true;
                        }
                        #[allow(clippy::collapsible_match)]
                        MouseEventKind::Drag(MouseButton::Left) => {
                            // Update text selection endpoint while dragging.
                            // Use clamped variant so dragging outside the pane
                            // extends the selection to the edge instead of freezing.
                            // Note: split borrows intentional — mutable borrow of
                            // text_selection cannot overlap with &app passed to
                            // screen_to_vt100_clamped.
                            if app.text_selection.is_some() {
                                if let Some(pos) =
                                    mouse::screen_to_vt100_clamped(&app, mouse.column, mouse.row)
                                {
                                    if let Some(ref mut sel) = app.text_selection {
                                        sel.endpoint = pos;
                                    }
                                    needs_redraw = true;
                                }
                            }
                        }
                        MouseEventKind::Up(MouseButton::Left) => {
                            // Finish text selection: extract text and copy to clipboard
                            if let Some(sel) = app.text_selection.take() {
                                // Only copy if there's a meaningful selection (not just a click)
                                if sel.anchor != sel.endpoint {
                                    let (start, end) = sel.ordered();
                                    let extracted = if app.terminal_scroll > 0 {
                                        // Scrollback: extract from tmux plain-text capture
                                        app.active_session_id.and_then(|sid| {
                                            let session = app.sessions.get(&sid)?;
                                            let tmux_name = session.tmux_session_name.as_ref()?;
                                            let inner = app.areas.terminal_pane_inner.get();
                                            let visible_rows = inner.height as usize;
                                            let history =
                                                ui::terminal_pane::tmux_history_size(tmux_name);
                                            let eff = app.terminal_scroll.min(history);
                                            let s = -(eff as i64);
                                            let e = s + visible_rows as i64 - 1;
                                            let content =
                                                ui::terminal_pane::capture_tmux_pane_plain(
                                                    tmux_name, s, e,
                                                )?;
                                            let text = mouse::extract_text_from_plain(
                                                &content, start, end,
                                            );
                                            if text.is_empty() {
                                                None
                                            } else {
                                                Some(text)
                                            }
                                        })
                                    } else {
                                        // Live view: extract from vt100 screen
                                        app.active_session_id.and_then(|sid| {
                                            let session = app.sessions.get(&sid)?;
                                            let guard = session.parser.try_read().ok()?;
                                            let screen = guard.screen();
                                            let text =
                                                mouse::extract_text_from_screen(screen, start, end);
                                            if text.is_empty() {
                                                None
                                            } else {
                                                Some(text)
                                            }
                                        })
                                    };
                                    if let Some(text) = extracted {
                                        match mouse::copy_to_clipboard(&text) {
                                            Ok(()) => {
                                                app.set_status_with(
                                                    app::StatusSeverity::Success,
                                                    format!("Copied {} chars", text.len()),
                                                );
                                            }
                                            Err(e) => {
                                                app.set_status_with(
                                                    app::StatusSeverity::Error,
                                                    format!("Clipboard error: {}", e),
                                                );
                                            }
                                        }
                                    }
                                }
                                needs_redraw = true;
                            }
                        }
                        MouseEventKind::Down(MouseButton::Right) => {
                            app.text_selection = None;
                            // Right-click in sidebar → context menu
                            let sidebar_area = app.areas.sidebar.get();
                            if !app.show_help
                                && app.dialog.is_none()
                                && app.screen_mode == ScreenMode::Normal
                                && mouse::point_in_rect(mouse.column, mouse.row, sidebar_area)
                            {
                                app.focus = app::FocusTarget::Sidebar;
                                app.input_mode = app::InputMode::Normal;
                                mouse::handle_sidebar_right_click(
                                    &mut app,
                                    mouse.column,
                                    mouse.row,
                                );
                            } else {
                                // Elsewhere → escape hatch for native text selection
                                app.mouse_captured = false;
                                app.mouse_capture_disabled_at = Some(Instant::now());
                                let _ = crossterm::execute!(io::stdout(), DisableMouseCapture);
                            }
                            needs_redraw = true;
                        }
                        MouseEventKind::Down(MouseButton::Middle) => {
                            // Middle click → escape hatch for native text selection
                            app.text_selection = None;
                            app.mouse_captured = false;
                            app.mouse_capture_disabled_at = Some(Instant::now());
                            let _ = crossterm::execute!(io::stdout(), DisableMouseCapture);
                            needs_redraw = true;
                        }
                        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                            app.text_selection = None;
                            let scroll_before = app.terminal_scroll;
                            mouse::handle_mouse(&mut app, mouse);
                            if app.terminal_scroll != scroll_before {
                                app.url_cache_dirty = true;
                            }
                            needs_redraw = true;
                        }
                        MouseEventKind::Moved => {
                            let old_hovered = app.url_cache.hovered;
                            app.url_cache.hovered =
                                mouse::url_at_position(&app, mouse.column, mouse.row);
                            if app.url_cache.hovered != old_hovered {
                                needs_redraw = true;
                            }

                            // Track context menu hover (update selected to follow mouse)
                            if let Some(app::Dialog::ContextMenu {
                                ref actions,
                                ref mut selected,
                                ..
                            }) = app.dialog
                            {
                                let dialog_area = app.areas.dialog.get();
                                let new_sel =
                                    if mouse::point_in_rect(mouse.column, mouse.row, dialog_area) {
                                        let inner_y = dialog_area.y + 1;
                                        if mouse.row >= inner_y {
                                            let action_row = (mouse.row - inner_y) as usize;
                                            if action_row < actions.len() {
                                                Some(action_row)
                                            } else {
                                                None
                                            }
                                        } else {
                                            None
                                        }
                                    } else {
                                        None
                                    };
                                if *selected != new_sel {
                                    *selected = new_sel;
                                    needs_redraw = true;
                                }
                            }

                            // Track sidebar hover (only when no overlay is open)
                            if !app.show_help
                                && app.dialog.is_none()
                                && app.screen_mode == ScreenMode::Normal
                            {
                                let old_sidebar_hover = app.sidebar_hovered;
                                let old_tp_hover = app.terminal_panel_hovered;

                                let sidebar_inner = app.areas.sidebar_inner.get();
                                let tp_inner = app.areas.sidebar_terminal_panel_inner.get();

                                if mouse::point_in_rect(mouse.column, mouse.row, sidebar_inner) {
                                    let item_row = (mouse.row - sidebar_inner.y) as usize
                                        + app.sidebar_scroll;
                                    app.sidebar_hovered = if item_row < app.sidebar_items.len() {
                                        Some(item_row)
                                    } else {
                                        None
                                    };
                                    app.terminal_panel_hovered = None;
                                } else if mouse::point_in_rect(mouse.column, mouse.row, tp_inner) {
                                    let item_row = (mouse.row - tp_inner.y) as usize;
                                    app.terminal_panel_hovered =
                                        if item_row < app.terminal_panel_items.len() {
                                            Some(item_row)
                                        } else {
                                            None
                                        };
                                    app.sidebar_hovered = None;
                                } else {
                                    app.sidebar_hovered = None;
                                    app.terminal_panel_hovered = None;
                                }

                                if app.sidebar_hovered != old_sidebar_hover
                                    || app.terminal_panel_hovered != old_tp_hover
                                {
                                    needs_redraw = true;
                                }
                            } else if app.sidebar_hovered.is_some()
                                || app.terminal_panel_hovered.is_some()
                            {
                                app.sidebar_hovered = None;
                                app.terminal_panel_hovered = None;
                                needs_redraw = true;
                            }
                        }
                        _ => {}
                    }
                }
                AppEvent::Input(CrosstermEvent::Paste(data)) => {
                    // Re-enable mouse capture on paste (user finished selecting).
                    if !app.mouse_captured {
                        app.mouse_captured = true;
                        app.mouse_capture_disabled_at = None;
                        let _ = crossterm::execute!(io::stdout(), EnableMouseCapture);
                    }
                    keys::handle_paste(&mut app, data);
                    needs_redraw = true;
                }
                AppEvent::Input(CrosstermEvent::Resize(w, h)) => {
                    tracing::info!("RESIZE-EVENT from crossterm: {}x{}", w, h);
                    app.last_terminal_size = (w, h);
                    session::resize_all(&app, h, w);
                    app.text_selection = None;
                    // Some terminals (e.g. Windows Terminal under WSL) reset DEC
                    // private modes on resize, silently dropping mouse tracking and
                    // bracketed paste. Our state still believes they're enabled, so
                    // we'd never re-issue the escapes and the scroll wheel would fall
                    // through to the host terminal's scrollback. Re-assert the modes
                    // we own. Skip mouse capture only while the user has it disabled
                    // for the native text-selection escape hatch.
                    if app.mouse_captured {
                        let _ = crossterm::execute!(io::stdout(), EnableMouseCapture);
                    }
                    let _ = crossterm::execute!(io::stdout(), EnableBracketedPaste);
                    needs_redraw = true;
                }
                AppEvent::Input(_) => {}
                AppEvent::PtyOutput { session_id } => {
                    last_activity.store(session::now_millis(), std::sync::atomic::Ordering::Relaxed);
                    pty_output_events = pty_output_events.wrapping_add(1);
                    // The session's vt100 parser was already updated by its reader
                    // thread, independent of any redraw. We only repaint when the
                    // output belongs to the *active* session — this instance can be
                    // attached to dozens of sessions, and letting every background
                    // session force a full redraw + URL rescan on each chunk is a
                    // storm that scales with the attached-session count. Background
                    // titles/activity indicators still refresh via the 30fps tick.
                    last_pty_output = Instant::now();
                    if app.active_session_id == Some(session_id) {
                        app.url_cache_dirty = true;
                        needs_redraw = true;
                    }
                    // Unread is NOT marked here: raw PTY output includes status-bar
                    // churn (spinner timer, token counts) that isn't new content.
                    // We flag unread on the Working→done transition instead — see the
                    // agent-tracking loop in the Tick handler.
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
                        Some(e) => app.set_status_with(
                            StatusSeverity::Error,
                            format!("Error creating '{}': {}", branch, e),
                        ),
                        None => {
                            app.set_status_with(
                                StatusSeverity::Success,
                                format!("Created worktree '{}'", branch),
                            );
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
                                app.set_status_with(
                                    StatusSeverity::Error,
                                    format!("Push '{}' — authentication needed", branch),
                                );
                                app.open_dialog(Dialog::AuthError {
                                    operation: "push".to_string(),
                                    message: e,
                                    selected: 0,
                                });
                            } else {
                                app.set_status_with(
                                    StatusSeverity::Error,
                                    format!("Push '{}' failed: {}", branch, e),
                                );
                            }
                        }
                        None => {
                            app.set_status_with(
                                StatusSeverity::Success,
                                format!("Pushed '{}'", branch),
                            );
                            app.refresh_worktree_status();
                        }
                    }
                    needs_redraw = true;
                }
                AppEvent::PullComplete {
                    branch,
                    worktree_idx,
                    error,
                    has_conflicts,
                } => {
                    match (error, has_conflicts) {
                        (None, _) => {
                            app.set_status_with(
                                StatusSeverity::Success,
                                format!("Pulled '{}'", branch),
                            );
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
                                app.set_status_with(
                                    StatusSeverity::Error,
                                    "Clone failed — authentication needed",
                                );
                                app.open_dialog(Dialog::AuthError {
                                    operation: "clone".to_string(),
                                    message: e,
                                    selected: 0,
                                });
                            } else {
                                app.set_status_with(
                                    StatusSeverity::Error,
                                    format!("Init error: {}", e),
                                );
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
                                std::sync::Arc::clone(&app.git_lock),
                            );
                        }
                    }
                    needs_redraw = true;
                }
                AppEvent::ConvertRepoComplete {
                    bare_repo_path: new_path,
                    error,
                } => {
                    match error {
                        Some(e) => app.set_status_with(
                            StatusSeverity::Error,
                            format!("Convert error: {}", e),
                        ),
                        None => {
                            app.bare_repo_path = new_path;
                            app.repo_detected = true;
                            app.regular_repo_path = None;
                            app.set_status_with(
                                StatusSeverity::Success,
                                "Repository converted to bare worktree layout!",
                            );
                            let _ = worktree::refresh_worktrees(&mut app);
                            // Start the status poller now that we have a repo
                            if let Ok(mut paths) = status_poller_paths.lock() {
                                *paths = worktree::collect_worktree_paths(&app);
                            }
                            worktree::spawn_status_poller(
                                event_tx.clone(),
                                std::sync::Arc::clone(&status_poller_paths),
                                std::sync::Arc::clone(&app.git_lock),
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
                AppEvent::TerminalNamesUpdated { updates } => {
                    for (session_id, name) in updates {
                        app.terminal_names.insert(session_id, name);
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
                AppEvent::SummaryReady {
                    session_id,
                    summary,
                } => {
                    app.agent_summaries.insert(session_id, summary);
                    needs_redraw = true;
                }
                AppEvent::ClaudeCommitMessageReady {
                    worktree_idx,
                    message,
                } => {
                    // Only apply if the dialog is still open and in GeneratingMessage phase
                    let applies = matches!(
                        &app.dialog,
                        Some(Dialog::GitCommit { worktree_idx: wi, phase, .. })
                        if *wi == worktree_idx && *phase == CommitPhase::GeneratingMessage
                    );
                    if applies {
                        match message {
                            Ok(msg) => {
                                if let Some(Dialog::GitCommit {
                                    ref mut commit_message,
                                    ref mut phase,
                                    ref mut cursor_pos,
                                    ..
                                }) = app.dialog
                                {
                                    *commit_message = msg;
                                    *cursor_pos = commit_message.chars().count();
                                    *phase = CommitPhase::Message;
                                }
                            }
                            Err(e) => {
                                if let Some(Dialog::GitCommit { ref mut phase, .. }) = app.dialog {
                                    *phase = CommitPhase::Message;
                                }
                                app.set_status(format!("AI message failed: {}", e));
                            }
                        }
                    }
                    needs_redraw = true;
                }
                AppEvent::UpdateAvailable { latest_version } => {
                    app.update_available = Some(latest_version.clone());
                    // Show the update dialog if not skipped, not already shown, and no dialog open
                    let is_skipped =
                        app.skipped_update_version.as_deref() == Some(latest_version.as_str());
                    if !is_skipped && !app.update_dialog_shown && app.dialog.is_none() {
                        app.update_dialog_shown = true;
                        app.open_dialog(Dialog::UpdateAvailable {
                            latest_version,
                            selected: 0,
                            phase: UpdatePhase::Prompt,
                        });
                    }
                    needs_redraw = true;
                }
                AppEvent::UpdateDownloadComplete { result, version } => {
                    match result {
                        Ok(binary_path) => {
                            // Transition dialog to Replacing phase
                            if let Some(Dialog::UpdateAvailable {
                                ref latest_version, ..
                            }) = app.dialog
                            {
                                let ver = latest_version.clone();
                                app.dialog = Some(Dialog::UpdateAvailable {
                                    latest_version: ver,
                                    selected: 0,
                                    phase: UpdatePhase::Replacing,
                                });
                            }
                            update::spawn_update_replace(binary_path, version, event_tx.clone());
                        }
                        Err(e) => {
                            if let Some(Dialog::UpdateAvailable {
                                ref latest_version, ..
                            }) = app.dialog
                            {
                                let ver = latest_version.clone();
                                app.dialog = Some(Dialog::UpdateAvailable {
                                    latest_version: ver,
                                    selected: 0,
                                    phase: UpdatePhase::Failed(e),
                                });
                            }
                        }
                    }
                    needs_redraw = true;
                }
                AppEvent::UpdateReplaceComplete { result, version } => {
                    match result {
                        Ok(()) => {
                            // The new binary is installed but this process is still
                            // running the old one. Prompt the user to restart (or do
                            // it for them) so the update actually takes effect.
                            app.dialog = Some(Dialog::UpdateAvailable {
                                latest_version: version,
                                selected: 0,
                                phase: UpdatePhase::Complete,
                            });
                            app.input_mode = app::InputMode::Dialog;
                            // Clear the update indicator since we've updated
                            app.update_available = None;
                        }
                        Err(e) => {
                            if let Some(Dialog::UpdateAvailable {
                                ref latest_version, ..
                            }) = app.dialog
                            {
                                let ver = latest_version.clone();
                                app.dialog = Some(Dialog::UpdateAvailable {
                                    latest_version: ver,
                                    selected: 0,
                                    phase: UpdatePhase::Failed(e),
                                });
                            }
                        }
                    }
                    needs_redraw = true;
                }
                AppEvent::WorktreeStatusReady {
                    worktree_path,
                    status,
                    next_refresh_at,
                } => {
                    // Update the per-worktree cache
                    app.worktree_statuses
                        .insert(worktree_path.clone(), status.clone());
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
                    // Enable unread tracking once the startup replay burst settles
                    // (a quiet gap with no PtyOutput) or the max grace elapses.
                    if !unread_tracking_enabled
                        && (last_pty_output.elapsed() > UNREAD_SETTLE_QUIET
                            || started_at.elapsed() > UNREAD_MAX_STARTUP_GRACE)
                    {
                        unread_tracking_enabled = true;
                    }

                    // Auto-trigger init/convert dialog on first tick
                    if let Some(kind) = app.auto_init_pending.take() {
                        match kind {
                            app::AutoInitKind::Convert => {
                                if let Some(ref repo_path) = app.regular_repo_path {
                                    let branch = worktree::git::current_branch_name(repo_path)
                                        .unwrap_or_else(|_| "main".to_string());
                                    let source = repo_path.clone();
                                    app.open_dialog(Dialog::ConvertRepo {
                                        mode: 0,
                                        target_path_input: String::new(),
                                        branch_name: branch,
                                        focused_field: 0,
                                        source_repo_path: source,
                                        confirmed: false,
                                    });
                                }
                            }
                            app::AutoInitKind::Init => {
                                app.open_dialog(Dialog::InitRepo {
                                    url_input: String::new(),
                                    branch_input: "main".to_string(),
                                    focused_field: 0,
                                });
                            }
                        }
                    }

                    // Auto-re-enable mouse capture after text selection timeout.
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

                    // ── Agent tracking (~3/sec, on every 10th tick) ──
                    // Fast enough that a one-second turn still shows its activity
                    // line on at least one sample, so its Working→Idle edge fires.
                    if app.spinner_frame.is_multiple_of(10) {
                        // Detect Working→Idle transitions and capture summaries.
                        // Session::is_active() already latches through the
                        // streaming phase and repaint blips (see working_latch).
                        let sids: Vec<u64> = app.sessions.keys().copied().collect();
                        let mut newly_done: Vec<u64> = Vec::new();
                        for sid in &sids {
                            if let Some(session) = app.sessions.get(sid) {
                                let currently_active = session.is_active();
                                let was_active = session.was_active;
                                let exited =
                                    session.exited.load(std::sync::atomic::Ordering::Relaxed);
                                if was_active != currently_active {
                                    tracing::debug!(
                                        sid,
                                        was_active,
                                        currently_active,
                                        exited,
                                        viewing = app.active_session_id == Some(*sid),
                                        unread_tracking_enabled,
                                        "agent activity edge"
                                    );
                                }

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
                                    // Claude just finished producing output. If the user
                                    // isn't viewing this session, flag it unread (green)
                                    // so they know there's new text to read.
                                    if unread_tracking_enabled
                                        && app.active_session_id != Some(*sid)
                                    {
                                        tracing::debug!(sid, "finished while not viewed → unread");
                                        newly_done.push(*sid);
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
                        // (The Tick handler repaints unconditionally below.)
                        for sid in &newly_done {
                            if let Some(session) = app.sessions.get_mut(sid) {
                                session.unread = true;
                            }
                        }
                    }

                    // Auto-send next queued prompt when Claude is idle.
                    // Checks ALL sessions with queues, not just the active one.
                    // Throttled to ~1 check/sec to avoid hammering tmux.
                    if app.dialog.is_none() && app.loading_message.is_none() {
                        let cooldown_ok = app
                            .prompt_queue_last_send
                            .map(|t| t.elapsed() > Duration::from_secs(5))
                            .unwrap_or(true);
                        let check_ok = app
                            .prompt_queue_last_check
                            .map(|t| t.elapsed() > Duration::from_millis(1000))
                            .unwrap_or(true);

                        if cooldown_ok && check_ok {
                            app.prompt_queue_last_check = Some(Instant::now());
                            // Phase 1: find a session ready to receive a prompt (immutable borrows)
                            let candidates: Vec<u64> = app
                                .prompt_queues
                                .iter()
                                .filter(|(_, q)| !q.is_empty())
                                .map(|(sid, _)| *sid)
                                .collect();

                            let mut send_to: Option<(
                                u64,
                                Option<String>,
                                tokio::sync::mpsc::UnboundedSender<bytes::Bytes>,
                            )> = None;
                            for sid in &candidates {
                                if let Some(session) = app.sessions.get(sid) {
                                    if !session.exited.load(Ordering::Relaxed)
                                        && !session.is_active()
                                    {
                                        let settled = session
                                            .last_output
                                            .read()
                                            .ok()
                                            .map(|t| t.elapsed() > Duration::from_secs(2))
                                            .unwrap_or(false);
                                        if settled {
                                            send_to = Some((
                                                *sid,
                                                session.tmux_session_name.clone(),
                                                session.write_tx.clone(),
                                            ));
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
                                                    .args([
                                                        "send-keys",
                                                        "-t",
                                                        &tmux,
                                                        "-l",
                                                        &prompt_clone,
                                                    ])
                                                    .output();
                                                tokio::time::sleep(Duration::from_millis(100))
                                                    .await;
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

    Ok(app.restart_requested)
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
        PendingAction::StageAll {
            worktree_idx,
            then_claude,
        } => {
            if let Err(e) = worktree::stage_all(app, worktree_idx) {
                app.set_status(format!("Failed to stage all: {}", e));
            }
            if matches!(app.dialog, Some(Dialog::GitCommit { .. })) {
                refresh_git_commit_dialog(app);
                if then_claude {
                    keys::handle_git_commit_claude_message(app);
                }
            } else {
                app.refresh_worktree_status();
            }
        }
        PendingAction::Commit {
            worktree_idx,
            message,
        } => {
            match worktree::commit(app, worktree_idx, &message) {
                Ok(()) => {
                    let _ = worktree::refresh_worktrees(app);
                    app.refresh_worktree_status();
                    // If a merge was waiting on this commit, re-queue it
                    if let Some(merge_action) = app.pending_merge.take() {
                        app.set_status("Committed — merging...");
                        app.queue_action("Merging...", merge_action);
                    } else {
                        app.set_status_with(
                            StatusSeverity::Success,
                            "Changes committed successfully",
                        );
                    }
                }
                Err(e) => {
                    app.set_status_with(StatusSeverity::Error, format!("Commit failed: {}", e));
                }
            }
        }
        PendingAction::CommitAndPush {
            worktree_idx,
            message,
        } => {
            match worktree::commit(app, worktree_idx, &message) {
                Ok(()) => {
                    let _ = worktree::refresh_worktrees(app);
                    app.refresh_worktree_status();
                    app.set_status_with(StatusSeverity::Success, "Committed — pushing...");
                    // Trigger push on a background thread
                    if let Some(wt) = app.worktrees.get(worktree_idx) {
                        let path = wt.path.clone();
                        let branch = wt.branch.clone();
                        let tx = app.event_tx.clone();
                        let git_lock = std::sync::Arc::clone(&app.git_lock);
                        std::thread::Builder::new()
                            .name("git-push".into())
                            .spawn(move || {
                                let _guard = git_lock.lock().unwrap_or_else(|e| e.into_inner());
                                let result = worktree::git::push_branch(&path, &branch);
                                let error = result.err().map(|e| format!("{}", e));
                                let _ = tx.send(AppEvent::PushComplete { branch, error });
                            })
                            .ok();
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
                            let status = if c.index_status == '?' {
                                '?'
                            } else {
                                c.work_status
                            };
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
                        cursor_pos: 0,
                    });
                }
                Err(e) => {
                    app.set_status(format!("Failed to get status: {}", e));
                }
            }
        }
        PendingAction::StageAllAndCommitClaude { worktree_idx } => {
            // Stage all files first
            if let Err(e) = worktree::stage_all(app, worktree_idx) {
                app.set_status(format!("Failed to stage all: {}", e));
                return;
            }
            // Now open the commit dialog with everything staged
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
                            let status = if c.index_status == '?' {
                                '?'
                            } else {
                                c.work_status
                            };
                            unstaged.push((status, c.path.clone()));
                        }
                    }
                    if staged.is_empty() {
                        app.set_status("No staged files after stage all");
                        return;
                    }
                    app.open_dialog(Dialog::GitCommit {
                        worktree_idx,
                        unstaged,
                        staged,
                        section: 0,
                        selected: 0,
                        phase: CommitPhase::Staging,
                        commit_message: String::new(),
                        cursor_pos: 0,
                    });
                    // Immediately trigger Claude commit message generation
                    keys::handle_git_commit_claude_message(app);
                }
                Err(e) => {
                    app.set_status(format!("Failed to get status: {}", e));
                }
            }
        }
        PendingAction::MergeExecute {
            source_worktree_idx,
            target_branch,
        } => {
            let source_name = app
                .worktrees
                .get(source_worktree_idx)
                .map(|w| w.branch.clone())
                .unwrap_or_default();

            // Guard: source worktree must not have an in-progress merge
            if let Some(conflict_branch) = worktree::merge_in_progress(app, source_worktree_idx) {
                app.set_status(format!(
                    "'{}' has an unresolved merge — resolve or abort first",
                    source_name
                ));
                app.open_dialog(Dialog::MergeConflict {
                    worktree_idx: source_worktree_idx,
                    source_branch: conflict_branch,
                    selected: 0,
                });
                return;
            }

            // Check source worktree is clean before merging
            match worktree::is_worktree_clean(app, source_worktree_idx) {
                Ok(false) => {
                    // Remember the merge so we can retry after commit
                    app.pending_merge = Some(PendingAction::MergeExecute {
                        source_worktree_idx,
                        target_branch: target_branch.clone(),
                    });
                    // Open the commit UI for the dirty source worktree
                    match worktree::status_porcelain(app, source_worktree_idx) {
                        Ok(changes) => {
                            let files: Vec<(String, String)> = changes
                                .iter()
                                .map(|c| {
                                    (
                                        format!("{}{}", c.index_status, c.work_status),
                                        c.path.clone(),
                                    )
                                })
                                .collect();
                            app.set_status(format!(
                                "'{}' has uncommitted changes — commit before merging",
                                source_name
                            ));
                            app.open_dialog(Dialog::DirtyWorktree {
                                worktree_idx: source_worktree_idx,
                                files,
                                selected: 0,
                            });
                        }
                        Err(e) => {
                            app.pending_merge = None;
                            app.set_status(format!(
                                "Failed to get status of '{}': {}",
                                source_name, e
                            ));
                        }
                    }
                    return;
                }
                Err(e) => {
                    app.set_status(format!(
                        "Failed to check status of '{}': {}",
                        source_name, e
                    ));
                    return;
                }
                Ok(true) => {}
            }

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
                        app.set_status(format!(
                            "Failed to create worktree for '{}': {}",
                            target_branch, e
                        ));
                        return;
                    }
                    if let Err(e) = worktree::refresh_worktrees(app) {
                        app.set_status(format!("Failed to refresh: {}", e));
                        return;
                    }
                    match worktree::find_worktree_for_branch(app, &target_branch) {
                        Some(idx) => idx,
                        None => {
                            app.set_status(format!(
                                "Could not find worktree for '{}'",
                                target_branch
                            ));
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
                                    let status = if c.index_status == '?' {
                                        '?'
                                    } else {
                                        c.work_status
                                    };
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
                                cursor_pos: 0,
                            });
                        }
                        Err(e) => {
                            app.pending_merge = None;
                            app.set_status(format!(
                                "Failed to get status of '{}': {}",
                                target_branch, e
                            ));
                        }
                    }
                    return;
                }
                Err(e) => {
                    app.set_status(format!(
                        "Failed to check status of '{}': {}",
                        target_branch, e
                    ));
                    return;
                }
                Ok(true) => {}
            }

            match worktree::merge_into_worktree(app, target_wt_idx, &source_name) {
                Ok(worktree::git::MergeResult::Success(output)) => {
                    let detail = output.lines().next().unwrap_or("ok").trim().to_string();
                    let msg = format!(
                        "Merged '{}' into '{}': {}",
                        source_name, target_branch, detail
                    );
                    app.set_status(msg.clone());
                    let _ = worktree::refresh_worktrees(app);
                    app.refresh_worktree_status();
                    app.open_dialog(Dialog::MergeSuccess { message: msg });
                }
                Ok(worktree::git::MergeResult::Conflict) => {
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
    let (worktree_idx, old_section, old_selected, phase, commit_message, cursor_pos) =
        match &app.dialog {
            Some(Dialog::GitCommit {
                worktree_idx,
                section,
                selected,
                phase,
                commit_message,
                cursor_pos,
                ..
            }) => (
                *worktree_idx,
                *section,
                *selected,
                *phase,
                commit_message.clone(),
                *cursor_pos,
            ),
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
            let status = if c.index_status == '?' {
                '?'
            } else {
                c.work_status
            };
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

    let len = if section == 0 {
        unstaged.len()
    } else {
        staged.len()
    };
    let selected = if len == 0 {
        0
    } else {
        old_selected.min(len - 1)
    };

    app.dialog = Some(Dialog::GitCommit {
        worktree_idx,
        unstaged,
        staged,
        section,
        selected,
        phase,
        commit_message,
        cursor_pos,
    });
}

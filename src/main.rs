mod app;
mod event;
mod keys;
mod session;
mod ui;
mod worktree;

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::{Context, Result};
use crossterm::event::{DisableMouseCapture, EnableMouseCapture, Event as CrosstermEvent};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use tracing_subscriber::EnvFilter;

use crate::app::App;
use crate::event::AppEvent;

/// Global flag set by the signal handler, read by the watchdog thread.
static SIGNAL_RECEIVED: AtomicBool = AtomicBool::new(false);

/// Restore the terminal to a usable state. Safe to call multiple times.
fn restore_terminal() {
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
    let bare_repo_path = worktree::git::detect_bare_repo(&target_dir).unwrap_or_else(|| {
        tracing::warn!("No bare repo detected at {:?}, using it as-is", target_dir);
        target_dir.clone()
    });
    tracing::info!("Bare repo path: {:?}", bare_repo_path);

    // Build tokio runtime manually so we control shutdown
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("Failed to create tokio runtime")?;

    let result = rt.block_on(async_main(bare_repo_path));

    // Clean exit — tell watchdog to stop, restore terminal
    alive.store(false, Ordering::Relaxed);
    restore_terminal();

    result
}

async fn async_main(bare_repo_path: std::path::PathBuf) -> Result<()> {
    // Initialize terminal
    enable_raw_mode().context("Failed to enable raw mode")?;
    let mut stdout = io::stdout();
    crossterm::execute!(stdout, EnterAlternateScreen, EnableMouseCapture)
        .context("Failed to enter alternate screen")?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).context("Failed to create terminal")?;

    // Create event channel and app
    let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel::<AppEvent>();
    let mut app = App::new(bare_repo_path, event_tx.clone());

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

    // Load initial worktree data
    if let Err(e) = worktree::refresh_worktrees(&mut app) {
        tracing::error!("Failed to load worktrees: {}", e);
        app.set_status(format!("Failed to load worktrees: {}", e));
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
                        let size = terminal.size()?;
                        keys::handle_key(&mut app, key, (size.width, size.height));
                        needs_redraw = true;
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
                        needs_redraw = true;
                    }
                    AppEvent::Tick => {
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

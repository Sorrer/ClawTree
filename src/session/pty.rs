use anyhow::{Context, Result};
use portable_pty::{CommandBuilder, native_pty_system, PtySize};
use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Instant;
use tokio::sync::mpsc;

use crate::event::AppEvent;

/// Callbacks that capture the window title set by Claude Code.
pub struct TitleCallbacks {
    pub title: String,
}

impl TitleCallbacks {
    pub fn new() -> Self {
        Self {
            title: String::new(),
        }
    }
}

impl vt100::Callbacks for TitleCallbacks {
    fn set_window_title(&mut self, _screen: &mut vt100::Screen, title: &[u8]) {
        if let Ok(s) = std::str::from_utf8(title) {
            self.title = s.to_string();
        }
    }

    fn set_window_icon_name(&mut self, _screen: &mut vt100::Screen, name: &[u8]) {
        // Some apps set the icon name instead of / in addition to the title
        if let Ok(s) = std::str::from_utf8(name) {
            if self.title.is_empty() {
                self.title = s.to_string();
            }
        }
    }
}

/// The parser type with our callbacks.
pub type ClaudeParser = vt100::Parser<TitleCallbacks>;

/// Holds the PTY state for a session.
pub struct PtyHandle {
    pub parser: Arc<RwLock<ClaudeParser>>,
    pub write_tx: mpsc::UnboundedSender<bytes::Bytes>,
    pub master_pty: Box<dyn portable_pty::MasterPty + Send>,
    pub exited: Arc<AtomicBool>,
    pub last_output: Arc<RwLock<Instant>>,
}

/// Spawn a `claude` process in a PTY within the given working directory.
pub fn spawn_claude_pty(
    working_dir: &Path,
    session_id: u64,
    event_tx: mpsc::UnboundedSender<AppEvent>,
    rows: u16,
    cols: u16,
    skip_permissions: bool,
) -> Result<PtyHandle> {
    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .context("Failed to open PTY")?;

    let mut cmd = CommandBuilder::new("claude");
    if skip_permissions {
        cmd.arg("--dangerously-skip-permissions");
    }
    cmd.cwd(working_dir);
    cmd.env("TERM", "xterm-256color");

    let _child = pair
        .slave
        .spawn_command(cmd)
        .context("Failed to spawn claude CLI")?;

    drop(pair.slave);

    let callbacks = TitleCallbacks::new();
    let parser = Arc::new(RwLock::new(
        vt100::Parser::new_with_callbacks(rows, cols, 1000, callbacks),
    ));
    let exited = Arc::new(AtomicBool::new(false));
    let last_output = Arc::new(RwLock::new(Instant::now()));

    // ── Reader thread (plain OS thread) ────────────────────────────
    let reader_parser = Arc::clone(&parser);
    let reader_exited = Arc::clone(&exited);
    let reader_tx = event_tx.clone();
    let reader_last_output = Arc::clone(&last_output);
    let mut reader = pair
        .master
        .try_clone_reader()
        .context("Failed to clone PTY reader")?;

    std::thread::Builder::new()
        .name(format!("pty-reader-{}", session_id))
        .spawn(move || {
            let mut buf = [0u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => {
                        reader_exited.store(true, Ordering::SeqCst);
                        let _ = reader_tx.send(AppEvent::PtyExited { session_id });
                        break;
                    }
                    Ok(n) => {
                        if let Ok(mut p) = reader_parser.write() {
                            p.process(&buf[..n]);
                        }
                        if let Ok(mut t) = reader_last_output.write() {
                            *t = Instant::now();
                        }
                        let _ = reader_tx.send(AppEvent::PtyOutput { session_id });
                    }
                    Err(_) => {
                        reader_exited.store(true, Ordering::SeqCst);
                        let _ = reader_tx.send(AppEvent::PtyExited { session_id });
                        break;
                    }
                }
            }
        })
        .context("Failed to spawn PTY reader thread")?;

    // ── Writer thread (plain OS thread) ────────────────────────────
    let (write_tx, mut write_rx) = mpsc::unbounded_channel::<bytes::Bytes>();
    let mut writer = pair
        .master
        .take_writer()
        .context("Failed to take PTY writer")?;

    std::thread::Builder::new()
        .name(format!("pty-writer-{}", session_id))
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .build()
                .expect("failed to build writer runtime");

            rt.block_on(async {
                while let Some(data) = write_rx.recv().await {
                    if writer.write_all(&data).is_err() {
                        break;
                    }
                    let _ = writer.flush();
                }
            });
        })
        .context("Failed to spawn PTY writer thread")?;

    Ok(PtyHandle {
        parser,
        write_tx,
        master_pty: pair.master,
        exited,
        last_output,
    })
}

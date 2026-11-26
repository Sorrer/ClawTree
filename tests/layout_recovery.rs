//! Layout recovery against a real (private) tmux server: save a layout, kill
//! the server as a crash would, then bring the sessions back.
//!
//! Every tmux call here uses `tmux -L clawtree-test-<pid>-…`, so the user's
//! own tmux server is never touched. Skipped when tmux isn't installed.

mod test_helpers;

use std::collections::HashSet;
use std::path::Path;
use std::process::Command;

use worktree_claude_tui::layout::{
    self, EntryKind, EntryScope, LayoutEntry, LoadOutcome, ResumeSource,
};
use worktree_claude_tui::tmux;

fn tmux_installed() -> bool {
    Command::new("tmux")
        .arg("-V")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn on_socket(socket: &str, args: &[&str]) -> std::process::Output {
    Command::new("tmux")
        .args(["-L", socket])
        .args(args)
        .output()
        .expect("run tmux")
}

fn session_names(socket: &str) -> HashSet<String> {
    let out = on_socket(socket, &["list-sessions", "-F", "#{session_name}"]);
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|s| s.to_string())
        .collect()
}

fn session_env(socket: &str, name: &str, var: &str) -> Option<String> {
    let out = on_socket(socket, &["show-environment", "-t", name, var]);
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .strip_prefix(&format!("{}=", var))
        .map(|s| s.to_string())
}

/// Kills the test server however the test ends.
struct ServerGuard(String);
impl Drop for ServerGuard {
    fn drop(&mut self) {
        let _ = on_socket(&self.0, &["kill-server"]);
    }
}

#[test]
fn terminal_session_survives_a_killed_tmux_server() {
    if !tmux_installed() {
        eprintln!("tmux not installed; skipping");
        return;
    }
    let socket = format!("clawtree-test-{}-lib", std::process::id());
    // The lib's tmux helpers read the socket from the environment. This is
    // the only test in this binary that sets it.
    std::env::set_var(tmux::SOCKET_ENV, &socket);
    let _guard = ServerGuard(socket.clone());

    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("proj");
    let worktree = project.join("main");
    let sub = worktree.join("src");
    std::fs::create_dir_all(&sub).unwrap();
    let state = tmp.path().join("state");
    let manifest = layout::manifest_path_in(&state, &project);

    // 1. A terminal session the way clawtree creates one, shell started in a subfolder.
    let name = format!("clawtree-test-{}-1", std::process::id());
    tmux::create_terminal_session(&worktree, &sub, 24, 80, &name).expect("create session");
    assert!(tmux::session_exists(&name));

    // 2. Record it, then kill the server as a VM crash would.
    let entry = LayoutEntry {
        tmux_name: name.clone(),
        kind: EntryKind::Terminal,
        scope: EntryScope::Worktree,
        path: worktree.clone(),
        cwd: Some(sub.clone()),
        branch: Some("main".into()),
        nickname: Some("build shell".into()),
        agent: tmux::Agent::Claude,
        claude_session_id: None,
        agent_mode: None,
        skip_permissions: false,
        order: 0,
        created_at: layout::now_secs(),
        exited: false,
    };
    let saved = layout::write_merged_to(
        &manifest,
        &project,
        vec![entry],
        &HashSet::new(),
        Some(name.clone()),
    );
    assert!(matches!(saved, layout::SaveOutcome::Saved(_)));
    assert!(!manifest.with_extension("json.tmp").exists());
    on_socket(&socket, &["kill-server"]);
    assert!(!tmux::session_exists(&name));

    // 3. Load and diff: one restorable entry.
    let LoadOutcome::Loaded(m) = layout::load_from(&manifest) else {
        panic!("manifest should load");
    };
    let d = layout::diff(
        &m,
        &session_names(&socket),
        &HashSet::new(),
        project.is_dir(),
        |e| e.path.is_dir(),
    );
    assert_eq!(d.restorable.len(), 1);
    assert!(d.stale.is_empty() && d.live.is_empty());

    // 4. Restore it under its old name and check it's wired up like before.
    let rows = layout::plan_rows(tmp.path(), &d.restorable, &m.entries);
    assert_eq!(rows[0].source, ResumeSource::Terminal);
    let launch = rows[0].claude_launch(None);
    layout::create_tmux_for_entry(&rows[0].entry, &name, &launch, false, 24, 80, None)
        .expect("restore");
    assert!(tmux::session_exists(&name));
    assert_eq!(
        session_env(&socket, &name, tmux::WORKTREE_ENV).map(std::path::PathBuf::from),
        Some(worktree.clone())
    );
    let cwd = on_socket(
        &socket,
        &["display-message", "-t", &name, "-p", "#{pane_current_path}"],
    );
    let cwd = String::from_utf8_lossy(&cwd.stdout).trim().to_string();
    assert_eq!(
        std::fs::canonicalize(&cwd).unwrap(),
        std::fs::canonicalize(&sub).unwrap(),
        "terminal reopens in its last cwd"
    );

    // 5. Now that it's live again, a second diff offers nothing.
    let d = layout::diff(&m, &session_names(&socket), &HashSet::new(), true, |e| {
        e.path.is_dir()
    });
    assert!(d.restorable.is_empty());
    assert_eq!(d.live.len(), 1);

    // 6. A conversation id recorded on a live tmux session counts as open,
    // whatever the session is called (this is how a restore by another
    // clawtree instance is seen before claude itself has started).
    tmux::set_env(&name, tmux::CLAUDE_SESSION_ENV, "conv-under-test");
    let open = layout::open_conversations(tmp.path());
    assert_eq!(
        open.get("conv-under-test").map(String::as_str),
        Some(format!("tmux session {}", name).as_str())
    );
}

fn clawtree(home: &Path, socket: &str, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_clawtree"))
        .args(args)
        .env("HOME", home)
        .env(tmux::SOCKET_ENV, socket)
        .output()
        .expect("run clawtree")
}

#[test]
fn restore_cli_lists_and_recreates_sessions() {
    if !tmux_installed() {
        eprintln!("tmux not installed; skipping");
        return;
    }
    let socket = format!("clawtree-test-{}-cli", std::process::id());
    let _guard = ServerGuard(socket.clone());

    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let project = tmp.path().join("proj");
    std::fs::create_dir_all(&project).unwrap();
    test_helpers::setup_bare_repo_with_worktree(&project);
    let worktree = project.join("main");
    assert!(worktree.is_dir());

    // A manifest as clawtree would have left it: one terminal in the worktree,
    // one whose worktree was deleted.
    let bare = std::fs::canonicalize(&project).unwrap();
    let wt = std::fs::canonicalize(&worktree).unwrap();
    let term = |name: &str, path: &Path| LayoutEntry {
        tmux_name: name.into(),
        kind: EntryKind::Terminal,
        scope: EntryScope::Worktree,
        path: path.to_path_buf(),
        cwd: None,
        branch: None,
        nickname: None,
        agent: tmux::Agent::Claude,
        claude_session_id: None,
        agent_mode: None,
        skip_permissions: false,
        order: 0,
        created_at: 1,
        exited: false,
    };
    let convo = |name: &str, path: &Path, id: &str| LayoutEntry {
        kind: EntryKind::Claude,
        claude_session_id: Some(id.into()),
        ..term(name, path)
    };
    let manifest = layout::manifest_path_in(&home.join(".clawtree").join("state"), &bare);
    let out = layout::write_merged_to(
        &manifest,
        &bare,
        vec![
            term("clawtree-term-main-1", &wt),
            term("clawtree-term-gone-2", &bare.join("gone")),
            // The same conversation recorded twice (restored under a new
            // name once): only one is ever listed. (In a deleted folder so
            // --no-tui below doesn't start a real claude.)
            convo("clawtree-main-1", &bare.join("gone"), "dup-conv"),
            convo("clawtree-main-19", &bare.join("gone"), "dup-conv"),
        ],
        &HashSet::new(),
        None,
    );
    assert!(matches!(out, layout::SaveOutcome::Saved(_)));

    // --version must not start the TUI (it used to be taken as the project folder).
    let v = clawtree(&home, &socket, &["--version"]);
    assert!(v.status.success());
    assert!(String::from_utf8_lossy(&v.stdout).starts_with("clawtree "));
    assert_eq!(
        clawtree(&home, &socket, &["--bogus"]).status.code(),
        Some(2)
    );

    let list = clawtree(
        &home,
        &socket,
        &["restore", "--list", bare.to_str().unwrap()],
    );
    let text = String::from_utf8_lossy(&list.stdout);
    assert!(
        list.status.success(),
        "{}",
        String::from_utf8_lossy(&list.stderr)
    );
    assert!(text.contains("Restorable (1)"), "{}", text);
    assert!(
        text.contains("Already open under another name (1)"),
        "{}",
        text
    );
    assert!(text.contains("Stale — folder gone (2)"), "{}", text);
    assert!(
        session_names(&socket).is_empty(),
        "--list must not create sessions"
    );

    let run = clawtree(
        &home,
        &socket,
        &["restore", "--no-tui", bare.to_str().unwrap()],
    );
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(session_names(&socket).contains("clawtree-term-main-1"));
    assert_eq!(
        session_env(&socket, "clawtree-term-main-1", tmux::WORKTREE_ENV)
            .map(std::path::PathBuf::from),
        Some(wt.clone())
    );

    // Running it again restores nothing twice.
    let again = clawtree(
        &home,
        &socket,
        &["restore", "--list", bare.to_str().unwrap()],
    );
    assert!(String::from_utf8_lossy(&again.stdout).contains("Restorable (0)"));
}

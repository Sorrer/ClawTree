mod test_helpers;

use std::process::Command;
use worktree_claude_tui::git;

// ── Issue 1: init_bare_repo must create a usable worktree ────────────────

#[test]
fn test_init_creates_worktree_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();

    git::init_bare_repo(dir, "main").unwrap();

    // The "main" worktree directory MUST exist after init
    assert!(
        dir.join("main").is_dir(),
        "init_bare_repo did not create the 'main' worktree directory"
    );
}

#[test]
fn test_init_creates_initial_commit() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();

    git::init_bare_repo(dir, "main").unwrap();

    // There must be at least one commit (required for worktrees to work)
    let output = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "No commits exist after init_bare_repo: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn test_init_worktree_is_listed() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();

    git::init_bare_repo(dir, "main").unwrap();

    let entries = git::list_worktrees(dir).unwrap();
    let non_bare: Vec<_> = entries.iter().filter(|e| !e.is_bare).collect();
    assert!(
        !non_bare.is_empty(),
        "list_worktrees returned no non-bare entries after init"
    );
    assert_eq!(
        non_bare[0].branch.as_deref(),
        Some("main"),
        "worktree branch should be 'main'"
    );
}

#[test]
fn test_init_custom_branch_creates_worktree() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();

    git::init_bare_repo(dir, "develop").unwrap();

    assert!(
        dir.join("develop").is_dir(),
        "init_bare_repo did not create the 'develop' worktree directory"
    );
}

// ── Issue 2: converting an empty git repo (no commits) ──────────────────

#[test]
fn test_convert_empty_repo_in_place() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();

    // Create a git repo with NO commits — just `git init`
    let out = Command::new("git")
        .args(["init"])
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(out.status.success(), "git init failed");

    test_helpers::git_config(dir);

    // Set branch name explicitly (some git versions default to "master")
    let _ = Command::new("git")
        .args(["symbolic-ref", "HEAD", "refs/heads/main"])
        .current_dir(dir)
        .output();

    // Convert should succeed even with no commits
    let branch = git::convert_repo_in_place(dir, "").unwrap();
    assert_eq!(branch, "main");

    // .bare directory should exist
    assert!(dir.join(".bare").is_dir(), ".bare directory not found");
    // .git should be a file, not a directory
    assert!(
        dir.join(".git").is_file(),
        ".git should be a file after conversion"
    );
    // Worktree for the branch should exist
    assert!(
        dir.join("main").is_dir(),
        "main worktree not created after converting empty repo"
    );
}

#[test]
fn test_convert_empty_repo_branch_detected() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();

    // Create a git repo with no commits
    let out = Command::new("git")
        .args(["init"])
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(out.status.success());

    test_helpers::git_config(dir);

    let _ = Command::new("git")
        .args(["symbolic-ref", "HEAD", "refs/heads/develop"])
        .current_dir(dir)
        .output();

    // Should detect "develop" as the branch even without commits
    let branch = git::convert_repo_in_place(dir, "").unwrap();
    assert_eq!(branch, "develop");
    assert!(dir.join("develop").is_dir());
}

// ── Issue 1 (part 2): reopening a repo that has .bare but no worktrees ──

#[test]
fn test_detect_bare_repo_with_no_worktrees() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();

    // Simulate a broken init: .bare exists, .git file exists, but no worktree
    let out = Command::new("git")
        .args(["init", "--bare"])
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(out.status.success());

    // Rename . bare structure — actually we need to init properly
    // Just use init_bare_repo but test that detect finds it
    // For this test: create .bare manually, create .git file
    let tmp2 = tempfile::tempdir().unwrap();
    let dir2 = tmp2.path();

    std::fs::create_dir_all(dir2.join(".bare")).unwrap();
    let out = Command::new("git")
        .args(["init", "--bare", ".bare"])
        .current_dir(dir2)
        .output()
        .unwrap();
    assert!(out.status.success());
    std::fs::write(dir2.join(".git"), "gitdir: ./.bare\n").unwrap();

    // detect_bare_repo should still find it even without worktrees
    let detected = git::detect_bare_repo(dir2);
    assert!(
        detected.is_some(),
        "detect_bare_repo should find .bare even without worktrees"
    );

    // list_worktrees should return only the bare entry (no non-bare)
    let entries = git::list_worktrees(dir2).unwrap();
    let non_bare: Vec<_> = entries.iter().filter(|e| !e.is_bare).collect();
    assert!(
        non_bare.is_empty(),
        "Expected no non-bare worktrees but found {:?}",
        non_bare
    );
}

// ── Regression: init then list_worktrees roundtrip ──────────────────────

#[test]
fn test_init_then_list_worktrees_roundtrip() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();

    git::init_bare_repo(dir, "main").unwrap();

    // Must be able to list worktrees and find the main branch
    let entries = git::list_worktrees(dir).unwrap();
    let branches: Vec<_> = entries.iter().filter_map(|e| e.branch.as_deref()).collect();
    assert!(
        branches.contains(&"main"),
        "Expected 'main' in worktree branches, got {:?}",
        branches
    );
}

#[test]
fn test_init_then_can_create_additional_worktree() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();

    git::init_bare_repo(dir, "main").unwrap();

    // Should be able to create another worktree branching from main
    git::create_worktree(dir, "feature-a", "feature-a", "main").unwrap();
    assert!(
        dir.join("feature-a").is_dir(),
        "feature-a worktree not created"
    );
}

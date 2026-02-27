mod test_helpers;

use worktree_claude_tui::git;

#[test]
fn test_detect_bare_repo_from_root() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    test_helpers::setup_bare_repo(dir);

    let detected = git::detect_bare_repo(dir);
    assert!(detected.is_some(), "bare repo not detected from root");
    // Should resolve to the directory containing .bare
    let detected_path = detected.unwrap();
    assert!(detected_path.join(".bare").is_dir());
}

#[test]
fn test_detect_bare_repo_from_worktree() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    test_helpers::setup_bare_repo_with_worktree(dir);

    let main_wt = dir.join("main");
    let detected = git::detect_bare_repo(&main_wt);
    assert!(detected.is_some(), "bare repo not detected from worktree");
}

#[test]
fn test_detect_bare_repo_none() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();

    // Empty directory — no git repo
    let detected = git::detect_bare_repo(dir);
    assert!(detected.is_none(), "unexpectedly detected a bare repo in empty dir");
}

#[test]
fn test_detect_regular_repo_found() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    test_helpers::setup_regular_repo(dir);

    let detected = git::detect_regular_repo(dir);
    assert!(detected.is_some(), "regular repo not detected");
    assert_eq!(detected.unwrap(), dir);
}

#[test]
fn test_detect_regular_repo_ignores_git_file() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    test_helpers::setup_bare_repo_with_worktree(dir);

    // A bare repo worktree has a .git *file*, not a .git directory
    let main_wt = dir.join("main");
    let detected = git::detect_regular_repo(&main_wt);
    assert!(detected.is_none(), "detect_regular_repo should not match .git files (bare worktrees)");
}

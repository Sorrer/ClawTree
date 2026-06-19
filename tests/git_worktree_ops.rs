mod test_helpers;

use worktree_claude_tui::git;

#[test]
fn test_create_worktree_new_branch() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    test_helpers::setup_bare_repo_with_worktree(dir);

    git::create_worktree(dir, "feature-a", "feature-a", "main").unwrap();

    assert!(dir.join("feature-a").is_dir());
    let entries = git::list_worktrees(dir).unwrap();
    let feature = entries
        .iter()
        .find(|e| e.branch.as_deref() == Some("feature-a"));
    assert!(feature.is_some(), "feature-a worktree not found in list");
}

#[test]
fn test_create_worktree_existing_branch() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    test_helpers::setup_bare_repo_with_worktree(dir);

    // Create a branch first
    let main_wt = dir.join("main");
    let _ = std::process::Command::new("git")
        .args(["branch", "existing-branch"])
        .current_dir(&main_wt)
        .output();

    // Create worktree for existing branch (will fail with -b, fallback without -b)
    git::create_worktree(dir, "existing-branch", "existing-branch", "").unwrap();

    assert!(dir.join("existing-branch").is_dir());
}

#[test]
fn test_remove_worktree() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    test_helpers::setup_bare_repo_with_worktree(dir);

    git::create_worktree(dir, "to-remove", "to-remove", "main").unwrap();
    assert!(dir.join("to-remove").is_dir());

    git::remove_worktree(dir, &dir.join("to-remove")).unwrap();
    assert!(!dir.join("to-remove").is_dir());
}

#[test]
fn test_force_remove_dirty_worktree() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    test_helpers::setup_bare_repo_with_worktree(dir);

    git::create_worktree(dir, "dirty-wt", "dirty-wt", "main").unwrap();

    // Make it dirty
    test_helpers::create_file(&dir.join("dirty-wt"), "untracked.txt", "data");

    // Regular remove may fail on dirty worktree, force remove should work
    git::force_remove_worktree(dir, &dir.join("dirty-wt")).unwrap();
    assert!(!dir.join("dirty-wt").is_dir());
}

#[test]
fn test_current_branch_name() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    test_helpers::setup_bare_repo_with_worktree(dir);

    let main_wt = dir.join("main");
    let branch = git::current_branch_name(&main_wt).unwrap();
    assert_eq!(branch, "main");
}

#[test]
fn test_is_worktree_clean_true() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    test_helpers::setup_bare_repo_with_worktree(dir);

    let main_wt = dir.join("main");
    assert!(git::is_worktree_clean(&main_wt).unwrap());
}

#[test]
fn test_is_worktree_clean_false() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    test_helpers::setup_bare_repo_with_worktree(dir);

    let main_wt = dir.join("main");
    test_helpers::create_file(&main_wt, "dirty.txt", "uncommitted");

    assert!(!git::is_worktree_clean(&main_wt).unwrap());
}

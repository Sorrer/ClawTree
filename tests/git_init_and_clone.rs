mod test_helpers;

use worktree_claude_tui::git;

#[test]
fn test_init_bare_repo() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();

    git::init_bare_repo(dir, "main").unwrap();

    // .bare directory should exist
    assert!(dir.join(".bare").is_dir(), ".bare directory not found");
    // .git file should point to .bare
    let git_content = std::fs::read_to_string(dir.join(".git")).unwrap();
    assert!(git_content.contains(".bare"), ".git file doesn't point to .bare");
    // HEAD should point to main
    let head = std::fs::read_to_string(dir.join(".bare/HEAD")).unwrap();
    assert!(head.contains("refs/heads/main"), "HEAD doesn't point to main: {}", head);
}

#[test]
fn test_init_bare_repo_custom_branch() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();

    git::init_bare_repo(dir, "develop").unwrap();

    assert!(dir.join(".bare").is_dir());
    // HEAD should reference develop
    let head = std::fs::read_to_string(dir.join(".bare/HEAD")).unwrap();
    assert!(head.contains("refs/heads/develop"), "HEAD doesn't point to develop: {}", head);
}

#[test]
fn test_clone_bare_repo() {
    // First create a source repo to clone from
    let source_tmp = tempfile::tempdir().unwrap();
    let source = source_tmp.path();
    test_helpers::setup_regular_repo(source);
    test_helpers::create_file(source, "hello.txt", "hello");
    test_helpers::git_commit(source, "Add hello.txt");

    // Clone it
    let target_tmp = tempfile::tempdir().unwrap();
    let target = target_tmp.path();

    git::clone_bare_repo(target, &source.to_string_lossy(), "main").unwrap();

    assert!(target.join(".bare").is_dir());
    let git_content = std::fs::read_to_string(target.join(".git")).unwrap();
    assert!(git_content.contains(".bare"));
    // Cloning from a repo with commits should create the worktree
    assert!(target.join("main").is_dir(), "main worktree not created after clone");
}

#[test]
fn test_list_worktrees_after_clone() {
    let source_tmp = tempfile::tempdir().unwrap();
    let source = source_tmp.path();
    test_helpers::setup_regular_repo(source);
    test_helpers::create_file(source, "file.txt", "data");
    test_helpers::git_commit(source, "Add file");

    let target_tmp = tempfile::tempdir().unwrap();
    let target = target_tmp.path();

    git::clone_bare_repo(target, &source.to_string_lossy(), "main").unwrap();

    let entries = git::list_worktrees(target).unwrap();
    assert!(entries.len() >= 2, "expected at least 2 entries, got {}", entries.len());

    let bare_entry = entries.iter().find(|e| e.is_bare);
    assert!(bare_entry.is_some(), "no bare entry found");

    let main_entry = entries.iter().find(|e| e.branch.as_deref() == Some("main"));
    assert!(main_entry.is_some(), "no main worktree entry found");
}

#[test]
fn test_list_branches_after_clone() {
    let source_tmp = tempfile::tempdir().unwrap();
    let source = source_tmp.path();
    test_helpers::setup_regular_repo(source);
    test_helpers::create_file(source, "file.txt", "data");
    test_helpers::git_commit(source, "Add file");

    let target_tmp = tempfile::tempdir().unwrap();
    let target = target_tmp.path();

    git::clone_bare_repo(target, &source.to_string_lossy(), "main").unwrap();

    let branches = git::list_branches(target).unwrap();
    assert!(branches.contains(&"main".to_string()), "branches: {:?}", branches);
}

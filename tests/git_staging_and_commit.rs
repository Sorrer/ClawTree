mod test_helpers;

use worktree_claude_tui::git;

#[test]
fn test_stage_file() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    test_helpers::setup_bare_repo_with_worktree(dir);

    let wt = dir.join("main");
    test_helpers::create_file(&wt, "new.txt", "hello");

    git::stage_file(&wt, "new.txt").unwrap();

    let changes = git::status_porcelain(&wt).unwrap();
    let staged = changes.iter().find(|c| c.path == "new.txt");
    assert!(staged.is_some());
    assert_eq!(staged.unwrap().index_status, 'A');
}

#[test]
fn test_unstage_file() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    test_helpers::setup_bare_repo_with_worktree(dir);

    let wt = dir.join("main");
    test_helpers::create_file(&wt, "staged.txt", "data");
    git::stage_file(&wt, "staged.txt").unwrap();

    git::unstage_file(&wt, "staged.txt").unwrap();

    let changes = git::status_porcelain(&wt).unwrap();
    let file = changes.iter().find(|c| c.path == "staged.txt");
    assert!(file.is_some());
    // After unstaging a new file, it should be untracked (??)
    assert_eq!(file.unwrap().index_status, '?');
}

#[test]
fn test_stage_all() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    test_helpers::setup_bare_repo_with_worktree(dir);

    let wt = dir.join("main");
    test_helpers::create_file(&wt, "a.txt", "aaa");
    test_helpers::create_file(&wt, "b.txt", "bbb");

    git::stage_all(&wt).unwrap();

    let changes = git::status_porcelain(&wt).unwrap();
    assert!(
        changes.iter().all(|c| c.index_status == 'A'),
        "not all files staged: {:?}",
        changes
            .iter()
            .map(|c| (c.index_status, &c.path))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_commit() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    test_helpers::setup_bare_repo_with_worktree(dir);

    let wt = dir.join("main");
    test_helpers::create_file(&wt, "committed.txt", "data");
    git::stage_all(&wt).unwrap();
    git::commit(&wt, "Add committed.txt").unwrap();

    // Should be clean after commit
    assert!(git::is_worktree_clean(&wt).unwrap());

    // Check subject
    let subject = git::head_subject(&wt).unwrap();
    assert_eq!(subject, "Add committed.txt");
}

#[test]
fn test_status_porcelain_empty() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    test_helpers::setup_bare_repo_with_worktree(dir);

    let wt = dir.join("main");
    let changes = git::status_porcelain(&wt).unwrap();
    assert!(changes.is_empty());
}

#[test]
fn test_status_porcelain_various() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    test_helpers::setup_bare_repo_with_worktree(dir);

    let wt = dir.join("main");

    // Create and commit a file first
    test_helpers::create_file(&wt, "existing.txt", "original");
    git::stage_all(&wt).unwrap();
    git::commit(&wt, "add existing").unwrap();

    // Now modify it (unstaged) and add an untracked file
    test_helpers::create_file(&wt, "existing.txt", "modified");
    test_helpers::create_file(&wt, "untracked.txt", "new");

    let changes = git::status_porcelain(&wt).unwrap();
    assert_eq!(changes.len(), 2);

    let modified = changes.iter().find(|c| c.path == "existing.txt").unwrap();
    assert_eq!(modified.work_status, 'M');

    let untracked = changes.iter().find(|c| c.path == "untracked.txt").unwrap();
    assert_eq!(untracked.index_status, '?');
}

#[test]
fn test_log_oneline() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    test_helpers::setup_bare_repo_with_worktree(dir);

    let wt = dir.join("main");
    test_helpers::create_file(&wt, "f1.txt", "a");
    git::stage_all(&wt).unwrap();
    git::commit(&wt, "First change").unwrap();

    test_helpers::create_file(&wt, "f2.txt", "b");
    git::stage_all(&wt).unwrap();
    git::commit(&wt, "Second change").unwrap();

    let log = git::log_oneline(&wt, 5).unwrap();
    assert!(
        log.len() >= 2,
        "expected at least 2 log entries, got {}",
        log.len()
    );
    assert!(log[0].contains("Second change"));
    assert!(log[1].contains("First change"));
}

#[test]
fn test_head_subject() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    test_helpers::setup_bare_repo_with_worktree(dir);

    let wt = dir.join("main");
    test_helpers::create_file(&wt, "f.txt", "data");
    git::stage_all(&wt).unwrap();
    git::commit(&wt, "My commit message").unwrap();

    let subject = git::head_subject(&wt).unwrap();
    assert_eq!(subject, "My commit message");
}

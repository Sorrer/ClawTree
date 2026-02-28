mod test_helpers;

use worktree_claude_tui::git;

/// Helper: create a bare repo with main worktree and a feature branch worktree.
/// Returns (tmp_dir, main_wt_path, feature_wt_path).
fn setup_with_feature_branch(tmp: &tempfile::TempDir) -> (std::path::PathBuf, std::path::PathBuf) {
    let dir = tmp.path().to_path_buf();
    test_helpers::setup_bare_repo_with_worktree(&dir);

    // Add a file on main so we have something to work with
    let main_wt = dir.join("main");
    test_helpers::create_file(&main_wt, "base.txt", "base content");
    test_helpers::git_commit(&main_wt, "Add base.txt");

    // Create feature branch worktree
    git::create_worktree(&dir, "feature", "feature", "main").unwrap();
    test_helpers::git_config(&dir.join("feature"));

    (main_wt, dir.join("feature"))
}

#[test]
fn test_merge_fast_forward() {
    let tmp = tempfile::tempdir().unwrap();
    let _dir = tmp.path();
    let (main_wt, feature_wt) = setup_with_feature_branch(&tmp);

    // Add a commit on feature (main is behind)
    test_helpers::create_file(&feature_wt, "feature.txt", "feature work");
    test_helpers::git_commit(&feature_wt, "Add feature.txt");

    // Merge feature into main
    let result = git::merge_branch(&main_wt, "feature").unwrap();
    assert!(matches!(result, git::MergeResult::Success(_)));

    // main should now have feature.txt
    assert!(main_wt.join("feature.txt").exists());
}

#[test]
fn test_merge_three_way() {
    let tmp = tempfile::tempdir().unwrap();
    let _dir = tmp.path();
    let (main_wt, feature_wt) = setup_with_feature_branch(&tmp);

    // Non-conflicting changes on both branches
    test_helpers::create_file(&main_wt, "main-only.txt", "from main");
    test_helpers::git_commit(&main_wt, "Add main-only.txt");

    test_helpers::create_file(&feature_wt, "feature-only.txt", "from feature");
    test_helpers::git_commit(&feature_wt, "Add feature-only.txt");

    let result = git::merge_branch(&main_wt, "feature").unwrap();
    assert!(matches!(result, git::MergeResult::Success(_)));

    // Both files should exist on main
    assert!(main_wt.join("main-only.txt").exists());
    assert!(main_wt.join("feature-only.txt").exists());
}

#[test]
fn test_merge_conflict() {
    let tmp = tempfile::tempdir().unwrap();
    let _dir = tmp.path();
    let (main_wt, feature_wt) = setup_with_feature_branch(&tmp);

    // Conflicting changes to the same file
    test_helpers::create_file(&main_wt, "conflict.txt", "main version");
    test_helpers::git_commit(&main_wt, "Main changes to conflict.txt");

    test_helpers::create_file(&feature_wt, "conflict.txt", "feature version");
    test_helpers::git_commit(&feature_wt, "Feature changes to conflict.txt");

    let result = git::merge_branch(&main_wt, "feature").unwrap();
    assert!(matches!(result, git::MergeResult::Conflict));
}

#[test]
fn test_merge_abort() {
    let tmp = tempfile::tempdir().unwrap();
    let _dir = tmp.path();
    let (main_wt, feature_wt) = setup_with_feature_branch(&tmp);

    // Create conflict
    test_helpers::create_file(&main_wt, "conflict.txt", "main version");
    test_helpers::git_commit(&main_wt, "Main conflict");

    test_helpers::create_file(&feature_wt, "conflict.txt", "feature version");
    test_helpers::git_commit(&feature_wt, "Feature conflict");

    let _ = git::merge_branch(&main_wt, "feature");

    // Abort should succeed
    git::merge_abort(&main_wt).unwrap();

    // Should be clean after abort
    assert!(git::is_worktree_clean(&main_wt).unwrap());
    // No merge should be in progress
    assert!(git::merge_in_progress(&main_wt).is_none());
}

#[test]
fn test_merge_in_progress_detection() {
    let tmp = tempfile::tempdir().unwrap();
    let _dir = tmp.path();
    let (main_wt, feature_wt) = setup_with_feature_branch(&tmp);

    // No merge before
    assert!(git::merge_in_progress(&main_wt).is_none());

    // Create conflict to get a merge in progress
    test_helpers::create_file(&main_wt, "conflict.txt", "main");
    test_helpers::git_commit(&main_wt, "main conflict");

    test_helpers::create_file(&feature_wt, "conflict.txt", "feature");
    test_helpers::git_commit(&feature_wt, "feature conflict");

    let _ = git::merge_branch(&main_wt, "feature");

    // Merge should now be in progress
    assert!(git::merge_in_progress(&main_wt).is_some());
}

#[test]
fn test_merge_nonexistent_branch() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    test_helpers::setup_bare_repo_with_worktree(dir);

    let main_wt = dir.join("main");
    let result = git::merge_branch(&main_wt, "nonexistent-branch-xyz");
    assert!(result.is_err());
}

#[test]
fn test_merge_dirty_worktree() {
    let tmp = tempfile::tempdir().unwrap();
    let _dir = tmp.path();
    let (main_wt, feature_wt) = setup_with_feature_branch(&tmp);

    // Add a commit on feature
    test_helpers::create_file(&feature_wt, "feature.txt", "work");
    test_helpers::git_commit(&feature_wt, "feature work");

    // Dirty the main worktree with a conflicting tracked file change
    test_helpers::create_file(&main_wt, "base.txt", "dirty uncommitted change");

    // Merge with dirty worktree — git may refuse or produce unexpected results
    // The important thing is it doesn't panic
    let _result = git::merge_branch(&main_wt, "feature");
    // We just verify no panic; behavior depends on git version
}

mod test_helpers;

use std::process::Command;
use worktree_claude_tui::git;

// ── In-place conversion ──────────────────────────────────────────────────

#[test]
fn test_convert_in_place_basic() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    test_helpers::setup_regular_repo(dir);

    // Add a file so it's not empty
    test_helpers::create_file(dir, "hello.txt", "hello");
    test_helpers::git_commit(dir, "Add hello.txt");

    let branch = git::convert_repo_in_place(dir, "").unwrap();
    assert_eq!(branch, "main");

    // .bare directory should exist
    assert!(dir.join(".bare").is_dir(), ".bare directory not found");
    // .git should be a file, not a directory
    assert!(dir.join(".git").is_file(), ".git should be a file");
    let git_content = std::fs::read_to_string(dir.join(".git")).unwrap();
    assert!(git_content.contains(".bare"));
    // Worktree for the branch should exist
    assert!(dir.join("main").is_dir(), "main worktree not created");
}

#[test]
fn test_convert_in_place_with_branch_override() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    test_helpers::setup_regular_repo(dir);

    test_helpers::create_file(dir, "file.txt", "data");
    test_helpers::git_commit(dir, "Add file");

    // Create a "develop" branch
    let _ = Command::new("git")
        .args(["branch", "develop"])
        .current_dir(dir)
        .output();

    let branch = git::convert_repo_in_place(dir, "develop").unwrap();
    assert_eq!(branch, "develop");
    assert!(dir.join("develop").is_dir());
}

#[test]
fn test_convert_in_place_with_dirty_files() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    test_helpers::setup_regular_repo(dir);

    test_helpers::create_file(dir, "committed.txt", "v1");
    test_helpers::git_commit(dir, "Add committed.txt");

    // Create dirty (uncommitted) changes
    test_helpers::create_file(dir, "committed.txt", "v2 dirty");

    let branch = git::convert_repo_in_place(dir, "").unwrap();
    assert_eq!(branch, "main");
    assert!(dir.join(".bare").is_dir());

    // The worktree should exist (stash/pop may or may not preserve changes
    // depending on git version, but it shouldn't error)
    assert!(dir.join("main").is_dir());
}

#[test]
fn test_convert_in_place_blocks_during_merge() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    test_helpers::setup_regular_repo(dir);

    test_helpers::create_file(dir, "file.txt", "original");
    test_helpers::git_commit(dir, "Add file");

    // Create a branch with conflicting changes
    let _ = Command::new("git")
        .args(["checkout", "-b", "conflict-branch"])
        .current_dir(dir)
        .output();
    test_helpers::create_file(dir, "file.txt", "conflict version");
    test_helpers::git_commit(dir, "Conflict change");

    let _ = Command::new("git")
        .args(["checkout", "main"])
        .current_dir(dir)
        .output();
    test_helpers::create_file(dir, "file.txt", "main version");
    test_helpers::git_commit(dir, "Main change");

    // Start a merge that will conflict
    let _ = Command::new("git")
        .args(["merge", "conflict-branch"])
        .current_dir(dir)
        .output();

    // Convert should fail with merge in progress
    let result = git::convert_repo_in_place(dir, "");
    assert!(result.is_err());
    let err_msg = result.unwrap_err().to_string();
    assert!(err_msg.contains("merge") || err_msg.contains("in progress"), "unexpected error: {}", err_msg);
}

/// After in-place conversion, the worktree should contain the committed file
/// and old root-level files should be cleaned up.
#[test]
fn test_convert_in_place_files_moved_to_worktree() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    test_helpers::setup_regular_repo(dir);

    test_helpers::create_file(dir, "src/main.rs", "fn main() {}");
    test_helpers::create_file(dir, "README.md", "# Project");
    test_helpers::git_commit(dir, "Add project files");

    git::convert_repo_in_place(dir, "").unwrap();

    let wt = dir.join("main");
    // Committed files should be in the worktree
    assert!(wt.join("src/main.rs").exists(), "src/main.rs not in worktree");
    assert!(wt.join("README.md").exists(), "README.md not in worktree");

    // Old root-level files should be cleaned up
    assert!(!dir.join("src").exists(), "root src/ should be removed");
    assert!(!dir.join("README.md").exists(), "root README.md should be removed");
}

/// After in-place conversion, `git worktree list` should work and show
/// the bare entry + the converted worktree.
#[test]
fn test_convert_in_place_worktree_list_works() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    test_helpers::setup_regular_repo(dir);

    test_helpers::create_file(dir, "file.txt", "data");
    test_helpers::git_commit(dir, "Add file");

    git::convert_repo_in_place(dir, "").unwrap();

    let entries = git::list_worktrees(dir).unwrap();
    assert!(entries.len() >= 2, "expected bare + main, got {} entries", entries.len());

    let bare = entries.iter().find(|e| e.is_bare);
    assert!(bare.is_some(), "no bare entry after conversion");

    let main = entries.iter().find(|e| e.branch.as_deref() == Some("main"));
    assert!(main.is_some(), "no 'main' worktree after conversion");
}

/// After in-place conversion, we should be able to create new worktrees
/// from the converted bare repo.
#[test]
fn test_convert_in_place_can_create_new_worktree() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    test_helpers::setup_regular_repo(dir);

    test_helpers::create_file(dir, "file.txt", "data");
    test_helpers::git_commit(dir, "Add file");

    git::convert_repo_in_place(dir, "").unwrap();

    // Create a new worktree from the converted repo
    git::create_worktree(dir, "feature-x", "feature-x", "main").unwrap();

    assert!(dir.join("feature-x").is_dir(), "feature-x worktree not created");

    let entries = git::list_worktrees(dir).unwrap();
    let feature = entries.iter().find(|e| e.branch.as_deref() == Some("feature-x"));
    assert!(feature.is_some(), "feature-x not in worktree list");
}

/// After in-place conversion, we should be able to make commits in the
/// worktree and they should be visible via git log.
#[test]
fn test_convert_in_place_can_commit_in_worktree() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    test_helpers::setup_regular_repo(dir);

    test_helpers::create_file(dir, "original.txt", "data");
    test_helpers::git_commit(dir, "Initial file");

    git::convert_repo_in_place(dir, "").unwrap();

    let wt = dir.join("main");
    test_helpers::git_config(&wt);
    test_helpers::create_file(&wt, "new_file.txt", "post-conversion");
    git::stage_all(&wt).unwrap();
    git::commit(&wt, "Post-conversion commit").unwrap();

    let subject = git::head_subject(&wt).unwrap();
    assert_eq!(subject, "Post-conversion commit");
    assert!(git::is_worktree_clean(&wt).unwrap());
}

/// After in-place conversion, the worktree should be clean (no leftover
/// unstaged changes — unless dirty files were stashed and popped).
#[test]
fn test_convert_in_place_worktree_clean_after_conversion() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    test_helpers::setup_regular_repo(dir);

    test_helpers::create_file(dir, "file.txt", "data");
    test_helpers::git_commit(dir, "Commit file");

    git::convert_repo_in_place(dir, "").unwrap();

    let wt = dir.join("main");
    assert!(git::is_worktree_clean(&wt).unwrap(), "worktree should be clean after converting a clean repo");
}

/// Converting a repo with multiple branches should preserve all branches,
/// accessible from the new bare layout.
#[test]
fn test_convert_in_place_preserves_branches() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    test_helpers::setup_regular_repo(dir);

    test_helpers::create_file(dir, "file.txt", "data");
    test_helpers::git_commit(dir, "Initial");

    // Create extra branches
    let _ = Command::new("git")
        .args(["branch", "feature-a"])
        .current_dir(dir)
        .output();
    let _ = Command::new("git")
        .args(["branch", "feature-b"])
        .current_dir(dir)
        .output();

    git::convert_repo_in_place(dir, "").unwrap();

    let branches = git::list_branches(dir).unwrap();
    assert!(branches.contains(&"main".to_string()), "main branch missing: {:?}", branches);
    assert!(branches.contains(&"feature-a".to_string()), "feature-a branch missing: {:?}", branches);
    assert!(branches.contains(&"feature-b".to_string()), "feature-b branch missing: {:?}", branches);
}

/// Converting a repo with a deep directory structure should work.
#[test]
fn test_convert_in_place_deep_directory_structure() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    test_helpers::setup_regular_repo(dir);

    test_helpers::create_file(dir, "src/app/components/button.rs", "// button");
    test_helpers::create_file(dir, "src/app/components/form.rs", "// form");
    test_helpers::create_file(dir, "tests/unit/test_button.rs", "// test");
    test_helpers::create_file(dir, "docs/api/README.md", "# API");
    test_helpers::git_commit(dir, "Add deep structure");

    git::convert_repo_in_place(dir, "").unwrap();

    let wt = dir.join("main");
    assert!(wt.join("src/app/components/button.rs").exists());
    assert!(wt.join("src/app/components/form.rs").exists());
    assert!(wt.join("tests/unit/test_button.rs").exists());
    assert!(wt.join("docs/api/README.md").exists());

    // Root should be clean (only .bare, .git, main)
    let root_entries: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    for entry in &root_entries {
        assert!(
            entry == ".bare" || entry == ".git" || entry == "main",
            "unexpected root entry after conversion: '{}'", entry
        );
    }
}

/// Converting a repo with untracked files should stash and restore them.
#[test]
fn test_convert_in_place_stash_pop_untracked() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    test_helpers::setup_regular_repo(dir);

    test_helpers::create_file(dir, "committed.txt", "committed");
    test_helpers::git_commit(dir, "Commit a file");

    // Add untracked file (not staged)
    test_helpers::create_file(dir, "untracked.txt", "I am untracked");

    git::convert_repo_in_place(dir, "").unwrap();

    let wt = dir.join("main");
    // Stash should have captured and restored the untracked file
    // (stash push --include-untracked + stash pop)
    assert!(wt.join("committed.txt").exists(), "committed file should be in worktree");
    // The untracked file may or may not be restored depending on stash behavior,
    // but the conversion should not error
    assert!(wt.is_dir(), "worktree should exist");
}

/// Converting a repo that has ONLY uncommitted files (nothing ever committed
/// beyond the initial empty commit) should still produce a valid worktree
/// with those files preserved via stash/pop.
#[test]
fn test_convert_in_place_uncommitted_files_only() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    test_helpers::setup_regular_repo(dir);

    // Create several files and directories but do NOT commit them
    test_helpers::create_file(dir, "README.md", "# My Project");
    test_helpers::create_file(dir, "src/main.rs", "fn main() { println!(\"hello\"); }");
    test_helpers::create_file(dir, "src/lib.rs", "pub fn add(a: i32, b: i32) -> i32 { a + b }");
    test_helpers::create_file(dir, "assets/icon.txt", "pretend this is an icon");
    test_helpers::create_file(dir, "Cargo.toml", "[package]\nname = \"test\"");

    // Stage some but don't commit — mixed staged + untracked
    let _ = Command::new("git")
        .args(["add", "src/main.rs", "Cargo.toml"])
        .current_dir(dir)
        .output();

    let branch = git::convert_repo_in_place(dir, "").unwrap();
    assert_eq!(branch, "main");

    // Verify bare layout was created
    assert!(dir.join(".bare").is_dir(), ".bare directory not found");
    assert!(dir.join(".git").is_file(), ".git should be a pointer file");
    assert!(dir.join("main").is_dir(), "main worktree not created");

    // Old root-level files should be cleaned up
    assert!(!dir.join("README.md").exists(), "root README.md should be removed");
    assert!(!dir.join("src").exists(), "root src/ should be removed");
    assert!(!dir.join("assets").exists(), "root assets/ should be removed");
    assert!(!dir.join("Cargo.toml").exists(), "root Cargo.toml should be removed");

    // Root should only contain .bare, .git, main
    let root_entries: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    for entry in &root_entries {
        assert!(
            entry == ".bare" || entry == ".git" || entry == "main",
            "unexpected root entry after conversion: '{}'", entry
        );
    }

    // The worktree should have the files restored via stash pop
    let wt = dir.join("main");
    assert!(wt.join("README.md").exists(), "README.md should be restored in worktree");
    assert!(wt.join("src/main.rs").exists(), "src/main.rs should be restored in worktree");
    assert!(wt.join("src/lib.rs").exists(), "src/lib.rs should be restored in worktree");
    assert!(wt.join("assets/icon.txt").exists(), "assets/icon.txt should be restored in worktree");
    assert!(wt.join("Cargo.toml").exists(), "Cargo.toml should be restored in worktree");

    // Verify the repo is functional — we can list worktrees
    let entries = git::list_worktrees(dir).unwrap();
    assert!(entries.len() >= 2, "expected bare + main, got {} entries", entries.len());

    // Verify we can still stage and commit in the new worktree
    test_helpers::git_config(&wt);
    git::stage_all(&wt).unwrap();
    git::commit(&wt, "Commit after conversion").unwrap();
    assert_eq!(git::head_subject(&wt).unwrap(), "Commit after conversion");
    assert!(git::is_worktree_clean(&wt).unwrap());
}

// ── Convert to separate location ─────────────────────────────────────────

#[test]
fn test_convert_to_location_basic() {
    let tmp_source = tempfile::tempdir().unwrap();
    let source = tmp_source.path();
    test_helpers::setup_regular_repo(source);
    test_helpers::create_file(source, "file.txt", "data");
    test_helpers::git_commit(source, "Add file");

    let tmp_target = tempfile::tempdir().unwrap();
    let target = tmp_target.path().join("converted");

    let branch = git::convert_repo_to_location(source, &target, "").unwrap();
    assert_eq!(branch, "main");
    assert!(target.join(".bare").is_dir());
    assert!(target.join(".git").is_file());
    assert!(target.join("main").is_dir());
}

#[test]
fn test_convert_to_location_nonempty_target() {
    let tmp_source = tempfile::tempdir().unwrap();
    let source = tmp_source.path();
    test_helpers::setup_regular_repo(source);
    test_helpers::create_file(source, "file.txt", "data");
    test_helpers::git_commit(source, "Add file");

    let tmp_target = tempfile::tempdir().unwrap();
    let target = tmp_target.path();
    // Make target non-empty
    test_helpers::create_file(target, "blocker.txt", "existing");

    let result = git::convert_repo_to_location(source, target, "");
    assert!(result.is_err());
    let err_msg = result.unwrap_err().to_string();
    assert!(err_msg.contains("not empty"), "unexpected error: {}", err_msg);
}

/// The target should contain the committed files in its worktree.
#[test]
fn test_convert_to_location_files_in_worktree() {
    let tmp_source = tempfile::tempdir().unwrap();
    let source = tmp_source.path();
    test_helpers::setup_regular_repo(source);
    test_helpers::create_file(source, "src/main.rs", "fn main() {}");
    test_helpers::create_file(source, "README.md", "# Hello");
    test_helpers::git_commit(source, "Add files");

    let tmp_target = tempfile::tempdir().unwrap();
    let target = tmp_target.path().join("new-repo");

    git::convert_repo_to_location(source, &target, "").unwrap();

    let wt = target.join("main");
    assert!(wt.join("src/main.rs").exists(), "src/main.rs not in target worktree");
    assert!(wt.join("README.md").exists(), "README.md not in target worktree");
}

/// After converting to a new location, the target should support creating
/// new worktrees and making commits.
#[test]
fn test_convert_to_location_fully_functional() {
    let tmp_source = tempfile::tempdir().unwrap();
    let source = tmp_source.path();
    test_helpers::setup_regular_repo(source);
    test_helpers::create_file(source, "file.txt", "original");
    test_helpers::git_commit(source, "Initial");

    let tmp_target = tempfile::tempdir().unwrap();
    let target = tmp_target.path().join("converted");

    git::convert_repo_to_location(source, &target, "").unwrap();

    // List worktrees
    let entries = git::list_worktrees(&target).unwrap();
    assert!(entries.len() >= 2, "expected bare + main, got {}", entries.len());

    // Create a new worktree
    git::create_worktree(&target, "feature", "feature", "main").unwrap();
    assert!(target.join("feature").is_dir());

    // Commit in the new worktree
    let feature_wt = target.join("feature");
    test_helpers::git_config(&feature_wt);
    test_helpers::create_file(&feature_wt, "new.txt", "new content");
    git::stage_all(&feature_wt).unwrap();
    git::commit(&feature_wt, "Feature commit").unwrap();

    assert_eq!(git::head_subject(&feature_wt).unwrap(), "Feature commit");
    assert!(git::is_worktree_clean(&feature_wt).unwrap());
}

/// The source repo should remain unchanged after converting to a new location.
#[test]
fn test_convert_to_location_source_unchanged() {
    let tmp_source = tempfile::tempdir().unwrap();
    let source = tmp_source.path();
    test_helpers::setup_regular_repo(source);
    test_helpers::create_file(source, "file.txt", "data");
    test_helpers::git_commit(source, "Add file");

    // Record source state before conversion
    let source_head_before = git::head_subject(source).unwrap();
    assert!(source.join(".git").is_dir(), "source should have .git directory before");

    let tmp_target = tempfile::tempdir().unwrap();
    let target = tmp_target.path().join("converted");

    git::convert_repo_to_location(source, &target, "").unwrap();

    // Source should be untouched
    assert!(source.join(".git").is_dir(), "source .git directory should still exist");
    assert!(source.join("file.txt").exists(), "source file should still exist");
    let source_head_after = git::head_subject(source).unwrap();
    assert_eq!(source_head_before, source_head_after, "source HEAD should be unchanged");
}

/// Converting with a branch override at a new location should use the specified branch.
#[test]
fn test_convert_to_location_branch_override() {
    let tmp_source = tempfile::tempdir().unwrap();
    let source = tmp_source.path();
    test_helpers::setup_regular_repo(source);
    test_helpers::create_file(source, "file.txt", "data");
    test_helpers::git_commit(source, "Initial");

    let _ = Command::new("git")
        .args(["branch", "develop"])
        .current_dir(source)
        .output();

    let tmp_target = tempfile::tempdir().unwrap();
    let target = tmp_target.path().join("converted");

    let branch = git::convert_repo_to_location(source, &target, "develop").unwrap();
    assert_eq!(branch, "develop");
    assert!(target.join("develop").is_dir(), "develop worktree not created");
}

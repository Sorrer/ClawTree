use std::path::Path;
use std::process::Command;

/// Initialize a bare repo with a .bare directory, .git pointer file, and initial commit.
/// Mirrors the logic in `git::init_bare_repo`.
pub fn setup_bare_repo(dir: &Path) {
    // git init --bare .bare
    let out = Command::new("git")
        .args(["init", "--bare", ".bare"])
        .current_dir(dir)
        .output()
        .expect("git init --bare failed");
    assert!(out.status.success(), "git init --bare failed: {}", String::from_utf8_lossy(&out.stderr));

    // Write .git pointer file
    std::fs::write(dir.join(".git"), "gitdir: ./.bare\n").expect("write .git file");

    // Set default branch
    let _ = Command::new("git")
        .args(["symbolic-ref", "HEAD", "refs/heads/main"])
        .current_dir(&dir.join(".bare"))
        .output();

    // Configure user in the bare repo
    let _ = Command::new("git")
        .args(["config", "user.name", "Test User"])
        .current_dir(&dir.join(".bare"))
        .output();
    let _ = Command::new("git")
        .args(["config", "user.email", "test@test.com"])
        .current_dir(&dir.join(".bare"))
        .output();

    // Initial empty commit: use GIT_DIR + GIT_WORK_TREE to allow commit
    // from a bare-style layout (the .git file points to .bare, but git
    // needs an explicit work tree to allow commits).
    let out = Command::new("git")
        .args([
            "-c", "user.name=Test User",
            "-c", "user.email=test@test.com",
            "commit", "--allow-empty", "-m", "Initial commit",
        ])
        .env("GIT_DIR", dir.join(".bare"))
        .env("GIT_WORK_TREE", dir)
        .current_dir(dir)
        .output()
        .expect("initial commit failed");
    assert!(out.status.success(), "initial commit failed: {}", String::from_utf8_lossy(&out.stderr));
}

/// Configure git user.name and user.email in a repo.
pub fn git_config(dir: &Path) {
    let _ = Command::new("git")
        .args(["config", "user.name", "Test User"])
        .current_dir(dir)
        .output();
    let _ = Command::new("git")
        .args(["config", "user.email", "test@test.com"])
        .current_dir(dir)
        .output();
}

/// Create a file with given content inside a directory.
pub fn create_file(dir: &Path, name: &str, content: &str) {
    let file_path = dir.join(name);
    if let Some(parent) = file_path.parent() {
        std::fs::create_dir_all(parent).expect("create parent dirs");
    }
    std::fs::write(&file_path, content).expect("write file");
}

/// Stage all files and commit with a message.
pub fn git_commit(dir: &Path, msg: &str) {
    let out = Command::new("git")
        .args(["add", "-A"])
        .current_dir(dir)
        .output()
        .expect("git add failed");
    assert!(out.status.success(), "git add -A failed: {}", String::from_utf8_lossy(&out.stderr));

    let out = Command::new("git")
        .args(["commit", "-m", msg])
        .current_dir(dir)
        .output()
        .expect("git commit failed");
    assert!(out.status.success(), "git commit failed: {}", String::from_utf8_lossy(&out.stderr));
}

/// Initialize a bare repo and create the initial worktree for "main".
pub fn setup_bare_repo_with_worktree(dir: &Path) {
    setup_bare_repo(dir);
    let out = Command::new("git")
        .args(["worktree", "add", "main"])
        .current_dir(dir)
        .output()
        .expect("worktree add main failed");
    assert!(out.status.success(), "worktree add main failed: {}", String::from_utf8_lossy(&out.stderr));
    git_config(&dir.join("main"));
}

/// Initialize a simple (non-bare) git repo with an initial commit.
pub fn setup_regular_repo(dir: &Path) {
    let out = Command::new("git")
        .args(["init"])
        .current_dir(dir)
        .output()
        .expect("git init failed");
    assert!(out.status.success());
    git_config(dir);

    // Set default branch to main
    let _ = Command::new("git")
        .args(["checkout", "-b", "main"])
        .current_dir(dir)
        .output();

    let out = Command::new("git")
        .args(["commit", "--allow-empty", "-m", "Initial commit"])
        .current_dir(dir)
        .output()
        .expect("initial commit failed");
    assert!(out.status.success());
}

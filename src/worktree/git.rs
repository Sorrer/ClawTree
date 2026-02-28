use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

/// A file change from `git status --porcelain`.
#[derive(Debug, Clone)]
pub struct FileChange {
    pub path: String,
    pub index_status: char, // X column from `git status --porcelain`
    pub work_status: char,  // Y column
}

/// A parsed worktree entry from `git worktree list --porcelain`.
#[derive(Debug, Clone)]
pub struct GitWorktreeEntry {
    pub path: PathBuf,
    pub head: String,
    pub branch: Option<String>,
    pub is_bare: bool,
}

/// Run a git command and return its stdout as a String.
/// Fails with a descriptive error if the command exits non-zero.
fn run_git(args: &[&str], cwd: &Path, description: &str) -> Result<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .with_context(|| format!("Failed to run {}", description))?;

    if !output.status.success() {
        anyhow::bail!(
            "{} failed: {}",
            description,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// List all worktrees from a bare repo path.
pub fn list_worktrees(bare_repo_path: &Path) -> Result<Vec<GitWorktreeEntry>> {
    let stdout = run_git(&["worktree", "list", "--porcelain"], bare_repo_path, "git worktree list")?;
    parse_worktree_list(&stdout)
}

fn parse_worktree_list(output: &str) -> Result<Vec<GitWorktreeEntry>> {
    let mut entries = Vec::new();
    let mut current_path: Option<PathBuf> = None;
    let mut current_head = String::new();
    let mut current_branch: Option<String> = None;
    let mut is_bare = false;

    for line in output.lines() {
        if let Some(rest) = line.strip_prefix("worktree ") {
            // Save previous entry
            if let Some(path) = current_path.take() {
                entries.push(GitWorktreeEntry {
                    path,
                    head: std::mem::take(&mut current_head),
                    branch: current_branch.take(),
                    is_bare,
                });
                is_bare = false;
            }
            current_path = Some(PathBuf::from(rest));
        } else if let Some(rest) = line.strip_prefix("HEAD ") {
            current_head = rest.to_string();
        } else if let Some(branch_ref) = line.strip_prefix("branch ") {
            // Extract short branch name from refs/heads/...
            current_branch = Some(
                branch_ref
                    .strip_prefix("refs/heads/")
                    .unwrap_or(branch_ref)
                    .to_string(),
            );
        } else if line == "bare" {
            is_bare = true;
        }
    }

    // Don't forget last entry
    if let Some(path) = current_path {
        entries.push(GitWorktreeEntry {
            path,
            head: current_head,
            branch: current_branch,
            is_bare,
        });
    }

    Ok(entries)
}

/// Create a new worktree. If `base_branch` is non-empty, the new branch is based off it.
pub fn create_worktree(bare_repo_path: &Path, branch: &str, rel_path: &str, base_branch: &str) -> Result<()> {
    let mut args = vec!["worktree", "add", "-b", branch, rel_path];
    if !base_branch.is_empty() {
        args.push(base_branch);
    }

    let output = Command::new("git")
        .args(&args)
        .current_dir(bare_repo_path)
        .output()
        .context("Failed to run git worktree add")?;

    if !output.status.success() {
        // Try without -b (branch already exists)
        let output2 = Command::new("git")
            .args(["worktree", "add", rel_path, branch])
            .current_dir(bare_repo_path)
            .output()
            .context("Failed to run git worktree add")?;

        if !output2.status.success() {
            anyhow::bail!(
                "git worktree add failed: {}",
                String::from_utf8_lossy(&output2.stderr)
            );
        }
    }

    Ok(())
}

/// Remove a worktree.
pub fn remove_worktree(bare_repo_path: &Path, worktree_path: &Path) -> Result<()> {
    let wt_str = worktree_path.to_string_lossy();
    run_git(&["worktree", "remove", &wt_str], bare_repo_path, "git worktree remove")?;
    Ok(())
}

/// List local branches.
pub fn list_branches(bare_repo_path: &Path) -> Result<Vec<String>> {
    let stdout = run_git(&["branch", "--format=%(refname:short)"], bare_repo_path, "git branch")?;
    Ok(stdout.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect())
}

/// Result of a merge operation.
pub enum MergeResult {
    /// Merge completed successfully.
    Success(String),
    /// Merge resulted in conflicts. The worktree is in a conflicted state.
    Conflict,
}

/// Check if a worktree has no uncommitted changes (clean working tree + index).
pub fn is_worktree_clean(worktree_path: &Path) -> Result<bool> {
    let stdout = run_git(&["status", "--porcelain"], worktree_path, "git status")?;
    Ok(stdout.trim().is_empty())
}

/// Check if a worktree has an in-progress merge. Returns the source branch
/// name (best-effort via `name-rev`) if a merge is in progress, or `None`.
pub fn merge_in_progress(worktree_path: &Path) -> Option<String> {
    let check = Command::new("git")
        .args(["rev-parse", "-q", "--verify", "MERGE_HEAD"])
        .current_dir(worktree_path)
        .output()
        .ok()?;
    if !check.status.success() {
        return None;
    }
    // Try to resolve MERGE_HEAD to a human-readable branch name
    let name = Command::new("git")
        .args(["name-rev", "--name-only", "--no-undefined", "MERGE_HEAD"])
        .current_dir(worktree_path)
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    Some(if name.is_empty() { "unknown".to_string() } else { name })
}

/// Merge a branch into the current branch of a worktree.
pub fn merge_branch(worktree_path: &Path, source_branch: &str) -> Result<MergeResult> {
    let output = Command::new("git")
        .args(["merge", source_branch])
        .current_dir(worktree_path)
        .output()
        .context("Failed to run git merge")?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let combined = format!("{}{}", stdout, stderr);

    if !output.status.success() {
        if stderr.contains("CONFLICT") || stdout.contains("CONFLICT") {
            return Ok(MergeResult::Conflict);
        }
        anyhow::bail!("git merge failed: {}", combined);
    }

    Ok(MergeResult::Success(combined))
}

/// Abort an in-progress merge.
pub fn merge_abort(worktree_path: &Path) -> Result<()> {
    run_git(&["merge", "--abort"], worktree_path, "git merge --abort")?;
    Ok(())
}

/// Force-remove a worktree (even if dirty).
pub fn force_remove_worktree(bare_repo_path: &Path, worktree_path: &Path) -> Result<()> {
    let wt_str = worktree_path.to_string_lossy();
    run_git(&["worktree", "remove", "--force", &wt_str], bare_repo_path, "git worktree remove --force")?;
    Ok(())
}

/// Initialize a new bare repo workflow in the given directory.
/// Creates `.bare/` (via `git init --bare`), `.git` pointer file,
/// and optionally an initial worktree.
pub fn init_bare_repo(dir: &Path, initial_branch: &str) -> Result<()> {
    let bare_dir = dir.join(".bare");

    // git init --bare .bare
    run_git(&["init", "--bare", ".bare"], dir, "git init --bare")?;

    // Create .git file pointing to .bare
    std::fs::write(dir.join(".git"), "gitdir: ./.bare\n")
        .context("Failed to write .git file")?;

    // Set default branch name
    let _ = Command::new("git")
        .args(["symbolic-ref", "HEAD", &format!("refs/heads/{}", initial_branch)])
        .current_dir(&bare_dir)
        .output();

    // Create an initial empty commit so worktrees have something to branch from
    let _ = Command::new("git")
        .args([
            "-c", "user.name=init",
            "-c", "user.email=init@init",
            "commit", "--allow-empty", "-m", "Initial commit",
        ])
        .current_dir(dir)
        .output();

    // Create the first worktree
    let _ = Command::new("git")
        .args(["worktree", "add", initial_branch])
        .current_dir(dir)
        .output();

    Ok(())
}

/// Clone a remote repo as a bare repo workflow.
/// Creates `.bare/` (via `git clone --bare`), `.git` pointer file,
/// and an initial worktree.
pub fn clone_bare_repo(dir: &Path, url: &str, initial_branch: &str) -> Result<()> {
    // git clone --bare <url> .bare
    let output = Command::new("git")
        .args(["clone", "--bare", url, ".bare"])
        .env("GIT_TERMINAL_PROMPT", "0")
        .current_dir(dir)
        .output()
        .context("Failed to run git clone --bare")?;

    if !output.status.success() {
        anyhow::bail!(
            "git clone --bare failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    // Create .git file pointing to .bare
    std::fs::write(dir.join(".git"), "gitdir: ./.bare\n")
        .context("Failed to write .git file")?;

    // Fix remote fetch config (bare clones default to fetch = +refs/heads/*:refs/heads/*)
    // We want the standard fetch refspec so `git fetch` works properly
    let _ = Command::new("git")
        .args(["config", "remote.origin.fetch", "+refs/heads/*:refs/remotes/origin/*"])
        .current_dir(dir)
        .output();

    // Create the first worktree
    let output = Command::new("git")
        .args(["worktree", "add", initial_branch])
        .current_dir(dir)
        .output()
        .context("Failed to create initial worktree")?;

    if !output.status.success() {
        // Branch might not exist, try creating it
        let _ = Command::new("git")
            .args(["worktree", "add", "-b", initial_branch, initial_branch])
            .current_dir(dir)
            .output();
    }

    Ok(())
}

/// Detect the bare repo root. Walks up from CWD looking for a `.bare` directory
/// or a bare git repo (has HEAD, refs/, objects/ but no .git/).
pub fn detect_bare_repo(start: &Path) -> Option<PathBuf> {
    let mut dir = start.to_path_buf();
    loop {
        // Check for .bare directory (common bare worktree pattern)
        let dot_bare = dir.join(".bare");
        if dot_bare.is_dir() {
            return Some(dir);
        }

        // Check if this directory is itself a bare git repo
        if dir.join("HEAD").is_file() && dir.join("refs").is_dir() && dir.join("objects").is_dir() {
            return Some(dir);
        }

        // Check for .git file (might be a worktree pointing to bare repo)
        let dot_git = dir.join(".git");
        if dot_git.is_file() {
            // Read .git file to find the actual repo
            if let Ok(content) = std::fs::read_to_string(&dot_git) {
                if let Some(gitdir) = content.strip_prefix("gitdir: ") {
                    let gitdir = gitdir.trim();
                    let gitdir_path = if Path::new(gitdir).is_absolute() {
                        PathBuf::from(gitdir)
                    } else {
                        dir.join(gitdir)
                    };
                    // Navigate up from .bare/worktrees/xxx to the repo root
                    if let Some(repo_root) = gitdir_path
                        .canonicalize()
                        .ok()
                        .and_then(|p| {
                            // Typically: /path/to/repo/.bare/worktrees/name
                            // We want: /path/to/repo
                            let p_str = p.to_string_lossy();
                            if p_str.contains(".bare") {
                                let mut ancestor = p.as_path();
                                while let Some(parent) = ancestor.parent() {
                                    if parent.file_name().map(|n| n == ".bare").unwrap_or(false) {
                                        return parent.parent().map(|p| p.to_path_buf());
                                    }
                                    if parent.join(".bare").is_dir() {
                                        return Some(parent.to_path_buf());
                                    }
                                    ancestor = parent;
                                }
                            }
                            None
                        })
                    {
                        return Some(repo_root);
                    }
                }
            }
        }

        if !dir.pop() {
            break;
        }
    }
    None
}

/// Get commits that are ahead of the upstream (unpushed).
/// Returns empty vec if no upstream is set (e.g. local-only branch).
pub fn unpushed_commits(worktree_path: &Path) -> Vec<String> {
    let output = Command::new("git")
        .args(["log", "--oneline", "@{u}..HEAD"])
        .current_dir(worktree_path)
        .output();
    match output {
        Ok(o) if o.status.success() => {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .filter(|l| !l.is_empty())
                .map(|l| l.to_string())
                .collect()
        }
        _ => Vec::new(),
    }
}

/// Get recent commits as one-line summaries ("hash subject").
pub fn log_oneline(worktree_path: &Path, count: usize) -> Result<Vec<String>> {
    let count_arg = format!("-{}", count);
    let stdout = run_git(&["log", "--oneline", &count_arg], worktree_path, "git log --oneline")?;
    Ok(stdout.lines().map(|l| l.to_string()).filter(|l| !l.is_empty()).collect())
}

/// Get the subject line of the HEAD commit.
pub fn head_subject(worktree_path: &Path) -> Result<String> {
    let stdout = run_git(&["log", "-1", "--format=%s"], worktree_path, "git log --format=%s")?;
    Ok(stdout.trim().to_string())
}

/// Get the short hash + subject of a specific branch or ref (e.g. "abc1234 Fix something").
/// `repo_path` can be any worktree or the bare repo — git resolves the ref from there.
pub fn branch_head_oneline(repo_path: &Path, branch: &str) -> Result<String> {
    let desc = format!("git log --oneline -1 {}", branch);
    let stdout = run_git(&["log", "--oneline", "-1", branch], repo_path, &desc)?;
    Ok(stdout.trim().to_string())
}

/// Result of a pull operation.
pub enum PullResult {
    /// Pull completed successfully (fast-forward or clean merge).
    Success,
    /// Pull resulted in merge conflicts.
    Conflict(String),
}

/// Pull the current branch from the remote.
/// Runs `git pull` and detects merge conflicts vs other errors.
pub fn pull_branch(worktree_path: &Path) -> Result<PullResult> {
    let output = Command::new("git")
        .args(["pull"])
        .env("GIT_TERMINAL_PROMPT", "0")
        .current_dir(worktree_path)
        .output()
        .context("Failed to run git pull")?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let combined = format!("{}{}", stdout, stderr);

    if output.status.success() {
        return Ok(PullResult::Success);
    }

    // Detect merge conflicts
    if stderr.contains("CONFLICT") || stdout.contains("CONFLICT")
        || stderr.contains("Automatic merge failed") || stdout.contains("Automatic merge failed")
    {
        return Ok(PullResult::Conflict(combined));
    }

    anyhow::bail!("{}", combined.trim())
}

/// Push a branch to the remote.
/// Tries `git push`, falls back to `git push -u origin <branch>` if no upstream.
pub fn push_branch(worktree_path: &Path, branch: &str) -> Result<String> {
    let output = Command::new("git")
        .args(["push"])
        .env("GIT_TERMINAL_PROMPT", "0")
        .current_dir(worktree_path)
        .output()
        .context("Failed to run git push")?;

    if output.status.success() {
        let msg = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Ok(if msg.is_empty() { "Pushed successfully".to_string() } else { msg });
    }

    // If no upstream, set it up
    let stderr = String::from_utf8_lossy(&output.stderr);
    if stderr.contains("no upstream") || stderr.contains("has no upstream") || stderr.contains("--set-upstream") {
        let output2 = Command::new("git")
            .args(["push", "-u", "origin", branch])
            .env("GIT_TERMINAL_PROMPT", "0")
            .current_dir(worktree_path)
            .output()
            .context("Failed to run git push -u origin")?;

        if output2.status.success() {
            let msg = String::from_utf8_lossy(&output2.stderr).trim().to_string();
            return Ok(if msg.is_empty() { "Pushed successfully (upstream set)".to_string() } else { msg });
        }

        anyhow::bail!(
            "git push failed: {}",
            String::from_utf8_lossy(&output2.stderr)
        );
    }

    anyhow::bail!("git push failed: {}", stderr);
}

/// Parse the output of `git status --porcelain` into a list of file changes.
pub fn parse_status_porcelain(output: &str) -> Vec<FileChange> {
    let mut changes = Vec::new();
    for line in output.lines() {
        if line.len() < 4 {
            continue;
        }
        let bytes = line.as_bytes();
        let index_status = bytes[0] as char;
        let work_status = bytes[1] as char;
        let path = line[3..].to_string();
        changes.push(FileChange {
            path,
            index_status,
            work_status,
        });
    }
    changes
}

/// Get file status using `git status --porcelain`.
pub fn status_porcelain(worktree_path: &Path) -> Result<Vec<FileChange>> {
    let stdout = run_git(&["status", "--porcelain"], worktree_path, "git status --porcelain")?;
    Ok(parse_status_porcelain(&stdout))
}

/// Stage a single file.
pub fn stage_file(worktree_path: &Path, file: &str) -> Result<()> {
    run_git(&["add", "--", file], worktree_path, "git add")?;
    Ok(())
}

/// Unstage a single file.
pub fn unstage_file(worktree_path: &Path, file: &str) -> Result<()> {
    run_git(&["restore", "--staged", "--", file], worktree_path, "git restore --staged")?;
    Ok(())
}

/// Stage all files.
pub fn stage_all(worktree_path: &Path) -> Result<()> {
    run_git(&["add", "-A"], worktree_path, "git add -A")?;
    Ok(())
}

/// Get the diff of staged changes.
pub fn diff_staged(worktree_path: &Path) -> Result<String> {
    run_git(&["diff", "--staged"], worktree_path, "git diff --staged")
}

/// Commit staged changes with a message.
pub fn commit(worktree_path: &Path, message: &str) -> Result<()> {
    run_git(&["commit", "-m", message], worktree_path, "git commit")?;
    Ok(())
}

/// Detect a regular (non-bare) git repo by walking up from `start`.
/// Returns the first directory containing a `.git/` **directory** (not a file).
pub fn detect_regular_repo(start: &Path) -> Option<PathBuf> {
    let mut dir = start.to_path_buf();
    loop {
        let dot_git = dir.join(".git");
        if dot_git.is_dir() {
            return Some(dir);
        }
        if !dir.pop() {
            break;
        }
    }
    None
}

/// Get the current branch name of a repo.
pub fn current_branch_name(repo_path: &Path) -> Result<String> {
    let stdout = run_git(&["rev-parse", "--abbrev-ref", "HEAD"], repo_path, "git rev-parse")?;
    Ok(stdout.trim().to_string())
}

/// Check for in-progress operations (rebase, merge, cherry-pick) in a repo.
fn has_in_progress_operation(repo_path: &Path) -> Option<&'static str> {
    let git_dir = repo_path.join(".git");
    if git_dir.join("rebase-merge").is_dir() || git_dir.join("rebase-apply").is_dir() {
        return Some("rebase");
    }
    if git_dir.join("MERGE_HEAD").is_file() {
        return Some("merge");
    }
    if git_dir.join("CHERRY_PICK_HEAD").is_file() {
        return Some("cherry-pick");
    }
    None
}

/// Recursively move files/directories from `src` into `dst` that don't
/// already exist in `dst`.  Used to preserve .gitignore'd files during
/// conversion (build outputs, .env, IDE configs, etc.).
fn move_missing_entries(src: &Path, dst: &Path) {
    let entries = match std::fs::read_dir(src) {
        Ok(rd) => rd,
        Err(_) => return,
    };
    for entry in entries.filter_map(|e| e.ok()) {
        let name = entry.file_name();
        let dst_path = dst.join(&name);
        if !dst_path.exists() {
            // Not in the checkout — move it over (ignored file)
            let _ = std::fs::rename(entry.path(), &dst_path);
        } else if entry.path().is_dir() && dst_path.is_dir() {
            // Both exist as dirs — recurse to catch nested ignored files
            move_missing_entries(&entry.path(), &dst_path);
        }
        // If both exist as files, the checkout version wins — skip
    }
}

/// Convert a regular git repo to bare worktree layout **in-place**.
/// Returns the branch name on success.
pub fn convert_repo_in_place(repo_path: &Path, branch_override: &str) -> Result<String> {
    // 1. Get current branch name
    let branch = if branch_override.is_empty() {
        current_branch_name(repo_path)?
    } else {
        branch_override.to_string()
    };

    // 2. Guard: check for in-progress operations
    if let Some(op) = has_in_progress_operation(repo_path) {
        anyhow::bail!("Cannot convert: {} in progress. Complete or abort it first.", op);
    }

    // 3. Stash uncommitted changes
    let stash_output = Command::new("git")
        .args(["stash", "push", "--include-untracked", "-m", "clawtree-conversion"])
        .current_dir(repo_path)
        .output()
        .context("Failed to stash changes")?;
    let stashed = stash_output.status.success()
        && !String::from_utf8_lossy(&stash_output.stdout).contains("No local changes");

    // 4. Move all root entries (except .git) into a temp holding dir so
    //    `git worktree add` runs against a clean root and can't collide
    //    with existing directories/files.
    let holding = repo_path.join(".clawtree_convert_tmp");
    std::fs::create_dir_all(&holding)
        .context("Failed to create temp holding directory")?;

    for entry in std::fs::read_dir(repo_path)
        .context("Failed to read repo directory")?
        .filter_map(|e| e.ok())
    {
        let name = entry.file_name();
        if name == ".git" || name == ".clawtree_convert_tmp" {
            continue;
        }
        let _ = std::fs::rename(entry.path(), holding.join(&name));
    }

    // 5. Rename .git/ directory to .bare/
    let dot_git = repo_path.join(".git");
    let dot_bare = repo_path.join(".bare");
    std::fs::rename(&dot_git, &dot_bare)
        .context("Failed to rename .git to .bare")?;

    // 6. Write .git text file
    std::fs::write(repo_path.join(".git"), "gitdir: ./.bare\n")
        .context("Failed to write .git pointer file")?;

    // 7. Mark the repo as bare so git doesn't think the root dir is a worktree
    // (without this, `git worktree add` fails with "already checked out")
    let _ = Command::new("git")
        .args(["config", "core.bare", "true"])
        .current_dir(repo_path)
        .output();

    // 8. Fix remote fetch refspec
    let _ = Command::new("git")
        .args(["config", "remote.origin.fetch", "+refs/heads/*:refs/remotes/origin/*"])
        .current_dir(repo_path)
        .output();

    // 9. Create worktree for the branch
    let wt_output = Command::new("git")
        .args(["worktree", "add", &branch])
        .current_dir(repo_path)
        .output()
        .context("Failed to create worktree")?;

    if !wt_output.status.success() {
        let stderr = String::from_utf8_lossy(&wt_output.stderr);
        tracing::warn!("worktree add failed: {}", stderr);
        // Try with --force as fallback
        let retry = Command::new("git")
            .args(["worktree", "add", "--force", &branch])
            .current_dir(repo_path)
            .output();
        if let Ok(ref out) = retry {
            if !out.status.success() {
                tracing::warn!("worktree add --force also failed: {}", String::from_utf8_lossy(&out.stderr));
            }
        }
    }

    // 10. Verify worktree was created — if not, roll back everything
    let wt_path = repo_path.join(&branch);
    if !wt_path.is_dir() {
        // Undo bare conversion: remove .git file, rename .bare back to .git
        let _ = std::fs::remove_file(repo_path.join(".git"));
        let _ = std::fs::rename(&dot_bare, &dot_git);

        // Restore working tree files from the holding dir
        for entry in std::fs::read_dir(&holding)
            .into_iter()
            .flatten()
            .filter_map(|e| e.ok())
        {
            let _ = std::fs::rename(entry.path(), repo_path.join(entry.file_name()));
        }
        let _ = std::fs::remove_dir_all(&holding);

        // Pop stash back so uncommitted changes aren't lost
        if stashed {
            let _ = Command::new("git")
                .args(["stash", "pop"])
                .current_dir(repo_path)
                .output();
        }

        anyhow::bail!(
            "Failed to create worktree for branch '{}'. The repo has been restored to its original state.",
            branch
        );
    }

    // 11. Move ignored files from the holding dir into the worktree.
    //     Committed files are already checked out by `git worktree add`,
    //     and staged/unstaged/untracked changes are in the stash.
    //     But .gitignore'd files (build outputs, .env, IDE configs, etc.)
    //     are only in the holding dir — move them so they aren't lost.
    move_missing_entries(&holding, &wt_path);
    let _ = std::fs::remove_dir_all(&holding);

    // 12. Pop stash in the new worktree if one was saved
    if stashed && wt_path.is_dir() {
        let _ = Command::new("git")
            .args(["stash", "pop"])
            .current_dir(&wt_path)
            .output();
    }

    Ok(branch)
}

/// Convert a regular git repo to bare worktree layout at a **different location**.
/// Returns the branch name on success.
pub fn convert_repo_to_location(source_repo: &Path, target_dir: &Path, branch_override: &str) -> Result<String> {
    // 1. Get branch name from source repo
    let branch = if branch_override.is_empty() {
        current_branch_name(source_repo)?
    } else {
        branch_override.to_string()
    };

    // 2. Guard: target directory must be empty or non-existent
    if target_dir.is_dir() {
        let count = std::fs::read_dir(target_dir)
            .context("Failed to read target directory")?
            .count();
        if count > 0 {
            anyhow::bail!("Target directory is not empty: {}", target_dir.display());
        }
    }

    // 3. Create target directory
    std::fs::create_dir_all(target_dir)
        .context("Failed to create target directory")?;

    // 4. Clone bare from source's .git directory
    let source_git = source_repo.join(".git");
    let source_url = if source_git.is_dir() {
        source_git.to_string_lossy().to_string()
    } else {
        source_repo.to_string_lossy().to_string()
    };

    let output = Command::new("git")
        .args(["clone", "--bare", &source_url, ".bare"])
        .env("GIT_TERMINAL_PROMPT", "0")
        .current_dir(target_dir)
        .output()
        .context("Failed to run git clone --bare")?;

    if !output.status.success() {
        anyhow::bail!(
            "git clone --bare failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    // 5. Write .git text file in target
    std::fs::write(target_dir.join(".git"), "gitdir: ./.bare\n")
        .context("Failed to write .git pointer file")?;

    // 6. Fix remote fetch refspec
    let _ = Command::new("git")
        .args(["config", "remote.origin.fetch", "+refs/heads/*:refs/remotes/origin/*"])
        .current_dir(target_dir)
        .output();

    // 7. Preserve original remote URL (point to upstream, not local clone)
    let original_url = Command::new("git")
        .args(["config", "--get", "remote.origin.url"])
        .current_dir(source_repo)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());

    if let Some(url) = original_url {
        if !url.is_empty() {
            let _ = Command::new("git")
                .args(["remote", "set-url", "origin", &url])
                .current_dir(target_dir)
                .output();
        }
    }

    // 8. Create worktree for the branch
    let wt_output = Command::new("git")
        .args(["worktree", "add", &branch])
        .current_dir(target_dir)
        .output()
        .context("Failed to create worktree")?;

    if !wt_output.status.success() {
        // Try creating with -b flag
        let _ = Command::new("git")
            .args(["worktree", "add", "-b", &branch, &branch])
            .current_dir(target_dir)
            .output();
    }

    Ok(branch)
}

/// Check whether a git error message indicates an authentication failure.
pub fn is_auth_error(msg: &str) -> bool {
    let lower = msg.to_lowercase();
    lower.contains("terminal prompts disabled")
        || lower.contains("authentication failed")
        || lower.contains("could not read username")
        || lower.contains("could not read password")
        || lower.contains("permission denied (publickey)")
        || lower.contains("http 401")
        || lower.contains("http 403")
        || (lower.contains("fatal: could not read") && lower.contains("terminal"))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── parse_worktree_list tests ──────────────────────────────────────

    #[test]
    fn test_parse_worktree_list() {
        let output = "\
worktree /home/user/project/.bare
HEAD abc123def456
bare

worktree /home/user/project/main
HEAD abc123def456
branch refs/heads/main

worktree /home/user/project/feature-x
HEAD def789abc012
branch refs/heads/feature-x
";
        let entries = parse_worktree_list(output).unwrap();
        assert_eq!(entries.len(), 3);
        assert!(entries[0].is_bare);
        assert_eq!(entries[1].branch.as_deref(), Some("main"));
        assert_eq!(entries[2].branch.as_deref(), Some("feature-x"));
    }

    #[test]
    fn test_parse_worktree_list_empty() {
        let entries = parse_worktree_list("").unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn test_parse_worktree_list_single_bare() {
        let output = "worktree /repo/.bare\nHEAD 0000000000000000000000000000000000000000\nbare\n";
        let entries = parse_worktree_list(output).unwrap();
        assert_eq!(entries.len(), 1);
        assert!(entries[0].is_bare);
        assert!(entries[0].branch.is_none());
        assert_eq!(entries[0].path, PathBuf::from("/repo/.bare"));
    }

    #[test]
    fn test_parse_worktree_list_detached_head() {
        let output = "\
worktree /repo/.bare
HEAD abc123
bare

worktree /repo/detached
HEAD def456
detached
";
        let entries = parse_worktree_list(output).unwrap();
        assert_eq!(entries.len(), 2);
        assert!(entries[1].branch.is_none());
        assert!(!entries[1].is_bare);
    }

    #[test]
    fn test_parse_worktree_list_spaces_in_path() {
        let output = "worktree /home/user/my project/main\nHEAD abc123\nbranch refs/heads/main\n";
        let entries = parse_worktree_list(output).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, PathBuf::from("/home/user/my project/main"));
        assert_eq!(entries[0].branch.as_deref(), Some("main"));
    }

    #[test]
    fn test_parse_worktree_list_no_trailing_newline() {
        let output = "worktree /repo/main\nHEAD abc123\nbranch refs/heads/main";
        let entries = parse_worktree_list(output).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].branch.as_deref(), Some("main"));
    }

    // ── is_auth_error tests ────────────────────────────────────────────

    #[test]
    fn test_is_auth_error_terminal_prompt() {
        assert!(is_auth_error("fatal: could not read Username for 'https://github.com': terminal prompts disabled"));
    }

    #[test]
    fn test_is_auth_error_auth_failed() {
        assert!(is_auth_error("remote: Authentication failed for 'https://github.com/repo.git'"));
    }

    #[test]
    fn test_is_auth_error_publickey() {
        assert!(is_auth_error("git@github.com: Permission denied (publickey)."));
    }

    #[test]
    fn test_is_auth_error_http_401() {
        assert!(is_auth_error("The requested URL returned error: HTTP 401"));
    }

    #[test]
    fn test_is_auth_error_http_403() {
        assert!(is_auth_error("The requested URL returned error: HTTP 403"));
    }

    #[test]
    fn test_is_auth_error_unrelated() {
        assert!(!is_auth_error("fatal: repository not found"));
    }

    #[test]
    fn test_is_auth_error_empty() {
        assert!(!is_auth_error(""));
    }

    #[test]
    fn test_is_auth_error_case_insensitive() {
        assert!(is_auth_error("AUTHENTICATION FAILED for repo"));
        assert!(is_auth_error("Terminal Prompts Disabled"));
    }

    // ── parse_status_porcelain tests ───────────────────────────────────

    #[test]
    fn test_parse_status_porcelain_empty() {
        let changes = parse_status_porcelain("");
        assert!(changes.is_empty());
    }

    #[test]
    fn test_parse_status_porcelain_modified() {
        let changes = parse_status_porcelain(" M src/main.rs\n");
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].index_status, ' ');
        assert_eq!(changes[0].work_status, 'M');
        assert_eq!(changes[0].path, "src/main.rs");
    }

    #[test]
    fn test_parse_status_porcelain_staged() {
        let changes = parse_status_porcelain("M  src/lib.rs\n");
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].index_status, 'M');
        assert_eq!(changes[0].work_status, ' ');
        assert_eq!(changes[0].path, "src/lib.rs");
    }

    #[test]
    fn test_parse_status_porcelain_untracked() {
        let changes = parse_status_porcelain("?? new_file.txt\n");
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].index_status, '?');
        assert_eq!(changes[0].work_status, '?');
        assert_eq!(changes[0].path, "new_file.txt");
    }

    #[test]
    fn test_parse_status_porcelain_mixed() {
        let output = " M src/main.rs\nM  src/lib.rs\n?? todo.txt\n";
        let changes = parse_status_porcelain(output);
        assert_eq!(changes.len(), 3);
        assert_eq!(changes[0].path, "src/main.rs");
        assert_eq!(changes[1].path, "src/lib.rs");
        assert_eq!(changes[2].path, "todo.txt");
    }

    #[test]
    fn test_parse_status_porcelain_renamed() {
        let changes = parse_status_porcelain("R  old.rs -> new.rs\n");
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].index_status, 'R');
        assert_eq!(changes[0].path, "old.rs -> new.rs");
    }

    #[test]
    fn test_parse_status_porcelain_short_line() {
        // Lines shorter than 4 chars should be skipped
        let changes = parse_status_porcelain("M\n M\nMM \n M src/ok.rs\n");
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].path, "src/ok.rs");
    }

    #[test]
    fn test_parse_status_porcelain_added_and_modified() {
        let changes = parse_status_porcelain("AM src/new.rs\n");
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].index_status, 'A');
        assert_eq!(changes[0].work_status, 'M');
        assert_eq!(changes[0].path, "src/new.rs");
    }
}

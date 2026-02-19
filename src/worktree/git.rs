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

/// List all worktrees from a bare repo path.
pub fn list_worktrees(bare_repo_path: &Path) -> Result<Vec<GitWorktreeEntry>> {
    let output = Command::new("git")
        .args(["worktree", "list", "--porcelain"])
        .current_dir(bare_repo_path)
        .output()
        .context("Failed to run git worktree list")?;

    if !output.status.success() {
        anyhow::bail!(
            "git worktree list failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_worktree_list(&stdout)
}

fn parse_worktree_list(output: &str) -> Result<Vec<GitWorktreeEntry>> {
    let mut entries = Vec::new();
    let mut current_path: Option<PathBuf> = None;
    let mut current_head = String::new();
    let mut current_branch: Option<String> = None;
    let mut is_bare = false;

    for line in output.lines() {
        if line.starts_with("worktree ") {
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
            current_path = Some(PathBuf::from(&line[9..]));
        } else if line.starts_with("HEAD ") {
            current_head = line[5..].to_string();
        } else if line.starts_with("branch ") {
            let branch_ref = &line[7..];
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
    let output = Command::new("git")
        .args([
            "worktree",
            "remove",
            &worktree_path.to_string_lossy(),
        ])
        .current_dir(bare_repo_path)
        .output()
        .context("Failed to run git worktree remove")?;

    if !output.status.success() {
        anyhow::bail!(
            "git worktree remove failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    Ok(())
}

/// List local branches.
pub fn list_branches(bare_repo_path: &Path) -> Result<Vec<String>> {
    let output = Command::new("git")
        .args(["branch", "--format=%(refname:short)"])
        .current_dir(bare_repo_path)
        .output()
        .context("Failed to run git branch")?;

    if !output.status.success() {
        anyhow::bail!(
            "git branch failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(stdout.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect())
}

/// Result of a merge operation.
#[allow(dead_code)]
pub enum MergeResult {
    /// Merge completed successfully.
    Success(String),
    /// Merge resulted in conflicts. The worktree is in a conflicted state.
    Conflict(String),
}

/// Check if a worktree has no uncommitted changes (clean working tree + index).
pub fn is_worktree_clean(worktree_path: &Path) -> Result<bool> {
    let output = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(worktree_path)
        .output()
        .context("Failed to run git status")?;

    if !output.status.success() {
        anyhow::bail!(
            "git status failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().is_empty())
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
            return Ok(MergeResult::Conflict(combined));
        }
        anyhow::bail!("git merge failed: {}", combined);
    }

    Ok(MergeResult::Success(combined))
}

/// Abort an in-progress merge.
pub fn merge_abort(worktree_path: &Path) -> Result<()> {
    let output = Command::new("git")
        .args(["merge", "--abort"])
        .current_dir(worktree_path)
        .output()
        .context("Failed to run git merge --abort")?;

    if !output.status.success() {
        anyhow::bail!(
            "git merge --abort failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    Ok(())
}

/// Force-remove a worktree (even if dirty).
pub fn force_remove_worktree(bare_repo_path: &Path, worktree_path: &Path) -> Result<()> {
    let output = Command::new("git")
        .args([
            "worktree",
            "remove",
            "--force",
            &worktree_path.to_string_lossy(),
        ])
        .current_dir(bare_repo_path)
        .output()
        .context("Failed to run git worktree remove --force")?;

    if !output.status.success() {
        anyhow::bail!(
            "git worktree remove --force failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    Ok(())
}

/// Initialize a new bare repo workflow in the given directory.
/// Creates `.bare/` (via `git init --bare`), `.git` pointer file,
/// and optionally an initial worktree.
pub fn init_bare_repo(dir: &Path, initial_branch: &str) -> Result<()> {
    let bare_dir = dir.join(".bare");

    // git init --bare .bare
    let output = Command::new("git")
        .args(["init", "--bare", ".bare"])
        .current_dir(dir)
        .output()
        .context("Failed to run git init --bare")?;

    if !output.status.success() {
        anyhow::bail!(
            "git init --bare failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

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

/// Get recent commits as one-line summaries ("hash subject").
pub fn log_oneline(worktree_path: &Path, count: usize) -> Result<Vec<String>> {
    let output = Command::new("git")
        .args(["log", "--oneline", &format!("-{}", count)])
        .current_dir(worktree_path)
        .output()
        .context("Failed to run git log --oneline")?;

    if !output.status.success() {
        anyhow::bail!(
            "git log --oneline failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(stdout.lines().map(|l| l.to_string()).filter(|l| !l.is_empty()).collect())
}

/// Get the subject line of the HEAD commit.
pub fn head_subject(worktree_path: &Path) -> Result<String> {
    let output = Command::new("git")
        .args(["log", "-1", "--format=%s"])
        .current_dir(worktree_path)
        .output()
        .context("Failed to run git log -1 --format=%s")?;

    if !output.status.success() {
        anyhow::bail!(
            "git log --format=%s failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Push a branch to the remote.
/// Tries `git push`, falls back to `git push -u origin <branch>` if no upstream.
pub fn push_branch(worktree_path: &Path, branch: &str) -> Result<String> {
    let output = Command::new("git")
        .args(["push"])
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

/// Get file status using `git status --porcelain`.
pub fn status_porcelain(worktree_path: &Path) -> Result<Vec<FileChange>> {
    let output = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(worktree_path)
        .output()
        .context("Failed to run git status --porcelain")?;

    if !output.status.success() {
        anyhow::bail!(
            "git status --porcelain failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut changes = Vec::new();
    for line in stdout.lines() {
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
    Ok(changes)
}

/// Stage a single file.
pub fn stage_file(worktree_path: &Path, file: &str) -> Result<()> {
    let output = Command::new("git")
        .args(["add", "--", file])
        .current_dir(worktree_path)
        .output()
        .context("Failed to run git add")?;

    if !output.status.success() {
        anyhow::bail!(
            "git add failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}

/// Unstage a single file.
pub fn unstage_file(worktree_path: &Path, file: &str) -> Result<()> {
    let output = Command::new("git")
        .args(["restore", "--staged", "--", file])
        .current_dir(worktree_path)
        .output()
        .context("Failed to run git restore --staged")?;

    if !output.status.success() {
        anyhow::bail!(
            "git restore --staged failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}

/// Stage all files.
pub fn stage_all(worktree_path: &Path) -> Result<()> {
    let output = Command::new("git")
        .args(["add", "-A"])
        .current_dir(worktree_path)
        .output()
        .context("Failed to run git add -A")?;

    if !output.status.success() {
        anyhow::bail!(
            "git add -A failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}

/// Commit staged changes with a message.
pub fn commit(worktree_path: &Path, message: &str) -> Result<()> {
    let output = Command::new("git")
        .args(["commit", "-m", message])
        .current_dir(worktree_path)
        .output()
        .context("Failed to run git commit")?;

    if !output.status.success() {
        anyhow::bail!(
            "git commit failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
}

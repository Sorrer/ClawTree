use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

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

/// Create a new worktree.
pub fn create_worktree(bare_repo_path: &Path, branch: &str, rel_path: &str) -> Result<()> {
    let output = Command::new("git")
        .args(["worktree", "add", "-b", branch, rel_path])
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

/// Merge a branch into the current branch of a worktree.
pub fn merge_branch(worktree_path: &Path, source_branch: &str) -> Result<String> {
    let output = Command::new("git")
        .args(["merge", source_branch])
        .current_dir(worktree_path)
        .output()
        .context("Failed to run git merge")?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    if !output.status.success() {
        // Check if it's a merge conflict (exit code 1 with conflict markers)
        if stderr.contains("CONFLICT") || stdout.contains("CONFLICT") {
            anyhow::bail!("Merge conflict: {}{}", stdout, stderr);
        }
        anyhow::bail!("git merge failed: {}{}", stdout, stderr);
    }

    Ok(format!("{}{}", stdout, stderr))
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

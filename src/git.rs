use anyhow::{Context, Result};
use std::path::Path;
use std::process::Command;

pub fn is_git_repo(dir: &Path) -> bool {
    dir.join(".git").exists()
}

pub fn current_branch(dir: &Path) -> Result<String> {
    let output = Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .current_dir(dir)
        .output()
        .context("failed to run git rev-parse")?;

    if !output.status.success() {
        anyhow::bail!(
            "git rev-parse failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

pub fn git_status(dir: &Path) -> Result<String> {
    let output = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(dir)
        .output()
        .context("failed to run git status")?;

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

pub fn is_worktree_clean(dir: &Path) -> Result<bool> {
    let status = git_status(dir)?;
    Ok(status.trim().is_empty())
}

pub fn git_diff(dir: &Path) -> Result<String> {
    let output = Command::new("git")
        .args(["diff", "HEAD"])
        .current_dir(dir)
        .output()
        .context("failed to run git diff")?;

    let diff = String::from_utf8_lossy(&output.stdout).to_string();

    if diff.trim().is_empty() {
        let output = Command::new("git")
            .args(["diff"])
            .current_dir(dir)
            .output()
            .context("failed to run git diff (unstaged)")?;
        return Ok(String::from_utf8_lossy(&output.stdout).to_string());
    }

    Ok(diff)
}

pub fn git_diff_stat(dir: &Path) -> Result<String> {
    let output = Command::new("git")
        .args(["diff", "--stat", "HEAD"])
        .current_dir(dir)
        .output()
        .context("failed to run git diff --stat")?;

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

pub fn has_changes(dir: &Path) -> Result<bool> {
    let diff = git_diff(dir)?;
    let status = git_status(dir)?;
    Ok(!diff.trim().is_empty() || !status.trim().is_empty())
}

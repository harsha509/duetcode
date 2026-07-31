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

/// Full picture of uncommitted work: tracked changes plus untracked files
/// rendered as new-file diffs, so reviewers never miss newly created files.
pub fn git_diff(dir: &Path) -> Result<String> {
    let mut diff = tracked_diff(dir)?;
    for path in untracked_files(dir)? {
        diff.push_str(&untracked_as_diff(dir, &path));
    }
    Ok(diff)
}

fn tracked_diff(dir: &Path) -> Result<String> {
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

fn untracked_files(dir: &Path) -> Result<Vec<String>> {
    let output = Command::new("git")
        .args(["ls-files", "--others", "--exclude-standard"])
        .current_dir(dir)
        .output()
        .context("failed to run git ls-files")?;

    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::to_string)
        .collect())
}

const MAX_UNTRACKED_BYTES: usize = 65_536;

fn untracked_as_diff(dir: &Path, rel_path: &str) -> String {
    let header = format!(
        "diff --git a/{0} b/{0}\nnew file mode 100644\n--- /dev/null\n+++ b/{0}\n",
        rel_path
    );

    let body = match std::fs::read(dir.join(rel_path)) {
        Ok(bytes) if bytes.len() > MAX_UNTRACKED_BYTES => "+(large file omitted)\n".to_string(),
        Ok(bytes) => match String::from_utf8(bytes) {
            Ok(text) => text.lines().map(|l| format!("+{}\n", l)).collect(),
            Err(_) => "+(binary file omitted)\n".to_string(),
        },
        Err(e) => format!("+(unreadable: {})\n", e),
    };

    format!("{}{}", header, body)
}

/// Every path with uncommitted work, as `<status> <path>` lines. Tracked
/// changes come from `--name-status`; untracked files are marked `?` so the
/// list matches exactly what `git_diff` renders.
pub fn changed_files(dir: &Path) -> Vec<String> {
    let mut files: Vec<String> = name_status(dir, &["diff", "--name-status", "HEAD"])
        .filter(|list| !list.is_empty())
        .or_else(|| name_status(dir, &["diff", "--name-status"]))
        .unwrap_or_default();

    for path in untracked_files(dir).unwrap_or_default() {
        files.push(format!("? {}", path));
    }
    files
}

fn name_status(dir: &Path, args: &[&str]) -> Option<Vec<String>> {
    let output = Command::new("git").args(args).current_dir(dir).output().ok()?;
    if !output.status.success() {
        return None;
    }
    Some(
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| line.replace('\t', " "))
            .collect(),
    )
}

/// Names the checkout a model is working against — repository, absolute path,
/// branch, and the files in play. Both the reviewer and the writer are given
/// this same block so a finding can never be attributed to the wrong project.
pub fn repo_identity(dir: &Path) -> String {
    let name = dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| dir.display().to_string());
    let branch = current_branch(dir).unwrap_or_else(|_| "unknown".into());
    let files = changed_files(dir);

    let mut block = format!(
        "repository: {}\npath:       {}\nbranch:     {}\n",
        name,
        dir.display(),
        branch
    );

    if files.is_empty() {
        block.push_str("changed files: none\n");
        return block;
    }

    block.push_str(&format!("changed files ({}):\n", files.len()));
    for file in &files {
        block.push_str(&format!("  {}\n", file));
    }
    block
}

pub fn git_diff_stat(dir: &Path) -> Result<String> {
    let output = Command::new("git")
        .args(["diff", "--stat", "HEAD"])
        .current_dir(dir)
        .output()
        .context("failed to run git diff --stat")?;

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

use crate::checks::CheckResult;
use crate::policy::ReviewVerdict;
use anyhow::{Context, Result};
use chrono::Local;
use std::path::{Path, PathBuf};

pub struct SessionLog {
    pub dir: PathBuf,
}

impl SessionLog {
    pub fn create(base_dir: &Path, task: &str) -> Result<Self> {
        let timestamp = Local::now().format("%Y%m%d-%H%M%S");
        let slug = slugify(task);
        let dir = base_dir.join(".duet-logs").join(format!("{}-{}", timestamp, slug));

        std::fs::create_dir_all(&dir)
            .with_context(|| format!("failed to create log dir: {}", dir.display()))?;

        Ok(Self { dir })
    }

    pub fn write_writer_response(&self, round: usize, response: &str) -> Result<()> {
        let path = self.dir.join(format!("round-{}-writer.md", round));
        std::fs::write(&path, response)
            .with_context(|| format!("failed to write {}", path.display()))
    }

    pub fn write_reviewer_response(&self, round: usize, response: &str) -> Result<()> {
        let path = self.dir.join(format!("round-{}-reviewer.md", round));
        std::fs::write(&path, response)
            .with_context(|| format!("failed to write {}", path.display()))
    }

    pub fn write_diff(&self, round: usize, diff: &str) -> Result<()> {
        let path = self.dir.join(format!("round-{}-diff.patch", round));
        std::fs::write(&path, diff)
            .with_context(|| format!("failed to write {}", path.display()))
    }

    pub fn write_checks(&self, round: usize, results: &[CheckResult]) -> Result<()> {
        let path = self.dir.join(format!("round-{}-checks.json", round));
        let json = serde_json::to_string_pretty(results)
            .context("failed to serialize check results")?;
        std::fs::write(&path, json)
            .with_context(|| format!("failed to write {}", path.display()))
    }

    pub fn write_summary(
        &self,
        task: &str,
        writer: &str,
        reviewer: &str,
        rounds: usize,
        verdict: &ReviewVerdict,
        checks_passed: bool,
        success: bool,
    ) -> Result<()> {
        let summary = serde_json::json!({
            "task": task,
            "writer": writer,
            "reviewer": reviewer,
            "total_rounds": rounds,
            "final_verdict": format!("{:?}", verdict.verdict),
            "blockers": verdict.blockers,
            "suggestions": verdict.suggestions,
            "checks_passed": checks_passed,
            "success": success,
            "timestamp": Local::now().to_rfc3339(),
        });

        let path = self.dir.join("summary.json");
        let json = serde_json::to_string_pretty(&summary)
            .context("failed to serialize summary")?;
        std::fs::write(&path, json)
            .with_context(|| format!("failed to write {}", path.display()))
    }
}

fn slugify(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c.is_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .take(6)
        .collect::<Vec<_>>()
        .join("-")
}

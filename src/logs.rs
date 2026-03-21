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
        let dir = base_dir.join(".duet").join("sessions").join(format!("{}-{}", timestamp, slug));

        std::fs::create_dir_all(&dir)
            .with_context(|| format!("failed to create log dir: {}", dir.display()))?;

        let prompt_path = dir.join("prompt.md");
        std::fs::write(&prompt_path, task)
            .with_context(|| format!("failed to write {}", prompt_path.display()))?;

        Ok(Self { dir })
    }

    pub fn write_writer_response(&self, round: usize, response: &str) -> Result<()> {
        let round_dir = self.dir.join(format!("round-{}", round));
        std::fs::create_dir_all(&round_dir)?;
        let path = round_dir.join("claude_out.md");
        std::fs::write(&path, response)
            .with_context(|| format!("failed to write {}", path.display()))
    }

    pub fn write_reviewer_response(&self, round: usize, response: &str) -> Result<()> {
        let round_dir = self.dir.join(format!("round-{}", round));
        std::fs::create_dir_all(&round_dir)?;
        let path = round_dir.join("gemini_out.md");
        std::fs::write(&path, response)
            .with_context(|| format!("failed to write {}", path.display()))
    }

    pub fn write_diff(&self, round: usize, diff: &str) -> Result<()> {
        let round_dir = self.dir.join(format!("round-{}", round));
        std::fs::create_dir_all(&round_dir)?;
        let path = round_dir.join("claude.patch");
        std::fs::write(&path, diff)
            .with_context(|| format!("failed to write {}", path.display()))
    }

    pub fn write_checks(&self, round: usize, results: &[CheckResult]) -> Result<()> {
        let round_dir = self.dir.join(format!("round-{}", round));
        std::fs::create_dir_all(&round_dir)?;
        let path = round_dir.join("checks.json");
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

        let path = self.dir.join("state.json");
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

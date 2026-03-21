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

    pub fn get_last_session(base_dir: &Path) -> Result<Option<PathBuf>> {
        let sessions_dir = base_dir.join(".duet").join("sessions");
        if !sessions_dir.exists() {
            return Ok(None);
        }

        let mut entries: Vec<_> = std::fs::read_dir(&sessions_dir)?
            .filter_map(Result::ok)
            .filter(|e| e.path().is_dir())
            .collect();

        // Sort by directory name (which starts with timestamp %Y%m%d-%H%M%S)
        entries.sort_by_key(|e| e.file_name());

        Ok(entries.last().map(|e| e.path()))
    }

    pub fn read_session_context(session_dir: &Path) -> Result<String> {
        let prompt_path = session_dir.join("prompt.md");
        let prompt = if prompt_path.exists() {
            std::fs::read_to_string(&prompt_path).unwrap_or_default()
        } else {
            String::new()
        };

        // Find the last round's claude output (or gemini if it was the writer)
        let mut last_response = String::new();
        for round in (1..=10).rev() {
            let claude_path = session_dir.join(format!("round-{}", round)).join("claude_out.md");
            let gemini_path = session_dir.join(format!("round-{}", round)).join("gemini_out.md");
            
            if claude_path.exists() {
                last_response = std::fs::read_to_string(&claude_path).unwrap_or_default();
                break;
            } else if gemini_path.exists() {
                last_response = std::fs::read_to_string(&gemini_path).unwrap_or_default();
                break;
            }
        }

        // If no rounds exist, maybe it was a plan mode session, check round-0
        if last_response.is_empty() {
            let plan_path = session_dir.join("round-0").join("claude_out.md");
            if plan_path.exists() {
                last_response = std::fs::read_to_string(&plan_path).unwrap_or_default();
            }
        }

        if prompt.is_empty() && last_response.is_empty() {
            return Ok(String::new());
        }

        Ok(format!(
            "PREVIOUS TASK:\n{}\n\nPREVIOUS AI RESPONSE:\n{}",
            prompt, last_response
        ))
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

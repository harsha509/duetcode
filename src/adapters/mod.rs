pub mod claude;
pub mod gemini;
pub mod pricing;

use anyhow::Result;
use std::path::PathBuf;

/// Cap on remembered conversation turns (user + model messages).
pub(crate) const MAX_HISTORY_TURNS: usize = 12;
/// Cap on serialized history size sent per request.
pub(crate) const MAX_HISTORY_BYTES: usize = 300_000;

/// Drop the oldest exchanges (in user/model pairs, so alternation survives)
/// until the history fits both caps. Call this while the history contains
/// only completed pairs, i.e. before appending a new user turn.
pub(crate) fn trim_history(history: &mut Vec<serde_json::Value>) {
    while history.len() > MAX_HISTORY_TURNS {
        history.drain(..2);
    }
    while history.len() > 2 && total_bytes(history) > MAX_HISTORY_BYTES {
        history.drain(..2);
    }
}

fn total_bytes(history: &[serde_json::Value]) -> usize {
    history.iter().map(|v| v.to_string().len()).sum()
}

#[derive(Debug, Clone, Default)]
pub struct UsageStats {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cost_usd: Option<f64>,
    pub model: String,
}

impl std::fmt::Display for UsageStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "tokens: {}in / {}out", self.input_tokens, self.output_tokens)?;
        if let Some(cost) = self.cost_usd {
            write!(f, " | cost: ${:.6}", cost)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct ImageInput {
    pub media_type: String,
    pub data: Vec<u8>,
}

impl ImageInput {
    pub fn load(path: PathBuf) -> Result<Self> {
        let extension = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();

        let media_type = match extension.as_str() {
            "png" => "image/png",
            "jpg" | "jpeg" => "image/jpeg",
            "gif" => "image/gif",
            "webp" => "image/webp",
            other => {
                anyhow::bail!(
                    "unsupported image format '.{}' — use png, jpg, gif, or webp",
                    other
                );
            }
        }
        .to_string();

        let data =
            std::fs::read(&path).map_err(|e| anyhow::anyhow!("cannot read {}: {}", path.display(), e))?;

        Ok(Self { media_type, data })
    }

    pub fn base64_data(&self) -> String {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD.encode(&self.data)
    }
}

/// A model that can take part in the write/review loop.
///
/// Adapters are stateful: each `generate` call continues the same session,
/// so a model remembers its earlier prompts and replies within one `dt` run.
pub trait ModelAdapter {
    fn generate(&mut self, prompt: &str, images: &[ImageInput]) -> Result<(String, UsageStats)>;

    fn name(&self) -> &str;

    fn streams_output(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn turn(role: &str, text: &str) -> serde_json::Value {
        json!({ "role": role, "content": text })
    }

    #[test]
    fn trim_keeps_recent_turns_under_turn_cap() {
        let mut history: Vec<_> = (0..20)
            .map(|i| turn(if i % 2 == 0 { "user" } else { "model" }, &format!("t{}", i)))
            .collect();
        trim_history(&mut history);
        assert!(history.len() <= MAX_HISTORY_TURNS);
        assert_eq!(history.last().unwrap()["content"], "t19");
        assert_eq!(history[0]["role"], "user");
    }

    #[test]
    fn trim_enforces_byte_cap() {
        let big = "x".repeat(MAX_HISTORY_BYTES / 2);
        let mut history = vec![
            turn("user", &big),
            turn("model", &big),
            turn("user", "small"),
            turn("model", "small"),
        ];
        trim_history(&mut history);
        assert_eq!(history.len(), 2);
        assert_eq!(history[0]["content"], "small");
    }

    #[test]
    fn trim_leaves_short_history_alone() {
        let mut history = vec![turn("user", "a"), turn("model", "b")];
        trim_history(&mut history);
        assert_eq!(history.len(), 2);
    }
}

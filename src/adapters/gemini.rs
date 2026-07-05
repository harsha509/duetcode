use super::{pricing, ImageInput, ModelAdapter, UsageStats};
use crate::config::GeminiConfig;
use crate::ui;
use anyhow::Result;
use colored::Colorize;
use std::io::{BufRead, BufReader, Write};
use std::time::Duration;

const GEMINI_API_BASE: &str = "https://generativelanguage.googleapis.com/v1beta/models";

pub struct GeminiAdapter {
    model: String,
    api_key: String,
    agent: ureq::Agent,
    /// Conversation history in Gemini `contents` format, so each review
    /// remembers what it said in earlier rounds.
    history: Vec<serde_json::Value>,
}

impl GeminiAdapter {
    pub fn new(config: &GeminiConfig) -> Result<Self> {
        let api_key = std::env::var(&config.api_key_env).map_err(|_| {
            anyhow::anyhow!(
                "{} not set — export it or add to your shell profile",
                config.api_key_env
            )
        })?;

        let agent = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(30))
            .timeout_read(Duration::from_secs(config.timeout_secs))
            .build();

        Ok(Self {
            model: config.model.clone(),
            api_key,
            agent,
            history: Vec::new(),
        })
    }

    pub fn is_key_available(config: &GeminiConfig) -> bool {
        std::env::var(&config.api_key_env).is_ok()
    }

    fn build_parts(prompt: &str, images: &[ImageInput]) -> Vec<serde_json::Value> {
        let mut parts = vec![serde_json::json!({ "text": prompt })];

        for img in images {
            parts.push(serde_json::json!({
                "inlineData": {
                    "mimeType": img.media_type,
                    "data": img.base64_data()
                }
            }));
        }

        parts
    }

    fn model_path(&self) -> String {
        if self.model.starts_with('/') {
            self.model.clone()
        } else {
            format!("/{}", self.model)
        }
    }

    fn stream_generate(
        &mut self,
        prompt: &str,
        images: &[ImageInput],
    ) -> Result<(String, UsageStats)> {
        super::trim_history(&mut self.history);
        self.history.push(serde_json::json!({
            "role": "user",
            "parts": Self::build_parts(prompt, images)
        }));

        let body = serde_json::json!({ "contents": self.history });
        let url = format!(
            "{}{}:streamGenerateContent?alt=sse",
            GEMINI_API_BASE,
            self.model_path(),
        );

        match self.exchange(&url, &body) {
            Ok((text, usage)) => {
                self.history.push(serde_json::json!({
                    "role": "model",
                    "parts": [{ "text": text }]
                }));
                Ok((text, usage))
            }
            Err(e) => {
                self.history.pop();
                Err(e)
            }
        }
    }

    fn exchange(&self, url: &str, body: &serde_json::Value) -> Result<(String, UsageStats)> {
        eprintln!("  {} streaming from {}", "●".green(), self.model);
        eprintln!("  {} thinking...", "◌".cyan());

        let start = std::time::Instant::now();

        // Key goes in a header so it never shows up in URLs or error output.
        let response = self.agent.post(url)
            .set("x-goog-api-key", &self.api_key)
            .send_json(body)
            .map_err(|e| match e {
                ureq::Error::Status(code, response) => {
                    let error_body = response.into_string().unwrap_or_default();
                    let error_msg = extract_api_error(&error_body).unwrap_or(error_body);
                    anyhow::anyhow!("Gemini API returned {}: {}", code, error_msg)
                }
                ureq::Error::Transport(t) => {
                    anyhow::anyhow!(
                        "failed to reach Gemini API: {} — \
                         try increasing timeout_secs in .duet/config.toml [gemini] \
                         or try a faster model like 'gemini-2.0-flash'",
                        t
                    )
                }
            })?;

        let reader = BufReader::new(response.into_reader());
        let mut collected = String::new();
        let mut header_printed = false;
        let mut input_tokens: u64 = 0;
        let mut output_tokens: u64 = 0;
        let mut chunk_count: u64 = 0;

        for line in reader.lines() {
            let line = match line {
                Ok(l) => l,
                Err(e) => {
                    eprintln!("  {} stream read error: {}", "✗".red(), e);
                    break;
                }
            };

            let trimmed = line.trim();
            if !trimmed.starts_with("data: ") {
                continue;
            }
            let json_str = &trimmed[6..];
            if json_str == "[DONE]" {
                break;
            }

            let chunk: serde_json::Value = match serde_json::from_str(json_str) {
                Ok(v) => v,
                Err(_) => continue,
            };

            chunk_count += 1;

            if let Some(text) = chunk
                .pointer("/candidates/0/content/parts/0/text")
                .and_then(|v| v.as_str())
            {
                if !text.is_empty() {
                    if !header_printed {
                        ui::stream_header("gemini");
                        header_printed = true;
                    }
                    eprint!("{}", text);
                    let _ = std::io::stderr().lock().flush();
                    collected.push_str(text);
                }
            }

            if let Some(meta) = chunk.get("usageMetadata") {
                if let Some(it) = meta.get("promptTokenCount").and_then(|v| v.as_u64()) {
                    input_tokens = it;
                }
                if let Some(ot) = meta.get("candidatesTokenCount").and_then(|v| v.as_u64()) {
                    output_tokens = ot;
                }
            }
        }

        if header_printed {
            eprintln!();
        }

        let elapsed = start.elapsed().as_secs_f64();
        let cost_usd = pricing::compute_cost(&self.model, input_tokens, output_tokens);

        eprintln!(
            "  {} finished ({:.1}s, {} chunks)",
            "●".green(),
            elapsed,
            chunk_count,
        );
        ui::stream_footer();

        if collected.is_empty() {
            anyhow::bail!(
                "Gemini returned empty response after {:.1}s — the model may have filtered the output",
                elapsed
            );
        }

        let usage = UsageStats {
            input_tokens,
            output_tokens,
            cost_usd,
            model: self.model.clone(),
        };

        Ok((collected, usage))
    }
}

impl ModelAdapter for GeminiAdapter {
    fn generate(
        &mut self,
        prompt: &str,
        images: &[ImageInput],
    ) -> Result<(String, UsageStats)> {
        self.stream_generate(prompt, images)
    }

    fn name(&self) -> &str {
        "gemini"
    }

    fn streams_output(&self) -> bool {
        true
    }
}

fn extract_api_error(response: &str) -> Option<String> {
    let json: serde_json::Value = serde_json::from_str(response).ok()?;
    let message = json.get("error")?.get("message")?.as_str()?;
    Some(message.to_string())
}

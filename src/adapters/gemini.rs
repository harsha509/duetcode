use super::{ImageInput, ModelAdapter, UsageStats};
use super::pricing;
use crate::config::GeminiConfig;
use anyhow::{Context, Result};
use colored::Colorize;
use std::io::{BufRead, BufReader, Write};
use std::time::Duration;

const GEMINI_API_BASE: &str = "https://generativelanguage.googleapis.com/v1beta/models";

pub struct GeminiAdapter {
    model: String,
    api_key: String,
    client: reqwest::blocking::Client,
}

impl GeminiAdapter {
    pub fn new(config: &GeminiConfig) -> Result<Self> {
        let api_key = std::env::var(&config.api_key_env).map_err(|_| {
            anyhow::anyhow!(
                "{} not set — export it or add to your shell profile",
                config.api_key_env
            )
        })?;

        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(config.timeout_secs))
            .build()
            .context("failed to build HTTP client")?;

        Ok(Self {
            model: config.model.clone(),
            api_key,
            client,
        })
    }

    pub fn is_key_available(config: &GeminiConfig) -> bool {
        std::env::var(&config.api_key_env).is_ok()
    }

    fn build_parts(prompt: &str, context: &str, images: &[ImageInput]) -> Vec<serde_json::Value> {
        let full_text = if context.is_empty() {
            prompt.to_string()
        } else {
            format!("{}\n\nCONTEXT:\n{}", prompt, context)
        };

        let mut parts = vec![serde_json::json!({ "text": full_text })];

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
        &self,
        prompt: &str,
        context: &str,
        images: &[ImageInput],
    ) -> Result<(String, UsageStats)> {
        let parts = Self::build_parts(prompt, context, images);

        let body = serde_json::json!({
            "contents": [{
                "parts": parts
            }]
        });

        let url = format!(
            "{}{}:streamGenerateContent?alt=sse&key={}",
            GEMINI_API_BASE,
            self.model_path(),
            self.api_key
        );

        eprintln!("  {} streaming from {}", "●".green(), self.model);
        eprintln!("  {} thinking...", "◌".cyan());

        let start = std::time::Instant::now();

        let response = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .map_err(|e| {
                if e.is_timeout() {
                    anyhow::anyhow!(
                        "Gemini API timed out — the model '{}' may need more time. \
                         Increase timeout_secs in duet.toml [gemini] section, \
                         or try a faster model like 'gemini-2.0-flash'",
                        self.model
                    )
                } else {
                    anyhow::anyhow!("failed to send request to Gemini API: {}", e)
                }
            })?;

        let status = response.status();
        if !status.is_success() {
            let error_body = response.text().unwrap_or_default();
            let error_msg = extract_api_error(&error_body)
                .unwrap_or_else(|| error_body.clone());
            anyhow::bail!("Gemini API returned {}: {}", status, error_msg);
        }

        let reader = BufReader::new(response);
        let mut collected = String::new();
        let mut header_printed = false;
        let separator = "─".repeat(60);
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
                        eprintln!("\n  {}", separator.dimmed());
                        eprintln!("  {}", "gemini:".cyan().bold());
                        eprintln!("  {}", separator.dimmed());
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
        eprintln!("  {}", separator.dimmed());

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
        &self,
        prompt: &str,
        context: &str,
        images: &[ImageInput],
    ) -> Result<(String, UsageStats)> {
        self.stream_generate(prompt, context, images)
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

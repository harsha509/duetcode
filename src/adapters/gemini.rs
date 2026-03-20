use super::{ImageInput, ModelAdapter};
use crate::config::GeminiConfig;
use anyhow::{Context, Result};
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
}

impl ModelAdapter for GeminiAdapter {
    fn generate(
        &self,
        prompt: &str,
        context: &str,
        images: &[ImageInput],
    ) -> Result<String> {
        let parts = Self::build_parts(prompt, context, images);

        let body = serde_json::json!({
            "contents": [{
                "parts": parts
            }]
        });

        let url = format!(
            "{}{}:generateContent?key={}",
            GEMINI_API_BASE,
            if self.model.starts_with('/') { self.model.clone() } else { format!("/{}", self.model) },
            self.api_key
        );

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
        let response_text = response
            .text()
            .context("failed to read Gemini API response")?;

        if !status.is_success() {
            let error_msg = extract_api_error(&response_text)
                .unwrap_or_else(|| response_text.clone());
            anyhow::bail!("Gemini API returned {}: {}", status, error_msg);
        }

        let json: serde_json::Value = serde_json::from_str(&response_text)
            .context("failed to parse Gemini API response as JSON")?;

        let text = json
            .get("candidates")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("content"))
            .and_then(|c| c.get("parts"))
            .and_then(|p| p.get(0))
            .and_then(|p| p.get("text"))
            .and_then(|t| t.as_str())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "unexpected Gemini API response structure: {}",
                    &response_text[..response_text.len().min(500)]
                )
            })?;

        Ok(text.to_string())
    }

    fn name(&self) -> &str {
        "gemini"
    }
}

fn extract_api_error(response: &str) -> Option<String> {
    let json: serde_json::Value = serde_json::from_str(response).ok()?;
    let message = json.get("error")?.get("message")?.as_str()?;
    Some(message.to_string())
}

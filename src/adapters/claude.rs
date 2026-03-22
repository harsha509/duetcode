use super::{ImageInput, ModelAdapter, UsageStats};
use super::pricing;
use crate::config::ClaudeConfig;
use anyhow::{Context, Result};
use colored::Colorize;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

const ANTHROPIC_API_URL: &str = "https://api.anthropic.com/v1/messages";
const ANTHROPIC_VERSION: &str = "2023-06-01";

pub struct ClaudeAdapter {
    config: ClaudeConfig,
    working_dir: std::path::PathBuf,
    verbose: bool,
    use_api: bool,
    api_key: Option<String>,
    client: Option<reqwest::blocking::Client>,
}

impl ClaudeAdapter {
    pub fn new(config: &ClaudeConfig, working_dir: &Path, verbose: bool) -> Self {
        let mode = config.mode.to_lowercase();

        let cli_available = mode != "api" && Self::check_cli_available(&config.command);
        let api_key = std::env::var(&config.api_key_env)
            .ok()
            .filter(|k| Self::is_real_api_key(k));
        let has_api_key = api_key.is_some();

        let use_api = match mode.as_str() {
            "api" => true,
            "cli" => false,
            _ => !cli_available && has_api_key, // "auto": prefer CLI, fall back to API
        };

        let client = if use_api || mode == "auto" {
            reqwest::blocking::Client::builder()
                .timeout(Duration::from_secs(config.timeout_secs))
                .build()
                .ok()
        } else {
            None
        };

        if verbose {
            if use_api {
                eprintln!("  {} claude mode: API (direct)", "[verbose]".dimmed());
            } else {
                eprintln!("  {} claude mode: CLI ({})", "[verbose]".dimmed(), config.command);
            }
        }

        Self {
            config: config.clone(),
            working_dir: working_dir.to_path_buf(),
            verbose,
            use_api,
            api_key,
            client,
        }
    }

    fn is_real_api_key(key: &str) -> bool {
        let k = key.trim();
        !k.is_empty() && k != "sk-ant-xxx" && k.starts_with("sk-ant-")
    }

    fn check_cli_available(command: &str) -> bool {
        Command::new("which")
            .arg(command)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    pub fn is_available(&self) -> bool {
        Self::check_cli_available(&self.config.command)
    }

    pub fn is_api_key_available(&self) -> bool {
        self.api_key.is_some()
    }

    pub fn check_auth(&self) -> Result<String> {
        let output = Command::new(&self.config.command)
            .args(["auth", "status", "--text"])
            .output()
            .with_context(|| format!("failed to run '{} auth status'", self.config.command))?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        if output.status.success() {
            Ok(stdout.trim().to_string())
        } else {
            let msg = if !stderr.trim().is_empty() { stderr } else { stdout };
            anyhow::bail!("not authenticated: {}", msg.trim())
        }
    }

    #[allow(dead_code)]
    pub fn check_works(&self) -> Result<String> {
        use std::time::Instant;

        let start = Instant::now();
        let mut child = Command::new(&self.config.command)
            .args(["-p", "say ok", "--output-format", "text", "--max-turns", "1"])
            .current_dir(&self.working_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("failed to run '{} -p'", self.config.command))?;

        let timeout = Duration::from_secs(15);
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    let output = child.wait_with_output()?;
                    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
                    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

                    if status.success() {
                        return Ok(format!("responded in {:.1}s", start.elapsed().as_secs_f64()));
                    } else {
                        let msg = if !stderr.trim().is_empty() { stderr } else { stdout };
                        anyhow::bail!("{}", msg.trim());
                    }
                }
                Ok(None) => {
                    if start.elapsed() > timeout {
                        let _ = child.kill();
                        anyhow::bail!("timed out after {}s", timeout.as_secs());
                    }
                    std::thread::sleep(Duration::from_millis(200));
                }
                Err(e) => anyhow::bail!("failed to check process: {}", e),
            }
        }
    }

    // ── API mode (direct Anthropic REST API with SSE streaming) ──

    fn run_api(&self, prompt: &str, context: &str, images: &[ImageInput]) -> Result<(String, UsageStats)> {
        let api_key = self.api_key.as_ref()
            .ok_or_else(|| anyhow::anyhow!(
                "{} not set — export it or add to your shell profile",
                self.config.api_key_env
            ))?;

        let client = self.client.as_ref()
            .ok_or_else(|| anyhow::anyhow!("HTTP client not initialized"))?;

        let full_text = if context.is_empty() {
            prompt.to_string()
        } else {
            format!("{}\n\nCONTEXT:\n{}", prompt, context)
        };

        let content = if images.is_empty() {
            serde_json::json!([{ "type": "text", "text": full_text }])
        } else {
            let mut parts = vec![serde_json::json!({ "type": "text", "text": full_text })];
            for img in images {
                parts.push(serde_json::json!({
                    "type": "image",
                    "source": {
                        "type": "base64",
                        "media_type": img.media_type,
                        "data": img.base64_data()
                    }
                }));
            }
            serde_json::json!(parts)
        };

        let body = serde_json::json!({
            "model": self.config.api_model,
            "max_tokens": 8192,
            "stream": true,
            "messages": [{
                "role": "user",
                "content": content
            }]
        });

        if self.verbose {
            eprintln!("  {} POST {} (model: {})", "[verbose]".dimmed(), ANTHROPIC_API_URL, self.config.api_model);
        }

        let response = client
            .post(ANTHROPIC_API_URL)
            .header("x-api-key", api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .map_err(|e| {
                if e.is_timeout() {
                    anyhow::anyhow!(
                        "Anthropic API timed out — increase timeout_secs in duet.toml [claude] section"
                    )
                } else {
                    anyhow::anyhow!("failed to send request to Anthropic API: {}", e)
                }
            })?;

        let status = response.status();
        if !status.is_success() {
            let error_body = response.text().unwrap_or_default();
            let error_msg = serde_json::from_str::<serde_json::Value>(&error_body)
                .ok()
                .and_then(|v| v.pointer("/error/message").and_then(|m| m.as_str()).map(String::from))
                .unwrap_or(error_body);
            anyhow::bail!("Anthropic API returned {}: {}", status, error_msg);
        }

        self.parse_sse_stream(response)
    }

    fn parse_sse_stream(&self, response: reqwest::blocking::Response) -> Result<(String, UsageStats)> {
        let reader = BufReader::new(response);
        let mut collected = String::new();
        let mut header_printed = false;
        let separator = "─".repeat(60);
        let start = std::time::Instant::now();
        let mut input_tokens: u64 = 0;
        let mut output_tokens: u64 = 0;
        let mut model_name = self.config.api_model.clone();

        for line in reader.lines() {
            let line = match line {
                Ok(l) => l,
                Err(e) => {
                    eprintln!("  {} SSE read error: {}", "✗".red(), e);
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

            let event: serde_json::Value = match serde_json::from_str(json_str) {
                Ok(v) => v,
                Err(_) => continue,
            };

            let event_type = event.get("type").and_then(|v| v.as_str()).unwrap_or("");

            match event_type {
                "content_block_delta" => {
                    if let Some(text) = event.pointer("/delta/text").and_then(|v| v.as_str()) {
                        if !header_printed {
                            eprintln!("\n  {}", separator.dimmed());
                            eprintln!("  {}", "claude:".cyan().bold());
                            eprintln!("  {}", separator.dimmed());
                            header_printed = true;
                        }
                        eprint!("{}", text);
                        let _ = std::io::stderr().lock().flush();
                        collected.push_str(text);
                    }
                }
                "message_start" => {
                    let model = event.pointer("/message/model").and_then(|v| v.as_str()).unwrap_or("?");
                    model_name = model.to_string();
                    if let Some(it) = event.pointer("/message/usage/input_tokens").and_then(|v| v.as_u64()) {
                        input_tokens = it;
                    }
                    eprintln!("  {} streaming from {}", "●".green(), model);
                    eprintln!("  {} thinking...", "◌".cyan());
                }
                "content_block_start" => {
                    let block_type = event.pointer("/content_block/type").and_then(|v| v.as_str()).unwrap_or("");
                    if block_type == "thinking" {
                        eprintln!("  {} reasoning...", "◌".cyan());
                    }
                }
                "message_delta" => {
                    if let Some(ot) = event.pointer("/usage/output_tokens").and_then(|v| v.as_u64()) {
                        output_tokens = ot;
                    }
                    if let Some(reason) = event.pointer("/delta/stop_reason").and_then(|v| v.as_str()) {
                        let elapsed = start.elapsed().as_secs_f64();
                        eprintln!("  {} finished ({:.1}s, reason: {})", "●".green(), elapsed, reason);
                    }
                }
                "message_stop" => {}
                "error" => {
                    let msg = event.pointer("/error/message").and_then(|v| v.as_str()).unwrap_or("unknown error");
                    eprintln!("  {} API error: {}", "✗".red(), msg);
                }
                _ => {
                    eprintln!("  {} {} ({:.0}s)", "·".dimmed(), event_type, start.elapsed().as_secs_f64());
                }
            }
        }

        if header_printed {
            eprintln!();
        }
        eprintln!("  {}", separator.dimmed());

        let cost_usd = pricing::compute_cost(&model_name, input_tokens, output_tokens);
        let usage = UsageStats {
            input_tokens,
            output_tokens,
            cost_usd,
            model: model_name,
        };

        Ok((collected, usage))
    }

    // ── CLI mode (spawn claude command) ──

    fn run_cli(&self, prompt: &str, context: &str, images: &[ImageInput]) -> Result<(String, UsageStats)> {
        let full_prompt = if context.is_empty() {
            prompt.to_string()
        } else {
            format!("{}\n\nCONTEXT:\n{}", prompt, context)
        };

        if images.is_empty() {
            self.run_cli_simple(&full_prompt)
        } else {
            self.run_cli_with_images(&full_prompt, images)
        }
    }

    fn run_cli_simple(&self, full_prompt: &str) -> Result<(String, UsageStats)> {
        let mut cmd = Command::new(&self.config.command);
        cmd.arg("-p")
            .arg(full_prompt)
            .arg("--model")
            .arg(&self.config.model)
            .arg("--output-format")
            .arg("stream-json")
            .arg("--verbose")
            .arg("--dangerously-skip-permissions")
            .current_dir(&self.working_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        if self.verbose {
            eprintln!("  {} {} -p <prompt> --output-format stream-json --verbose --dangerously-skip-permissions", "[verbose]".dimmed(), self.config.command);
        }

        let mut child = cmd
            .spawn()
            .with_context(|| format!("failed to execute '{}'", self.config.command))?;

        let (stdout, usage) = self.stream_cli_json(&mut child)?;
        let status = child.wait().context("failed to wait for claude")?;
        let stderr = self.collect_stderr(&mut child);

        if self.verbose && !stderr.is_empty() {
            eprintln!("  {} stderr: {}", "[verbose]".dimmed(), stderr.trim());
        }

        if !status.success() {
            let details = if !stderr.trim().is_empty() {
                stderr.trim().to_string()
            } else if !stdout.trim().is_empty() {
                stdout.trim().to_string()
            } else {
                "no output (claude may need authentication — run `claude` interactively first)".to_string()
            };
            anyhow::bail!("claude CLI exited with {}: {}", status, details);
        }

        Ok((stdout, usage))
    }

    fn run_cli_with_images(&self, full_prompt: &str, images: &[ImageInput]) -> Result<(String, UsageStats)> {
        let mut content_parts = vec![serde_json::json!({
            "type": "text",
            "text": full_prompt
        })];

        for img in images {
            content_parts.push(serde_json::json!({
                "type": "image",
                "source": {
                    "type": "base64",
                    "media_type": img.media_type,
                    "data": img.base64_data()
                }
            }));
        }

        let message = serde_json::json!({
            "type": "human",
            "content": content_parts
        });

        let json_str = serde_json::to_string(&message)
            .context("failed to serialize image payload")?;

        let mut cmd = Command::new(&self.config.command);
        cmd.arg("-p")
            .arg("--input-format")
            .arg("stream-json")
            .arg("--output-format")
            .arg("stream-json")
            .arg("--verbose")
            .arg("--model")
            .arg(&self.config.model)
            .arg("--dangerously-skip-permissions") // Always skip permissions to allow auto-editing
            .current_dir(&self.working_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = cmd
            .spawn()
            .with_context(|| format!("failed to spawn '{}'", self.config.command))?;

        if let Some(ref mut stdin) = child.stdin {
            stdin.write_all(json_str.as_bytes()).context("failed to write to claude stdin")?;
        }
        drop(child.stdin.take());

        let (stdout, usage) = self.stream_cli_json(&mut child)?;
        let status = child.wait().context("failed to wait for claude")?;
        let stderr = self.collect_stderr(&mut child);

        if !status.success() {
            let details = if !stderr.trim().is_empty() {
                stderr.trim().to_string()
            } else if !stdout.trim().is_empty() {
                stdout.trim().to_string()
            } else {
                "no output".to_string()
            };
            anyhow::bail!("claude CLI exited with {}: {}", status, details);
        }

        Ok((stdout, usage))
    }

    fn describe_tool_action(tool: &str, input: Option<&serde_json::Value>) -> String {
        let get_str = |key: &str| -> Option<&str> {
            input.and_then(|v| v.get(key)).and_then(|v| v.as_str())
        };
        let truncate = |s: &str, max: usize| -> String {
            if s.len() <= max { s.to_string() } else { format!("{}…", &s[..max]) }
        };

        match tool {
            "Read" => {
                if let Some(path) = get_str("file_path") {
                    format!("reading {}", path)
                } else {
                    "reading file".to_string()
                }
            }
            "Write" | "Edit" => {
                if let Some(path) = get_str("file_path") {
                    format!("editing {}", path)
                } else {
                    "editing file".to_string()
                }
            }
            "Bash" => {
                if let Some(cmd) = get_str("command") {
                    format!("running `{}`", truncate(cmd.trim(), 60))
                } else {
                    "running command".to_string()
                }
            }
            "Grep" => {
                let pattern = get_str("pattern").unwrap_or("?");
                if let Some(path) = get_str("path") {
                    format!("searching '{}' in {}", truncate(pattern, 30), path)
                } else {
                    format!("searching '{}'", truncate(pattern, 40))
                }
            }
            "Glob" => {
                if let Some(pattern) = get_str("pattern") {
                    format!("finding files matching '{}'", truncate(pattern, 40))
                } else {
                    "finding files".to_string()
                }
            }
            "WebSearch" => {
                if let Some(query) = get_str("query").or_else(|| get_str("search_term")) {
                    format!("searching web: {}", truncate(query, 50))
                } else {
                    "searching the web".to_string()
                }
            }
            "WebFetch" => {
                if let Some(url) = get_str("url") {
                    format!("fetching {}", truncate(url, 60))
                } else {
                    "fetching URL".to_string()
                }
            }
            "Agent" | "Task" => {
                if let Some(desc) = get_str("prompt").or_else(|| get_str("description")) {
                    let first_line = desc.lines().next().unwrap_or(desc);
                    format!("subtask: {}", truncate(first_line, 60))
                } else {
                    "running subtask".to_string()
                }
            }
            _ => format!("using {}", tool),
        }
    }

    fn stream_cli_json(&self, child: &mut std::process::Child) -> Result<(String, UsageStats)> {
        let stdout_pipe = child.stdout.take().context("failed to capture claude stdout")?;
        let reader = BufReader::new(stdout_pipe);
        let mut full_result = String::new();
        let mut delta_text = String::new();
        let mut streaming_text = false;
        let separator = "─".repeat(60);
        let start = std::time::Instant::now();
        let mut cost_usd: Option<f64> = None;
        let mut model_name = self.config.model.clone();
        let mut input_tokens: u64 = 0;
        let mut output_tokens: u64 = 0;

        for line in reader.lines() {
            let line = match line {
                Ok(l) => l,
                Err(e) => {
                    eprintln!("  {} stream read error: {}", "✗".red(), e);
                    break;
                }
            };

            let trimmed = line.trim();
            if trimmed.is_empty() { continue; }

            let event: serde_json::Value = match serde_json::from_str(trimmed) {
                Ok(v) => v,
                Err(_) => continue,
            };

            let event_type = event.get("type").and_then(|v| v.as_str()).unwrap_or("");
            let subtype = event.get("subtype").and_then(|v| v.as_str()).unwrap_or("");

            match (event_type, subtype) {
                ("system", "init") => {
                    let model = event.get("model").and_then(|v| v.as_str()).unwrap_or("unknown");
                    model_name = model.to_string();
                    eprintln!("  {} connected (model: {})", "●".green(), model);
                    eprintln!("  {} thinking...", "◌".cyan());
                }
                ("system", "api_retry") => {
                    let attempt = event.get("attempt").and_then(|v| v.as_u64()).unwrap_or(0);
                    let max = event.get("max_retries").and_then(|v| v.as_u64()).unwrap_or(10);
                    let error = event.get("error").and_then(|v| v.as_str()).unwrap_or("unknown");
                    eprintln!("  {} API retry {}/{} — {} ({:.0}s elapsed)",
                        "↻".yellow(), attempt, max, error, start.elapsed().as_secs_f64());
                }
                ("assistant", "chunk") | ("content_block_delta", _) => {
                    if let Some(text) = event.pointer("/delta/text").and_then(|v| v.as_str()) {
                        if !streaming_text {
                            eprintln!("\n  {}", separator.dimmed());
                            eprintln!("  {}", "claude:".cyan().bold());
                            eprintln!("  {}", separator.dimmed());
                            streaming_text = true;
                        }
                        
                        // If it's the first chunk, we might want to indent it, but streaming markdown is hard.
                        // We'll just print it directly for now, but ensure we keep the text for the final result.
                        eprint!("{}", text);
                        let _ = std::io::stderr().lock().flush();
                        delta_text.push_str(text);
                    }
                }
                ("assistant", "thinking") => {
                    if streaming_text { eprintln!(); streaming_text = false; }
                    eprintln!("  {} reasoning...", "◌".cyan());
                }
                ("assistant", "tool_use") | ("tool_use", _) => {
                    let tool = event.get("tool").and_then(|v| v.as_str())
                        .or_else(|| event.pointer("/content_block/name").and_then(|v| v.as_str()))
                        .unwrap_or("tool");
                    let input = event.get("input")
                        .or_else(|| event.pointer("/content_block/input"));
                    if streaming_text { eprintln!(); streaming_text = false; }
                    let desc = Self::describe_tool_action(tool, input);
                    
                    // Only show clean summaries, suppress raw tool calls
                    if tool != "Bash" || !desc.starts_with("running `cat >") && !desc.starts_with("running `python -c") {
                        eprintln!("  {} {}", "⚡".cyan(), desc);
                    }
                }
                ("assistant", "tool_result") | ("tool_result", _) => {
                    let is_error = event.get("is_error").and_then(|v| v.as_bool()).unwrap_or(false);
                    if is_error {
                        eprintln!("  {} tool failed", "✗".red());
                    }
                }
                ("result", _) => {
                    if let Some(result) = event.get("result").and_then(|v| v.as_str()) {
                        full_result = result.to_string();
                    }
                    if let Some(cost) = event.get("cost_usd").and_then(|v| v.as_f64()) {
                        cost_usd = Some(cost);
                        let duration = event.get("duration_ms").and_then(|v| v.as_u64()).unwrap_or(0);
                        eprintln!("  {} done ({:.1}s, ${:.4})", "●".green(), duration as f64 / 1000.0, cost);
                    }
                    if let Some(it) = event.get("input_tokens").and_then(|v| v.as_u64()) {
                        input_tokens = it;
                    }
                    if let Some(ot) = event.get("output_tokens").and_then(|v| v.as_u64()) {
                        output_tokens = ot;
                    }
                    if let Some(m) = event.get("model").and_then(|v| v.as_str()) {
                        model_name = m.to_string();
                    }
                }
                ("system", "task_started") => {}
                ("system", "task_progress") => {}
                ("system", "task_notification") => {
                    if let Some(msg) = event.get("message").and_then(|v| v.as_str()) {
                        eprintln!("  {} {}", "ℹ".cyan(), msg);
                    }
                }
                ("assistant", _) => {
                    if let Some(blocks) = event.get("message")
                        .and_then(|m| m.get("content"))
                        .and_then(|c| c.as_array())
                    {
                        for block in blocks {
                            match block.get("type").and_then(|t| t.as_str()) {
                                Some("text") => {
                                    if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                                        if !text.is_empty() {
                                            if !streaming_text {
                                                eprintln!("\n  {}", separator.dimmed());
                                                eprintln!("  {}", "claude:".cyan().bold());
                                                eprintln!("  {}", separator.dimmed());
                                                streaming_text = true;
                                            }
                                            eprint!("{}", text);
                                            let _ = std::io::stderr().lock().flush();
                                            delta_text.push_str(text);
                                        }
                                    }
                                }
                                Some("tool_use") => {
                                    let tool = block.get("name").and_then(|n| n.as_str()).unwrap_or("tool");
                                    let input = block.get("input");
                                    if streaming_text { eprintln!(); streaming_text = false; }
                                    let desc = Self::describe_tool_action(tool, input);
                                    eprintln!("  {} {}", "⚡".cyan(), desc);
                                }
                                _ => {}
                            }
                        }
                    }
                }
                ("user", _) => {}
                _ => {
                    if self.verbose {
                        let elapsed = start.elapsed().as_secs_f64();
                        eprintln!("  {} {} {} ({:.0}s)", "·".dimmed(), event_type, subtype, elapsed);
                    }
                }
            }
        }

        if streaming_text { eprintln!(); }
        eprintln!("  {}", separator.dimmed());

        if cost_usd.is_none() && (input_tokens > 0 || output_tokens > 0) {
            cost_usd = pricing::compute_cost(&model_name, input_tokens, output_tokens);
        }

        let usage = UsageStats {
            input_tokens,
            output_tokens,
            cost_usd,
            model: model_name,
        };

        let text = if !full_result.is_empty() {
            full_result
        } else if !delta_text.is_empty() {
            delta_text
        } else {
            String::new()
        };

        Ok((text, usage))
    }

    fn collect_stderr(&self, child: &mut std::process::Child) -> String {
        child.stderr.take()
            .map(|pipe| {
                let reader = BufReader::new(pipe);
                reader.lines().filter_map(|l| l.ok()).collect::<Vec<_>>().join("\n")
            })
            .unwrap_or_default()
    }
}

impl ModelAdapter for ClaudeAdapter {
    fn generate(&self, prompt: &str, context: &str, images: &[ImageInput]) -> Result<(String, UsageStats)> {
        if self.use_api {
            self.run_api(prompt, context, images)
        } else {
            let result = self.run_cli(prompt, context, images);
            match result {
                Ok(r) => Ok(r),
                Err(e) if self.api_key.is_some() && self.config.mode == "auto" => {
                    eprintln!("  {} CLI failed ({}), falling back to API...", "↻".yellow(), e);
                    self.run_api(prompt, context, images)
                }
                Err(e) => Err(e),
            }
        }
    }

    fn name(&self) -> &str {
        "claude"
    }

    fn streams_output(&self) -> bool {
        true
    }
}

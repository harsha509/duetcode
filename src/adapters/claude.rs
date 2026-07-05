use super::{pricing, ImageInput, ModelAdapter, UsageStats};
use crate::config::ClaudeConfig;
use crate::ui;
use anyhow::{Context, Result};
use colored::Colorize;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

const ANTHROPIC_API_URL: &str = "https://api.anthropic.com/v1/messages";
const ANTHROPIC_VERSION: &str = "2023-06-01";
const MAX_OUTPUT_TOKENS: u32 = 8192;

pub struct ClaudeAdapter {
    config: ClaudeConfig,
    working_dir: PathBuf,
    verbose: bool,
    use_api: bool,
    api_key: Option<String>,
    agent: Option<ureq::Agent>,
    /// CLI session id captured from the first call; later calls `--resume` it
    /// so Claude keeps its full context (files read, edits made, reasoning).
    session_id: Option<String>,
    /// API-mode conversation history in Anthropic messages format.
    messages: Vec<serde_json::Value>,
}

struct CliOutput {
    text: String,
    usage: UsageStats,
    session_id: Option<String>,
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
            _ => !cli_available && has_api_key,
        };

        let agent = if use_api || mode == "auto" {
            Some(ureq::AgentBuilder::new()
                .timeout_connect(Duration::from_secs(30))
                .timeout_read(Duration::from_secs(config.timeout_secs))
                .build())
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
            agent,
            session_id: None,
            messages: Vec::new(),
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

    // ── API mode (direct Anthropic REST API with SSE streaming) ──

    fn run_api(&mut self, prompt: &str, images: &[ImageInput]) -> Result<(String, UsageStats)> {
        let api_key = self.api_key.clone().ok_or_else(|| anyhow::anyhow!(
            "{} not set — export it or add to your shell profile",
            self.config.api_key_env
        ))?;
        let agent = self.agent.clone()
            .ok_or_else(|| anyhow::anyhow!("HTTP client not initialized"))?;

        super::trim_history(&mut self.messages);
        self.messages.push(serde_json::json!({
            "role": "user",
            "content": build_content(prompt, images)
        }));

        let body = serde_json::json!({
            "model": self.config.api_model,
            "max_tokens": MAX_OUTPUT_TOKENS,
            "stream": true,
            "messages": self.messages,
        });

        if self.verbose {
            eprintln!(
                "  {} POST {} (model: {}, history: {} turns)",
                "[verbose]".dimmed(), ANTHROPIC_API_URL, self.config.api_model, self.messages.len()
            );
        }

        let result = agent
            .post(ANTHROPIC_API_URL)
            .set("x-api-key", &api_key)
            .set("anthropic-version", ANTHROPIC_VERSION)
            .set("content-type", "application/json")
            .send_json(&body)
            .map_err(map_api_error)
            .and_then(|response| self.parse_sse_stream(response.into_reader()));

        match result {
            Ok((text, usage)) => {
                self.messages.push(serde_json::json!({
                    "role": "assistant",
                    "content": [{ "type": "text", "text": text }]
                }));
                Ok((text, usage))
            }
            Err(e) => {
                self.messages.pop();
                Err(e)
            }
        }
    }

    fn parse_sse_stream(&self, body: impl Read) -> Result<(String, UsageStats)> {
        let reader = BufReader::new(body);
        let mut collected = String::new();
        let mut header_printed = false;
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
                            ui::stream_header("claude");
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
        ui::stream_footer();

        let cost_usd = pricing::compute_cost(&model_name, input_tokens, output_tokens);
        let usage = UsageStats {
            input_tokens,
            output_tokens,
            cost_usd,
            model: model_name,
        };

        Ok((collected, usage))
    }

    // ── CLI mode (spawn claude command, resume the session across calls) ──

    fn run_cli(&mut self, prompt: &str, images: &[ImageInput]) -> Result<(String, UsageStats)> {
        let resuming = self.session_id.is_some();
        match self.run_cli_once(prompt, images) {
            Err(e) if resuming => {
                eprintln!("  {} could not resume session ({:#}) — starting fresh", "↻".yellow(), e);
                self.session_id = None;
                self.run_cli_once(prompt, images)
            }
            other => other,
        }
    }

    fn run_cli_once(&mut self, prompt: &str, images: &[ImageInput]) -> Result<(String, UsageStats)> {
        let output = if images.is_empty() {
            self.spawn_cli_text(prompt)
        } else {
            self.spawn_cli_images(prompt, images)
        }?;

        let CliOutput { text, usage, session_id } = output;
        if session_id.is_some() {
            self.session_id = session_id;
        }
        Ok((text, usage))
    }

    fn base_cli_command(&self) -> Command {
        let mut cmd = Command::new(&self.config.command);
        cmd.arg("--model")
            .arg(&self.config.model)
            .arg("--output-format")
            .arg("stream-json")
            .arg("--verbose")
            .current_dir(&self.working_dir);

        if self.config.skip_permissions {
            cmd.arg("--dangerously-skip-permissions");
        }
        if let Some(id) = &self.session_id {
            cmd.arg("--resume").arg(id);
        }
        cmd
    }

    fn spawn_cli_text(&self, prompt: &str) -> Result<CliOutput> {
        let mut cmd = self.base_cli_command();
        cmd.arg("-p")
            .arg(prompt)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        if self.verbose {
            eprintln!(
                "  {} {} -p <prompt> --output-format stream-json{}",
                "[verbose]".dimmed(),
                self.config.command,
                if self.session_id.is_some() { " --resume <session>" } else { "" },
            );
        }

        let mut child = cmd
            .spawn()
            .with_context(|| format!("failed to execute '{}'", self.config.command))?;

        self.finish_cli(&mut child)
    }

    fn spawn_cli_images(&self, prompt: &str, images: &[ImageInput]) -> Result<CliOutput> {
        let mut content_parts = vec![serde_json::json!({
            "type": "text",
            "text": prompt
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

        let mut cmd = self.base_cli_command();
        cmd.arg("-p")
            .arg("--input-format")
            .arg("stream-json")
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

        self.finish_cli(&mut child)
    }

    fn finish_cli(&self, child: &mut Child) -> Result<CliOutput> {
        let output = self.stream_cli_json(child)?;
        let status = child.wait().context("failed to wait for claude")?;
        let stderr = collect_stderr(child);

        if self.verbose && !stderr.is_empty() {
            eprintln!("  {} stderr: {}", "[verbose]".dimmed(), stderr.trim());
        }

        if !status.success() {
            let details = if !stderr.trim().is_empty() {
                stderr.trim().to_string()
            } else if !output.text.trim().is_empty() {
                output.text.trim().to_string()
            } else {
                "no output (claude may need authentication — run `claude` interactively first)".to_string()
            };
            anyhow::bail!("claude CLI exited with {}: {}", status, details);
        }

        Ok(output)
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

    fn stream_cli_json(&self, child: &mut Child) -> Result<CliOutput> {
        let stdout_pipe = child.stdout.take().context("failed to capture claude stdout")?;
        let reader = BufReader::new(stdout_pipe);
        let mut full_result = String::new();
        let mut delta_text = String::new();
        let mut streaming_text = false;
        let start = std::time::Instant::now();
        let mut cost_usd: Option<f64> = None;
        let mut model_name = self.config.model.clone();
        let mut input_tokens: u64 = 0;
        let mut output_tokens: u64 = 0;
        let mut session_id: Option<String> = None;

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
                    if let Some(id) = event.get("session_id").and_then(|v| v.as_str()) {
                        session_id = Some(id.to_string());
                    }
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
                            ui::stream_header("claude");
                            streaming_text = true;
                        }
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
                ("system", "task_started") | ("system", "task_progress") => {}
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
                                                ui::stream_header("claude");
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
        ui::stream_footer();

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
        } else {
            delta_text
        };

        Ok(CliOutput { text, usage, session_id })
    }
}

fn build_content(prompt: &str, images: &[ImageInput]) -> serde_json::Value {
    if images.is_empty() {
        return serde_json::json!([{ "type": "text", "text": prompt }]);
    }
    let mut parts = vec![serde_json::json!({ "type": "text", "text": prompt })];
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
}

fn map_api_error(e: ureq::Error) -> anyhow::Error {
    match e {
        ureq::Error::Status(code, response) => {
            let error_body = response.into_string().unwrap_or_default();
            let error_msg = serde_json::from_str::<serde_json::Value>(&error_body)
                .ok()
                .and_then(|v| v.pointer("/error/message").and_then(|m| m.as_str()).map(String::from))
                .unwrap_or(error_body);
            anyhow::anyhow!("Anthropic API returned {}: {}", code, error_msg)
        }
        ureq::Error::Transport(t) => {
            anyhow::anyhow!(
                "failed to reach Anthropic API: {} — \
                 try increasing timeout_secs in .duet/config.toml [claude]",
                t
            )
        }
    }
}

fn collect_stderr(child: &mut Child) -> String {
    child.stderr.take()
        .map(|pipe| {
            BufReader::new(pipe)
                .lines()
                .map_while(Result::ok)
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

impl ModelAdapter for ClaudeAdapter {
    fn generate(&mut self, prompt: &str, images: &[ImageInput]) -> Result<(String, UsageStats)> {
        if self.use_api {
            return self.run_api(prompt, images);
        }
        match self.run_cli(prompt, images) {
            Err(e) if self.api_key.is_some() && self.config.mode == "auto" => {
                eprintln!("  {} CLI failed ({:#}) — falling back to API", "↻".yellow(), e);
                self.run_api(prompt, images)
            }
            other => other,
        }
    }

    fn name(&self) -> &str {
        "claude"
    }

    fn streams_output(&self) -> bool {
        true
    }
}

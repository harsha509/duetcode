use super::{pricing, readable_extras, ImageInput, ModelAdapter, UsageStats};
use crate::config::ClaudeConfig;
use crate::events::{Event, Sink};
use crate::ui;
use anyhow::{Context, Result};
use colored::Colorize;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

/// Whether a failed run means the stored session is unusable and the call
/// should be retried from a fresh one.
///
/// Only a resume the CLI actually rejected qualifies, and that shows up as a
/// run that never announced a session id. Every other failure — a quota limit,
/// a dropped connection — happened *inside* a session that is still on disk and
/// still resumable: retrying burns a second call to fail the same way, and
/// dropping the id costs the task everything it had already done.
fn should_restart_session(was_resuming: bool, connected: bool) -> bool {
    was_resuming && !connected
}

const ANTHROPIC_API_URL: &str = "https://api.anthropic.com/v1/messages";
const ANTHROPIC_VERSION: &str = "2023-06-01";
const MAX_OUTPUT_TOKENS: u32 = 8192;

/// Budgets for the short probes run at startup and in `dt doctor`.
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);
const AUTH_TIMEOUT: Duration = Duration::from_secs(30);

pub struct ClaudeAdapter {
    config: ClaudeConfig,
    working_dir: PathBuf,
    verbose: bool,
    use_api: bool,
    api_key: Option<String>,
    agent: Option<ureq::Agent>,
    /// CLI session ids keyed by the checkout they belong to; later calls in the
    /// same project `--resume` theirs, so Claude keeps its full context (files
    /// read, edits made, reasoning).
    ///
    /// Keyed rather than a single id because the CLI stores sessions per
    /// working directory: resuming project A's id from project B looks it up in
    /// the wrong place, and inherits a conversation about the wrong checkout.
    /// A map also lets A → B → A pick A's own session back up.
    sessions: HashMap<PathBuf, String>,
    /// Sibling projects the CLI may read besides `working_dir`.
    readable_dirs: Vec<PathBuf>,
    /// API-mode conversation history in Anthropic messages format.
    messages: Vec<serde_json::Value>,
    /// Files opened during the most recent `generate`, so a review's account of
    /// what it read can be checked rather than believed.
    files_opened: Vec<String>,
    /// True when this model holds the reviewer seat: the spawn then runs
    /// read-only, the same way the gemini reviewer always does.
    read_only: bool,
    /// Set by `run_cli` for a timeout or a user stop: `generate` must not
    /// fall back to the API on top of it.
    not_retryable: bool,
    sink: Arc<dyn Sink>,
}

struct CliOutput {
    text: String,
    usage: UsageStats,
    session_id: Option<String>,
    /// Files the CLI actually opened this turn, from its own tool events.
    files_opened: Vec<String>,
    /// The CLI's own error text from a failed result frame. Kept apart from
    /// `text` so failure classification never reads the model's prose.
    error_detail: Option<String>,
}

/// The files a tool call actually opened, or nothing for a call that opened
/// none. Only `Read` names a file it read; Grep and Glob search the tree
/// without reading any one file, and Write and Edit are not reading at all.
///
/// `file_path` is the key the CLI uses, and the only one seen in practice. The
/// alternatives are read too, because of how this fails if the key ever moves:
/// no path is captured, the turn looks like it opened nothing, and a review
/// that honestly read six files has all six reported as unsupported. Accusing
/// truthful reviews is a worse failure than reading a key that is never set.
fn opened_paths(tool: &str, input: Option<&serde_json::Value>) -> Vec<String> {
    if tool != "Read" {
        return Vec::new();
    }
    let Some(input) = input else {
        return Vec::new();
    };
    ["file_path", "path", "absolute_path"]
        .iter()
        .filter_map(|key| input.get(*key).and_then(|v| v.as_str()))
        .map(str::to_string)
        .collect()
}

/// One CLI invocation, successful or not.
///
/// The session id is reported either way. The CLI announces it in `system/init`
/// before doing any work, so a run that dies later — on a quota limit, say —
/// still leaves a session on disk holding everything it did. Discarding the id
/// with the error is what makes the writer forget a task it had already begun.
struct CliAttempt {
    session_id: Option<String>,
    /// True when the CLI's own stderr said the session outgrew its context.
    overflowed: bool,
    /// True when the watchdog killed this run at its deadline. A retry or
    /// fallback would burn another full budget on the same wedge.
    timed_out: bool,
    result: Result<CliOutput>,
}

impl CliAttempt {
    /// A run that never got far enough to have a session.
    fn failed(error: anyhow::Error) -> Self {
        Self { session_id: None, overflowed: false, timed_out: false, result: Err(error) }
    }

    /// True when the CLI got as far as announcing its session, which means a
    /// `--resume` was accepted.
    fn connected(&self) -> bool {
        self.session_id.is_some()
    }
}

impl ClaudeAdapter {
    pub fn new(config: &ClaudeConfig, working_dir: &Path, verbose: bool, sink: Arc<dyn Sink>) -> Self {
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
            sessions: HashMap::new(),
            readable_dirs: Vec::new(),
            messages: Vec::new(),
            files_opened: Vec::new(),
            read_only: false,
            not_retryable: false,
            sink,
        }
    }

    /// The stored session for the checkout the adapter currently points at.
    fn current_session(&self) -> Option<&String> {
        self.sessions.get(&self.working_dir)
    }

    fn emit_chunk(&self, started: &mut bool, text: &str) {
        if !*started {
            self.sink.event(Event::StreamStart { model: "claude".into() });
            *started = true;
        }
        self.sink.event(Event::StreamChunk { model: "claude".into(), text: text.to_string() });
    }

    fn is_real_api_key(key: &str) -> bool {
        let k = key.trim();
        !k.is_empty() && k != "sk-ant-xxx" && k.starts_with("sk-ant-")
    }

    fn check_cli_available(command: &str) -> bool {
        // Bounded so a wedged CLI cannot hang startup or `dt doctor` forever.
        crate::process::capture_with_timeout(Command::new("which").arg(command), PROBE_TIMEOUT)
            .map(|o| o.is_some_and(|o| o.status.success()))
            .unwrap_or(false)
    }

    pub fn is_available(&self) -> bool {
        Self::check_cli_available(&self.config.command)
    }

    pub fn is_api_key_available(&self) -> bool {
        self.api_key.is_some()
    }

    pub fn check_auth(&self) -> Result<String> {
        let mut cmd = Command::new(&self.config.command);
        cmd.args(["auth", "status", "--text"]);
        let output = crate::process::capture_with_timeout(&mut cmd, AUTH_TIMEOUT)
            .with_context(|| format!("failed to run '{} auth status'", self.config.command))?
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "'{} auth status' did not finish within {}s",
                    self.config.command,
                    AUTH_TIMEOUT.as_secs()
                )
            })?;

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
            if super::stop_requested() {
                // A stop ends the stream here: a partial answer must never be
                // mistaken for a complete one.
                self.sink.event(Event::StreamEnd { model: "claude".into() });
                ui::stream_footer();
                return Err(super::interrupted_error());
            }
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
                        self.emit_chunk(&mut header_printed, text);
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
                    self.sink.event(Event::Thinking { model: "claude".into() });
                }
                "content_block_start" => {
                    let block_type = event.pointer("/content_block/type").and_then(|v| v.as_str()).unwrap_or("");
                    if block_type == "thinking" {
                        self.sink.event(Event::Thinking { model: "claude".into() });
                    }
                }
                "message_delta" => {
                    if let Some(ot) = event.pointer("/usage/output_tokens").and_then(|v| v.as_u64()) {
                        output_tokens = ot;
                    }
                    if let Some(reason) = event.pointer("/delta/stop_reason").and_then(|v| v.as_str()) {
                        let elapsed = start.elapsed().as_secs_f64();
                        self.sink.event(Event::StreamEnd { model: "claude".into() });
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

        self.sink.event(Event::StreamEnd { model: "claude".into() });
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
        // Cleared per turn: what matters is what this answer went and read, not
        // what some earlier answer did.
        self.files_opened.clear();

        let was_resuming = self.current_session().is_some();

        let attempt = self.spawn_cli(prompt, images);
        let connected = attempt.connected();
        let overflowed = attempt.overflowed;
        // A timeout or a user stop killed this run on purpose; no retry or
        // API fallback may re-send the prompt behind the caller's back.
        self.not_retryable = attempt.timed_out || super::stop_requested();
        self.record_session(&attempt);

        let error = match attempt.result {
            Ok(CliOutput { text, usage, files_opened, .. }) => {
                self.files_opened = files_opened;
                return Ok((text, usage));
            }
            Err(error) => error,
        };
        if self.not_retryable {
            return Err(error);
        }

        // A rejected resume is one failure a fresh session fixes; an overflowed
        // one is another — the session is still on disk, but too big to continue.
        let restart = should_restart_session(was_resuming, connected) || (was_resuming && overflowed);
        if !restart {
            return Err(error);
        }

        if connected {
            eprintln!(
                "  {} the session outgrew its context ({:#}) — starting fresh",
                "↻".yellow(),
                error
            );
        } else {
            eprintln!("  {} could not resume session ({:#}) — starting fresh", "↻".yellow(), error);
        }
        self.sessions.remove(&self.working_dir);

        let retry = self.spawn_cli(prompt, images);
        self.not_retryable = retry.timed_out || super::stop_requested();
        self.record_session(&retry);
        let CliOutput { text, usage, files_opened, .. } = retry.result?;
        self.files_opened = files_opened;
        Ok((text, usage))
    }

    fn spawn_cli(&self, prompt: &str, images: &[ImageInput]) -> CliAttempt {
        // The prompt travels on stdin as a stream-json message, never in argv:
        // argv is visible via `ps`, and endpoint-security agents kill a node
        // process once one argument runs past about a kilobyte.
        let payload = match stream_json_payload(prompt, images) {
            Ok(payload) => payload,
            Err(e) => return CliAttempt::failed(e),
        };

        let mut cmd = self.base_cli_command();
        cmd.arg("-p")
            .arg("--input-format")
            .arg("stream-json")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        if self.verbose {
            eprintln!(
                "  {} {} -p --input-format stream-json --output-format stream-json{} <prompt on stdin>",
                "[verbose]".dimmed(),
                self.config.command,
                if self.current_session().is_some() { " --resume <session>" } else { "" },
            );
        }

        let mut child = match crate::process::spawn_grouped(&mut cmd)
            .with_context(|| format!("failed to execute '{}'", self.config.command))
        {
            Ok(child) => child,
            Err(e) => return CliAttempt::failed(e),
        };

        let writer = super::feed_stdin(&mut child, &payload);
        let mut attempt = self.finish_cli(&mut child);
        let delivered = super::prompt_delivered(writer);

        // A failed write is worth reporting only when the run otherwise looks
        // fine; if the CLI died, the pipe broke because of that.
        if attempt.result.is_ok() {
            if let Err(e) = delivered {
                attempt.result = Err(e);
                // A session whose prompt only partly arrived is poisoned:
                // dropping the id keeps it out of the store, and makes the
                // restart path discard the resumed session it went into.
                attempt.session_id = None;
            }
        }
        attempt
    }

    /// Remembers the session this run belongs to, so the next task in the same
    /// project resumes it — whether or not this run succeeded.
    fn record_session(&mut self, attempt: &CliAttempt) {
        if let Some(id) = &attempt.session_id {
            self.sessions.insert(self.working_dir.clone(), id.clone());
        }
    }

    fn base_cli_command(&self) -> Command {
        let mut cmd = Command::new(&self.config.command);
        cmd.arg("--model")
            .arg(&self.config.model)
            .arg("--output-format")
            .arg("stream-json")
            .arg("--verbose")
            .current_dir(&self.working_dir);

        if self.read_only {
            // The reviewer judges the diff as it stands: write access would
            // let it "fix" what it is judging, or be talked into running
            // things by code riding in the review subject.
            cmd.arg("--permission-mode").arg("plan");
        } else if self.config.skip_permissions {
            cmd.arg("--dangerously-skip-permissions");
        }
        // Variadic in the CLI, so every sibling project goes on one flag. Placed
        // before the caller's `-p`, which ends the list.
        let extra = readable_extras(&self.readable_dirs, &self.working_dir);
        if !extra.is_empty() {
            cmd.arg("--add-dir");
            for dir in extra {
                cmd.arg(dir);
            }
        }
        if let Some(id) = self.current_session() {
            cmd.arg("--resume").arg(id);
        }
        cmd
    }

    fn finish_cli(&self, child: &mut Child) -> CliAttempt {
        // Drained on its own thread, started before stdout is read: a child
        // that fills its stderr pipe blocks writing it, stops producing
        // stdout, and the two processes wait on each other forever.
        let stderr_drain = super::drain_stderr(child);

        let timeout = Duration::from_secs(self.config.cli_timeout_secs);
        // Armed before stdout is read: a wedged CLI never closes the pipe, and
        // only a kill at the deadline unblocks the reader below.
        let watchdog = crate::process::Watchdog::arm(child, timeout);
        let streamed = self.stream_cli_json(child);
        let timed_out = watchdog.disarm();
        let reap_grace = if timeout.is_zero() { timeout } else { crate::process::REAP_GRACE };

        let output = match streamed {
            Ok(output) => output,
            Err(e) => {
                // The child may still be alive on this path: reap or kill it,
                // or its group slot leaks until dt exits.
                let _ = crate::process::wait_or_kill(child, reap_grace);
                if let Some(handle) = stderr_drain {
                    let _ = handle.join();
                }
                let e =
                    if super::stop_requested() { super::interrupted_error() } else { e };
                return CliAttempt::failed(e);
            }
        };
        // Read off the parsed output, so a failure below still reports it.
        let session_id = output.session_id.clone();

        // stdout is closed by now, so the run's budget is already spent
        // streaming; the exit wait gets a short grace, not a second budget.
        let status =
            match crate::process::wait_or_kill(child, reap_grace).context("failed to wait for claude")
            {
                Ok(Some(status)) => status,
                Ok(None) => {
                    return CliAttempt {
                        session_id,
                        overflowed: false,
                        timed_out: false,
                        result: Err(anyhow::anyhow!(
                            "claude CLI did not exit after finishing its output — killed after {}s",
                            crate::process::REAP_GRACE.as_secs()
                        )),
                    };
                }
                Err(e) => {
                    return CliAttempt { session_id, overflowed: false, timed_out: false, result: Err(e) }
                }
            };
        let stderr = stderr_drain.map(|h| h.join().unwrap_or_default()).unwrap_or_default();

        if self.verbose && !stderr.is_empty() {
            eprintln!("  {} stderr: {}", "[verbose]".dimmed(), stderr.trim());
        }

        // A user stop outranks every other classification of the corpse.
        if super::stop_requested() {
            return CliAttempt {
                session_id,
                overflowed: false,
                timed_out,
                result: Err(super::interrupted_error()),
            };
        }

        // Honored only for a failed run: a child can finish cleanly in the
        // same instant the deadline passes, and that answer is not discarded.
        if timed_out && !status.success() {
            return CliAttempt {
                session_id,
                overflowed: false,
                timed_out: true,
                result: Err(anyhow::anyhow!(
                    "claude CLI timed out after {}s — raise cli_timeout_secs in \
                     .duet/config.toml [claude] to allow longer runs",
                    timeout.as_secs()
                )),
            };
        }

        if !status.success() {
            let details = if !stderr.trim().is_empty() {
                stderr.trim().to_string()
            } else if !output.text.trim().is_empty() {
                output.text.trim().to_string()
            } else {
                "no output (claude may need authentication — run `claude` interactively first)".to_string()
            };
            // Classified from the CLI's own diagnostics — stderr or its error
            // result frame — never from the model's prose.
            let overflowed = super::is_context_overflow(&stderr)
                || output.error_detail.as_deref().is_some_and(super::is_context_overflow);
            return CliAttempt {
                session_id,
                overflowed,
                timed_out: false,
                result: Err(anyhow::anyhow!("claude CLI exited with {}: {}", status, details)),
            };
        }

        CliAttempt { session_id, overflowed: false, timed_out: false, result: Ok(output) }
    }

    fn describe_tool_action(tool: &str, input: Option<&serde_json::Value>) -> String {
        let get_str = |key: &str| -> Option<&str> {
            input.and_then(|v| v.get(key)).and_then(|v| v.as_str())
        };
        let truncate = truncate_chars;

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
        let mut files_opened: Vec<String> = Vec::new();
        let mut started = false;
        let start = std::time::Instant::now();
        let mut cost_usd: Option<f64> = None;
        let mut model_name = self.config.model.clone();
        let mut input_tokens: u64 = 0;
        let mut output_tokens: u64 = 0;
        let mut session_id: Option<String> = None;
        let mut error_detail: Option<String> = None;

        for line in reader.lines() {
            if super::stop_requested() {
                // A stop ends the stream here: a partial answer must never be
                // mistaken for a complete one.
                return Err(super::interrupted_error());
            }
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
                    self.sink.event(Event::Thinking { model: "claude".into() });
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
                        self.emit_chunk(&mut started, text);
                        delta_text.push_str(text);
                    }
                }
                ("assistant", "thinking") => {
                    self.sink.event(Event::Thinking { model: "claude".into() });
                }
                ("assistant", "tool_use") | ("tool_use", _) => {
                    let tool = event.get("tool").and_then(|v| v.as_str())
                        .or_else(|| event.pointer("/content_block/name").and_then(|v| v.as_str()))
                        .unwrap_or("tool");
                    let input = event.get("input")
                        .or_else(|| event.pointer("/content_block/input"));
                    let desc = Self::describe_tool_action(tool, input);
                    files_opened.extend(opened_paths(tool, input));

                    if tool != "Bash" || !desc.starts_with("running `cat >") && !desc.starts_with("running `python -c") {
                        self.sink.event(Event::ToolAction { model: "claude".into(), desc });
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
                    // A failed run reports its reason here, on stdout — often
                    // with stderr empty — so it is kept for classification.
                    if event.get("is_error").and_then(|v| v.as_bool()).unwrap_or(false)
                        || subtype.starts_with("error")
                    {
                        error_detail = event
                            .get("result")
                            .or_else(|| event.get("error"))
                            .and_then(|v| v.as_str())
                            .map(str::to_string);
                    }
                    // Older CLI: cost_usd. Newer CLI: total_cost_usd.
                    let cost = event.get("cost_usd").and_then(|v| v.as_f64())
                        .or_else(|| event.get("total_cost_usd").and_then(|v| v.as_f64()));
                    if let Some(cost) = cost {
                        cost_usd = Some(cost);
                        let duration = event.get("duration_ms").and_then(|v| v.as_u64()).unwrap_or(0);
                        eprintln!("  {} done ({:.1}s, ${:.4})", "●".green(), duration as f64 / 1000.0, cost);
                    }
                    // Older CLI: top-level token counts. Newer CLI: nested
                    // under `usage`, with cache tokens split out.
                    if let Some(it) = event.get("input_tokens").and_then(|v| v.as_u64()) {
                        input_tokens = it;
                    }
                    if let Some(ot) = event.get("output_tokens").and_then(|v| v.as_u64()) {
                        output_tokens = ot;
                    }
                    if let Some(usage) = event.get("usage") {
                        let count = |key: &str| usage.get(key).and_then(|v| v.as_u64()).unwrap_or(0);
                        let total_in = count("input_tokens")
                            + count("cache_read_input_tokens")
                            + count("cache_creation_input_tokens");
                        if total_in > 0 {
                            input_tokens = total_in;
                        }
                        if let Some(ot) = usage.get("output_tokens").and_then(|v| v.as_u64()) {
                            output_tokens = ot;
                        }
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
                                            self.emit_chunk(&mut started, text);
                                            delta_text.push_str(text);
                                        }
                                    }
                                }
                                Some("tool_use") => {
                                    let tool = block.get("name").and_then(|n| n.as_str()).unwrap_or("tool");
                                    let input = block.get("input");
                                    let desc = Self::describe_tool_action(tool, input);
                                    files_opened.extend(opened_paths(tool, input));
                                    self.sink.event(Event::ToolAction { model: "claude".into(), desc });
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

        self.sink.event(Event::StreamEnd { model: "claude".into() });
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

        Ok(CliOutput { text, usage, session_id, files_opened, error_detail })
    }
}

/// Truncates to `max` *characters*, never bytes. Tool arguments are arbitrary
/// user text — a non-ASCII path, a CJK search pattern — and slicing one at a
/// byte index that lands mid-character panics. `panic = "abort"` in the release
/// profile turns that into a killed `dt` process, losing the session.
fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(max).collect::<String>())
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

/// The turn as one frame for `--input-format stream-json`. The CLI's contract
/// is a `user` frame wrapping a messages-API message — text first, images after.
fn stream_json_payload(prompt: &str, images: &[ImageInput]) -> Result<String> {
    let message = serde_json::json!({
        "type": "user",
        "message": {
            "role": "user",
            "content": build_content(prompt, images)
        }
    });
    serde_json::to_string(&message).context("failed to serialize prompt payload")
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

impl ModelAdapter for ClaudeAdapter {
    fn generate(&mut self, prompt: &str, images: &[ImageInput]) -> Result<(String, UsageStats)> {
        if self.use_api {
            return self.run_api(prompt, images);
        }
        match self.run_cli(prompt, images) {
            // Case-insensitive to match the constructor, which lowercases
            // `mode` before deciding whether to build the API agent. Never on
            // a timeout or a user stop: both killed the run deliberately.
            Err(e) if !self.not_retryable
                && self.api_key.is_some()
                && self.config.mode.eq_ignore_ascii_case("auto") =>
            {
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

    /// Sessions are keyed by checkout, so moving here simply looks up this
    /// project's own session — project A's is neither resumed nor lost, and
    /// coming back to A resumes exactly where A left off.
    fn set_working_dir(&mut self, dir: &Path) {
        self.working_dir = dir.to_path_buf();
    }

    fn set_readable_dirs(&mut self, dirs: &[PathBuf]) {
        self.readable_dirs = dirs.to_vec();
    }

    /// Only the CLI carries tools; the API path is a bare messages call.
    fn can_read_files(&self) -> bool {
        !self.use_api
    }

    fn files_opened_last_turn(&self) -> Vec<String> {
        self.files_opened.clone()
    }

    fn set_read_only(&mut self, read_only: bool) {
        self.read_only = read_only;
    }

    /// Drops this checkout's session and the API history behind it, so the next
    /// call starts from the prompt alone.
    fn reset_session(&mut self) {
        self.sessions.remove(&self.working_dir);
        self.messages.clear();
        self.files_opened.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What the audit is allowed to treat as "this review opened that file".
    /// Only a read counts: a Grep or Glob searches the tree without reading any
    /// one file, and an Edit is not reading at all. Counting either would let a
    /// claim be validated by a call that never opened what it names.
    #[test]
    fn only_a_read_records_the_file_it_opened() {
        let input = serde_json::json!({ "file_path": "src/policy.rs" });
        assert_eq!(opened_paths("Read", Some(&input)), vec!["src/policy.rs".to_string()]);

        assert!(opened_paths("Edit", Some(&input)).is_empty());
        assert!(opened_paths("Write", Some(&input)).is_empty());
        assert!(opened_paths("Grep", Some(&serde_json::json!({ "path": "src" }))).is_empty());
        assert!(opened_paths("Read", None).is_empty());
    }

    /// If the CLI ever moves the key, the failure is silent and inverted: no
    /// path is captured, so an honest review's every read is reported as
    /// unsupported. Reading the alternatives costs nothing and stops that.
    #[test]
    fn a_read_is_recorded_whichever_key_names_the_path() {
        for key in ["file_path", "path", "absolute_path"] {
            let input = serde_json::json!({ key: "/repo/src/policy.rs" });
            assert_eq!(
                opened_paths("Read", Some(&input)),
                vec!["/repo/src/policy.rs".to_string()],
                "key {} was not read",
                key
            );
        }
    }

    /// The case that made the writer forget a half-finished research task: the
    /// CLI connected, worked, then died on an account quota limit. The session
    /// holds everything it did, so it must survive to be resumed.
    #[test]
    fn a_failure_after_connecting_keeps_the_session() {
        assert!(!should_restart_session(true, true));
    }

    /// The only failure a fresh start actually fixes.
    #[test]
    fn a_resume_the_cli_never_accepted_starts_fresh() {
        assert!(should_restart_session(true, false));
    }

    /// Nothing to fall back to, so a first-call failure is just a failure —
    /// retrying it would only spend a second call.
    #[test]
    fn a_first_call_is_never_retried() {
        assert!(!should_restart_session(false, false));
        assert!(!should_restart_session(false, true));
    }

    /// The CLI already owns its working directory; granting it again would be
    /// noise, and on some paths an outright error.
    #[test]
    fn the_working_directory_is_not_granted_twice() {
        let working = std::env::temp_dir();
        assert!(readable_extras(std::slice::from_ref(&working), &working).is_empty());
    }

    /// A folder removed from disk since the workspace was declared must not
    /// make every later spawn fail.
    #[test]
    fn a_project_that_no_longer_exists_is_dropped() {
        let present = std::env::temp_dir();
        let readable = vec![PathBuf::from("/dt-no-such-workspace-project"), present.clone()];
        assert_eq!(readable_extras(&readable, Path::new("/elsewhere")), vec![&present]);
    }

    #[test]
    fn short_ascii_is_returned_unchanged() {
        assert_eq!(truncate_chars("cargo test", 60), "cargo test");
    }

    #[test]
    fn long_ascii_is_cut_and_marked() {
        assert_eq!(truncate_chars("abcdefghij", 4), "abcd…");
    }

    /// The panic this replaces: `&s[..max]` where `max` lands inside a
    /// multibyte character aborts the process.
    #[test]
    fn multibyte_boundaries_do_not_panic() {
        assert_eq!(truncate_chars("検索パターンをここに入れる長い文字列", 4), "検索パタ…");
        assert_eq!(truncate_chars("grep 'résumé' in café", 7), "grep 'r…");
    }

    /// A CJK pattern is 3 bytes per character, so a byte-based limit of 30 cut
    /// after 10 characters; a character-based one must keep all 30.
    #[test]
    fn character_limit_is_not_a_byte_limit() {
        let cjk = "検".repeat(30);
        assert_eq!(truncate_chars(&cjk, 30), cjk);
        assert!(!truncate_chars(&cjk, 30).ends_with('…'));
    }

    #[test]
    fn zero_max_yields_only_the_ellipsis() {
        assert_eq!(truncate_chars("abc", 0), "…");
    }

    /// The CLI's stream-json input contract: a `user` frame wrapping a
    /// messages-API message. A `human` frame is not in the contract, and a
    /// prompt the CLI drops reads as a run that answered nothing.
    #[test]
    fn the_prompt_is_a_user_frame_wrapping_a_message() {
        let payload = stream_json_payload("hello", &[]).expect("payload");
        let frame: serde_json::Value = serde_json::from_str(&payload).expect("valid json");
        assert_eq!(frame["type"], "user");
        assert_eq!(frame["message"]["role"], "user");
        assert_eq!(frame["message"]["content"][0]["text"], "hello");
    }
}

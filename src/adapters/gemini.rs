use super::{pricing, readable_extras, ImageInput, ModelAdapter, UsageStats};
use crate::config::GeminiConfig;
use crate::events::{Event, Sink};
use crate::ui;
use anyhow::{Context, Result};
use colored::Colorize;
use std::collections::{HashMap, HashSet};
use std::hash::{BuildHasher, RandomState};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

const GEMINI_API_BASE: &str = "https://generativelanguage.googleapis.com/v1beta/models";

/// Read-only tool access: the CLI may open and search the checkout but cannot
/// edit it. A reviewer that could write would be free to "fix" what it is
/// judging, and the diff it reported on would no longer be the one it read.
const APPROVAL_MODE: &str = "plan";

pub struct GeminiAdapter {
    config: GeminiConfig,
    /// True when calls go through the `gemini` CLI, which carries file tools.
    /// False for the bare HTTPS transport, which has none.
    use_cli: bool,
    verbose: bool,
    api_key: Option<String>,
    agent: ureq::Agent,
    /// Conversation history in Gemini `contents` format, so each review
    /// remembers what it said in earlier rounds. API transport only — in CLI
    /// mode the CLI keeps the conversation itself and `--resume` recalls it.
    history: Vec<serde_json::Value>,
    working_dir: PathBuf,
    /// Sibling projects the CLI may read besides `working_dir`.
    readable_dirs: Vec<PathBuf>,
    /// CLI session ids keyed by the checkout they belong to, for the same
    /// reason the Claude adapter keys them: resuming project A's session from
    /// project B inherits a conversation about the wrong checkout.
    sessions: HashMap<PathBuf, String>,
    /// Files opened during the most recent `generate`, so a review's account of
    /// what it read can be checked rather than believed.
    files_opened: Vec<String>,
    /// Directories already sized up for the CLI's file crawler, so each is
    /// warned about at most once however many rounds run inside it.
    crawl_checked: HashSet<PathBuf>,
    /// The subset of those whose trees are too large to crawl. Any run whose
    /// workspace touches one gets the crawl guard from the first spawn instead
    /// of an OOM death and a retry.
    crawl_hazards: HashSet<PathBuf>,
    sink: Arc<dyn Sink>,
}

struct CliOutput {
    text: String,
    usage: UsageStats,
    session_id: Option<String>,
    /// Files the CLI actually opened this turn, from its own tool events.
    files_opened: Vec<String>,
    /// The CLI's last typed `error` event. Kept apart from `text` so failure
    /// classification never reads the model's prose.
    error_detail: Option<String>,
}

/// One CLI invocation, successful or not. The session id is reported either
/// way: the CLI announces it in `init` before doing any work, so a run that
/// dies later still leaves a resumable session behind.
struct CliAttempt {
    session_id: Option<String>,
    /// True when the CLI's own stderr said the session outgrew its context.
    overflowed: bool,
    result: Result<CliOutput>,
}

impl CliAttempt {
    fn failed(error: anyhow::Error) -> Self {
        Self { session_id: None, overflowed: false, result: Err(error) }
    }

    /// True when the CLI got as far as announcing its session, which means a
    /// `--resume` was accepted.
    fn connected(&self) -> bool {
        self.session_id.is_some()
    }
}

/// Whether a failed run means the stored session is unusable and the call
/// should be retried from a fresh one. Only a resume the CLI actually rejected
/// qualifies, and that shows up as a run that never announced a session id.
fn should_restart_session(was_resuming: bool, connected: bool) -> bool {
    was_resuming && !connected
}

/// Tools whose implementation enumerates the whole workspace in memory before
/// filtering, which is what dies on a checkout carrying a large dependency
/// tree. Disabled only for the retry after an out-of-memory death: a reviewer
/// without them still opens files and greps through ripgrep, where the API
/// fallback it would otherwise become reads nothing at all.
const CRAWLER_TOOLS: &[&str] = &["glob", "read_many_files"];

/// Appended to the prompt of every crawl-guarded run. The CLI drops excluded
/// tools from the toolkit without telling the model why, and a model that
/// finds its bulk tools missing has been observed to stop reading altogether —
/// a 51k-token review answered with zero tool calls — rather than fall back to
/// the tools still present. What to read stays the model's decision; this only
/// says that reading still works.
const CRAWL_GUARD_PROMPT_NOTE: &str = "\n\n[tooling note: this workspace is too large for the \
    bulk file tools, so glob and read_many_files are disabled for this run. read_file and \
    ripgrep search still work — open the specific files your answer depends on, and grep for \
    what you can no longer glob, rather than answering from the prompt alone.]";

/// `prompt` with the crawl-guard note attached, for any run spawned with the
/// crawler tools excluded.
fn with_crawl_guard_note(prompt: &str) -> String {
    format!("{}{}", prompt, CRAWL_GUARD_PROMPT_NOTE)
}

/// Whether a failed CLI run died of memory exhaustion. Node prints the heap
/// trace on its way down; a SIGKILL with no output is the OS reclaiming
/// memory, which `describe_status` already names as such.
fn is_oom_failure(error: &anyhow::Error) -> bool {
    let text = format!("{:#}", error);
    text.contains("heap out of memory")
        || text.contains("Ineffective mark-compacts")
        || text.contains("SIGKILL")
}

/// `base` with the crawler tools added to `tools.exclude`, every other
/// setting left exactly as it was. Anything that is not the expected shape is
/// replaced rather than tripped over — these files are user-edited.
fn with_crawler_tools_excluded(mut base: serde_json::Value) -> serde_json::Value {
    if !base.is_object() {
        base = serde_json::json!({});
    }
    let root = base.as_object_mut().expect("just made an object");
    let tools = root.entry("tools").or_insert_with(|| serde_json::json!({}));
    if !tools.is_object() {
        *tools = serde_json::json!({});
    }
    let exclude = tools
        .as_object_mut()
        .expect("just made an object")
        .entry("exclude")
        .or_insert_with(|| serde_json::json!([]));
    if !exclude.is_array() {
        *exclude = serde_json::json!([]);
    }
    let list = exclude.as_array_mut().expect("just made an array");
    for tool in CRAWLER_TOOLS {
        if !list.iter().any(|v| v.as_str() == Some(tool)) {
            list.push(serde_json::Value::String((*tool).to_string()));
        }
    }
    base
}

/// Writes the settings file for a guarded run, handed to the CLI via
/// `GEMINI_CLI_SYSTEM_SETTINGS_PATH`. Built on the system settings the CLI
/// would otherwise read, so it shadows nothing but the two disabled tools.
fn crawl_guard_settings() -> Result<GuardFile> {
    let base_path = std::env::var("GEMINI_CLI_SYSTEM_SETTINGS_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            if cfg!(target_os = "macos") {
                PathBuf::from("/Library/Application Support/GeminiCli/settings.json")
            } else {
                PathBuf::from("/etc/gemini-cli/settings.json")
            }
        });
    let base = std::fs::read_to_string(&base_path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_else(|| serde_json::json!({}));

    let content = serde_json::to_string_pretty(&with_crawler_tools_excluded(base))?;

    // The temp dir is shared: a fixed filename there is a file anyone else
    // can have opened first — a symlink written through, or a concurrent
    // `dt` race. A fresh, create_new-only name per file avoids both.
    for _ in 0..3 {
        let path = candidate_guard_path();
        match write_new_file(&path, &content) {
            Ok(()) => return Ok(GuardFile(path)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => {
                return Err(e).with_context(|| format!("failed to write {}", path.display()));
            }
        }
    }
    anyhow::bail!("could not create a crawl-guard settings file in {}", std::env::temp_dir().display())
}

/// A guard settings file that lives exactly as long as the run using it:
/// written fresh per run so mid-session settings edits are never shadowed by
/// a stale snapshot, deleted on drop so nothing leaks in the temp dir.
struct GuardFile(PathBuf);

impl Drop for GuardFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn candidate_guard_path() -> PathBuf {
    let suffix = RandomState::new().hash_one(());
    std::env::temp_dir()
        .join(format!("duetcode-gemini-crawl-guard-{}-{:016x}.json", std::process::id(), suffix))
}

fn write_new_file(path: &Path, content: &str) -> std::io::Result<()> {
    let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(path)?;
    file.write_all(content.as_bytes())
}

impl GeminiAdapter {
    pub fn new(
        config: &GeminiConfig,
        working_dir: &Path,
        verbose: bool,
        sink: Arc<dyn Sink>,
    ) -> Result<Self> {
        let mode = config.mode.to_lowercase();
        let cli_available = mode != "api" && Self::check_cli_available(&config.command);
        let api_key = std::env::var(&config.api_key_env).ok().filter(|k| !k.trim().is_empty());

        let use_cli = match mode.as_str() {
            "cli" => {
                if !cli_available {
                    anyhow::bail!(
                        "[gemini] mode = \"cli\" but '{}' is not on PATH — \
                         install it with `npm i -g @google/gemini-cli`, or set mode = \"auto\"",
                        config.command
                    );
                }
                true
            }
            "api" => false,
            _ => cli_available,
        };

        // The CLI carries its own authentication, so the key is required only
        // when calls actually go over the API.
        if !use_cli && api_key.is_none() {
            anyhow::bail!(
                "{} not set — export it, or install the gemini CLI so the reviewer \
                 can read the code it is judging",
                config.api_key_env
            );
        }

        // Said up front, not after a crash: a CLI without ripgrep greps
        // in-process and can die of memory exhaustion mid-review, and the
        // OOM it leaves behind names nothing the user can act on.
        if use_cli {
            if let Some(warning) = super::ripgrep::ripgrep_status().warning() {
                sink.event(Event::Warn { text: warning });
            }
        }

        let agent = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(30))
            .timeout_read(Duration::from_secs(config.timeout_secs))
            .build();

        if verbose {
            if use_cli {
                eprintln!("  {} gemini mode: CLI ({})", "[verbose]".dimmed(), config.command);
            } else {
                eprintln!("  {} gemini mode: API (direct, no file access)", "[verbose]".dimmed());
            }
        }

        Ok(Self {
            config: config.clone(),
            use_cli,
            verbose,
            api_key,
            agent,
            history: Vec::new(),
            working_dir: working_dir.to_path_buf(),
            readable_dirs: Vec::new(),
            sessions: HashMap::new(),
            files_opened: Vec::new(),
            crawl_checked: HashSet::new(),
            crawl_hazards: HashSet::new(),
            sink,
        })
    }

    pub fn is_key_available(config: &GeminiConfig) -> bool {
        std::env::var(&config.api_key_env).map(|k| !k.trim().is_empty()).unwrap_or(false)
    }

    pub fn is_cli_available(config: &GeminiConfig) -> bool {
        Self::check_cli_available(&config.command)
    }

    fn check_cli_available(command: &str) -> bool {
        Command::new("which")
            .arg(command)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// True when a failed CLI call may be retried over the API: only in `auto`
    /// mode, and only with a key to retry with. An explicit `mode = "cli"` must
    /// fail loudly rather than quietly downgrade to a reviewer that cannot read.
    fn can_fall_back_to_api(&self) -> bool {
        self.config.mode.eq_ignore_ascii_case("auto") && self.api_key.is_some()
    }

    /// The stored session for the checkout the adapter currently points at.
    fn current_session(&self) -> Option<&String> {
        self.sessions.get(&self.working_dir)
    }

    /// Remembers the session this run belongs to, so the next round in the same
    /// project resumes it — whether or not this run succeeded.
    fn record_session(&mut self, attempt: &CliAttempt) {
        if let Some(id) = &attempt.session_id {
            self.sessions.insert(self.working_dir.clone(), id.clone());
        }
    }

    /// Sizes up, once per directory, what the CLI's file crawler would walk,
    /// warning about any tree large enough to kill it. Covers the sibling
    /// dirs too: a glob searches every workspace directory, not just the one
    /// under review.
    ///
    /// Returns whether the current workspace holds any such tree, so the
    /// caller can disable the crawler up front — the assessment already knows
    /// how a crawl here ends, and letting it run only adds the crash.
    fn crawl_guard_needed(&mut self) -> bool {
        let dirs: Vec<PathBuf> = std::iter::once(self.working_dir.clone())
            .chain(self.readable_dirs.iter().cloned())
            .collect();
        let mut newly_hazardous = false;
        for dir in &dirs {
            if !self.crawl_checked.insert(dir.clone()) {
                continue;
            }
            if let Some(warning) = super::crawl::assess(dir).warning() {
                self.sink.event(Event::Warn { text: warning });
                self.crawl_hazards.insert(dir.clone());
                newly_hazardous = true;
            }
        }
        if newly_hazardous {
            self.sink.event(Event::Warn {
                text: format!(
                    "running with gemini's crawler tools ({}) disabled so the run survives — \
                     file reads and ripgrep search still work",
                    CRAWLER_TOOLS.join(", ")
                ),
            });
        }
        dirs.iter().any(|dir| self.crawl_hazards.contains(dir))
    }

    fn run_cli(
        &mut self,
        prompt: &str,
        crawl_guard: Option<&Path>,
    ) -> Result<(String, UsageStats)> {
        // Cleared per turn: what matters is what this answer went and read, not
        // what some earlier answer did.
        self.files_opened.clear();

        let was_resuming = self.current_session().is_some();
        let attempt = self.spawn_cli(prompt, crawl_guard);
        let connected = attempt.connected();
        let overflowed = attempt.overflowed;
        self.record_session(&attempt);

        // A rejected resume is one failure a fresh session fixes; an overflowed
        // one is another — the session is still on disk, but too big to continue.
        let restart = should_restart_session(was_resuming, connected) || (was_resuming && overflowed);

        if !restart {
            let CliOutput { text, usage, files_opened, .. } = attempt.result?;
            self.files_opened = files_opened;
            return Ok((text, usage));
        }

        if connected {
            self.sink.event(Event::Warn {
                text: "gemini's session outgrew its context — starting a fresh one".to_string(),
            });
        }
        self.sessions.remove(&self.working_dir);
        let retry = self.spawn_cli(prompt, crawl_guard);
        self.record_session(&retry);
        let CliOutput { text, usage, files_opened, .. } = retry.result?;
        self.files_opened = files_opened;
        Ok((text, usage))
    }

    fn spawn_cli(&self, prompt: &str, crawl_guard: Option<&Path>) -> CliAttempt {
        let mut cmd = Command::new(&self.config.command);
        cmd.arg("-m")
            .arg(&self.config.model)
            .arg("-o")
            .arg("stream-json")
            .arg("--approval-mode")
            .arg(APPROVAL_MODE)
            // Without this the CLI silently downgrades --approval-mode to
            // "default" in an untrusted folder and then refuses to run at all.
            .arg("--skip-trust")
            .current_dir(&self.working_dir);

        // Repeated flags rather than one comma-separated value: a project path
        // is free to contain a comma, and splitting on it would invent a
        // directory that does not exist.
        let extra = readable_extras(&self.readable_dirs, &self.working_dir);
        for dir in &extra {
            cmd.arg("--include-directories").arg(dir);
        }
        if let Some(id) = self.current_session() {
            cmd.arg("-r").arg(id);
        }
        if let Some(guard) = crawl_guard {
            cmd.env("GEMINI_CLI_SYSTEM_SETTINGS_PATH", guard);
        }

        // The prompt goes in on stdin rather than in `-p`; see `feed_stdin`.
        cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());

        if self.verbose {
            let siblings: String = extra
                .iter()
                .map(|dir| format!(" --include-directories {}", dir.display()))
                .collect();
            eprintln!(
                "  {} {} -m {} -o stream-json --approval-mode {}{}{} <prompt on stdin> (in {})",
                "[verbose]".dimmed(),
                self.config.command,
                self.config.model,
                APPROVAL_MODE,
                siblings,
                if self.current_session().is_some() { " -r <session>" } else { "" },
                self.working_dir.display(),
            );
        }

        let mut child = match crate::process::spawn_grouped(&mut cmd)
            .with_context(|| format!("failed to execute '{}'", self.config.command))
        {
            Ok(child) => child,
            Err(e) => return CliAttempt::failed(e),
        };

        let writer = super::feed_stdin(&mut child, prompt);
        let mut attempt = self.finish_cli(&mut child);
        // Joined only once the run is over: a prompt larger than the pipe
        // buffer is still being written while the CLI is answering.
        let delivered = super::prompt_delivered(writer);

        // A failed write is worth reporting only when the run otherwise looks
        // fine. If the CLI died, the pipe broke because of that, and its own
        // exit status explains more than "broken pipe" does.
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

    fn finish_cli(&self, child: &mut Child) -> CliAttempt {
        // Drained on its own thread, started before stdout is read. Left until
        // afterwards it would deadlock a long review: once the CLI fills the
        // stderr pipe it blocks writing, so it stops producing stdout, and both
        // processes wait on each other forever.
        let stderr_drain = super::drain_stderr(child);

        let timeout = Duration::from_secs(self.config.cli_timeout_secs);
        // Armed before stdout is read: a wedged CLI never closes the pipe, and
        // only a kill at the deadline unblocks the reader below.
        let watchdog = crate::process::Watchdog::arm(child, timeout);
        let streamed = self.stream_cli_json(child);
        let timed_out = watchdog.disarm();

        let output = match streamed {
            Ok(output) => output,
            Err(e) => return CliAttempt::failed(e),
        };
        // Read off the parsed output, so a failure below still reports it.
        let session_id = output.session_id.clone();

        // stdout is closed by now, so the run's budget is already spent
        // streaming; the exit wait gets a short grace, not a second budget.
        let reap = if timeout.is_zero() { timeout } else { crate::process::REAP_GRACE };
        let status =
            match crate::process::wait_or_kill(child, reap).context("failed to wait for gemini") {
                Ok(Some(status)) => status,
                Ok(None) => {
                    return CliAttempt {
                        session_id,
                        overflowed: false,
                        result: Err(anyhow::anyhow!(
                            "gemini CLI did not exit after finishing its output — killed after {}s",
                            crate::process::REAP_GRACE.as_secs()
                        )),
                    };
                }
                Err(e) => return CliAttempt { session_id, overflowed: false, result: Err(e) },
            };
        let stderr = stderr_drain.map(|h| h.join().unwrap_or_default()).unwrap_or_default();

        if self.verbose && !stderr.is_empty() {
            eprintln!("  {} stderr: {}", "[verbose]".dimmed(), stderr.trim());
        }

        // Honored only for a failed run: a child can finish cleanly in the
        // same instant the deadline passes, and that answer is not discarded.
        if timed_out && !status.success() {
            return CliAttempt {
                session_id,
                overflowed: false,
                result: Err(anyhow::anyhow!(
                    "gemini CLI timed out after {}s — raise cli_timeout_secs in \
                     .duet/config.toml [gemini] to allow longer runs",
                    timeout.as_secs()
                )),
            };
        }

        if !status.success() {
            // Classified from the CLI's own diagnostics — stderr or its typed
            // error events — never from the model's prose.
            let overflowed = super::is_context_overflow(&stderr)
                || output.error_detail.as_deref().is_some_and(super::is_context_overflow);
            return CliAttempt {
                session_id,
                overflowed,
                result: Err(anyhow::anyhow!(
                    "gemini CLI {}",
                    describe_status(&status, &stderr, &output.text)
                )),
            };
        }

        CliAttempt { session_id, overflowed: false, result: Ok(output) }
    }

    /// Parses the CLI's newline-delimited JSON events. Shapes are those the
    /// installed CLI actually emits: `init`, `message` (with `delta` chunks for
    /// the assistant), `tool_use`, `tool_result`, `error`, `result`.
    fn stream_cli_json(&self, child: &mut Child) -> Result<CliOutput> {
        let stdout_pipe = child.stdout.take().context("failed to capture gemini stdout")?;
        let reader = BufReader::new(stdout_pipe);

        let mut text = String::new();
        let mut started = false;
        let mut files_opened: Vec<String> = Vec::new();
        let mut session_id: Option<String> = None;
        let mut error_detail: Option<String> = None;
        let mut model_name = self.config.model.clone();
        let mut input_tokens: u64 = 0;
        let mut output_tokens: u64 = 0;
        let start = std::time::Instant::now();

        for line in reader.lines() {
            let line = match line {
                Ok(l) => l,
                Err(e) => {
                    eprintln!("  {} stream read error: {}", "✗".red(), e);
                    break;
                }
            };

            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let event: serde_json::Value = match serde_json::from_str(trimmed) {
                Ok(v) => v,
                Err(_) => continue,
            };

            match event.get("type").and_then(|v| v.as_str()).unwrap_or("") {
                "init" => {
                    if let Some(id) = event.get("session_id").and_then(|v| v.as_str()) {
                        session_id = Some(id.to_string());
                    }
                    if let Some(model) = event.get("model").and_then(|v| v.as_str()) {
                        eprintln!("  {} connected (model: {})", "●".green(), model);
                    }
                    self.sink.event(Event::Thinking { model: "gemini".into() });
                }
                "message" => {
                    if let Some(chunk) = assistant_chunk(&event) {
                        self.emit_chunk(&mut started, chunk);
                        text.push_str(chunk);
                    }
                }
                "tool_use" => {
                    let tool = event.get("tool_name").and_then(|v| v.as_str()).unwrap_or("tool");
                    files_opened.extend(opened_paths(tool, event.get("parameters")));
                    let desc = describe_tool_action(tool, event.get("parameters"));
                    self.sink.event(Event::ToolAction { model: "gemini".into(), desc });
                }
                "tool_result" => {
                    if event.get("status").and_then(|v| v.as_str()) == Some("error") {
                        eprintln!("  {} tool failed", "✗".red());
                    }
                }
                "error" => {
                    if let Some(message) = error_message(&event) {
                        self.sink.event(Event::Warn { text: format!("gemini: {}", message) });
                        error_detail = Some(message);
                    }
                }
                "result" => {
                    let stats = parse_result_stats(&event);
                    input_tokens = stats.input_tokens;
                    output_tokens = stats.output_tokens;
                    if let Some(resolved) = stats.model {
                        model_name = resolved;
                    }
                }
                other => {
                    if self.verbose {
                        eprintln!(
                            "  {} {} ({:.0}s)",
                            "·".dimmed(),
                            other,
                            start.elapsed().as_secs_f64()
                        );
                    }
                }
            }
        }

        self.sink.event(Event::StreamEnd { model: "gemini".into() });

        let elapsed = start.elapsed().as_secs_f64();
        eprintln!("  {} finished ({:.1}s)", "●".green(), elapsed);
        ui::stream_footer();

        let usage = UsageStats {
            cost_usd: pricing::compute_cost(&model_name, input_tokens, output_tokens),
            input_tokens,
            output_tokens,
            model: model_name,
        };

        Ok(CliOutput { text, usage, session_id, files_opened, error_detail })
    }

    fn emit_chunk(&self, started: &mut bool, text: &str) {
        if !*started {
            self.sink.event(Event::StreamStart { model: "gemini".into() });
            *started = true;
        }
        self.sink.event(Event::StreamChunk { model: "gemini".into(), text: text.to_string() });
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
        if self.config.model.starts_with('/') {
            self.config.model.clone()
        } else {
            format!("/{}", self.config.model)
        }
    }

    /// The key for an API call. Present whenever this transport is reachable —
    /// the constructor refuses API-only mode without one, and the auto-mode
    /// fallback checks for one before retrying.
    fn require_api_key(&self) -> Result<&str> {
        self.api_key.as_deref().ok_or_else(|| {
            anyhow::anyhow!("{} not set — cannot reach the Gemini API", self.config.api_key_env)
        })
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
        eprintln!("  {} streaming from {}", "●".green(), self.config.model);
        eprintln!("  {} thinking...", "◌".cyan());

        let start = std::time::Instant::now();

        // Key goes in a header so it never shows up in URLs or error output.
        let response = self.agent.post(url)
            .set("x-goog-api-key", self.require_api_key()?)
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
        let mut finish_reason: Option<String> = None;

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
                        self.sink.event(Event::StreamStart { model: "gemini".into() });
                        header_printed = true;
                    }
                    self.sink.event(Event::StreamChunk {
                        model: "gemini".into(),
                        text: text.to_string(),
                    });
                    collected.push_str(text);
                }
            }

            if let Some(reason) =
                chunk.pointer("/candidates/0/finishReason").and_then(|v| v.as_str())
            {
                finish_reason = Some(reason.to_string());
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

        self.sink.event(Event::StreamEnd { model: "gemini".into() });

        let elapsed = start.elapsed().as_secs_f64();
        let cost_usd = pricing::compute_cost(&self.config.model, input_tokens, output_tokens);

        eprintln!(
            "  {} finished ({:.1}s, {} chunks)",
            "●".green(),
            elapsed,
            chunk_count,
        );
        ui::stream_footer();

        if collected.is_empty() {
            anyhow::bail!(
                "Gemini returned no text after {:.1}s — {}",
                elapsed,
                describe_empty_response(finish_reason.as_deref())
            );
        }

        let usage = UsageStats {
            input_tokens,
            output_tokens,
            cost_usd,
            model: self.config.model.clone(),
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
        if !self.use_cli {
            return self.stream_generate(prompt, images);
        }

        // `-p` carries text only. Images are rare here — the reviewer never
        // sends any — so rather than dropping them silently, hand the call to
        // the API transport, which does support them.
        if !images.is_empty() && self.api_key.is_some() {
            return self.stream_generate(prompt, images);
        }
        if !images.is_empty() {
            self.sink.event(Event::Warn {
                text: format!(
                    "{} image(s) dropped — the gemini CLI takes text prompts only, and no {} \
                     is set to send them over the API instead",
                    images.len(),
                    self.config.api_key_env
                ),
            });
        }

        // A workspace the preflight sized as lethal to the crawler gets the
        // guard from the first spawn: the same degraded run the OOM retry
        // below would reach, without paying for the crash on the way.
        let guard = if self.crawl_guard_needed() { crawl_guard_settings().ok() } else { None };
        let mut result = match &guard {
            Some(file) => self.run_cli(&with_crawl_guard_note(prompt), Some(&file.0)),
            None => self.run_cli(prompt, None),
        };

        // An OOM death gets one retry with the crawler tools disabled before
        // any thought of the API: a reviewer that reads files but cannot glob
        // beats one that cannot read at all. Not when the guard was already
        // on — the identical spawn cannot end differently.
        if let Err(e) = &result {
            if guard.is_none() && is_oom_failure(e) {
                if let Ok(retry_guard) = crawl_guard_settings() {
                    self.sink.event(Event::Warn {
                        text: format!(
                            "gemini ran out of memory crawling the workspace — retrying with \
                             its crawler tools ({}) disabled; file reads and ripgrep search \
                             still work",
                            CRAWLER_TOOLS.join(", ")
                        ),
                    });
                    result = self.run_cli(&with_crawl_guard_note(prompt), Some(&retry_guard.0));
                }
            }
        }

        match result {
            Ok(result) => Ok(result),
            Err(e) if self.can_fall_back_to_api() => {
                self.sink.event(Event::Warn {
                    text: format!(
                        "gemini CLI failed ({:#}) — falling back to the API, which cannot read files",
                        e
                    ),
                });
                self.stream_generate(prompt, images)
            }
            Err(e) => Err(e),
        }
    }

    fn name(&self) -> &str {
        "gemini"
    }

    fn streams_output(&self) -> bool {
        true
    }

    /// Sessions are keyed by checkout, so moving here looks up this project's
    /// own session rather than resuming a conversation about another one.
    fn set_working_dir(&mut self, dir: &Path) {
        self.working_dir = dir.to_path_buf();
    }

    fn set_readable_dirs(&mut self, dirs: &[PathBuf]) {
        self.readable_dirs = dirs.to_vec();
    }

    /// Only the CLI carries tools; the API path is a bare generateContent call.
    fn can_read_files(&self) -> bool {
        self.use_cli
    }

    fn files_opened_last_turn(&self) -> Vec<String> {
        self.files_opened.clone()
    }

    /// Drops this checkout's session and the API history behind it, so the next
    /// call starts from the prompt alone.
    fn reset_session(&mut self) {
        self.sessions.remove(&self.working_dir);
        self.history.clear();
        self.files_opened.clear();
    }
}

/// The assistant's own text from a `message` event. The prompt is echoed back
/// as a `user` message, and counting that as output would put the reviewer's
/// own instructions into its answer.
fn assistant_chunk(event: &serde_json::Value) -> Option<&str> {
    if event.get("role").and_then(|v| v.as_str()) != Some("assistant") {
        return None;
    }
    event.get("content").and_then(|v| v.as_str()).filter(|c| !c.is_empty())
}

/// Token counts and the model the CLI actually used, from a `result` event.
#[derive(Debug, Default, PartialEq, Eq)]
struct ResultStats {
    input_tokens: u64,
    output_tokens: u64,
    /// The concrete model the CLI resolved to, which may differ from the alias
    /// that was requested — `pro` reports back as `gemini-3.1-pro-preview`.
    /// Pricing needs the resolved name, not the alias.
    model: Option<String>,
}

fn parse_result_stats(event: &serde_json::Value) -> ResultStats {
    let Some(stats) = event.get("stats") else {
        return ResultStats::default();
    };
    let count = |key: &str| stats.get(key).and_then(|v| v.as_u64()).unwrap_or(0);

    ResultStats {
        input_tokens: count("input_tokens"),
        output_tokens: count("output_tokens"),
        model: stats
            .get("models")
            .and_then(|m| m.as_object())
            .and_then(|m| m.keys().next())
            .cloned(),
    }
}

/// The files a tool call actually opened, or nothing for a call that opened
/// none.
///
/// Only the tools that read a named file count. A grep or a glob is a search
/// across the tree rather than a reading of any one file, and counting one as
/// "I opened src/foo.rs" would make the record it feeds no better than the
/// claim it exists to check.
fn opened_paths(tool: &str, params: Option<&serde_json::Value>) -> Vec<String> {
    if !matches!(tool, "read_file" | "read_many_files") {
        return Vec::new();
    }
    let Some(params) = params else {
        return Vec::new();
    };

    let single = ["file_path", "path", "absolute_path"]
        .iter()
        .filter_map(|key| params.get(*key).and_then(|v| v.as_str()))
        .map(str::to_string);

    // read_many_files takes a list, so the singular keys find nothing there.
    let many = ["paths", "file_paths"]
        .iter()
        .filter_map(|key| params.get(*key).and_then(|v| v.as_array()))
        .flatten()
        .filter_map(|v| v.as_str())
        .map(str::to_string);

    single.chain(many).collect()
}

/// One-line description of a CLI tool call, for the activity line the user
/// sees. Names are the gemini CLI's own tool identifiers.
fn describe_tool_action(tool: &str, params: Option<&serde_json::Value>) -> String {
    let get_str = |key: &str| -> Option<&str> {
        params.and_then(|v| v.get(key)).and_then(|v| v.as_str())
    };

    match tool {
        "read_file" | "read_many_files" => match get_str("file_path").or_else(|| get_str("path")) {
            Some(path) => format!("reading {}", path),
            None => "reading files".to_string(),
        },
        "search_file_content" => {
            let pattern = get_str("pattern").unwrap_or("?");
            match get_str("path") {
                Some(path) => format!("searching '{}' in {}", truncate_chars(pattern, 30), path),
                None => format!("searching '{}'", truncate_chars(pattern, 40)),
            }
        }
        "glob" => match get_str("pattern") {
            Some(pattern) => format!("finding files matching '{}'", truncate_chars(pattern, 40)),
            None => "finding files".to_string(),
        },
        "list_directory" => match get_str("path") {
            Some(path) => format!("listing {}", path),
            None => "listing directory".to_string(),
        },
        "run_shell_command" => match get_str("command") {
            Some(cmd) => format!("running `{}`", truncate_chars(cmd.trim(), 60)),
            None => "running command".to_string(),
        },
        "google_web_search" => match get_str("query") {
            Some(query) => format!("searching web: {}", truncate_chars(query, 50)),
            None => "searching the web".to_string(),
        },
        "web_fetch" => match get_str("url").or_else(|| get_str("prompt")) {
            Some(url) => format!("fetching {}", truncate_chars(url, 60)),
            None => "fetching URL".to_string(),
        },
        other => format!("using {}", other),
    }
}

/// Truncates to `max` *characters*, never bytes: tool arguments are arbitrary
/// text, and slicing one at a byte index that lands mid-character panics —
/// which `panic = "abort"` in the release profile turns into a killed process.
fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(max).collect::<String>())
    }
}

/// How the CLI ended, in words. A process killed by a signal has no exit code
/// at all, and reporting that as `-1` describes nothing — the signal is the
/// whole explanation, and a SIGKILL with no output is almost always the machine
/// running out of memory rather than anything the CLI did.
fn describe_status(status: &std::process::ExitStatus, stderr: &str, output: &str) -> String {
    use std::os::unix::process::ExitStatusExt;

    match (status.code(), status.signal()) {
        (Some(code), _) => format!("exited with {}: {}", code, describe_exit(code, stderr, output)),
        (None, Some(9)) => format!(
            "was killed (SIGKILL) — usually the machine running out of memory{}",
            trailing_detail(stderr, output)
        ),
        (None, Some(signal)) => format!(
            "was killed by signal {}{}",
            signal,
            trailing_detail(stderr, output)
        ),
        (None, None) => format!("ended abnormally{}", trailing_detail(stderr, output)),
    }
}

/// Whatever the process managed to say before dying, or nothing at all.
fn trailing_detail(stderr: &str, output: &str) -> String {
    let detail = first_non_empty(stderr, output);
    if detail == "no output" {
        String::new()
    } else {
        format!(": {}", detail)
    }
}

fn first_non_empty<'a>(stderr: &'a str, output: &'a str) -> &'a str {
    if !stderr.trim().is_empty() {
        stderr.trim()
    } else if !output.trim().is_empty() {
        output.trim()
    } else {
        "no output"
    }
}

/// Turns a CLI exit into something a user can act on. The documented codes are
/// 0/1/42/53; 55 (untrusted folder) is not documented but is what an
/// unreadable checkout actually returns, so it is named explicitly.
fn describe_exit(code: i32, stderr: &str, output: &str) -> String {
    let detail = first_non_empty(stderr, output);

    match code {
        42 => format!("invalid prompt or arguments — {}", detail),
        53 => format!("turn limit exceeded — {}", detail),
        55 => format!("workspace not trusted — {}", detail),
        _ => detail.to_string(),
    }
}

fn error_message(event: &serde_json::Value) -> Option<String> {
    event
        .get("message")
        .and_then(|v| v.as_str())
        .or_else(|| event.pointer("/error/message").and_then(|v| v.as_str()))
        .map(str::to_string)
}

/// Why a response came back with no text in it. The API says which of these it
/// was, so say that: guessing at a content filter when the model had in fact
/// reached for a tool sends the reader looking in entirely the wrong place.
///
/// `MALFORMED_FUNCTION_CALL` is the one this transport provokes on its own. It
/// declares no tools, and a prompt written for the CLI asks the model to go and
/// read the repository — so it tries to call something that was never offered.
fn describe_empty_response(finish_reason: Option<&str>) -> String {
    match finish_reason {
        Some("MALFORMED_FUNCTION_CALL") => "the model tried to call a tool, and this transport \
             offers none. Install the gemini CLI so it can read the checkout instead of \
             guessing at it."
            .to_string(),
        Some("SAFETY") | Some("PROHIBITED_CONTENT") | Some("BLOCKLIST") => {
            "a content filter blocked the response".to_string()
        }
        Some("MAX_TOKENS") => "the model stopped at its output limit before writing any text"
            .to_string(),
        Some(other) => format!("the model stopped with finishReason {}", other),
        None => "the stream ended without a finishReason".to_string(),
    }
}

fn extract_api_error(response: &str) -> Option<String> {
    let json: serde_json::Value = serde_json::from_str(response).ok()?;
    let message = json.get("error")?.get("message")?.as_str()?;
    Some(message.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::prompt_delivered;

    fn event(raw: &str) -> serde_json::Value {
        serde_json::from_str(raw).expect("test fixture must be valid JSON")
    }

    /// Fixtures are lines the installed CLI actually emitted (v0.53.1), not
    /// shapes read off documentation — the field names are what this parser
    /// depends on, so they are pinned to observed output.
    const REAL_RESULT: &str = r#"{"type":"result","timestamp":"2026-08-05T17:53:45.174Z","status":"success","stats":{"total_tokens":43498,"input_tokens":20846,"output_tokens":62,"cached":8089,"input":12757,"duration_ms":153348,"tool_calls":2,"models":{"gemini-3.1-pro-preview":{"total_tokens":43498,"input_tokens":20846,"output_tokens":62}}}}"#;

    #[test]
    fn result_stats_come_from_the_named_token_fields() {
        let stats = parse_result_stats(&event(REAL_RESULT));
        assert_eq!(stats.input_tokens, 20846);
        assert_eq!(stats.output_tokens, 62);
        // Not `input` (12757), which is a different count sitting beside it.
        assert_eq!(stats.model.as_deref(), Some("gemini-3.1-pro-preview"));
    }

    /// A `result` without stats must not fail the run: the answer is already
    /// collected by then, and a missing count is worth less than the review.
    #[test]
    fn a_result_without_stats_is_not_an_error() {
        assert_eq!(parse_result_stats(&event(r#"{"type":"result"}"#)), ResultStats::default());
    }

    /// The CLI resolves aliases, so the model that priced the call is the one
    /// it reports back — `pro` bills as `gemini-3.1-pro-preview`.
    #[test]
    fn the_resolved_model_is_what_gets_priced() {
        let stats = parse_result_stats(&event(REAL_RESULT));
        let model = stats.model.unwrap();
        assert!(pricing::compute_cost(&model, 1_000_000, 0).is_some());
    }

    #[test]
    fn only_the_assistants_own_text_is_collected() {
        let assistant = event(r#"{"type":"message","role":"assistant","content":"372","delta":true}"#);
        assert_eq!(assistant_chunk(&assistant), Some("372"));

        // The prompt echoed back — collecting it would put the review's own
        // instructions into the review.
        let user = event(r#"{"type":"message","role":"user","content":"How many lines?"}"#);
        assert_eq!(assistant_chunk(&user), None);

        let empty = event(r#"{"type":"message","role":"assistant","content":""}"#);
        assert_eq!(assistant_chunk(&empty), None);
    }

    /// The whole point of the CLI reviewer is that the user can see it reading.
    #[test]
    fn a_file_read_is_described_by_its_path() {
        let params = serde_json::json!({ "file_path": "src/prompts.rs" });
        assert_eq!(describe_tool_action("read_file", Some(&params)), "reading src/prompts.rs");

        let grep = serde_json::json!({ "pattern": "formatCreditErrorMessage" });
        assert_eq!(
            describe_tool_action("search_file_content", Some(&grep)),
            "searching 'formatCreditErrorMessage'"
        );
    }

    #[test]
    fn an_unknown_tool_is_still_named() {
        assert_eq!(describe_tool_action("update_topic", None), "using update_topic");
    }

    /// 55 is undocumented but is exactly what an untrusted checkout returns,
    /// and "exited with 55" alone tells the user nothing they can act on.
    #[test]
    fn exit_codes_are_explained() {
        assert!(describe_exit(55, "not trusted", "").contains("workspace not trusted"));
        assert!(describe_exit(53, "", "").contains("turn limit"));
        assert!(describe_exit(42, "", "").contains("invalid prompt"));
        assert_eq!(describe_exit(1, "boom", ""), "boom");
    }

    /// Falls back through stderr, then stdout, so the error is never empty.
    #[test]
    fn an_exit_with_no_stderr_reports_whatever_was_printed() {
        assert_eq!(describe_exit(1, "  ", "partial output"), "partial output");
        assert_eq!(describe_exit(1, "", ""), "no output");
    }

    /// A signal death has no exit code, and "exited with -1" names nothing the
    /// user can act on. Observed in practice: the CLI is a heavy node process
    /// and the OS kills it under memory pressure, leaving no output at all.
    #[test]
    fn a_signal_death_is_named_rather_than_reported_as_minus_one() {
        use std::os::unix::process::ExitStatusExt;
        let killed = std::process::ExitStatus::from_raw(9);

        let described = describe_status(&killed, "", "");
        assert!(described.contains("SIGKILL"), "got: {}", described);
        assert!(described.contains("out of memory"), "got: {}", described);
        assert!(!described.contains("-1"), "got: {}", described);
    }

    #[test]
    fn an_ordinary_exit_code_still_reports_its_code() {
        use std::os::unix::process::ExitStatusExt;
        // Wait-status encoding: the exit code sits in the high byte.
        let exited = std::process::ExitStatus::from_raw(42 << 8);
        assert!(describe_status(&exited, "bad flag", "").contains("exited with 42"));
    }

    /// Only a rejected resume justifies burning a second call.
    #[test]
    fn only_a_rejected_resume_restarts_the_session() {
        assert!(should_restart_session(true, false));
        assert!(!should_restart_session(true, true));
        assert!(!should_restart_session(false, false));
    }

    /// A truncated tool argument must not slice a multi-byte character in half:
    /// `panic = "abort"` would turn that into a killed process mid-review.
    #[test]
    fn tool_arguments_truncate_on_character_boundaries() {
        assert_eq!(truncate_chars("日本語のパターン", 3), "日本語…");
        assert_eq!(truncate_chars("short", 40), "short");
    }

    /// The failure this transport causes itself must not read as the model
    /// refusing: a malformed tool call means no tools were offered, and the fix
    /// is the CLI, not a differently-worded prompt.
    #[test]
    fn an_empty_response_names_the_reason_the_api_gave() {
        let tool_call = describe_empty_response(Some("MALFORMED_FUNCTION_CALL"));
        assert!(tool_call.contains("tool"), "got: {}", tool_call);
        assert!(!tool_call.contains("filter"), "got: {}", tool_call);

        assert!(describe_empty_response(Some("SAFETY")).contains("content filter"));
        assert!(describe_empty_response(Some("MAX_TOKENS")).contains("output limit"));
        // An unfamiliar reason is still passed through rather than swallowed.
        assert!(describe_empty_response(Some("RECITATION")).contains("RECITATION"));
        assert!(describe_empty_response(None).contains("without a finishReason"));
    }

    /// A prompt that never fully reached the CLI is not a prompt: whatever the
    /// CLI answered, it answered to a question it only partly heard.
    #[test]
    fn a_prompt_that_was_never_written_is_not_reported_as_delivered() {
        let broken = std::thread::spawn(|| {
            Err(std::io::Error::new(std::io::ErrorKind::BrokenPipe, "broken pipe"))
        });
        let message = format!("{:#}", prompt_delivered(Some(broken)).unwrap_err());
        assert!(message.contains("failed to send the prompt"), "got: {}", message);

        // No writer means no stdin to write to, so nothing was sent at all.
        let message = format!("{:#}", prompt_delivered(None).unwrap_err());
        assert!(message.contains("never sent"), "got: {}", message);

        let written = std::thread::spawn(|| Ok(()));
        assert!(prompt_delivered(Some(written)).is_ok());
    }

    /// The OOM signature is pinned to what node actually prints on its way
    /// down (observed in the wild), plus the SIGKILL case where the OS killed
    /// it before it could print anything.
    #[test]
    fn an_oom_death_is_recognised_and_an_ordinary_failure_is_not() {
        let heap = anyhow::anyhow!(
            "gemini CLI exited with 1: FATAL ERROR: Ineffective mark-compacts near heap limit \
             Allocation failed - JavaScript heap out of memory"
        );
        assert!(is_oom_failure(&heap));

        let killed = anyhow::anyhow!(
            "gemini CLI was killed (SIGKILL) — usually the machine running out of memory"
        );
        assert!(is_oom_failure(&killed));

        let ordinary = anyhow::anyhow!("gemini CLI exited with 42: invalid prompt or arguments");
        assert!(!is_oom_failure(&ordinary));
    }

    /// The guard settings ride on top of whatever the machine already has —
    /// disabling two tools must not cost the user their other settings, their
    /// own exclusions, or produce duplicates when applied twice.
    #[test]
    fn the_crawl_guard_extends_settings_without_clobbering_them() {
        let base = serde_json::json!({
            "theme": "dark",
            "tools": { "exclude": ["some_tool"], "sandbox": true }
        });
        let merged = with_crawler_tools_excluded(with_crawler_tools_excluded(base));

        assert_eq!(merged["theme"], "dark");
        assert_eq!(merged["tools"]["sandbox"], true);
        let exclude = merged["tools"]["exclude"].as_array().unwrap();
        let names: Vec<&str> = exclude.iter().filter_map(|v| v.as_str()).collect();
        assert_eq!(names, vec!["some_tool", "glob", "read_many_files"]);
    }

    /// A settings file that is not the expected shape must still come out
    /// usable — these files are user-edited, and a retry that crashes on a
    /// malformed one dies twice for one bug.
    #[test]
    fn a_malformed_settings_base_is_replaced_not_tripped_over() {
        let merged = with_crawler_tools_excluded(serde_json::json!({ "tools": "oops" }));
        let names: Vec<&str> = merged["tools"]["exclude"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert_eq!(names, vec!["glob", "read_many_files"]);
    }

    /// A guarded run's prompt must name what is gone and what remains. The CLI
    /// removes excluded tools silently, and a model that only sees two tools
    /// missing answers from the prompt alone instead of reaching for the
    /// reads and greps it still has.
    #[test]
    fn the_crawl_guard_note_names_what_is_gone_and_what_remains() {
        for tool in CRAWLER_TOOLS {
            assert!(CRAWL_GUARD_PROMPT_NOTE.contains(tool), "note omits {}", tool);
        }
        let noted = with_crawl_guard_note("PROMPT");
        assert!(noted.starts_with("PROMPT"));
        assert!(noted.contains("read_file"));
        assert!(noted.contains("ripgrep"));
    }

    /// An explicit `mode = "cli"` must not silently degrade to the API
    /// transport — that is precisely the reviewer the user asked to stop using.
    #[test]
    fn cli_mode_without_the_binary_names_the_fix() {
        let config = GeminiConfig {
            command: "definitely-not-a-real-binary-xyz".into(),
            mode: "cli".into(),
            ..GeminiConfig::default()
        };
        let sink: Arc<dyn Sink> = Arc::new(crate::events::TerminalSink::new(false));
        let message = match GeminiAdapter::new(&config, Path::new("."), false, sink) {
            Err(e) => format!("{:#}", e),
            Ok(_) => panic!("a missing CLI in cli mode must fail"),
        };
        assert!(message.contains("npm i -g @google/gemini-cli"), "got: {}", message);
    }
}

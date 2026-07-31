//! `dt serve` — JSON-lines protocol for GUI frontends (VS Code extension).
//!
//! Commands arrive on stdin, one JSON object per line:
//!   {"cmd":"task","task":"...","auto":true,"images":["/path.png"]}
//!   {"cmd":"plan","task":"..."}
//!   {"cmd":"review","task":"optional context","dirs":["/proj-a","/proj-b"]}
//!   {"cmd":"answer","id":3,"value":"y"}        // reply to an "ask" event
//!   {"cmd":"ping"} / {"cmd":"quit"}
//!
//! Events stream to stdout as JSON lines (see `events::Event`); stderr is
//! free-form logging. Adapters are constructed once per serve process, so
//! both models keep their context across tasks — same as the terminal REPL.

use crate::adapters::{ImageInput, ModelAdapter};
use crate::cli;
use crate::config::Config;
use crate::events::{AskKind, Event, Sink};
use crate::git;
use crate::orchestrator::{self, TaskOptions};
use anyhow::Result;
use serde::Deserialize;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{mpsc, Arc, Mutex};

#[derive(Debug, Default, Deserialize)]
struct Command {
    cmd: String,
    #[serde(default)]
    task: Option<String>,
    #[serde(default)]
    auto: Option<bool>,
    #[serde(default)]
    images: Option<Vec<String>>,
    #[serde(default)]
    id: Option<u64>,
    #[serde(default)]
    value: Option<String>,
    /// Projects a review covers. A multi-root workspace sends every folder so
    /// one clean project never hides the changes sitting in another; omitted
    /// or empty means the serve directory alone.
    #[serde(default)]
    dirs: Option<Vec<String>>,
    /// Project a task or plan runs in. Lets a fix land in the project the
    /// review was about rather than always the serve directory; omitted means
    /// the serve directory.
    #[serde(default)]
    dir: Option<String>,
}

/// Serializes events as JSON lines on stdout; asks block until the frontend
/// replies with an `answer` command.
pub struct JsonSink {
    answers: Mutex<Receiver<String>>,
    next_id: AtomicU64,
}

impl JsonSink {
    fn new(answers: Receiver<String>) -> Self {
        Self { answers: Mutex::new(answers), next_id: AtomicU64::new(1) }
    }
}

impl Sink for JsonSink {
    fn event(&self, event: Event) {
        let json = match serde_json::to_string(&event) {
            Ok(j) => j,
            Err(_) => return,
        };
        let mut out = std::io::stdout().lock();
        let _ = writeln!(out, "{}", json);
        let _ = out.flush();
    }

    fn ask(&self, kind: AskKind, question: &str) -> String {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        self.event(Event::Ask { id, kind, question: question.to_string() });
        self.answers
            .lock()
            .expect("answer channel poisoned")
            .recv()
            .unwrap_or_default()
    }
}

pub fn run(dir: &Path, writer_name: &str, overrides: cli::ModelOverrides) -> Result<()> {
    if !git::is_git_repo(dir) {
        anyhow::bail!("not a git repository — run `git init` first");
    }

    let (ans_tx, ans_rx) = mpsc::channel::<String>();
    let (cmd_tx, cmd_rx) = mpsc::channel::<Command>();

    let sink: Arc<JsonSink> = Arc::new(JsonSink::new(ans_rx));
    spawn_stdin_reader(cmd_tx, ans_tx, sink.clone());

    let setup = cli::setup_task(dir, writer_name, &[], false, sink.clone(), &overrides)?;
    let cli::TaskSetup { config, images: _, mut writer, mut reviewer } = setup;

    sink.event(Event::Ready {
        writer: writer.name().to_string(),
        reviewer: reviewer.name().to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
    });

    for cmd in cmd_rx {
        match cmd.cmd.as_str() {
            "ping" => sink.event(Event::Pong),
            "quit" => break,
            "task" | "plan" => {
                let Some(task) = cmd.task.as_deref().map(str::trim).filter(|t| !t.is_empty()) else {
                    sink.event(Event::Error { message: "missing 'task' field".into() });
                    continue;
                };

                let images = match load_images(cmd.images.as_deref().unwrap_or(&[])) {
                    Ok(images) => images,
                    Err(e) => {
                        sink.event(Event::Error { message: format!("{:#}", e) });
                        continue;
                    }
                };

                let target = match task_target(&cmd, dir) {
                    Ok(target) => target,
                    Err(message) => {
                        sink.event(Event::Error { message });
                        continue;
                    }
                };
                // Announced even for the serve directory: the writer's project
                // is never left implicit, so it cannot silently diverge from
                // the project a review was about.
                sink.event(Event::ProjectStarted {
                    name: target.label.clone(),
                    path: target.dir.display().to_string(),
                });

                let own_config = project_config(&target, sink.as_ref());
                let task_config = own_config.as_ref().unwrap_or(&config);

                let opts = TaskOptions {
                    config: task_config,
                    task,
                    images: &images,
                    repo_dir: &target.dir,
                    continue_session: false,
                    auto: cmd.auto.unwrap_or(false),
                    plan_first: cmd.cmd == "plan",
                };

                match orchestrator::run(&opts, writer.as_mut(), reviewer.as_mut(), sink.as_ref()) {
                    Ok(r) => sink.event(Event::TaskDone {
                        success: r.success,
                        rounds: r.rounds,
                        message: r.message,
                    }),
                    Err(e) => sink.event(Event::Error { message: format!("{:#}", e) }),
                }
            }
            "review" => run_review(&config, reviewer.as_mut(), dir, &cmd, sink.as_ref()),
            other => {
                sink.event(Event::Error { message: format!("unknown command '{}'", other) });
            }
        }
    }

    sink.event(Event::Bye);
    Ok(())
}

/// One project a review covers: where it lives and how the UI labels it.
struct ReviewTarget {
    dir: PathBuf,
    label: String,
}

impl ReviewTarget {
    fn new(dir: PathBuf) -> Self {
        let label = dir
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| dir.display().to_string());
        Self { dir, label }
    }
}

/// Every project the frontend asked for, falling back to the serve directory
/// when it sent none (plain `dt serve` clients and single-folder workspaces).
fn review_targets(cmd: &Command, default_dir: &Path) -> Vec<ReviewTarget> {
    match cmd.dirs.as_deref() {
        Some(dirs) if !dirs.is_empty() => {
            dirs.iter().map(|dir| ReviewTarget::new(PathBuf::from(dir))).collect()
        }
        _ => vec![ReviewTarget::new(default_dir.to_path_buf())],
    }
}

/// The project a task runs in: the requested one when the frontend named it,
/// otherwise the serve directory. Rejects anything that is not a git checkout
/// rather than writing into an unrelated folder.
fn task_target(cmd: &Command, default_dir: &Path) -> Result<ReviewTarget, String> {
    let Some(requested) = cmd.dir.as_deref().map(str::trim).filter(|d| !d.is_empty()) else {
        return Ok(ReviewTarget::new(default_dir.to_path_buf()));
    };

    let target = ReviewTarget::new(PathBuf::from(requested));
    if !git::is_git_repo(&target.dir) {
        return Err(format!("'{}' is not a git repository — cannot run a task there", requested));
    }
    Ok(target)
}

/// A project's own `.duet/config.toml` when it has one, so each project keeps
/// its own review prompt; `None` means fall back to the serve session's config.
fn project_config(target: &ReviewTarget, sink: &dyn Sink) -> Option<Config> {
    if !Config::config_path(&target.dir).exists() {
        return None;
    }
    match Config::load(&target.dir) {
        Ok(config) => Some(config),
        Err(e) => {
            sink.event(Event::Warn {
                text: format!("{} — {:#}, using the session config", target.label, e),
            });
            None
        }
    }
}

/// Outcome tally across the reviewed projects.
#[derive(Default)]
struct ReviewTally {
    reviewed: usize,
    approved: usize,
    failures: Vec<String>,
}

/// Reviews every requested project in turn. Projects that are not git repos or
/// have nothing uncommitted are skipped rather than failing the run, so a clean
/// first folder never hides the changes sitting in a second one.
fn run_review(
    session_config: &Config,
    reviewer: &mut dyn ModelAdapter,
    default_dir: &Path,
    cmd: &Command,
    sink: &dyn Sink,
) {
    let targets = review_targets(cmd, default_dir);
    let multi = targets.len() > 1;
    let mut tally = ReviewTally::default();

    for target in &targets {
        if !has_changes_to_review(target, multi, sink) {
            continue;
        }

        // Always announced, single project or not: the frontend needs the path
        // to aim the follow-up fix at the project the review was actually about.
        sink.event(Event::ProjectStarted {
            name: target.label.clone(),
            path: target.dir.display().to_string(),
        });

        let own_config = project_config(target, sink);
        let config = own_config.as_ref().unwrap_or(session_config);

        match orchestrator::review_only(config, reviewer, &target.dir, cmd.task.as_deref(), sink) {
            Ok(result) => {
                tally.reviewed += 1;
                if result.success {
                    tally.approved += 1;
                }
            }
            Err(e) => {
                let message = format!("{} — {:#}", target.label, e);
                // Warn rather than Error: later projects are still coming, and
                // an Error would end the run in the UI.
                if multi {
                    sink.event(Event::Warn { text: message.clone() });
                }
                tally.failures.push(if multi { message } else { format!("{:#}", e) });
            }
        }
    }

    emit_review_outcome(&targets, &tally, sink);
}

/// True when the project is a git repo with something uncommitted. Skips are
/// announced only in multi-project runs, where the reason is not obvious.
fn has_changes_to_review(target: &ReviewTarget, multi: bool, sink: &dyn Sink) -> bool {
    if !git::is_git_repo(&target.dir) {
        if multi {
            sink.event(Event::Warn {
                text: format!("{} — not a git repository, skipped", target.label),
            });
        }
        return false;
    }

    match git::git_diff(&target.dir) {
        Ok(diff) if diff.trim().is_empty() => {
            if multi {
                sink.event(Event::Info {
                    text: format!("{} — no uncommitted changes, skipped", target.label),
                });
            }
            false
        }
        Ok(_) => true,
        Err(e) => {
            sink.event(Event::Warn {
                text: format!("{} — could not read changes: {:#}", target.label, e),
            });
            false
        }
    }
}

fn emit_review_outcome(targets: &[ReviewTarget], tally: &ReviewTally, sink: &dyn Sink) {
    if tally.reviewed == 0 {
        let message = if tally.failures.is_empty() {
            nothing_to_review_message(targets)
        } else {
            tally.failures.join("; ")
        };
        sink.event(Event::Error { message });
        return;
    }

    sink.event(Event::TaskDone {
        success: tally.approved == tally.reviewed && tally.failures.is_empty(),
        rounds: tally.reviewed,
        message: review_summary(tally),
    });
}

fn nothing_to_review_message(targets: &[ReviewTarget]) -> String {
    if targets.len() == 1 {
        return "no uncommitted changes to review".to_string();
    }
    let names: Vec<&str> = targets.iter().map(|t| t.label.as_str()).collect();
    format!("no uncommitted changes to review in any workspace project ({})", names.join(", "))
}

fn review_summary(tally: &ReviewTally) -> String {
    if tally.reviewed == 1 && tally.failures.is_empty() {
        return if tally.approved == 1 { "approved".into() } else { "changes requested by AI".into() };
    }
    let mut summary = format!("{}/{} projects approved", tally.approved, tally.reviewed);
    if !tally.failures.is_empty() {
        summary.push_str(&format!(", {} failed", tally.failures.len()));
    }
    summary
}

/// Routes stdin lines: `answer` commands unblock a pending ask; everything
/// else queues for the main loop. EOF requests a clean shutdown.
fn spawn_stdin_reader(cmd_tx: Sender<Command>, ans_tx: Sender<String>, sink: Arc<JsonSink>) {
    std::thread::spawn(move || {
        let stdin = std::io::stdin();
        for line in stdin.lock().lines() {
            let Ok(line) = line else { break };
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            match serde_json::from_str::<Command>(line) {
                Ok(cmd) if cmd.cmd == "answer" => {
                    let _ = cmd.id; // single outstanding ask; id kept for protocol clarity
                    let _ = ans_tx.send(cmd.value.unwrap_or_default());
                }
                Ok(cmd) => {
                    if cmd_tx.send(cmd).is_err() {
                        break;
                    }
                }
                Err(e) => {
                    sink.event(Event::Error { message: format!("invalid command JSON: {}", e) });
                }
            }
        }
        let _ = cmd_tx.send(Command { cmd: "quit".into(), ..Default::default() });
    });
}

fn load_images(paths: &[String]) -> Result<Vec<ImageInput>> {
    paths
        .iter()
        .map(|p| ImageInput::load(std::path::PathBuf::from(p)))
        .collect()
}

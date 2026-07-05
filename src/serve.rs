//! `dt serve` — JSON-lines protocol for GUI frontends (VS Code extension).
//!
//! Commands arrive on stdin, one JSON object per line:
//!   {"cmd":"task","task":"...","auto":true,"images":["/path.png"]}
//!   {"cmd":"plan","task":"..."}
//!   {"cmd":"review","task":"optional context"}
//!   {"cmd":"answer","id":3,"value":"y"}        // reply to an "ask" event
//!   {"cmd":"ping"} / {"cmd":"quit"}
//!
//! Events stream to stdout as JSON lines (see `events::Event`); stderr is
//! free-form logging. Adapters are constructed once per serve process, so
//! both models keep their context across tasks — same as the terminal REPL.

use crate::adapters::ImageInput;
use crate::cli;
use crate::events::{AskKind, Event, Sink};
use crate::git;
use crate::orchestrator::{self, TaskOptions};
use anyhow::Result;
use serde::Deserialize;
use std::io::{BufRead, Write};
use std::path::Path;
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

pub fn run(dir: &Path, writer_name: &str) -> Result<()> {
    if !git::is_git_repo(dir) {
        anyhow::bail!("not a git repository — run `git init` first");
    }

    let (ans_tx, ans_rx) = mpsc::channel::<String>();
    let (cmd_tx, cmd_rx) = mpsc::channel::<Command>();

    let sink: Arc<JsonSink> = Arc::new(JsonSink::new(ans_rx));
    spawn_stdin_reader(cmd_tx, ans_tx, sink.clone());

    let setup = cli::setup_task(dir, writer_name, &[], false, sink.clone())?;
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

                let opts = TaskOptions {
                    config: &config,
                    task,
                    images: &images,
                    repo_dir: dir,
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
            "review" => {
                match orchestrator::review_only(
                    &config, reviewer.as_mut(), dir, cmd.task.as_deref(), sink.as_ref(),
                ) {
                    Ok(r) => sink.event(Event::TaskDone {
                        success: r.success,
                        rounds: r.rounds,
                        message: r.message,
                    }),
                    Err(e) => sink.event(Event::Error { message: format!("{:#}", e) }),
                }
            }
            other => {
                sink.event(Event::Error { message: format!("unknown command '{}'", other) });
            }
        }
    }

    sink.event(Event::Bye);
    Ok(())
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

//! `dt serve` — JSON-lines protocol for GUI frontends (VS Code extension).
//!
//! Commands arrive on stdin, one JSON object per line:
//!   {"cmd":"workspace","projects":["/proj-a","/proj-b"]}
//!   {"cmd":"task","task":"...","auto":true,"dir":"/proj-a","images":["/p.png"]}
//!   {"cmd":"plan","task":"..."}
//!   {"cmd":"review","task":"optional context","dirs":["/proj-a","/proj-b"]}
//!   {"cmd":"answer","id":3,"value":"y"}        // reply to an "ask" event
//!   {"cmd":"ping"} / {"cmd":"quit"}
//!
//! `workspace` declares the projects the session spans; frontends resend it
//! whenever the set changes. It is a command rather than a startup flag so a
//! folder being added never costs a respawn, which would throw away both
//! models' accumulated context.
//!
//! Events stream to stdout as JSON lines (see `events::Event`); stderr is
//! free-form logging. Adapters are constructed once per serve process, so
//! both models keep their context across tasks — same as the terminal REPL.

use crate::adapters::{ImageInput, ModelAdapter};
use crate::cli;
use crate::config::Config;
use crate::events::{AskKind, Event, Sink};
use crate::git;
use crate::orchestrator::{self, Outcome, TaskOptions};
use anyhow::Result;
use serde::Deserialize;
use std::collections::HashMap;
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
    /// Projects a review covers, when it should differ from the session's
    /// workspace. Omitted or empty means the whole workspace, so one clean
    /// project never hides the changes sitting in another.
    #[serde(default)]
    dirs: Option<Vec<String>>,
    /// Project a task or plan writes to. Lets a fix land in the project the
    /// review was about; omitted means the session picks one (see
    /// `task_target`).
    #[serde(default)]
    dir: Option<String>,
    /// Projects the session spans, sent by the `workspace` command.
    #[serde(default)]
    projects: Option<Vec<String>>,
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

    // Until a frontend declares otherwise, the session spans the one project it
    // was started in — which is exactly how the plain CLI behaves.
    let mut workspace: Vec<PathBuf> = vec![dir.to_path_buf()];

    // Answers still waiting for a review, per project. A task that answers
    // instead of writing code leaves no diff, so the review command would have
    // nothing to judge unless the answer is kept here until it is reviewed.
    let mut pending_answers: HashMap<PathBuf, PendingAnswer> = HashMap::new();

    for cmd in cmd_rx {
        match cmd.cmd.as_str() {
            "ping" => sink.event(Event::Pong),
            "quit" => break,
            "workspace" => {
                workspace = workspace_projects(&cmd, dir);
                sink.event(Event::Info {
                    text: format!("workspace: {}", describe_projects(&workspace)),
                });
            }
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

                let target = match task_target(&cmd, &workspace, sink.as_ref()) {
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

                // Per project: a workspace folder keeps its own prompts, and a
                // task runs against the folder the frontend named.
                crate::prompt_sync::sync(&target.dir, sink.as_ref());

                let own_config = project_config(&target, sink.as_ref());
                let task_config = own_config.as_ref().unwrap_or(&config);

                // Both models follow the task to its project: the writer edits
                // there, and a CLI-backed reviewer reads the same checkout.
                // Without this the picker would only steer the diff, leaving
                // the models standing in whichever project serve started in.
                aim_adapters(&target.dir, &workspace, writer.as_mut(), reviewer.as_mut());

                let opts = TaskOptions {
                    config: task_config,
                    task,
                    images: &images,
                    repo_dir: &target.dir,
                    workspace: &workspace,
                    continue_session: false,
                    auto: cmd.auto.unwrap_or(false),
                    plan_first: cmd.cmd == "plan",
                    // The panel has a review button, so the loop never stops to
                    // ask: the user spends a reviewer call when they want one.
                    review_on_demand: true,
                };

                match orchestrator::run(&opts, writer.as_mut(), reviewer.as_mut(), sink.as_ref()) {
                    Ok(r) => {
                        // A reviewed run, or one that changed code, leaves
                        // nothing pending — the diff is what a later review
                        // should judge, not an answer from a task ago.
                        match r.answer {
                            Some(answer) => {
                                let pending =
                                    PendingAnswer { task: task.to_string(), answer };
                                pending_answers.insert(project_key(&target.dir), pending);
                            }
                            None => {
                                pending_answers.remove(&project_key(&target.dir));
                            }
                        }
                        sink.event(Event::TaskDone {
                            outcome: r.outcome,
                            rounds: r.rounds,
                            message: r.message,
                        });
                    }
                    Err(e) => sink.event(Event::Error { message: format!("{:#}", e) }),
                }
            }
            "review" => {
                run_review(
                    &config,
                    reviewer.as_mut(),
                    &workspace,
                    &cmd,
                    &pending_answers,
                    sink.as_ref(),
                );
            }
            other => {
                sink.event(Event::Error { message: format!("unknown command '{}'", other) });
            }
        }
    }

    sink.event(Event::Bye);
    Ok(())
}

/// One project a review covers: where it lives and how the UI labels it.
#[derive(Debug)]
struct ReviewTarget {
    dir: PathBuf,
    label: String,
}

impl ReviewTarget {
    fn new(dir: PathBuf) -> Self {
        let label = project_label(&dir);
        Self { dir, label }
    }
}

/// How a project is named in prompts to the user: its folder name, falling back
/// to the full path when there is no final component.
fn project_label(dir: &Path) -> String {
    dir.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| dir.display().to_string())
}

/// The workspace a `workspace` command declares. Blank entries are dropped and
/// repeats collapsed; an empty list falls back to the serve directory, so a
/// frontend can never leave the session with nowhere to run.
fn workspace_projects(cmd: &Command, default_dir: &Path) -> Vec<PathBuf> {
    let mut projects: Vec<PathBuf> = Vec::new();
    for path in cmd.projects.as_deref().unwrap_or_default() {
        let path = path.trim();
        if path.is_empty() {
            continue;
        }
        let dir = PathBuf::from(path);
        if !projects.contains(&dir) {
            projects.push(dir);
        }
    }
    if projects.is_empty() {
        projects.push(default_dir.to_path_buf());
    }
    projects
}

fn describe_projects(projects: &[PathBuf]) -> String {
    projects.iter().map(|dir| project_label(dir)).collect::<Vec<_>>().join(", ")
}

/// Every project the frontend asked for, falling back to the session's
/// workspace when a `review` command names none.
fn review_targets(cmd: &Command, workspace: &[PathBuf]) -> Vec<ReviewTarget> {
    match cmd.dirs.as_deref() {
        Some(dirs) if !dirs.is_empty() => {
            dirs.iter().map(|dir| ReviewTarget::new(PathBuf::from(dir))).collect()
        }
        _ => workspace.iter().cloned().map(ReviewTarget::new).collect(),
    }
}

/// The project a task writes to.
///
/// An explicit `dir` always wins. Otherwise the session picks the workspace's
/// only git checkout, and asks when there is more than one — deliberately never
/// defaulting to "the first folder", which is how a task ends up editing one
/// project while its diff, checks, and session log describe another.
fn task_target(
    cmd: &Command,
    workspace: &[PathBuf],
    sink: &dyn Sink,
) -> Result<ReviewTarget, String> {
    if let Some(requested) = cmd.dir.as_deref().map(str::trim).filter(|d| !d.is_empty()) {
        let target = ReviewTarget::new(PathBuf::from(requested));
        if !git::is_git_repo(&target.dir) {
            return Err(format!("'{}' is not a git repository — cannot run a task there", requested));
        }
        return Ok(target);
    }

    let candidates: Vec<ReviewTarget> = workspace
        .iter()
        .filter(|dir| git::is_git_repo(dir))
        .cloned()
        .map(ReviewTarget::new)
        .collect();

    match candidates.len() {
        0 => Err("no git repository in this workspace — cannot run a task".to_string()),
        1 => Ok(candidates.into_iter().next().expect("one candidate")),
        _ => choose_target(candidates, sink),
    }
}

/// Asks which project to write to, matching the reply against project names
/// first and full paths second.
fn choose_target(candidates: Vec<ReviewTarget>, sink: &dyn Sink) -> Result<ReviewTarget, String> {
    let names = candidates.iter().map(|t| t.label.as_str()).collect::<Vec<_>>().join(", ");
    let answer =
        sink.ask(AskKind::Text, &format!("which project should this task write to? ({})", names));
    let answer = answer.trim().to_string();

    if answer.is_empty() {
        return Err(format!(
            "no project chosen — name one of: {}, or pick one in the composer",
            names
        ));
    }

    // Paths compare as paths, not as text: `/w/api/` and `/w/api` are the same
    // project, and a pasted path very often carries the trailing separator.
    candidates
        .into_iter()
        .find(|t| t.label.eq_ignore_ascii_case(&answer) || t.dir.as_path() == Path::new(&answer))
        .ok_or_else(|| format!("'{}' is not a project in this workspace ({})", answer, names))
}

/// Points both models at the project a task or review is about, and offers the
/// rest of the workspace as readable context.
fn aim_adapters<'a>(
    target: &Path,
    workspace: &[PathBuf],
    writer: &'a mut dyn ModelAdapter,
    reviewer: &'a mut dyn ModelAdapter,
) {
    for adapter in [writer, reviewer] {
        adapter.set_working_dir(target);
        adapter.set_readable_dirs(workspace);
    }
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

/// An answer a task produced and no reviewer has judged yet, with the question
/// it answered — a review of an answer is only meaningful against its question.
struct PendingAnswer {
    task: String,
    answer: String,
}

/// Keys a pending answer by a path that compares equal however a frontend
/// spells it: `/w/api` and `/w/api/` are one project, and a lookup that missed
/// would silently review the diff instead of the answer waiting on it.
fn project_key(dir: &Path) -> PathBuf {
    dir.components().collect()
}

/// Outcome tally across the reviewed projects.
#[derive(Default)]
struct ReviewTally {
    reviewed: usize,
    approved: usize,
    failures: Vec<String>,
    /// Projects where the review was refused because the reviewer had nothing
    /// it could check. Kept apart from `reviewed`, which would otherwise report
    /// a review that never happened.
    unreviewable: Vec<String>,
}

/// Reviews every requested project in turn. Projects that are not git repos or
/// have nothing uncommitted are skipped rather than failing the run, so a clean
/// first folder never hides the changes sitting in a second one.
///
/// A project whose last task answered instead of writing code is reviewed as an
/// answer: the reviewer judges what the writer concluded, against the diff it
/// concluded it about. Reviewing that diff cold would judge the wrong thing.
fn run_review(
    session_config: &Config,
    reviewer: &mut dyn ModelAdapter,
    workspace: &[PathBuf],
    cmd: &Command,
    pending_answers: &HashMap<PathBuf, PendingAnswer>,
    sink: &dyn Sink,
) {
    let targets = review_targets(cmd, workspace);
    let multi = targets.len() > 1;
    let mut tally = ReviewTally::default();

    for target in &targets {
        let pending = pending_answers.get(&project_key(&target.dir));
        if pending.is_none() && !has_changes_to_review(target, multi, sink) {
            continue;
        }

        // Always announced, single project or not: the frontend needs the path
        // to aim the follow-up fix at the project the review was actually about.
        sink.event(Event::ProjectStarted {
            name: target.label.clone(),
            path: target.dir.display().to_string(),
        });

        crate::prompt_sync::sync(&target.dir, sink);

        // A CLI-backed reviewer reads the checkout it is judging, so it moves
        // with the loop instead of staying where serve started.
        reviewer.set_working_dir(&target.dir);
        reviewer.set_readable_dirs(workspace);

        let own_config = project_config(target, sink);
        let config = own_config.as_ref().unwrap_or(session_config);

        let review = match pending {
            Some(p) => orchestrator::answer_review_only(
                reviewer,
                &target.dir,
                &p.task,
                &p.answer,
                cmd.task.as_deref(),
                sink,
            ),
            None => orchestrator::review_only(config, reviewer, &target.dir, cmd.task.as_deref(), sink),
        };

        match review {
            // A refused review is not a review: the reason was already warned
            // about, and counting it would let the summary claim otherwise.
            Ok(result) if result.outcome == Outcome::Unreviewed => {
                tally.unreviewable.push(if multi {
                    format!("{} — {}", target.label, result.message)
                } else {
                    result.message
                });
            }
            Ok(result) => {
                tally.reviewed += 1;
                if result.outcome == Outcome::Approved {
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
        // Refusing a review that could not check anything is neither a failure
        // nor "nothing to review" — there was something, and it was declined.
        if tally.failures.is_empty() && !tally.unreviewable.is_empty() {
            sink.event(Event::TaskDone {
                outcome: Outcome::Unreviewed,
                rounds: 0,
                message: tally.unreviewable.join("; "),
            });
            return;
        }
        let message = if tally.failures.is_empty() {
            nothing_to_review_message(targets)
        } else {
            tally.failures.join("; ")
        };
        sink.event(Event::Error { message });
        return;
    }

    let all_approved = tally.approved == tally.reviewed && tally.failures.is_empty();
    sink.event(Event::TaskDone {
        outcome: if all_approved { Outcome::Approved } else { Outcome::Stopped },
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
    if tally.reviewed == 1 && tally.failures.is_empty() && tally.unreviewable.is_empty() {
        return if tally.approved == 1 { "approved".into() } else { "changes requested by AI".into() };
    }
    let mut summary = format!("{}/{} projects approved", tally.approved, tally.reviewed);
    if !tally.failures.is_empty() {
        summary.push_str(&format!(", {} failed", tally.failures.len()));
    }
    if !tally.unreviewable.is_empty() {
        summary.push_str(&format!(", {} unreviewable", tally.unreviewable.len()));
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

#[cfg(test)]
mod tests {
    use super::*;

    fn command(projects: Option<Vec<&str>>, dirs: Option<Vec<&str>>) -> Command {
        let owned = |v: Vec<&str>| v.into_iter().map(str::to_string).collect();
        Command {
            cmd: "workspace".into(),
            projects: projects.map(owned),
            dirs: dirs.map(owned),
            ..Default::default()
        }
    }

    fn targets(dirs: &[&str]) -> Vec<ReviewTarget> {
        dirs.iter().map(|d| ReviewTarget::new(PathBuf::from(d))).collect()
    }

    /// Answers whatever it was built with, so the choice logic can be exercised
    /// without a frontend on the other end.
    struct StubSink(&'static str);

    impl Sink for StubSink {
        fn event(&self, _event: Event) {}
        fn ask(&self, _kind: AskKind, _question: &str) -> String {
            self.0.to_string()
        }
    }

    #[test]
    fn a_workspace_drops_blanks_and_repeats() {
        let cmd = command(Some(vec!["/a", "", "  ", "/b", "/a"]), None);
        assert_eq!(
            workspace_projects(&cmd, Path::new("/fallback")),
            vec![PathBuf::from("/a"), PathBuf::from("/b")]
        );
    }

    /// A frontend must not be able to leave the session with nowhere to run.
    #[test]
    fn an_empty_workspace_falls_back_to_the_serve_directory() {
        let cmd = command(Some(vec![" "]), None);
        assert_eq!(workspace_projects(&cmd, Path::new("/fallback")), vec![PathBuf::from("/fallback")]);
    }

    #[test]
    fn a_review_without_dirs_covers_the_whole_workspace() {
        let workspace = vec![PathBuf::from("/a"), PathBuf::from("/b")];
        let found = review_targets(&command(None, None), &workspace);
        assert_eq!(found.iter().map(|t| t.dir.clone()).collect::<Vec<_>>(), workspace);
    }

    #[test]
    fn a_review_with_dirs_covers_exactly_those() {
        let workspace = vec![PathBuf::from("/a"), PathBuf::from("/b")];
        let found = review_targets(&command(None, Some(vec!["/b"])), &workspace);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].dir, PathBuf::from("/b"));
    }

    #[test]
    fn a_project_can_be_chosen_by_name() {
        let chosen = choose_target(targets(&["/w/api", "/w/web"]), &StubSink("web")).expect("chosen");
        assert_eq!(chosen.dir, PathBuf::from("/w/web"));
    }

    #[test]
    fn a_project_can_be_chosen_by_path() {
        let chosen =
            choose_target(targets(&["/w/api", "/w/web"]), &StubSink("/w/api")).expect("chosen");
        assert_eq!(chosen.dir, PathBuf::from("/w/api"));
    }

    /// A pasted path usually carries the trailing separator, and it names the
    /// same project either way.
    /// The task command and the review command name the same project through
    /// different fields, so the pending answer must survive a path spelled with
    /// a trailing separator — otherwise the button reviews the wrong thing.
    #[test]
    fn a_pending_answer_is_found_however_the_path_is_spelled() {
        let mut pending = HashMap::new();
        pending.insert(
            project_key(Path::new("/w/api")),
            PendingAnswer { task: "t".into(), answer: "a".into() },
        );
        assert!(pending.contains_key(&project_key(Path::new("/w/api/"))));
        assert!(!pending.contains_key(&project_key(Path::new("/w/web"))));
    }

    #[test]
    fn a_pasted_path_matches_despite_a_trailing_separator() {
        let chosen =
            choose_target(targets(&["/w/api", "/w/web"]), &StubSink("/w/web/")).expect("chosen");
        assert_eq!(chosen.dir, PathBuf::from("/w/web"));
    }

    /// Guessing on an unrecognised answer is how a task ends up writing to the
    /// wrong repo, so the run stops instead.
    #[test]
    fn an_unrecognised_answer_names_the_real_projects() {
        let err = choose_target(targets(&["/w/api", "/w/web"]), &StubSink("mobile")).unwrap_err();
        assert!(err.contains("mobile"), "{}", err);
        assert!(err.contains("api") && err.contains("web"), "{}", err);
    }

    #[test]
    fn no_answer_is_not_a_choice() {
        let err = choose_target(targets(&["/w/api", "/w/web"]), &StubSink("  ")).unwrap_err();
        assert!(err.contains("no project chosen"), "{}", err);
    }
}

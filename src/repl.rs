//! Interactive session: launch bare `dt` and fire tasks at the duet without
//! leaving the CLI. Both adapters live for the whole session, so Claude keeps
//! its resumed CLI session and Gemini keeps its history across tasks.

use crate::adapters::ModelAdapter;
use crate::config::Config;
use crate::orchestrator::{self, OrchestratorResult, TaskOptions};
use crate::ui;
use anyhow::Result;
use std::path::Path;

pub fn run(
    dir: &Path,
    config: &Config,
    writer: &mut dyn ModelAdapter,
    reviewer: &mut dyn ModelAdapter,
    verbose: bool,
    auto_start: bool,
) -> Result<()> {
    let mut auto = auto_start;
    ui::session_banner(writer.name(), reviewer.name(), auto);

    while let Some(line) = ui::read_line("dt ❯") {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        match line {
            "/quit" | "/exit" | "/q" => break,
            "/help" | "/h" => {
                ui::session_help();
                continue;
            }
            "/auto" => {
                auto = !auto;
                ui::info(if auto {
                    "auto mode on — rounds run without per-round prompts"
                } else {
                    "auto mode off — you approve each round"
                });
                continue;
            }
            _ => {}
        }

        let outcome = if let Some(rest) = line.strip_prefix("/review") {
            let task = rest.trim();
            let task = if task.is_empty() { None } else { Some(task) };
            orchestrator::review_only(config, reviewer, dir, task, verbose)
        } else if let Some(task) = line.strip_prefix("/plan ") {
            let spec = TaskSpec { task: task.trim(), plan_first: true };
            run_task(dir, config, writer, reviewer, &spec, verbose, auto)
        } else if line.starts_with('/') {
            ui::warn("unknown command — type /help");
            continue;
        } else {
            let spec = TaskSpec { task: line, plan_first: false };
            run_task(dir, config, writer, reviewer, &spec, verbose, auto)
        };

        match outcome {
            Ok(result) => report(&result, writer.name(), reviewer.name()),
            Err(e) => ui::warn(&format!("task failed: {:#}", e)),
        }
    }

    ui::stopped("Session ended.");
    Ok(())
}

struct TaskSpec<'a> {
    task: &'a str,
    plan_first: bool,
}

fn run_task(
    dir: &Path,
    config: &Config,
    writer: &mut dyn ModelAdapter,
    reviewer: &mut dyn ModelAdapter,
    spec: &TaskSpec,
    verbose: bool,
    auto: bool,
) -> Result<OrchestratorResult> {
    let opts = TaskOptions {
        config,
        task: spec.task,
        images: &[],
        repo_dir: dir,
        continue_session: false,
        verbose,
        auto,
        plan_first: spec.plan_first,
    };
    orchestrator::run(&opts, writer, reviewer)
}

fn report(result: &OrchestratorResult, writer: &str, reviewer: &str) {
    ui::final_line(result.success, result.rounds, writer, reviewer, &result.message);
}

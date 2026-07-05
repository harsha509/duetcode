//! Interactive session: launch bare `dt` and fire tasks at the duet without
//! leaving the CLI. Both adapters live for the whole session, so Claude keeps
//! its resumed CLI session and Gemini keeps its history across tasks.

use crate::adapters::{ImageInput, ModelAdapter};
use crate::config::Config;
use crate::events::Sink;
use crate::orchestrator::{self, OrchestratorResult, TaskOptions};
use crate::ui;
use anyhow::Result;
use std::path::{Path, PathBuf};

pub fn run(
    dir: &Path,
    config: &Config,
    writer: &mut dyn ModelAdapter,
    reviewer: &mut dyn ModelAdapter,
    auto_start: bool,
    sink: &dyn Sink,
) -> Result<()> {
    let mut auto = auto_start;
    let mut staged_images: Vec<ImageInput> = Vec::new();
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
            "/paste" | "/p" => {
                paste_image(&mut staged_images);
                continue;
            }
            _ => {}
        }

        if let Some(rest) = line.strip_prefix("/image") {
            stage_image(rest.trim(), &mut staged_images);
            continue;
        }

        let outcome = if let Some(rest) = line.strip_prefix("/review") {
            let task = rest.trim();
            let task = if task.is_empty() { None } else { Some(task) };
            orchestrator::review_only(config, reviewer, dir, task, sink)
        } else if let Some(task) = line.strip_prefix("/plan ") {
            let images = std::mem::take(&mut staged_images);
            let spec = TaskSpec { task: task.trim(), plan_first: true, images: &images };
            run_task(dir, config, writer, reviewer, &spec, auto, sink)
        } else if line.starts_with('/') {
            ui::warn("unknown command — type /help");
            continue;
        } else {
            let images = std::mem::take(&mut staged_images);
            let spec = TaskSpec { task: line, plan_first: false, images: &images };
            run_task(dir, config, writer, reviewer, &spec, auto, sink)
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
    images: &'a [ImageInput],
}

fn run_task(
    dir: &Path,
    config: &Config,
    writer: &mut dyn ModelAdapter,
    reviewer: &mut dyn ModelAdapter,
    spec: &TaskSpec,
    auto: bool,
    sink: &dyn Sink,
) -> Result<OrchestratorResult> {
    let opts = TaskOptions {
        config,
        task: spec.task,
        images: spec.images,
        repo_dir: dir,
        continue_session: false,
        auto,
        plan_first: spec.plan_first,
    };
    orchestrator::run(&opts, writer, reviewer, sink)
}

fn stage_image(arg: &str, staged: &mut Vec<ImageInput>) {
    if arg.is_empty() {
        if staged.is_empty() {
            ui::info("no images staged — use /image <path> to attach one to the next task");
        } else {
            ui::info(&format!("{} image(s) staged for the next task", staged.len()));
        }
        return;
    }

    match ImageInput::load(expand_path(arg)) {
        Ok(img) => {
            ui::info(&format!(
                "staged {} ({} KB) — sent with the next task",
                arg,
                img.data.len() / 1024
            ));
            staged.push(img);
        }
        Err(e) => ui::warn(&format!("{:#}", e)),
    }
}

fn paste_image(staged: &mut Vec<ImageInput>) {
    match read_clipboard_png() {
        Ok(data) => {
            ui::info(&format!(
                "pasted image from clipboard ({} KB) — sent with the next task",
                data.len() / 1024
            ));
            staged.push(ImageInput { media_type: "image/png".to_string(), data });
        }
        Err(e) => ui::warn(&format!("{:#}", e)),
    }
}

/// Read an image off the OS clipboard and encode it as PNG.
fn read_clipboard_png() -> Result<Vec<u8>> {
    let mut clipboard = arboard::Clipboard::new()
        .map_err(|e| anyhow::anyhow!("clipboard unavailable: {}", e))?;

    let img = clipboard.get_image().map_err(|e| match e {
        arboard::Error::ContentNotAvailable => anyhow::anyhow!(
            "no image on the clipboard — copy a screenshot first \
             (Cmd+Ctrl+Shift+4 on macOS captures straight to the clipboard)"
        ),
        other => anyhow::anyhow!("failed to read clipboard image: {}", other),
    })?;

    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, img.width as u32, img.height as u32);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder
            .write_header()
            .map_err(|e| anyhow::anyhow!("failed to encode clipboard image: {}", e))?;
        writer
            .write_image_data(&img.bytes)
            .map_err(|e| anyhow::anyhow!("failed to encode clipboard image: {}", e))?;
    }
    Ok(out)
}

/// Handle paths as terminals hand them over: drag-and-drop quoting,
/// backslash-escaped spaces, and `~/`.
fn expand_path(raw: &str) -> PathBuf {
    let cleaned = raw.trim_matches(|c| c == '"' || c == '\'').replace("\\ ", " ");
    if let Some(rest) = cleaned.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    PathBuf::from(cleaned)
}

fn report(result: &OrchestratorResult, writer: &str, reviewer: &str) {
    ui::final_line(result.success, result.rounds, writer, reviewer, &result.message);
}

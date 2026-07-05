//! Structured events emitted by the orchestrator and adapters.
//!
//! Every user-visible occurrence flows through a `Sink`: the terminal sink
//! renders events exactly like the classic CLI, while `dt serve` serializes
//! them as JSON lines for frontends (VS Code extension, future web UI).

use crate::adapters::UsageStats;
use crate::ui;
use serde::Serialize;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AskKind {
    YesNo,
    Text,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    Ready { writer: String, reviewer: String, version: String },
    TaskStarted { task: String, writer: String, reviewer: String, mode: String, max_rounds: usize },
    RoundStarted { round: usize, budget: usize },
    Section { title: String },
    Working { actor: String, action: String },
    Info { text: String },
    Warn { text: String },
    Blocker { text: String },
    Success { text: String },
    Stopped { text: String },
    Changes { stat: String },
    Check { name: String, passed: bool },
    Verdict { approved: bool, blockers: Vec<String>, suggestions: Vec<String> },
    Usage { model: String, input_tokens: u64, output_tokens: u64, cost_usd: Option<f64> },
    CostSummary { calls: usize, input_tokens: u64, output_tokens: u64, cost_usd: Option<f64> },
    Response { model: String, text: String },
    StreamStart { model: String },
    StreamChunk { model: String, text: String },
    StreamEnd { model: String },
    Thinking { model: String },
    ToolAction { model: String, desc: String },
    Ask { id: u64, kind: AskKind, question: String },
    TaskDone { success: bool, rounds: usize, message: String },
    Error { message: String },
    Pong,
    Bye,
}

pub trait Sink: Send + Sync {
    fn event(&self, event: Event);

    /// Ask the user a question and block until an answer arrives.
    fn ask(&self, kind: AskKind, question: &str) -> String;
}

pub fn ask_yes_no(sink: &dyn Sink, question: &str) -> bool {
    matches!(
        sink.ask(AskKind::YesNo, question).trim().to_lowercase().as_str(),
        "y" | "yes"
    )
}

/// Renders events as the classic colored terminal output.
pub struct TerminalSink {
    verbose: bool,
    /// True while streamed chunks are mid-line, so the next non-chunk output
    /// knows to terminate the line first.
    mid_stream: AtomicBool,
}

impl TerminalSink {
    pub fn new(verbose: bool) -> Self {
        Self { verbose, mid_stream: AtomicBool::new(false) }
    }

    fn break_stream(&self) {
        if self.mid_stream.swap(false, Ordering::Relaxed) {
            eprintln!();
        }
    }
}

impl Sink for TerminalSink {
    fn event(&self, event: Event) {
        match event {
            Event::TaskStarted { task, writer, reviewer, mode, max_rounds } => {
                ui::banner(&task, &writer, &reviewer, &mode, max_rounds);
            }
            Event::RoundStarted { round, budget } => ui::round_header(round, budget),
            Event::Section { title } => ui::section(&title),
            Event::Working { actor, action } => ui::working(&actor, &action),
            Event::Info { text } => ui::info(&text),
            Event::Warn { text } => ui::warn(&text),
            Event::Blocker { text } => ui::blocker(&text),
            Event::Success { text } => ui::success(&text),
            Event::Stopped { text } => ui::stopped(&text),
            Event::Changes { stat } => ui::changes(&stat),
            Event::Check { name, passed } => ui::check_result(&name, passed),
            Event::Verdict { approved, blockers, suggestions } => {
                ui::verdict(approved, &blockers, &suggestions);
            }
            Event::Usage { model, input_tokens, output_tokens, cost_usd } => {
                ui::usage(&UsageStats { model, input_tokens, output_tokens, cost_usd });
            }
            Event::CostSummary { calls, input_tokens, output_tokens, cost_usd } => {
                ui::cost_summary(calls, input_tokens, output_tokens, cost_usd);
            }
            Event::Response { model, text } => ui::response_block(&model, &text, self.verbose),
            Event::StreamStart { model } => ui::stream_header(&model),
            Event::StreamChunk { text, .. } => {
                eprint!("{}", text);
                let _ = std::io::stderr().lock().flush();
                self.mid_stream.store(true, Ordering::Relaxed);
            }
            Event::StreamEnd { .. } => self.break_stream(),
            Event::Thinking { .. } => {
                self.break_stream();
                ui::thinking();
            }
            Event::ToolAction { desc, .. } => {
                self.break_stream();
                ui::tool_action(&desc);
            }
            // Protocol-only events; nothing to render in a terminal.
            Event::Ready { .. } | Event::Ask { .. } | Event::TaskDone { .. }
            | Event::Error { .. } | Event::Pong | Event::Bye => {}
        }
    }

    fn ask(&self, kind: AskKind, question: &str) -> String {
        self.break_stream();
        match kind {
            AskKind::YesNo => {
                if ui::ask_yes_no(question) { "y".into() } else { "n".into() }
            }
            AskKind::Text => ui::ask_text(question),
        }
    }
}

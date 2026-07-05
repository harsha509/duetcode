//! All terminal output lives here so the rest of the code stays free of
//! formatting concerns and the CLI has one consistent voice.

use crate::adapters::UsageStats;
use crate::policy::{ReviewVerdict, Verdict};
use colored::Colorize;
use std::io::Write;

const WIDTH: usize = 60;

pub fn separator() -> String {
    "─".repeat(WIDTH)
}

pub fn banner(task: &str, writer: &str, reviewer: &str, mode: &str, max_rounds: usize) {
    println!("\n{} {}", "Task:".cyan().bold(), task);
    println!(
        "{} {}  ·  {} {}  ·  {} {}  ·  {} {}",
        "writer:".dimmed(),
        writer.green().bold(),
        "reviewer:".dimmed(),
        reviewer.magenta().bold(),
        "mode:".dimmed(),
        mode.yellow(),
        "max rounds:".dimmed(),
        max_rounds.to_string().yellow(),
    );
}

pub fn round_header(round: usize, budget: usize) {
    println!("\n{}", format!("━━━ Round {}/{} ━━━", round, budget).cyan().bold());
}

pub fn section(title: &str) {
    println!("\n{}", format!("━━━ {} ━━━", title).cyan().bold());
}

/// Status line showing which model is doing what right now.
pub fn working(actor: &str, action: &str) {
    println!("  {} {} {}", "●".cyan(), actor.bold(), action.dimmed());
}

pub fn info(msg: &str) {
    println!("  {} {}", "ℹ".cyan(), msg);
}

pub fn warn(msg: &str) {
    println!("  {} {}", "⚠".yellow(), msg.yellow());
}

pub fn blocker(msg: &str) {
    println!("    {} {}", "-".red(), msg.red());
}

pub fn success(msg: &str) {
    println!("\n{}", msg.green().bold());
}

pub fn stopped(msg: &str) {
    println!("\n{}", msg.yellow());
}

pub fn check_result(name: &str, passed: bool) {
    let icon = if passed { "PASS".green() } else { "FAIL".red() };
    println!("    [{}] {}", icon, name);
}

pub fn changes(stat: &str) {
    println!("  {} Changes:\n{}", "~~".blue(), indent(stat, "     "));
}

pub fn verdict(v: &ReviewVerdict) {
    let label = match v.verdict {
        Verdict::Approved => "APPROVED".green().bold(),
        Verdict::ChangesRequested => "CHANGES REQUESTED".red().bold(),
    };
    println!("  {} AI Verdict: {}", "⚖".cyan(), label);

    if !v.blockers.is_empty() {
        println!("  {} Blockers:", "✗".red());
        for b in &v.blockers {
            blocker(b);
        }
    }
    if !v.suggestions.is_empty() {
        println!("  {} Suggestions:", "~".yellow());
        for s in &v.suggestions {
            println!("    - {}", s.yellow());
        }
    }
}

pub fn usage(u: &UsageStats) {
    if u.input_tokens == 0 && u.output_tokens == 0 {
        return;
    }
    let cost = u.cost_usd.map(|c| format!(" | ${:.6}", c)).unwrap_or_default();
    eprintln!(
        "  {} {} — {}in / {}out{}",
        "$".yellow(),
        u.model,
        u.input_tokens,
        u.output_tokens,
        cost,
    );
}

pub fn cost_summary(entries: &[UsageStats]) {
    if entries.is_empty() {
        return;
    }
    let total_in: u64 = entries.iter().map(|u| u.input_tokens).sum();
    let total_out: u64 = entries.iter().map(|u| u.output_tokens).sum();
    let total_cost: f64 = entries.iter().filter_map(|u| u.cost_usd).sum();
    let has_cost = entries.iter().any(|u| u.cost_usd.is_some());

    println!("\n{}", "━━━ Cost Summary ━━━".cyan().bold());
    println!(
        "  {} calls | {}in + {}out = {} tokens",
        entries.len(),
        total_in,
        total_out,
        total_in + total_out,
    );
    if has_cost {
        println!("  Total cost: {}", format!("${:.6}", total_cost).yellow().bold());
    }
}

pub fn response_block(model: &str, text: &str, verbose: bool) {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return;
    }

    let sep = separator();
    println!("\n  {}", sep.dimmed());
    println!("  {}", format!("{}:", model).cyan().bold());
    println!("  {}", sep.dimmed());

    let mut skin = termimad::MadSkin::default();
    skin.set_headers_fg(termimad::crossterm::style::Color::Cyan);
    skin.bold.set_fg(termimad::crossterm::style::Color::Yellow);
    skin.italic.set_fg(termimad::crossterm::style::Color::Magenta);

    let total_lines = trimmed.lines().count();
    let shown = if verbose || total_lines <= 40 { total_lines } else { 40 };
    let body: String = trimmed
        .lines()
        .take(shown)
        .map(|l| format!("  {}", l))
        .collect::<Vec<_>>()
        .join("\n");
    skin.print_text(&body);

    if shown < total_lines {
        println!(
            "\n  {}",
            format!("... +{} more lines (use -v to see all)", total_lines - shown).dimmed()
        );
    }
    println!("  {}\n", sep.dimmed());
}

/// Header printed by adapters when a model starts streaming its answer.
pub fn stream_header(model: &str) {
    eprintln!("\n  {}", separator().dimmed());
    eprintln!("  {}", format!("{}:", model).cyan().bold());
    eprintln!("  {}", separator().dimmed());
}

pub fn stream_footer() {
    eprintln!("  {}", separator().dimmed());
}

pub fn ask_yes_no(question: &str) -> bool {
    let answer = ask(&format!("  {} {} (y/n): ", "?".cyan().bold(), question)).to_lowercase();
    answer == "y" || answer == "yes"
}

/// Free-text prompt; preserves the user's casing.
pub fn ask_text(question: &str) -> String {
    ask(&format!("  {} {}: ", "?".cyan().bold(), question))
}

fn ask(prompt: &str) -> String {
    eprint!("{}", prompt);
    let _ = std::io::stderr().flush();
    let mut input = String::new();
    let _ = std::io::stdin().read_line(&mut input);
    input.trim().to_string()
}

pub fn session_banner(writer: &str, reviewer: &str, auto: bool) {
    println!("\n{}", "dt — duet session".cyan().bold());
    println!(
        "{} {}  ·  {} {}  ·  {} {}",
        "writer:".dimmed(),
        writer.green().bold(),
        "reviewer:".dimmed(),
        reviewer.magenta().bold(),
        "auto:".dimmed(),
        if auto { "on".yellow() } else { "off".yellow() },
    );
    println!(
        "{}",
        "type a task to start the duet · /help for commands · /quit to leave".dimmed()
    );
}

pub fn session_help() {
    println!("  {}", "commands:".bold());
    println!("    {}           toggle auto mode (loop without per-round prompts)", "/auto".green());
    println!("    {} <path>   attach a screenshot/image to the next task (repeatable)", "/image".green());
    println!("    {}          attach the image on the clipboard to the next task", "/paste".green());
    println!("    {} [task]  review uncommitted changes", "/review".green());
    println!("    {} <task>   plan first, then execute", "/plan".green());
    println!("    {}           show this help", "/help".green());
    println!("    {}           leave the session", "/quit".green());
    println!("  anything else is sent to the duet as a coding task");
}

/// Read one line for the session prompt. Returns None on EOF (ctrl-D).
pub fn read_line(prompt: &str) -> Option<String> {
    eprint!("\n{} ", prompt.cyan().bold());
    let _ = std::io::stderr().flush();
    let mut input = String::new();
    match std::io::stdin().read_line(&mut input) {
        Ok(0) | Err(_) => None,
        Ok(_) => Some(input),
    }
}

pub fn final_line(success: bool, rounds: usize, writer: &str, reviewer: &str, message: &str) {
    let status = if success { "SUCCESS".green().bold() } else { "STOPPED".red().bold() };
    println!("\n{} rounds={}, writer={}, reviewer={}", status, rounds, writer, reviewer);
    println!("{}\n", message);
}

pub fn indent(text: &str, prefix: &str) -> String {
    text.lines()
        .map(|l| format!("{}{}", prefix, l))
        .collect::<Vec<_>>()
        .join("\n")
}

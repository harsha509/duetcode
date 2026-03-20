use crate::adapters::{ImageInput, ModelAdapter};
use crate::checks;
use crate::config::Config;
use crate::git;
use crate::logs::SessionLog;
use crate::policy::{self, PolicyResult, Verdict};
use crate::prompts;
use anyhow::{Context, Result};
use colored::Colorize;
use std::path::Path;

pub struct OrchestratorResult {
    pub success: bool,
    pub rounds: usize,
    pub message: String,
}

pub fn run(
    config: &Config,
    task: &str,
    writer: &dyn ModelAdapter,
    reviewer: &dyn ModelAdapter,
    images: &[ImageInput],
    repo_dir: &Path,
    verbose: bool,
) -> Result<OrchestratorResult> {
    println!(
        "\n{} {}",
        "Task:".cyan().bold(),
        task
    );
    println!(
        "{} writer={}, reviewer={}",
        "Models:".cyan().bold(),
        writer.name().green(),
        reviewer.name().green()
    );
    println!(
        "{} {}\n",
        "Max rounds:".cyan().bold(),
        config.policy.max_rounds
    );

    let impl_template = load_prompt_template(&config.prompts.implementation, prompts::DEFAULT_IMPLEMENT_TEMPLATE, repo_dir)?;
    let review_template = load_prompt_template(&config.prompts.review, prompts::DEFAULT_REVIEW_TEMPLATE, repo_dir)?;
    let fix_template = load_prompt_template(&config.prompts.fix, prompts::DEFAULT_FIX_TEMPLATE, repo_dir)?;

    let session_log = SessionLog::create(repo_dir, task)?;
    println!(
        "{} {}\n",
        "Logs:".cyan().bold(),
        session_log.dir.display()
    );

    let repo_context = build_repo_context(repo_dir)?;

    let mut last_verdict = None;
    let mut last_checks = Vec::new();
    let mut total_rounds = 0;

    for round in 1..=config.policy.max_rounds {
        total_rounds = round;
        println!(
            "{}",
            format!("━━━ Round {}/{} ━━━", round, config.policy.max_rounds)
                .cyan()
                .bold()
        );

        // Step 1: Call writer
        let writer_prompt = if round == 1 {
            prompts::build_implement_prompt(&impl_template, task, &repo_context)
        } else {
            let feedback = last_verdict
                .as_ref()
                .map(|v| policy::format_review_feedback(v))
                .unwrap_or_default();
            prompts::build_fix_prompt(&fix_template, task, &feedback)
        };

        println!("  {} Calling {}...", ">>".yellow(), writer.name());
        let writer_response = writer
            .generate(&writer_prompt, "", images)
            .with_context(|| format!("writer ({}) failed in round {}", writer.name(), round))?;

        session_log.write_writer_response(round, &writer_response)?;
        println!("  {} {} responded ({} chars)", "<<".green(), writer.name(), writer_response.len());
        if !writer.streams_output() {
            print_response(writer.name(), &writer_response, verbose);
        }

        // Step 2: Capture diff
        let diff = git::git_diff(repo_dir)?;
        session_log.write_diff(round, &diff)?;

        if diff.trim().is_empty() && !git::has_changes(repo_dir)? {
            println!("  {} No changes produced", "!!".red().bold());
            return Ok(OrchestratorResult {
                success: false,
                rounds: round,
                message: format!("{} produced no changes in round {}", writer.name(), round),
            });
        }

        let stat = git::git_diff_stat(repo_dir).unwrap_or_default();
        if !stat.trim().is_empty() {
            println!("  {} Changes:\n{}", "~~".blue(), indent(&stat, "     "));
        }

        // Step 3: Run checks
        println!("  {} Running checks...", ">>".yellow());
        let check_results = checks::run_checks(&config.checks, repo_dir);
        session_log.write_checks(round, &check_results)?;

        for cr in &check_results {
            let icon = if cr.passed { "PASS".green() } else { "FAIL".red() };
            println!("  [{}] {}", icon, cr.name);
        }

        let checks_summary = checks::format_check_results(&check_results);

        // Step 4: Call reviewer
        let review_prompt = prompts::build_review_prompt(
            &review_template,
            task,
            &diff,
            &checks_summary,
        );

        println!("  {} Calling {}...", ">>".yellow(), reviewer.name());
        let reviewer_response = reviewer
            .generate(&review_prompt, "", &[])
            .with_context(|| format!("reviewer ({}) failed in round {}", reviewer.name(), round))?;

        session_log.write_reviewer_response(round, &reviewer_response)?;
        println!("  {} {} responded ({} chars)", "<<".green(), reviewer.name(), reviewer_response.len());
        if !reviewer.streams_output() {
            print_response(reviewer.name(), &reviewer_response, verbose);
        }

        // Step 5: Parse verdict
        let verdict = policy::parse_verdict(&reviewer_response);
        print_verdict(&verdict);

        // Step 6: Evaluate policy
        let policy_result = policy::evaluate(&verdict, &check_results, round, &config.policy);

        last_verdict = Some(verdict.clone());
        last_checks = check_results;

        match policy_result {
            PolicyResult::Pass => {
                println!(
                    "\n{}\n",
                    "All checks passed and reviewer approved!".green().bold()
                );

                session_log.write_summary(
                    task,
                    writer.name(),
                    reviewer.name(),
                    round,
                    &verdict,
                    true,
                    true,
                )?;

                return Ok(OrchestratorResult {
                    success: true,
                    rounds: round,
                    message: "approved with all checks passing".to_string(),
                });
            }
            PolicyResult::Continue(reason) => {
                println!("  {} {}\n", "→".yellow().bold(), reason);
            }
            PolicyResult::Fail(reason) => {
                println!(
                    "\n{} {}\n",
                    "FAILED:".red().bold(),
                    reason
                );

                session_log.write_summary(
                    task,
                    writer.name(),
                    reviewer.name(),
                    round,
                    &verdict,
                    checks::all_passed(&last_checks),
                    false,
                )?;

                return Ok(OrchestratorResult {
                    success: false,
                    rounds: round,
                    message: reason,
                });
            }
        }
    }

    let final_verdict = last_verdict.unwrap_or_else(|| policy::ReviewVerdict {
        verdict: Verdict::ChangesRequested,
        blockers: vec!["no rounds completed".to_string()],
        suggestions: vec![],
        tests_to_add: vec![],
        raw: String::new(),
    });

    session_log.write_summary(
        task,
        writer.name(),
        reviewer.name(),
        total_rounds,
        &final_verdict,
        checks::all_passed(&last_checks),
        false,
    )?;

    Ok(OrchestratorResult {
        success: false,
        rounds: total_rounds,
        message: format!("exhausted {} rounds without full approval", config.policy.max_rounds),
    })
}

pub fn run_plan_flow(
    config: &Config,
    task: &str,
    writer: &dyn ModelAdapter,
    reviewer: &dyn ModelAdapter,
    images: &[ImageInput],
    repo_dir: &Path,
    verbose: bool,
) -> Result<OrchestratorResult> {
    println!(
        "\n{} {}",
        "Task:".cyan().bold(),
        task
    );
    println!(
        "{} writer={}, reviewer={}",
        "Models:".cyan().bold(),
        writer.name().green(),
        reviewer.name().green()
    );
    println!("{}\n", "Mode: plan".cyan().bold());

    let impl_template = load_prompt_template(&config.prompts.implementation, prompts::DEFAULT_IMPLEMENT_TEMPLATE, repo_dir)?;
    let review_template = load_prompt_template(&config.prompts.review, prompts::DEFAULT_REVIEW_TEMPLATE, repo_dir)?;
    let fix_template = load_prompt_template(&config.prompts.fix, prompts::DEFAULT_FIX_TEMPLATE, repo_dir)?;

    let session_log = SessionLog::create(repo_dir, task)?;
    let repo_context = build_repo_context(repo_dir)?;

    // ── Step 1: Claude creates a plan ──
    println!("{}", "━━━ Planning ━━━".cyan().bold());
    let plan_prompt = prompts::build_plan_prompt(
        prompts::DEFAULT_PLAN_TEMPLATE,
        task,
        &repo_context,
    );

    println!("  {} Asking {} to plan...", ">>".yellow(), writer.name());
    let plan_response = writer
        .generate(&plan_prompt, "", images)
        .with_context(|| format!("{} failed during planning", writer.name()))?;

    if !writer.streams_output() {
        print_response(writer.name(), &plan_response, verbose);
    }
    session_log.write_writer_response(0, &plan_response)?;

    // ── Step 2: Gemini reviews the plan ──
    let answer = ask_user(&format!(
        "  {} Review this plan with {}? (y/n): ",
        "?".cyan().bold(),
        reviewer.name()
    ));

    if answer != "y" && answer != "yes" {
        println!("\n{}", "Plan saved but not reviewed. Exiting.".yellow());
        return Ok(OrchestratorResult {
            success: false,
            rounds: 0,
            message: "plan created, user skipped review".to_string(),
        });
    }

    let plan_review_prompt = prompts::build_plan_review_prompt(
        prompts::DEFAULT_PLAN_REVIEW_TEMPLATE,
        task,
        &plan_response,
    );

    println!("  {} Asking {} to review plan...", ">>".yellow(), reviewer.name());
    let plan_review = reviewer
        .generate(&plan_review_prompt, "", &[])
        .with_context(|| format!("{} failed during plan review", reviewer.name()))?;

    if !reviewer.streams_output() {
        print_response(reviewer.name(), &plan_review, verbose);
    }
    session_log.write_reviewer_response(0, &plan_review)?;

    let plan_verdict = policy::parse_verdict(&plan_review);
    print_verdict(&plan_verdict);

    // ── Step 3: Ask user to execute ──
    let answer = ask_user(&format!(
        "  {} Execute this task? (y/n): ",
        "?".cyan().bold(),
    ));

    if answer != "y" && answer != "yes" {
        println!("\n{}", "Exiting without executing.".yellow());
        return Ok(OrchestratorResult {
            success: false,
            rounds: 0,
            message: "plan reviewed, user chose not to execute".to_string(),
        });
    }

    // ── Step 4: Claude implements with the plan as context ──
    let mut round = 0;
    let mut last_review_text = String::new();

    loop {
        round += 1;
        if round > config.policy.max_rounds {
            println!("\n{}", format!("Max rounds ({}) reached.", config.policy.max_rounds).red().bold());
            return Ok(OrchestratorResult {
                success: false,
                rounds: round - 1,
                message: "max rounds exceeded".to_string(),
            });
        }

        println!("\n{}", format!("━━━ Executing (round {}) ━━━", round).cyan().bold());

        let writer_prompt = if round == 1 {
            prompts::build_implement_with_plan_prompt(
                &impl_template, task, &repo_context, &plan_response,
            )
        } else {
            prompts::build_fix_prompt(&fix_template, task, &last_review_text)
        };

        println!("  {} Calling {}...", ">>".yellow(), writer.name());
        let writer_response = writer
            .generate(&writer_prompt, "", if round == 1 { images } else { &[] })
            .with_context(|| format!("{} failed in round {}", writer.name(), round))?;

        if !writer.streams_output() {
            print_response(writer.name(), &writer_response, verbose);
        }
        session_log.write_writer_response(round, &writer_response)?;

        // Show diff
        let diff = git::git_diff(repo_dir)?;
        session_log.write_diff(round, &diff)?;

        let stat = git::git_diff_stat(repo_dir).unwrap_or_default();
        if !stat.trim().is_empty() {
            println!("  {} Changes:\n{}", "~~".blue(), indent(&stat, "     "));
        }

        // ── Step 5: Ask user if they want to review ──
        let answer = ask_user(&format!(
            "  {} Review changes with {}? (y/n): ",
            "?".cyan().bold(),
            reviewer.name()
        ));

        if answer != "y" && answer != "yes" {
            println!("\n{}", "Task completed.".green().bold());
            return Ok(OrchestratorResult {
                success: true,
                rounds: round,
                message: "executed, user skipped review".to_string(),
            });
        }

        // Run checks
        println!("  {} Running checks...", ">>".yellow());
        let check_results = checks::run_checks(&config.checks, repo_dir);
        session_log.write_checks(round, &check_results)?;

        for cr in &check_results {
            let icon = if cr.passed { "PASS".green() } else { "FAIL".red() };
            println!("  [{}] {}", icon, cr.name);
        }

        let checks_summary = checks::format_check_results(&check_results);

        // Gemini reviews
        let review_prompt = prompts::build_review_prompt(
            &review_template,
            task,
            &diff,
            &checks_summary,
        );

        println!("  {} Calling {}...", ">>".yellow(), reviewer.name());
        let reviewer_response = reviewer
            .generate(&review_prompt, "", &[])
            .with_context(|| format!("{} failed in round {}", reviewer.name(), round))?;

        if !reviewer.streams_output() {
            print_response(reviewer.name(), &reviewer_response, verbose);
        }
        session_log.write_reviewer_response(round, &reviewer_response)?;
        last_review_text = reviewer_response.clone();

        let verdict = policy::parse_verdict(&reviewer_response);
        print_verdict(&verdict);

        if verdict.verdict == Verdict::Approved && checks::all_passed(&check_results) {
            println!("\n{}", "Task completed. Approved!".green().bold());
            return Ok(OrchestratorResult {
                success: true,
                rounds: round,
                message: "approved".to_string(),
            });
        }

        // Changes requested — ask user
        let answer = ask_user(&format!(
            "  {} Let {} fix the issues? (y/n): ",
            "?".cyan().bold(),
            writer.name()
        ));

        if answer != "y" && answer != "yes" {
            println!("\n{}", "Stopping. Review feedback saved in logs.".yellow());
            return Ok(OrchestratorResult {
                success: false,
                rounds: round,
                message: "user stopped after review".to_string(),
            });
        }
    }
}

pub fn review_only(
    config: &Config,
    reviewer: &dyn ModelAdapter,
    repo_dir: &Path,
) -> Result<OrchestratorResult> {
    let diff = git::git_diff(repo_dir)?;
    if diff.trim().is_empty() {
        anyhow::bail!("no uncommitted changes to review");
    }

    let review_template = load_prompt_template(
        &config.prompts.review,
        prompts::DEFAULT_REVIEW_TEMPLATE,
        repo_dir,
    )?;

    println!("{}", "Running checks...".cyan());
    let check_results = checks::run_checks(&config.checks, repo_dir);
    let checks_summary = checks::format_check_results(&check_results);

    let review_prompt = prompts::build_review_prompt(
        &review_template,
        "Review the current uncommitted changes",
        &diff,
        &checks_summary,
    );

    println!("Calling {}...", reviewer.name().green());
    let response = reviewer.generate(&review_prompt, "", &[])?;

    let verdict = policy::parse_verdict(&response);
    print_verdict(&verdict);

    let success = verdict.verdict == Verdict::Approved && checks::all_passed(&check_results);

    Ok(OrchestratorResult {
        success,
        rounds: 1,
        message: if success {
            "approved".to_string()
        } else {
            "changes requested or checks failed".to_string()
        },
    })
}

fn load_prompt_template(
    path: &std::path::PathBuf,
    default: &str,
    repo_dir: &Path,
) -> Result<String> {
    let full_path = repo_dir.join(path);
    if full_path.exists() {
        prompts::load_template(&full_path)
    } else {
        Ok(default.to_string())
    }
}

fn build_repo_context(dir: &Path) -> Result<String> {
    let branch = git::current_branch(dir).unwrap_or_else(|_| "unknown".to_string());
    let status = git::git_status(dir).unwrap_or_default();

    let mut context = format!("Branch: {}\n", branch);

    if !status.trim().is_empty() {
        context.push_str(&format!("Working tree status:\n{}\n", status));
    }

    Ok(context)
}

fn print_verdict(verdict: &policy::ReviewVerdict) {
    let verdict_str = match verdict.verdict {
        Verdict::Approved => "APPROVED".green().bold(),
        Verdict::ChangesRequested => "CHANGES REQUESTED".red().bold(),
    };
    println!("  {} Verdict: {}", "⚖".cyan(), verdict_str);

    if !verdict.blockers.is_empty() {
        println!("  {} Blockers:", "✗".red());
        for b in &verdict.blockers {
            println!("    - {}", b.red());
        }
    }

    if !verdict.suggestions.is_empty() {
        println!("  {} Suggestions:", "~".yellow());
        for s in &verdict.suggestions {
            println!("    - {}", s.yellow());
        }
    }

}

fn print_response(model: &str, response: &str, verbose: bool) {
    let trimmed = response.trim();
    if trimmed.is_empty() {
        return;
    }

    let separator = "─".repeat(60);
    println!("\n  {}", separator.dimmed());
    println!("  {}", format!("{}:", model).cyan().bold());
    println!("  {}", separator.dimmed());

    let lines: Vec<&str> = trimmed.lines().collect();

    if verbose || lines.len() <= 40 {
        for line in &lines {
            println!("  {}", line);
        }
    } else {
        for line in &lines[..40] {
            println!("  {}", line);
        }
        println!(
            "\n  {}",
            format!("... +{} more lines (use -v to see all)", lines.len() - 40).dimmed()
        );
    }

    println!("  {}\n", separator.dimmed());
}

fn ask_user(prompt: &str) -> String {
    use std::io::Write;
    eprint!("{}", prompt);
    std::io::stderr().flush().unwrap_or(());
    let mut input = String::new();
    std::io::stdin().read_line(&mut input).unwrap_or(0);
    input.trim().to_lowercase()
}

fn indent(text: &str, prefix: &str) -> String {
    text.lines()
        .map(|l| format!("{}{}", prefix, l))
        .collect::<Vec<_>>()
        .join("\n")
}

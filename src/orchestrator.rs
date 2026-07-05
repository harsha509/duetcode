use crate::adapters::{ImageInput, ModelAdapter, UsageStats};
use crate::checks;
use crate::config::Config;
use crate::git;
use crate::logs::{RunSummary, SessionLog};
use crate::policy::{self, ReviewVerdict, Verdict};
use crate::prompts;
use crate::ui;
use anyhow::{Context, Result};
use std::path::Path;

pub struct OrchestratorResult {
    pub success: bool,
    pub rounds: usize,
    pub message: String,
}

/// Everything a task run needs, bundled so call sites stay small.
pub struct TaskOptions<'a> {
    pub config: &'a Config,
    pub task: &'a str,
    pub images: &'a [ImageInput],
    pub repo_dir: &'a Path,
    pub continue_session: bool,
    pub verbose: bool,
    pub auto: bool,
    pub plan_first: bool,
}

// ── Internal types ──

struct Session {
    log: SessionLog,
    repo_context: String,
    impl_template: String,
    review_template: String,
    fix_template: String,
}

struct CostTracker {
    entries: Vec<UsageStats>,
}

impl CostTracker {
    fn new() -> Self {
        Self { entries: Vec::new() }
    }

    fn add(&mut self, usage: UsageStats) {
        ui::usage(&usage);
        self.entries.push(usage);
    }

    fn summary(&self) {
        ui::cost_summary(&self.entries);
    }
}

struct ReviewOutcome {
    verdict: ReviewVerdict,
    response: String,
    checks_passed: bool,
    checks_summary: String,
}

impl ReviewOutcome {
    fn approved(&self) -> bool {
        self.verdict.verdict == Verdict::Approved && self.checks_passed
    }
}

struct ReviewInput<'a> {
    round: usize,
    diff: &'a str,
    writer_notes: &'a str,
    clarification: Option<&'a str>,
}

enum DiffOutcome {
    NoChanges,
    UserSkipped,
    Review(String),
}

/// Detects a deadlocked loop: the reviewer repeating the same blockers,
/// or the writer no longer changing the code in response to feedback.
#[derive(Default)]
struct StallDetector {
    count: usize,
    prev_blockers: Vec<String>,
    prev_diff: String,
}

impl StallDetector {
    fn observe_review(&mut self, blockers: &[String], diff: &str) {
        let same_diff = !self.prev_diff.is_empty() && diff == self.prev_diff;
        let same_blockers =
            !self.prev_blockers.is_empty() && policy::blockers_similar(blockers, &self.prev_blockers);

        if same_diff || same_blockers {
            self.count += 1;
        } else {
            self.count = 0;
        }
        self.prev_blockers = blockers.to_vec();
        self.prev_diff = diff.to_string();
    }

    fn observe_no_changes(&mut self) {
        self.count += 1;
    }

    fn is_stuck(&self) -> bool {
        self.count >= 2
    }
}

enum Escalation {
    Continue(String),
    Stop,
}

enum PlanOutcome {
    Proceed(String),
    Abort(OrchestratorResult),
}

// ── Public API ──

pub fn run(
    opts: &TaskOptions,
    writer: &mut dyn ModelAdapter,
    reviewer: &mut dyn ModelAdapter,
) -> Result<OrchestratorResult> {
    let mode = match (opts.plan_first, opts.auto) {
        (true, true) => "plan + auto",
        (true, false) => "plan",
        (false, true) => "auto",
        (false, false) => "interactive",
    };
    ui::banner(opts.task, writer.name(), reviewer.name(), mode, opts.config.policy.max_rounds);

    let session = setup_session(opts)?;
    ui::info(&format!("logs: {}", session.log.dir.display()));

    let mut costs = CostTracker::new();

    let plan = if opts.plan_first {
        match plan_phase(opts, writer, reviewer, &session, &mut costs)? {
            PlanOutcome::Proceed(plan) => Some(plan),
            PlanOutcome::Abort(result) => return Ok(result),
        }
    } else {
        None
    };

    execute_loop(opts, writer, reviewer, &session, plan.as_deref(), &mut costs)
}

pub fn review_only(
    config: &Config,
    reviewer: &mut dyn ModelAdapter,
    repo_dir: &Path,
    task: Option<&str>,
    verbose: bool,
) -> Result<OrchestratorResult> {
    let diff = git::git_diff(repo_dir)?;
    if diff.trim().is_empty() {
        anyhow::bail!("no uncommitted changes to review");
    }

    let review_template =
        load_prompt_template(&config.prompts.review, prompts::DEFAULT_REVIEW_TEMPLATE, repo_dir)?;
    let task_context = task.unwrap_or(
        "Review the current uncommitted changes for bugs, edge cases, and best practices.",
    );

    let review_prompt = prompts::build_review_prompt(
        &review_template,
        task_context,
        &diff,
        "",
        "(not provided — judge the diff on its own)",
    );

    let mut costs = CostTracker::new();
    ui::working(reviewer.name(), "reviewing uncommitted changes…");
    let (response, usage) = reviewer.generate(&review_prompt, &[])?;
    costs.add(usage);

    if !reviewer.streams_output() {
        ui::response_block(reviewer.name(), &response, verbose);
    }

    let verdict = policy::parse_verdict(&response);
    ui::verdict(&verdict);
    costs.summary();

    let success = verdict.verdict == Verdict::Approved;
    Ok(OrchestratorResult {
        success,
        rounds: 1,
        message: if success { "approved".into() } else { "changes requested by AI".into() },
    })
}

// ── Phases ──

fn plan_phase(
    opts: &TaskOptions,
    writer: &mut dyn ModelAdapter,
    reviewer: &mut dyn ModelAdapter,
    session: &Session,
    costs: &mut CostTracker,
) -> Result<PlanOutcome> {
    ui::section("Planning");

    let plan_prompt =
        prompts::build_plan_prompt(prompts::DEFAULT_PLAN_TEMPLATE, opts.task, &session.repo_context, "");

    ui::working(writer.name(), "drafting a plan…");
    let (plan, usage) = writer
        .generate(&plan_prompt, opts.images)
        .with_context(|| format!("{} failed during planning", writer.name()))?;
    costs.add(usage);

    if !writer.streams_output() {
        ui::response_block(writer.name(), &plan, opts.verbose);
    }
    session.log.write_writer_response(0, &plan)?;

    if !ui::ask_yes_no(&format!("review this plan with {}?", reviewer.name())) {
        ui::stopped("Plan saved but not reviewed. Exiting.");
        costs.summary();
        return Ok(PlanOutcome::Abort(OrchestratorResult {
            success: false,
            rounds: 0,
            message: "plan created, user skipped review".into(),
        }));
    }

    let review_prompt =
        prompts::build_plan_review_prompt(prompts::DEFAULT_PLAN_REVIEW_TEMPLATE, opts.task, &plan);

    ui::working(reviewer.name(), "reviewing the plan…");
    let (plan_review, usage) = reviewer
        .generate(&review_prompt, &[])
        .with_context(|| format!("{} failed during plan review", reviewer.name()))?;
    costs.add(usage);

    if !reviewer.streams_output() {
        ui::response_block(reviewer.name(), &plan_review, opts.verbose);
    }
    session.log.write_reviewer_response(0, &plan_review)?;
    ui::verdict(&policy::parse_verdict(&plan_review));

    if !ui::ask_yes_no("execute this task?") {
        ui::stopped("Exiting without executing.");
        costs.summary();
        return Ok(PlanOutcome::Abort(OrchestratorResult {
            success: false,
            rounds: 0,
            message: "plan reviewed, user chose not to execute".into(),
        }));
    }

    Ok(PlanOutcome::Proceed(plan))
}

fn execute_loop(
    opts: &TaskOptions,
    writer: &mut dyn ModelAdapter,
    reviewer: &mut dyn ModelAdapter,
    session: &Session,
    plan: Option<&str>,
    costs: &mut CostTracker,
) -> Result<OrchestratorResult> {
    let max_rounds = opts.config.policy.max_rounds;
    let hard_cap = max_rounds * 2;
    let mut budget = max_rounds;

    let mut stall = StallDetector::default();
    let mut clarifications_used = 0usize;
    let mut clarification: Option<String> = None;
    let mut feedback: Option<String> = None;
    let mut last_verdict: Option<ReviewVerdict> = None;
    let mut last_checks_passed = false;
    // True while the task is being handled as a text answer (no code changes
    // yet): review targets the answer itself, and later no-change rounds are
    // revisions, not stalls.
    let mut answer_mode = false;
    let mut round = 0;

    while round < budget {
        round += 1;
        ui::round_header(round, budget);

        let clar = clarification.take();
        let writer_prompt = build_writer_prompt(
            session, opts.task, plan, round, feedback.as_deref(), clar.as_deref(), answer_mode,
        );
        let diff_before = git::git_diff(opts.repo_dir).unwrap_or_default();

        ui::working(
            writer.name(),
            if round == 1 { "implementing…" } else { "addressing review feedback…" },
        );
        let round_images = if round == 1 { opts.images } else { &[][..] };
        let (writer_response, usage) = writer
            .generate(&writer_prompt, round_images)
            .with_context(|| format!("writer ({}) failed in round {}", writer.name(), round))?;
        costs.add(usage);

        session.log.write_writer_response(round, &writer_response)?;
        if !writer.streams_output() {
            ui::response_block(writer.name(), &writer_response, opts.verbose);
        }

        match writer_diff_outcome(opts, reviewer.name(), &session.log, round, &diff_before)? {
            DiffOutcome::NoChanges if !answer_mode && round > 1 => {
                ui::warn(&format!("{} made no changes in response to feedback", writer.name()));
                stall.observe_no_changes();
            }
            DiffOutcome::NoChanges => {
                answer_mode = true;
                ui::info(&format!("{} answered without making code changes", writer.name()));

                let wants_review = opts.auto
                    || ui::ask_yes_no(&format!("review this answer with {}?", reviewer.name()));
                if !wants_review {
                    costs.summary();
                    return Ok(ok_result(round, "completed — answer accepted without review"));
                }

                let review =
                    run_answer_review(opts, reviewer, session, costs, round, &writer_response, clar.as_deref())?;
                last_checks_passed = true;

                if review.approved() {
                    ui::success("Answer approved!");
                    session.log.write_summary(&RunSummary {
                        task: opts.task,
                        writer: writer.name(),
                        reviewer: reviewer.name(),
                        rounds: round,
                        verdict: &review.verdict,
                        checks_passed: true,
                        success: true,
                    })?;
                    costs.summary();
                    return Ok(ok_result(round, "answer approved by reviewer"));
                }

                stall.observe_review(&review.verdict.blockers, "");
                feedback = Some(review.response.clone());
                last_verdict = Some(review.verdict.clone());

                if !opts.auto
                    && !ui::ask_yes_no(&format!("let {} revise the answer?", writer.name()))
                {
                    ui::stopped("Stopping. Review feedback saved in logs.");
                    session.log.write_summary(&RunSummary {
                        task: opts.task,
                        writer: writer.name(),
                        reviewer: reviewer.name(),
                        rounds: round,
                        verdict: &review.verdict,
                        checks_passed: true,
                        success: false,
                    })?;
                    costs.summary();
                    return Ok(ok_result(round, "user stopped after answer review"));
                }
            }
            DiffOutcome::UserSkipped => {
                ui::success("Task completed.");
                costs.summary();
                return Ok(ok_result(round, "completed — user accepted without review"));
            }
            DiffOutcome::Review(diff) => {
                answer_mode = false;
                let input = ReviewInput {
                    round,
                    diff: &diff,
                    writer_notes: &writer_response,
                    clarification: clar.as_deref(),
                };
                let review = run_review(opts, reviewer, session, costs, &input)?;
                last_checks_passed = review.checks_passed;

                if review.approved() {
                    ui::success("Approved!");
                    session.log.write_summary(&RunSummary {
                        task: opts.task,
                        writer: writer.name(),
                        reviewer: reviewer.name(),
                        rounds: round,
                        verdict: &review.verdict,
                        checks_passed: true,
                        success: true,
                    })?;
                    notify_approval(writer, &review.response, costs);
                    costs.summary();
                    return Ok(ok_result(round, "approved with all checks passing"));
                }

                if review.verdict.verdict == Verdict::Approved && !review.checks_passed {
                    ui::warn("AI approved, but checks failed");
                }

                stall.observe_review(&review.verdict.blockers, &diff);
                feedback = Some(build_feedback(&review));
                last_verdict = Some(review.verdict.clone());

                if !opts.auto && !ask_fix(&review, writer.name()) {
                    ui::stopped("Stopping. Review feedback saved in logs.");
                    session.log.write_summary(&RunSummary {
                        task: opts.task,
                        writer: writer.name(),
                        reviewer: reviewer.name(),
                        rounds: round,
                        verdict: &review.verdict,
                        checks_passed: review.checks_passed,
                        success: false,
                    })?;
                    costs.summary();
                    return Ok(ok_result(round, "user stopped after review"));
                }
            }
        }

        if opts.auto && (stall.is_stuck() || round == budget) && round < hard_cap {
            match escalate(last_verdict.as_ref(), &mut clarifications_used, &session.log, round)? {
                Escalation::Continue(text) => {
                    clarification = Some(text);
                    stall = StallDetector::default();
                    budget = (round + max_rounds).min(hard_cap);
                }
                Escalation::Stop => break,
            }
        }
    }

    let final_verdict = last_verdict.unwrap_or_else(|| ReviewVerdict {
        verdict: Verdict::ChangesRequested,
        blockers: vec!["no review completed".into()],
        suggestions: vec![],
    });
    session.log.write_summary(&RunSummary {
        task: opts.task,
        writer: writer.name(),
        reviewer: reviewer.name(),
        rounds: round,
        verdict: &final_verdict,
        checks_passed: last_checks_passed,
        success: false,
    })?;
    costs.summary();

    Ok(OrchestratorResult {
        success: false,
        rounds: round,
        message: format!("stopped after {} rounds without full approval", round),
    })
}

// ── Round steps ──

fn build_writer_prompt(
    session: &Session,
    task: &str,
    plan: Option<&str>,
    round: usize,
    feedback: Option<&str>,
    clarification: Option<&str>,
    answer_mode: bool,
) -> String {
    let mut prompt = if round == 1 {
        match plan {
            Some(p) => prompts::build_implement_with_plan_prompt(
                &session.impl_template, task, &session.repo_context, p,
            ),
            None => prompts::build_implement_prompt(
                &session.impl_template, task, &session.repo_context, "",
            ),
        }
    } else if answer_mode {
        prompts::build_answer_fix_prompt(
            prompts::DEFAULT_ANSWER_FIX_TEMPLATE, task, feedback.unwrap_or_default(),
        )
    } else {
        prompts::build_fix_prompt(&session.fix_template, task, feedback.unwrap_or_default())
    };

    if let Some(text) = clarification {
        prompt.push_str(&format!(
            "\n\nUSER CLARIFICATION (authoritative — follow this over any conflicting review feedback):\n{}",
            text
        ));
    }
    prompt
}

fn writer_diff_outcome(
    opts: &TaskOptions,
    reviewer_name: &str,
    log: &SessionLog,
    round: usize,
    diff_before: &str,
) -> Result<DiffOutcome> {
    let diff_after = git::git_diff(opts.repo_dir)?;
    let changed = diff_after != diff_before;
    let has_uncommitted = !diff_after.trim().is_empty();

    if !changed && !has_uncommitted {
        return Ok(DiffOutcome::NoChanges);
    }

    log.write_diff(round, &diff_after)?;

    let stat = git::git_diff_stat(opts.repo_dir).unwrap_or_default();
    if !stat.trim().is_empty() {
        ui::changes(&stat);
    }

    if opts.auto || ui::ask_yes_no(&format!("review changes with {}?", reviewer_name)) {
        Ok(DiffOutcome::Review(diff_after))
    } else {
        Ok(DiffOutcome::UserSkipped)
    }
}

fn run_review(
    opts: &TaskOptions,
    reviewer: &mut dyn ModelAdapter,
    session: &Session,
    costs: &mut CostTracker,
    input: &ReviewInput,
) -> Result<ReviewOutcome> {
    ui::working("checks", "running configured checks…");
    let check_results = checks::run_checks(&opts.config.checks, opts.repo_dir);
    session.log.write_checks(input.round, &check_results)?;

    if check_results.is_empty() {
        ui::info("no checks configured (.duet/config.toml [checks])");
    }
    for cr in &check_results {
        ui::check_result(&cr.name, cr.passed);
    }

    let checks_summary = checks::format_check_results(&check_results);
    let mut review_prompt = prompts::build_review_prompt(
        &session.review_template,
        opts.task,
        input.diff,
        &checks_summary,
        input.writer_notes,
    );
    if let Some(text) = input.clarification {
        review_prompt.push_str(&format!("\n\nUSER CLARIFICATION (authoritative):\n{}", text));
    }

    ui::working(reviewer.name(), "reviewing the changes…");
    let (response, usage) = reviewer
        .generate(&review_prompt, &[])
        .with_context(|| format!("reviewer ({}) failed in round {}", reviewer.name(), input.round))?;
    costs.add(usage);

    session.log.write_reviewer_response(input.round, &response)?;
    if !reviewer.streams_output() {
        ui::response_block(reviewer.name(), &response, opts.verbose);
    }

    let verdict = policy::parse_verdict(&response);
    ui::verdict(&verdict);

    Ok(ReviewOutcome {
        checks_passed: checks::all_passed(&check_results),
        verdict,
        response,
        checks_summary,
    })
}

/// Second opinion on a text answer (no diff involved, so no checks either).
fn run_answer_review(
    opts: &TaskOptions,
    reviewer: &mut dyn ModelAdapter,
    session: &Session,
    costs: &mut CostTracker,
    round: usize,
    answer: &str,
    clarification: Option<&str>,
) -> Result<ReviewOutcome> {
    let mut prompt = prompts::build_answer_review_prompt(
        prompts::DEFAULT_ANSWER_REVIEW_TEMPLATE, opts.task, answer,
    );
    if let Some(text) = clarification {
        prompt.push_str(&format!("\n\nUSER CLARIFICATION (authoritative):\n{}", text));
    }

    ui::working(reviewer.name(), "reviewing the answer…");
    let (response, usage) = reviewer
        .generate(&prompt, &[])
        .with_context(|| format!("reviewer ({}) failed in round {}", reviewer.name(), round))?;
    costs.add(usage);

    session.log.write_reviewer_response(round, &response)?;
    if !reviewer.streams_output() {
        ui::response_block(reviewer.name(), &response, opts.verbose);
    }

    let verdict = policy::parse_verdict(&response);
    ui::verdict(&verdict);

    Ok(ReviewOutcome {
        checks_passed: true,
        checks_summary: String::new(),
        verdict,
        response,
    })
}

/// Feedback for the writer's next round. If the reviewer approved but checks
/// failed, the raw review alone would read as "done" — spell out the failure.
fn build_feedback(review: &ReviewOutcome) -> String {
    if review.checks_passed {
        review.response.clone()
    } else {
        format!(
            "{}\n\nNOTE: automated checks are FAILING and must pass before approval:\n{}",
            review.response, review.checks_summary
        )
    }
}

fn ask_fix(review: &ReviewOutcome, writer_name: &str) -> bool {
    let question = if review.verdict.verdict == Verdict::Approved && !review.checks_passed {
        format!("AI approved, but checks failed. Let {} try to fix the checks?", writer_name)
    } else {
        format!("let {} fix the issues?", writer_name)
    };
    ui::ask_yes_no(&question)
}

fn escalate(
    last_verdict: Option<&ReviewVerdict>,
    clarifications_used: &mut usize,
    log: &SessionLog,
    round: usize,
) -> Result<Escalation> {
    ui::section("Needs your input");
    ui::warn("the models are not converging on their own");

    if let Some(v) = last_verdict {
        if !v.blockers.is_empty() {
            ui::info("open blockers:");
            for b in &v.blockers {
                ui::blocker(b);
            }
        }
    }

    if *clarifications_used >= 1 {
        ui::stopped("Clarification already given once — stopping so you can take over.");
        return Ok(Escalation::Stop);
    }

    let text = ui::ask_text("guidance for both models (empty to stop)");
    if text.is_empty() {
        return Ok(Escalation::Stop);
    }

    *clarifications_used += 1;
    log.write_clarification(round, &text)?;
    Ok(Escalation::Continue(text))
}

fn notify_approval(writer: &mut dyn ModelAdapter, reviewer_response: &str, costs: &mut CostTracker) {
    ui::working(writer.name(), "acknowledging approval…");
    let prompt = format!(
        "The reviewer has APPROVED your changes with the following feedback:\n\n{}\n\n\
         No further action is required. Please acknowledge.",
        reviewer_response
    );
    if let Ok((_text, usage)) = writer.generate(&prompt, &[]) {
        costs.add(usage);
    }
}

// ── Setup helpers ──

fn setup_session(opts: &TaskOptions) -> Result<Session> {
    let config = opts.config;
    let repo_dir = opts.repo_dir;

    let impl_template = load_prompt_template(
        &config.prompts.implementation, prompts::DEFAULT_IMPLEMENT_TEMPLATE, repo_dir,
    )?;
    let review_template =
        load_prompt_template(&config.prompts.review, prompts::DEFAULT_REVIEW_TEMPLATE, repo_dir)?;
    let fix_template =
        load_prompt_template(&config.prompts.fix, prompts::DEFAULT_FIX_TEMPLATE, repo_dir)?;

    let log = SessionLog::create(repo_dir, opts.task)?;
    let mut repo_context = build_repo_context(repo_dir)?;

    if opts.continue_session {
        if let Some(last_session) = SessionLog::get_last_session(repo_dir)? {
            let previous_context = SessionLog::read_session_context(&last_session)?;
            if !previous_context.is_empty() {
                ui::info("continuing from previous session");
                repo_context = format!("{}\n\n{}", repo_context, previous_context);
            }
        }
    }

    Ok(Session { log, repo_context, impl_template, review_template, fix_template })
}

fn load_prompt_template(path: &std::path::Path, default: &str, repo_dir: &Path) -> Result<String> {
    let full_path = repo_dir.join(path);
    if full_path.exists() {
        prompts::load_template(&full_path)
    } else {
        Ok(default.to_string())
    }
}

fn build_repo_context(dir: &Path) -> Result<String> {
    let branch = git::current_branch(dir).unwrap_or_else(|_| "unknown".into());
    let status = git::git_status(dir).unwrap_or_default();

    let mut context = format!("Branch: {}\n", branch);
    if !status.trim().is_empty() {
        context.push_str(&format!("Working tree status:\n{}\n", status));
    }

    Ok(context)
}

fn ok_result(rounds: usize, message: &str) -> OrchestratorResult {
    OrchestratorResult { success: true, rounds, message: message.into() }
}

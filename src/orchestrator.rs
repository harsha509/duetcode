use crate::adapters::{ImageInput, ModelAdapter, UsageStats};
use crate::checks;
use crate::config::Config;
use crate::events::{ask_yes_no, AskKind, Event, Sink};
use crate::git;
use crate::logs::{RunSummary, SessionLog};
use crate::policy::{self, ReviewVerdict, Verdict};
use crate::prompts;
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

struct CostTracker<'a> {
    entries: Vec<UsageStats>,
    sink: &'a dyn Sink,
}

impl<'a> CostTracker<'a> {
    fn new(sink: &'a dyn Sink) -> Self {
        Self { entries: Vec::new(), sink }
    }

    fn add(&mut self, usage: UsageStats) {
        if usage.input_tokens > 0 || usage.output_tokens > 0 {
            self.sink.event(Event::Usage {
                model: usage.model.clone(),
                input_tokens: usage.input_tokens,
                output_tokens: usage.output_tokens,
                cost_usd: usage.cost_usd,
            });
        }
        self.entries.push(usage);
    }

    fn summary(&self) {
        if self.entries.is_empty() {
            return;
        }
        let input_tokens: u64 = self.entries.iter().map(|u| u.input_tokens).sum();
        let output_tokens: u64 = self.entries.iter().map(|u| u.output_tokens).sum();
        let cost_usd = if self.entries.iter().any(|u| u.cost_usd.is_some()) {
            Some(self.entries.iter().filter_map(|u| u.cost_usd).sum())
        } else {
            None
        };
        self.sink.event(Event::CostSummary {
            calls: self.entries.len(),
            input_tokens,
            output_tokens,
            cost_usd,
        });
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
    sink: &dyn Sink,
) -> Result<OrchestratorResult> {
    let mode = match (opts.plan_first, opts.auto) {
        (true, true) => "plan + auto",
        (true, false) => "plan",
        (false, true) => "auto",
        (false, false) => "interactive",
    };
    sink.event(Event::TaskStarted {
        task: opts.task.to_string(),
        writer: writer.name().to_string(),
        reviewer: reviewer.name().to_string(),
        mode: mode.to_string(),
        max_rounds: opts.config.policy.max_rounds,
    });

    let session = setup_session(opts, sink)?;
    sink.event(Event::Info { text: format!("logs: {}", session.log.dir.display()) });

    let mut costs = CostTracker::new(sink);

    let plan = if opts.plan_first {
        match plan_phase(opts, writer, reviewer, &session, &mut costs, sink)? {
            PlanOutcome::Proceed(plan) => Some(plan),
            PlanOutcome::Abort(result) => return Ok(result),
        }
    } else {
        None
    };

    execute_loop(opts, writer, reviewer, &session, plan.as_deref(), &mut costs, sink)
}

pub fn review_only(
    config: &Config,
    reviewer: &mut dyn ModelAdapter,
    repo_dir: &Path,
    task: Option<&str>,
    sink: &dyn Sink,
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

    let mut costs = CostTracker::new(sink);
    sink.event(Event::Working {
        actor: reviewer.name().to_string(),
        action: "reviewing uncommitted changes…".to_string(),
    });
    let (response, usage) = reviewer.generate(&review_prompt, &[])?;
    costs.add(usage);

    if !reviewer.streams_output() {
        sink.event(Event::Response { model: reviewer.name().to_string(), text: response.clone() });
    }

    let verdict = policy::parse_verdict(&response);
    emit_verdict(sink, &verdict);
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
    sink: &dyn Sink,
) -> Result<PlanOutcome> {
    sink.event(Event::Section { title: "Planning".into() });

    let plan_prompt =
        prompts::build_plan_prompt(prompts::DEFAULT_PLAN_TEMPLATE, opts.task, &session.repo_context, "");

    sink.event(Event::Working {
        actor: writer.name().to_string(),
        action: "drafting a plan…".to_string(),
    });
    let (plan, usage) = writer
        .generate(&plan_prompt, opts.images)
        .with_context(|| format!("{} failed during planning", writer.name()))?;
    costs.add(usage);

    if !writer.streams_output() {
        sink.event(Event::Response { model: writer.name().to_string(), text: plan.clone() });
    }
    session.log.write_writer_response(0, &plan)?;

    if !ask_yes_no(sink, &format!("review this plan with {}?", reviewer.name())) {
        sink.event(Event::Stopped { text: "Plan saved but not reviewed. Exiting.".into() });
        costs.summary();
        return Ok(PlanOutcome::Abort(OrchestratorResult {
            success: false,
            rounds: 0,
            message: "plan created, user skipped review".into(),
        }));
    }

    let review_prompt =
        prompts::build_plan_review_prompt(prompts::DEFAULT_PLAN_REVIEW_TEMPLATE, opts.task, &plan);

    sink.event(Event::Working {
        actor: reviewer.name().to_string(),
        action: "reviewing the plan…".to_string(),
    });
    let (plan_review, usage) = reviewer
        .generate(&review_prompt, &[])
        .with_context(|| format!("{} failed during plan review", reviewer.name()))?;
    costs.add(usage);

    if !reviewer.streams_output() {
        sink.event(Event::Response { model: reviewer.name().to_string(), text: plan_review.clone() });
    }
    session.log.write_reviewer_response(0, &plan_review)?;
    emit_verdict(sink, &policy::parse_verdict(&plan_review));

    if !ask_yes_no(sink, "execute this task?") {
        sink.event(Event::Stopped { text: "Exiting without executing.".into() });
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
    sink: &dyn Sink,
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
        sink.event(Event::RoundStarted { round, budget });

        let clar = clarification.take();
        let writer_prompt = build_writer_prompt(
            session, opts.task, plan, round, feedback.as_deref(), clar.as_deref(), answer_mode,
        );
        let diff_before = git::git_diff(opts.repo_dir).unwrap_or_default();

        sink.event(Event::Working {
            actor: writer.name().to_string(),
            action: if round == 1 { "implementing…" } else { "addressing review feedback…" }.to_string(),
        });
        let round_images = if round == 1 { opts.images } else { &[][..] };
        let (writer_response, usage) = writer
            .generate(&writer_prompt, round_images)
            .with_context(|| format!("writer ({}) failed in round {}", writer.name(), round))?;
        costs.add(usage);

        session.log.write_writer_response(round, &writer_response)?;
        if !writer.streams_output() {
            sink.event(Event::Response { model: writer.name().to_string(), text: writer_response.clone() });
        }

        match writer_diff_outcome(opts, reviewer.name(), &session.log, round, &diff_before, sink)? {
            DiffOutcome::NoChanges if !answer_mode && round > 1 => {
                sink.event(Event::Warn {
                    text: format!("{} made no changes in response to feedback", writer.name()),
                });
                stall.observe_no_changes();
            }
            DiffOutcome::NoChanges => {
                answer_mode = true;
                sink.event(Event::Info {
                    text: format!("{} answered without making code changes", writer.name()),
                });

                let wants_review = opts.auto
                    || ask_yes_no(sink, &format!("review this answer with {}?", reviewer.name()));
                if !wants_review {
                    costs.summary();
                    return Ok(ok_result(round, "completed — answer accepted without review"));
                }

                let input = ReviewInput {
                    round,
                    diff: "",
                    writer_notes: &writer_response,
                    clarification: clar.as_deref(),
                };
                let review = run_answer_review(opts, reviewer, session, costs, &input, sink)?;
                last_checks_passed = true;

                if review.approved() {
                    sink.event(Event::Success { text: "Answer approved!".into() });
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
                    && !ask_yes_no(sink, &format!("let {} revise the answer?", writer.name()))
                {
                    sink.event(Event::Stopped { text: "Stopping. Review feedback saved in logs.".into() });
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
                sink.event(Event::Success { text: "Task completed.".into() });
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
                let review = run_review(opts, reviewer, session, costs, &input, sink)?;
                last_checks_passed = review.checks_passed;

                if review.approved() {
                    sink.event(Event::Success { text: "Approved!".into() });
                    session.log.write_summary(&RunSummary {
                        task: opts.task,
                        writer: writer.name(),
                        reviewer: reviewer.name(),
                        rounds: round,
                        verdict: &review.verdict,
                        checks_passed: true,
                        success: true,
                    })?;
                    notify_approval(writer, &review.response, costs, sink);
                    costs.summary();
                    return Ok(ok_result(round, "approved with all checks passing"));
                }

                if review.verdict.verdict == Verdict::Approved && !review.checks_passed {
                    sink.event(Event::Warn { text: "AI approved, but checks failed".into() });
                }

                stall.observe_review(&review.verdict.blockers, &diff);
                feedback = Some(build_feedback(&review));
                last_verdict = Some(review.verdict.clone());

                if !opts.auto && !ask_fix(&review, writer.name(), sink) {
                    sink.event(Event::Stopped { text: "Stopping. Review feedback saved in logs.".into() });
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
            match escalate(last_verdict.as_ref(), &mut clarifications_used, &session.log, round, sink)? {
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
    sink: &dyn Sink,
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
        sink.event(Event::Changes { stat });
    }

    if opts.auto || ask_yes_no(sink, &format!("review changes with {}?", reviewer_name)) {
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
    sink: &dyn Sink,
) -> Result<ReviewOutcome> {
    sink.event(Event::Working { actor: "checks".into(), action: "running configured checks…".into() });
    let check_results = checks::run_checks(&opts.config.checks, opts.repo_dir);
    session.log.write_checks(input.round, &check_results)?;

    if check_results.is_empty() {
        sink.event(Event::Info { text: "no checks configured (.duet/config.toml [checks])".into() });
    }
    for cr in &check_results {
        sink.event(Event::Check { name: cr.name.clone(), passed: cr.passed });
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

    sink.event(Event::Working {
        actor: reviewer.name().to_string(),
        action: "reviewing the changes…".to_string(),
    });
    let (response, usage) = reviewer
        .generate(&review_prompt, &[])
        .with_context(|| format!("reviewer ({}) failed in round {}", reviewer.name(), input.round))?;
    costs.add(usage);

    session.log.write_reviewer_response(input.round, &response)?;
    if !reviewer.streams_output() {
        sink.event(Event::Response { model: reviewer.name().to_string(), text: response.clone() });
    }

    let verdict = policy::parse_verdict(&response);
    emit_verdict(sink, &verdict);

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
    input: &ReviewInput,
    sink: &dyn Sink,
) -> Result<ReviewOutcome> {
    let mut prompt = prompts::build_answer_review_prompt(
        prompts::DEFAULT_ANSWER_REVIEW_TEMPLATE, opts.task, input.writer_notes,
    );
    if let Some(text) = input.clarification {
        prompt.push_str(&format!("\n\nUSER CLARIFICATION (authoritative):\n{}", text));
    }

    sink.event(Event::Working {
        actor: reviewer.name().to_string(),
        action: "reviewing the answer…".to_string(),
    });
    let (response, usage) = reviewer
        .generate(&prompt, &[])
        .with_context(|| format!("reviewer ({}) failed in round {}", reviewer.name(), input.round))?;
    costs.add(usage);

    session.log.write_reviewer_response(input.round, &response)?;
    if !reviewer.streams_output() {
        sink.event(Event::Response { model: reviewer.name().to_string(), text: response.clone() });
    }

    let verdict = policy::parse_verdict(&response);
    emit_verdict(sink, &verdict);

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

fn ask_fix(review: &ReviewOutcome, writer_name: &str, sink: &dyn Sink) -> bool {
    let question = if review.verdict.verdict == Verdict::Approved && !review.checks_passed {
        format!("AI approved, but checks failed. Let {} try to fix the checks?", writer_name)
    } else {
        format!("let {} fix the issues?", writer_name)
    };
    ask_yes_no(sink, &question)
}

fn escalate(
    last_verdict: Option<&ReviewVerdict>,
    clarifications_used: &mut usize,
    log: &SessionLog,
    round: usize,
    sink: &dyn Sink,
) -> Result<Escalation> {
    sink.event(Event::Section { title: "Needs your input".into() });
    sink.event(Event::Warn { text: "the models are not converging on their own".into() });

    if let Some(v) = last_verdict {
        if !v.blockers.is_empty() {
            sink.event(Event::Info { text: "open blockers:".into() });
            for b in &v.blockers {
                sink.event(Event::Blocker { text: b.clone() });
            }
        }
    }

    if *clarifications_used >= 1 {
        sink.event(Event::Stopped {
            text: "Clarification already given once — stopping so you can take over.".into(),
        });
        return Ok(Escalation::Stop);
    }

    let text = sink.ask(AskKind::Text, "guidance for both models (empty to stop)");
    let text = text.trim().to_string();
    if text.is_empty() {
        return Ok(Escalation::Stop);
    }

    *clarifications_used += 1;
    log.write_clarification(round, &text)?;
    Ok(Escalation::Continue(text))
}

fn notify_approval(
    writer: &mut dyn ModelAdapter,
    reviewer_response: &str,
    costs: &mut CostTracker,
    sink: &dyn Sink,
) {
    sink.event(Event::Working {
        actor: writer.name().to_string(),
        action: "acknowledging approval…".to_string(),
    });
    let prompt = format!(
        "The reviewer has APPROVED your changes with the following feedback:\n\n{}\n\n\
         No further action is required. Please acknowledge.",
        reviewer_response
    );
    if let Ok((_text, usage)) = writer.generate(&prompt, &[]) {
        costs.add(usage);
    }
}

fn emit_verdict(sink: &dyn Sink, verdict: &ReviewVerdict) {
    sink.event(Event::Verdict {
        approved: verdict.verdict == Verdict::Approved,
        blockers: verdict.blockers.clone(),
        suggestions: verdict.suggestions.clone(),
    });
}

// ── Setup helpers ──

fn setup_session(opts: &TaskOptions, sink: &dyn Sink) -> Result<Session> {
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
                sink.event(Event::Info { text: "continuing from previous session".into() });
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

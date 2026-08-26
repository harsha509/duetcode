use crate::adapters::{ImageInput, ModelAdapter, UsageStats};
use crate::checks;
use crate::config::Config;
use crate::events::{ask_yes_no, AskKind, Event, Sink};
use crate::git;
use crate::logs::{RunSummary, SessionLog, SessionRoles};
use crate::policy::{self, ReviewVerdict, Verdict, VerdictKind};
use crate::prompts;
use crate::review_subject;
use anyhow::{Context, Result};
use serde::Serialize;
use std::path::{Path, PathBuf};

/// How a run ended. `Unreviewed` is neither a pass nor a failure: the writer
/// finished and the user declined the review, so there is no verdict to report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Approved,
    Unreviewed,
    Stopped,
}

impl Outcome {
    /// Whether the run ended without anything going wrong — the exit-code test.
    /// A declined review is not a failure.
    pub fn success(self) -> bool {
        !matches!(self, Outcome::Stopped)
    }
}

pub struct OrchestratorResult {
    pub outcome: Outcome,
    pub rounds: usize,
    pub message: String,
    /// The answer the run ended on, when the writer answered instead of
    /// changing code and no reviewer ever judged it. A frontend holds this so
    /// its own review action has something to judge: an answer leaves no diff,
    /// and a diff review would judge the wrong thing.
    pub answer: Option<String>,
}

/// Everything a task run needs, bundled so call sites stay small.
pub struct TaskOptions<'a> {
    pub config: &'a Config,
    pub task: &'a str,
    pub images: &'a [ImageInput],
    /// The one checkout this task writes to: diffed, checked, and logged.
    pub repo_dir: &'a Path,
    /// Every project in the workspace, so the task can read its siblings for
    /// context. May include `repo_dir`; empty means a lone project, which is
    /// how the plain CLI and a single-folder window run.
    pub workspace: &'a [PathBuf],
    pub continue_session: bool,
    pub auto: bool,
    pub plan_first: bool,
    /// True when the caller offers review as its own action — the VS Code
    /// panel's review button. The loop then never asks whether to review: it
    /// hands the work back unreviewed and lets the user spend a reviewer call
    /// when they want one. A terminal, which has no such button, leaves this
    /// false and keeps the prompt.
    pub review_on_demand: bool,
}

// ── Internal types ──

struct Session {
    log: SessionLog,
    repo_context: String,
    impl_template: String,
    review_template: String,
    fix_template: String,
    /// What each sibling project's worktree looked like when the task began, so
    /// a later change there is attributable to this run rather than to whatever
    /// was already uncommitted.
    stray_baseline: Vec<(PathBuf, String)>,
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

/// What both kinds of review need. What they are judging differs — a diff for
/// code, a [`Subject`] for an answer — so that arrives as its own argument
/// rather than as a field one of them always ignores.
struct ReviewInput<'a> {
    round: usize,
    writer_notes: &'a str,
    clarification: Option<&'a str>,
}

enum DiffOutcome {
    /// The writer wrote no reviewable code this round, so the task is being
    /// answered rather than implemented. What that answer is about is worked
    /// out separately, by [`resolve_subject`] — it is not always the tree.
    NoChanges,
    /// The writer changed code and no review was run on it — either the user
    /// said no, or the caller reviews on demand and was never asked.
    NotReviewed,
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
    // One task is one turn: while this guard lives, the first Ctrl+C stops the
    // run instead of killing dt, and a second one still tears everything down.
    let _turn = crate::process::TurnGuard::new();
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

    let roles = SessionRoles { writer: writer.name(), reviewer: reviewer.name() };
    let mut session = setup_session(opts, &roles, sink)?;
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

    execute_loop(opts, writer, reviewer, &mut session, plan.as_deref(), &mut costs, sink)
}

pub fn review_only(
    config: &Config,
    reviewer: &mut dyn ModelAdapter,
    repo_dir: &Path,
    task: Option<&str>,
    sink: &dyn Sink,
) -> Result<OrchestratorResult> {
    let _turn = crate::process::TurnGuard::new();
    // A task naming a pull request is about that pull request, not about
    // whatever happens to be uncommitted here, so its diff is fetched and
    // reviewed in place of the working tree.
    if let Some(task) = task.filter(|t| review_subject::names_absent_code(t)) {
        return fetched_review_only(config, reviewer, repo_dir, task, sink);
    }

    let diff = git::git_diff(repo_dir)?;
    if diff.trim().is_empty() {
        anyhow::bail!("no uncommitted changes to review");
    }

    let review_template =
        load_prompt_template(&config.prompts.review, prompts::DEFAULT_REVIEW_TEMPLATE, repo_dir)?;
    let task_context = task.unwrap_or(
        "Review the current uncommitted changes for bugs, edge cases, and best practices.",
    );

    let (review_diff, diff_truncated) = capped_review_diff(&diff, &*reviewer);

    let review_prompt = prompts::build_review_prompt(
        &review_template,
        task_context,
        &review_repo_block(repo_dir),
        &review_diff,
        "",
        "(not provided — judge the diff on its own)",
    );

    let mut costs = CostTracker::new(sink);
    sink.event(Event::Working {
        actor: reviewer.name().to_string(),
        action: "reviewing uncommitted changes…".to_string(),
    });
    // A standalone review is a fresh judgement on the code as it stands, so it
    // starts from a clean session. See `ModelAdapter::reset_session`.
    reviewer.reset_session();
    let (response, usage) = match reviewer.generate(&review_prompt, &[]) {
        Ok(ok) => ok,
        Err(_e) if crate::process::is_cancelled() => return Ok(stopped_run(sink, 1)),
        Err(e) => return Err(e),
    };
    costs.add(usage);

    if !reviewer.streams_output() {
        sink.event(Event::Response { model: reviewer.name().to_string(), text: response.clone() });
    }

    report_unsupported_reads(reviewer, &response, sink);
    report_unread_truncation(reviewer, diff_truncated, sink);
    let verdict = policy::parse_verdict(&response);
    emit_verdict(sink, VerdictKind::Code, &verdict);
    costs.summary();

    let approved = verdict.verdict == Verdict::Approved;
    Ok(OrchestratorResult {
        outcome: if approved { Outcome::Approved } else { Outcome::Stopped },
        rounds: 1,
        message: if approved { "approved".into() } else { "changes requested by AI".into() },
        answer: None,
    })
}

/// The review a task gets when it names a change that is not in this checkout:
/// the named pull requests are fetched and their diff is the subject. The
/// working tree is deliberately never consulted — reviewing whatever happened
/// to be uncommitted here while the user pointed at a pull request judges the
/// wrong code entirely.
fn fetched_review_only(
    config: &Config,
    reviewer: &mut dyn ModelAdapter,
    repo_dir: &Path,
    task: &str,
    sink: &dyn Sink,
) -> Result<OrchestratorResult> {
    let subject = resolve_subject(task, repo_dir, sink);
    if let Some(refused) = refusal(&subject, reviewer) {
        sink.event(Event::Warn { text: refused.warning });
        return Ok(unreviewed_result(0, refused.message.into(), None));
    }
    let Subject::Fetched { labels, .. } = &subject else {
        // On a task naming absent code, `resolve_subject` returns Fetched or
        // Absent, and Absent was refused above.
        return Ok(unreviewed_result(0, UNGROUNDED_MESSAGE.into(), None));
    };

    let review_template =
        load_prompt_template(&config.prompts.review, prompts::DEFAULT_REVIEW_TEMPLATE, repo_dir)?;
    let changes = changes_block(&subject, reviewer);
    let review_prompt = prompts::build_review_prompt(
        &review_template,
        task,
        &fetched_repo_block(labels),
        &changes.text,
        "",
        "(not provided — judge the diff on its own)",
    );

    let mut costs = CostTracker::new(sink);
    sink.event(Event::Working {
        actor: reviewer.name().to_string(),
        action: format!("reviewing {}…", labels.join(", ")),
    });
    // A standalone review is a fresh judgement on the change as it stands, so
    // it starts from a clean session. See `ModelAdapter::reset_session`.
    reviewer.reset_session();
    let (response, usage) = match reviewer.generate(&review_prompt, &[]) {
        Ok(ok) => ok,
        Err(_e) if crate::process::is_cancelled() => return Ok(stopped_run(sink, 1)),
        Err(e) => return Err(e),
    };
    costs.add(usage);

    if !reviewer.streams_output() {
        sink.event(Event::Response { model: reviewer.name().to_string(), text: response.clone() });
    }

    report_unsupported_reads(reviewer, &response, sink);
    report_unread_truncation(reviewer, changes.truncated, sink);
    let verdict = policy::parse_verdict(&response);
    emit_verdict(sink, VerdictKind::Code, &verdict);
    costs.summary();

    let approved = verdict.verdict == Verdict::Approved;
    Ok(OrchestratorResult {
        outcome: if approved { Outcome::Approved } else { Outcome::Stopped },
        rounds: 1,
        message: if approved { "approved".into() } else { "changes requested by AI".into() },
        answer: None,
    })
}

/// Reviews an answer a writer already gave, outside any loop — what a frontend
/// runs when the user asks for a review of a task that produced no code. The
/// subject is resolved fresh, so the answer is judged against the change it is
/// about as it stands right now.
///
/// `task` is the question the answer answered, not whatever the user typed when
/// asking for the review; judging an answer against the wrong question is worse
/// than not reviewing it, and the question is also what names the change under
/// discussion. Anything typed rides along as an instruction instead.
pub fn answer_review_only(
    reviewer: &mut dyn ModelAdapter,
    repo_dir: &Path,
    task: &str,
    answer: &str,
    instruction: Option<&str>,
    sink: &dyn Sink,
) -> Result<OrchestratorResult> {
    let _turn = crate::process::TurnGuard::new();
    let subject = resolve_subject(task, repo_dir, sink);
    if let Some(refused) = refusal(&subject, reviewer) {
        sink.event(Event::Warn { text: refused.warning });
        return Ok(unreviewed_result(0, refused.message.into(), None));
    }

    let changes = changes_block(&subject, reviewer);
    let mut prompt = prompts::build_answer_review_prompt(
        prompts::DEFAULT_ANSWER_REVIEW_TEMPLATE,
        task,
        answer,
        &changes.text,
        access_block(reviewer, &subject),
        &git::repo_identity(repo_dir),
    );
    if let Some(text) = instruction.map(str::trim).filter(|t| !t.is_empty()) {
        prompt.push_str(&format!("\n\nUSER INSTRUCTION (authoritative):\n{}", text));
    }

    let mut costs = CostTracker::new(sink);
    sink.event(Event::Working {
        actor: reviewer.name().to_string(),
        action: "reviewing the answer…".to_string(),
    });
    // A standalone review is a fresh judgement on the answer as it stands, so it
    // starts from a clean session. See `ModelAdapter::reset_session`.
    reviewer.reset_session();
    let (response, usage) = match reviewer.generate(&prompt, &[]) {
        Ok(ok) => ok,
        Err(_e) if crate::process::is_cancelled() => return Ok(stopped_run(sink, 1)),
        Err(e) => return Err(e),
    };
    costs.add(usage);

    if !reviewer.streams_output() {
        sink.event(Event::Response { model: reviewer.name().to_string(), text: response.clone() });
    }

    report_unsupported_reads(reviewer, &response, sink);
    report_unread_truncation(reviewer, changes.truncated, sink);
    let verdict = policy::parse_verdict(&response);
    emit_verdict(sink, VerdictKind::Answer, &verdict);
    costs.summary();

    let approved = verdict.verdict == Verdict::Approved;
    Ok(OrchestratorResult {
        outcome: if approved { Outcome::Approved } else { Outcome::Stopped },
        rounds: 1,
        message: if approved {
            answer_sound_message(policy::parse_answer_conclusion(&response).as_deref())
        } else {
            "the answer was found unsound".into()
        },
        answer: None,
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
    let (plan, usage) = match writer.generate(&plan_prompt, opts.images) {
        Ok(ok) => ok,
        Err(_e) if crate::process::is_cancelled() => {
            costs.summary();
            return Ok(PlanOutcome::Abort(stopped_run(sink, 0)));
        }
        Err(e) => {
            return Err(e).with_context(|| format!("{} failed during planning", writer.name()))
        }
    };
    costs.add(usage);

    if !writer.streams_output() {
        sink.event(Event::Response { model: writer.name().to_string(), text: plan.clone() });
    }
    session.log.write_writer_response(0, &plan)?;

    let plan_review_wanted = ask_yes_no(sink, &format!("review this plan with {}?", reviewer.name()));
    if crate::process::is_cancelled() || !plan_review_wanted {
        sink.event(Event::Stopped { text: "Plan saved but not reviewed. Exiting.".into() });
        costs.summary();
        return Ok(PlanOutcome::Abort(OrchestratorResult {
            outcome: Outcome::Stopped,
            rounds: 0,
            message: if crate::process::is_cancelled() {
                "stopped by user".into()
            } else {
                "plan created, user skipped review".into()
            },
            answer: None,
        }));
    }

    let review_prompt =
        prompts::build_plan_review_prompt(prompts::DEFAULT_PLAN_REVIEW_TEMPLATE, opts.task, &plan);

    sink.event(Event::Working {
        actor: reviewer.name().to_string(),
        action: "reviewing the plan…".to_string(),
    });
    let (plan_review, usage) = match reviewer.generate(&review_prompt, &[]) {
        Ok(ok) => ok,
        Err(_e) if crate::process::is_cancelled() => {
            costs.summary();
            return Ok(PlanOutcome::Abort(stopped_run(sink, 0)));
        }
        Err(e) => {
            return Err(e).with_context(|| format!("{} failed during plan review", reviewer.name()))
        }
    };
    costs.add(usage);

    if !reviewer.streams_output() {
        sink.event(Event::Response { model: reviewer.name().to_string(), text: plan_review.clone() });
    }
    session.log.write_reviewer_response(0, &plan_review)?;
    emit_verdict(sink, VerdictKind::Code, &policy::parse_verdict(&plan_review));

    let execute = ask_yes_no(sink, "execute this task?");
    if crate::process::is_cancelled() || !execute {
        sink.event(Event::Stopped { text: "Exiting without executing.".into() });
        costs.summary();
        return Ok(PlanOutcome::Abort(OrchestratorResult {
            outcome: Outcome::Stopped,
            rounds: 0,
            message: if crate::process::is_cancelled() {
                "stopped by user".into()
            } else {
                "plan reviewed, user chose not to execute".into()
            },
            answer: None,
        }));
    }

    Ok(PlanOutcome::Proceed(plan))
}

fn execute_loop(
    opts: &TaskOptions,
    writer: &mut dyn ModelAdapter,
    reviewer: &mut dyn ModelAdapter,
    session: &mut Session,
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
    // The code an answer is judged against, resolved on the first answer round
    // and kept for the rest of the run.
    let mut subject: Option<Subject> = None;
    let mut round = 0;

    while round < budget {
        if crate::process::is_cancelled() {
            costs.summary();
            return Ok(stopped_run(sink, round));
        }
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
        let (writer_response, usage) = match writer.generate(&writer_prompt, round_images) {
            Ok(ok) => ok,
            Err(_e) if crate::process::is_cancelled() => {
                costs.summary();
                return Ok(stopped_run(sink, round));
            }
            Err(e) => {
                return Err(e)
                    .with_context(|| format!("writer ({}) failed in round {}", writer.name(), round))
            }
        };
        costs.add(usage);

        session.log.write_writer_response(round, &writer_response)?;
        if !writer.streams_output() {
            sink.event(Event::Response { model: writer.name().to_string(), text: writer_response.clone() });
        }
        report_stray_writes(session, opts.repo_dir, sink);

        match writer_diff_outcome(opts, reviewer.name(), &session.log, round, &diff_before, sink)? {
            // Nothing written and nothing said: reviewing an empty answer
            // spends a reviewer call on confirming nothing is nothing.
            DiffOutcome::NoChanges if writer_response.trim().is_empty() => {
                sink.event(Event::Warn {
                    text: format!(
                        "{} returned an empty answer — nothing to review this round",
                        writer.name()
                    ),
                });
                stall.observe_no_changes();
            }
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

                // Asked before the subject is resolved, because resolving can
                // mean fetching pull requests over the network. A caller that
                // reviews on demand — the panel — always declines here and
                // spends its reviewer call later, through `answer_review_only`;
                // fetching first would do that work twice and throw the first
                // copy away.
                if !wants_review(opts, reviewer.name(), "review this answer", sink) {
                    costs.summary();
                    return Ok(unreviewed_result(
                        round,
                        unreviewed_message(
                            opts.review_on_demand, writer.name(), reviewer.name(), "answer",
                        ),
                        Some(writer_response.clone()),
                    ));
                }

                // Resolved once and kept: a later round is the same answer
                // revised, about the same code, and re-resolving would fetch
                // every pull request again for nothing.
                let subject = subject
                    .get_or_insert_with(|| resolve_subject(opts.task, opts.repo_dir, sink));

                // Checked before the prompt, so a reviewer call is never spent
                // on a review that could not check anything.
                if let Some(refused) = refusal(subject, reviewer) {
                    sink.event(Event::Warn { text: refused.warning });
                    costs.summary();
                    return Ok(unreviewed_result(
                        round,
                        refused.message.into(),
                        Some(writer_response.clone()),
                    ));
                }

                let input = ReviewInput {
                    round,
                    writer_notes: &writer_response,
                    clarification: clar.as_deref(),
                };
                let review = match run_answer_review(
                    opts, reviewer, session, costs, &input, subject, sink,
                ) {
                    Ok(review) => review,
                    Err(_e) if crate::process::is_cancelled() => {
                        costs.summary();
                        return Ok(stopped_run(sink, round));
                    }
                    Err(e) => return Err(e),
                };
                last_checks_passed = true;

                if review.approved() {
                    let conclusion = policy::parse_answer_conclusion(&review.response);
                    sink.event(Event::Success {
                        text: format!(
                            "{} found {}'s answer SOUND — a verdict on the answer, not on the code it discusses",
                            reviewer.name(),
                            writer.name()
                        ),
                    });
                    if let Some(text) = &conclusion {
                        sink.event(Event::Info {
                            text: format!("{}'s own conclusion stands: {}", writer.name(), text),
                        });
                    }
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
                    return Ok(ok_result(round, &answer_sound_message(conclusion.as_deref())));
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
            DiffOutcome::NotReviewed => {
                sink.event(Event::Info { text: "Task completed without a review.".into() });
                costs.summary();
                return Ok(unreviewed_result(
                    round,
                    unreviewed_message(
                        opts.review_on_demand, writer.name(), reviewer.name(), "changes",
                    ),
                    None,
                ));
            }
            DiffOutcome::Review(diff) => {
                answer_mode = false;
                let input = ReviewInput {
                    round,
                    writer_notes: &writer_response,
                    clarification: clar.as_deref(),
                };
                let review = match run_review(opts, reviewer, session, costs, &input, &diff, sink) {
                    Ok(review) => review,
                    Err(_e) if crate::process::is_cancelled() => {
                        costs.summary();
                        return Ok(stopped_run(sink, round));
                    }
                    Err(e) => return Err(e),
                };
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
        outcome: Outcome::Stopped,
        rounds: round,
        message: format!("stopped after {} rounds without full approval", round),
        answer: None,
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

    // Every round, not just the first: a fix round rewrites the answer, and one
    // that dropped the block would leave the panel with nothing to summarise.
    // Appended for the reason `ensure_placeholder` documents — the end of a
    // prompt is the part models actually follow.
    if wants_pr_verdict(task) {
        prompt.push_str("\n\n");
        prompt.push_str(prompts::PR_VERDICT_BLOCK);
    }

    if let Some(text) = clarification {
        prompt.push_str(&format!(
            "\n\nUSER CLARIFICATION (authoritative — follow this over any conflicting review feedback):\n{}",
            text
        ));
    }
    prompt
}

/// Whether the task is asking about pull requests, and so has a go/no-go answer
/// worth stating plainly.
///
/// Keyed on a pull request being *named*, which is a cheap scan of the task
/// text — no `gh`, no network. A task that merely edits code is left alone: a
/// verdict header on "add a retry to the upload path" answers nothing, and
/// asking for one invites the writer to frame ordinary work as a judgement.
fn wants_pr_verdict(task: &str) -> bool {
    !review_subject::pull_requests(task).is_empty()
}

/// Whether this round produced code worth sending to the reviewer.
///
/// Both arguments are whole-worktree diffs, which is why neither condition
/// alone is enough. A tree that was already dirty when the round began is not
/// the writer's work — reviewing it attributes someone else's edits to the
/// writer — so an unchanged diff means "no code this round" even when the tree
/// is dirty. And a writer that reverts the tree back to clean leaves an empty
/// diff, which a reviewer can only rubber-stamp.
fn wrote_reviewable_code(diff_before: &str, diff_after: &str) -> bool {
    diff_after != diff_before && !diff_after.trim().is_empty()
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

    if !wrote_reviewable_code(diff_before, &diff_after) {
        return Ok(DiffOutcome::NoChanges);
    }

    log.write_diff(round, &diff_after)?;

    let stat = git::git_diff_stat(opts.repo_dir).unwrap_or_default();
    if !stat.trim().is_empty() {
        sink.event(Event::Changes { stat, diff: Some(capped_event_diff(&diff_after)) });
    }

    if wants_review(opts, reviewer_name, "review changes", sink) {
        Ok(DiffOutcome::Review(diff_after))
    } else {
        Ok(DiffOutcome::NotReviewed)
    }
}

/// Diff carried on the changes event, bounded so one huge patch cannot bloat
/// the protocol stream or the panel. The full patch stays in the session log.
const EVENT_DIFF_CAP: usize = 200 * 1024;

fn capped_event_diff(diff: &str) -> String {
    if diff.len() <= EVENT_DIFF_CAP {
        return diff.to_string();
    }
    let mut end = EVENT_DIFF_CAP;
    while !diff.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n… truncated — the full patch is in the session log", &diff[..end])
}

/// Whether a reviewer runs now. Auto mode always reviews; a caller that offers
/// review as its own action never does, so the reviewer call is spent only when
/// the user asks for it; everyone else is asked.
fn wants_review(opts: &TaskOptions, reviewer_name: &str, action: &str, sink: &dyn Sink) -> bool {
    if opts.auto {
        return true;
    }
    if opts.review_on_demand {
        return false;
    }
    ask_yes_no(sink, &format!("{} with {}?", action, reviewer_name))
}

fn run_review(
    opts: &TaskOptions,
    reviewer: &mut dyn ModelAdapter,
    session: &Session,
    costs: &mut CostTracker,
    input: &ReviewInput,
    diff: &str,
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
    let (review_diff, diff_truncated) = capped_review_diff(diff, reviewer);
    let mut review_prompt = prompts::build_review_prompt(
        &session.review_template,
        opts.task,
        &review_repo_block(opts.repo_dir),
        &review_diff,
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

    report_unsupported_reads(reviewer, &response, sink);
    report_unread_truncation(reviewer, diff_truncated, sink);
    let verdict = policy::parse_verdict(&response);
    emit_verdict(sink, VerdictKind::Code, &verdict);

    Ok(ReviewOutcome {
        checks_passed: checks::all_passed(&check_results),
        verdict,
        response,
        checks_summary,
    })
}

/// Second opinion on a text answer. The answer produced no diff of its own, so
/// there is nothing for the checks to run against either; `subject` is the code
/// the answer is about, and decides both what the reviewer is shown and what it
/// is told about its own reach.
fn run_answer_review(
    opts: &TaskOptions,
    reviewer: &mut dyn ModelAdapter,
    session: &Session,
    costs: &mut CostTracker,
    input: &ReviewInput,
    subject: &Subject,
    sink: &dyn Sink,
) -> Result<ReviewOutcome> {
    let changes = changes_block(subject, reviewer);
    let mut prompt = prompts::build_answer_review_prompt(
        prompts::DEFAULT_ANSWER_REVIEW_TEMPLATE,
        opts.task,
        input.writer_notes,
        &changes.text,
        access_block(reviewer, subject),
        &git::repo_identity(opts.repo_dir),
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

    report_unread_truncation(reviewer, changes.truncated, sink);
    let verdict = policy::parse_verdict(&response);
    emit_verdict(sink, VerdictKind::Answer, &verdict);

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
    if !crate::process::is_cancelled() {
        if let Ok((_text, usage)) = writer.generate(&prompt, &[]) {
            costs.add(usage);
        }
    }
}

fn emit_verdict(sink: &dyn Sink, kind: VerdictKind, verdict: &ReviewVerdict) {
    sink.event(Event::Verdict {
        kind,
        approved: verdict.verdict == Verdict::Approved,
        blockers: verdict.blockers.clone(),
        suggestions: verdict.suggestions.clone(),
    });
}

// ── Setup helpers ──

fn setup_session(opts: &TaskOptions, roles: &SessionRoles, sink: &dyn Sink) -> Result<Session> {
    let config = opts.config;
    let repo_dir = opts.repo_dir;

    let impl_template = load_prompt_template(
        &config.prompts.implementation, prompts::DEFAULT_IMPLEMENT_TEMPLATE, repo_dir,
    )?;
    let review_template =
        load_prompt_template(&config.prompts.review, prompts::DEFAULT_REVIEW_TEMPLATE, repo_dir)?;
    let fix_template =
        load_prompt_template(&config.prompts.fix, prompts::DEFAULT_FIX_TEMPLATE, repo_dir)?;

    let log = SessionLog::create(repo_dir, opts.task, roles)?;
    let mut repo_context = build_repo_context(repo_dir, opts.workspace)?;

    // Snapshotted before the writer runs, so pre-existing edits in a sibling
    // are never mistaken for something this task did.
    let stray_baseline = sibling_projects(repo_dir, opts.workspace)
        .into_iter()
        .map(|dir| (dir.clone(), git::git_diff(dir).unwrap_or_default()))
        .collect();

    if opts.continue_session {
        if let Some(last_session) = SessionLog::get_last_session(repo_dir)? {
            let previous_context = SessionLog::read_session_context(&last_session)?;
            if !previous_context.is_empty() {
                sink.event(Event::Info { text: "continuing from previous session".into() });
                repo_context = format!("{}\n\n{}", repo_context, previous_context);
            }
        }
    }

    Ok(Session { log, repo_context, impl_template, review_template, fix_template, stray_baseline })
}

fn load_prompt_template(path: &std::path::Path, default: &str, repo_dir: &Path) -> Result<String> {
    let full_path = repo_dir.join(path);
    if full_path.exists() {
        prompts::load_template(&full_path)
    } else {
        Ok(default.to_string())
    }
}

/// Cap on the working tree attached to an answer review. The diff is supporting
/// evidence there, not the subject, so a large working tree is trimmed rather
/// than billed in full — an answer review that costs more than the code review
/// it stands in for defeats the point.
const ANSWER_REVIEW_DIFF_BYTES: usize = 40_000;

/// Total budget for the changes fetched for one answer review, shared evenly
/// between them. Larger than the working-tree cap because here the diff *is*
/// the subject rather than context for it, but still bounded: a change nobody
/// can fit in one reading is not reviewed better by pasting more of it.
const FETCHED_DIFF_BYTES: usize = 120_000;

/// Budget for the working-tree diff attached to a code review. Past the cut a
/// reviewer with tools reads the files directly, one without says the diff is
/// insufficient; unbounded diffs blow up a CLI session's context.
const REVIEW_DIFF_BYTES: usize = 120_000;

/// Told to the user, and returned as the run's message, when a review was
/// refused for want of anything to check.
const UNREVIEWABLE_MESSAGE: &str =
    "unreviewable — the reviewer cannot read files and there are no changes to judge the answer against";

/// Told to the user when the answer is about code this checkout does not
/// contain and none of it could be retrieved.
const UNGROUNDED_MESSAGE: &str =
    "unreviewable — the answer is about code that is not in this checkout, and it could not be fetched";

/// What an answer review is judged against — and, the part that decides whether
/// the review means anything, whether the reviewer's own tools are looking at
/// that same code.
enum Subject {
    /// Uncommitted work in the checkout. A reviewer with tools opens the very
    /// files this diff describes, so what it reads corroborates.
    WorkingTree(String),
    /// A change fetched from outside the checkout. The reviewer has the diff,
    /// but its tools are standing somewhere else entirely.
    ///
    /// `missing` names anything the task pointed at that did not come back. A
    /// task naming two pull requests where only one fetches is still worth
    /// reviewing — but the reviewer has to be told which half it cannot see,
    /// or it judges those claims from the checkout, which is the failure this
    /// whole path exists to prevent.
    ///
    /// `truncated` says the diff was cut to fit its budget — which decides
    /// whether a verdict that opened no files can be taken at its word.
    Fetched { diff: String, labels: Vec<String>, missing: Vec<String>, truncated: bool },
    /// The answer is about a change outside the checkout that could not be
    /// retrieved. There is nothing here to review: no diff, and a reviewer whose
    /// tools would read a different revision while reporting it as the change.
    Absent { labels: Vec<String>, reason: String },
    /// A clean tree, and no change named anywhere else — so the question is
    /// about the checkout as it stands.
    Nothing,
}

/// Works out what the reviewer will be judging, fetching the change when the
/// task names one that is not in the checkout.
///
/// Only the task is scanned, never the answer. The task is the question being
/// reviewed, so it is what decides the subject; an answer that happens to
/// mention a pull request in passing must not redirect the review at it.
fn resolve_subject(task: &str, repo_dir: &Path, sink: &dyn Sink) -> Subject {
    if !review_subject::names_absent_code(task) {
        let diff = git::git_diff(repo_dir).unwrap_or_default();
        return if diff.trim().is_empty() {
            Subject::Nothing
        } else {
            Subject::WorkingTree(diff)
        };
    }

    let named = review_subject::pull_requests(task);
    if named.is_empty() {
        return Subject::Absent {
            labels: Vec::new(),
            reason: "it names no GitHub pull request URL that could be fetched".into(),
        };
    }
    if named.len() > review_subject::MAX_PULL_REQUESTS {
        sink.event(Event::Warn {
            text: format!(
                "{} pull requests named; fetching the first {}",
                named.len(),
                review_subject::MAX_PULL_REQUESTS
            ),
        });
    }

    let mut fetched: Vec<(String, String)> = Vec::new();
    let mut missing: Vec<String> = Vec::new();
    let mut failure: Option<String> = None;

    for pr in named.iter().take(review_subject::MAX_PULL_REQUESTS) {
        sink.event(Event::Working {
            actor: "gh".into(),
            action: format!("fetching {}…", pr.label),
        });
        match review_subject::fetch_diff(pr, repo_dir) {
            Ok(diff) => fetched.push((pr.label.clone(), diff)),
            Err(e) => {
                let text = format!("{:#}", e);
                sink.event(Event::Warn { text: text.clone() });
                failure.get_or_insert(text);
                missing.push(pr.label.clone());
            }
        }
    }
    // Anything past the cap was never attempted. The answer very likely covers
    // it, so it is missing evidence in exactly the same way a failed fetch is.
    missing.extend(
        named.iter().skip(review_subject::MAX_PULL_REQUESTS).map(|pr| pr.label.clone()),
    );

    if fetched.is_empty() {
        return Subject::Absent {
            labels: named.iter().map(|pr| pr.label.clone()).collect(),
            reason: failure.unwrap_or_else(|| "no diff could be fetched".into()),
        };
    }

    let labels: Vec<String> = fetched.iter().map(|(label, _)| label.clone()).collect();
    sink.event(Event::Info {
        text: format!(
            "reviewing against {} — not against the working tree",
            labels.join(", ")
        ),
    });
    let (diff, truncated) = join_fetched_diffs(&fetched);
    Subject::Fetched { diff, labels, missing, truncated }
}

/// One labelled block per pull request, sharing the budget evenly so a single
/// huge change cannot squeeze the others out of the prompt entirely. The flag
/// says whether any of them had to be cut.
fn join_fetched_diffs(parts: &[(String, String)]) -> (String, bool) {
    let share = FETCHED_DIFF_BYTES / parts.len().max(1);
    let mut any_truncated = false;
    let joined = parts
        .iter()
        .map(|(label, diff)| {
            let body = match truncate_bytes(diff, share) {
                (kept, false) => kept.to_string(),
                (kept, true) => {
                    any_truncated = true;
                    format!(
                        "{}\n\n[{} truncated at {} KB — later files are not shown. Say the diff \
                         is insufficient where that matters.]",
                        kept,
                        label,
                        share / 1024,
                    )
                }
            };
            format!("───── {} ─────\n{}", label, body)
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    (joined, any_truncated)
}

/// Why this review must not be run, or `None` when it can be.
///
/// Refusing costs the user the review they asked for, so each refusal says what
/// is missing and how to supply it.
struct Refusal {
    warning: String,
    /// One line, carried as the run's message and shown in a multi-project tally.
    message: &'static str,
}

fn refusal(subject: &Subject, reviewer: &dyn ModelAdapter) -> Option<Refusal> {
    match subject {
        // The reviewer's tools resolve every path the answer cites — in the
        // wrong revision. That reads as verification and is not, so no reviewer
        // is grounded enough for this, tools or no tools.
        Subject::Absent { labels, reason } => Some(Refusal {
            warning: ungrounded_warning(labels, reason, reviewer.name()),
            message: UNGROUNDED_MESSAGE,
        }),
        // No tools and no diff: nothing but the answer's own prose, and an
        // approval drawn from that says only that the answer reads well.
        Subject::Nothing if !reviewer.can_read_files() => Some(Refusal {
            warning: unreviewable_warning(reviewer.name()),
            message: UNREVIEWABLE_MESSAGE,
        }),
        _ => None,
    }
}

fn unreviewable_warning(reviewer_name: &str) -> String {
    format!(
        "{} has no file access and there is nothing uncommitted, so it could only judge the \
         wording of the answer — skipping the review rather than returning an approval it \
         cannot support. {}",
        reviewer_name,
        install_hint(reviewer_name),
    )
}

fn ungrounded_warning(labels: &[String], reason: &str, reviewer_name: &str) -> String {
    let subject = if labels.is_empty() {
        "a change that is not in this checkout".to_string()
    } else {
        labels.join(", ")
    };
    format!(
        "this question is about {}, and {}. {} would read the checkout instead, where every path \
         the answer cites resolves against a different revision — which reads as verification and \
         is not. Skipping the review. Check the change out locally (`gh pr checkout <number>`), or \
         make `gh` available and authenticated, then run the review again.",
        subject, reason, reviewer_name,
    )
}

/// How to give *this* reviewer file access. Either model can hold the reviewer
/// seat — `--writer gemini` puts Claude in it — so naming the wrong CLI would
/// send the user to install something they are not using.
fn install_hint(reviewer_name: &str) -> &'static str {
    match reviewer_name {
        "gemini" => "Install the gemini CLI (`npm i -g @google/gemini-cli`) so the reviewer can read the code.",
        "claude" => "Install the Claude Code CLI, or set [claude] mode = \"cli\", so the reviewer can read the code.",
        _ => "Give the reviewer a CLI transport so it can read the code.",
    }
}

/// The access rules that match both what the reviewer can do and where the code
/// under review actually is. A reviewer holding a fetched diff is told its tools
/// point somewhere else; getting that wrong is the whole bug.
fn access_block(reviewer: &dyn ModelAdapter, subject: &Subject) -> &'static str {
    if !reviewer.can_read_files() {
        return prompts::ANSWER_REVIEW_ACCESS_NONE;
    }
    match subject {
        Subject::Fetched { .. } => prompts::ANSWER_REVIEW_ACCESS_ELSEWHERE,
        _ => prompts::ANSWER_REVIEW_ACCESS_TOOLS,
    }
}

/// The code the answer is judged against, as it appears in the prompt, and
/// whether it had to be cut to fit — the part that decides what a verdict
/// which opened no files is worth.
struct ChangesBlock {
    text: String,
    truncated: bool,
}

/// What a truncated changes block demands of a reviewer that can read: the
/// cut is only survivable if the reviewer covers it, so a review that opens
/// nothing and declares nothing unverified has judged evidence it never saw.
/// Empty for a reviewer without tools, which can only name what it is missing.
fn read_requirement(reviewer: &dyn ModelAdapter) -> &'static str {
    if !reviewer.can_read_files() {
        return "";
    }
    "\n\n[the diff above is incomplete, so a verdict cannot rest on it alone: open the files \
     your judgement leans on with your read-only tools, under the access rules in this prompt, \
     and list what you still could not check under UNVERIFIED. Ending with both `FILES READ: \
     none` and `UNVERIFIED: none` is not an acceptable review of a truncated diff.]"
}

/// The code the answer is judged against, as it appears in the prompt. Says
/// plainly when there is nothing, so the reviewer judges the prose alone
/// instead of assuming evidence it was never given — and, when there is too
/// much, holds a reviewer with tools to reading past the cut.
fn changes_block(subject: &Subject, reviewer: &dyn ModelAdapter) -> ChangesBlock {
    match subject {
        Subject::Nothing => ChangesBlock {
            text: "(nothing uncommitted in the working tree — judge the answer on its own)".into(),
            truncated: false,
        },
        Subject::WorkingTree(diff) => match truncate_bytes(diff, ANSWER_REVIEW_DIFF_BYTES) {
            (kept, false) => ChangesBlock { text: kept.to_string(), truncated: false },
            (kept, true) => ChangesBlock {
                text: format!(
                    "{}\n\n[diff truncated at {} KB — later files are not shown. Judge only what \
                     is above, and say the diff is insufficient where it matters.]{}",
                    kept,
                    ANSWER_REVIEW_DIFF_BYTES / 1024,
                    read_requirement(reviewer),
                ),
                truncated: true,
            },
        },
        Subject::Fetched { diff, labels, missing, truncated } => {
            let mut block = format!(
                "{}, fetched with `gh pr diff`. This — not the working tree of the checkout you \
                 are standing in — is the change under discussion.\n\n{}",
                labels.join(" and "),
                diff,
            );
            if !missing.is_empty() {
                block.push_str(&format!(
                    "\n\n[{} could not be fetched and is not shown here. The answer probably \
                     discusses it: say the evidence is missing rather than judging those claims \
                     from the checkout, which is a different revision.]",
                    missing.join(", "),
                ));
            }
            if *truncated {
                block.push_str(read_requirement(reviewer));
            }
            ChangesBlock { text: block, truncated: *truncated }
        }
        // Refused before a prompt is ever built; stated rather than asserted
        // away, so a future caller that skips the refusal gets an honest block
        // instead of a confident one.
        Subject::Absent { .. } => ChangesBlock {
            text: "(the change under discussion is not in this checkout and could not be fetched)"
                .into(),
            truncated: false,
        },
    }
}

/// Cuts `text` to at most `cap` bytes on a character boundary. The flag says
/// whether anything was dropped.
fn truncate_bytes(text: &str, cap: usize) -> (&str, bool) {
    if text.len() <= cap {
        return (text, false);
    }
    let mut end = cap;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    (&text[..end], true)
}

/// Caps the working-tree diff attached to a code review prompt. A reviewer
/// with tools is told to read past the cut; a reviewer without tools is told
/// to note where the diff is insufficient.
fn capped_review_diff(diff: &str, reviewer: &dyn ModelAdapter) -> (String, bool) {
    let (kept, truncated) = truncate_bytes(diff, REVIEW_DIFF_BYTES);
    if !truncated {
        return (diff.to_string(), false);
    }
    (
        format!(
            "{}\n\n[diff truncated at {} KB — files after the cut are not shown. Say so where \
             the missing part matters.{}]",
            kept,
            REVIEW_DIFF_BYTES / 1024,
            read_requirement(reviewer),
        ),
        true,
    )
}

/// Warns when a review names files it never opened.
///
/// Everything else a review says has to be taken on its word; this does not.
/// The warning is deliberately not a failure: the finding underneath may still
/// be sound, and the reader is the one who should decide what a review that
/// overstated its own work is worth.
fn report_unsupported_reads(reviewer: &dyn ModelAdapter, response: &str, sink: &dyn Sink) {
    let opened = reviewer.files_opened_last_turn();
    let unsupported = policy::unsupported_read_claims(response, &opened);
    if unsupported.is_empty() {
        return;
    }

    sink.event(Event::Warn {
        text: format!(
            "{} listed {} as read, but opened {} this turn — treat that part of the review \
             as unverified",
            reviewer.name(),
            unsupported.join(", "),
            if opened.is_empty() {
                "no files".to_string()
            } else {
                format!("only {}", opened.join(", "))
            },
        ),
    });
}

/// Warns when a verdict was returned over a truncated diff without a single
/// file opened to cover the cut.
///
/// The truncation notice tells a reviewer with tools to read past the cut or
/// name what it took on trust; a review that does neither has judged evidence
/// it never saw. Deliberately not a failure, for the same reason as
/// [`report_unsupported_reads`]: the verdict underneath may still be right,
/// and the reader is the one who should decide what it is worth.
fn report_unread_truncation(reviewer: &dyn ModelAdapter, truncated: bool, sink: &dyn Sink) {
    if !truncated || !reviewer.can_read_files() {
        return;
    }
    if !reviewer.files_opened_last_turn().is_empty() {
        return;
    }
    sink.event(Event::Warn {
        text: format!(
            "{} returned a verdict on a truncated diff without opening a single file — the \
             claims past the cut were taken on trust; treat the verdict as unverified there",
            reviewer.name(),
        ),
    });
}

fn review_repo_block(dir: &Path) -> String {
    format!("{}\n{}", git::repo_identity(dir), prompts::REVIEW_GROUND_RULES)
}

/// The `{repo}` block for a review of a fetched change. Names the pull
/// requests rather than the checkout, and carries the elsewhere ground rules:
/// the working-tree rules would tell the reviewer to announce this repository
/// and trust the files around it, which is exactly what ungrounds a fetched
/// review.
fn fetched_repo_block(labels: &[String]) -> String {
    format!(
        "under review: {} (fetched — not the checkout you are standing in)\n{}",
        labels.join(", "),
        prompts::FETCHED_REVIEW_GROUND_RULES
    )
}

fn build_repo_context(primary: &Path, workspace: &[PathBuf]) -> Result<String> {
    let status = git::git_status(primary).unwrap_or_default();

    // Same identity block the reviewer is given, so the writer acts on findings
    // about the checkout it is actually standing in.
    let mut context = git::repo_identity(primary);
    if !status.trim().is_empty() {
        context.push_str(&format!("working tree status:\n{}\n", status));
    }

    // Siblings are named and located, so a task spanning several repos reasons
    // from all of them instead of guessing at the ones it cannot see. They are
    // marked read-only because only the primary is diffed, checked, reviewed,
    // and logged — an edit anywhere else escapes the loop entirely.
    let siblings = sibling_projects(primary, workspace);
    if siblings.is_empty() {
        return Ok(context);
    }

    context.push_str(
        "\nother projects in this workspace — read them for context, but do NOT \
         edit them. Every change you make belongs in the repository above:\n",
    );
    for dir in siblings {
        context.push_str(&format!("\n{}", git::repo_identity(dir)));
    }

    Ok(context)
}

/// Workspace projects other than the one being written to, in workspace order
/// and without repeats. `workspace` may or may not list the primary itself, so
/// both conventions collapse to the same answer here.
fn sibling_projects<'a>(primary: &Path, workspace: &'a [PathBuf]) -> Vec<&'a PathBuf> {
    let mut siblings: Vec<&PathBuf> = Vec::new();
    for dir in workspace {
        if dir.as_path() != primary && !siblings.contains(&dir) {
            siblings.push(dir);
        }
    }
    siblings
}

/// Warns when a round left changes in a project the task is not writing to.
///
/// The loop only ever diffs, reviews, and runs checks against the primary
/// repository, so an edit in a sibling reaches no reviewer and trips no check.
/// Reporting it turns a silent escape into a visible one. The baseline moves up
/// afterwards, so each stray change is announced once rather than every round.
fn report_stray_writes(session: &mut Session, primary: &Path, sink: &dyn Sink) {
    for (dir, baseline) in &mut session.stray_baseline {
        let current = git::git_diff(dir).unwrap_or_default();
        if current == *baseline {
            continue;
        }
        *baseline = current;

        let name = dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| dir.display().to_string());
        sink.event(Event::Warn {
            text: format!(
                "{} was modified, but this task writes to {} — changes there are \
                 not reviewed, checked, or recorded in the session",
                name,
                primary.display(),
            ),
        });
    }
}

fn ok_result(rounds: usize, message: &str) -> OrchestratorResult {
    OrchestratorResult { outcome: Outcome::Approved, rounds, message: message.into(), answer: None }
}

/// The work stands, but no reviewer ever judged it — reported as neither a pass
/// nor a failure. `answer` carries the writer's text when the run produced one,
/// so an on-demand review still has the thing it needs to judge.
fn unreviewed_result(rounds: usize, message: String, answer: Option<String>) -> OrchestratorResult {
    OrchestratorResult { outcome: Outcome::Unreviewed, rounds, message, answer }
}

/// The result of a run the user stopped mid-flight: announced once, then handed
/// back so the session (REPL or serve) can carry on with the next command.
fn stopped_run(sink: &dyn Sink, rounds: usize) -> OrchestratorResult {
    sink.event(Event::Stopped { text: "stopped — this run ends here, the session continues".into() });
    OrchestratorResult {
        outcome: Outcome::Stopped,
        rounds,
        message: "stopped by user".into(),
        answer: None,
    }
}

/// Closing line for a run that ends with nothing reviewed. A terminal asked and
/// was told no; a frontend with its own review action was never asked at all,
/// so it is pointed at that action instead of being told it declined.
fn unreviewed_message(
    review_on_demand: bool,
    writer: &str,
    reviewer: &str,
    subject: &str,
) -> String {
    if review_on_demand {
        format!("{}'s {} stands unreviewed — run a review to have {} judge it", writer, subject, reviewer)
    } else {
        format!("review declined — {}'s {} stands unreviewed", writer, subject)
    }
}

/// The closing line of an answer run the reviewer found sound. It names what
/// was judged, and carries the answer's own conclusion alongside, because that
/// conclusion is frequently the opposite — a review answer saying "do not merge"
/// is a sound answer. Kept to one line: `dt serve` frontends render this message
/// as a single row.
fn answer_sound_message(conclusion: Option<&str>) -> String {
    const BASE: &str = "the reviewer found the answer SOUND — a verdict on the answer, \
                        not on the code it discusses";
    match conclusion {
        Some(text) => format!("{} · the answer's own conclusion: {}", BASE, text),
        None => BASE.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dirs(paths: &[&str]) -> Vec<PathBuf> {
        paths.iter().map(PathBuf::from).collect()
    }

    /// The verdict block answers "does this merge?", which is only a question
    /// when a pull request is on the table.
    #[test]
    fn only_a_task_naming_a_pull_request_asks_for_a_verdict() {
        assert!(wants_pr_verdict(
            "review https://github.com/acme/api/pull/542 and tell me if it is safe"
        ));
        assert!(wants_pr_verdict(
            "compare github.com/acme/api/pull/542 with github.com/acme/web/pull/462"
        ));
    }

    /// Ordinary work must not be pushed into a pass/fail frame: "GO" answers
    /// nothing about a task that was never a judgement.
    #[test]
    fn ordinary_work_is_left_alone() {
        assert!(!wants_pr_verdict("add a retry to the upload path"));
        assert!(!wants_pr_verdict("why is the checkout test flaky?"));
        assert!(!wants_pr_verdict("fix the bug from ticket 542"));
    }

    /// The gate is the same detector that fetches the diff, so it inherits the
    /// requirement for a real github.com URL. `owner/repo#n` is a label this
    /// code *prints*, never one it reads — a task written that way gets no
    /// verdict block, and would get no fetched diff either.
    #[test]
    fn the_shorthand_label_is_not_a_pull_request_reference() {
        assert!(!wants_pr_verdict("look at acme/api#542"));
        assert!(!wants_pr_verdict("review PR #542"));
    }

    /// A reviewer that fails the test if it is ever called. The gate's whole
    /// value is that the call does not happen.
    struct Reviewer {
        reads_files: bool,
    }

    impl ModelAdapter for Reviewer {
        fn generate(&mut self, _: &str, _: &[ImageInput]) -> Result<(String, UsageStats)> {
            panic!("the reviewer must not be called when there is nothing to review");
        }

        fn name(&self) -> &str {
            "stub"
        }

        fn can_read_files(&self) -> bool {
            self.reads_files
        }
    }

    fn fetched() -> Subject {
        Subject::Fetched {
            diff: "+ fn added() {}".into(),
            labels: vec!["acme/api#529".into()],
            missing: Vec::new(),
            truncated: false,
        }
    }

    fn absent() -> Subject {
        Subject::Absent {
            labels: vec!["acme/api#529".into()],
            reason: "gh is not installed".into(),
        }
    }

    /// A reviewer with no file access, judging an answer with no diff behind
    /// it, has nothing but the prose — and approved it anyway.
    #[test]
    fn a_blind_reviewer_with_no_diff_cannot_review() {
        assert!(refusal(&Subject::Nothing, &Reviewer { reads_files: false }).is_some());
    }

    /// Either kind of evidence is enough to go ahead: tools to read the code,
    /// or a diff to read instead.
    #[test]
    fn evidence_of_either_kind_allows_the_review() {
        assert!(refusal(&Subject::Nothing, &Reviewer { reads_files: true }).is_none());
        let tree = Subject::WorkingTree("+ fn added() {}".into());
        assert!(refusal(&tree, &Reviewer { reads_files: false }).is_none());
    }

    /// The reported bug. The reviewer's tools work perfectly and point at the
    /// wrong revision: every path the answer cites resolves, every line number
    /// reads back, and none of it is the change. Having tools is what makes
    /// this worse than having none, so tools must not excuse it.
    #[test]
    fn a_change_that_is_not_here_is_refused_however_capable_the_reviewer() {
        assert!(refusal(&absent(), &Reviewer { reads_files: true }).is_some());
        assert!(refusal(&absent(), &Reviewer { reads_files: false }).is_some());
    }

    /// Fetching the change is what turns that refusal back into a review.
    #[test]
    fn a_fetched_change_is_reviewable() {
        assert!(refusal(&fetched(), &Reviewer { reads_files: true }).is_none());
        assert!(refusal(&fetched(), &Reviewer { reads_files: false }).is_none());
    }

    /// Telling a tool-capable reviewer it cannot open files is what produced
    /// the split-second approval, so the two must never come apart.
    #[test]
    fn the_access_block_follows_the_reviewers_actual_capability() {
        assert_eq!(
            access_block(&Reviewer { reads_files: true }, &Subject::Nothing),
            prompts::ANSWER_REVIEW_ACCESS_TOOLS
        );
        assert_eq!(
            access_block(&Reviewer { reads_files: false }, &Subject::Nothing),
            prompts::ANSWER_REVIEW_ACCESS_NONE
        );
    }

    /// A reviewer holding a fetched diff has working tools aimed somewhere
    /// else. Handing it the ordinary "open the files the answer cites" rules is
    /// exactly how a review of the wrong revision reads as verification.
    #[test]
    fn a_reviewer_reading_a_fetched_change_is_told_its_tools_point_elsewhere() {
        assert_eq!(
            access_block(&Reviewer { reads_files: true }, &fetched()),
            prompts::ANSWER_REVIEW_ACCESS_ELSEWHERE
        );
    }

    /// With no tools there is nothing to misaim, so the rules stay the ones for
    /// a reviewer that can see only the prompt.
    #[test]
    fn a_blind_reviewer_is_never_told_about_tools_it_does_not_have() {
        assert_eq!(
            access_block(&Reviewer { reads_files: false }, &fetched()),
            prompts::ANSWER_REVIEW_ACCESS_NONE
        );
    }

    /// A pull request on a host `gh` cannot reach is still a change that is not
    /// in this checkout. The directory here does not exist, which proves the
    /// path never falls back to reading the working tree.
    #[test]
    fn an_unfetchable_host_resolves_to_absent_without_reading_the_tree() {
        let sink = AskCounter { asked: std::sync::atomic::AtomicUsize::new(0) };
        let subject = resolve_subject(
            "review https://git.acme.corp/acme/api/pull/529",
            Path::new("/nonexistent-checkout"),
            &sink,
        );
        match subject {
            Subject::Absent { labels, reason } => {
                assert!(labels.is_empty());
                assert!(reason.contains("no GitHub pull request URL"), "reason: {reason}");
            }
            _ => panic!("a pull request that cannot be fetched must not be reviewable"),
        }
    }

    /// The reported bug, review-command edition: "review <PR link>" was judged
    /// against whatever happened to be uncommitted, because the link rode along
    /// as prose while the working tree was diffed. A task naming a pull request
    /// that cannot be fetched must now refuse — never call the reviewer on the
    /// tree — and the nonexistent directory proves no tree was ever read.
    #[test]
    fn a_review_task_naming_a_pull_request_never_reviews_the_working_tree() {
        let config = Config::default();
        let sink = AskCounter { asked: std::sync::atomic::AtomicUsize::new(0) };
        let mut reviewer = Reviewer { reads_files: true };
        let result = review_only(
            &config,
            &mut reviewer,
            Path::new("/nonexistent-checkout"),
            Some("review https://github.com/acme/api/pull/529"),
            &sink,
        )
        .expect("a refused review is a result, not an error");
        assert_eq!(result.outcome, Outcome::Unreviewed);
    }

    /// The `{repo}` block of a fetched review must name the pull request and
    /// carry the elsewhere rules — handing it the checkout's identity is what
    /// aimed the reviewer at the wrong code.
    #[test]
    fn a_fetched_review_is_grounded_in_the_pull_request_not_the_checkout() {
        let block = fetched_repo_block(&["acme/api#529".into(), "acme/ui#451".into()]);
        assert!(block.contains("acme/api#529, acme/ui#451"));
        assert!(block.contains(prompts::FETCHED_REVIEW_GROUND_RULES));
        assert!(!block.contains(prompts::REVIEW_GROUND_RULES));
    }

    /// The refusal is only useful if it says what was missing and what to do.
    #[test]
    fn the_ungrounded_refusal_names_the_change_and_the_way_out() {
        let warning = ungrounded_warning(&["acme/api#529".into()], "gh is not installed", "gemini");
        assert!(warning.contains("acme/api#529"));
        assert!(warning.contains("gh is not installed"));
        assert!(warning.contains("gh pr checkout"));
        assert!(warning.contains("gemini"));
    }

    /// The refusal has to tell the user how to get a real review.
    #[test]
    fn the_refusal_names_the_reviewer_and_the_fix() {
        let warning = unreviewable_warning("gemini");
        assert!(warning.contains("gemini"));
        assert!(warning.contains("npm i -g @google/gemini-cli"));
    }

    /// Either model can hold the reviewer seat — `--writer gemini` puts Claude
    /// there — so the fix offered must be for the reviewer actually in use.
    #[test]
    fn the_fix_offered_matches_the_reviewer_in_the_seat() {
        assert!(unreviewable_warning("claude").contains("Claude Code CLI"));
        assert!(!unreviewable_warning("claude").contains("gemini-cli"));
        assert!(!unreviewable_warning("gemini").contains("Claude Code CLI"));
    }

    /// The workspace lists every project including the one being written to;
    /// only the others are context.
    #[test]
    fn siblings_exclude_the_project_being_written_to() {
        let workspace = dirs(&["/w/api", "/w/web"]);
        let found = sibling_projects(Path::new("/w/api"), &workspace);
        assert_eq!(found, vec![&PathBuf::from("/w/web")]);
    }

    #[test]
    fn siblings_keep_workspace_order_without_repeats() {
        let workspace = dirs(&["/w/web", "/w/api", "/w/web"]);
        let found = sibling_projects(Path::new("/w/api"), &workspace);
        assert_eq!(found, vec![&PathBuf::from("/w/web")]);
    }

    /// How the plain CLI, the REPL, and a single-folder window all run: the
    /// same code path, with nothing beside the primary.
    #[test]
    fn a_lone_project_has_no_siblings() {
        assert!(sibling_projects(Path::new("/w/api"), &[]).is_empty());
        assert!(sibling_projects(Path::new("/w/api"), &dirs(&["/w/api"])).is_empty());
    }

    /// A trailing separator is the same directory, so it must not turn the
    /// primary into its own sibling — which would warn about every edit.
    #[test]
    fn a_trailing_separator_still_names_the_primary() {
        assert!(sibling_projects(Path::new("/w/api"), &dirs(&["/w/api/"])).is_empty());
    }

    /// An answer with no changes behind it must say so, or the reviewer reads
    /// an empty section as evidence it was given and never looked at.
    #[test]
    fn an_empty_tree_is_named_rather_than_left_blank() {
        let block = changes_block(&Subject::Nothing, &Reviewer { reads_files: true });
        assert!(block.text.contains("nothing uncommitted"));
        assert!(!block.truncated);
    }

    #[test]
    fn a_diff_within_the_cap_is_attached_whole() {
        let diff = "diff --git a/src/lib.rs b/src/lib.rs\n+fn added() {}\n";
        let block =
            changes_block(&Subject::WorkingTree(diff.into()), &Reviewer { reads_files: true });
        assert_eq!(block.text, diff);
        assert!(!block.truncated);
    }

    /// A truncated diff must announce the truncation: a reviewer that thinks it
    /// saw every file will report absences that are only missing from the cut.
    /// One with tools is also told a review that opens nothing and declares
    /// nothing unverified is unacceptable — the demand a zero-read rubber
    /// stamp slipped past.
    #[test]
    fn an_oversized_diff_is_cut_and_says_so() {
        let diff = "x".repeat(ANSWER_REVIEW_DIFF_BYTES * 2);
        let block =
            changes_block(&Subject::WorkingTree(diff.clone()), &Reviewer { reads_files: true });
        assert!(block.text.contains("diff truncated"));
        assert!(block.text.contains("not an acceptable review"));
        assert!(block.text.len() < diff.len());
        assert!(block.truncated);
    }

    /// The read demand would be a taunt to a reviewer with no tools — it can
    /// only name what it is missing, and the truncation notice already says so.
    #[test]
    fn a_blind_reviewer_is_not_told_to_read() {
        let diff = "x".repeat(ANSWER_REVIEW_DIFF_BYTES * 2);
        let block =
            changes_block(&Subject::WorkingTree(diff), &Reviewer { reads_files: false });
        assert!(block.text.contains("diff truncated"));
        assert!(!block.text.contains("not an acceptable review"));
        assert!(block.truncated);
    }

    /// A fetched diff has to be labelled as the subject. A reviewer that reads
    /// it as "some diff" falls back on the checkout for anything it does not
    /// cover — which is the wrong revision.
    #[test]
    fn a_fetched_change_is_named_and_set_against_the_checkout() {
        let block = changes_block(&fetched(), &Reviewer { reads_files: true });
        assert!(block.text.contains("acme/api#529"));
        assert!(block.text.contains("not the working tree"));
        assert!(block.text.contains("+ fn added() {}"));
        assert!(!block.truncated);
    }

    /// A cut fetched diff carries the same read demand as a cut working tree:
    /// this is the exact shape of the zero-read rubber stamp observed in the
    /// wild — 117 KB of truncated PR, `FILES READ: none`, `VERDICT: SOUND`.
    #[test]
    fn a_truncated_fetched_change_demands_reads_of_a_reviewer_with_tools() {
        let subject = Subject::Fetched {
            diff: "+ fn added() {}".into(),
            labels: vec!["acme/api#529".into()],
            missing: Vec::new(),
            truncated: true,
        };
        let block = changes_block(&subject, &Reviewer { reads_files: true });
        assert!(block.text.contains("not an acceptable review"));
        assert!(block.truncated);

        let blind = changes_block(&subject, &Reviewer { reads_files: false });
        assert!(!blind.text.contains("not an acceptable review"));
        assert!(blind.truncated);
    }

    /// The pull requests share the budget, so one enormous change cannot push
    /// the others out of the prompt entirely.
    #[test]
    fn fetched_pull_requests_are_labelled_and_share_the_budget() {
        let parts = vec![
            ("acme/api#1".to_string(), "x".repeat(FETCHED_DIFF_BYTES * 2)),
            ("acme/ui#2".to_string(), "+ small\n".to_string()),
        ];
        let (joined, truncated) = join_fetched_diffs(&parts);
        assert!(joined.contains("acme/api#1"));
        assert!(joined.contains("acme/ui#2"));
        assert!(joined.contains("truncated at"));
        assert!(truncated);
        // The small one survives whole even though the first blew its share.
        assert!(joined.contains("+ small"));
        assert!(joined.len() < FETCHED_DIFF_BYTES + 1_000);
    }

    /// Two pull requests where only one fetches. Reviewing the half that
    /// arrived is right; letting the reviewer settle claims about the other
    /// half from the checkout is the exact failure being fixed, so the gap is
    /// named in the prompt rather than left silent.
    #[test]
    fn a_change_that_did_not_arrive_is_named_as_missing() {
        let partial = Subject::Fetched {
            diff: "+ fn added() {}".into(),
            labels: vec!["acme/api#529".into()],
            missing: vec!["acme/ui#451".into()],
            truncated: false,
        };
        let block = changes_block(&partial, &Reviewer { reads_files: true });
        assert!(block.text.contains("acme/api#529"));
        assert!(block.text.contains("acme/ui#451"));
        assert!(block.text.contains("could not be fetched"));
        assert!(block.text.contains("say the evidence is missing"));
    }

    /// Refused before any prompt is built — but if a future caller ever skips
    /// the refusal, the reviewer must be told the change is missing rather than
    /// handed a blank it will read as "no changes".
    #[test]
    fn an_unfetchable_change_is_never_described_as_nothing() {
        let block = changes_block(&absent(), &Reviewer { reads_files: true });
        assert!(block.text.contains("could not be fetched"));
    }

    /// A reviewer that read files during its turn, for the truncation warning
    /// tests: the warning is about doing no reading at all, not about which
    /// files were chosen.
    struct ReadingReviewer {
        opened: Vec<String>,
    }

    impl ModelAdapter for ReadingReviewer {
        fn generate(&mut self, _: &str, _: &[ImageInput]) -> Result<(String, UsageStats)> {
            panic!("the truncation warning must not call the reviewer");
        }

        fn name(&self) -> &str {
            "stub"
        }

        fn can_read_files(&self) -> bool {
            true
        }

        fn files_opened_last_turn(&self) -> Vec<String> {
            self.opened.clone()
        }
    }

    /// Collects warnings, so a test can assert one fired without a terminal.
    struct WarnCollector(std::sync::Mutex<Vec<String>>);

    impl Sink for WarnCollector {
        fn event(&self, event: Event) {
            if let Event::Warn { text } = event {
                self.0.lock().unwrap().push(text);
            }
        }

        fn ask(&self, _kind: AskKind, _question: &str) -> String {
            String::new()
        }
    }

    /// The observed failure: a verdict on a truncated diff from a reviewer
    /// that opened nothing. The warning fires only when every part lines up —
    /// a cut diff, a reviewer with tools, and not one file read to cover it.
    #[test]
    fn a_zero_read_verdict_on_a_truncated_diff_is_warned_about() {
        let fires = |truncated: bool, opened: Vec<String>| {
            let sink = WarnCollector(std::sync::Mutex::new(Vec::new()));
            report_unread_truncation(&ReadingReviewer { opened }, truncated, &sink);
            !sink.0.into_inner().unwrap().is_empty()
        };

        assert!(fires(true, Vec::new()));
        assert!(!fires(false, Vec::new()));
        assert!(!fires(true, vec!["src/lib.rs".into()]));

        // A reviewer with no tools cannot be blamed for not using them.
        let sink = WarnCollector(std::sync::Mutex::new(Vec::new()));
        report_unread_truncation(&Reviewer { reads_files: false }, true, &sink);
        assert!(sink.0.lock().unwrap().is_empty());
    }

    /// Records whether the loop stopped to ask, and answers yes if it did.
    struct AskCounter {
        asked: std::sync::atomic::AtomicUsize,
    }

    impl Sink for AskCounter {
        fn event(&self, _event: Event) {}

        fn ask(&self, _kind: AskKind, _question: &str) -> String {
            self.asked.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            "y".into()
        }
    }

    fn review_decision(auto: bool, review_on_demand: bool) -> (bool, usize) {
        let config = Config::default();
        let opts = TaskOptions {
            config: &config,
            task: "t",
            images: &[],
            repo_dir: Path::new("/w/api"),
            workspace: &[],
            continue_session: false,
            auto,
            plan_first: false,
            review_on_demand,
        };
        let sink = AskCounter { asked: std::sync::atomic::AtomicUsize::new(0) };
        let wants = wants_review(&opts, "gemini", "review changes", &sink);
        (wants, sink.asked.load(std::sync::atomic::Ordering::Relaxed))
    }

    /// The panel offers its own review action, so the loop must not block on a
    /// prompt — and must not spend a reviewer call the user never asked for.
    #[test]
    fn reviewing_on_demand_neither_asks_nor_reviews() {
        assert_eq!(review_decision(false, true), (false, 0));
    }

    /// Auto mode is the two-brain loop: review every round, ask nothing.
    #[test]
    fn auto_mode_reviews_without_asking_even_on_demand() {
        assert_eq!(review_decision(true, true), (true, 0));
        assert_eq!(review_decision(true, false), (true, 0));
    }

    /// A terminal has no review button, so it is asked.
    #[test]
    fn a_terminal_run_is_asked_whether_to_review() {
        assert_eq!(review_decision(false, false), (true, 1));
    }

    /// Cutting mid-character would panic on a slice; multi-byte diffs are
    /// ordinary in comments and strings.
    #[test]
    fn truncation_lands_on_a_character_boundary() {
        let text = "é".repeat(100);
        let (kept, cut) = truncate_bytes(&text, 51);
        assert!(cut);
        assert_eq!(kept.len(), 50);
        assert!(kept.chars().all(|c| c == 'é'));
    }

    /// A code review diff is capped and the reviewer is told when it is cut.
    #[test]
    fn a_code_review_diff_is_capped_and_announced() {
        let diff = "x".repeat(REVIEW_DIFF_BYTES * 2);
        let (text, truncated) = capped_review_diff(&diff, &Reviewer { reads_files: true });
        assert!(text.contains("diff truncated"));
        assert!(text.contains("not an acceptable review"));
        assert!(truncated);

        let (text, truncated) = capped_review_diff(&diff, &Reviewer { reads_files: false });
        assert!(text.contains("diff truncated"));
        assert!(!text.contains("not an acceptable review"));
        assert!(truncated);

        let small = "+fn added() {}";
        let (text, truncated) = capped_review_diff(small, &Reviewer { reads_files: true });
        assert_eq!(text, small);
        assert!(!truncated);
    }

    const DIFF: &str = "diff --git a/src/lib.rs b/src/lib.rs\n+fn added() {}\n";
    const STALE: &str = "diff --git a/.gitignore b/.gitignore\n+.duet/sessions/\n";

    #[test]
    fn new_code_is_reviewable() {
        assert!(wrote_reviewable_code("", DIFF));
    }

    #[test]
    fn further_edits_on_top_of_existing_work_are_reviewable() {
        assert!(wrote_reviewable_code(STALE, &format!("{}{}", STALE, DIFF)));
    }

    #[test]
    fn clean_tree_and_no_edits_is_not_reviewable() {
        assert!(!wrote_reviewable_code("", ""));
    }

    /// A prose answer leaves the worktree exactly as it found it. If the tree
    /// was already dirty, that pre-existing diff belongs to whoever made it —
    /// routing it to the code reviewer reviews the wrong thing entirely.
    #[test]
    fn untouched_pre_existing_changes_are_not_the_writers_work() {
        assert!(!wrote_reviewable_code(STALE, STALE));
    }

    /// Reverting the tree back to clean changes the diff but leaves nothing to
    /// review; an empty diff can only ever be approved.
    #[test]
    fn revert_back_to_clean_is_not_reviewable() {
        assert!(!wrote_reviewable_code(STALE, ""));
    }
}

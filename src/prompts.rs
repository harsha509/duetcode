use anyhow::{Context, Result};
use std::path::Path;

pub fn load_template(path: &Path) -> Result<String> {
    std::fs::read_to_string(path)
        .with_context(|| format!("failed to read prompt template: {}", path.display()))
}

pub fn render(template: &str, vars: &[(&str, &str)]) -> String {
    let mut result = template.to_string();
    for (key, value) in vars {
        result = result.replace(&format!("{{{}}}", key), value);
    }
    result
}

fn context_with_session(context: &str, previous_session: &str) -> String {
    if previous_session.is_empty() {
        context.to_string()
    } else {
        format!("{}\n\nPREVIOUS SESSION CONTEXT:\n{}", context, previous_session)
    }
}

pub fn build_implement_prompt(template: &str, task: &str, context: &str, previous_session: &str) -> String {
    render(template, &[("task", task), ("context", &context_with_session(context, previous_session))])
}

/// Guardrails against the reviewer inventing findings it cannot see. A diff
/// shows changed hunks, not whole files, so "this symbol is undefined" is only
/// knowable when the diff itself deletes the definition.
pub const REVIEW_GROUND_RULES: &str = r#"REQUIRED — the first line of your reply must be exactly:
Reviewing <repository> (<branch>) — <n> file(s): <comma-separated paths>
taking every value from the block above. Write that line before anything else.

Then, while reviewing:
- Every file you mention must appear in the changed-files list above. If a file is not on
  that list, it is not part of these changes — do not review it or invent findings about it.
- You are reading a diff, not whole files. Lines with no `+`/`-` marker are unchanged
  context, and code outside the diff still exists.
- Never claim a symbol is undefined, removed, unused, or left over from a refactor unless a
  `-` line in this diff actually deletes it. If you cannot see its definition, say the diff
  is insufficient to judge and ask for the file — do not guess.
- Cite the file path with any line number you give, and only for lines present in the diff."#;

/// Appends a `{key}` section to a template that lacks the placeholder, so
/// prompt files written before the placeholder existed still receive the
/// content. Appended rather than prefixed: instructions closest to the end of
/// the prompt are the ones models actually follow.
pub fn ensure_placeholder(template: &str, key: &str, heading: &str) -> String {
    let placeholder = format!("{{{}}}", key);
    if template.contains(&placeholder) {
        return template.to_string();
    }
    format!("{}\n\n{}\n{}\n", template, heading, placeholder)
}

pub fn build_review_prompt(
    template: &str,
    task: &str,
    repo: &str,
    diff: &str,
    checks: &str,
    writer_notes: &str,
) -> String {
    let template = ensure_placeholder(template, "repo", "REPOSITORY UNDER REVIEW:");
    render(
        &template,
        &[
            ("task", task),
            ("repo", repo),
            ("diff", diff),
            ("checks", checks),
            ("writer_notes", writer_notes),
        ],
    )
}

pub fn build_fix_prompt(template: &str, task: &str, review_feedback: &str) -> String {
    render(
        template,
        &[("task", task), ("review_feedback", review_feedback)],
    )
}

pub fn build_plan_prompt(template: &str, task: &str, context: &str, previous_session: &str) -> String {
    render(template, &[("task", task), ("context", &context_with_session(context, previous_session))])
}

pub fn build_plan_review_prompt(template: &str, task: &str, plan: &str) -> String {
    render(template, &[("task", task), ("plan", plan)])
}

/// `changes` is the working tree the answer is about. An answer leaves no diff
/// of its own, so without it the reviewer can only judge the prose; with it the
/// reviewer can check the answer against the code it describes.
///
/// `access` states what the reviewer may actually do — see
/// [`ANSWER_REVIEW_ACCESS_TOOLS`] and [`ANSWER_REVIEW_ACCESS_NONE`]. Getting it
/// wrong in either direction ruins the review: a reviewer with tools that is
/// told it has none will not open a single file, and one without tools that is
/// told it has them will invent what it "found".
pub fn build_answer_review_prompt(
    template: &str,
    task: &str,
    answer: &str,
    changes: &str,
    access: &str,
    repo: &str,
) -> String {
    render(
        template,
        &[
            ("task", task),
            ("answer", answer),
            ("changes", changes),
            ("access", access),
            ("repo", repo),
        ],
    )
}

/// Access rules for a reviewer running inside the checkout with read-only
/// tools. Its whole point is the instruction to *go and look*: the reviewer
/// this replaced was told it could not, and so approved answers it had no way
/// to check.
pub const ANSWER_REVIEW_ACCESS_TOOLS: &str = r#"What you can and cannot check:
- You are running inside the repository described above, with read-only tools. Use them. Open the
  files the answer cites and read the lines it points at. A claim you could have checked and did
  not is a claim you failed to review.
- Go looking for what the answer left out, not only for what it got wrong: the call site it never
  mentions, the other branch, the caller that makes its conclusion moot. An answer can be accurate
  in every line it wrote and still be wrong about the codebase.
- A cited path that does not exist, or a cited line that says something other than what the answer
  claims, is a blocker. Name the file and say what you found there instead.
- You cannot edit anything, and must not try. Never run `git add`, `git commit`, or `git push`.
- Never write "verified", "confirmed", or "I checked" about a claim you did not actually open the
  file to check. Restating a claim is not verifying it, and reporting verification you did not
  perform is a failed review however correct the claim later turns out to be."#;

/// Access rules for a reviewer whose tools work, but not on the code under
/// review: the change was fetched from elsewhere, and the checkout the reviewer
/// stands in is a different revision of it — or a different project entirely.
///
/// Telling *this* reviewer to "open the files the answer cites" is precisely
/// what produced the failure these rules exist to stop. Every path resolves,
/// every line number reads back cleanly, and none of it is the change under
/// review; the review comes back corroborating claims it never checked.
pub const ANSWER_REVIEW_ACCESS_ELSEWHERE: &str = r#"What you can and cannot check:
- The changes above are the code under review, and you are NOT standing in it. The checkout around
  you is a different revision of the same project, or a different project altogether. Any file you
  open here may be older than the change, newer than it, or unrelated to it.
- Settle every claim about what the change does, adds, or removes against the diff above. That is
  the only place those claims can be settled.
- Your read-only tools are still worth using, for what the diff cannot show: a definition the change
  calls but does not touch, a caller it never mentions, a convention it departs from. Whenever you
  use them, say so, and say the file came from the surrounding checkout rather than from the change.
- Never quote a line number from a file you opened as though it were a line of the change. Never
  report a claim as confirmed because the checkout agrees with it — the checkout can agree with an
  answer that is wrong about the change, and contradict one that is right.
- You cannot edit anything, and must not try. Never run `git add`, `git commit`, or `git push`.
- Never write "verified", "confirmed", or "I checked" about a claim you settled anywhere other than
  the diff above, or a file you actually opened and quoted as being outside the change."#;

/// Access rules for an API-only reviewer, which has no tools and can see
/// nothing but this prompt.
pub const ANSWER_REVIEW_ACCESS_NONE: &str = r#"What you can and cannot check:
- The question, the answer, and the changes above are everything you have. You cannot open a
  repository, a file, a pull request, or a URL, and you cannot run anything.
- Check the answer against those changes wherever they bear on it. An answer that describes,
  judges, or draws conclusions about this code can be contradicted by it: say so when the diff
  does not support what the answer claims, and say so when the answer misses something the diff
  plainly shows.
- The changes are a diff, not whole files. Unmarked lines are unchanged context and the code
  around them still exists — never call a symbol undefined or removed unless a `-` line deletes
  it. Where the diff is too narrow to settle a claim, say that instead of guessing.
- Never write "verified", "confirmed", "I checked", or any equivalent about a claim whose evidence
  is neither quoted in the answer nor visible in the changes above. Restating a claim is not
  verifying it. Reporting verification you did not perform is a failed review, however correct the
  claim later turns out to be.
- You can still judge reasoning: conclusions that do not follow from the evidence given, internal
  contradictions, parts of the question left unanswered, and claims asserted with more confidence
  than their stated support carries."#;

pub fn build_answer_fix_prompt(template: &str, task: &str, review_feedback: &str) -> String {
    render(
        template,
        &[("task", task), ("review_feedback", review_feedback)],
    )
}

pub fn build_implement_with_plan_prompt(template: &str, task: &str, context: &str, plan: &str) -> String {
    let full_context = format!("{}\n\nAPPROVED PLAN:\n{}", context, plan);
    render(template, &[("task", task), ("context", &full_context)])
}

pub const DEFAULT_IMPLEMENT_TEMPLATE: &str = r#"You are an expert software engineer working in the current repository.

TASK: {task}

REPOSITORY CONTEXT:
{context}

Rules:
- If the task is a question, answer it directly — do NOT make code changes
- If the task requires code changes, make only the changes necessary
- Follow existing code style and conventions
- Do not remove or break existing functionality
- NEVER run `git add`, `git commit`, or `git push`. Only edit the files and leave them uncommitted.

After completing the task, briefly explain what you did and why.
"#;

pub const DEFAULT_REVIEW_TEMPLATE: &str = r#"You are a senior code reviewer. You are reviewing uncommitted changes in a codebase.

TASK: {task}

WRITER'S NOTES (what the author says they did and why):
{writer_notes}

DIFF:
{diff}

CHECK RESULTS:
{checks}

Follow this review process:

1. UNDERSTAND THE CHANGES
   Read the diff carefully. Identify what files were changed, what was added, modified, or removed. Determine the purpose and intent behind these changes. If you cannot understand what the changes are trying to accomplish, flag that as a serious issue — unclear changes indicate poor code clarity.

2. ANALYZE THE CODEBASE CONTEXT
   Based on the file paths, naming conventions, and surrounding code visible in the diff, understand how these changes fit into the broader codebase. Consider whether the changes follow existing patterns and conventions.

3. VERIFY CORRECTNESS
   Check if the implementation is logically correct. Look for bugs, off-by-one errors, race conditions, null/undefined handling, and incorrect assumptions.

4. CHECK EDGE CASES
   Identify edge cases relevant to what the code does. Are they handled? What happens with empty inputs, large inputs, concurrent access, error conditions, or unexpected state?

5. ASSESS IMPACT
   Could these changes break existing functionality? Does the change do more, or less, than the task asked for? Consider how far the new logic reaches: work threaded through a shared path carries risk beyond its own purpose, where a narrow entry point would not. Where you suspect a caller, import, or definition *outside* the diff is affected, say what you suspect and what would settle it — do not assert it. The diff rule below is not a reason to stay silent, only a reason to phrase it as a question.

6. CHECK THE DESIGN
   Judge the changed code against the principles it should meet: single responsibility, honest names, small focused functions, no duplication, no dead code, clear control flow, and types that describe the data instead of escaping the type system. Hold the new and changed code to this — not the untouched code around it, which is not what is being reviewed.

7. CHECK THAT IT CAN LAND
   Say so if the change cannot be applied in the order it assumes: a migration that only compiles once it has already run, a test that cannot pass until the feature ships and a feature gated on that test, a rename whose two halves each need the other to go first. Work shaped like this looks fine in a diff and cannot be carried out.

8. SUGGESTIONS
   Offer concrete, actionable improvements. Not style nitpicks — focus on correctness, robustness, and maintainability.

Rules:
- NEVER run `git add`, `git commit`, or `git push`. You are a reviewer only.
- Be direct and honest. Do not pad your review with generic praise.
- A diff shows changed hunks, not whole files. Unmarked lines are unchanged context and the
  code around them still exists. Never report a symbol as undefined, removed, or left over
  unless a `-` line in this diff deletes it — say the diff is insufficient instead.
- The writer's notes are a claim, not evidence. Where a note asserts something the diff does not
  show, treat it as unverified rather than established.

REPOSITORY UNDER REVIEW:
{repo}

Finish with these three sections, in this order:

BLOCKERS:
- one line per defect that has to be fixed before this can merge
- write a single bullet reading `none` when there are none

SUGGESTIONS:
- one line per improvement that is not blocking
- write a single bullet reading `none` when there are none

VERDICT: APPROVED

Write `VERDICT: CHANGES_REQUESTED` on that last line instead whenever BLOCKERS has any entry.

Every blocker and suggestion must be its own `- ` bullet. These lines are read back by the loop
to decide whether successive rounds are converging on the same complaint; a finding written as
prose here is invisible to it, however clearly you argued it above.
"#;

pub const DEFAULT_FIX_TEMPLATE: &str = r#"You are an expert software engineer. A reviewer found issues with your implementation. Address the feedback below.

TASK: {task}

REVIEWER FEEDBACK:
{review_feedback}

Rules:
- NEVER run `git add`, `git commit`, or `git push`. Only edit the files and leave them uncommitted.

Fix the issues the reviewer raised. Explain what you changed and why.
"#;

pub const DEFAULT_PLAN_TEMPLATE: &str = r#"You are an expert software engineer. Create a detailed plan for the following task. Do NOT make any code changes yet — just describe your approach.

TASK: {task}

REPOSITORY CONTEXT:
{context}

Cover:
- Which files you'd create or modify
- What approach you'd take and why
- Any risks or trade-offs
- Rough order of operations

Rules:
- NEVER run `git add`, `git commit`, or `git push`. This is a planning phase only.
"#;

pub const DEFAULT_PLAN_REVIEW_TEMPLATE: &str = r#"You are a senior software architect. A developer has proposed the following plan. Review it.

TASK: {task}

PROPOSED PLAN:
{plan}

Give your honest assessment. Is this the right approach? Anything missing? Any risks?

Rules:
- NEVER run `git add`, `git commit`, or `git push`. You are a reviewer only.

At the end, write one of these on its own line:
VERDICT: APPROVED
VERDICT: CHANGES_REQUESTED
"#;

pub const DEFAULT_ANSWER_REVIEW_TEMPLATE: &str = r#"You are a senior engineer giving a second opinion. Another engineer investigated the question below and wrote the answer that follows. Judge that answer.

TASK / QUESTION: {task}

REPOSITORY UNDER REVIEW:
{repo}

THEIR ANSWER:
{answer}

CHANGES THE ANSWER IS ABOUT:
{changes}

Assess whether the reasoning is sound, internally consistent, and actually answers the question. Flag anything wrong, contradictory, or missing. Do not make any code changes.

{access}

Rules:
- NEVER run `git add`, `git commit`, or `git push`. You are a reviewer only.
- Be direct. If the answer is sound and complete, approve it. Do not pad with generic praise.

Finish with these lines, in this order:

FILES READ: <every file you actually opened, comma-separated — or "none">
UNVERIFIED: <the load-bearing claims you had to take on trust, comma-separated — or "none, the answer quotes its evidence">
THEIR CONCLUSION: <the answer's own bottom line, in one line, in its words — what it decides or recommends>

BLOCKERS:
- one line per thing that has to change before this answer is sound
- write a single bullet reading `none` when there are none

VERDICT: SOUND

Write `VERDICT: UNSOUND` on that last line instead when the answer is not sound.

SOUND judges this answer, and nothing else — which is why it is not the word a code review uses.
It is not an endorsement of whatever the answer is about: if the answer recommends against merging
a change, calling the answer sound means you agree that change should not be merged.
"#;

pub const DEFAULT_ANSWER_FIX_TEMPLATE: &str = r#"You are an expert software engineer. A reviewer checked the answer you gave and found issues. Revise your answer.

TASK / QUESTION: {task}

REVIEWER FEEDBACK:
{review_feedback}

Rules:
- This is a question/analysis task — do NOT make code changes unless the feedback explicitly requires them.
- NEVER run `git add`, `git commit`, or `git push`.

Give the corrected, complete answer.
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_replaces_all_placeholders() {
        let out = render("do {task} in {context}", &[("task", "X"), ("context", "Y")]);
        assert_eq!(out, "do X in Y");
    }

    #[test]
    fn render_leaves_unknown_placeholders_untouched() {
        let out = render("do {task} with {unknown}", &[("task", "X")]);
        assert_eq!(out, "do X with {unknown}");
    }

    #[test]
    fn review_prompt_includes_writer_notes() {
        let out = build_review_prompt(DEFAULT_REVIEW_TEMPLATE, "t", "r", "d", "c", "my notes");
        assert!(out.contains("my notes"));
        assert!(!out.contains("{writer_notes}"));
    }

    #[test]
    fn review_prompt_names_the_repository() {
        let repo = "repository: agentic-testing-service";
        let out = build_review_prompt(DEFAULT_REVIEW_TEMPLATE, "t", repo, "d", "c", "n");
        assert!(out.contains(repo));
        assert!(!out.contains("{repo}"));
    }

    /// Prompt files written by an older `dt init` have no `{repo}` placeholder;
    /// the repository block must still reach the reviewer.
    #[test]
    fn legacy_template_without_repo_placeholder_still_gets_the_block() {
        let legacy = "You are a reviewer.\n\nDIFF:\n{diff}\n";
        let out = build_review_prompt(legacy, "t", "repository: my-service", "d", "c", "n");
        assert!(out.contains("repository: my-service"));
        assert!(out.contains("REPOSITORY UNDER REVIEW:"));
        assert!(out.contains("You are a reviewer."));
    }

    /// The answer is judged against the code it is about, so the reviewer can
    /// contradict it instead of taking every claim on trust.
    #[test]
    fn answer_review_prompt_carries_the_changes() {
        let out = build_answer_review_prompt(
            DEFAULT_ANSWER_REVIEW_TEMPLATE,
            "t",
            "their answer",
            "+ fn added() {}",
            ANSWER_REVIEW_ACCESS_NONE,
            "repository: my-service",
        );
        assert!(out.contains("their answer"));
        assert!(out.contains("+ fn added() {}"));
        assert!(out.contains("repository: my-service"));
        assert!(!out.contains("{changes}"));
        assert!(!out.contains("{access}"));
        assert!(!out.contains("{repo}"));
    }

    /// A reviewer with tools must be told to use them. The template this
    /// replaced said the opposite to every reviewer, tools or not, which is why
    /// answers came back approved with nothing opened.
    #[test]
    fn access_block_matches_what_the_reviewer_can_actually_do() {
        let with_tools = build_answer_review_prompt(
            DEFAULT_ANSWER_REVIEW_TEMPLATE, "t", "a", "d", ANSWER_REVIEW_ACCESS_TOOLS, "r",
        );
        assert!(with_tools.contains("read-only tools. Use them."));
        assert!(!with_tools.contains("You cannot open a"));

        let without = build_answer_review_prompt(
            DEFAULT_ANSWER_REVIEW_TEMPLATE, "t", "a", "d", ANSWER_REVIEW_ACCESS_NONE, "r",
        );
        assert!(without.contains("You cannot open a"));
        assert!(!without.contains("read-only tools. Use them."));
    }

    /// The three access blocks say genuinely different things, and each has one
    /// job. Their whole purpose is that a reviewer is never told it can do
    /// something it cannot, nor told to do something that would mislead it.
    #[test]
    fn each_access_block_says_something_the_others_do_not() {
        let elsewhere = build_answer_review_prompt(
            DEFAULT_ANSWER_REVIEW_TEMPLATE, "t", "a", "d", ANSWER_REVIEW_ACCESS_ELSEWHERE, "r",
        );
        assert!(elsewhere.contains("you are NOT standing in it"));
        // The instruction that produced the failure: it must not reach a
        // reviewer whose tools point at a different revision.
        assert!(!elsewhere.contains("Open the files the answer cites"));
        assert!(!elsewhere.contains("You cannot open a"));
    }

    /// An answer is judged sound or unsound, never approved — a word that would
    /// read as a verdict on the code the answer is about.
    #[test]
    fn the_answer_verdict_has_its_own_vocabulary() {
        assert!(DEFAULT_ANSWER_REVIEW_TEMPLATE.contains("VERDICT: SOUND"));
        assert!(DEFAULT_ANSWER_REVIEW_TEMPLATE.contains("VERDICT: UNSOUND"));
        assert!(!DEFAULT_ANSWER_REVIEW_TEMPLATE.contains("VERDICT: APPROVED"));
        assert!(DEFAULT_REVIEW_TEMPLATE.contains("VERDICT: APPROVED"));
    }

    /// A review that opened nothing has to say so on the record.
    #[test]
    fn answer_review_prompt_demands_the_files_read() {
        let out = build_answer_review_prompt(
            DEFAULT_ANSWER_REVIEW_TEMPLATE, "t", "a", "d", ANSWER_REVIEW_ACCESS_TOOLS, "r",
        );
        assert!(out.contains("FILES READ:"));
    }

    #[test]
    fn ensure_placeholder_leaves_templates_that_already_have_it_alone() {
        let template = "A {repo} B";
        assert_eq!(ensure_placeholder(template, "repo", "HEAD:"), template);
    }
}

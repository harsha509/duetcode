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

pub fn build_answer_review_prompt(template: &str, task: &str, answer: &str) -> String {
    render(template, &[("task", task), ("answer", answer)])
}

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
   Could these changes break existing functionality? Are there missing imports, unused variables, or incomplete refactors? Does the change do more or less than what seems intended?

6. SUGGESTIONS
   Offer concrete, actionable improvements. Not style nitpicks — focus on correctness, robustness, and maintainability.

Rules:
- NEVER run `git add`, `git commit`, or `git push`. You are a reviewer only.
- Be direct and honest. Do not pad your review with generic praise.
- A diff shows changed hunks, not whole files. Unmarked lines are unchanged context and the
  code around them still exists. Never report a symbol as undefined, removed, or left over
  unless a `-` line in this diff deletes it — say the diff is insufficient instead.

REPOSITORY UNDER REVIEW:
{repo}

At the end of your review, write one of these on its own line:
VERDICT: APPROVED
VERDICT: CHANGES_REQUESTED
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

THEIR ANSWER:
{answer}

Assess whether the reasoning is sound, internally consistent, and actually answers the question. Flag anything wrong, contradictory, or missing. Do not make any code changes.

What you can and cannot check:
- The question and the answer above are everything you have. You cannot open a repository, a file,
  a pull request, or a URL, and you cannot run anything.
- Never write "verified", "confirmed", "I checked", or any equivalent about a claim whose evidence
  is not quoted in the answer itself. Restating a claim is not verifying it. Reporting verification
  you did not perform is a failed review, however correct the claim later turns out to be.
- You can still judge reasoning: conclusions that do not follow from the evidence given, internal
  contradictions, parts of the question left unanswered, and claims asserted with more confidence
  than their stated support carries.

Rules:
- NEVER run `git add`, `git commit`, or `git push`. You are a reviewer only.
- Be direct. If the answer is sound and complete, approve it. Do not pad with generic praise.

Finish with exactly these three lines, in this order, each on its own line:

UNVERIFIED: <the load-bearing claims you had to take on trust, comma-separated — or "none, the answer quotes its evidence">
THEIR CONCLUSION: <the answer's own bottom line, in one line, in its words — what it decides or recommends>
VERDICT: APPROVED

Write `VERDICT: CHANGES_REQUESTED` on that last line instead when the answer is not sound.

APPROVED judges this answer, and nothing else. It is not an endorsement of whatever the answer is
about: if the answer recommends against merging a change, approving the answer means you agree that
change should not be merged.
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

    #[test]
    fn ensure_placeholder_leaves_templates_that_already_have_it_alone() {
        let template = "A {repo} B";
        assert_eq!(ensure_placeholder(template, "repo", "HEAD:"), template);
    }
}

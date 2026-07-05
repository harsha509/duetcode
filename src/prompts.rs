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

pub fn build_review_prompt(
    template: &str,
    task: &str,
    diff: &str,
    checks: &str,
    writer_notes: &str,
) -> String {
    render(
        template,
        &[
            ("task", task),
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

pub const DEFAULT_ANSWER_REVIEW_TEMPLATE: &str = r#"You are a senior engineer giving a second opinion. Another engineer investigated the repository and answered the question below. Verify their answer.

TASK / QUESTION: {task}

THEIR ANSWER:
{answer}

Assess whether the reasoning is sound, internally consistent, and actually answers the question. If you are able to inspect the repository, verify the specific claims (files, line numbers, APIs) really exist and support the conclusions. Flag anything wrong, unverifiable, or missing. Do not make any code changes.

Rules:
- NEVER run `git add`, `git commit`, or `git push`. You are a reviewer only.
- Be direct. If the answer is accurate and complete, approve it.

At the end, write one of these on its own line:
VERDICT: APPROVED
VERDICT: CHANGES_REQUESTED
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
        let out = build_review_prompt(DEFAULT_REVIEW_TEMPLATE, "t", "d", "c", "my notes");
        assert!(out.contains("my notes"));
        assert!(!out.contains("{writer_notes}"));
    }
}

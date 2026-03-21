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

pub fn build_implement_prompt(template: &str, task: &str, context: &str, previous_session: &str) -> String {
    let full_context = if previous_session.is_empty() {
        context.to_string()
    } else {
        format!("{}\n\nPREVIOUS SESSION CONTEXT:\n{}", context, previous_session)
    };
    render(template, &[("task", task), ("context", &full_context)])
}

pub fn build_review_prompt(template: &str, task: &str, diff: &str, checks: &str) -> String {
    render(
        template,
        &[("task", task), ("diff", diff), ("checks", checks)],
    )
}

pub fn build_fix_prompt(template: &str, task: &str, review_feedback: &str) -> String {
    render(
        template,
        &[("task", task), ("review_feedback", review_feedback)],
    )
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

After completing the task, briefly explain what you did and why.
"#;

pub const DEFAULT_REVIEW_TEMPLATE: &str = r#"You are a senior code reviewer. Review the following diff for the given task.

TASK: {task}

DIFF:
{diff}

CHECK RESULTS:
{checks}

Give your honest review of this code. Talk about what's good, what's wrong, and what could be better. Be direct.

At the end of your review, write one of these on its own line:
VERDICT: APPROVED
VERDICT: CHANGES_REQUESTED
"#;

pub const DEFAULT_FIX_TEMPLATE: &str = r#"You are an expert software engineer. A reviewer found issues with your implementation. Address the feedback below.

TASK: {task}

REVIEWER FEEDBACK:
{review_feedback}

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
"#;

pub const DEFAULT_PLAN_REVIEW_TEMPLATE: &str = r#"You are a senior software architect. A developer has proposed the following plan. Review it.

TASK: {task}

PROPOSED PLAN:
{plan}

Give your honest assessment. Is this the right approach? Anything missing? Any risks?

At the end, write one of these on its own line:
VERDICT: APPROVED
VERDICT: CHANGES_REQUESTED
"#;

pub fn build_plan_prompt(template: &str, task: &str, context: &str, previous_session: &str) -> String {
    let full_context = if previous_session.is_empty() {
        context.to_string()
    } else {
        format!("{}\n\nPREVIOUS SESSION CONTEXT:\n{}", context, previous_session)
    };
    render(template, &[("task", task), ("context", &full_context)])
}

pub fn build_plan_review_prompt(template: &str, task: &str, plan: &str) -> String {
    render(template, &[("task", task), ("plan", plan)])
}

pub fn build_implement_with_plan_prompt(template: &str, task: &str, context: &str, plan: &str) -> String {
    let full_context = format!("{}\n\nAPPROVED PLAN:\n{}", context, plan);
    render(template, &[("task", task), ("context", &full_context)])
}

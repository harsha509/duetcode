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
- Cite the file path with any line number you give, and only for lines present in the diff.
- Search narrow, not wide. When you glob or grep, point it at a directory the diff touches
  (`src/`, `tests/`) — never a workspace-wide pattern like `**/*`. Dependency and build trees
  (a venv, node_modules, target, vendor) are never part of a review, and a workspace-wide
  crawl can be large enough to kill the review process."#;

/// The counterpart of [`REVIEW_GROUND_RULES`] for a fetched change. The
/// reviewer's tools still work, but on a checkout that is not the change under
/// review — so rules written for the working tree, which tell the reviewer to
/// announce this repository and trust the files around it, are exactly what
/// would unground this review.
pub const FETCHED_REVIEW_GROUND_RULES: &str = r#"REQUIRED — the first line of your reply must be exactly:
Reviewing <the pull request(s) named in the block above>
Write that line before anything else.

Then, while reviewing:
- The diff above is the change under review, and you are NOT standing in it. The checkout
  around you is a different revision of the same project, or a different project altogether.
  Any file you open here may be older than the change, newer than it, or unrelated to it.
- Settle every claim about what the change does, adds, or removes against the diff itself.
  That is the only place those claims can be settled.
- Read-only tools, if you have them, are still worth using for what the diff cannot show: a
  definition the change calls but does not touch, a caller it never mentions, a convention it
  departs from. Whenever you use them, say so, and say the file came from the surrounding
  checkout rather than from the change.
- Never quote a line number from a file you opened as though it were a line of the change, and
  never report a finding as confirmed because the checkout agrees with it — the checkout can
  agree with a change that is wrong, and contradict one that is right.
- Search narrow, not wide. When you glob or grep, point it at a directory the diff touches
  (`src/`, `tests/`) — never a workspace-wide pattern like `**/*`. Dependency and build trees
  (a venv, node_modules, target, vendor) are never part of a review, and a workspace-wide
  crawl can be large enough to kill the review process."#;

/// Asked for only when the task names pull requests, where the question really
/// being put is "does this merge?" — and several screens of correct, careful
/// analysis do not answer it. The detail is what makes the review worth having;
/// the block is what makes it usable without reading all of it first.
///
/// Prefixed lines rather than a fenced block or an HTML comment: the panel can
/// lift them into a summary card, and they still read as plain text in the
/// terminal, where there is nothing to lift them into.
pub const PR_VERDICT_BLOCK: &str = r#"REQUIRED — because this task is about pull requests, your reply must OPEN with a
verdict block, before any heading, preamble, or analysis:

VERDICT: GO
or
VERDICT: NO-GO

then one line per finding, most serious first, using exactly these prefixes:

BLOCKER: <what breaks, in one plain sentence>
WARNING: <what is risky but not merge-blocking, in one plain sentence>

Rules for the block:
- NO-GO if there is at least one BLOCKER. GO if there are none.
- One sentence per line, plainly worded — a reader who has not opened the diff
  must understand what is wrong and why it matters. Name the file or PR if it
  helps them find it; do not paste code, stack traces, or line ranges.
- No BLOCKER or WARNING line for something you only suspect. If it is
  unverified, leave it out of the block and raise it in the analysis below.
- Nothing else in the block: no bullets, no bold, no sub-headings.

Then a blank line, and your full analysis exactly as you would have written it —
same depth, same evidence, same structure. The block summarises that analysis;
it does not replace it."#;

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
- Search narrow, not wide: point globs and greps at a directory the answer cites, never at the
  whole workspace with a `**/*` pattern. Dependency and build trees (a venv, node_modules,
  target, vendor) hold nothing an answer is about, and a workspace-wide crawl can be large
  enough to kill the review process.
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

THE STANDARD YOU ARE APPLYING

Approve once the change definitely improves the health of this codebase, even when it is not
perfect. There is no perfect code — only better code. Your job is not to turn this change into
the one you would have written: it is to stop what is wrong, and to say what would make the
rest better. Never withhold approval over something you would merely have done differently.

Raise everything you can support — but label it, so the reader can triage in one pass. A long
review is not the problem; an unsorted one is. What buries the two findings that mattered is
thirteen more presented as though they weighed the same.

Anything optional — taste, polish, a smaller improvement you would not insist on — goes in as a
suggestion whose line begins `Nit:`. There is no limit on how many of those you raise, and no
need to hold one back for being small. A blocker is the opposite: rare, and earned.

Follow this review process in this order. Design faults matter more than surface ones, and a
review that opens on naming has usually stopped looking:

1. UNDERSTAND THE CHANGES
   Read the diff carefully. Identify what files were changed, what was added, modified, or removed. Determine the purpose and intent behind these changes. If you cannot understand what the changes are trying to accomplish, flag that as a serious issue — unclear changes indicate poor code clarity.

2. ANALYZE THE CODEBASE CONTEXT
   Based on the file paths, naming conventions, and surrounding code visible in the diff, understand how these changes fit into the broader codebase. Consider whether the changes follow existing patterns and conventions.

3. VERIFY CORRECTNESS
   Check if the implementation is logically correct. Look for bugs, off-by-one errors, race conditions, null/undefined handling, and incorrect assumptions.

4. CHECK EDGE CASES
   Identify edge cases relevant to what the code does. Are they handled? What happens with empty inputs, large inputs, concurrent access, error conditions, or unexpected state?

5. CHECK THE TESTS
   Does the change carry the tests it needs, and do they actually test it? A test that would
   still pass with the change reverted is not covering it. Look for assertions on the new
   behaviour rather than on the fact that nothing threw, for the failure paths and not only the
   happy one, and for tests that encode a bug as expected behaviour. Missing tests are worth
   raising; the absence of a test is not by itself a defect in the code.

6. ASSESS IMPACT
   Could these changes break existing functionality? Does the change do more, or less, than the task asked for? Consider how far the new logic reaches: work threaded through a shared path carries risk beyond its own purpose, where a narrow entry point would not. Where you suspect a caller, import, or definition *outside* the diff is affected, say what you suspect and what would settle it — do not assert it. The diff rule below is not a reason to stay silent, only a reason to phrase it as a question.

7. CHECK THE DESIGN
   Judge the new and changed code against the principles below — not the untouched code around
   it, which is not what is being reviewed. Name the principle you are invoking and say what to
   do instead: a principle cited without a concrete alternative is not a review comment.
   - Single responsibility — one reason to change. A unit that parses, decides, and writes is
     three units wearing one name.
   - Open/closed — extending behaviour should not mean editing the same conditional again.
     Flag the change that adds the fifth branch to a switch that will need a sixth.
   - Liskov substitution — an implementation must honour what callers of the interface already
     assume. Narrowed inputs, new failure modes, and silently ignored calls all break it.
   - Interface segregation — no caller should have to depend on members it never uses.
   - Dependency inversion — policy should not reach directly for a concrete clock, filesystem,
     transport, or global. The tell is a new hard-wired dependency that leaves the unit with no
     way to be tested.
   - DRY — the same *knowledge* written twice will drift apart. Two passages that merely look
     alike are not duplication; two places that must be changed together are.
   - KISS and YAGNI — the simplest thing that satisfies the task, and nothing built for a
     requirement nobody stated: an interface with one implementation, a config knob nobody
     sets, a parameter every caller passes the same value for.
   - Honest names, small focused functions, no dead code, clear control flow, and types that
     describe the data instead of escaping the type system.
   - Comments that say why, not what. A comment restating the line above it is noise; the one
     worth asking for records the reason a reader could not recover from the code.
   - Consistency with what this codebase already does. Where the change departs from the
     surrounding convention, that is worth raising — and where your own preference merely
     differs from the convention, the convention wins.

8. CHECK THE MECHANICS
   Small, checkable things a careful reading of the diff can settle:
   - Imports and other module-level declarations belong at the top of the file, grouped and
     ordered the way that file already does it — not inside a function or partway down, unless
     the language requires it or a documented cycle has to be broken.
   - Arguments line up end to end at every call site the change touches: names, types,
     defaults, and units. Milliseconds against seconds, cents against dollars, id against uuid,
     local time against UTC.
   - No new name shadows an existing one in scope.
   - Errors are handled or propagated on purpose — not swallowed, and not caught so broadly
     that unrelated failures disappear with them.
   - Nothing left behind: debug output, commented-out code, an import the change orphaned.
   - No credential, token, or key in the diff, and nothing sensitive written to a log.

9. CHECK THAT IT CAN LAND
   Say so if the change cannot be applied in the order it assumes: a migration that only compiles once it has already run, a test that cannot pass until the feature ships and a feature gated on that test, a rename whose two halves each need the other to go first. Work shaped like this looks fine in a diff and cannot be carried out.

10. SUGGESTIONS
   Offer concrete, actionable improvements: say what to do, not only what is wrong. Style and
   polish are worth raising too — mark those `Nit:` so a reader can skip them — but they never
   outrank correctness, robustness, and maintainability, and they are never blockers.

Rules:
- NEVER run `git add`, `git commit`, or `git push`. You are a reviewer only.
- Be direct and honest. Do not pad your review with generic praise.
- A diff shows changed hunks, not whole files. Unmarked lines are unchanged context and the
  code around them still exists. Never report a symbol as undefined, removed, or left over
  unless a `-` line in this diff deletes it — say the diff is insufficient instead.
- The writer's notes are a claim, not evidence. Where a note asserts something the diff does not
  show, treat it as unverified rather than established. The same goes for the task text: if it
  states something about this code that the diff contradicts, say so rather than reasoning from
  the premise you were handed.
- Never name an API, function, flag, field, library, or configuration key that you have not seen
  in the diff, in a file you opened, or in the project's own dependencies. A suggestion built on
  an invented symbol costs more to disprove than it was ever worth. If you believe something
  exists but have not seen it, say which it is and that you did not confirm it.

EVIDENCE

Every finding you report carries its evidence, in the analysis, before it reaches the summary
bullets at the end:
- Quote the line you are talking about — the `+`/`-` line from the diff, or a line from a file
  you actually opened — and give its path. A finding with nothing quoted under it is an
  impression, not a review comment.
- For a blocker, state the path that reaches it: the input, state, or call order that produces
  the failure. "This could break" without a route to the break is a suggestion at most.
- Where you used your tools, say which file you opened. Where you did not, do not imply that
  you did — "verified", "confirmed" and "I checked" describe an act you either performed or did
  not, and claiming one you skipped is a failed review however right the finding turns out.

SAYING YOU DO NOT KNOW IS A VALID RESULT

You are not required to find something. A review that reports two real defects and says the
rest could not be settled from this diff is worth more than one that reports eight and is wrong
about three, and it is a better outcome than a fabricated finding in every case. When the diff
is too narrow, the file was not available, or the answer depends on code you could not see, say
exactly that and what would settle it. "I cannot determine this from the diff" is an acceptable
sentence to write, and an expected one.

BEFORE YOU WRITE THE FINAL SECTIONS — check your own work

Go back over every finding you are about to report and ask what supports it. If you cannot
point to a quoted line, an opened file, or a stated failure path, either demote it to a
question phrased as a question, or drop it. Do this pass before writing the bullets below —
a finding you retract here costs nothing, and one you leave in costs the reader their trust in
all the others.

REPOSITORY UNDER REVIEW:
{repo}

Classify every finding before you write it down. This decides what the reader is shown and
whether the loop spends another round, so it is not a question of tone:
- A BLOCKER is a defect you can make fail. Name the input, state, or sequence, and name what
  goes wrong: a wrong result, a crash, lost data, an opening for an attacker, a build or test
  that will not go green. If you cannot say how it fails, it is not a blocker.
- Everything else worth saying is a SUGGESTION — design, naming, duplication, simplification,
  missing tests, the mechanics above. Real improvements that do not make this change wrong.
- A principle cited without a failure it produces is a suggestion, never a blocker. So is
  anything the diff left you unable to verify, and so is anything you would merely have written
  differently.

Finish with these four sections, in this order:

FILES READ: comma-separated paths of every file you opened, or `none`
  This is checked against the tool calls you actually made. Listing a file you did not open is
  the one error here that is caught every time, so list only what you opened, and write `none`
  without embarrassment — a review of the diff alone is a legitimate review, and saying so
  costs you nothing. Naming a file you did not read costs the whole review its credibility.

BLOCKERS:
- one line per defect that has to be fixed before this can merge
- write a single bullet reading `none` when there are none

SUGGESTIONS:
- one line per improvement that is not blocking
- begin the line `Nit:` when it is optional — taste, polish, or anything you would not insist
  on. Raise as many as you found; the label is what stops them crowding out the rest
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

    /// Every tool-bearing reviewer is told to scope its searches: a
    /// workspace-wide glob makes the CLI crawl dependency trees it cannot
    /// afford to hold in memory. The rule rides in the blocks that reach
    /// custom prompt files too, not only the built-in templates.
    #[test]
    fn tool_bearing_reviewers_are_told_to_search_narrow() {
        assert!(REVIEW_GROUND_RULES.contains("Search narrow, not wide"));
        assert!(ANSWER_REVIEW_ACCESS_TOOLS.contains("Search narrow, not wide"));
        assert!(FETCHED_REVIEW_GROUND_RULES.contains("Search narrow, not wide"));
    }

    /// The fetched rules exist because the working-tree rules unground a
    /// fetched review: they must say the reviewer is not standing in the
    /// change, and must not demand the working-tree announcement of this
    /// repository's branch and changed files.
    #[test]
    fn fetched_ground_rules_point_away_from_the_checkout() {
        assert!(FETCHED_REVIEW_GROUND_RULES.contains("NOT standing in it"));
        assert!(!FETCHED_REVIEW_GROUND_RULES.contains("<branch>"));
    }
}

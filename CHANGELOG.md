# Changelog

All notable changes to the `dt` CLI are documented here. The VS Code extension
keeps its own changelog in [editors/vscode/CHANGELOG.md](editors/vscode/CHANGELOG.md).

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/).

Releases before 0.1.3 predate this file; see the git history for those.

## 0.2.0 - 2026-08-05

From this release the CLI and the VS Code extension share a version number.
They were drifting apart — CLI 0.1.5 against extension 0.1.9 — which left no
way to say which pairs with which.

### Fixed

- **The reviewer is finally asked for the findings the loop reads.** `BLOCKERS:`
  and `SUGGESTIONS:` have always been parsed out of a review, and no prompt has
  ever asked for them: across 22 real reviews, every one that was asked for a
  `VERDICT:` line produced one, and not a single one produced a `BLOCKERS:`
  section. So the list was always empty, and everything downstream of it was
  dead — the stall detector could only notice a repeated *diff*, never a
  repeated complaint, and the sidebar, panel, and `state.json` reported zero
  blockers on every run ever recorded. Both review prompts now specify the
  sections and say why the format matters.

- **The review prompt no longer forbids what it asks for.** Step 5 asked the
  reviewer to find "missing imports, unused variables, or incomplete refactors",
  while the diff rules forbid claiming a symbol is missing or left over unless a
  `-` line deletes it. The reviewer was told to look for something and then told
  not to say it, so the most common regression of all — a caller the change
  forgot to update — had no way to be reported. Suspicion about code outside the
  diff is now explicitly welcome, phrased as a question with what would settle
  it, rather than an assertion.

- **An empty findings section stays empty.** The parser skipped a bullet reading
  exactly `none`, so the ``- `none` `` and `- **None**` that reviewers actually
  write became a blocker no one could fix — failing the run and then feeding the
  stall detector a complaint that could never be resolved.

### Added

- **The reviewer checks the things a careful reviewer checks.** Correctness now
  covers parameters, types, defaults, and units lining up across every call site
  the diff touches — milliseconds against seconds, cents against dollars, ids
  against uuids, local time against UTC. Two steps join the process: one for
  design (single responsibility, honest names, no duplication, no dead code,
  types that describe the data), held to the changed code rather than the code
  around it; and one for whether the change can actually land, which catches work
  that is circular by construction — a migration that only compiles once it has
  run, a test gated on a feature gated on that test.

- **The writer's notes are marked as a claim.** They are written by the author of
  the code under review and were presented to the reviewer as context; a note
  asserting something the diff does not show is now to be treated as unverified.

## 0.1.5 - 2026-08-05

### Fixed

- **Upgrading `dt` now upgrades a project's prompts.** `dt init` copies the
  built-in templates into `.duet/prompts/`, and the loader prefers those copies,
  so every later improvement to a prompt stopped at the project boundary: a
  repository initialised months ago kept reviewing with that month's prompt for
  good, and nothing said so. Installing a new binary changed nothing, which made
  improving a built-in prompt pointless for every existing project.

  Each run now reconciles a project's copies against the templates the binary
  shipped with, and what a file is decides what happens to it. A copy dt wrote
  and nobody has touched is replaced with the current template. A copy that was
  edited by hand is left alone — the edit is the point — and reported once per
  version, not once per run. A copy predating this tracking, where authorship
  cannot be known, is renamed aside to `<name>.bak` and brought current. A
  manifest in `.duet/prompts/.manifest.json` records the fingerprint of what was
  written, so "edited" means edited rather than merely "unlike today's
  template". Projects with no `.duet/prompts/` are left untouched: adopting duet
  is still `dt init`'s decision to make.

## 0.1.4 - 2026-08-05

### Fixed

- **An approved answer no longer reads as an approved change.** When the writer
  answers instead of editing code, the reviewer judges that answer — but the run
  ended on `Answer approved!` and a green `SUCCESS`, the same words used when a
  diff is approved with checks passing. Asking `dt` to review a pull request and
  getting back "request changes, three blocking issues" therefore closed with an
  approval banner, and the tail of the log read as permission to merge. The
  closing lines now name what was approved — `gemini approved claude's answer — a
  verdict on the answer, not on the code it discusses` — and carry the answer's
  own conclusion beside it, so an approved "do not merge this" still says so.

### Changed

- **The answer reviewer no longer claims verification it cannot perform.** The
  review prompt invited it to "verify the specific claims … if you are able to
  inspect the repository", while the reviewer is a plain API call with no
  repository, file, or network access. Reviews came back asserting that files,
  commits, and library behaviour had been confirmed when nothing had been opened,
  which reads as corroboration and is worth less than nothing. The prompt now
  states the reviewer's actual reach, forbids the language of verification for
  anything not quoted in the answer itself, and requires the claims taken on
  trust to be listed. It also spells out what an approval covers: the answer, and
  not whatever the answer is about.

### Fixed

- **A writer that answers a question is reviewed on its answer.** Whether the
  round produced code was decided from the state of the whole worktree rather
  than from what the writer did, so an unrelated uncommitted change already
  present when the run started was handed to the reviewer as the writer's work.
  Asking `dt` a question in a repository with any pending edit got a review of
  that edit instead of the answer. The answer-review path — which verifies the
  claims in a written answer — existed but could only be reached with a
  completely clean tree.
- **An empty diff is never sent for review.** A writer that reverted the tree
  back to clean left nothing to review, and the reviewer, given an empty diff,
  could only approve it. That approval ended the loop and reported the run as a
  success, exit code 0 included.
- **Verdict parsing fails closed.** With no `VERDICT:` line, the last few lines
  were scanned for the word "approved" behind a single literal `NOT APPROVED`
  guard, so ordinary prose — "this cannot be approved until the null check is
  added" — was read as an approval and reported to the user as success. Only a
  line that is nothing but the word now counts; everything else is a rejection.
- **`VERDICT: NOT APPROVED` is a rejection.** The approval branch was tested
  before the rejection branch, and "NOT APPROVED" contains "APPROVED", so an
  explicit rejection in that wording was recorded as an approval.
- **Blockers survive indentation and numbered lists.** Bullets were matched on
  the raw line, so `  - indented` and `1. numbered` were dropped. A reviewer
  that consistently indents its bullets produced an empty blocker list, which in
  turn disabled blocker-based stall detection entirely and left the escalation
  prompt showing no open blockers.
- **Non-ASCII tool arguments no longer abort the process.** Tool-use progress
  lines were truncated by slicing at a byte index while measuring in bytes, so a
  cut landing inside a multibyte character panicked — a non-ASCII file path, an
  accented Bash command, a CJK `Grep` pattern. Release builds set
  `panic = "abort"`, so this killed `dt` mid-run and lost the session.
  Truncation is now measured and cut in characters.
- **`mode = "Auto"` falls back to the API.** The constructor lowercased `mode`
  before deciding whether to build the HTTP agent, but the CLI-failure fallback
  compared the raw value, so any capitalisation other than `auto` built the
  agent and then never used it — a CLI failure became a hard error.
- **`dt init` no longer dirties `.gitignore`.** The guard tested for the literal
  `.duet/sessions/`, which a broader existing `.duet/` rule does not contain, so
  every run appended a duplicate entry that ignored nothing new. Any rule
  already covering the sessions directory is now recognised.

# Changelog

All notable changes to the `dt` CLI are documented here. The VS Code extension
keeps its own changelog in [editors/vscode/CHANGELOG.md](editors/vscode/CHANGELOG.md).

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/).

Releases before 0.1.3 predate this file; see the git history for those.

## 0.2.5 - 2026-08-06

No changes to the CLI. The version is shared with the VS Code extension and both
are released from one tag, so `dt` moves with it — the panel is where this
release happened, and
[editors/vscode/CHANGELOG.md](editors/vscode/CHANGELOG.md) says what changed
there. `dt serve` and its protocol are untouched.

## 0.2.4 - 2026-08-06

The reviewer could read code, and read the wrong code. Asked to review an
answer about two pull requests, it opened eleven files in the checkout it
happened to be standing in — all on `main`, none of them the change — and
reported back agreeing. Every path resolved and every line number read cleanly,
which is what made it look like corroboration.

### Added

- **The change under review is fetched, not assumed.** When a task names GitHub
  pull requests, dt now fetches their diffs with `gh pr diff` and reviews the
  answer against those, saying so on screen: `reviewing the answer against
  acme/api#529 — not against the working tree`. Up to four per task, sharing a
  120 KB budget so one large change cannot crowd out the rest. Anything that
  did not come back is named in the prompt as missing, rather than left for the
  reviewer to fill in from the checkout.
- **A reviewer whose tools point elsewhere is told so.** Holding a fetched
  diff, it is now instructed that the checkout around it is a *different
  revision* — to settle every claim about the change in the diff, to use its
  tools only for what the diff cannot show, and never to quote a line number
  from a file it opened as though it were a line of the change.
- **A review that cannot reach the code is refused.** If the answer is about a
  change outside the checkout that could not be fetched — no `gh`, not
  authenticated, an enterprise host, a link with no fetchable URL — the run
  ends `NO REVIEW` and names the way out (`gh pr checkout <number>`), instead
  of returning a confident review of an unrelated revision. This holds however
  capable the reviewer is: file access is what makes this failure convincing,
  so it is not an excuse for it.

### Changed

- **An answer is judged `SOUND` or `UNSOUND`, never `APPROVED`.** The two
  verdicts shared one word, and an approved answer arguing *against* a merge
  read as an approval of the merge. They no longer collide — in the terminal,
  in the panel, and in the closing summary, which said "approved" for a single
  answer review and now says "the answer was found sound". Both vocabularies
  still parse, so a custom prompt template written before the split keeps
  working.

## 0.2.3 - 2026-08-05

The reviewer could not read code. Reviewing an answer, it was handed the prose
and told in as many words that it could not open a file — so it approved
answers in a second flat, having checked nothing. It can read now.

### Added

- **Gemini runs as a CLI, with tools.** `[gemini] mode` takes `"cli"`, `"api"`,
  or `"auto"` (the default: CLI when it is on `PATH`, API otherwise), matching
  how `[claude]` already works. In CLI mode the reviewer runs inside the
  checkout under `--approval-mode plan` — read-only, so it can open and search
  the code but never edit what it is judging — and sibling projects come along
  via `--include-directories`, so a review can follow a claim across the repos
  in a workspace. Every file it opens is reported as it happens, the same way
  the writer's reads already are.
- **A review with nothing to check is refused rather than answered.** When the
  reviewer has no file access *and* there is no diff behind the answer, the run
  now ends `NO REVIEW` and the reviewer is never called. Previously that was
  precisely the case that returned `APPROVED`.

### Changed

- **The reviewer is told what it can actually do.** The answer-review prompt
  carried one fixed line — "You cannot open a repository, a file … and you
  cannot run anything" — which was true of the API transport and false of the
  CLI. It now matches the reviewer in use, and a reviewer with tools is told to
  go and look: at the files the answer cites, and at what the answer left out.
- **Reviews declare what they opened.** A new `FILES READ:` line lists the files
  the reviewer actually read, so a review that inspected nothing says so instead
  of passing quietly.
- **`dt doctor` treats the CLI and the API key as alternatives.** A missing
  `GEMINI_API_KEY` was a hard failure even with the CLI installed. Only having
  neither is a failure now; having just the key warns that the reviewer will run
  without file access.

### Fixed

- **A long review can no longer hang.** The CLI's stderr was left unread until
  after its stdout had been consumed to the end. Once a talkative run filled
  that pipe the CLI would block writing to it, stop producing stdout, and the
  two processes would wait on each other forever. It is drained as it arrives.
- **A killed CLI says what happened.** A process killed by a signal has no exit
  code, and the failure was reported as `exited with -1: no output`, which
  names nothing. A SIGKILL — in practice the machine running out of memory
  under a heavy `node` process — now says so.
- **The offer of a fix matches the reviewer.** A refused review always advised
  installing the Gemini CLI, including when Claude held the reviewer seat under
  `--writer gemini`, sending the user to install something they were not using.

## 0.2.2 - 2026-08-05

A reviewer call is the expensive half of a run, and the loop used to interrupt
every task to ask for one. Frontends with a review button of their own now skip
the question entirely, and the review they eventually run is better armed.

### Added

- **A run can end unreviewed without being called a failure.** Declining a
  review reported `SUCCESS` in green — the same word an approved run earns. The
  outcome is now three-valued: approved, unreviewed, or stopped. `NO REVIEW` is
  printed uncolored, and still exits 0, because declining a review is not an
  error.

- **`review_on_demand` for frontends that offer their own review action.** The
  loop then never asks whether to review: it hands the work back unreviewed and
  spends a reviewer call only when the user asks for one. `dt serve` sets it;
  the terminal does not, since a prompt is the only route to a review there.

- **Answers can be reviewed on their own.** `answer_review_only` judges an
  answer a writer already gave, against the question it answered — the review a
  frontend runs after a task that produced no diff, which the diff review has
  nothing to work with.

### Changed

- **An answer review now sees the code the answer is about.** The reviewer was
  given the question and the prose and nothing else, so every claim about the
  repository had to be taken on trust. The working tree goes with it now, capped
  at 40 KB and marked where it was cut, and the prompt asks the reviewer to
  contradict the answer where the diff does not support it.

### Fixed

- **A reverted tree no longer shows the reviewer changes that do not exist.**
  The answer review was handed the diff from before the round. A writer that
  reverts its own work leaves an empty tree, and the reviewer was still shown
  the old one.

## 0.2.1 - 2026-08-05

A multi-root VS Code workspace used to be invisible to `dt`: the session was
anchored to the first folder and nothing could move it. This release makes the
session span the workspace, and fixes the two bugs that anchoring caused.

### Fixed

- **A task now runs in the project it says it is running in.** The composer's
  project picker steered the diff, the checks, the session log, and the prompt's
  "path:" line — but not the writer. The model was left standing in whichever
  folder `dt serve` was spawned in, so picking a second project meant editing
  the first one while `dt` diffed the second, found nothing, and reported no
  changes to review. Both models now follow the task to its project.

- **A failed call no longer costs the writer its session.** The CLI announces
  its session id before doing any work, but a non-zero exit discarded the whole
  response — id included — so a run that died partway through left nothing to
  resume and the next task started cold. Worse, any failure while resuming was
  read as "this session is broken": the id was dropped and the call immediately
  retried, which for an account quota limit meant spending a second call to fail
  the same way and losing the conversation permanently. The id is now recorded
  whether or not the run succeeded, and only a resume the CLI never accepted —
  visible as a run that never announced a session — earns a fresh start.

- **Session ids are kept per project.** The CLI stores its sessions by working
  directory, so one id shared across projects was looked up in the wrong place
  on every switch. Each project keeps its own, and returning to an earlier
  project resumes where it left off.

### Added

- **`dt serve` spans a workspace.** A new `workspace` command declares the
  projects a session covers; frontends resend it when the set changes. It is a
  command rather than a startup flag so adding a folder never forces a respawn,
  which would throw away both models' accumulated context. A `review` that names
  no directories now covers the whole workspace.

- **Tasks can read their sibling projects.** The repository context names every
  other project in the workspace with its path, branch, and changed files, and
  the Claude CLI is granted them as additional readable roots — so research
  across a front end and its back end sees both, instead of reasoning from the
  one checkout it happens to write to. Siblings are explicitly marked read-only:
  only the chosen project is diffed, checked, and reviewed.

- **Edits outside the chosen project are reported.** Sibling worktrees are
  recorded when the task begins and re-checked each round, so a change that
  escapes the loop — reaching no reviewer and tripping no check — is announced
  instead of passing silently. Recording the baseline first means edits that
  were already sitting there are never blamed on the run.

### Changed

- **A task never silently picks the first folder.** An explicitly named project
  still wins; otherwise the session uses the workspace's only git checkout, and
  asks when there is more than one. Defaulting to "the first folder" is what
  produced a task whose diff, checks, and log described a different project than
  the one it edited.

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

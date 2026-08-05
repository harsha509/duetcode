# Changelog

All notable changes to the DT Duet extension are documented here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/).

## 0.2.2 - 2026-08-05

Requires `dt` 0.2.2. The panel no longer interrupts a task to ask whether to
review — the review button decides that — and a model's code is finally set
apart from its prose.

### Added

- **Code blocks are rendered as code.** Prose and code arrived as one
  monospace wall in which neither could be read. A fenced block is now its own
  card: labelled with its language, syntax-tinted, and scrolling sideways
  instead of wrapping into something that reads like a sentence. Diffs carry
  their own colouring — added, removed, hunk headers — and a symbol named
  mid-sentence in backticks is marked so it is not read as a word.

- **The review button reviews the last answer, not just the diff.** A task that
  answers a question leaves nothing uncommitted, so the button had nothing to
  judge and said so. The answer is now held per project until a review judges
  it, and the button picks the review that fits what the task produced.

### Changed

- **No more "review this with gemini?" mid-task.** The panel has a review
  button, so the task runs to the end and closes as `NO REVIEW` — grey, neither
  a pass nor a failure. Press review when you want the second opinion. Auto mode
  is unchanged: writer, reviewer, writer, every round, no clicking.

### Fixed

- **A `#` comment inside a shell or Python block is no longer read as a
  heading.** The severity tinter treats `#` lines as markdown headings, so a
  comment in a code block was tinted — and opened a section whose colour then
  leaked onto everything after it. Code is now excluded from that pass.

## 0.2.1 - 2026-08-05

Requires `dt` 0.2.1. The extension now tells the server which projects the
workspace contains, and older servers reject that as an unknown command — so
upgrading the extension without upgrading the binary shows an error on every
task.

### Fixed

- **The project picker now decides where a task actually runs.** It set the
  project a task was reported against, while the writer kept working in the
  folder the server was started in — so choosing a second project edited the
  first. Fixed in `dt` 0.2.1, which is why this release requires it.

- **The picker keeps your choice when a folder is added or removed.** The list
  was rebuilt from scratch on every workspace change, which quietly reset it to
  the first project — and the picker decides where the next task writes.

- **The picker is never left empty.** The project list was posted when the panel
  was created, which can be before the webview is listening; a dropped list left
  the composer with no project selected. The webview now asks for it once it is
  ready.

### Changed

- **The whole workspace is declared to the server**, on startup and whenever
  folders are added or removed. The update goes to the running server instead of
  restarting it: a respawn would cost both models the context they have built up,
  which is far too much to pay for a change of scope.

- **Only settings that are baked into the server restart it.** Any `dt.*` change
  used to force a respawn. Today every setting is a spawn-time one, so nothing
  changes yet — but a future setting that is not can no longer cost a session
  its context by accident.

## 0.2.0 - 2026-08-05

Requires `dt` 0.2.0. From this release the extension and the CLI share a version
number: they had drifted to 0.1.9 against 0.1.5, with nothing to say which
pairs with which, so they are now released together.

### Changed

- **Blockers and suggestions appear in the panel.** They were always rendered
  and always empty, because no prompt asked the reviewer to produce them —
  fixed in `dt` 0.2.0, which is why this release requires it. A review now ends
  with its findings listed as red blockers and yellow suggestions beneath the
  verdict, instead of leaving them buried in the prose.

## 0.1.9 - 2026-08-05

Requires `dt` 0.1.5 or newer for the prompt refresh described below.

### Added

- **Findings are colour-coded.** A model's review arrived as one undifferentiated
  block, so the blocking items and the nitpicks read alike and a reader had to
  find the severity by reading everything. Lines that announce something — a
  heading, or a list item with a bold lead-in — are now tinted red, yellow, or
  green, and a classified heading lends its severity to the findings nested under
  it. Body prose keeps the default colour, so the tint still means something.
  Inheritance escalates but never clears: a green heading does not paint the
  items beneath it green, because "nothing to do here" is the one claim this
  must never invent.

## 0.1.8 - 2026-08-05

Requires `dt` 0.1.2 or newer.

### Fixed

- **Every workspace project's sessions are listed.** The sidebar read
  `.duet/sessions` from the first workspace folder only, while a task runs in the
  project the composer picked and writes its session under *that* repository. In
  a multi-root workspace the other projects' sessions were therefore never shown
  — and when the first folder had none, the panel looked empty and the history
  looked lost, though every session was on disk the whole time. Sessions from all
  folders are now listed, grouped under a node per project so it is clear which
  repository each run belongs to. A single-folder workspace keeps the flat list
  it had.

- **A project added to the workspace is watched.** The file watcher that
  refreshes the tree covered the first folder only and was created once at
  startup, so sessions recorded in any other project appeared only after a window
  reload. Every folder is watched now, and the set is rebuilt when folders are
  added or removed.

## 0.1.7 - 2026-08-01

Requires `dt` 0.1.2 or newer.

### Added

- **Projects are set up on first use.** A project without `.duet/config.toml`
  made `dt serve` exit at startup, which the panel reported only as
  `dt serve exited (code 1) — next task restarts it`. That restart could never
  succeed: the missing config does not change between spawns, so every attempt
  failed the same way. Before the first task, every git project in the
  workspace that lacks a config is now offered `dt init` in a single prompt,
  listing the projects and what the command writes. Declining is remembered per
  project, so the prompt does not return for a project you keep uninitialized.

## 0.1.6 - 2026-07-31

Requires `dt` 0.1.2 or newer.

### Added

- **Project picker in the composer.** In a multi-root workspace, tasks now run
  in the project you select rather than always the first workspace folder. The
  picker follows the review: after a review of project B, a fix typed straight
  afterwards runs in project B.
- **Every run announces its project.** Reviews and tasks emit the repository
  name and absolute path into the timeline before any model output, so the
  project a finding belongs to is never inferred.

### Fixed

- **The reviewer now states what it is reviewing.** Reviews open with
  `Reviewing <repository> (<branch>) — <n> file(s): <paths>`, taken from the
  checkout itself. Previously the reviewer was handed a bare diff with no
  indication of which repository it came from.
- **Findings are constrained to what the diff shows.** The reviewer is told
  that unmarked diff lines are unchanged context and that code outside the diff
  still exists, and is barred from reporting a symbol as undefined or left over
  unless a `-` line actually deletes it. This removes a class of confident
  false positives — a reviewer reading an unchanged context line and reporting
  a `NameError` for a variable that was still assigned 236 lines above it.
- **Writer and reviewer are given the same repository block**, so a fix cannot
  be attempted against a different checkout than the one reviewed.

## 0.1.5 - 2026-07-31

Requires `dt` 0.1.1 or newer — the `review` command gained a `dirs` field.

### Fixed

- **Review covers every project in a multi-root workspace.** The panel sent
  only the first workspace folder, so clicking *review* in a workspace holding
  two projects reported "no uncommitted changes" whenever that first folder
  happened to be clean — even with uncommitted work sitting in the second.
  Every folder is now reviewed in turn, each under its own `.duet/config.toml`
  when it has one. Projects that are clean or are not git repositories are
  skipped with a note instead of ending the run, and "no uncommitted changes"
  is reported only when every project is clean.
- The folder list is read when *review* is clicked rather than at activation,
  so folders added to the workspace mid-session are included without a reload.

## 0.1.4 - 2026-07-31

First public release on the VS Code Marketplace.

### Added

- **Duet panel** — each round shows the writer's stream (tool actions, code,
  explanation) above the reviewer's block (check results, review, verdict
  with blockers), full-width.
- **Sessions sidebar** — lists every run from `.duet/sessions` with outcome
  icons; selecting one replays it round by round, patches included.
- **Task composer** — write a task and toggle `auto` (models loop unattended
  until mutual approval) or `plan` (plan → review → approve → execute).
- **Image attachments** — attach screenshots from the picker or paste them
  directly with Cmd+V; pasted images are cleaned up when the task ends.
- **Inline approvals** — prompts that need your decision (review now? fix
  issues? guidance on a deadlock) render as buttons instead of terminal
  input.
- **Settings page** — API keys, Claude CLI login status, `dt` binary path and
  writer selection behind the ⚙ gear.
- **Model version overrides** — `dt.claudeModel` and `dt.geminiModel` pin a
  model per workspace, with a refresh button that fetches the current model
  list from the provider.
- **Keychain-backed secrets** — API keys are stored in VS Code SecretStorage
  (the OS keychain) and injected only into the spawned `dt` process, never
  written to `settings.json`.
- **Machine-scoped `dt.binaryPath`** — a cloned repository's workspace
  settings cannot redirect the extension at an untrusted binary.

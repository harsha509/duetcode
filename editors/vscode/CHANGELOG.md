# Changelog

All notable changes to the DT Duet extension are documented here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/).

## 0.3.0 - 2026-08-28

Requires `dt` 0.3.0. No extension code changes: the version moves in lockstep
with the CLI, whose reviewers on the API transports now run read-only git
tools — those tool calls appear in the panel's activity line like any others.

## 0.2.13 - 2026-08-27

Requires `dt` 0.2.13. The timeline now reads in the order things happened,
in the run's own colors.

### Added

- **Seat colors.** The writer's card is green and the reviewer's yellow — a
  full tinted box each, instead of a colored left edge.
- **A live activity bubble.** One pulsing line per seat shows the current
  step — thinking, the file being read — updating in place instead of
  stacking a line per tool call, and disappearing when real output streams.
- **Inline diffs.** The diffstat row grows a *view diff* toggle that expands
  the round's actual patch, syntax-highlighted and collapsed by default.
- **Markdown rendering.** Headings, bold, bullet markers and horizontal
  rules in model prose now render instead of showing their raw symbols;
  inline code keeps working, including inside bold.

### Changed

- **Rows keep their order.** Diffstats, warnings and info lines land inside
  the round at the moment they happened, instead of stacking below the
  columns that kept growing above them.
- **Less chrome.** The mode chip, the workspace echo and the logs path are
  gone from the timeline, and the project banner shows only in multi-root
  workspaces; round headers still carry the budget.

### Fixed

- **A stored session with recorded checks but no reviewer text no longer
  crashes the timeline renderer.**

## 0.2.12 - 2026-08-16

Requires `dt` 0.2.12. The sessions sidebar can now delete what it lists.

### Added

- **Delete a session from its row.** A trash icon on hover, and a *Delete
  Session* entry in the right-click menu, remove that session's logs after a
  confirmation. The tree updates immediately.

- **Clear All Sessions.** In the view's `…` menu it clears every folder's
  sessions; on a project row in a multi-root workspace, only that project's.
  Both confirm first and name how many are going.

  Deletions run asynchronously and one session at a time, so clearing a long
  history does not stall the editor, and a directory that cannot be removed
  is reported by name instead of stranding the rest of the batch.

## 0.2.11 - 2026-08-16

Requires `dt` 0.2.11. The webview's CSP nonce is now generated with
`crypto.randomBytes` instead of `Math.random()`, and pasted-image temp files
get `crypto.randomUUID()` names created with the exclusive `wx` flag. The
version otherwise tracks the CLI's hardening release: hung subprocesses are
killed instead of stalling the panel, and checks from other workspace
projects ask for consent before running. (0.2.10 shipped CLI-only.)

## 0.2.9 - 2026-08-10

Requires `dt` 0.2.9. No extension-side changes: the version tracks the CLI,
which now engages gemini's crawl guard before the first spawn on oversized
checkouts instead of after an out-of-memory death, tells a guarded reviewer
which tools still work so it keeps reading files, and warns when a verdict
over a truncated diff arrives with no files opened. Those warnings surface
in the panel's output like any other.

## 0.2.8 - 2026-08-10

Requires `dt` 0.2.8. No extension-side changes: the version tracks the CLI,
which this release teaches to survive gemini's workspace crawl on checkouts
carrying large dependency trees — reviews now warn about the offending
directories up front (and in `dt doctor`) instead of dying of memory
exhaustion, and an out-of-memory death retries with the reviewer still able
to read files. Those warnings surface in the panel's output like any other.

## 0.2.7 - 2026-08-08

Requires `dt` 0.2.7. A passing review no longer reads like a failing one, and a
past session is no longer relabelled by a setting you changed afterwards.

### Fixed

- **A review that approved no longer prints lines in blocker-red.** Prose was
  tinted line by line by keyword, with no knowledge of the verdict printed
  directly beneath it — so a review concluding `APPROVED` with no blockers could
  colour four lines as blockers on the same screen. On one real review the
  triggers were the word "critical" inside "this is a critical finding", the
  word "security", the word "Blocking" in the *name* of a UI bug, and "fails" in
  "if removeSecretIfExists fails". When the verdict is an approval carrying no
  blockers, blocker-red is now dropped to warning across the round. A rejected
  round keeps its tint, including one whose verdict could not be parsed, where
  the tinted prose is the only trace of what went wrong.

- **A finding is never painted green.** "Verified" and "Sound" inside a finding
  describe the checking the reviewer did, not an all-clear, and green stated the
  one thing the line did not say. Three nits on that same review were coloured
  as though there were nothing to do.

- **A past session shows the models that actually ran it.** The column headings
  and the models line came from the current configuration, so switching the
  writer relabelled every finished session in the history — a session where
  Claude wrote and Gemini reviewed displayed as the opposite. They now come from
  the session's own record: `roles.json` for sessions written by `dt` 0.2.7, and
  the recorded verdict metadata for older ones. A session with neither shows a
  bare `writer` / `reviewer` with no model name, because nothing on disk says
  which models ran it and a guess would read exactly like a fact.

## 0.2.6 - 2026-08-06

Requires `dt` 0.2.6. A pull request review now says go or no-go before it says
anything else, and a running task can be stopped rather than trampled.

### Added

- **A go/no-go card above a pull request review.** When a task names a GitHub
  pull request, `dt` 0.2.6 asks the writer to open with a verdict, and the panel
  lifts it into a card at the top of the answer: `✓ GO` or `✕ NO-GO`, the
  blocker and warning counts, then each finding as one plain sentence. The full
  analysis follows underneath, unchanged. Only a block at the very start of an
  answer is lifted — the words appear in ordinary review prose too, and a card
  assembled from sentences scattered through the analysis would be a different,
  worse review than the one the model wrote.
- **A stop button for a running task.** It appears in the Sessions toolbar only
  while something is running, and ends the task by taking `dt serve` down with
  it — the protocol has no cancel, so a task owns the server until it finishes.
  The timeline closes with `STOPPED` rather than falling silent.

### Changed

- **+ (New Task) will not discard a running task.** It now refuses while one is
  in flight and says so, offering **Stop It** for when that is what you meant.
  Previously it restarted the models underneath a live task without asking,
  ending it mid-round.

## 0.2.5 - 2026-08-06

Requires `dt` 0.2.5. The CLI is unchanged — it shares a version number with the
extension — and this release is all panel: **+** starts a session that is
actually new, and the writer's tool calls appear where they happened instead of
in a heap underneath the answer.

### Changed

- **+ (New Task) starts a genuinely new session.** It used to reveal the panel
  you already had: same timeline, same conversation, so a "new" task began with
  everything the previous one had told the models still in scope. It now closes
  the open panel and restarts `dt serve`, which drops both models' accumulated
  context along with their CLI resume ids. The Sessions sidebar is deliberately
  untouched — it is the on-disk archive of past runs, and opening one only
  renders stored text, so browsing history can never turn into context for a
  live task.

### Fixed

- **Tool actions appear in the order they happened.** A turn that interleaved
  text and tool calls — answer, grep, more answer — put every `⚡` line below
  the entire reply, because streamed text kept flowing into a block pinned
  above them. Both models were affected; the reviewer only looked correct when
  it happened to run all its tools before writing anything. `◌ thinking…` had
  the same problem.
- **The composer's ⚙ and 📎 are the same size.** They were two different kinds
  of character — one defaulting to emoji presentation, one to text — so they
  arrived from different fonts at different sizes, weights, and baselines, which
  no amount of CSS could reconcile. Both are drawn now, in matching square
  toolbar buttons. The row's buttons and checkboxes also pick up VS Code's font,
  which form controls do not inherit on their own.
- **A deliberate restart no longer reports itself as a crash.** Changing the
  writer or a model restarts `dt serve`, which announced itself as
  `dt serve exited (code null)` in red. The dying process is detached from its
  events before it is killed, so neither the exit nor whatever its output was
  still flushing can land in the next session's timeline.
- **Screenshots pasted into an abandoned task are cleaned up.** Closing the
  panel mid-task leaked one temp file per pasted image — they were swept only
  when a task ran to completion.

## 0.2.4 - 2026-08-06

Requires `dt` 0.2.4. The review button stops agreeing with answers it could not
check: asked about a pull request, the reviewer was reading whatever branch your
window happened to be on.

### Changed

- **An answer's verdict chip reads `SOUND` / `UNSOUND`.** It used to read
  `APPROVED`, the same chip a code review shows — so an approved answer that
  argued *against* merging a change looked like an approval of that change. The
  code review chip is unchanged.
- **Reviewing an answer about a pull request fetches the pull request.** Name
  the pull requests in the task and the reviewer is given their diffs, not the
  files sitting in your editor. If they cannot be fetched, the review is refused
  and tells you what to do about it, rather than reporting a confident review of
  a different revision. Needs [`gh`](https://cli.github.com) installed and
  authenticated.

## 0.2.3 - 2026-08-05

Requires `dt` 0.2.3. The extension itself is unchanged — it shares a version
number with the CLI — but the review button behaves very differently, because
the reviewer behind it can now read your code.

### Changed

- **The review button's verdict is now based on the code.** Reviewing an answer
  used to send the reviewer nothing but the text and tell it that it could not
  open a file, so a question that changed no code came back approved in about a
  second having checked nothing. With the Gemini CLI installed
  (`npm i -g @google/gemini-cli`), the reviewer opens the files the answer cites
  and reports what it read — the panel shows each file as it is opened, the way
  it already shows the writer's.
- **A review that could check nothing is refused rather than approved.** When
  the reviewer has no file access and there is nothing uncommitted, the run ends
  `NO REVIEW` instead of returning an approval nothing supports.

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

# Changelog

All notable changes to the DT Duet extension are documented here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/).

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

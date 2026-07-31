# Changelog

All notable changes to the DT Duet extension are documented here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/).

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

# DT Duet — AI Pair Programming

One model writes, another reviews — in a loop, until both approve.

DT Duet runs [duetcode](https://github.com/harsha509/duetcode) inside
VS Code: Claude implements your task, Gemini reviews the diff against your
tests and linters, and you watch both sides of the conversation live.

## Features

- **Duet panel** — per round, the writer's stream (tool actions, code,
  explanation) stacks above the reviewer's block (check results, review,
  verdict with blockers), each full-width.
- **Sessions sidebar** — every past run from `.duet/sessions`, with outcome
  icons; click to replay any session round by round, patches included.
- **Task composer** — type a task, toggle `auto` (models loop unattended
  until mutual approval) or `plan` (plan → review → approve → execute).
- **Screenshots** — attach with the picker or **paste directly with Cmd+V**;
  pasted images are cleaned up after the task.
- **Approvals as buttons** — when a model needs your call (review now?
  fix issues? guidance when the models deadlock), it's a click, not a
  terminal prompt.
- **Secure keys** — API keys live in your OS keychain via VS Code
  SecretStorage and are injected only into the spawned `dt` process. Claude
  CLI users need no Anthropic key at all.

## Requirements

- The `dt` binary — `cargo install --git https://github.com/harsha509/duetcode`
  (set `dt.binaryPath` if it's not on VS Code's PATH, e.g. `~/.cargo/bin/dt`).
- A workspace initialized with `dt init`.
- Gemini API key — add it via the ⚙ gear (stored in the keychain) or export
  `GEMINI_API_KEY`.
- Claude CLI authenticated (`claude` → `/login`), or an Anthropic API key.

## Quick start

1. Open a `dt init`-ed project.
2. Click the DT Duet icon in the activity bar.
3. Hit **+** (New Task), type what you want, press Enter.
4. Watch Claude write and Gemini review; answer prompts with the buttons.

Settings live behind the **⚙ gear** (sidebar title or panel): API keys,
Claude CLI login, binary path, writer model.

## Security

- Keys: OS keychain only — never settings.json, never synced, never logged.
- `dt.binaryPath` is machine-scoped, so a cloned repo's workspace settings
  cannot redirect the extension to an untrusted binary.
- The extension talks to `dt serve` over stdio only; no network ports.
- Model providers (Anthropic, Google) receive your diffs and prompts, as
  with any AI coding tool.

## Development

```bash
cd editors/vscode
npm install
npm run compile   # or press F5 for an Extension Development Host
```

The extension is a thin client over `dt serve`'s JSON-lines protocol —
see `src/serve.rs` in the repo root. Releasing: see [RELEASING.md](RELEASING.md).

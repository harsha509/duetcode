# DT Duet — VS Code extension

Run [duetcode](https://github.com/harsha509/duetcode) from VS Code: one model
writes, the other reviews, and you watch both sides of the duet live.

- **Sessions sidebar** — every past run from `.duet/sessions`, click to open.
- **Duet panel** — round-aligned columns: writer on the left, reviewer on the
  right, with checks, verdicts, blockers, and cost per task.
- **Task composer** — type a task, toggle `auto`/`plan`, attach images with
  the picker or **paste screenshots directly** (Cmd+V).
- Approval prompts appear as buttons; clarifications as an input field.

## Requirements

- The `dt` binary on your PATH (or set `dt.binaryPath`), built with
  `cargo install --path .` from the repo root.
- A workspace initialized with `dt init` (and `GEMINI_API_KEY` exported for
  the Gemini side).

## Development

```bash
cd editors/vscode
npm install
npm run compile
```

Then open this folder in VS Code and press **F5** to launch an Extension
Development Host. The extension talks to `dt serve` over a JSON-lines
protocol; see `src/serve.rs` in the repo root for the protocol reference.

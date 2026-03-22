# duetcode (dt)

AI pair programming CLI — one model writes code, another reviews it, with you in control.

`dt` orchestrates Claude and Gemini in a structured write/review cycle. One model implements a task, then you decide if you want the other model to review the diff. The loop continues until the reviewer approves and all quality checks pass — or until you decide it's done.

## How it works

```
You give a task
    → Claude (writer) implements it
    → git diff captured
    → You are asked: "Review changes with gemini? (y/n)"
    → Your configured checks run (test / lint / typecheck)
    → Gemini (reviewer) reviews the diff + check results
    → APPROVED? → done
    → CHANGES_REQUESTED? → You are asked: "Let claude fix? (y/n)" → repeat
```

You can flip the roles (`--writer gemini`) so Gemini writes and Claude reviews.

## Installation

### Prerequisites

- [Rust](https://rustup.rs/) (1.80+)
- [Claude Code CLI](https://claude.ai/download) installed and authenticated
- A [Gemini API key](https://aistudio.google.com/apikey) exported as `GEMINI_API_KEY`
- Git

### Using Cargo (Recommended)

You can install `dt` directly from the GitHub repository using Cargo:

```bash
cargo install --git https://github.com/harsha509/duetcode.git
```

### From source

```bash
git clone https://github.com/harsha509/duetcode.git
cd duetcode
cargo install --path .
```

### Verify

```bash
dt --version
```

## Quick start

For a complete list of commands, flags, and configuration options, please see the [**Usage Guide (USAGE.md)**](USAGE.md).

### 1. Initialize in your project

```bash
cd your-project
dt init
```

This creates:
- `.duet/config.toml` — configuration file
- `.duet/prompts/` — editable prompt templates (implement, review, fix, plan)

### 2. Check your setup

```bash
dt doctor
```

Verifies: git repo, claude CLI, GEMINI_API_KEY, config file, prompt templates.

### 3. Run a task

```bash
dt "add input validation to the signup form"
```

Default: Claude writes, Gemini reviews. To flip:

```bash
dt "add input validation" --writer gemini
```

### 4. Plan before executing

For larger tasks, ask the AI to create a plan first:

```bash
dt plan "refactor the authentication flow"
```

This will:
1. Generate a plan without modifying code
2. Ask if you want Gemini to review the plan
3. Ask if you want to execute the approved plan

### 5. Pass screenshots

```bash
dt "match this design" --image ./mockup.png
dt "fix layout bug" --image ./before.png --image ./expected.png
```

Images are base64-encoded and sent to both Claude (via stream-json stdin) and Gemini (via inlineData API).

### 6. Review existing changes

Review uncommitted changes without running the full write loop. The reviewer analyzes the diff to understand what was changed and why, checks for correctness, edge cases, and potential issues:

```bash
dt review
dt review --reviewer claude
```

Optionally, tell the reviewer what task to verify against:

```bash
dt review --task "add OAuth login flow"
```

## Configuration

`.duet/config.toml` (created by `dt init`):

```toml
[claude]
command = "claude"
args = ["-p"]
model = "sonnet"
skip_permissions = false

[gemini]
model = "gemini-3.1-pro-preview"
api_key_env = "GEMINI_API_KEY"

[checks]
# Configure these for your project's toolchain
# test = "npm test"
# lint = "npm run lint"
# typecheck = "npx tsc --noEmit"

[policy]
max_rounds = 4
require_both_approvals = true
allow_dirty_worktree = true

[prompts]
implementation = ".duet/prompts/implement.txt"
review = ".duet/prompts/review.txt"
fix = ".duet/prompts/fix.txt"
```

### Configuration reference

| Section | Key | Description | Default |
|---------|-----|-------------|---------|
| `claude` | `command` | Path to the Claude CLI binary | `"claude"` |
| `claude` | `model` | Claude model to use | `"sonnet"` |
| `claude` | `skip_permissions` | Pass `--dangerously-skip-permissions` to Claude | `false` |
| `gemini` | `model` | Gemini model name | `"gemini-3.1-pro-preview"` |
| `gemini` | `api_key_env` | Environment variable holding the API key | `"GEMINI_API_KEY"` |
| `checks` | `test` | Test command to run | *none* (configure for your stack) |
| `checks` | `lint` | Lint command to run | *none* (configure for your stack) |
| `checks` | `typecheck` | Typecheck command to run | *none* (configure for your stack) |
| `policy` | `max_rounds` | Maximum write/review rounds before failure | `4` |
| `policy` | `require_both_approvals` | Require reviewer approval + passing checks | `true` |
| `policy` | `allow_dirty_worktree` | Allow running with uncommitted changes | `true` |
| `prompts` | `implementation` | Path to the implement prompt template | `".duet/prompts/implement.txt"` |
| `prompts` | `review` | Path to the review prompt template | `".duet/prompts/review.txt"` |
| `prompts` | `fix` | Path to the fix prompt template | `".duet/prompts/fix.txt"` |

### Setting up checks for your project

Edit `.duet/config.toml` to match your project's toolchain. Checks are optional — if not configured, the write/review loop still works but without automated verification.

```toml
# Python
[checks]
test = "pytest"
lint = "ruff check ."
typecheck = "mypy ."

# Node.js / TypeScript
[checks]
test = "npm test"
lint = "eslint ."
typecheck = "tsc --noEmit"

# Go
[checks]
test = "go test ./..."
lint = "golangci-lint run"
typecheck = "go vet ./..."

# Rust
[checks]
test = "cargo test"
lint = "cargo clippy -- -D warnings"
typecheck = "cargo check"
```

## Prompt templates

The `.duet/prompts/` directory contains editable templates:

- **`implement.txt`** — Sent to the writer on the first round. Variables: `{task}`, `{context}`
- **`review.txt`** — Sent to the reviewer each round. Variables: `{task}`, `{diff}`, `{checks}`
- **`fix.txt`** — Sent to the writer on rounds 2+ with review feedback. Variables: `{task}`, `{review_feedback}`
- **`plan.txt`** — Sent to the writer during plan mode. Variables: `{task}`, `{context}`

If a template file is missing, built-in defaults are used.

All prompts include a safety rule: models are prohibited from running `git add`, `git commit`, or `git push`. They can only edit files and leave them uncommitted.

### Reviewer output contract

The reviewer must respond in this format for reliable parsing:

```
VERDICT: APPROVED | CHANGES_REQUESTED

BLOCKERS:
- <issue>

SUGGESTIONS:
- <improvement>

TESTS_TO_ADD:
- <test>
```

### Writer output contract

The writer must respond with:

```
SUMMARY:
- <what was done>

ADDRESSED:
- <requirements met>

UNRESOLVED:
- <open items or "none">
```

## Session logs

Each run creates a log folder at `.duet/sessions/{timestamp}-{task-slug}/` containing:

| File | Content |
|------|---------|
| `prompt.md` | Original task description |
| `state.json` | Final outcome with metadata |
| `round-{n}/claude_out.md` | Writer's full response |
| `round-{n}/gemini_out.md` | Reviewer's full response |
| `round-{n}/claude.patch` | Git diff for that round |
| `round-{n}/checks.json` | Check results (pass/fail + output) |

Clear all sessions with `dt clear`.

## Commands

| Command | Description |
|---------|-------------|
| `dt init` | Create `.duet/config.toml` and `.duet/prompts/` in the current repo |
| `dt doctor` | Verify all dependencies and configuration |
| `dt <task>` | Run the full write/review loop (shorthand for `dt run`) |
| `dt run <task>` | Run the full write/review loop |
| `dt run <task> --writer gemini` | Use Gemini as writer, Claude as reviewer |
| `dt run <task> --image <path>` | Include screenshot(s) as context |
| `dt plan <task>` | Plan first, then execute after approval |
| `dt review` | Review current uncommitted changes |
| `dt review --task "description"` | Review changes against a specific task |
| `dt review --reviewer claude` | Use Claude as the reviewer |
| `dt clear` | Clear all past session logs |

## Exit codes

| Code | Meaning |
|------|---------|
| `0` | Reviewer approved and all checks pass |
| `1` | Failure: max rounds exceeded, checks failed, or error |

## Architecture

```
src/
  main.rs           Entry point
  cli.rs            Clap subcommands and dispatch
  config.rs         duet.toml parsing
  orchestrator.rs   Round loop: write → diff → check → review → verdict
  adapters/
    mod.rs          ModelAdapter trait + ImageInput
    claude.rs       Claude CLI subprocess adapter
    gemini.rs       Gemini REST API adapter
  git.rs            Git operations (diff, status, branch)
  checks.rs         Test/lint/typecheck runners
  prompts.rs        Template loading and interpolation
  policy.rs         Verdict parsing and pass/fail evaluation
  logs.rs           Per-round session logging
  errors.rs         Typed error definitions
```

## License

MIT

# duetcode (dt)

AI pair programming CLI — one model writes code, another reviews it, with you in control.

`dt` orchestrates Claude and Gemini in a structured write/review cycle. One model implements a task, the other reviews the diff against your tests and linters, and the loop continues until the reviewer approves and all checks pass. Run it interactively (you gate each round), fully automatic (`--auto`, the models iterate until mutual approval and only escalate to you if they deadlock), as a persistent session, or from the [VS Code extension](https://marketplace.visualstudio.com/items?itemName=harsha509.dt-duet).

## How it works

```
You give a task
    → Claude (writer) implements it              [keeps its session across rounds]
    → git diff captured (new files included)
    → your checks run (test / lint / typecheck)
    → Gemini (reviewer) reviews diff + checks + writer's notes
    → APPROVED and checks pass?  → done
    → CHANGES_REQUESTED?         → writer fixes → repeat
    → models stuck on the same blockers? → you're asked for guidance
```

- **Interactive mode** (default): you confirm each review and fix round.
- **Auto mode** (`--auto`): no prompts — the loop runs until approval, a round
  budget is exhausted, or the models stop converging, at which point dt shows
  the open blockers and asks you for one clarification (injected into both
  models' prompts) before continuing.
- **Question tasks**: if the writer answers without changing code (e.g. "do we
  have performance issues?"), the reviewer gives a second opinion on the
  answer itself, and the writer revises until the answer is found sound. An
  answer verdict reads `SOUND` / `UNSOUND` rather than `APPROVED`, because it
  judges the answer and not the code the answer is about — those are regularly
  opposite, since an answer can soundly argue against a change.
- **Questions about a pull request**: name the pull requests in the task and dt
  fetches their diffs with `gh pr diff`, so the reviewer judges the answer
  against the change itself rather than against whatever is checked out. If the
  change cannot be fetched, the review is refused instead of run against the
  wrong revision — install and authenticate [`gh`](https://cli.github.com), or
  check the branch out locally, and run it again. The answer opens with a
  verdict — `GO` or `NO-GO`, then a `BLOCKER:`/`WARNING:` line per finding in
  one plain sentence each — so the merge decision is readable without working
  through the analysis first. Give the full URL: `owner/repo#123` is a label dt
  prints, not one it reads.
- Both models keep context: the Claude CLI session is resumed across rounds
  (`--resume`), and API-mode Claude and Gemini carry capped message history.
- Flip roles anytime with `--writer gemini`.

## Installation

### Prerequisites

- [Rust](https://rustup.rs/) (1.80+)
- [Claude Code CLI](https://claude.ai/download) installed and authenticated
- [Gemini CLI](https://github.com/google-gemini/gemini-cli) (`npm i -g @google/gemini-cli`),
  or a [Gemini API key](https://aistudio.google.com/apikey) exported as `GEMINI_API_KEY`
- Git
- Optional: [`gh`](https://cli.github.com), authenticated — needed only to
  review answers about a pull request that is not checked out locally

Install the Gemini CLI if you can. Only the CLI can open the files it is
reviewing — over the API the reviewer is a bare HTTPS call with no tools, and
its verdict can never be more than an opinion on the text it was handed.

### Using Cargo (recommended)

```bash
cargo install --git https://github.com/harsha509/duetcode --tag <version>
```

Use the newest tag from the [releases page](https://github.com/harsha509/duetcode/releases)
— for example `--tag v0.2.3`. Without `--tag` you get whatever is on `main`,
which may include unreleased work.

Every release also carries prebuilt Linux and macOS binaries, if you would
rather not build from source.

### Upgrading

```bash
cargo install --git https://github.com/harsha509/duetcode --tag <version> --force
```

`--force` is required. Without it cargo sees that `dt` is already installed and
stops, so the upgrade silently does nothing.

The first `dt` run in a project after an upgrade brings its `.duet/prompts/`
up to date with the new binary — replacing copies you have not edited, and
leaving the ones you have.

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

For the full command reference see the [Usage Guide (USAGE.md)](USAGE.md).

### 1. Initialize and verify

```bash
cd your-project
dt init      # creates .duet/config.toml and .duet/prompts/
dt doctor    # checks git, claude CLI, gemini CLI / GEMINI_API_KEY, config
```

### 2. Run a task

```bash
dt "add input validation to the signup form"          # interactive
dt "add input validation to the signup form" --auto   # loop until both approve
dt "add input validation" --writer gemini              # flip roles
```

### 3. Or start a session

Bare `dt` in an initialized repo opens an interactive session where both
models keep their context across tasks:

```
$ dt
dt ❯ add a dark mode toggle
dt ❯ now persist the preference        ← both models remember the previous task
dt ❯ /image ~/Desktop/mock.png         ← attach a screenshot to the next task
dt ❯ /paste                            ← attach the image on the clipboard
dt ❯ /auto                             ← toggle autonomous looping
dt ❯ /plan refactor the auth flow      ← plan → review → approve → execute
dt ❯ /review                           ← review uncommitted changes
dt ❯ /quit
```

### 4. Plan before executing

```bash
dt plan "refactor the authentication flow"
```

Generates a plan (no code changes), offers a plan review by the other model,
then asks before executing.

### 5. Pass screenshots

```bash
dt "match this design" --image ./mockup.png
dt "fix layout bug" --image ./before.png --image ./expected.png
```

### 6. Review existing changes

```bash
dt review                          # Gemini reviews your uncommitted changes
dt review --reviewer claude
dt review --task "add OAuth login" # tell the reviewer what to verify against
```

### 7. VS Code

Install **[DT Duet](https://marketplace.visualstudio.com/items?itemName=harsha509.dt-duet)**
from the VS Code Marketplace, or search "DT Duet" in the Extensions view.

It gives you a sessions sidebar, a live duet panel (writer stream stacked
over reviewer verdicts per round), Cmd+V screenshot paste, button-based
approvals, and keychain-stored API keys. It talks to `dt serve`, a
JSON-lines protocol over stdio that any frontend can use — the extension
still needs the `dt` binary installed above. Source lives in
[`editors/vscode/`](editors/vscode/).

## Configuration

`.duet/config.toml` (created by `dt init`):

```toml
[claude]
command = "claude"
model = "sonnet"
skip_permissions = true      # writer edits files without interactive prompts
mode = "auto"                # "cli", "api", or "auto" (CLI with API fallback)
api_key_env = "ANTHROPIC_API_KEY"
api_model = "claude-sonnet-4-20250514"
timeout_secs = 300

[gemini]
command = "gemini"
mode = "auto"                # "cli", "api", or "auto" (CLI with API fallback)
model = "gemini-3.1-pro-preview"
api_key_env = "GEMINI_API_KEY"
timeout_secs = 300

[checks]
# Configure these for your project's toolchain
# test = "npm test"
# lint = "npm run lint"
# typecheck = "npx tsc --noEmit"

[policy]
max_rounds = 4
auto = false                 # true = --auto by default
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
| `claude` | `model` | Claude model for CLI mode | `"sonnet"` |
| `claude` | `skip_permissions` | Pass `--dangerously-skip-permissions` so the writer can edit files unattended | `true` |
| `claude` | `mode` | `"cli"`, `"api"`, or `"auto"` (CLI first, API fallback) | `"auto"` |
| `claude` | `api_key_env` | Env var holding the Anthropic API key | `"ANTHROPIC_API_KEY"` |
| `claude` | `api_model` | Model id for API mode | `"claude-sonnet-4-20250514"` |
| `gemini` | `command` | Path to the Gemini CLI binary | `"gemini"` |
| `gemini` | `mode` | `"cli"`, `"api"`, or `"auto"` (CLI first, API fallback). Only the CLI can read the code it reviews | `"auto"` |
| `gemini` | `model` | Gemini model name, used by both transports | `"gemini-3.1-pro-preview"` |
| `gemini` | `api_key_env` | Env var holding the API key | `"GEMINI_API_KEY"` |
| `checks` | `test` / `lint` / `typecheck` | Commands run before each review | *none* |
| `policy` | `max_rounds` | Round budget (auto mode may extend once, to 2×, after your clarification) | `4` |
| `policy` | `auto` | Run autonomously by default | `false` |
| `policy` | `allow_dirty_worktree` | Allow starting with uncommitted changes | `true` |
| `prompts` | `implementation` / `review` / `fix` | Template paths | `.duet/prompts/…` |

### Setting up checks for your project

Checks are optional — without them the loop still works, just without
automated verification.

```toml
# Python                      # Node.js / TypeScript
[checks]                      [checks]
test = "pytest"               test = "npm test"
lint = "ruff check ."         lint = "eslint ."
typecheck = "mypy ."          typecheck = "tsc --noEmit"

# Go                          # Rust
[checks]                      [checks]
test = "go test ./..."        test = "cargo test"
lint = "golangci-lint run"    lint = "cargo clippy -- -D warnings"
typecheck = "go vet ./..."    typecheck = "cargo check"
```

## Prompt templates

`.duet/prompts/` contains editable templates (built-in defaults are used if a
file is missing):

- **`implement.txt`** — writer, round 1. Variables: `{task}`, `{context}`
- **`review.txt`** — reviewer, every code round. Variables: `{task}`, `{diff}`, `{checks}`, `{writer_notes}`
- **`fix.txt`** — writer, rounds 2+. Variables: `{task}`, `{review_feedback}`
- **`plan.txt`** — writer, plan mode. Variables: `{task}`, `{context}`

Plan review and answer review (second opinions on plans and on text answers)
use built-in templates. All prompts forbid the models from running `git add`,
`git commit`, or `git push` — changes stay uncommitted for you to inspect.

The reviewer ends every review with machine-parsed sections:

```
FILES READ: <paths>, or none

BLOCKERS:
- <defect that has to be fixed before this can merge>

SUGGESTIONS:
- <improvement>
- Nit: <optional — taste or polish, raised freely and never blocking>

VERDICT: APPROVED | CHANGES_REQUESTED
```

A review of an *answer* ends `VERDICT: SOUND | UNSOUND` instead.

`FILES READ:` is checked, not taken on trust. dt records the files the reviewer
actually opened from its own tool calls, and warns when a review names one it
never read:

```
⚠ gemini listed services/billing.py as read, but opened only utils/vapi.py
  this turn — treat that part of the review as unverified
```

It is the one claim in a review that does not have to be believed, so `none` is
a legitimate answer — a review of the diff alone is still a review.

A blocker has to be a defect the reviewer can say how to reach; anything it
could not verify, and anything it would merely have written differently, is a
suggestion. Standalone reviews (`dt review`, the panel's review button) start
from a clean model session, so a verdict is a fresh judgement rather than a
continuation of whatever the reviewer last concluded. Rounds within one task
keep their continuity, which is where it earns its keep.

## Session logs

Each run creates `.duet/sessions/{timestamp}-{task-slug}/`:

| File | Content |
|------|---------|
| `prompt.md` | Original task |
| `roles.json` | Which model wrote and which reviewed, written when the session is created |
| `state.json` | Final outcome and metadata |
| `round-{n}/claude_out.md` | Writer's response |
| `round-{n}/gemini_out.md` | Reviewer's response |
| `round-{n}/claude.patch` | Diff for that round (new files included) |
| `round-{n}/checks.json` | Check results |
| `round-{n}/clarification.md` | Your guidance, when the models deadlocked |

`dt clear` removes all session logs. The directory is gitignored by `dt init`.

## Commands

| Command | Description |
|---------|-------------|
| `dt` | Interactive session in an initialized repo (usage screen elsewhere) |
| `dt <task>` | Run the write/review loop (shorthand for `dt run`) |
| `dt <task> --auto` | Loop without prompts until both models approve |
| `dt <task> --writer gemini` | Gemini writes, Claude reviews |
| `dt <task> --image <path>` | Include screenshot(s) |
| `dt <task> -c` | Continue from the previous session's context |
| `dt plan <task>` | Plan → review → approve → execute |
| `dt review [--task "…"] [--reviewer claude]` | Review uncommitted changes |
| `dt init` / `dt doctor` / `dt clear` | Setup, diagnostics, log cleanup |
| `dt serve` | JSON-lines server for GUI frontends (VS Code extension) |

## Exit codes

| Code | Meaning |
|------|---------|
| `0` | Approved with all checks passing (or task completed by your choice) |
| `1` | Stopped without full approval, or error |

## Architecture

```
src/
  main.rs           Entry point
  cli.rs            Clap commands and dispatch
  config.rs         .duet/config.toml parsing
  orchestrator.rs   Round loop: write → diff → check → review → verdict,
                    stall detection, user escalation
  events.rs         Event enum + Sink trait; TerminalSink renders the CLI
  serve.rs          `dt serve` JSON-lines protocol for frontends
  repl.rs           Interactive `dt ❯` session
  adapters/
    mod.rs          ModelAdapter trait, ImageInput, history trimming
    claude.rs       Claude CLI (session --resume) + Anthropic API adapter
    gemini.rs       Gemini REST adapter with conversation history
    pricing.rs      Cost estimation
  git.rs            Diff (tracked + untracked), status, branch
  checks.rs         Test/lint/typecheck runners
  prompts.rs        Templates and interpolation
  policy.rs         Verdict parsing, blocker similarity (stall detection)
  logs.rs           Per-round session logging
  ui.rs             All terminal rendering and input

editors/vscode/     VS Code extension (thin TypeScript client over dt serve)
```

## License

MIT

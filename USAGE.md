# duetcode (`dt`) Usage Guide

`dt` orchestrates two LLM agents: a **Writer** (default: Claude) that implements code, and a **Reviewer** (default: Gemini) that reviews the changes against your linters and tests. This guide covers every command and mode.

---

## Core Commands

### 1. `dt` (Interactive Session)

Bare `dt` in an initialized repo opens a persistent session. Both models keep their context for the whole session — follow-up tasks build on what was just done, on both the writer and reviewer side.

```
$ dt
dt ❯ add a dark mode toggle
dt ❯ now persist it to localStorage      ← full memory of the previous task
```

Session commands:

| Command | Effect |
|---|---|
| `/auto` | Toggle autonomous looping on/off |
| `/image <path>` | Stage a screenshot for the next task (repeatable; handles drag-and-drop paths and `~/`) |
| `/paste` | Stage the image currently on the OS clipboard (`Cmd+Ctrl+Shift+4` → snip → `/paste`) |
| `/plan <task>` | Plan first, then execute after approval |
| `/review [task]` | Review uncommitted changes |
| `/help`, `/quit` | Help / leave |

Anything else you type is sent to the duet as a task.

### 2. `dt <task>` (One-shot Run)

```bash
dt "add input validation to the signup form"
```

**Interactive flow (default):**
1. The writer reads your codebase and makes the change.
2. `dt` shows the diff stat and asks: `review changes with gemini? (y/n)`
3. Your configured checks run; the reviewer gets the diff, check results, and the writer's own explanation of what it did.
4. Verdict: `APPROVED` (and checks pass) → done. Otherwise: `let claude fix the issues? (y/n)` and the loop continues.

**Auto flow (`--auto`):** same loop with no questions — the models iterate until mutual approval or the round budget (`max_rounds`) runs out. If the reviewer keeps raising the same blockers or the writer stops making progress, dt pauses, shows you the disputed blockers, and asks for one clarification; your guidance is injected into **both** models' prompts and the loop resumes (budget extends once, up to 2×`max_rounds`).

```bash
dt "add input validation" --auto
```

**Question tasks:** if the task is a question ("do we have performance issues?"), the writer answers without touching code — and the reviewer then gives a second opinion on the answer itself (auto: always; interactive: you're asked). Wrong or incomplete answers loop back for revision like code does.

**Role flip / images / continuity:**
```bash
dt "fix bug" --writer gemini          # Gemini writes, Claude reviews
dt "match this" --image mockup.png    # attach screenshots
dt "fix the test" -c                  # carry previous session's context
```

### 3. `dt plan <task>`

For large or ambiguous tasks: plan before touching files.

```bash
dt plan "refactor the database connection logic"
```

1. The writer produces a Markdown plan (no code changes).
2. `review this plan with gemini? (y/n)` — the reviewer critiques the approach.
3. `execute this task? (y/n)` — on yes, implementation starts with the approved plan as context (add `--auto` to run the execution loop unattended).

### 4. `dt review`

Review uncommitted changes you wrote yourself — including new untracked files, which are folded into the diff.

```bash
dt review
dt review --task "add OAuth login flow"   # verify against your intent
dt review --reviewer claude
```

---

## Setup & Utility Commands

### `dt init`

Creates `.duet/config.toml` (checks, models, policy) and `.duet/prompts/` (editable prompt templates), and gitignores `.duet/sessions/`.

### `dt doctor`

Verifies: git repo, `.duet/config.toml` parses, Claude CLI presence and auth, `ANTHROPIC_API_KEY` fallback, `GEMINI_API_KEY`, prompt templates.

### `dt clear`

Deletes all session logs.

### `dt serve`

Runs dt as a JSON-lines server on stdin/stdout for GUI frontends — this is what the [VS Code extension](https://marketplace.visualstudio.com/items?itemName=harsha509.dt-duet) talks to. Commands in (`task`, `plan`, `review`, `answer`, `ping`, `quit`), events out (`round_started`, `stream_chunk`, `verdict`, `ask`, `task_done`, …). Adapters persist across tasks, so context carries exactly like the interactive session.

---

## Global Flags

| Flag | Applies to | Description |
|---|---|---|
| `-a, --auto` | `dt <task>`, `dt run`, `dt plan`, bare `dt` | Loop without per-round prompts until both models approve |
| `--writer <model>` | task commands, `dt serve` | Which model writes (`claude`/`gemini`); the other reviews |
| `--reviewer <model>` | `dt review` | Which model reviews |
| `-t, --task <desc>` | `dt review` | Intent for the reviewer to verify against |
| `--image <path>` | task commands | Attach one or more images |
| `-c, --continue-session` | task commands | Include the previous session's context |
| `-v, --verbose` | all | Full untruncated output and diagnostics |

---

## Configuration (`.duet/config.toml`)

### Quality checks

Run before every review; failures are shown to the reviewer and block approval until fixed.

```toml
[checks]
test = "npm test"
lint = "npm run lint"
typecheck = "npx tsc --noEmit"
```

### Policy

```toml
[policy]
max_rounds = 4      # round budget per task
auto = false        # true = behave as if --auto was always passed
allow_dirty_worktree = true
```

### Claude modes

```toml
[claude]
mode = "auto"       # "cli" (Claude Code CLI, keeps its session via --resume),
                    # "api" (direct Anthropic API with message history),
                    # "auto" (CLI first, API fallback)
skip_permissions = true   # let the writer edit files without interactive prompts
```

### Customizing prompts

Edit the files in `.duet/prompts/` to add project rules (e.g. "always use Tailwind utilities"). Available variables: `implement.txt` — `{task}`, `{context}` · `review.txt` — `{task}`, `{diff}`, `{checks}`, `{writer_notes}` · `fix.txt` — `{task}`, `{review_feedback}` · `plan.txt` — `{task}`, `{context}`.

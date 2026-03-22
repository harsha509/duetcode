# duetcode (`dt`) Usage Guide

`dt` is an AI pair programming CLI that orchestrates two LLM agents: a **Writer** (default: Claude) that implements code, and a **Reviewer** (default: Gemini) that reviews the changes against your linters and tests.

This guide covers all available commands and how to use them effectively.

---

## Core Commands

### 1. `dt <task>` (Default Run)

The primary command. It takes a natural language task, asks the Writer to implement it, and then interactively guides you through reviewing and fixing the code.

**Basic usage:**
```bash
dt "add input validation to the signup form"
```

**With images (e.g., UI mockups or bug screenshots):**
```bash
dt "make the button look like this" --image ./mockup.png
```

**Interactive Flow:**
1. Claude reads your codebase and writes the code.
2. `dt` shows you the exact files changed.
3. You are asked: `Review changes with gemini? (y/n)`
4. If `y`, `dt` runs your local tests/linters and sends the diff + test results to Gemini.
5. Gemini replies with `APPROVED` or `CHANGES_REQUESTED`.
6. If changes are requested, you are asked: `Let claude fix? (y/n)`.
7. If `y`, the loop continues.

### 2. `dt plan <task>`

For large, complex, or ambiguous tasks, it's best to ask the AI to write a plan *before* it starts editing files.

**Usage:**
```bash
dt plan "refactor the database connection logic"
```

**Interactive Flow:**
1. Claude explores the codebase and writes a Markdown plan (no files are changed).
2. You are asked: `Review this plan with gemini? (y/n)`
3. If `y`, Gemini critiques the architecture and approach.
4. You are asked: `Execute this task? (y/n)`
5. If `y`, Claude begins implementing the code using the approved plan as context.

### 3. `dt review`

If you have already written some code yourself and just want Gemini to review your uncommitted changes, use the review command. The reviewer analyzes the diff to understand what was changed and why, checks for correctness, edge cases, and potential issues — even without a task description.

**Usage:**
```bash
dt review
```

**With a task description** (helps the reviewer verify changes against your intent):
```bash
dt review --task "add OAuth login flow"
```

This will:
1. Capture your current `git diff`.
2. Send the diff to the Reviewer.
3. The Reviewer analyzes the changes: understands intent, verifies correctness, checks edge cases, assesses impact.
4. Output the Reviewer's structured feedback and verdict.

---

## Setup & Utility Commands

### `dt init`

Initializes `dt` in a new repository.

**Usage:**
```bash
cd my-project
dt init
```

This creates:
- `.duet/config.toml`: The configuration file where you define your linters and model preferences.
- `.duet/prompts/`: A directory containing the default system prompts. You can edit these to customize how the agents behave in your specific project.

### `dt doctor`

Diagnoses your environment to ensure `dt` can run properly.

**Usage:**
```bash
dt doctor
```

Checks for:
- Git repository presence
- Claude CLI installation and authentication
- `GEMINI_API_KEY` environment variable
- Valid `.duet/config.toml` configuration

---

## Global Flags

| Flag | Applies to | Description | Example |
|---|---|---|---|
| `--writer <model>` | `dt <task>`, `dt plan` | Override the default writer model. | `dt "fix bug" --writer gemini` |
| `--reviewer <model>` | `dt review` | Override the default reviewer model. | `dt review --reviewer claude` |
| `-t, --task <desc>` | `dt review` | Describe the task for the reviewer to verify against. | `dt review --task "add login"` |
| `-i, --image <path>` | `dt <task>`, `dt plan` | Attach one or more images to the prompt. | `dt "fix UI" -i bug.png` |
| `-c` | `dt <task>`, `dt plan` | Continue from the previous session's context. | `dt "fix the test" -c` |
| `-v, --verbose` | All commands | Show raw API events and full un-truncated outputs. | `dt "fix bug" -v` |

---

## Configuration (`.duet/config.toml`)

When you run `dt init`, a `.duet/config.toml` file is created. Here is how to configure it:

### Quality Checks
`dt` runs these commands before asking the Reviewer to look at the code. If a check fails, the Reviewer is given the error output so it can suggest a fix. Checks are optional — if not configured, the write/review loop still works but without automated verification.

```toml
[checks]
# Configure these for your project's toolchain:
# test = "npm test"
# lint = "npm run lint"
# typecheck = "npx tsc --noEmit"
```

Examples for common stacks:

```toml
# Python
test = "pytest"
lint = "ruff check ."
typecheck = "mypy ."

# Rust
test = "cargo test"
lint = "cargo clippy -- -D warnings"
typecheck = "cargo check"

# Go
test = "go test ./..."
lint = "golangci-lint run"
typecheck = "go vet ./..."
```

### Policy
Control how many times the agents are allowed to loop before giving up.

```toml
[policy]
max_rounds = 3
```

### Customizing Prompts
If you want the agents to follow specific coding guidelines (e.g., "Always use Tailwind utility classes"), you can edit the files in the `.duet/prompts/` directory created by `dt init`. The `.duet/config.toml` file maps to these templates:

```toml
[prompts]
implementation = ".duet/prompts/implement.txt"
review = ".duet/prompts/review.txt"
fix = ".duet/prompts/fix.txt"
```
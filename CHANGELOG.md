# Changelog

All notable changes to the `dt` CLI are documented here. The VS Code extension
keeps its own changelog in [editors/vscode/CHANGELOG.md](editors/vscode/CHANGELOG.md).

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/).

Releases before 0.1.3 predate this file; see the git history for those.

## 0.1.3 - 2026-08-01

### Fixed

- **A writer that answers a question is reviewed on its answer.** Whether the
  round produced code was decided from the state of the whole worktree rather
  than from what the writer did, so an unrelated uncommitted change already
  present when the run started was handed to the reviewer as the writer's work.
  Asking `dt` a question in a repository with any pending edit got a review of
  that edit instead of the answer. The answer-review path — which verifies the
  claims in a written answer — existed but could only be reached with a
  completely clean tree.
- **An empty diff is never sent for review.** A writer that reverted the tree
  back to clean left nothing to review, and the reviewer, given an empty diff,
  could only approve it. That approval ended the loop and reported the run as a
  success, exit code 0 included.
- **Verdict parsing fails closed.** With no `VERDICT:` line, the last few lines
  were scanned for the word "approved" behind a single literal `NOT APPROVED`
  guard, so ordinary prose — "this cannot be approved until the null check is
  added" — was read as an approval and reported to the user as success. Only a
  line that is nothing but the word now counts; everything else is a rejection.
- **`VERDICT: NOT APPROVED` is a rejection.** The approval branch was tested
  before the rejection branch, and "NOT APPROVED" contains "APPROVED", so an
  explicit rejection in that wording was recorded as an approval.
- **Blockers survive indentation and numbered lists.** Bullets were matched on
  the raw line, so `  - indented` and `1. numbered` were dropped. A reviewer
  that consistently indents its bullets produced an empty blocker list, which in
  turn disabled blocker-based stall detection entirely and left the escalation
  prompt showing no open blockers.
- **Non-ASCII tool arguments no longer abort the process.** Tool-use progress
  lines were truncated by slicing at a byte index while measuring in bytes, so a
  cut landing inside a multibyte character panicked — a non-ASCII file path, an
  accented Bash command, a CJK `Grep` pattern. Release builds set
  `panic = "abort"`, so this killed `dt` mid-run and lost the session.
  Truncation is now measured and cut in characters.
- **`mode = "Auto"` falls back to the API.** The constructor lowercased `mode`
  before deciding whether to build the HTTP agent, but the CLI-failure fallback
  compared the raw value, so any capitalisation other than `auto` built the
  agent and then never used it — a CLI failure became a hard error.
- **`dt init` no longer dirties `.gitignore`.** The guard tested for the literal
  `.duet/sessions/`, which a broader existing `.duet/` rule does not contain, so
  every run appended a duplicate entry that ignored nothing new. Any rule
  already covering the sessions directory is now recognised.

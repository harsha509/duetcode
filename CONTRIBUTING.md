# Contributing to duetcode

First off, thank you for considering contributing to `duetcode`! It's people like you that make open source such a great community.

## Development Setup

1. Ensure you have Rust installed (1.80+)
2. Clone the repository:
   ```bash
   git clone https://github.com/harsha509/duetcode.git
   cd duetcode
   ```
3. Build the project:
   ```bash
   cargo build
   ```
4. Run tests:
   ```bash
   cargo test
   ```

## Architecture Overview

`dt` is built around an interactive state machine that orchestrates two LLM agents:

- **Writer**: Implements code changes or answers questions (default: Claude)
- **Reviewer**: Reviews diffs and linter/test outputs (default: Gemini)
- **Orchestrator**: The CLI itself, which manages state, runs local checks, and prompts the user for decisions.

Key directories:
- `src/adapters/`: Implementations for different LLM providers (Claude CLI, Gemini API)
- `src/orchestrator.rs`: The main state machine and user interaction loop
- `src/policy.rs`: Logic for parsing reviewer verdicts (APPROVED / CHANGES_REQUESTED)
- `src/checks.rs`: Execution of local linters and tests

## Pull Request Process

1. Fork the repo and create your branch from `main`.
2. If you've added code that should be tested, add tests.
3. Ensure the test suite passes (`cargo test`).
4. Make sure your code lints (`cargo clippy`).
5. Format your code (`cargo fmt`).
6. Issue that pull request!

## License

By contributing, you agree that your contributions will be licensed under its MIT License.
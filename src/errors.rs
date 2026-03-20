use thiserror::Error;

#[derive(Error, Debug)]
#[allow(dead_code)]
pub enum DuetError {
    #[error("not a git repository (run `git init` first)")]
    NotGitRepo,

    #[error("duet.toml not found (run `duetcode init` first)")]
    ConfigNotFound,

    #[error("failed to parse duet.toml: {0}")]
    ConfigParse(String),

    #[error("claude CLI not found — install from https://claude.ai/download")]
    ClaudeNotFound,

    #[error("GEMINI_API_KEY not set — export it or add to your shell profile")]
    GeminiKeyMissing,

    #[error("adapter error ({adapter}): {message}")]
    Adapter { adapter: String, message: String },

    #[error("no changes produced by {adapter} in round {round}")]
    NoChanges { adapter: String, round: usize },

    #[error("max rounds ({max}) exceeded without approval")]
    MaxRoundsExceeded { max: usize },

    #[error("checks failed after approval: {details}")]
    ChecksFailed { details: String },

    #[error("image not found: {path}")]
    ImageNotFound { path: String },

    #[error("unsupported image format: {extension}")]
    UnsupportedImageFormat { extension: String },
}

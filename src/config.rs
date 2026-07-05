use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const CONFIG_FILENAME: &str = ".duet/config.toml";

#[derive(Debug, Default, Deserialize, Serialize)]
pub struct Config {
    #[serde(default)]
    pub claude: ClaudeConfig,
    #[serde(default)]
    pub gemini: GeminiConfig,
    #[serde(default)]
    pub checks: ChecksConfig,
    #[serde(default)]
    pub policy: PolicyConfig,
    #[serde(default)]
    pub prompts: PromptsConfig,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ClaudeConfig {
    #[serde(default = "default_claude_command")]
    pub command: String,
    #[serde(default = "default_claude_model")]
    pub model: String,
    /// Pass --dangerously-skip-permissions so the writer can edit files
    /// without interactive prompts. Disable to run with normal permissions.
    #[serde(default = "default_true")]
    pub skip_permissions: bool,
    /// "cli", "api", or "auto" (try CLI, fall back to API)
    #[serde(default = "default_claude_mode")]
    pub mode: String,
    #[serde(default = "default_claude_api_key_env")]
    pub api_key_env: String,
    #[serde(default = "default_claude_api_model")]
    pub api_model: String,
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct GeminiConfig {
    #[serde(default = "default_gemini_model")]
    pub model: String,
    #[serde(default = "default_gemini_api_key_env")]
    pub api_key_env: String,
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,
}

#[derive(Debug, Default, Deserialize, Serialize)]
pub struct ChecksConfig {
    #[serde(default)]
    pub test: Option<String>,
    #[serde(default)]
    pub lint: Option<String>,
    #[serde(default)]
    pub typecheck: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct PolicyConfig {
    #[serde(default = "default_max_rounds")]
    pub max_rounds: usize,
    /// Run the write→review loop without per-round confirmation prompts.
    #[serde(default)]
    pub auto: bool,
    #[serde(default = "default_true")]
    pub allow_dirty_worktree: bool,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct PromptsConfig {
    #[serde(default = "default_implement_prompt")]
    pub implementation: PathBuf,
    #[serde(default = "default_review_prompt")]
    pub review: PathBuf,
    #[serde(default = "default_fix_prompt")]
    pub fix: PathBuf,
}

fn default_claude_command() -> String { "claude".into() }
fn default_claude_model() -> String { "sonnet".into() }
fn default_claude_mode() -> String { "auto".into() }
fn default_claude_api_key_env() -> String { "ANTHROPIC_API_KEY".into() }
fn default_claude_api_model() -> String { "claude-sonnet-4-20250514".into() }
fn default_gemini_model() -> String { "gemini-3.1-pro-preview".into() }
fn default_gemini_api_key_env() -> String { "GEMINI_API_KEY".into() }
fn default_timeout_secs() -> u64 { 300 }
fn default_max_rounds() -> usize { 4 }
fn default_true() -> bool { true }
fn default_implement_prompt() -> PathBuf { PathBuf::from(".duet/prompts/implement.txt") }
fn default_review_prompt() -> PathBuf { PathBuf::from(".duet/prompts/review.txt") }
fn default_fix_prompt() -> PathBuf { PathBuf::from(".duet/prompts/fix.txt") }

impl Default for ClaudeConfig {
    fn default() -> Self {
        Self {
            command: default_claude_command(),
            model: default_claude_model(),
            skip_permissions: true,
            mode: default_claude_mode(),
            api_key_env: default_claude_api_key_env(),
            api_model: default_claude_api_model(),
            timeout_secs: default_timeout_secs(),
        }
    }
}

impl Default for GeminiConfig {
    fn default() -> Self {
        Self {
            model: default_gemini_model(),
            api_key_env: default_gemini_api_key_env(),
            timeout_secs: default_timeout_secs(),
        }
    }
}

impl Default for PolicyConfig {
    fn default() -> Self {
        Self {
            max_rounds: default_max_rounds(),
            auto: false,
            allow_dirty_worktree: true,
        }
    }
}

impl Default for PromptsConfig {
    fn default() -> Self {
        Self {
            implementation: default_implement_prompt(),
            review: default_review_prompt(),
            fix: default_fix_prompt(),
        }
    }
}

impl Config {
    pub fn load(dir: &Path) -> Result<Self> {
        let path = dir.join(CONFIG_FILENAME);
        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let config: Config =
            toml::from_str(&content).with_context(|| format!("failed to parse {}", path.display()))?;
        Ok(config)
    }

    pub fn config_path(dir: &Path) -> PathBuf {
        dir.join(CONFIG_FILENAME)
    }

    pub fn write_default(dir: &Path) -> Result<PathBuf> {
        let config = Self::default();
        let content = toml::to_string_pretty(&config)
            .context("failed to serialize default config")?;
        let path = Self::config_path(dir);

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        std::fs::write(&path, content)
            .with_context(|| format!("failed to write {}", path.display()))?;
        Ok(path)
    }
}

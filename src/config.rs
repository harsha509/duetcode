use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const CONFIG_FILENAME: &str = ".duet/config.toml";

#[derive(Debug, Deserialize, Serialize)]
pub struct Config {
    pub claude: ClaudeConfig,
    pub gemini: GeminiConfig,
    pub checks: ChecksConfig,
    pub policy: PolicyConfig,
    pub prompts: PromptsConfig,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ClaudeConfig {
    pub command: String,
    #[serde(default = "default_claude_args")]
    pub args: Vec<String>,
    #[serde(default = "default_claude_model")]
    pub model: String,
    #[serde(default)]
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

#[derive(Debug, Deserialize, Serialize)]
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
    #[serde(default = "default_true")]
    pub require_both_approvals: bool,
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

fn default_claude_args() -> Vec<String> {
    vec!["-p".to_string()]
}
fn default_claude_model() -> String {
    "sonnet".to_string()
}
fn default_claude_mode() -> String {
    "auto".to_string()
}
fn default_claude_api_key_env() -> String {
    "ANTHROPIC_API_KEY".to_string()
}
fn default_claude_api_model() -> String {
    "claude-sonnet-4-20250514".to_string()
}
fn default_gemini_model() -> String {
    "gemini-3.1-pro-preview".to_string()
}
fn default_gemini_api_key_env() -> String {
    "GEMINI_API_KEY".to_string()
}
fn default_timeout_secs() -> u64 {
    300
}
fn default_max_rounds() -> usize {
    4
}
fn default_true() -> bool {
    true
}
fn default_implement_prompt() -> PathBuf {
    PathBuf::from(".duet/prompts/implement.txt")
}
fn default_review_prompt() -> PathBuf {
    PathBuf::from(".duet/prompts/review.txt")
}
fn default_fix_prompt() -> PathBuf {
    PathBuf::from(".duet/prompts/fix.txt")
}

impl Default for ClaudeConfig {
    fn default() -> Self {
        Self {
            command: "claude".to_string(),
            args: default_claude_args(),
            model: default_claude_model(),
            skip_permissions: false,
            mode: default_claude_mode(),
            api_key_env: default_claude_api_key_env(),
            api_model: default_claude_api_model(),
            timeout_secs: default_timeout_secs(),
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

    pub fn default_config() -> Self {
        Config {
            claude: ClaudeConfig {
                command: "claude".to_string(),
                args: default_claude_args(),
                model: default_claude_model(),
                skip_permissions: false,
                mode: default_claude_mode(),
                api_key_env: default_claude_api_key_env(),
                api_model: default_claude_api_model(),
                timeout_secs: default_timeout_secs(),
            },
            gemini: GeminiConfig {
                model: default_gemini_model(),
                api_key_env: default_gemini_api_key_env(),
                timeout_secs: default_timeout_secs(),
            },
            checks: ChecksConfig {
                test: Some("cargo test".to_string()),
                lint: Some(
                    "cargo clippy --all-targets --all-features -- -D warnings".to_string(),
                ),
                typecheck: Some("cargo check".to_string()),
            },
            policy: PolicyConfig {
                max_rounds: default_max_rounds(),
                require_both_approvals: true,
                allow_dirty_worktree: true,
            },
            prompts: PromptsConfig {
                implementation: default_implement_prompt(),
                review: default_review_prompt(),
                fix: default_fix_prompt(),
            },
        }
    }

    pub fn write_default(dir: &Path) -> Result<PathBuf> {
        let config = Self::default_config();
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

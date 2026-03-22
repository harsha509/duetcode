use crate::adapters::claude::ClaudeAdapter;
use crate::adapters::gemini::GeminiAdapter;
use crate::adapters::ImageInput;
use crate::config::Config;
use crate::git;
use crate::orchestrator;
use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use colored::Colorize;
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "dt",
    version,
    about = "AI pair programming — one model writes, another reviews, in a loop until approval"
)]
pub struct Cli {
    /// Coding task to implement (shorthand for `dt run`)
    pub task: Option<String>,

    /// Which model writes code: "claude" or "gemini"
    #[arg(long, default_value = "claude")]
    pub writer: String,

    /// Path(s) to screenshot/image files to include as context
    #[arg(long = "image")]
    pub images: Vec<PathBuf>,

    /// Show detailed output (commands being run, stderr, etc.)
    #[arg(long, short)]
    pub verbose: bool,

    /// Plan mode: plan first, you approve each step before execution
    #[arg(long)]
    pub plan: bool,

    /// Continue from the previous session's context
    #[arg(long, short = 'c')]
    pub continue_session: bool,

    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Initialize dt in the current repository
    Init,

    /// Check that all dependencies are available
    Doctor {
        /// Show detailed output and run ping test
        #[arg(long, short)]
        verbose: bool,
    },

    /// Run a coding task through the write/review loop
    Run {
        /// The coding task to implement
        task: String,

        /// Which model writes code: "claude" or "gemini"
        #[arg(long, default_value = "claude")]
        writer: String,

        /// Path(s) to screenshot/image files to include as context
        #[arg(long = "image")]
        images: Vec<PathBuf>,

        /// Continue from the previous session's context
        #[arg(long, short = 'c')]
        continue_session: bool,

        /// Show detailed output
        #[arg(long, short)]
        verbose: bool,
    },

    /// Plan a task interactively: plan → review → approve → execute → review
    Plan {
        /// The coding task to plan and execute
        task: String,

        /// Which model writes code: "claude" or "gemini"
        #[arg(long, default_value = "claude")]
        writer: String,

        /// Path(s) to screenshot/image files to include as context
        #[arg(long = "image")]
        images: Vec<PathBuf>,

        /// Continue from the previous session's context
        #[arg(long, short = 'c')]
        continue_session: bool,

        /// Show detailed output
        #[arg(long, short)]
        verbose: bool,
    },

    /// Review current uncommitted changes with the reviewer model
    Review {
        /// Which model reviews: "claude" or "gemini"
        #[arg(long, default_value = "gemini")]
        reviewer: String,

        /// Describe the task so the reviewer can verify changes against it
        #[arg(long, short)]
        task: Option<String>,

        /// Show detailed output
        #[arg(long, short)]
        verbose: bool,
    },

    /// Clear all past session logs
    Clear,
}

pub fn run() -> Result<()> {
    let cli = Cli::parse();
    let cwd = std::env::current_dir().context("failed to determine current directory")?;
    let verbose = cli.verbose;

    match cli.command {
        Some(Commands::Init) => cmd_init(&cwd),
        Some(Commands::Doctor { verbose: v }) => cmd_doctor(&cwd, verbose || v),
        Some(Commands::Run {
            task,
            writer,
            images,
            continue_session,
            verbose: v,
        }) => cmd_run(&cwd, &task, &writer, &images, continue_session, verbose || v),
        Some(Commands::Plan {
            task,
            writer,
            images,
            continue_session,
            verbose: v,
        }) => cmd_plan(&cwd, &task, &writer, &images, continue_session, verbose || v),
        Some(Commands::Review { reviewer, task, verbose: v }) => cmd_review(&cwd, &reviewer, task.as_deref(), verbose || v),
        Some(Commands::Clear) => cmd_clear(&cwd),

        None => {
            if let Some(task) = cli.task {
                if cli.plan {
                    cmd_plan(&cwd, &task, &cli.writer, &cli.images, cli.continue_session, verbose)
                } else {
                    cmd_run(&cwd, &task, &cli.writer, &cli.images, cli.continue_session, verbose)
                }
            } else {
                print_usage();
                Ok(())
            }
        }
    }
}

fn print_usage() {
    println!(
        "{}\n",
        "dt — AI pair programming CLI".cyan().bold()
    );
    println!("Usage:");
    println!("  {} \"add OAuth login\"              auto mode: Claude writes, Gemini reviews", "dt".green());
    println!("  {} plan \"add OAuth login\"          plan mode: you approve each step", "dt".green());
    println!("  {} \"task\" --plan                   same as dt plan", "dt".green());
    println!("  {} \"task\" --writer gemini           flip: Gemini writes, Claude reviews", "dt".green());
    println!("  {} \"task\" --image mock.png          include screenshots", "dt".green());
    println!("  {} init                            set up .duet/config.toml in current repo", "dt".green());
    println!("  {} doctor                          check dependencies", "dt".green());
    println!("  {} review                          review uncommitted changes", "dt".green());
    println!("  {} review --task \"add login\"       review changes against a specific task", "dt".green());
    println!("  {} clear                           clear all past session logs", "dt".green());
    println!("\nRun {} for all options.", "dt --help".cyan());
}

fn cmd_init(dir: &std::path::Path) -> Result<()> {
    println!("{}", "Initializing dt...".cyan().bold());

    if !git::is_git_repo(dir) {
        anyhow::bail!("not a git repository — run `git init` first");
    }

    let config_path = Config::config_path(dir);
    if config_path.exists() {
        println!("  {} .duet/config.toml already exists", "✓".green());
    } else {
        Config::write_default(dir)?;
        println!("  {} created .duet/config.toml", "✓".green());
    }

    let prompts_dir = dir.join(".duet").join("prompts");
    if !prompts_dir.exists() {
        std::fs::create_dir_all(&prompts_dir).context("failed to create .duet/prompts/")?;
    }

    write_default_prompt(&prompts_dir, "implement.txt", crate::prompts::DEFAULT_IMPLEMENT_TEMPLATE)?;
    write_default_prompt(&prompts_dir, "review.txt", crate::prompts::DEFAULT_REVIEW_TEMPLATE)?;
    write_default_prompt(&prompts_dir, "fix.txt", crate::prompts::DEFAULT_FIX_TEMPLATE)?;
    write_default_prompt(&prompts_dir, "plan.txt", crate::prompts::DEFAULT_PLAN_TEMPLATE)?;

    let gitignore_path = dir.join(".gitignore");
    let ignore_entry = "\n# duetcode sessions\n.duet/sessions/\n";

    if gitignore_path.exists() {
        let content = std::fs::read_to_string(&gitignore_path).unwrap_or_default();
        if !content.contains(".duet/sessions/") {
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .open(&gitignore_path)
                .context("failed to open .gitignore")?;
            file.write_all(ignore_entry.as_bytes())
                .context("failed to update .gitignore")?;
        }
    } else {
        std::fs::write(&gitignore_path, ignore_entry.trim_start())
            .context("failed to create .gitignore")?;
    }

    println!("\n{}", "dt initialized! Edit .duet/config.toml to customize.".green().bold());
    println!("Run {} to verify your setup.", "dt doctor".cyan());

    Ok(())
}

fn cmd_doctor(dir: &std::path::Path, verbose: bool) -> Result<()> {
    println!("{}\n", "dt doctor".cyan().bold());
    let mut all_ok = true;

    if git::is_git_repo(dir) {
        println!("  {} git repository", "✓".green());
    } else {
        println!("  {} git repository — not found", "✗".red());
        all_ok = false;
    }

    let config_path = Config::config_path(dir);
    let config = if config_path.exists() {
        println!("  {} .duet/config.toml", "✓".green());
        match Config::load(dir) {
            Ok(c) => {
                println!("  {} .duet/config.toml parses correctly", "✓".green());
                Some(c)
            }
            Err(e) => {
                println!("  {} .duet/config.toml parse error: {}", "✗".red(), e);
                all_ok = false;
                None
            }
        }
    } else {
        println!("  {} .duet/config.toml — not found (run `dt init`)", "✗".red());
        all_ok = false;
        None
    };

    let claude_config = config
        .as_ref()
        .map(|c| c.claude.clone())
        .unwrap_or_default();

    let claude = ClaudeAdapter::new(&claude_config, dir, verbose);

    let cli_ok = if claude.is_available() {
        println!("  {} claude CLI found", "✓".green());
        match claude.check_auth() {
            Ok(status) => {
                println!("  {} claude CLI authenticated ({})", "✓".green(), status);
                true
            }
            Err(e) => {
                println!("  {} claude CLI auth — {}", "~".yellow(), e);
                false
            }
        }
    } else {
        println!("  {} claude CLI — not found", "~".yellow());
        false
    };

    if claude.is_api_key_available() {
        println!("  {} {} is set (API fallback ready)", "✓".green(), claude_config.api_key_env);
    } else if cli_ok {
        println!("  {} {} not set (using CLI only)", "~".yellow(), claude_config.api_key_env);
    } else {
        println!(
            "  {} no claude access — install CLI or set {}",
            "✗".red(),
            claude_config.api_key_env
        );
        all_ok = false;
    }

    let mode = &claude_config.mode;
    println!("  {} claude mode: {} ({})", "ℹ".cyan(),
        mode,
        match mode.as_str() {
            "api" => "always use API",
            "cli" => "always use CLI",
            _ => if cli_ok { "using CLI, API as fallback" } else { "using API directly" },
        }
    );

    let gemini_config = config
        .as_ref()
        .map(|c| c.gemini.clone())
        .unwrap_or_default();

    if GeminiAdapter::is_key_available(&gemini_config) {
        println!("  {} {} is set", "✓".green(), gemini_config.api_key_env);
    } else {
        println!(
            "  {} {} — not set",
            "✗".red(),
            gemini_config.api_key_env
        );
        all_ok = false;
    }

    let prompts_dir = dir.join(".duet").join("prompts");
    for name in &["implement.txt", "review.txt", "fix.txt"] {
        let path = prompts_dir.join(name);
        if path.exists() {
            println!("  {} .duet/prompts/{}", "✓".green(), name);
        } else {
            println!(
                "  {} .duet/prompts/{} — not found (will use built-in defaults)",
                "~".yellow(),
                name
            );
        }
    }

    println!();
    if all_ok {
        println!("{}", "All checks passed! Ready to use dt.".green().bold());
    } else {
        println!(
            "{}",
            "Some checks failed. Fix the issues above and re-run `dt doctor`.".red().bold()
        );
    }

    Ok(())
}

/// Shared setup for cmd_run and cmd_plan: validates git repo, loads config, resolves adapters.
fn setup_task(
    dir: &std::path::Path,
    writer_name: &str,
    image_paths: &[PathBuf],
    verbose: bool,
) -> Result<(
    Config,
    Vec<ImageInput>,
    Box<dyn crate::adapters::ModelAdapter>,
    Box<dyn crate::adapters::ModelAdapter>,
    &'static str,
    &'static str,
)> {
    if !git::is_git_repo(dir) {
        anyhow::bail!("not a git repository — run `git init` first");
    }

    let config = Config::load(dir).context("failed to load config")?;

    let images: Vec<ImageInput> = image_paths
        .iter()
        .map(|p| ImageInput::load(p.clone()))
        .collect::<Result<Vec<_>>>()?;

    let (writer_label, reviewer_label): (&'static str, &'static str) = match writer_name.to_lowercase().as_str() {
        "claude" => ("claude", "gemini"),
        "gemini" => ("gemini", "claude"),
        other => anyhow::bail!("unknown writer '{}' — use 'claude' or 'gemini'", other),
    };

    let claude = ClaudeAdapter::new(&config.claude, dir, verbose);
    let gemini = GeminiAdapter::new(&config.gemini)?;

    let (writer, reviewer): (Box<dyn crate::adapters::ModelAdapter>, Box<dyn crate::adapters::ModelAdapter>) =
        if writer_label == "claude" {
            (Box::new(claude), Box::new(gemini))
        } else {
            (Box::new(gemini), Box::new(claude))
        };

    Ok((config, images, writer, reviewer, writer_label, reviewer_label))
}

fn cmd_run(
    dir: &std::path::Path,
    task: &str,
    writer_name: &str,
    image_paths: &[PathBuf],
    continue_session: bool,
    verbose: bool,
) -> Result<()> {
    let (config, images, writer, reviewer, writer_label, reviewer_label) =
        setup_task(dir, writer_name, image_paths, verbose)?;

    if !config.policy.allow_dirty_worktree && !git::is_worktree_clean(dir)? {
        anyhow::bail!(
            "worktree has uncommitted changes — commit or stash them first \
             (or set allow_dirty_worktree = true in duet.toml)"
        );
    }

    let result = orchestrator::run(&config, task, writer.as_ref(), reviewer.as_ref(), &images, dir, continue_session, verbose)?;

    println!(
        "\n{} rounds={}, writer={}, reviewer={}",
        if result.success { "SUCCESS".green().bold() } else { "FAILED".red().bold() },
        result.rounds,
        writer_label,
        reviewer_label,
    );
    println!("{}\n", result.message);

    if result.success { Ok(()) } else { std::process::exit(1); }
}

fn cmd_plan(
    dir: &std::path::Path,
    task: &str,
    writer_name: &str,
    image_paths: &[PathBuf],
    continue_session: bool,
    verbose: bool,
) -> Result<()> {
    let (config, images, writer, reviewer, writer_label, reviewer_label) =
        setup_task(dir, writer_name, image_paths, verbose)?;

    let result = orchestrator::run_plan_flow(&config, task, writer.as_ref(), reviewer.as_ref(), &images, dir, continue_session, verbose)?;

    println!(
        "\n{} rounds={}, writer={}, reviewer={}",
        if result.success { "SUCCESS".green().bold() } else { "STOPPED".yellow().bold() },
        result.rounds,
        writer_label,
        reviewer_label,
    );
    println!("{}\n", result.message);

    if result.success { Ok(()) } else { std::process::exit(1); }
}

fn cmd_review(dir: &std::path::Path, reviewer_name: &str, task: Option<&str>, verbose: bool) -> Result<()> {
    if !git::is_git_repo(dir) {
        anyhow::bail!("not a git repository");
    }

    let config = Config::load(dir).context("failed to load config")?;

    let reviewer: Box<dyn crate::adapters::ModelAdapter> = match reviewer_name.to_lowercase().as_str() {
        "gemini" => Box::new(GeminiAdapter::new(&config.gemini)?),
        "claude" => Box::new(ClaudeAdapter::new(&config.claude, dir, verbose)),
        other => anyhow::bail!("unknown reviewer '{}' — use 'claude' or 'gemini'", other),
    };

    let result = orchestrator::review_only(&config, reviewer.as_ref(), dir, task, verbose)?;

    if result.success {
        println!("\n{}", "Final Result: APPROVED".green().bold());
    } else {
        println!("\n{}", "Final Result: CHANGES NEEDED".red().bold());
        std::process::exit(1);
    }

    Ok(())
}

fn cmd_clear(dir: &std::path::Path) -> Result<()> {
    let sessions_dir = dir.join(".duet").join("sessions");

    if sessions_dir.exists() {
        std::fs::remove_dir_all(&sessions_dir)
            .with_context(|| format!("failed to remove {}", sessions_dir.display()))?;
        println!("{} Cleared all past sessions.", "✓".green());
    } else {
        println!("{} No sessions found to clear.", "ℹ".cyan());
    }

    Ok(())
}

fn write_default_prompt(dir: &std::path::Path, name: &str, content: &str) -> Result<()> {
    let path = dir.join(name);
    if path.exists() {
        println!("  {} prompts/{} already exists", "✓".green(), name);
    } else {
        std::fs::write(&path, content)
            .with_context(|| format!("failed to write {}", path.display()))?;
        println!("  {} created prompts/{}", "✓".green(), name);
    }
    Ok(())
}

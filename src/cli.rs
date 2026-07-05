use crate::adapters::claude::ClaudeAdapter;
use crate::adapters::gemini::GeminiAdapter;
use crate::adapters::{ImageInput, ModelAdapter};
use crate::config::Config;
use crate::git;
use crate::orchestrator::{self, TaskOptions};
use crate::repl;
use crate::ui;
use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use colored::Colorize;
use std::path::{Path, PathBuf};

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

    /// Auto mode: loop write→review without prompts until both models approve
    #[arg(long, short = 'a')]
    pub auto: bool,

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

        /// Auto mode: loop write→review without prompts until both models approve
        #[arg(long, short = 'a')]
        auto: bool,

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

        /// Auto mode for the execution phase after the plan is approved
        #[arg(long, short = 'a')]
        auto: bool,

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

/// Everything cmd_task needs, collected from whichever CLI form was used.
struct TaskArgs {
    task: String,
    writer: String,
    images: Vec<PathBuf>,
    continue_session: bool,
    verbose: bool,
    auto: bool,
    plan_first: bool,
}

pub fn run() -> Result<()> {
    let cli = Cli::parse();
    let cwd = std::env::current_dir().context("failed to determine current directory")?;

    match cli.command {
        Some(Commands::Init) => cmd_init(&cwd),
        Some(Commands::Doctor { verbose: v }) => cmd_doctor(&cwd, cli.verbose || v),
        Some(Commands::Run { task, writer, images, auto, continue_session, verbose: v }) => {
            cmd_task(&cwd, TaskArgs {
                task,
                writer,
                images,
                continue_session,
                verbose: cli.verbose || v,
                auto: cli.auto || auto,
                plan_first: false,
            })
        }
        Some(Commands::Plan { task, writer, images, auto, continue_session, verbose: v }) => {
            cmd_task(&cwd, TaskArgs {
                task,
                writer,
                images,
                continue_session,
                verbose: cli.verbose || v,
                auto: cli.auto || auto,
                plan_first: true,
            })
        }
        Some(Commands::Review { reviewer, task, verbose: v }) => {
            cmd_review(&cwd, &reviewer, task.as_deref(), cli.verbose || v)
        }
        Some(Commands::Clear) => cmd_clear(&cwd),

        None => match cli.task {
            Some(task) => cmd_task(&cwd, TaskArgs {
                task,
                writer: cli.writer,
                images: cli.images,
                continue_session: cli.continue_session,
                verbose: cli.verbose,
                auto: cli.auto,
                plan_first: cli.plan,
            }),
            None => {
                if git::is_git_repo(&cwd) && Config::config_path(&cwd).exists() {
                    cmd_session(&cwd, &cli.writer, cli.verbose, cli.auto)
                } else {
                    print_usage();
                    Ok(())
                }
            }
        },
    }
}

/// Bare `dt` in an initialized repo: interactive session where both models
/// keep their context across tasks.
fn cmd_session(dir: &Path, writer_name: &str, verbose: bool, auto: bool) -> Result<()> {
    let TaskSetup { config, images: _, mut writer, mut reviewer } =
        setup_task(dir, writer_name, &[], verbose)?;

    let auto = auto || config.policy.auto;
    repl::run(dir, &config, writer.as_mut(), reviewer.as_mut(), verbose, auto)
}

fn print_usage() {
    println!(
        "{}\n",
        "dt — AI pair programming CLI".cyan().bold()
    );
    println!("Usage:");
    println!("  {}                                 start an interactive session (in an initialized repo)", "dt".green());
    println!("  {} \"add OAuth login\"              interactive: Claude writes, Gemini reviews", "dt".green());
    println!("  {} \"add OAuth login\" --auto        auto: loop until both models approve", "dt".green());
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

struct TaskSetup {
    config: Config,
    images: Vec<ImageInput>,
    writer: Box<dyn ModelAdapter>,
    reviewer: Box<dyn ModelAdapter>,
}

/// Validates the repo, loads config, and resolves writer/reviewer adapters.
fn setup_task(
    dir: &Path,
    writer_name: &str,
    image_paths: &[PathBuf],
    verbose: bool,
) -> Result<TaskSetup> {
    if !git::is_git_repo(dir) {
        anyhow::bail!("not a git repository — run `git init` first");
    }

    let config = Config::load(dir).context("failed to load config")?;

    let images: Vec<ImageInput> = image_paths
        .iter()
        .map(|p| ImageInput::load(p.clone()))
        .collect::<Result<Vec<_>>>()?;

    let claude: Box<dyn ModelAdapter> = Box::new(ClaudeAdapter::new(&config.claude, dir, verbose));
    let gemini: Box<dyn ModelAdapter> = Box::new(GeminiAdapter::new(&config.gemini)?);

    let (writer, reviewer) = match writer_name.to_lowercase().as_str() {
        "claude" => (claude, gemini),
        "gemini" => (gemini, claude),
        other => anyhow::bail!("unknown writer '{}' — use 'claude' or 'gemini'", other),
    };

    Ok(TaskSetup { config, images, writer, reviewer })
}

fn cmd_task(dir: &Path, args: TaskArgs) -> Result<()> {
    let TaskSetup { config, images, mut writer, mut reviewer } =
        setup_task(dir, &args.writer, &args.images, args.verbose)?;

    if !config.policy.allow_dirty_worktree && !git::is_worktree_clean(dir)? {
        anyhow::bail!(
            "worktree has uncommitted changes — commit or stash them first \
             (or set allow_dirty_worktree = true in .duet/config.toml)"
        );
    }

    let opts = TaskOptions {
        config: &config,
        task: &args.task,
        images: &images,
        repo_dir: dir,
        continue_session: args.continue_session,
        verbose: args.verbose,
        auto: args.auto || config.policy.auto,
        plan_first: args.plan_first,
    };

    let result = orchestrator::run(&opts, writer.as_mut(), reviewer.as_mut())?;

    ui::final_line(result.success, result.rounds, writer.name(), reviewer.name(), &result.message);

    if result.success { Ok(()) } else { std::process::exit(1); }
}

fn cmd_review(dir: &Path, reviewer_name: &str, task: Option<&str>, verbose: bool) -> Result<()> {
    if !git::is_git_repo(dir) {
        anyhow::bail!("not a git repository");
    }

    let config = Config::load(dir).context("failed to load config")?;

    let mut reviewer: Box<dyn ModelAdapter> = match reviewer_name.to_lowercase().as_str() {
        "gemini" => Box::new(GeminiAdapter::new(&config.gemini)?),
        "claude" => Box::new(ClaudeAdapter::new(&config.claude, dir, verbose)),
        other => anyhow::bail!("unknown reviewer '{}' — use 'claude' or 'gemini'", other),
    };

    let result = orchestrator::review_only(&config, reviewer.as_mut(), dir, task, verbose)?;

    if result.success {
        println!("\n{}", "Final Result: APPROVED".green().bold());
    } else {
        println!("\n{}", "Final Result: CHANGES NEEDED".red().bold());
        std::process::exit(1);
    }

    Ok(())
}

fn cmd_init(dir: &Path) -> Result<()> {
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

fn cmd_doctor(dir: &Path, verbose: bool) -> Result<()> {
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

fn cmd_clear(dir: &Path) -> Result<()> {
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

fn write_default_prompt(dir: &Path, name: &str, content: &str) -> Result<()> {
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

mod adapters;
mod checks;
mod cli;
mod config;
mod events;
mod git;
mod logs;
mod orchestrator;
mod policy;
mod prompts;
mod repl;
mod serve;
mod ui;

fn main() {
    if let Err(e) = cli::run() {
        eprintln!("\n{}: {:#}", colored::Colorize::red("error"), e);
        std::process::exit(1);
    }
}

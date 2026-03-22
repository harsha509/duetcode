mod adapters;
mod checks;
mod cli;
mod config;
mod git;
mod logs;
mod orchestrator;
mod policy;
mod prompts;

fn main() {
    if let Err(e) = cli::run() {
        eprintln!("\n{}: {:#}", colored::Colorize::red("error"), e);
        std::process::exit(1);
    }
}

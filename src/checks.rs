use crate::config::ChecksConfig;
use serde::Serialize;
use std::path::Path;
use std::process::Command;

#[derive(Debug, Clone, Serialize)]
pub struct CheckResult {
    pub name: String,
    pub passed: bool,
    pub output: String,
    pub exit_code: Option<i32>,
}

pub fn run_checks(config: &ChecksConfig, dir: &Path) -> Vec<CheckResult> {
    let mut results = Vec::new();

    let checks: Vec<(&str, &Option<String>)> = vec![
        ("typecheck", &config.typecheck),
        ("lint", &config.lint),
        ("test", &config.test),
    ];

    for (name, cmd_opt) in checks {
        if let Some(cmd) = cmd_opt {
            results.push(run_single_check(name, cmd, dir));
        }
    }

    results
}

fn run_single_check(name: &str, cmd: &str, dir: &Path) -> CheckResult {
    let parts: Vec<&str> = cmd.split_whitespace().collect();
    if parts.is_empty() {
        return CheckResult {
            name: name.to_string(),
            passed: false,
            output: "empty command".to_string(),
            exit_code: None,
        };
    }

    let result = Command::new(parts[0])
        .args(&parts[1..])
        .current_dir(dir)
        .output();

    match result {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            let combined = format!("{}{}", stdout, stderr);

            CheckResult {
                name: name.to_string(),
                passed: output.status.success(),
                output: combined.trim().to_string(),
                exit_code: output.status.code(),
            }
        }
        Err(e) => CheckResult {
            name: name.to_string(),
            passed: false,
            output: format!("failed to run '{}': {}", cmd, e),
            exit_code: None,
        },
    }
}

pub fn format_check_results(results: &[CheckResult]) -> String {
    if results.is_empty() {
        return "No checks configured or run.".to_string();
    }

    results
        .iter()
        .map(|r| {
            let status = if r.passed { "PASSED" } else { "FAILED" };
            let output = if r.output.trim().is_empty() {
                "(no output)".to_string()
            } else if r.output.len() > 1000 {
                let truncated: String = r.output.chars().take(1000).collect();
                format!("{}... (truncated)", truncated)
            } else {
                r.output.clone()
            };

            format!("CHECK: {}\nSTATUS: {}\nOUTPUT:\n```\n{}\n```", r.name, status, output)
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

pub fn all_passed(results: &[CheckResult]) -> bool {
    results.iter().all(|r| r.passed)
}

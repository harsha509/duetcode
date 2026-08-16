use crate::config::ChecksConfig;
use crate::process;
use serde::Serialize;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

#[derive(Debug, Clone, Serialize)]
pub struct CheckResult {
    pub name: String,
    pub passed: bool,
    pub output: String,
    pub exit_code: Option<i32>,
}

pub fn run_checks(config: &ChecksConfig, dir: &Path) -> Vec<CheckResult> {
    config
        .defined()
        .into_iter()
        .map(|(name, cmd)| run_single_check(name, cmd, dir, Duration::from_secs(config.timeout_secs)))
        .collect()
}

fn run_single_check(name: &str, cmd: &str, dir: &Path, timeout: Duration) -> CheckResult {
    let parts: Vec<&str> = cmd.split_whitespace().collect();
    if parts.is_empty() {
        return CheckResult {
            name: name.to_string(),
            passed: false,
            output: "empty command".to_string(),
            exit_code: None,
        };
    }

    let mut command = Command::new(parts[0]);
    command
        .args(&parts[1..])
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Own process group, so a timeout kill reaches the check's grandchildren
    // and a survivor cannot hold the drain joins open after the check exits.
    let mut child = match process::spawn_grouped(&mut command) {
        Ok(child) => child,
        Err(e) => {
            return CheckResult {
                name: name.to_string(),
                passed: false,
                output: format!("failed to run '{}': {}", cmd, e),
                exit_code: None,
            };
        }
    };

    // Drained while the check runs: a check with a full pipe blocks writing
    // and never exits, which reads as a timeout it did not earn.
    let stdout = child.stdout.take().map(process::drain_read);
    let stderr = child.stderr.take().map(process::drain_read);

    let status = match process::wait_or_kill(&mut child, timeout) {
        Ok(Some(status)) => status,
        Ok(None) => {
            // The kill closed the pipes, so the joins return promptly — and
            // what the check printed before hanging names the test that hung.
            let stdout = stdout.and_then(|h| h.join().ok()).unwrap_or_default();
            let stderr = stderr.and_then(|h| h.join().ok()).unwrap_or_default();
            let captured = format!("{}{}", stdout, stderr);
            let mut output =
                format!("timed out after {}s — raise timeout_secs in [checks]", timeout.as_secs());
            if !captured.trim().is_empty() {
                output = format!("{}\n{}", output, captured.trim());
            }
            return CheckResult { name: name.to_string(), passed: false, output, exit_code: None };
        }
        Err(e) => {
            return CheckResult {
                name: name.to_string(),
                passed: false,
                output: format!("failed to wait for '{}': {}", cmd, e),
                exit_code: None,
            };
        }
    };

    let stdout = stdout.and_then(|h| h.join().ok()).unwrap_or_default();
    let stderr = stderr.and_then(|h| h.join().ok()).unwrap_or_default();
    let combined = format!("{}{}", stdout, stderr);

    CheckResult {
        name: name.to_string(),
        passed: status.success(),
        output: combined.trim().to_string(),
        exit_code: status.code(),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_passing_check_passes() {
        let result = run_single_check("test", "true", Path::new("."), Duration::from_secs(30));
        assert!(result.passed);
        assert_eq!(result.exit_code, Some(0));
    }

    #[test]
    fn a_failing_check_reports_its_exit_code() {
        let result = run_single_check("test", "false", Path::new("."), Duration::from_secs(30));
        assert!(!result.passed);
        assert_eq!(result.exit_code, Some(1));
    }

    #[test]
    fn a_missing_binary_is_reported_not_panicked() {
        let result = run_single_check(
            "test",
            "definitely-not-a-real-check-binary",
            Path::new("."),
            Duration::from_secs(30),
        );
        assert!(!result.passed);
        assert!(result.output.contains("failed to run"), "got: {}", result.output);
        assert_eq!(result.exit_code, None);
    }

    /// A hung test suite must fail the round, not hang the loop: the timeout
    /// is what this whole rework exists for.
    #[test]
    fn a_check_that_hangs_is_cut_off_by_its_timeout() {
        let start = std::time::Instant::now();
        let result = run_single_check("test", "sleep 5", Path::new("."), Duration::from_millis(300));
        assert!(start.elapsed() < Duration::from_secs(4));
        assert!(!result.passed);
        assert!(result.output.contains("timed out"), "got: {}", result.output);
        assert_eq!(result.exit_code, None);
    }
}

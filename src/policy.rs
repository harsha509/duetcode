use crate::checks::CheckResult;
use crate::config::PolicyConfig;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Approved,
    ChangesRequested,
}

#[derive(Debug, Clone)]
pub struct ReviewVerdict {
    pub verdict: Verdict,
    pub blockers: Vec<String>,
    pub suggestions: Vec<String>,
    #[allow(dead_code)]
    pub tests_to_add: Vec<String>,
    pub raw: String,
}

#[derive(Debug)]
pub enum PolicyResult {
    Pass,
    Continue(String),
    Fail(String),
}

pub fn evaluate(
    verdict: &ReviewVerdict,
    checks: &[CheckResult],
    round: usize,
    config: &PolicyConfig,
) -> PolicyResult {
    let checks_pass = checks.iter().all(|c| c.passed);

    if verdict.verdict == Verdict::Approved && checks_pass {
        return PolicyResult::Pass;
    }

    if round >= config.max_rounds {
        let reason = if !checks_pass {
            let failed: Vec<_> = checks
                .iter()
                .filter(|c| !c.passed)
                .map(|c| c.name.as_str())
                .collect();
            format!(
                "max rounds ({}) reached — checks still failing: {}",
                config.max_rounds,
                failed.join(", ")
            )
        } else {
            format!(
                "max rounds ({}) reached — reviewer has not approved",
                config.max_rounds
            )
        };
        return PolicyResult::Fail(reason);
    }

    if verdict.verdict == Verdict::ChangesRequested {
        let reason = format_review_feedback(verdict);
        return PolicyResult::Continue(reason);
    }

    if !checks_pass {
        let failed: Vec<_> = checks
            .iter()
            .filter(|c| !c.passed)
            .map(|c| c.name.as_str())
            .collect();
        return PolicyResult::Continue(format!(
            "reviewer approved but checks failing: {}",
            failed.join(", ")
        ));
    }

    PolicyResult::Pass
}

pub fn parse_verdict(raw: &str) -> ReviewVerdict {
    let lines: Vec<&str> = raw.lines().collect();

    let verdict = parse_verdict_line(&lines);
    let blockers = parse_list_section(&lines, "BLOCKERS:");
    let suggestions = parse_list_section(&lines, "SUGGESTIONS:");
    let tests_to_add = parse_list_section(&lines, "TESTS_TO_ADD:");

    ReviewVerdict {
        verdict,
        blockers,
        suggestions,
        tests_to_add,
        raw: raw.to_string(),
    }
}

fn parse_verdict_line(lines: &[&str]) -> Verdict {
    for line in lines.iter().rev() {
        let upper = line.to_uppercase();
        if upper.contains("VERDICT:") || upper.contains("VERDICT :") {
            if upper.contains("APPROVED") && !upper.contains("CHANGES_REQUESTED") {
                return Verdict::Approved;
            }
            if upper.contains("CHANGES_REQUESTED") || upper.contains("CHANGES REQUESTED") {
                return Verdict::ChangesRequested;
            }
        }
    }

    for line in lines.iter().rev().take(5) {
        let upper = line.to_uppercase();
        if upper.contains("APPROVED") && !upper.contains("NOT APPROVED") {
            return Verdict::Approved;
        }
    }

    Verdict::ChangesRequested
}

fn parse_list_section(lines: &[&str], header: &str) -> Vec<String> {
    let header_upper = header.to_uppercase();
    let mut items = Vec::new();
    let mut in_section = false;

    for line in lines {
        let upper = line.to_uppercase();

        if upper.contains(&header_upper) {
            in_section = true;
            continue;
        }

        if in_section {
            if line.trim().is_empty() {
                continue;
            }
            if line.starts_with("- ") || line.starts_with("* ") {
                let item = line.trim_start_matches("- ").trim_start_matches("* ").trim();
                if !item.is_empty() && item.to_lowercase() != "none" {
                    items.push(item.to_string());
                }
            } else if upper.contains(':')
                && (upper.contains("BLOCKERS")
                    || upper.contains("SUGGESTIONS")
                    || upper.contains("TESTS")
                    || upper.contains("VERDICT"))
            {
                break;
            }
        }
    }

    items
}

pub fn format_review_feedback(verdict: &ReviewVerdict) -> String {
    verdict.raw.clone()
}

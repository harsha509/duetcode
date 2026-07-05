use std::collections::HashSet;

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
}

pub fn parse_verdict(raw: &str) -> ReviewVerdict {
    let lines: Vec<&str> = raw.lines().collect();

    ReviewVerdict {
        verdict: parse_verdict_line(&lines),
        blockers: parse_list_section(&lines, "BLOCKERS:"),
        suggestions: parse_list_section(&lines, "SUGGESTIONS:"),
    }
}

/// Word-level Jaccard similarity between two blocker lists. Used to detect
/// a stalled loop: the reviewer keeps raising essentially the same issues.
pub fn blockers_similar(a: &[String], b: &[String]) -> bool {
    if a.is_empty() && b.is_empty() {
        return true;
    }
    if a.is_empty() || b.is_empty() {
        return false;
    }

    let wa = word_set(a);
    let wb = word_set(b);
    let intersection = wa.intersection(&wb).count();
    let union = wa.union(&wb).count();

    union > 0 && (intersection as f64 / union as f64) >= 0.5
}

fn word_set(items: &[String]) -> HashSet<String> {
    items
        .iter()
        .flat_map(|s| {
            s.to_lowercase()
                .chars()
                .map(|c| if c.is_alphanumeric() { c } else { ' ' })
                .collect::<String>()
                .split_whitespace()
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .collect()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_explicit_approved_verdict() {
        let v = parse_verdict("Looks good.\n\nVERDICT: APPROVED");
        assert_eq!(v.verdict, Verdict::Approved);
        assert!(v.blockers.is_empty());
    }

    #[test]
    fn parses_changes_requested_with_blockers() {
        let review = "BLOCKERS:\n- missing null check in foo()\n- test coverage absent\n\nVERDICT: CHANGES_REQUESTED";
        let v = parse_verdict(review);
        assert_eq!(v.verdict, Verdict::ChangesRequested);
        assert_eq!(v.blockers.len(), 2);
    }

    #[test]
    fn defaults_to_changes_requested_when_no_verdict() {
        let v = parse_verdict("I am not sure about this diff.");
        assert_eq!(v.verdict, Verdict::ChangesRequested);
    }

    #[test]
    fn similar_blockers_detected_despite_rewording() {
        let a = vec!["missing null check in foo() function".to_string()];
        let b = vec!["the foo() function is missing a null check".to_string()];
        assert!(blockers_similar(&a, &b));
    }

    #[test]
    fn different_blockers_not_similar() {
        let a = vec!["missing null check in foo()".to_string()];
        let b = vec!["SQL injection in the login handler".to_string()];
        assert!(!blockers_similar(&a, &b));
    }

    #[test]
    fn empty_vs_nonempty_not_similar() {
        let a: Vec<String> = vec![];
        let b = vec!["anything".to_string()];
        assert!(!blockers_similar(&a, &b));
        assert!(blockers_similar(&a, &[]));
    }
}

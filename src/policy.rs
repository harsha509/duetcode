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

/// This gate fails closed: anything that is not unambiguously an approval is a
/// rejection, because a false Approved ends the loop and reports a rejected
/// change to the user as a success.
fn parse_verdict_line(lines: &[&str]) -> Verdict {
    for line in lines.iter().rev() {
        let upper = line.to_uppercase();
        if upper.contains("VERDICT:") || upper.contains("VERDICT :") {
            // Rejections are tested first: "NOT APPROVED" also contains "APPROVED".
            if upper.contains("CHANGES_REQUESTED")
                || upper.contains("CHANGES REQUESTED")
                || upper.contains("NOT APPROVED")
            {
                return Verdict::ChangesRequested;
            }
            if upper.contains("APPROVED") {
                return Verdict::Approved;
            }
        }
    }

    // No verdict line. Only a line that is *nothing but* the word counts —
    // prose such as "this cannot be approved until…" merely contains it.
    if lines.iter().rev().take(5).any(|line| is_bare_approval(line)) {
        return Verdict::Approved;
    }

    Verdict::ChangesRequested
}

/// True for a line whose entire content is the word "approved", ignoring
/// markdown emphasis and punctuation — `APPROVED`, `**Approved**`, `Approved.`
fn is_bare_approval(line: &str) -> bool {
    let words: String = line
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect();
    words.trim().eq_ignore_ascii_case("approved")
}

/// Longest conclusion kept, in characters. It is displayed on one terminal
/// line, and a reviewer that ignores "in one line" must not flood the screen.
const MAX_CONCLUSION_CHARS: usize = 200;

/// The reviewer's one-line restatement of what the reviewed *answer* concluded,
/// from its `THEIR CONCLUSION:` line.
///
/// In answer mode the writer's output is often itself a verdict on someone
/// else's code, so the reviewer's APPROVED and the answer's own conclusion can
/// point opposite ways: an approved "do not merge this" is still "do not merge
/// this". Callers show both, so the approval cannot be skimmed as an
/// endorsement of the thing the answer is about.
pub fn parse_answer_conclusion(raw: &str) -> Option<String> {
    let text = raw
        .lines()
        .rev()
        .find_map(|line| strip_label(line, "THEIR CONCLUSION:"))
        .filter(|s| !s.is_empty())?;

    if text.chars().count() <= MAX_CONCLUSION_CHARS {
        return Some(text.to_string());
    }
    Some(format!("{}…", text.chars().take(MAX_CONCLUSION_CHARS).collect::<String>()))
}

/// Matches an `ALL CAPS:` label at the start of a line and returns the rest,
/// tolerating the markdown a reviewer wraps it in — `**THEIR CONCLUSION:** x`,
/// `- THEIR CONCLUSION: x`, `> Their conclusion: x`.
fn strip_label<'a>(line: &'a str, label: &str) -> Option<&'a str> {
    debug_assert!(label.is_ascii(), "label is compared with eq_ignore_ascii_case");

    let trimmed = line.trim_matches(|c: char| c.is_whitespace() || matches!(c, '*' | '#' | '>' | '-'));
    if !trimmed.is_char_boundary(label.len()) {
        return None;
    }

    let (head, rest) = trimmed.split_at(label.len());
    head.eq_ignore_ascii_case(label)
        .then(|| rest.trim_matches(|c: char| c.is_whitespace() || c == '*'))
}

/// Strips one markdown list marker — `-`, `*`, `+`, `1.` or `1)` — returning
/// the item text. Reviewers pick their own bullet style, and a blocker missed
/// here is a blocker the stall detector and the escalation summary never see.
fn strip_list_marker(line: &str) -> Option<&str> {
    for marker in ["- ", "* ", "+ "] {
        if let Some(rest) = line.strip_prefix(marker) {
            return Some(rest.trim());
        }
    }

    let digits = line.chars().take_while(char::is_ascii_digit).count();
    if digits > 0 {
        let rest = &line[digits..];
        for marker in [". ", ") "] {
            if let Some(item) = rest.strip_prefix(marker) {
                return Some(item.trim());
            }
        }
    }

    None
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
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            if let Some(item) = strip_list_marker(trimmed) {
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

    /// A reviewer that discusses approval in prose without emitting a verdict
    /// line must never be read as approving. Each of these ends the loop and
    /// reports success to the user if the fallback matches on the word alone.
    #[test]
    fn prose_mentioning_approval_is_not_an_approval() {
        for review in [
            "This cannot be approved until the null check is added.",
            "I would have approved this, but the tests are missing.",
            "This is not yet approved.",
            "Changes are needed before this can be approved.",
        ] {
            assert_eq!(
                parse_verdict(review).verdict,
                Verdict::ChangesRequested,
                "wrongly approved: {review}"
            );
        }
    }

    #[test]
    fn explicit_not_approved_verdict_is_changes_requested() {
        let v = parse_verdict("VERDICT: NOT APPROVED");
        assert_eq!(v.verdict, Verdict::ChangesRequested);
    }

    /// A bare approval with no `VERDICT:` line is still an approval.
    #[test]
    fn bare_approval_line_still_approves() {
        for review in ["Looks good.\n\nAPPROVED", "Fine by me.\n\n**APPROVED**", "Ship it.\nApproved."] {
            assert_eq!(parse_verdict(review).verdict, Verdict::Approved, "missed: {review}");
        }
    }

    /// The approval covers the answer; this line covers what the answer said.
    /// Losing it turns an approved "request changes" into a bare SUCCESS.
    #[test]
    fn answer_conclusion_is_read_through_markdown() {
        for review in [
            "THEIR CONCLUSION: Request changes — 3 blocking issues\nVERDICT: APPROVED",
            "**THEIR CONCLUSION:** Request changes — 3 blocking issues\nVERDICT: APPROVED",
            "- Their conclusion: Request changes — 3 blocking issues\nVERDICT: APPROVED",
        ] {
            assert_eq!(
                parse_answer_conclusion(review).as_deref(),
                Some("Request changes — 3 blocking issues"),
                "missed: {review}"
            );
        }
    }

    #[test]
    fn missing_or_empty_conclusion_is_none() {
        assert_eq!(parse_answer_conclusion("Looks fine.\n\nVERDICT: APPROVED"), None);
        assert_eq!(parse_answer_conclusion("THEIR CONCLUSION:\nVERDICT: APPROVED"), None);
    }

    /// A reviewer that ignores "in one line" must not flood the closing line.
    #[test]
    fn long_conclusion_is_truncated_on_a_char_boundary() {
        let review = format!("THEIR CONCLUSION: {}\nVERDICT: APPROVED", "é".repeat(400));
        let conclusion = parse_answer_conclusion(&review).expect("conclusion");
        assert_eq!(conclusion.chars().count(), MAX_CONCLUSION_CHARS + 1);
        assert!(conclusion.ends_with('…'));
    }

    /// Reviewers indent bullets under the header or use numbered lists. Dropping
    /// those leaves `blockers` empty, which silently disables stall detection.
    #[test]
    fn blockers_survive_indentation_and_numbering() {
        let review = "BLOCKERS:\n  - indented blocker\n1. numbered blocker\n- plain blocker\n\nVERDICT: CHANGES_REQUESTED";
        let v = parse_verdict(review);
        assert_eq!(
            v.blockers,
            vec!["indented blocker", "numbered blocker", "plain blocker"]
        );
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

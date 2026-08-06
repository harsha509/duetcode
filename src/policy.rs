use serde::Serialize;
use std::collections::HashSet;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Approved,
    ChangesRequested,
}

/// Which pair of words a verdict is reported in.
///
/// A code review approves a change or requests changes to it. An answer review
/// judges an *answer*, and that answer is frequently itself a verdict on
/// somebody else's code — so the two were reported in the same two words, and
/// an approved "do not merge this" read as an approval of the merge. They no
/// longer share a vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VerdictKind {
    Code,
    Answer,
}

impl VerdictKind {
    pub fn label(self, approved: bool) -> &'static str {
        match (self, approved) {
            (VerdictKind::Code, true) => "APPROVED",
            (VerdictKind::Code, false) => "CHANGES REQUESTED",
            (VerdictKind::Answer, true) => "SOUND",
            (VerdictKind::Answer, false) => "UNSOUND",
        }
    }
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
            // Rejections are tested first: "NOT APPROVED" also contains
            // "APPROVED", and "UNSOUND" contains "SOUND".
            if upper.contains("CHANGES_REQUESTED")
                || upper.contains("CHANGES REQUESTED")
                || upper.contains("NOT APPROVED")
                || upper.contains("UNSOUND")
                || upper.contains("NOT SOUND")
            {
                return Verdict::ChangesRequested;
            }
            // Both vocabularies are read: answer reviews say SOUND, code reviews
            // say APPROVED, and a custom prompt template predating the split
            // still says APPROVED for either.
            if upper.contains("APPROVED") || upper.contains("SOUND") {
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

/// True for a line whose entire content is the verdict word, ignoring markdown
/// emphasis and punctuation — `APPROVED`, `**Approved**`, `Sound.`
///
/// `unsound` deliberately fails this and falls through to the rejection, the
/// same way `not approved` does.
fn is_bare_approval(line: &str) -> bool {
    let words: String = line
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect();
    matches!(words.trim().to_lowercase().as_str(), "approved" | "sound")
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

/// True for the placeholder a reviewer writes when a section is empty. The
/// prompt asks for a bare `none`, but reviewers dress it up — `` `none` ``,
/// **None**, "None." — and a decorated placeholder read as a real item is a
/// blocker that never existed, which fails the run and feeds the stall
/// detector a finding no one can fix.
fn is_nothing_to_report(item: &str) -> bool {
    let bare: String = item
        .chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace())
        .collect();
    matches!(bare.trim().to_lowercase().as_str(), "none" | "no blockers" | "no suggestions")
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
                if !item.is_empty() && !is_nothing_to_report(item) {
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

    /// Answer reviews report in their own words, so both vocabularies have to
    /// parse. `UNSOUND` contains `SOUND`, exactly as `NOT APPROVED` contains
    /// `APPROVED` — reading it as an approval would end the run reporting an
    /// answer the reviewer had just rejected.
    #[test]
    fn the_answer_vocabulary_parses_and_unsound_is_not_sound() {
        assert_eq!(parse_verdict("VERDICT: SOUND").verdict, Verdict::Approved);
        assert_eq!(parse_verdict("VERDICT: UNSOUND").verdict, Verdict::ChangesRequested);
        assert_eq!(parse_verdict("VERDICT: NOT SOUND").verdict, Verdict::ChangesRequested);
    }

    /// A prompt template written before the split still says APPROVED for an
    /// answer, and must keep working.
    #[test]
    fn the_code_vocabulary_still_parses_after_the_split() {
        assert_eq!(parse_verdict("VERDICT: APPROVED").verdict, Verdict::Approved);
        assert_eq!(parse_verdict("VERDICT: CHANGES_REQUESTED").verdict, Verdict::ChangesRequested);
    }

    /// The two verdicts must never read as each other; that collision is the
    /// whole reason for the split.
    #[test]
    fn each_kind_reports_in_its_own_words() {
        assert_eq!(VerdictKind::Code.label(true), "APPROVED");
        assert_eq!(VerdictKind::Code.label(false), "CHANGES REQUESTED");
        assert_eq!(VerdictKind::Answer.label(true), "SOUND");
        assert_eq!(VerdictKind::Answer.label(false), "UNSOUND");
    }

    /// A bare `SOUND` with no `VERDICT:` prefix is still an approval, and a bare
    /// `UNSOUND` still is not.
    #[test]
    fn a_bare_soundness_line_is_read_the_same_way() {
        assert_eq!(parse_verdict("Reads well.\n\n**SOUND**").verdict, Verdict::Approved);
        assert_eq!(parse_verdict("Reads badly.\n\nUNSOUND").verdict, Verdict::ChangesRequested);
    }

    /// A bare approval with no `VERDICT:` line is still an approval.
    #[test]
    fn bare_approval_line_still_approves() {
        for review in ["Looks good.\n\nAPPROVED", "Fine by me.\n\n**APPROVED**", "Ship it.\nApproved."] {
            assert_eq!(parse_verdict(review).verdict, Verdict::Approved, "missed: {review}");
        }
    }

    /// An empty section is written every way but the one asked for. Reading a
    /// decorated placeholder as an item invents a blocker nobody can fix, which
    /// fails the run and then stalls it.
    #[test]
    fn a_dressed_up_empty_section_yields_no_blockers() {
        for empty in ["- none", "- `none`", "- **None**", "- None.", "- no blockers"] {
            let review = format!("BLOCKERS:\n{}\n\nVERDICT: APPROVED", empty);
            assert!(parse_verdict(&review).blockers.is_empty(), "counted as a blocker: {empty}");
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

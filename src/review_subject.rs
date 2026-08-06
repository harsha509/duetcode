//! Finding the code an answer is actually about.
//!
//! An answer leaves no diff of its own, so the reviewer is given the code the
//! answer discusses. When that code is the working tree, a reviewer with tools
//! reads the very bytes the answer read and its reads corroborate. When the
//! answer is about a pull request, they do not: the reviewer stands in a
//! checkout of some other revision, and every file it opens is the wrong one.
//! Nothing announces that. Paths resolve, line numbers read, and the review
//! comes back agreeing with claims it never checked.
//!
//! This module answers two questions about a task's text — does it point at
//! code outside the checkout, and can that code be fetched — so the caller can
//! hand the reviewer the real change or refuse the review outright.

use anyhow::{anyhow, Result};
use std::path::Path;
use std::process::Command;

/// A GitHub pull request named in a task.
///
/// `url` is rebuilt from the parsed parts rather than sliced out of the prose,
/// so a link wrapped in markdown or trailed by a full stop still reaches `gh`
/// as a bare URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullRequest {
    pub url: String,
    /// How the pull request is named to the user and in the prompt, e.g.
    /// `octocat/hello-world#529`.
    pub label: String,
}

/// Cap on how many pull requests one task pulls in. Each is a subprocess and a
/// network round trip, and a reviewer given a dozen diffs reads none of them
/// closely.
pub const MAX_PULL_REQUESTS: usize = 4;

/// Text that points at a specific change somewhere other than the working tree.
///
/// Deliberately narrow. Every marker here names a particular pull request that
/// cannot be read from the checkout; none of them fire on a task that merely
/// discusses the code in front of it. The phrase "pull request" on its own is
/// absent on purpose — it appears in tasks about templates, CI, and review
/// process, none of which are ungrounded, and this list drives a refusal.
const ABSENT_CODE_MARKERS: [&str; 3] = ["/pull/", "/merge_requests/", "gh pr "];

const PULL_SEGMENT: &str = "/pull/";

/// True when the task is about a change that is not in this checkout.
///
/// Under-detects by design: "review PR #529", with no URL and no command, reads
/// as ordinary prose here and will not trip the refusal. Over-detecting is the
/// worse failure, because it blocks reviews that were properly grounded.
pub fn names_absent_code(text: &str) -> bool {
    let lower = text.to_lowercase();
    ABSENT_CODE_MARKERS.iter().any(|marker| lower.contains(marker))
}

/// Every GitHub pull request named in `text`, in the order they appear and
/// without repeats.
pub fn pull_requests(text: &str) -> Vec<PullRequest> {
    let mut found: Vec<PullRequest> = Vec::new();
    for (at, _) in text.match_indices(PULL_SEGMENT) {
        if let Some(pr) = parse_pull_request(text, at) {
            if !found.contains(&pr) {
                found.push(pr);
            }
        }
    }
    found
}

/// Reads one `…/owner/repo/pull/123` occurrence, given the offset of `/pull/`.
/// Returns `None` for anything that is not a GitHub pull request URL — an
/// unrelated path, an enterprise host, a `/pull/` with no number after it.
fn parse_pull_request(text: &str, at: usize) -> Option<PullRequest> {
    let number: String = text[at + PULL_SEGMENT.len()..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    if number.is_empty() {
        return None;
    }

    let mut segments = text[..at].rsplit('/');
    let repo = segments.next()?;
    let owner = segments.next()?;
    // The host segment carries whatever prose preceded the link — "see
    // github.com" — so only its last whitespace-delimited word is the host.
    let host = segments.next()?.split_whitespace().next_back()?;

    if !is_github_host(host) || !is_repo_name(owner) || !is_repo_name(repo) {
        return None;
    }

    Some(PullRequest {
        url: format!("https://github.com/{}/{}/pull/{}", owner, repo, number),
        label: format!("{}/{}#{}", owner, repo, number),
    })
}

/// Only github.com is treated as fetchable. An enterprise host still trips
/// [`names_absent_code`] through the raw `/pull/` marker, so its review is
/// refused rather than silently grounded against the wrong checkout.
fn is_github_host(host: &str) -> bool {
    host.eq_ignore_ascii_case("github.com") || host.eq_ignore_ascii_case("www.github.com")
}

fn is_repo_name(segment: &str) -> bool {
    !segment.is_empty()
        && segment
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// The pull request's diff, by way of `gh pr diff`.
///
/// Run inside `dir` so it picks up whatever `gh` configuration the checkout
/// carries; the URL is absolute, so the directory never decides *which* pull
/// request is fetched.
pub fn fetch_diff(pr: &PullRequest, dir: &Path) -> Result<String> {
    let output = Command::new("gh")
        .args(["pr", "diff", &pr.url])
        .current_dir(dir)
        .output()
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => {
                anyhow!("the gh CLI is not installed (`brew install gh`)")
            }
            _ => anyhow!("could not run gh: {}", e),
        })?;

    if !output.status.success() {
        return Err(anyhow!(
            "gh could not read {}: {}",
            pr.label,
            first_line(&String::from_utf8_lossy(&output.stderr))
        ));
    }

    let diff = String::from_utf8_lossy(&output.stdout).to_string();
    if diff.trim().is_empty() {
        return Err(anyhow!("gh returned an empty diff for {}", pr.label));
    }
    Ok(diff)
}

/// `gh` reports a failure over several lines, sometimes with a usage dump. Only
/// the first line says what went wrong, and it is the only part that fits the
/// one-line warning it ends up in.
fn first_line(text: &str) -> String {
    match text.lines().map(str::trim).find(|line| !line.is_empty()) {
        Some(line) => line.to_string(),
        None => "no output".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The reported failure: a task naming pull requests, reviewed against
    /// whatever happened to be checked out.
    #[test]
    fn a_task_naming_pull_requests_is_about_absent_code() {
        assert!(names_absent_code(
            "review this prs https://github.com/acme/api/pull/529 https://github.com/acme/ui/pull/451"
        ));
    }

    #[test]
    fn a_gh_command_and_a_gitlab_link_both_count() {
        assert!(names_absent_code("run gh pr diff 12 and tell me what changed"));
        assert!(names_absent_code("https://gitlab.com/acme/api/-/merge_requests/8"));
    }

    /// A task about the code in front of it must not be refused. Every one of
    /// these would lose its review if the markers were widened.
    #[test]
    fn ordinary_tasks_are_not_about_absent_code() {
        for task in [
            "are there any performance issues in this repo?",
            "add a retry to the upload path",
            "why does the pull request template ask for a test plan?",
            "explain how we pull config at startup",
        ] {
            assert!(!names_absent_code(task), "wrongly flagged: {task}");
        }
    }

    #[test]
    fn a_pull_request_url_is_parsed_into_a_canonical_form() {
        let found = pull_requests("look at https://github.com/acme/api/pull/529 please");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].url, "https://github.com/acme/api/pull/529");
        assert_eq!(found[0].label, "acme/api#529");
    }

    /// Links arrive wrapped in prose and punctuation. Slicing the raw text
    /// would hand `gh` a URL with a bracket or a full stop on the end.
    #[test]
    fn surrounding_prose_and_punctuation_are_stripped() {
        for text in [
            "see github.com/acme/api/pull/529.",
            "[#529](https://github.com/acme/api/pull/529)",
            "(https://www.github.com/acme/api/pull/529)",
            "https://github.com/acme/api/pull/529/files",
        ] {
            let found = pull_requests(text);
            assert_eq!(found.len(), 1, "missed: {text}");
            assert_eq!(found[0].url, "https://github.com/acme/api/pull/529", "wrong: {text}");
        }
    }

    #[test]
    fn several_pull_requests_are_found_in_order_without_repeats() {
        let found = pull_requests(
            "https://github.com/acme/api/pull/529 https://github.com/acme/ui/pull/451 \
             and again https://github.com/acme/api/pull/529",
        );
        assert_eq!(
            found.iter().map(|p| p.label.as_str()).collect::<Vec<_>>(),
            vec!["acme/api#529", "acme/ui#451"]
        );
    }

    /// Anything that is not a github.com pull request URL must not be handed to
    /// `gh` as though it were one.
    #[test]
    fn non_pull_request_urls_are_not_parsed() {
        for text in [
            "https://github.com/acme/api/pull/",
            "https://github.com/acme/api/pull/abc",
            "https://git.acme.corp/acme/api/pull/529",
            "https://github.com/acme/pull/529",
            "/pull/529",
        ] {
            assert!(pull_requests(text).is_empty(), "wrongly parsed: {text}");
        }
    }

    /// An enterprise host cannot be fetched, but the task is still about code
    /// that is not in the checkout — so the refusal has to fire on it.
    #[test]
    fn an_unfetchable_host_is_still_absent_code() {
        let text = "https://git.acme.corp/acme/api/pull/529";
        assert!(pull_requests(text).is_empty());
        assert!(names_absent_code(text));
    }

    #[test]
    fn the_first_useful_line_of_a_failure_is_kept() {
        assert_eq!(first_line("\n\ngh: not found\nusage: ...\n"), "gh: not found");
        assert_eq!(first_line("   \n"), "no output");
    }
}


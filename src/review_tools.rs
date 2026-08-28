//! Read-only git tools for API-backed models: any adapter declares them,
//! duetcode itself runs the allowlisted command in the checkout, and the
//! output goes back in the next turn. Argv is never built from model flags.

use serde_json::{json, Value};
use std::path::Path;

/// Cap on tool exchanges in one `generate`; past it the model is made to
/// answer with what it has already read.
pub const MAX_TOOL_ROUNDS: usize = 8;

/// Cap on one tool result, so a huge diff cannot flood the conversation.
const MAX_OUTPUT_BYTES: usize = 30_000;

/// One tool as declared to the API. `schema` is provider-neutral JSON schema;
/// each adapter wraps it in its own wire format.
pub struct ToolSpec {
    pub name: &'static str,
    pub description: &'static str,
    pub schema: Value,
}

pub fn specs() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: "git_status",
            description: "Working tree status of the repository under review: current branch, \
                          staged and unstaged changes, untracked files. Read-only.",
            schema: json!({ "type": "object", "properties": {} }),
        },
        ToolSpec {
            name: "git_diff",
            description: "Uncommitted changes as a patch (git diff HEAD). Set stat=true for the \
                          per-file summary instead of the full patch; set path to limit the diff \
                          to one file or directory. Read-only.",
            schema: json!({
                "type": "object",
                "properties": {
                    "stat": { "type": "boolean", "description": "summary only, no patch body" },
                    "path": { "type": "string", "description": "relative path to limit the diff to" }
                }
            }),
        },
        ToolSpec {
            name: "git_log",
            description: "Recent commits, one line each. Optional max_count (default 20, max \
                          100); optional path to list only commits touching it. Read-only.",
            schema: json!({
                "type": "object",
                "properties": {
                    "max_count": { "type": "integer", "description": "how many commits, 1-100" },
                    "path": { "type": "string", "description": "relative path to filter by" }
                }
            }),
        },
        ToolSpec {
            name: "git_show",
            description: "A commit's patch, or a file's content at a revision. rev is a commit \
                          reference such as HEAD, HEAD~2, or a hash; with path, shows that \
                          file's full content at rev. Read-only.",
            schema: json!({
                "type": "object",
                "properties": {
                    "rev": { "type": "string", "description": "commit reference" },
                    "path": { "type": "string", "description": "relative path to show at rev" }
                },
                "required": ["rev"]
            }),
        },
    ]
}

/// Executes one tool call in `dir`. Errors come back as text so the model can
/// correct its next call instead of the whole turn dying.
pub fn execute(dir: &Path, name: &str, args: &Value) -> String {
    match plan(name, args) {
        Ok(argv) => run_git(dir, &argv),
        Err(reason) => format!("error: {}", reason),
    }
}

/// The call as a readable git command line, for tool-action events.
pub fn describe(name: &str, args: &Value) -> String {
    match plan(name, args) {
        Ok(argv) => format!("running `git {}`", argv.join(" ")),
        Err(_) => format!("calling {} with invalid arguments", name),
    }
}

/// The path a call read, when it named one — recorded so a review's account of
/// what it checked can be verified rather than believed.
pub fn opened_path(name: &str, args: &Value) -> Option<String> {
    if plan(name, args).is_err() {
        return None;
    }
    args.get("path").and_then(Value::as_str).map(str::to_string)
}

/// The argv for one call, or why the arguments were refused. Flags are never
/// taken from the model: an injected `--output` would write to disk.
fn plan(name: &str, args: &Value) -> Result<Vec<String>, String> {
    match name {
        "git_status" => Ok(vec!["status".into()]),
        "git_diff" => {
            let mut argv = vec!["diff".to_string()];
            if args.get("stat").and_then(Value::as_bool).unwrap_or(false) {
                argv.push("--stat".into());
            }
            argv.push("HEAD".into());
            if let Some(path) = path_arg(args)? {
                argv.push("--".into());
                argv.push(path);
            }
            Ok(argv)
        }
        "git_log" => {
            let count =
                args.get("max_count").and_then(Value::as_u64).unwrap_or(20).clamp(1, 100);
            let mut argv =
                vec!["log".into(), "--oneline".into(), "-n".into(), count.to_string()];
            if let Some(path) = path_arg(args)? {
                argv.push("--".into());
                argv.push(path);
            }
            Ok(argv)
        }
        "git_show" => {
            let rev = args
                .get("rev")
                .and_then(Value::as_str)
                .ok_or("git_show needs a rev argument")?;
            check_rev(rev)?;
            let spec = match path_arg(args)? {
                Some(path) => format!("{}:{}", rev, path),
                None => rev.to_string(),
            };
            Ok(vec!["show".into(), spec])
        }
        other => Err(format!("unknown tool {}", other)),
    }
}

/// The optional `path` argument, refused unless it is a plain relative path
/// inside the checkout — no flags, no absolute paths, no parent escapes, no
/// pathspec magic.
fn path_arg(args: &Value) -> Result<Option<String>, String> {
    let Some(path) = args.get("path").and_then(Value::as_str) else {
        return Ok(None);
    };
    let refused = path.is_empty()
        || path.starts_with('-')
        || path.starts_with('/')
        || path.starts_with(':')
        || path.contains("..")
        || path.chars().any(char::is_control);
    if refused {
        return Err(format!("path {:?} is not a plain relative path in the repository", path));
    }
    Ok(Some(path.to_string()))
}

/// A commit reference is refused unless it looks like one: no leading dash
/// (a flag), no colon (the rev:path split is composed here, not by the model).
fn check_rev(rev: &str) -> Result<(), String> {
    let allowed = |c: char| c.is_ascii_alphanumeric() || "._-/~^@{}".contains(c);
    let refused =
        rev.is_empty() || rev.len() > 100 || rev.starts_with('-') || !rev.chars().all(allowed);
    if refused {
        return Err(format!("{:?} is not a valid commit reference", rev));
    }
    Ok(())
}

fn run_git(dir: &Path, argv: &[String]) -> String {
    let args: Vec<&str> = argv.iter().map(String::as_str).collect();
    match crate::git::git_output(dir, &args) {
        Ok(out) if out.status.success() => {
            let text = String::from_utf8_lossy(&out.stdout).to_string();
            if text.trim().is_empty() {
                "(no output)".into()
            } else {
                capped(text)
            }
        }
        Ok(out) => format!(
            "error: git {} failed: {}",
            argv.join(" "),
            first_line(&String::from_utf8_lossy(&out.stderr))
        ),
        Err(e) => format!("error: {:#}", e),
    }
}

fn first_line(text: &str) -> &str {
    text.lines().map(str::trim).find(|line| !line.is_empty()).unwrap_or("no output")
}

fn capped(text: String) -> String {
    if text.len() <= MAX_OUTPUT_BYTES {
        return text;
    }
    let mut cut = MAX_OUTPUT_BYTES;
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}\n[output truncated at {} KB]", &text[..cut], MAX_OUTPUT_BYTES / 1024)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_are_never_accepted_as_arguments() {
        for args in [
            json!({ "path": "--output=/tmp/evil" }),
            json!({ "path": "-R" }),
            json!({ "rev": "--output=/tmp/evil" }),
        ] {
            let name = if args.get("rev").is_some() { "git_show" } else { "git_diff" };
            assert!(plan(name, &args).is_err(), "accepted: {}", args);
        }
    }

    #[test]
    fn paths_may_not_escape_the_checkout() {
        for path in ["../secrets", "/etc/passwd", ":(top)x", "a/../../b", "a\nb"] {
            let args = json!({ "path": path });
            assert!(plan("git_diff", &args).is_err(), "accepted: {:?}", path);
        }
        assert!(plan("git_diff", &json!({ "path": "src/main.rs" })).is_ok());
    }

    #[test]
    fn a_show_of_a_file_at_a_rev_is_composed_here_not_by_the_model() {
        let argv = plan("git_show", &json!({ "rev": "HEAD~2", "path": "src/a.rs" })).unwrap();
        assert_eq!(argv, vec!["show", "HEAD~2:src/a.rs"]);
        assert!(plan("git_show", &json!({ "rev": "HEAD:src/a.rs" })).is_err());
    }

    #[test]
    fn log_count_is_clamped_and_diff_stat_is_a_fixed_flag() {
        let argv = plan("git_log", &json!({ "max_count": 5000 })).unwrap();
        assert_eq!(argv, vec!["log", "--oneline", "-n", "100"]);
        let argv = plan("git_diff", &json!({ "stat": true })).unwrap();
        assert_eq!(argv, vec!["diff", "--stat", "HEAD"]);
    }

    #[test]
    fn an_unknown_tool_or_bad_call_reports_instead_of_running() {
        assert!(execute(Path::new("."), "git_push", &json!({})).starts_with("error:"));
        assert!(execute(Path::new("."), "git_show", &json!({})).starts_with("error:"));
    }

    #[test]
    fn a_call_is_described_as_the_command_it_runs() {
        assert_eq!(describe("git_status", &json!({})), "running `git status`");
        assert_eq!(
            describe("git_diff", &json!({ "stat": true, "path": "src" })),
            "running `git diff --stat HEAD -- src`"
        );
    }

    #[test]
    fn only_a_valid_call_naming_a_path_counts_as_a_read() {
        assert_eq!(
            opened_path("git_show", &json!({ "rev": "HEAD", "path": "src/a.rs" })),
            Some("src/a.rs".into())
        );
        assert_eq!(opened_path("git_status", &json!({})), None);
        assert_eq!(opened_path("git_diff", &json!({ "path": "../x" })), None);
    }

    #[test]
    fn oversized_output_is_capped_on_a_char_boundary() {
        let text = "é".repeat(MAX_OUTPUT_BYTES);
        let capped = capped(text);
        assert!(capped.len() < MAX_OUTPUT_BYTES + 50);
        assert!(capped.ends_with("[output truncated at 29 KB]"));
    }
}

//! How much of a checkout gemini's file crawler would actually walk.
//!
//! The CLI's glob-based tools crawl every directory in the workspace before
//! any ignore file is consulted; only a short hardcoded list of directory
//! names is pruned during the walk, and `.gitignore` is applied to the
//! results afterwards. A checkout carrying a large tree outside that list —
//! an in-repo Python venv is the common case — hands the crawler tens of
//! thousands of entries per search, and the node process dies of heap
//! exhaustion mid-review. Nothing in gemini's configuration prevents this,
//! so the best duetcode can do is see it coming and say so before the
//! review starts, naming the trees to move or remove.

use std::path::{Path, PathBuf};

/// Directory names gemini's glob crawl skips — its `COMMON_IGNORE_PATTERNS`,
/// observed in v0.54 and pinned the same way the adapter pins the CLI's
/// stream-event shapes. Everything else is walked, gitignored or not.
const CRAWL_PRUNED_DIRS: &[&str] = &["node_modules", ".git", "bower_components", ".svn", ".hg"];

/// Entry count past which a workspace is reported as a crawl hazard. The
/// observed OOM died walking ~90k entries into a ~9 GB heap; warning at a
/// quarter of that leaves headroom for smaller machines and busier reviews.
const HAZARD_THRESHOLD: usize = 25_000;

/// Counting stops here: past this the verdict cannot change, and a runaway
/// tree must not make the preflight itself slow.
const COUNT_CAP: usize = 200_000;

/// Dependency and build trees by directory name, across ecosystems, so a
/// warning can say what an offender *is* rather than only how big. Names the
/// crawler already prunes (node_modules and friends) are absent on purpose —
/// they can never be offenders, and listing them would imply otherwise.
const EXTERNAL_TREES: &[(&str, &str)] = &[
    // Python
    (".venv", "Python virtualenv"),
    ("venv", "Python virtualenv"),
    ("env", "Python virtualenv"),
    (".tox", "Python tox environments"),
    ("site-packages", "Python packages"),
    ("__pypackages__", "Python packages"),
    ("__pycache__", "Python bytecode cache"),
    (".mypy_cache", "mypy cache"),
    (".pytest_cache", "pytest cache"),
    (".ruff_cache", "ruff cache"),
    // JavaScript / TypeScript
    (".next", "Next.js build output"),
    (".nuxt", "Nuxt build output"),
    (".yarn", "Yarn cache"),
    (".pnpm-store", "pnpm store"),
    (".turbo", "Turborepo cache"),
    (".angular", "Angular cache"),
    // Compiled-language and general build output
    ("target", "Rust or Maven build output"),
    ("build", "build output"),
    ("out", "build output"),
    ("dist", "build output"),
    ("coverage", "coverage output"),
    (".gradle", "Gradle cache"),
    ("obj", ".NET build output"),
    // Vendored dependencies and platform caches
    ("vendor", "vendored dependencies"),
    ("_build", "Elixir build output"),
    ("deps", "Elixir dependencies"),
    ("Pods", "CocoaPods dependencies"),
    ("Carthage", "Carthage dependencies"),
    ("DerivedData", "Xcode build output"),
    (".dart_tool", "Dart tool cache"),
    (".terraform", "Terraform providers"),
];

/// What a directory name is known to hold, when it is a recognised
/// dependency or build tree.
fn describe_tree(name: &str) -> Option<&'static str> {
    EXTERNAL_TREES.iter().find(|(n, _)| *n == name).map(|(_, kind)| *kind)
}

/// What the crawler would find under one workspace root.
pub struct CrawlAssessment {
    root: PathBuf,
    /// Entries the crawl would visit, counted up to [`COUNT_CAP`].
    pub entries: usize,
    pub capped: bool,
    /// Immediate subdirectories by subtree size, largest first.
    offenders: Vec<(String, usize)>,
}

pub fn assess(root: &Path) -> CrawlAssessment {
    assess_with_cap(root, COUNT_CAP)
}

fn assess_with_cap(root: &Path, cap: usize) -> CrawlAssessment {
    let mut remaining = cap;
    let mut offenders: Vec<(String, usize)> = Vec::new();

    if let Ok(entries) = std::fs::read_dir(root) {
        for entry in entries.flatten() {
            if remaining == 0 {
                break;
            }
            let Ok(file_type) = entry.file_type() else { continue };
            // The crawl runs with `follow: false`, so neither do we.
            if file_type.is_symlink() {
                continue;
            }
            remaining -= 1;
            if file_type.is_dir() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if is_pruned(&name) {
                    continue;
                }
                let before = remaining;
                count_tree(&entry.path(), &mut remaining);
                offenders.push((name, 1 + before - remaining));
            }
        }
    }

    offenders.sort_by(|a, b| b.1.cmp(&a.1));
    offenders.truncate(3);

    CrawlAssessment {
        root: root.to_path_buf(),
        entries: cap - remaining,
        capped: remaining == 0,
        offenders,
    }
}

impl CrawlAssessment {
    /// The warning a gemini CLI run should print, or nothing when the
    /// workspace is small enough to crawl safely.
    pub fn warning(&self) -> Option<String> {
        self.warning_at(HAZARD_THRESHOLD)
    }

    fn warning_at(&self, threshold: usize) -> Option<String> {
        if self.entries < threshold {
            return None;
        }
        let offenders: Vec<String> = self
            .offenders
            .iter()
            .filter(|(_, count)| count * 10 >= self.entries)
            .map(|(name, count)| match describe_tree(name) {
                Some(kind) => format!("{}: {} entries — {}", name, count, kind),
                None => format!("{}: {} entries", name, count),
            })
            .collect();
        let biggest = if offenders.is_empty() {
            String::new()
        } else {
            format!(" ({})", offenders.join(", "))
        };
        Some(format!(
            "gemini's file crawler will walk {}{} entries under {}{} — .gitignore does not \
             prune its crawl, and trees this size can run the CLI out of memory mid-review; \
             move or remove them (a venv belongs outside the checkout)",
            self.entries,
            if self.capped { "+" } else { "" },
            self.root.display(),
            biggest,
        ))
    }

    /// One-line form for a run where the crawler is already disabled: the
    /// offender and where, without the full explanation `dt doctor` gives.
    pub fn brief(&self) -> Option<String> {
        self.brief_at(HAZARD_THRESHOLD)
    }

    fn brief_at(&self, threshold: usize) -> Option<String> {
        if self.entries < threshold {
            return None;
        }
        let what = match self.offenders.first() {
            Some((name, count)) if count * 10 >= self.entries => {
                format!("{}: {} entries", name, count)
            }
            _ => format!("{} entries", self.entries),
        };
        Some(format!("{} under {}", what, self.root.display()))
    }
}

fn is_pruned(name: &str) -> bool {
    CRAWL_PRUNED_DIRS.contains(&name)
}

/// Counts the entries a crawl of `dir` would visit, decrementing `budget`
/// per entry and stopping when it runs out.
fn count_tree(dir: &Path, budget: &mut usize) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        if *budget == 0 {
            return;
        }
        let Ok(file_type) = entry.file_type() else { continue };
        if file_type.is_symlink() {
            continue;
        }
        *budget -= 1;
        if file_type.is_dir() {
            let name = entry.file_name();
            if !is_pruned(&name.to_string_lossy()) {
                count_tree(&entry.path(), budget);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// A fresh directory tree for one test, removed on drop so parallel
    /// tests never see each other's files.
    struct TestTree {
        root: PathBuf,
    }

    impl TestTree {
        fn new(name: &str) -> Self {
            let root = std::env::temp_dir()
                .join(format!("duetcode-crawl-{}-{}", std::process::id(), name));
            let _ = fs::remove_dir_all(&root);
            fs::create_dir_all(&root).expect("test tree must be creatable");
            Self { root }
        }

        fn files(&self, dir: &str, count: usize) -> &Self {
            let dir = self.root.join(dir);
            fs::create_dir_all(&dir).expect("test dir must be creatable");
            for i in 0..count {
                fs::write(dir.join(format!("f{}", i)), "").expect("test file must be writable");
            }
            self
        }
    }

    impl Drop for TestTree {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    /// A small checkout needs no warning at all.
    #[test]
    fn a_small_workspace_is_not_a_hazard() {
        let tree = TestTree::new("small");
        tree.files("src", 5);
        assert!(assess(&tree.root).warning().is_none());
    }

    /// What gemini prunes, the count must prune too — a giant node_modules
    /// never reaches the crawler, and warning about it would send the user
    /// to delete a tree that was never the problem.
    #[test]
    fn pruned_directories_are_not_counted() {
        let tree = TestTree::new("pruned");
        tree.files("node_modules/dep", 50).files("src", 3);
        let assessment = assess(&tree.root);
        // node_modules itself is seen (1 entry); its subtree is not.
        assert!(assessment.entries < 10, "counted {}", assessment.entries);
    }

    /// The one-line form names the offender and where — no lecture, and
    /// nothing below the threshold.
    #[test]
    fn the_brief_form_is_one_short_line() {
        let tree = TestTree::new("brief");
        tree.files(".venv/lib", 40).files("src", 2);
        let brief = assess(&tree.root).brief_at(10).expect("40 entries past threshold 10");
        assert!(brief.contains(".venv"), "got: {}", brief);
        assert!(!brief.contains(".gitignore"), "got: {}", brief);
        assert!(assess(&tree.root).brief().is_none(), "40 entries is under the real threshold");
    }

    /// The warning names the tree to remove, not just a number — and says what
    /// a recognised dependency tree is, so the user knows it is safe to move.
    #[test]
    fn the_warning_names_the_offending_tree() {
        let tree = TestTree::new("offender");
        tree.files(".venv/lib", 40).files("src", 2);
        let warning = assess(&tree.root).warning_at(10).expect("40 entries past threshold 10");
        assert!(warning.contains(".venv"), "got: {}", warning);
        assert!(warning.contains("Python virtualenv"), "got: {}", warning);
        assert!(warning.contains(".gitignore does not prune"), "got: {}", warning);
    }

    /// An offender with no catalog entry still shows up, just unlabelled — the
    /// accidental `path/to/venv` tree matched no known name and was the worst
    /// offender of all.
    #[test]
    fn an_unrecognised_offender_is_still_reported() {
        let tree = TestTree::new("unknown-offender");
        tree.files("path/to/junk", 40).files("src", 2);
        let warning = assess(&tree.root).warning_at(10).expect("40 entries past threshold 10");
        assert!(warning.contains("path: "), "got: {}", warning);
    }

    /// Counting stops at the cap rather than walking a runaway tree to the
    /// end; the capped count reports as a lower bound.
    #[test]
    fn counting_stops_at_the_cap() {
        let tree = TestTree::new("capped");
        tree.files("big", 30);
        let assessment = assess_with_cap(&tree.root, 10);
        assert_eq!(assessment.entries, 10);
        assert!(assessment.capped);
        let warning = assessment.warning_at(5).expect("capped count is past threshold");
        assert!(warning.contains("10+"), "got: {}", warning);
    }

    /// The crawl does not follow symlinks, so a link — even a cycle — must
    /// neither inflate the count nor hang the preflight.
    #[test]
    fn symlinks_are_not_followed() {
        let tree = TestTree::new("symlink");
        tree.files("real", 3);
        std::os::unix::fs::symlink(&tree.root, tree.root.join("real/loop"))
            .expect("test symlink must be creatable");
        let assessment = assess(&tree.root);
        assert!(assessment.entries < 6, "counted {}", assessment.entries);
    }
}

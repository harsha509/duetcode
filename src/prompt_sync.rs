//! Keeps a project's `.duet/prompts/` in step with the built-in templates.
//!
//! `dt init` copies the templates into the project and the loader prefers those
//! copies, so every later improvement to a built-in prompt stopped at the
//! project boundary: a repository initialised months ago kept reviewing with
//! that month's prompt for good, and nothing ever said so. Upgrading `dt` or the
//! extension changed the binary and left the prompts where they were.
//!
//! Each run now reconciles the copies against the templates the binary shipped
//! with. What a file *is* decides what happens to it:
//!
//! - written by us, untouched since — replaced with the current template
//! - written by us, edited since — left alone; the edit is the whole point
//! - older than this tracking — backed up, then brought current
//!
//! A manifest records the fingerprint of what was written, so "edited" means
//! edited rather than merely "unlike today's template". Nothing is created in a
//! project that has no `.duet/prompts/`: adopting duet is `dt init`'s decision
//! to make, not this module's.

use crate::events::{Event, Sink};
use crate::prompts;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const MANIFEST_FILE: &str = ".manifest.json";
const MAX_BACKUPS: u32 = 50;

/// The prompts a project keeps its own copy of, with the text that ships now.
const MANAGED: [(&str, &str); 4] = [
    ("implement.txt", prompts::DEFAULT_IMPLEMENT_TEMPLATE),
    ("review.txt", prompts::DEFAULT_REVIEW_TEMPLATE),
    ("fix.txt", prompts::DEFAULT_FIX_TEMPLATE),
    ("plan.txt", prompts::DEFAULT_PLAN_TEMPLATE),
];

#[derive(Serialize, Deserialize, Default)]
struct Manifest {
    /// The dt version that last wrote these files, for the reader's benefit.
    version: String,
    /// Prompt file name to the fingerprint of the text dt wrote there.
    files: BTreeMap<String, String>,
    /// Prompt file name to the dt version whose drift was already reported, so
    /// a deliberately customised prompt warns once per upgrade, not every run.
    notified: BTreeMap<String, String>,
}

/// What reconciling one prompt file calls for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    UpToDate,
    /// The project predates this file — an older `dt init` wrote fewer prompts.
    Create,
    /// Ours, untouched, and the template has moved on.
    Update,
    /// Edited by hand. Left alone; the user is told once per version.
    KeepCustomised,
    /// Predates the manifest, so authorship is unknowable. Backed up, then
    /// brought current: a stale prompt no one remembers editing is the case
    /// this module exists to end.
    Adopt,
}

/// The whole policy, as a pure function of what is on disk.
fn decide(current: Option<&str>, recorded: Option<&str>, template: &str) -> Action {
    let Some(current) = current else {
        return Action::Create;
    };
    if current == template {
        return Action::UpToDate;
    }
    match recorded {
        Some(recorded) if recorded == fingerprint(current) => Action::Update,
        Some(_) => Action::KeepCustomised,
        None => Action::Adopt,
    }
}

/// FNV-1a, 64-bit. Not a security hash — it answers "is this still the text we
/// wrote?", and it has to give that answer identically in every future build,
/// which rules out `DefaultHasher` (documented as unstable across releases).
fn fingerprint(text: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{:016x}", hash)
}

/// Brings `<dir>/.duet/prompts/` up to date with this binary's templates.
///
/// Best effort by design: a project whose prompts cannot be read or written
/// still runs its task with whatever is there. Reporting the problem is more
/// use than refusing to work.
pub fn sync(dir: &Path, sink: &dyn Sink) {
    let prompts_dir = dir.join(".duet").join("prompts");
    if !prompts_dir.is_dir() {
        return;
    }

    let manifest_path = prompts_dir.join(MANIFEST_FILE);
    let mut manifest = read_manifest(&manifest_path);
    let version = env!("CARGO_PKG_VERSION");
    let mut changed = false;

    for (name, template) in MANAGED {
        let path = prompts_dir.join(name);
        let current = std::fs::read_to_string(&path).ok();
        let action = decide(current.as_deref(), manifest.files.get(name).map(String::as_str), template);

        match action {
            Action::UpToDate => {
                // Record it so a template change later counts as ours, not theirs.
                changed |= manifest.files.insert(name.to_string(), fingerprint(template)).is_none();
            }
            Action::Create | Action::Update | Action::Adopt => {
                let backup = if action == Action::Adopt {
                    match back_up(&path) {
                        Ok(path) => Some(path),
                        Err(e) => {
                            warn(sink, format!("could not back up {}: {:#} — left as it is", name, e));
                            continue;
                        }
                    }
                } else {
                    None
                };

                if let Err(e) = std::fs::write(&path, template) {
                    warn(sink, format!("could not update .duet/prompts/{}: {:#}", name, e));
                    continue;
                }
                manifest.files.insert(name.to_string(), fingerprint(template));
                manifest.notified.remove(name);
                changed = true;
                report(sink, name, action, backup.as_deref(), version);
            }
            Action::KeepCustomised => {
                if manifest.notified.get(name).map(String::as_str) != Some(version) {
                    manifest.notified.insert(name.to_string(), version.to_string());
                    changed = true;
                    warn(
                        sink,
                        format!(
                            ".duet/prompts/{} is customised, so dt {} left it alone — its built-in \
                             version has changed; delete the file to take the new one",
                            name, version
                        ),
                    );
                }
            }
        }
    }

    if changed {
        manifest.version = version.to_string();
        if let Err(e) = write_manifest(&manifest_path, &manifest) {
            warn(sink, format!("could not record prompt versions: {:#}", e));
        }
    }
}

fn report(sink: &dyn Sink, name: &str, action: Action, backup: Option<&Path>, version: &str) {
    let text = match (action, backup) {
        (Action::Create, _) => format!("added .duet/prompts/{} from dt {}", name, version),
        (Action::Update, _) => format!("updated .duet/prompts/{} to the dt {} prompt", name, version),
        (Action::Adopt, Some(backup)) => format!(
            "replaced .duet/prompts/{}, which predates prompt tracking, with the dt {} prompt — \
             the old copy is {}",
            name,
            version,
            backup.file_name().unwrap_or_default().to_string_lossy()
        ),
        (Action::Adopt, None) => format!("replaced .duet/prompts/{} with the dt {} prompt", name, version),
        _ => return,
    };
    sink.event(Event::Info { text });
}

fn warn(sink: &dyn Sink, text: String) {
    sink.event(Event::Warn { text });
}

/// Renames the file aside, never over an existing backup — the copy from the
/// last upgrade is as worth keeping as this one.
fn back_up(path: &Path) -> std::io::Result<PathBuf> {
    let base = path.with_extension(format!(
        "{}.bak",
        path.extension().map(|e| e.to_string_lossy().into_owned()).unwrap_or_default()
    ));
    let mut candidate = base.clone();
    for n in 2..=MAX_BACKUPS {
        if !candidate.exists() {
            break;
        }
        candidate = base.with_extension(format!("bak.{}", n));
    }
    std::fs::rename(path, &candidate)?;
    Ok(candidate)
}

fn read_manifest(path: &Path) -> Manifest {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

fn write_manifest(path: &Path, manifest: &Manifest) -> anyhow::Result<()> {
    let json = serde_json::to_string_pretty(manifest)?;
    std::fs::write(path, json)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const OLD: &str = "an older built-in prompt\n";
    const NEW: &str = "the current built-in prompt\n";

    #[test]
    fn missing_file_is_created() {
        assert_eq!(decide(None, None, NEW), Action::Create);
    }

    #[test]
    fn matching_the_template_needs_nothing() {
        assert_eq!(decide(Some(NEW), Some(&fingerprint(NEW)), NEW), Action::UpToDate);
    }

    /// The case this module exists for: dt wrote the file, nobody touched it,
    /// and the built-in prompt has since improved.
    #[test]
    fn our_untouched_copy_follows_the_template() {
        assert_eq!(decide(Some(OLD), Some(&fingerprint(OLD)), NEW), Action::Update);
    }

    /// A deliberate edit outranks any improvement to the built-in prompt.
    #[test]
    fn a_hand_edited_prompt_is_never_overwritten() {
        let edited = "our own review prompt\n";
        assert_eq!(decide(Some(edited), Some(&fingerprint(OLD)), NEW), Action::KeepCustomised);
    }

    /// Written before the manifest existed, so authorship cannot be known.
    #[test]
    fn an_untracked_prompt_is_adopted() {
        assert_eq!(decide(Some(OLD), None, NEW), Action::Adopt);
    }

    /// The fingerprint decides whether an edit is respected, so it must mean
    /// the same thing in every future build.
    #[test]
    fn fingerprint_is_stable_and_distinguishing() {
        assert_eq!(fingerprint(""), "cbf29ce484222325");
        assert_eq!(fingerprint("dt"), "08914407b53b90e5");
        assert_ne!(fingerprint(OLD), fingerprint(NEW));
    }
}

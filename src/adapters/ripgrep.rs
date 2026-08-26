//! Whether the gemini CLI has a ripgrep it will actually use.
//!
//! Without one, gemini's search tool falls back to an in-process grep that
//! reads whole trees into memory — on a large checkout that is a heap
//! exhaustion crash, and the review dies with it. Being on PATH is not
//! enough: the CLI only accepts an `rg` whose *real* path sits under one of
//! a fixed set of system prefixes, so a `cargo install ripgrep` living in
//! `~/.cargo/bin` is found and then refused. (A gemini install that bundles
//! its own binary needs none of this; the npm package currently ships
//! without one.)

use std::path::{Path, PathBuf};
use std::process::Command;

/// The `rg` the gemini CLI would resolve, if any.
#[derive(Debug)]
pub enum RipgrepStatus {
    /// On PATH and under a prefix the CLI trusts.
    Trusted(PathBuf),
    /// On PATH, but the CLI will refuse it and fall back anyway.
    Untrusted(PathBuf),
    Missing,
}

/// Prefixes the gemini CLI accepts a system `rg` from — its
/// `isTrustedSystemPath` allowlist, observed in v0.54. Pinned to observed
/// behavior the same way the stream-event shapes in the adapter are.
const GEMINI_TRUSTED_PREFIXES: &[&str] = &[
    "/usr/bin",
    "/bin",
    "/usr/local/bin",
    "/opt/homebrew/bin",
    "/opt/homebrew/Cellar",
    "/usr/local/Cellar",
    "/usr/sbin",
    "/sbin",
];

const INSTALL_HINT: &str = "install it with `brew install ripgrep` (macOS) or `apt install ripgrep`";

pub fn ripgrep_status() -> RipgrepStatus {
    let Some(found) = find_on_path("rg") else {
        return RipgrepStatus::Missing;
    };
    // The CLI judges the real location, not the symlink — Homebrew's
    // /opt/homebrew/bin/rg is a link into the Cellar, and a link *from* a
    // trusted prefix into an untrusted home directory must not pass.
    let real = found.canonicalize().unwrap_or(found);
    if is_gemini_trusted(&real) {
        RipgrepStatus::Trusted(real)
    } else {
        RipgrepStatus::Untrusted(real)
    }
}

impl RipgrepStatus {
    /// The warning a gemini CLI run should print, or nothing when gemini can
    /// search the checkout safely.
    pub fn warning(&self) -> Option<String> {
        match self {
            Self::Trusted(_) => None,
            Self::Untrusted(path) => Some(format!(
                "rg at {} is outside the paths gemini trusts, so gemini will ignore it \
                 and grep in-process — which can run out of memory on large repos; {}",
                path.display(),
                INSTALL_HINT
            )),
            Self::Missing => Some(format!(
                "ripgrep not found — gemini will grep in-process, which can run out of \
                 memory on large repos; {}",
                INSTALL_HINT
            )),
        }
    }
}

/// A binary as a child process would resolve it: `which` sees real
/// executables only, never shell functions or aliases defined in the user's
/// interactive shell.
fn find_on_path(binary: &str) -> Option<PathBuf> {
    let mut cmd = Command::new("which");
    cmd.arg(binary);
    let output = crate::process::capture_with_timeout(&mut cmd, PROBE_TIMEOUT).ok()??;
    if !output.status.success() {
        return None;
    }
    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if path.is_empty() {
        None
    } else {
        Some(PathBuf::from(path))
    }
}

/// Budget for the `which` probe; a wedged filesystem must not hang startup.
const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

fn is_gemini_trusted(path: &Path) -> bool {
    // Component-wise, so /usr/binx does not pass as /usr/bin.
    GEMINI_TRUSTED_PREFIXES.iter().any(|prefix| path.starts_with(prefix))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Homebrew's rg is a symlink into the Cellar, so the *resolved* path is
    /// the one that must pass.
    #[test]
    fn system_install_locations_are_trusted() {
        assert!(is_gemini_trusted(Path::new("/opt/homebrew/Cellar/ripgrep/15.2.0/bin/rg")));
        assert!(is_gemini_trusted(Path::new("/usr/local/bin/rg")));
        assert!(is_gemini_trusted(Path::new("/usr/bin/rg")));
    }

    /// The case that motivates the whole check: `cargo install ripgrep` puts
    /// a working binary where gemini refuses to run it.
    #[test]
    fn a_cargo_installed_rg_is_not_trusted() {
        assert!(!is_gemini_trusted(Path::new("/Users/someone/.cargo/bin/rg")));
    }

    /// Prefix matching is by path component, not by string.
    #[test]
    fn a_lookalike_prefix_is_not_trusted() {
        assert!(!is_gemini_trusted(Path::new("/usr/binx/rg")));
    }

    /// A usable rg needs no message at all; the two failure shapes each name
    /// their own problem and the same fix.
    #[test]
    fn only_the_broken_states_warn() {
        assert!(RipgrepStatus::Trusted(PathBuf::from("/usr/bin/rg")).warning().is_none());

        let untrusted = RipgrepStatus::Untrusted(PathBuf::from("/home/u/.cargo/bin/rg"))
            .warning()
            .unwrap();
        assert!(untrusted.contains(".cargo/bin/rg"), "got: {}", untrusted);
        assert!(untrusted.contains("outside the paths gemini trusts"), "got: {}", untrusted);
        assert!(untrusted.contains("brew install ripgrep"), "got: {}", untrusted);

        let missing = RipgrepStatus::Missing.warning().unwrap();
        assert!(missing.contains("not found"), "got: {}", missing);
        assert!(missing.contains("brew install ripgrep"), "got: {}", missing);
    }
}

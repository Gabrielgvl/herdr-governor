//! Transcript a5 fixture-probe tests moved out of
//! `src/adapters/transcript/tests/`: a member's `src/` must pass against
//! its own tree alone (the guard-selftest mirrors each member's `src/`
//! plus the lockfile), so the committed corpus under the workspace
//! `tests/fixtures/contract/a5-samples/` is read here at run time. Every
//! case id recorded in `a5-probe-outcomes.json` keeps its same-named test;
//! the helpers below are duplicated from `src/adapters/transcript/tests.rs`
//! (the src half keeps only what its window tests still use).

#[cfg(test)]
mod a5_claude;
#[cfg(test)]
mod a5_cross;
#[cfg(test)]
mod a5_devin;
#[cfg(test)]
mod a5_pi;
#[cfg(test)]
mod bounded_tail;
#[cfg(test)]
mod user_turns;

use std::path::{Path, PathBuf};

use herdr_governor::adapters::transcript::{SessionPointer, TranscriptRoots};

/// `tests/fixtures/contract/a5-samples/` — the committed corpus.
fn samples() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../tests/fixtures/contract/a5-samples")
}

/// `SessionPointer::new` with the locator verbatim — as the daemon
/// builds it from `native_session` and the run cwd.
fn pointer(kind: &str, native_session: &str, cwd: Option<&str>) -> SessionPointer {
    SessionPointer::new(kind, native_session, cwd)
}

/// Write `bytes` as `dir/name`, creating parent dirs.
#[cfg(test)]
fn stage(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let path = dir.join(name);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(&path, bytes).unwrap();
    path
}

/// Roots with no real search space — path-keyed pointers don't need any.
fn no_roots() -> TranscriptRoots {
    TranscriptRoots::default()
}

/// uid-0 runners (root, CAP_DAC_OVERRIDE) make a mode-000 file readable —
/// the EACCES legs skip there, the same guard the Python suite applies
/// (`test_a5_permission_denied_causes`).
fn privileged_runner() -> bool {
    std::fs::metadata("/proc/self").is_ok_and(|m| std::os::unix::fs::MetadataExt::uid(&m) == 0)
}

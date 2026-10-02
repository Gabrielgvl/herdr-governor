//! `tests` — the a5 fixture mirror: every case id recorded in
//! `tests/fixtures/contract/a5-probe-outcomes.json` gets a same-named
//! test here (the mapping table rides the node report), plus the
//! plan-named window tests. Fixture paths resolve through
//! `CARGO_MANIFEST_DIR`; harness names live inside this subtree (I9).

mod a5_claude;
mod a5_cross;
mod a5_devin;
mod a5_pi;
mod windows;

use std::path::{Path, PathBuf};

use super::{SessionPointer, TranscriptRoots};

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

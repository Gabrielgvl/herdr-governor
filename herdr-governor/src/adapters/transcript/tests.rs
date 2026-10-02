//! `tests` — the plan-named window tests. The a5 fixture-probe mirror
//! (one test per case id in `a5-probe-outcomes.json`) lives in
//! `herdr-governor/tests/transcript_fixtures/`, since it reads the
//! workspace corpus at run time; harness names live inside this
//! subtree (I9).

mod windows;

use std::path::{Path, PathBuf};

use super::SessionPointer;

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

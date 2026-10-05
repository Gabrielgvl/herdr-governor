//! `publish` — §4.8's body-file publication (F17/H#62–64): a `File`
//! follow-up's text lands at `<state>/followups/<runId>/<seq>.md` through
//! a same-dir tmp, `0600`, fsync and an atomic rename, then a re-read
//! verifying size and digest. A failed publication enqueues nothing —
//! the outbox row commits only against a verified file.

use std::fs;
use std::io::{self, Write as _};
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};

use governor_core::identity::{Digest, RunId};
use sha2::Digest as _;

use crate::daemon::paths::{self, Paths};

/// Write `text` as `<runId>/<seq>.md` and return the final path. The tmp
/// file sits beside the target so the rename is atomic on one
/// filesystem; `digest` (sha-256 of `text`, the `body_digest` the row
/// commits) re-verifies the landed bytes — a mismatch refuses like any
/// I/O error.
pub(in crate::daemon) fn publish_body(
    paths: &Paths,
    run: &RunId,
    seq: u64,
    text: &str,
    digest: &Digest,
) -> io::Result<String> {
    let dir = paths.followups().join(&run.0);
    fs::create_dir_all(&dir)?;
    let mut permissions = fs::metadata(&dir)?.permissions();
    permissions.set_mode(paths::DIR_MODE);
    fs::set_permissions(&dir, permissions)?;

    let staging = dir.join(format!("{seq}.md.tmp"));
    let target = dir.join(format!("{seq}.md"));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(paths::FILE_MODE)
        .open(&staging)?;
    // A stale staging file surviving a crash keeps its old mode — pin
    // `0600` unconditionally, not only at create.
    fs::set_permissions(&staging, fs::Permissions::from_mode(paths::FILE_MODE))?;
    file.write_all(text.as_bytes())?;
    file.sync_all()?;
    drop(file);
    fs::rename(&staging, &target)?;

    let bytes = fs::read(&target)?;
    if bytes.len() != text.len() || Digest(sha2::Sha256::digest(&bytes).into()) != *digest {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "published follow-up body failed size/digest verification",
        ));
    }
    Ok(target.to_string_lossy().into_owned())
}

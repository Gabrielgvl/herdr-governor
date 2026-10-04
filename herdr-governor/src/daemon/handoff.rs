//! `handoff` — F24's marked file and its frozen copy (§4.9): the
//! no-follow read of `<state>/handoffs/<runId>/handoff.md` the
//! `read_handoff` predicate judges, the one `freeze_bytes` publisher both
//! freeze paths share (the step-3 poll and the `active` × `absent`
//! one-shot read, → F4), and `freeze_guarded` — compute the core's
//! transition and, when it journals a `FreezeHandoff`, publish the bytes
//! *before* the row can name the copy. Sync `std::fs` I/O inline in the
//! coordinator, like the follow-up publication (`delivery::publish`).
//! ponytail: one bounded (≤ 256 KiB) read per supervised Run per tick;
//! upgrade path a per-Run `(ino, size, mtime)` probe cache if D1 shows
//! tick pressure.

use std::fs;
use std::io::{self, Read as _, Write as _};
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};

use governor_core::acceptance::{HANDOFF_MAX_BYTES, HandoffReading, read_handoff};
use governor_core::config::Policy;
use governor_core::identity::{Digest, RunId, Timestamp};
use governor_core::lifecycle::{Event, StateChange, Transition, transition};
use rustix::fs::{Mode, OFlags};
use sha2::Digest as _;

use crate::store::Store;

use super::DaemonError;
use super::coordinator::apply::apply_with_retry;
use super::coordinator::{empty, versioned};
use super::paths::{self, Paths};
use super::supervision::supervised;

/// One marked-file read: the core's verdict plus, for `Valid`, the exact
/// bytes that digest — the bytes `freeze_bytes` publishes.
pub(super) struct Marked {
    /// `read_handoff`'s verdict.
    pub reading: HandoffReading,
    /// The bytes behind a `Valid` verdict; `None` otherwise.
    pub bytes: Option<Vec<u8>>,
}

/// `<state>/handoffs/<runId>/handoff.md` — the path the task prompt names.
pub(super) fn marked_path(paths: &Paths, run: &RunId) -> PathBuf {
    paths.handoffs().join(&run.0).join("handoff.md")
}

/// `<state>/frozen/<runId>/<wg>-<digest16>.md` — the frozen copy's name:
/// the generation and the digest prefix make it unique per freeze.
pub(super) fn frozen_path(
    paths: &Paths,
    run: &RunId,
    work_generation: u64,
    digest: &Digest,
) -> PathBuf {
    let prefix: String = hex(digest).chars().take(16).collect();
    paths
        .frozen()
        .join(&run.0)
        .join(format!("{work_generation}-{prefix}.md"))
}

/// F24/N5 — read the marked file without following symlinks: one
/// `O_NOFOLLOW` open (a symlink refuses `ELOOP`), the type and size
/// `fstat`ed on that descriptor, at most `HANDOFF_MAX_BYTES + 1` bytes
/// read from it. Every failure is `NotWritten` — never an error, never a
/// settlement cause by itself.
pub(super) fn read_marked(run: &RunId, path: &Path) -> Marked {
    let not_written = Marked {
        reading: HandoffReading::NotWritten,
        bytes: None,
    };
    let Ok(fd) = rustix::fs::open(
        path,
        OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK,
        Mode::empty(),
    ) else {
        return not_written;
    };
    let file = fs::File::from(fd);
    let Ok(meta) = file.metadata() else {
        return not_written;
    };
    let cap = u64::try_from(HANDOFF_MAX_BYTES)
        .unwrap_or(u64::MAX)
        .saturating_add(1);
    let mut bytes = Vec::new();
    let read = (&file).take(cap).read_to_end(&mut bytes).is_ok();
    let reading = read_handoff(
        run,
        meta.file_type().is_file(),
        meta.len(),
        read.then_some(bytes.as_slice()),
    );
    match reading {
        HandoffReading::Valid { .. } => Marked {
            reading,
            bytes: Some(bytes),
        },
        HandoffReading::NotWritten => not_written,
    }
}

/// §4.9 `freeze_bytes` — publish `bytes` at `target` (tmp `0600`, fsync,
/// atomic rename in the same dir); an existing copy whose bytes already
/// hash to `digest` is kept (a replayed freeze re-creates the copy
/// byte-identically). The landed file is re-verified against `digest`.
pub(super) fn freeze_bytes(target: &Path, digest: &Digest, bytes: &[u8]) -> io::Result<()> {
    if fs::read(target).is_ok_and(|landed| sha(&landed) == *digest) {
        return Ok(());
    }
    let dir = target
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "frozen path has no dir"))?;
    fs::create_dir_all(dir)?;
    fs::set_permissions(dir, fs::Permissions::from_mode(paths::DIR_MODE))?;
    let staging = target.with_extension("md.tmp");
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(paths::FILE_MODE)
        .open(&staging)?;
    fs::set_permissions(&staging, fs::Permissions::from_mode(paths::FILE_MODE))?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    fs::rename(&staging, target)?;
    if sha(&fs::read(target)?) != *digest {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "frozen handoff failed digest verification",
        ));
    }
    Ok(())
}

/// The F4 invariant as one call: a transition that journals a
/// `FreezeHandoff` gets its bytes published at the row's `frozen_path`
/// first; a failed publication applies nothing (the next pass retries),
/// so the row never names a copy that does not exist. Transitions
/// without a freeze pass through untouched.
pub(super) fn freeze_guarded(transition: Transition, bytes: Option<&[u8]>) -> Transition {
    let freeze = transition.state_changes.iter().find_map(|change| {
        if let StateChange::FreezeHandoff(frozen) = change {
            Some((frozen.frozen_path.clone(), frozen.digest))
        } else {
            None
        }
    });
    let Some((path, digest)) = freeze else {
        return transition;
    };
    match bytes.map(|b| freeze_bytes(Path::new(&path), &digest, b)) {
        Some(Ok(())) => transition,
        Some(Err(_)) | None => empty(),
    }
}

/// §4.7 step 3 — the handoff poll over every supervised Run: a `Valid`
/// marked file becomes `Event::Handoff{digest}` with its frozen path;
/// the core decides (freeze a new digest, resume an unassessed one,
/// ignore an assessed one) and `freeze_guarded` publishes before any
/// freeze row lands. A settled Run is never polled — a late handoff
/// never reopens it.
pub(super) fn poll(
    store: &mut Store,
    (policy, paths): (&Policy, &Paths),
    now: Timestamp,
) -> Result<(), DaemonError> {
    for run in store.unsettled_runs()? {
        if !supervised(run.state) {
            continue;
        }
        let marked = read_marked(&run.id, &marked_path(paths, &run.id));
        let HandoffReading::Valid { digest } = marked.reading else {
            continue;
        };
        apply_with_retry(store, now, |st| {
            let Some(current) = st.run(&run.id).ok().flatten() else {
                return empty();
            };
            let journal = st.journal(&current.id).unwrap_or_default();
            let handoffs = st.handoffs(&current.id).unwrap_or_default();
            let path = frozen_path(paths, &current.id, current.work_generation, &digest);
            freeze_guarded(
                transition(
                    &current,
                    &versioned(&current, Event::Handoff { digest }),
                    now,
                    policy,
                    (None, journal.as_slice(), handoffs.as_slice()),
                    &path.to_string_lossy(),
                ),
                marked.bytes.as_deref(),
            )
        })?;
    }
    Ok(())
}

/// sha-256 of `bytes` as a core `Digest`.
fn sha(bytes: &[u8]) -> Digest {
    Digest(sha2::Sha256::digest(bytes).into())
}

/// Lowercase hex of a digest.
fn hex(digest: &Digest) -> String {
    use std::fmt::Write as _;
    digest.0.iter().fold(String::new(), |mut out, byte| {
        let _unused = write!(out, "{byte:02x}");
        out
    })
}

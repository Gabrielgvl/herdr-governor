//! F24/N5 — the handoff reading: a marked regular file within the byte bound
//! reads `Valid` with the digest of the bytes that freeze; anything else is
//! "not written yet".

use sha2::Digest as _;

use crate::identity::{Digest, RunId};

/// N5/F24 — the handoff is a regular file of at most 256 KiB; anything else
/// counts as not written yet.
pub const HANDOFF_MAX_BYTES: usize = 256 * 1024;

/// F24 — the handoff marker the file's final non-whitespace content must be:
/// `<!-- herdr-governor handoff run=<runId> -->`.
pub const HANDOFF_MARKER_PREFIX: &str = "<!-- herdr-governor handoff run=";

/// F24 — the marker's closing bytes.
pub const HANDOFF_MARKER_SUFFIX: &str = " -->";

/// F24 — the result of reading the marked file without following symlinks:
/// a regular file within `HANDOFF_MAX_BYTES` whose final non-whitespace
/// content is the marker, or "not written yet".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandoffReading {
    /// A valid marked file was read and digested.
    Valid {
        /// The frozen content's digest.
        digest: Digest,
    },
    /// Anything else counts as not written yet — never a failure, never a
    /// settlement cause by itself.
    NotWritten,
}

/// F24/N5 — the reading predicate over the adapter's no-follow stat and the
/// file's bytes; the core never touches a filesystem, so both arrive as
/// values. `regular_file` is the `symlink_metadata`/`O_NOFOLLOW` file-type
/// answer: a symlink, directory, other node, missing path or failed stat is
/// `false`. `size` is the metadata byte count, `bytes` the file's full
/// content (`None` when the read failed); a `size`/`bytes` disagreement is an
/// untrusted read.
///
/// `Valid` requires all of: a regular file, at most `HANDOFF_MAX_BYTES`, and
/// trailing-ASCII-whitespace-stripped content ending in the marker — the
/// free-Markdown report ahead of it is unrestricted. `digest` is the sha-256
/// of the bytes as read — the bytes that freeze. Anything else is
/// `NotWritten`: never a failure, never a settlement cause by itself.
#[must_use]
pub fn read_handoff(
    run: &RunId,
    regular_file: bool,
    size: u64,
    bytes: Option<&[u8]>,
) -> HandoffReading {
    let Ok(max) = u64::try_from(HANDOFF_MAX_BYTES) else {
        return HandoffReading::NotWritten;
    };
    if !regular_file || size > max {
        return HandoffReading::NotWritten;
    }
    let Some(content) = bytes else {
        return HandoffReading::NotWritten;
    };
    if u64::try_from(content.len()) != Ok(size) || content.len() > HANDOFF_MAX_BYTES {
        return HandoffReading::NotWritten;
    }
    let marker = alloc::format!(
        "{}{}{}",
        HANDOFF_MARKER_PREFIX,
        run.0,
        HANDOFF_MARKER_SUFFIX
    );
    if !content.trim_ascii_end().ends_with(marker.as_bytes()) {
        return HandoffReading::NotWritten;
    }
    HandoffReading::Valid {
        digest: Digest(sha2::Sha256::digest(content).into()),
    }
}

//! `error` — the typed failure taxonomy of the transcript adapter, one
//! variant per A5 probe-outcome kind (`a5-probe-outcomes.json`). Failure
//! is a value here: nothing in the adapter panics, and the terminal
//! `Unreadable` is what the caller maps to terminal evidence.

use std::io::ErrorKind;

use thiserror::Error;

use super::window::Cursor;

/// Every way a transcript read can fail, typed. `code`/`reason` payloads
/// pin the evidence detail strings (`EACCES`, `ENOENT`, `invalid_json`,
/// `session_id_mismatch`, `anchor_mismatch`, `kind_not_path`,
/// `id_not_filename_safe`).
#[non_exhaustive]
#[derive(Debug, Error)]
pub enum TranscriptError {
    /// The source could not be opened or read — `code` is the errno name,
    /// or a policy name (`symlink_refused`) when no errno applies.
    #[error("transcript source unreadable: {code}")]
    SourceUnreadable {
        /// errno name (`EACCES`, `ENOENT`, `EIO`) or `symlink_refused`.
        code: &'static str,
    },
    /// A complete record failed to parse or broke source identity — a
    /// corrupt source defeats a no-progress proof (`A5-CORRUPT-NOT-ABSENCE`).
    #[error("transcript source malformed at {offset}: {reason}")]
    SourceMalformed {
        /// Byte offset of the faulty record (record index for the
        /// document source, which has no per-record offsets).
        offset: u64,
        /// `invalid_json` | `session_id_mismatch`.
        reason: &'static str,
    },
    /// The consumed region changed since `cursor` was issued — the window
    /// would mix two different sources.
    #[error("transcript source rewritten: {reason}")]
    SourceRewritten {
        /// `shorter_than_cursor` | `boundary_lost` | `anchor_mismatch` |
        /// `path_replaced` | `vanished_mid_read`.
        reason: &'static str,
    },
    /// The source or scan exceeded its byte budget.
    #[error("transcript source exceeds budget: {bytes_at_least} bytes against {budget}")]
    SourceExceedsBudget {
        /// Lower bound on the offending size.
        bytes_at_least: u64,
        /// The budget that was exceeded.
        budget: u64,
    },
    /// One record is larger than the window; `resume` skips past it so the
    /// gap is exposed, not silent (a5 oversized-record cases).
    #[error("transcript record at {offset} is {bytes} bytes — exceeds the window")]
    RecordExceedsBudget {
        /// Byte offset (line sources) or 1-based record index (document).
        offset: u64,
        /// The record's serialized size.
        bytes: u64,
        /// The cursor that resumes past the skipped record.
        resume: Cursor,
    },
    /// The pointer cannot name a source — unsafe id or wrong locator kind.
    #[error("session pointer invalid: {reason}")]
    SessionPointerInvalid {
        /// `kind_not_path` | `id_not_filename_safe`.
        reason: &'static str,
    },
    /// Several candidates matched and no rule disambiguates — ambiguity is
    /// a typed answer, never a guess (a5_claude_ambiguous_candidates).
    #[error("session pointer ambiguous: {candidates} candidates")]
    Ambiguous {
        /// How many candidates matched.
        candidates: usize,
    },
    /// No transcript source exists for this session kind — the supervision
    /// fallback result (terminal evidence is the caller's orchestration).
    #[error("no transcript source: {reason}")]
    Unreadable {
        /// `no_transcript_source` | `no_roots`.
        reason: &'static str,
    },
}

impl TranscriptError {
    /// The cursor past a skipped oversized record — present only on
    /// `RecordExceedsBudget`, so a caller can continue after the gap.
    #[must_use]
    pub fn resume_cursor(&self) -> Option<Cursor> {
        match self {
            Self::RecordExceedsBudget { resume, .. } => Some(*resume),
            Self::SourceUnreadable { .. }
            | Self::SourceMalformed { .. }
            | Self::SourceRewritten { .. }
            | Self::SourceExceedsBudget { .. }
            | Self::SessionPointerInvalid { .. }
            | Self::Ambiguous { .. }
            | Self::Unreadable { .. } => None,
        }
    }
}

/// errno names for the probed causes; anything else collapses to `EIO`.
pub(super) fn io_code(err: &std::io::Error) -> &'static str {
    if err.kind() == ErrorKind::NotFound {
        "ENOENT"
    } else if err.kind() == ErrorKind::PermissionDenied {
        "EACCES"
    } else {
        "EIO"
    }
}

/// Any `io::Error` as the typed unreadable failure.
pub(super) fn unreadable(err: &std::io::Error) -> TranscriptError {
    TranscriptError::SourceUnreadable { code: io_code(err) }
}

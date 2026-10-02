//! `error` — the store's typed error enum (thiserror, per the module-boundary
//! error convention): open and migration failures, corrupt persisted rows,
//! and constraint/conflict surfaces. Filled by P4.S1.

use thiserror::Error;

/// Typed store failures. `#[non_exhaustive]`: the P4.S2 codecs and P4.S3
/// writers add the row-decode and apply-conflict variants.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum StoreError {
    /// A `rusqlite` call failed.
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    /// `PRAGMA user_version` is newer than this build knows; `open` refuses
    /// rather than downgrading a schema it does not understand.
    #[error("user_version {found} exceeds supported schema version {known}")]
    SchemaTooNew {
        /// The database's `user_version`.
        found: u32,
        /// The newest schema version this build can open.
        known: u32,
    },
    /// An Appendix-B pragma read-back at open returned the wrong value.
    #[error("pragma {name}: expected {expected}, read back {actual}")]
    PragmaContract {
        /// The pragma that failed verification.
        name: &'static str,
        /// The contract value.
        expected: String,
        /// The value actually read back.
        actual: String,
    },
    /// A persisted row failed its checked decode — unknown enum spelling,
    /// malformed JSON, an out-of-shape timestamp or digest, or a value that
    /// cannot be represented in the row's column shape. Persisted data is
    /// never trusted: a bad row is this typed error, never a panic.
    #[error("corrupt row in {table}.{column}: {reason}")]
    CorruptRow {
        /// The table the bad value was read from (or was bound for).
        table: &'static str,
        /// The column holding the bad value.
        column: &'static str,
        /// What was wrong with it.
        reason: String,
    },
}

/// Which compare-and-swap write lost (Appendix B / F20): the shell re-reads
/// the row and recomputes rather than retrying the same write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ConflictKind {
    /// A `relay_instance_id` is already bound — a relay id never rebinds (F1).
    RelayBinding,
    /// The `launches` primary key or idempotency scope is taken.
    Launch,
    /// The `runs` primary key, `launch_id` or `child_name` is taken.
    Run,
    /// `version = expected` (or `settlement IS NULL`) did not hold (F20).
    RunVersion,
    /// The expected owner no longer owns the Run (F4).
    Owner,
    /// The journal row is not in the state the write transitions from (F8).
    Effect,
    /// `(run_id, message_key)` already queued, or the outbox row is not in
    /// the state the write transitions from (F17/F9).
    FollowUp,
    /// The obligation is past `pending` (F21).
    Recovery,
}

/// Why `Store::apply` committed nothing. `#[non_exhaustive]`: later phases
/// may name further refusals.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ApplyError {
    /// A conditional write matched no row; the whole transition rolled back.
    #[error("conflict on {kind:?} {key}")]
    Conflict {
        /// Which write lost.
        kind: ConflictKind,
        /// The row key the write addressed.
        key: String,
    },
    /// A Launch-row write found the row in a phase that is not a legal
    /// predecessor of the requested one (P4.0 matrix); `done` is terminal.
    #[error("launch {launch}: no legal predecessor for phase {phase}")]
    PhaseConflict {
        /// The Launch.
        launch: String,
        /// The phase the write asked for.
        phase: &'static str,
    },
    /// SQLite rejected a write under a CHECK, FOREIGN KEY, trigger or
    /// NOT NULL constraint — surfaced, never swallowed.
    #[error("constraint: {message}")]
    Constraint {
        /// SQLite's message.
        message: String,
    },
    /// The transition asked for a journal write the vocabulary forbids
    /// (`failed` without a certainty, or a plan-time state) — refused before
    /// the transaction opens.
    #[error("malformed effect write {key}: {reason}")]
    MalformedWrite {
        /// The effect key.
        key: String,
        /// What was wrong.
        reason: &'static str,
    },
    /// A core value has no persisted representation (a timestamp outside
    /// RFC3339's year range, an unencodable JSON member) — fail-closed.
    #[error("encode: {0}")]
    Encode(StoreError),
    /// Any other `rusqlite` failure.
    #[error("sqlite: {0}")]
    Sqlite(rusqlite::Error),
}

impl From<rusqlite::Error> for ApplyError {
    fn from(err: rusqlite::Error) -> Self {
        if let rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error {
                code: rusqlite::ErrorCode::ConstraintViolation,
                ..
            },
            _,
        ) = &err
        {
            return Self::Constraint {
                message: err.to_string(),
            };
        }
        Self::Sqlite(err)
    }
}

impl From<StoreError> for ApplyError {
    fn from(err: StoreError) -> Self {
        if let StoreError::Sqlite(inner) = err {
            return inner.into();
        }
        Self::Encode(err)
    }
}

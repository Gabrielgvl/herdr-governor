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
}

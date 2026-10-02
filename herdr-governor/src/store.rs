//! `store` — the SQLite store (spec §9). Its public API is
//! `apply(Transition)` plus read queries; lifecycle writes live only under
//! `transitions` (I10).
//!
//! `error` — the typed store errors; `migrate` — the Appendix-B DDL and the
//! expand-only `user_version` migration; `rows` — row ↔ core-type codecs;
//! `reads` — the typed read queries; `transitions` — the per-transaction
//! writers `apply` dispatches to.

use std::fmt;
use std::path::Path;

use rusqlite::{Connection, TransactionBehavior};

use governor_core::config::Qualification;
use governor_core::identity::Timestamp;
use governor_core::lifecycle::Transition;

mod error;
mod migrate;
mod reads;
mod rows;
#[cfg(test)]
mod tests_migrate;
mod transitions;

pub use error::{ApplyError, ConflictKind, StoreError};
pub use transitions::crash_checkpoint_count;

/// `PRAGMA busy_timeout` in milliseconds — the coordinator is the single
/// writer, but the mcp read path may share the file (Appendix-B header).
const BUSY_TIMEOUT_MS: i64 = 5000;

/// The SQLite store: the single writer for every lifecycle table.
///
/// Sibling store modules (`rows`, `reads`) reach the connection through
/// `conn()`; the public surface is `open`, [`Store::apply`] and the typed
/// read queries. Only `apply` (and the F26 `record_qualification`) ever
/// opens a write transaction, and only `transitions` holds write SQL.
pub struct Store {
    /// The open connection; reached through `conn()`.
    conn: Connection,
}

impl Store {
    /// The live connection — sibling store modules (`reads`, `rows`,
    /// `transitions`) and the in-crate tests drive it through this handle.
    #[must_use]
    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    /// Opens (creating if needed) the database at `path`, applies the
    /// Appendix-B pragma contract — `foreign_keys=ON`, `journal_mode=WAL`,
    /// `synchronous=FULL`, `busy_timeout=5000`, each verified by read-back —
    /// then runs the expand-only `user_version` migration.
    ///
    /// A database stamped with a `user_version` newer than this build knows is
    /// refused before any pragma or DDL that writes to the file.
    ///
    /// # Errors
    /// [`StoreError::Sqlite`] on connection failure,
    /// [`StoreError::PragmaContract`] when a verified pragma disagrees, and
    /// [`StoreError::SchemaTooNew`] for a newer schema.
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        let mut conn = Connection::open(path)?;
        // Per-connection pragmas first: none of them writes to the file.
        conn.pragma_update(None, "busy_timeout", BUSY_TIMEOUT_MS)?;
        require_pragma(&conn, "busy_timeout", BUSY_TIMEOUT_MS)?;
        conn.pragma_update(None, "synchronous", "FULL")?;
        require_pragma(&conn, "synchronous", 2)?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        require_pragma(&conn, "foreign_keys", 1)?;
        // Refuse a too-new schema before `journal_mode` — the only pragma here
        // persisted in the database header — or any DDL touches the file.
        migrate::check_supported(&conn)?;
        require_wal(&conn)?;
        migrate::migrate(&mut conn)?;
        Ok(Self { conn })
    }
}

/// The `apply` region (P4.S3) — the single lifecycle writer.
impl Store {
    /// Commits `transition` as exactly one `BEGIN IMMEDIATE … COMMIT`:
    /// `state_changes` in order, then `events`, then `effects`. A
    /// conditional write that matches no row, a constraint, or an
    /// unencodable value abandons the whole transaction — nothing of it is
    /// visible afterwards. `now` stamps the store-owned time columns; the
    /// core's own times ride verbatim.
    ///
    /// # Errors
    /// [`ApplyError::Conflict`] when a compare-and-swap write lost (re-read
    /// and recompute, F20); [`ApplyError::PhaseConflict`] on an illegal
    /// Launch phase step; [`ApplyError::Constraint`] when SQLite rejected a
    /// write; [`ApplyError::MalformedWrite`] for a journal write the
    /// vocabulary forbids; [`ApplyError::Encode`] for a value with no
    /// persisted shape; [`ApplyError::Sqlite`] otherwise.
    pub fn apply(&mut self, transition: &Transition, now: Timestamp) -> Result<(), ApplyError> {
        transitions::check_well_formed(transition)?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        transitions::apply_in(&tx, transition, now)?;
        tx.commit()?;
        Ok(())
    }

    /// F26 — records (or replaces) one qualification verdict in its own
    /// transaction; the `qualify` driver (Phase 6) is the caller.
    ///
    /// # Errors
    /// [`ApplyError::Encode`] for an unencodable value, [`ApplyError::Sqlite`]
    /// or [`ApplyError::Constraint`] from the engine.
    pub fn record_qualification(
        &mut self,
        qualification: &Qualification,
        now: Timestamp,
    ) -> Result<(), ApplyError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        transitions::record_qualification(&tx, qualification, now)?;
        tx.commit()?;
        Ok(())
    }
}

impl fmt::Debug for Store {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Store").finish_non_exhaustive()
    }
}

/// Sets `journal_mode=WAL` (a pragma that returns the resulting mode) and
/// verifies the read-back.
fn require_wal(conn: &Connection) -> Result<(), StoreError> {
    let mode: String =
        conn.pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get(0))?;
    if mode.eq_ignore_ascii_case("wal") {
        Ok(())
    } else {
        Err(StoreError::PragmaContract {
            name: "journal_mode",
            expected: "wal".to_owned(),
            actual: mode,
        })
    }
}

/// Reads an integer pragma back and errors when it is not `expected`.
fn require_pragma(conn: &Connection, name: &'static str, expected: i64) -> Result<(), StoreError> {
    let actual: i64 = conn.pragma_query_value(None, name, |row| row.get(0))?;
    if actual == expected {
        Ok(())
    } else {
        Err(StoreError::PragmaContract {
            name,
            expected: expected.to_string(),
            actual: actual.to_string(),
        })
    }
}

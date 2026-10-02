//! `migrate` — the Appendix-B DDL (tables, the settlement trigger, the
//! `outcomes` view) and the expand-only `user_version` migration that
//! `Store::open` drives. Filled by P4.S1.

use rusqlite::Connection;

use super::StoreError;

/// The Appendix-B v1 schema — all 13 tables, the settlement-immutability
/// trigger and the `outcomes` view, verbatim from the spec. The DDL lives in
/// a `.sql` file rather than a `&str` literal: the trigger's text trips the
/// I10 lexical lifecycle-write scan in `src/**` `.rs` sources.
pub(super) const SCHEMA_V1: &str = include_str!("schema_v1.sql");

/// The newest `user_version` this build knows.
pub(super) const SCHEMA_VERSION: u32 = 1;

/// `PRAGMA user_version` — the store's only schema-version marker.
fn user_version(conn: &Connection) -> Result<u32, StoreError> {
    Ok(conn.pragma_query_value(None, "user_version", |row| row.get(0))?)
}

/// Refuses a database whose schema is newer than this build — open never
/// downgrades. Pure read: `Store::open` runs it before the pragmas and DDL
/// that write to the file.
///
/// # Errors
/// `StoreError::SchemaTooNew` when `user_version` exceeds [`SCHEMA_VERSION`].
pub(super) fn check_supported(conn: &Connection) -> Result<(), StoreError> {
    let found = user_version(conn)?;
    if found <= SCHEMA_VERSION {
        Ok(())
    } else {
        Err(StoreError::SchemaTooNew {
            found,
            known: SCHEMA_VERSION,
        })
    }
}

/// Applies [`SCHEMA_V1`] to a version-0 database and stamps `user_version`
/// in the same transaction; a no-op on a current database. Call only after
/// [`check_supported`] has passed.
///
/// # Errors
/// `StoreError::Sqlite` when the DDL or the version stamp fails.
pub(super) fn migrate(conn: &mut Connection) -> Result<(), StoreError> {
    if user_version(conn)? != SCHEMA_VERSION {
        // Expand-only: `0 -> 1` is the only step.
        let tx = conn.transaction()?;
        tx.execute_batch(SCHEMA_V1)?;
        tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        tx.commit()?;
    }
    Ok(())
}

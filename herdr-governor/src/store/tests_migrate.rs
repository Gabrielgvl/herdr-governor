//! `Store::open` tests — the Appendix-B object set, the pragma contract,
//! idempotent versioning and newer-schema refusal. SELECT/PRAGMA only;
//! lifecycle-write SQL belongs under `transitions/` or `tests/` (I10), so the
//! settlement-trigger probe lives in `tests/store_schema.rs`.

use std::path::PathBuf;

use rusqlite::Connection;
use tempfile::tempdir;

use super::{Store, StoreError};

/// A `(TempDir, db path)` pair for a not-yet-created database file.
fn db_path() -> (tempfile::TempDir, PathBuf) {
    let dir = tempdir().expect("tempdir must be creatable");
    let path = dir.path().join("store.db");
    (dir, path)
}

/// `(type, name, tbl_name, sql)` for every `sqlite_master` row in creation
/// order — a full-fidelity snapshot for the zero-DDL idempotency check.
fn schema_snapshot(conn: &Connection) -> Vec<(String, String, String, Option<String>)> {
    conn.prepare("SELECT type, name, tbl_name, sql FROM sqlite_master ORDER BY rowid")
        .expect("sqlite_master query must prepare")
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .expect("sqlite_master query must run")
        .collect::<Result<_, _>>()
        .expect("sqlite_master rows must decode")
}

/// An integer pragma read-back on `conn`.
fn pragma_i64(conn: &Connection, name: &str) -> i64 {
    conn.pragma_query_value(None, name, |row| row.get(0))
        .expect("pragma read-back must succeed")
}

#[test]
fn open_creates_all_appendix_b_objects() {
    let (_dir, path) = db_path();
    let store = Store::open(&path).expect("open must succeed on a fresh path");

    let objects: Vec<(String, String)> = store
        .conn()
        .prepare(
            "SELECT type, name FROM sqlite_master \
             WHERE name NOT LIKE 'sqlite_%' ORDER BY type, name",
        )
        .expect("sqlite_master query must prepare")
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .expect("sqlite_master query must run")
        .collect::<Result<_, _>>()
        .expect("sqlite_master rows must decode");

    let expected: Vec<(String, String)> = [
        ("table", "callers"),
        ("table", "cooldowns"),
        ("table", "effects"),
        ("table", "handoffs"),
        ("table", "judgment_sets"),
        ("table", "judgments"),
        ("table", "launches"),
        ("table", "mailbox"),
        ("table", "outbox"),
        ("table", "qualifications"),
        ("table", "recoveries"),
        ("table", "relay_bindings"),
        ("table", "runs"),
        ("trigger", "runs_settlement_immutable"),
        ("view", "outcomes"),
    ]
    .iter()
    .map(|(kind, name)| ((*kind).to_owned(), (*name).to_owned()))
    .collect();

    assert_eq!(
        objects, expected,
        "sqlite_master must hold exactly the 13 Appendix-B tables plus the trigger and the view"
    );
}

#[test]
fn open_verifies_pragma_contract() {
    let (_dir, path) = db_path();
    let store = Store::open(&path).expect("open must succeed on a fresh path");

    assert_eq!(
        pragma_i64(store.conn(), "foreign_keys"),
        1,
        "foreign_keys must read back ON"
    );
    assert_eq!(
        pragma_i64(store.conn(), "synchronous"),
        2,
        "synchronous must read back FULL (2)"
    );
    assert_eq!(
        pragma_i64(store.conn(), "busy_timeout"),
        5000,
        "busy_timeout must read back 5000 ms"
    );
    let journal_mode: String = store
        .conn()
        .pragma_query_value(None, "journal_mode", |row| row.get(0))
        .expect("journal_mode read-back must succeed");
    assert_eq!(journal_mode, "wal", "journal_mode must read back wal");
}

#[test]
fn open_is_idempotent_and_versioned() {
    let (_dir, path) = db_path();
    let store = Store::open(&path).expect("first open must succeed");

    let version: u32 = store
        .conn()
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .expect("user_version read-back must succeed");
    assert_eq!(version, 1, "open must stamp user_version 1");
    let before = schema_snapshot(store.conn());
    drop(store);

    let reopened = Store::open(&path).expect("second open must succeed");
    let after = schema_snapshot(reopened.conn());
    assert_eq!(
        before, after,
        "a second open must apply zero DDL — sqlite_schema must be byte-identical"
    );
    let version_after: u32 = reopened
        .conn()
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .expect("user_version read-back must succeed");
    assert_eq!(version_after, 1, "a second open must not move user_version");
}

#[test]
fn open_refuses_newer_schema() {
    let (_dir, path) = db_path();
    {
        let conn = Connection::open(&path).expect("raw open must succeed");
        conn.pragma_update(None, "user_version", 2)
            .expect("user_version stamp must succeed");
    }

    match Store::open(&path) {
        Err(StoreError::SchemaTooNew { found, known }) => {
            assert_eq!(found, 2, "the refused database's user_version");
            assert_eq!(known, 1, "the build's newest known schema version");
        }
        other => panic!("expected SchemaTooNew, got {other:?}"),
    }

    // Refusal is read-only: the file keeps its version and its original
    // journal mode — `open` must not WAL-convert a schema it cannot read.
    let conn = Connection::open(&path).expect("reopen must succeed");
    let version: u32 = conn
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .expect("user_version read-back must succeed");
    assert_eq!(version, 2, "refusal must not move user_version");
    let mode: String = conn
        .pragma_query_value(None, "journal_mode", |row| row.get(0))
        .expect("journal_mode read-back must succeed");
    assert_eq!(mode, "delete", "refusal must not convert the journal mode");
}

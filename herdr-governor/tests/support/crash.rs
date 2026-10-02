//! P4.S4 crash-suite helpers: the declared scenario table, the probe
//! driver and the reopen assertions shared by every boundary case.
//!
//! All-or-nothing is asserted on the whole database: a canonical dump of
//! every table is taken after the seed (`S0`) and after a clean apply in a
//! fresh store (`S1`); a store reopened after a kill must dump as exactly
//! `S0` (pre-commit kill) or exactly `S1` (kill after commit). Reopening
//! through `Store::open` is itself the WAL-recovery check — a database
//! `open` cannot recover is a real bug, never papered over here.

use std::fmt::Write as _;
use std::os::unix::process::ExitStatusExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use governor_core::identity::Timestamp;
use herdr_governor::store::Store;
use rusqlite::types::Value;
use tempfile::TempDir;

/// The 18 canned scenarios with their declared statement-boundary count
/// `N` — the 12 named Appendix-B transactions, the two P4.1 follow-up
/// edges, then the four composed launch scenarios. `matrix.rs` asserts
/// each against `store_probe count` so the table cannot rot when a writer
/// changes its statement list.
pub const SCENARIOS: [(&str, usize); 18] = [
    ("bind_caller", 2),
    ("admit_launch", 2),
    ("route", 2),
    ("plan_effect", 1),
    ("dispatch_effect", 1),
    ("effect_result", 2),
    ("enqueue_follow_up", 1),
    ("dispatch_follow_up", 2),
    ("resolve_follow_up", 2),
    ("settle", 7),
    ("handover", 1),
    ("recovery_dispatch", 3),
    ("freeze_handoff", 2),
    ("record_evidence", 1),
    ("launch_begin", 3),
    ("launch_finish_done", 4),
    ("launch_finish_routed_abandon", 5),
    ("launch_finish_abstain", 3),
];

/// The store-stamped time the probe writes with (mirrors the probe).
pub const NOW: Timestamp = Timestamp(1_790_812_800_000);

/// `SIGABRT` — what `std::process::abort()` raises.
const SIGABRT: i32 = 6;

/// The declared boundary count of `scenario`.
#[must_use]
pub fn declared(scenario: &str) -> usize {
    SCENARIOS
        .iter()
        .find(|(name, _)| *name == scenario)
        .map(|(_, n)| *n)
        .expect("a declared scenario")
}

/// Runs `store_probe <scenario> <db> <args…>` with `env` exported.
#[must_use]
pub fn probe(scenario: &str, db: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_store_probe"));
    command
        .arg(scenario)
        .arg(db)
        .args(args)
        .env_remove("GOV_STORE_CRASH_AT")
        .env_remove("GOV_STORE_CRASH_COUNT")
        .envs(env.iter().copied());
    command.output().expect("spawn store_probe")
}

/// A fresh store seeded with `scenario`'s pre-state through the probe.
#[must_use]
pub fn seeded(scenario: &str) -> (TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("store.db");
    let out = probe(scenario, &db, &["seed"], &[]);
    assert!(
        out.status.success(),
        "seed {scenario}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    (dir, db)
}

/// Reopens `db` (WAL recovery) and dumps every table canonically: one
/// line per row, `table|col=value|…`, sorted.
#[must_use]
pub fn dump(db: &Path) -> Vec<String> {
    let store = Store::open(db).expect("reopen after kill: WAL must recover");
    let conn = store.conn();
    let integrity: String = conn
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .expect("integrity_check");
    assert_eq!(integrity, "ok", "integrity_check after reopen");
    let mut tables = conn
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'")
        .expect("list tables");
    let names: Vec<String> = tables
        .query_map([], |row| row.get(0))
        .expect("query tables")
        .collect::<Result<_, _>>()
        .expect("table names");
    let mut lines = Vec::new();
    for table in names {
        let mut rows = conn
            .prepare(&format!("SELECT * FROM {table}"))
            .expect("select table");
        let columns: Vec<String> = rows
            .column_names()
            .iter()
            .map(|c| (*c).to_owned())
            .collect();
        let mut cursor = rows.query([]).expect("query table");
        while let Some(row) = cursor.next().expect("next row") {
            let mut line = table.clone();
            for (i, column) in columns.iter().enumerate() {
                let value: Value = row.get(i).expect("column value");
                write!(line, "|{column}={}", render(&value)).expect("row line");
            }
            lines.push(line);
        }
    }
    lines.sort();
    lines
}

/// A SQLite value as a canonical token (no `Debug` dependence).
fn render(value: &Value) -> String {
    match value {
        Value::Null => "NULL".to_owned(),
        Value::Integer(i) => i.to_string(),
        Value::Real(r) => r.to_string(),
        Value::Text(t) => format!("'{t}'"),
        Value::Blob(b) => format!(
            "x{}",
            b.iter().fold(String::new(), |mut acc, byte| {
                write!(acc, "{byte:02x}").expect("hex");
                acc
            })
        ),
    }
}

/// The dump of `scenario` after its target committed cleanly — `S1`.
#[must_use]
pub fn committed(scenario: &str) -> Vec<String> {
    let (_dir, db) = seeded(scenario);
    let out = probe(scenario, &db, &["apply"], &[]);
    assert!(
        out.status.success(),
        "apply {scenario}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    dump(&db)
}

/// Asserts the probe died of `SIGABRT` — a normal exit means the boundary
/// was never reached (exit 4) or the knob was wrong (exit 2).
fn assert_aborted(scenario: &str, what: &str, out: &Output) {
    assert_eq!(
        out.status.signal(),
        Some(SIGABRT),
        "{scenario} {what}: expected SIGABRT, got {:?} / stderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Idempotent replay after reopen: re-applying the target either commits
/// (exit 0) or answers with a typed conflict (exit 3) — and the end state
/// is exactly `S1` either way, never a partial double-apply.
fn assert_replay_converges(scenario: &str, db: &Path, s1: &[String]) {
    let out = probe(scenario, db, &["apply"], &[]);
    assert!(
        matches!(out.status.code(), Some(0 | 3)),
        "{scenario} replay: {:?} / {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        dump(db),
        s1,
        "{scenario}: replay must land on the committed state"
    );
}

/// Kill after statement `k` (`0` = before the transaction): the reopened
/// store is exactly the seed state, and the replay commits the target.
pub fn assert_pre_commit(scenario: &str, k: usize) {
    assert!(
        k <= declared(scenario),
        "{scenario}: boundary {k} is past the declared count"
    );
    let s1 = committed(scenario);
    let (_dir, db) = seeded(scenario);
    let s0 = dump(&db);
    assert_ne!(s0, s1, "{scenario}: the target must change the store");
    let knob = k.to_string();
    let env: &[(&str, &str)] = if k == 0 {
        &[]
    } else {
        &[("GOV_STORE_CRASH_AT", knob.as_str())]
    };
    let out = probe(scenario, &db, &["abort-after", &knob], env);
    assert_aborted(scenario, &format!("abort-after {k}"), &out);
    assert_eq!(
        dump(&db),
        s0,
        "{scenario} abort-after {k}: a pre-commit kill must leave no trace"
    );
    assert_replay_converges(scenario, &db, &s1);
}

/// Kill right after `COMMIT` returned: the reopened store holds the whole
/// transaction (`synchronous=FULL` durability), and the replay is a no-op
/// or a typed conflict.
pub fn assert_after_commit(scenario: &str) {
    let s1 = committed(scenario);
    let (_dir, db) = seeded(scenario);
    let out = probe(scenario, &db, &["abort-after-commit"], &[]);
    assert_aborted(scenario, "abort-after-commit", &out);
    assert_eq!(
        dump(&db),
        s1,
        "{scenario} abort-after-commit: the committed transaction must survive whole"
    );
    assert_replay_converges(scenario, &db, &s1);
}

/// `store_probe count` on a seeded store: the measured boundary count.
#[must_use]
pub fn measured(scenario: &str) -> usize {
    let (_dir, db) = seeded(scenario);
    let out = probe(scenario, &db, &["count"], &[("GOV_STORE_CRASH_COUNT", "1")]);
    assert!(
        out.status.success(),
        "count {scenario}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse()
        .expect("a boundary count")
}

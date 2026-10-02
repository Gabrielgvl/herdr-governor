//! The one P4.S1 schema test that needs raw lifecycle SQL: the Appendix-B
//! `runs_settlement_immutable` trigger must abort a settlement mutation on an
//! already-settled row. It lives outside `src/` because I10 confines
//! INSERT/UPDATE/DELETE on the lifecycle tables to `store/transitions/` or
//! `tests/`.

#[cfg(test)]
mod tests {
    use herdr_governor::store::Store;
    use rusqlite::Connection;
    use tempfile::tempdir;

    #[test]
    fn settlement_trigger_rejects_update() {
        let dir = tempdir().expect("tempdir must be creatable");
        let path = dir.path().join("store.db");
        let store = Store::open(&path).expect("open must migrate a fresh database");
        drop(store);

        let conn = Connection::open(&path).expect("raw connection must open");
        conn.execute_batch(
            "INSERT INTO callers (caller_id, agent_kind, native_session, first_seen_at)
             VALUES (1, 'test', 'sess-1', '2026-10-01T00:00:00.000Z');
             INSERT INTO launches (launch_id, caller_id, project_root, idempotency_key,
                                   digest_version, task_digest, task_json, phase,
                                   created_at, updated_at)
             VALUES ('l1', 1, '/p', 'k1', 1, 'd', '{}', 'launching',
                     '2026-10-01T00:00:00.000Z', '2026-10-01T00:00:00.000Z');
             INSERT INTO runs (run_id, launch_id, owner_caller_id, state, child_name,
                               cwd, max_age_deadline, settlement, settled_at,
                               created_at, updated_at)
             VALUES ('r1', 'l1', 1, 'settled', 'w1:r1', '/p',
                     '2026-10-02T00:00:00.000Z', 'accepted', '2026-10-01T12:00:00.000Z',
                     '2026-10-01T00:00:00.000Z', '2026-10-01T12:00:00.000Z');",
        )
        .expect("fixture rows must insert");

        let err = conn
            .execute(
                "UPDATE runs SET settlement = 'rejected' WHERE run_id = 'r1'",
                [],
            )
            .expect_err("the settlement-immutability trigger must abort the update");
        assert!(
            err.to_string().contains("settlement is immutable"),
            "the trigger's RAISE(ABORT) message must surface, got: {err}"
        );
        let kept: String = conn
            .query_row(
                "SELECT settlement FROM runs WHERE run_id = 'r1'",
                [],
                |row| row.get(0),
            )
            .expect("settlement read must succeed");
        assert_eq!(kept, "accepted", "the settled row must be unchanged");
    }
}

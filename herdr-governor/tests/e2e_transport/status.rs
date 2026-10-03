//! `status` — F7 end-to-end over the daemon boundary: the same
//! `daemon::status::page` the coordinator's `Msg::Tool` arm calls, driven
//! in-process against a real tempdir store. The socket-level leg — v1
//! frame → `mcp::serve` → `Msg::Tool` → this page — is `transport`'s
//! `transport_herdr_status_serves_over_the_real_socket` (P5.M2's listener).

use std::fmt::Write as _;

use governor_core::config::ConfigVersion;
use governor_core::identity::{
    AgentKind, CallerBinding, CallerKey, EventId, NativeSession, PaneId, RelayInstanceId, Timestamp,
};
use governor_core::lifecycle::{StateChange, Transition};
use herdr_governor::daemon::status::{self, ConfigHealth, HerdrHealth, StatusView};
use herdr_governor::store::Store;
use tempfile::{TempDir, tempdir};

const NOW: Timestamp = Timestamp(1_790_812_800_000);
const T0: &str = "2026-10-01T00:00:00.000Z";
const T1: &str = "2026-10-01T00:01:30.000Z";
const RELAY_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const RELAY_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const HEX: &str = "abababababababababababababababababababababababababababababababab";
const TASK: &str = r#"{"objective":"o","scope":"s","done_when":["d"],"constraints":[],"tier":null,"recovery_of":null,"label":null,"cwd":null}"#;

fn view() -> StatusView {
    StatusView {
        now: NOW,
        pid: 4242,
        uptime_secs: 7,
        version: "0.1.0-test",
        herdr: Some(HerdrHealth {
            at: Timestamp(NOW.0 - 3_000),
            incarnation: "42:1790812800.5".into(),
        }),
        config: ConfigHealth {
            valid: true,
            version: ConfigVersion("cfg-digest".into()),
            last_good_at: NOW,
            last_error: None,
        },
    }
}

/// Bind a caller so its `callers` row exists; returns the core key.
fn seed_caller(store: &mut Store, relay: &str, kind: &str, session: &str) -> CallerKey {
    let caller = CallerKey {
        agent_kind: AgentKind(kind.into()),
        native_session: NativeSession(session.into()),
    };
    store
        .apply(
            &Transition {
                state_changes: vec![StateChange::BindCaller(CallerBinding {
                    caller: caller.clone(),
                    relay_instance: RelayInstanceId(relay.into()),
                    pane_at_bind: PaneId("w1:p0".into()),
                })],
                events: Vec::new(),
                effects: Vec::new(),
            },
            NOW,
        )
        .expect("bind caller");
    caller
}

fn caller_id(store: &Store, session: &str) -> i64 {
    store
        .conn()
        .query_row(
            "SELECT caller_id FROM callers WHERE native_session = ?1",
            [session],
            |row| row.get(0),
        )
        .unwrap()
}

fn insert_run(store: &Store, run: &str, launch: &str, owner: i64, state: &str) {
    store
        .conn()
        .execute_batch(&format!(
            "INSERT INTO launches (launch_id, caller_id, project_root, idempotency_key,
                 digest_version, task_digest, task_json, phase, created_at, updated_at)
             VALUES ('{launch}', {owner}, '/p', 'k-{launch}', 1, '{HEX}', '{TASK}',
                     'routed', '{T0}', '{T0}');
             INSERT INTO runs (run_id, launch_id, owner_caller_id, state, child_name,
                 cwd, max_age_deadline, created_at, updated_at)
             VALUES ('{run}', '{launch}', {owner}, '{state}', 'gov-{run}', '/p',
                     '{T1}', '{T0}', '{T0}');"
        ))
        .unwrap();
}

fn insert_event(store: &Store, event: &str, dedup: &str, run: &str, kind: &str, body: &str) {
    store
        .conn()
        .execute_batch(&format!(
            "INSERT INTO mailbox (event_id, dedup_key, run_id, kind, body_json, created_at)
             VALUES ('{event}', '{dedup}', '{run}', '{kind}', '{body}', '{T0}')"
        ))
        .unwrap();
}

fn store_in(dir: &TempDir) -> Store {
    Store::open(&dir.path().join("governor.db")).unwrap()
}

/// F7 — `unreadEventIds` pages through the one opaque cursor; traversal
/// returns every id exactly once, in `(created_at, event_id)` order.
#[test]
fn f7_status_pages_unread_ids() {
    let tmp = tempdir().unwrap();
    let mut store = store_in(&tmp);
    let owner = seed_caller(&mut store, RELAY_A, "kind-a", "sess-a");
    let owner_id = caller_id(&store, "sess-a");
    insert_run(&store, "r-1", "l-1", owner_id, "active");
    store
        .conn()
        .execute_batch(
            "INSERT INTO recoveries (predecessor_run_id, origin, state, expires_at,
                 created_at, updated_at)
             VALUES ('r-1', 'provider_limit', 'pending', '2026-10-01T01:00:00.000Z',
                     '2026-10-01T00:00:00.000Z', '2026-10-01T00:00:00.000Z')",
        )
        .unwrap();
    for i in 0..40_u8 {
        insert_event(
            &store,
            &format!("ev-{i:02}"),
            &format!("run:r-1:stalled:{i}"),
            "r-1",
            "stalled",
            "{}",
        );
    }

    let mut seen = Vec::new();
    let mut recoveries = Vec::new();
    let mut cursor = None;
    let mut pages = 0_usize;
    loop {
        let page = status::page(&store, &owner, None, cursor.as_deref(), 600, &view()).unwrap();
        pages = pages.saturating_add(1);
        assert!(
            serde_json::to_string(&page).unwrap().len() <= 600,
            "every page stays under the budget"
        );
        for id in page["unreadEventIds"].as_array().unwrap() {
            seen.push(id.as_str().unwrap().to_owned());
        }
        for recovery in page["recoveries"].as_array().unwrap() {
            recoveries.push(recovery["predecessor"].as_str().unwrap().to_owned());
        }
        match page.get("nextCursor") {
            Some(next) => cursor = Some(next.as_str().unwrap().to_owned()),
            None => break,
        }
    }
    assert!(pages > 1, "the listing actually paged ({pages})");
    let expected: Vec<String> = (0..40_u8).map(|i| format!("ev-{i:02}")).collect();
    assert_eq!(seen, expected, "every unread id once, in order");
    assert_eq!(recoveries, ["r-1"], "the pending recovery reported once");
}

/// F7 — the health/config sections carry what the coordinator knows:
/// daemon liveness, the last-good tick's freshness + incarnation, and
/// the live config's validity, version, last-good time and last error.
#[test]
fn f7_status_reports_last_good_config_and_herdr_freshness() {
    let tmp = tempdir().unwrap();
    let mut store = store_in(&tmp);
    let owner = seed_caller(&mut store, RELAY_A, "kind-a", "sess-a");
    let mut view = view();
    view.config.valid = false;
    view.config.last_error = Some("decode".into());

    let page = status::page(&store, &owner, None, None, status::BYTE_BUDGET, &view).unwrap();
    assert_eq!(page["health"]["daemon"]["pid"], 4242);
    assert_eq!(page["health"]["daemon"]["uptimeSecs"], 7);
    assert_eq!(page["health"]["daemon"]["version"], "0.1.0-test");
    assert_eq!(page["health"]["herdr"]["freshSecsAgo"], 3);
    assert_eq!(page["health"]["herdr"]["incarnation"], "42:1790812800.5");
    assert_eq!(page["config"]["valid"], false);
    assert_eq!(page["config"]["version"], "cfg-digest");
    assert_eq!(page["config"]["lastGoodAt"], NOW.0);
    assert_eq!(page["config"]["lastError"], "decode");
    assert_eq!(page["runs"], serde_json::json!([]));
    assert_eq!(page["recoveries"], serde_json::json!([]));
    assert_eq!(page["unreadEventIds"], serde_json::json!([]));
    assert_eq!(page["cooldowns"], serde_json::json!([]));
    assert!(page.get("nextCursor").is_none(), "nothing paged");
}

/// F7 — `eventId` returns one event's body, but only to the caller the
/// event is addressed to; anyone else (or a nonexistent id) is `NOT_OWNER`.
#[test]
fn f7_event_body_only_to_its_destination() {
    let tmp = tempdir().unwrap();
    let mut store = store_in(&tmp);
    let owner_a = seed_caller(&mut store, RELAY_A, "kind-a", "sess-a");
    let owner_b = seed_caller(&mut store, RELAY_B, "kind-b", "sess-b");
    insert_run(&store, "r-1", "l-1", caller_id(&store, "sess-b"), "active");
    insert_event(
        &store,
        "ev-1",
        "run:r-1:settled",
        "r-1",
        "settled",
        "{\"w\":1}",
    );

    let page = status::page(
        &store,
        &owner_b,
        Some(&EventId("ev-1".into())),
        None,
        status::BYTE_BUDGET,
        &view(),
    )
    .unwrap();
    assert_eq!(page["event"]["id"], "ev-1");
    assert_eq!(page["event"]["kind"], "settled");
    assert_eq!(page["event"]["body"]["w"], 1);

    let error = status::page(
        &store,
        &owner_a,
        Some(&EventId("ev-1".into())),
        None,
        status::BYTE_BUDGET,
        &view(),
    )
    .unwrap_err();
    assert_eq!(error.code, "NOT_OWNER", "another caller's event refuses");
    let missing = status::page(
        &store,
        &owner_b,
        Some(&EventId("ev-x".into())),
        None,
        status::BYTE_BUDGET,
        &view(),
    )
    .unwrap_err();
    assert_eq!(
        missing.code, "NOT_OWNER",
        "a nonexistent id is indistinguishable — no existence oracle"
    );
}

/// F7 — 300 owned unsettled Runs traverse under the 48,000-byte budget:
/// every run exactly once, every page under the bound.
#[test]
fn f7_status_with_300_runs_traverses_without_loss_or_duplicates() {
    let tmp = tempdir().unwrap();
    let mut store = store_in(&tmp);
    let owner = seed_caller(&mut store, RELAY_A, "kind-a", "sess-a");
    let owner_id = caller_id(&store, "sess-a");
    let mut batch = String::new();
    for i in 0..300_u32 {
        write!(
            batch,
            "INSERT INTO launches (launch_id, caller_id, project_root, idempotency_key,
                 digest_version, task_digest, task_json, phase, created_at, updated_at)
             VALUES ('l-{i}', {owner_id}, '/p', 'k-{i}', 1, '{HEX}', '{TASK}', 'routed',
                     '{T0}', '{T0}');
             INSERT INTO runs (run_id, launch_id, owner_caller_id, state, child_name,
                 cwd, max_age_deadline, created_at, updated_at)
             VALUES ('r-{i}', 'l-{i}', {owner_id}, 'active', 'gov-{i}', '/p', '{T1}',
                     '{T0}', '{T0}');"
        )
        .unwrap();
    }
    store.conn().execute_batch(&batch).unwrap();

    let mut seen = std::collections::BTreeSet::new();
    let mut cursor = None;
    let mut pages = 0_usize;
    loop {
        let page = status::page(
            &store,
            &owner,
            None,
            cursor.as_deref(),
            status::BYTE_BUDGET,
            &view(),
        )
        .unwrap();
        pages = pages.saturating_add(1);
        assert!(
            serde_json::to_string(&page).unwrap().len() <= status::BYTE_BUDGET,
            "every page stays under 48,000 bytes"
        );
        for run in page["runs"].as_array().unwrap() {
            let id = run["runId"].as_str().unwrap();
            assert!(seen.insert(id.to_owned()), "duplicate run {id}");
            assert_eq!(run["state"], "active");
            assert!(run["deadlines"]["maxAgeDeadline"].is_i64());
            assert!(run["retire"].is_null(), "retire lands with C7");
        }
        match page.get("nextCursor") {
            Some(next) => cursor = Some(next.as_str().unwrap().to_owned()),
            None => break,
        }
    }
    assert_eq!(seen.len(), 300, "every run traversed, no loss");
    assert!(pages > 1, "300 runs do not fit one page ({pages})");
}

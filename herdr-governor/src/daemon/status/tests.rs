//! `status` unit tests — the §4.12 contract over a real tempdir store:
//! the fixed section order, the opaque cursor's resume (each section
//! emitted exactly once), a malformed cursor's `REQUEST_INVALID`, and the
//! serialized byte bound under a maximally loaded page.

use governor_core::config::{ConfigVersion, Provider};
use governor_core::delivery::{MailboxEvent, MailboxEventKind, MailboxSubject};
use governor_core::identity::{
    AgentKind, CallerBinding, CallerKey, DedupKey, Digest, EventId, IdempotencyKey, LaunchId,
    NativeSession, PaneId, ProjectRoot, RelayInstanceId, RunId, Timestamp,
};
use governor_core::lifecycle::{Run, State, StateChange, Transition};
use governor_core::recovery::{Cooldown, RecoveryObligation, RecoveryOrigin, RecoveryStatus};
use governor_core::task::{Launch, LaunchPhase, Task};

use serde_json::Value;

use crate::daemon::api::ToolError;
use crate::daemon::status::{self, BYTE_BUDGET, ConfigHealth, HerdrHealth, Section, StatusView};
use crate::store::Store;

const NOW: Timestamp = Timestamp(1_790_812_800_000);
const RELAY: &str = "abababababababababababababababababababab";

fn view() -> StatusView {
    StatusView {
        now: NOW,
        pid: 4242,
        uptime_secs: 7,
        version: "0.1.0-test",
        herdr: Some(HerdrHealth {
            at: Timestamp(NOW.0 - 3_000),
            incarnation: "42:1790812800.000000005".into(),
        }),
        config: ConfigHealth {
            valid: true,
            version: ConfigVersion("cfg-digest".into()),
            last_good_at: NOW,
            last_error: None,
        },
    }
}

fn bind(store: &mut Store) -> CallerKey {
    let caller = CallerKey {
        agent_kind: AgentKind("kind-a".into()),
        native_session: NativeSession("sess-a".into()),
    };
    store
        .apply(
            &Transition {
                state_changes: vec![StateChange::BindCaller(CallerBinding {
                    caller: caller.clone(),
                    relay_instance: RelayInstanceId(RELAY.into()),
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

/// Lifecycle writes ride `store::apply` — the only public write path
/// (I10 keeps raw lifecycle SQL inside `store/transitions`).
fn seed(store: &mut Store, changes: Vec<StateChange>, events: Vec<MailboxEvent>) {
    store
        .apply(
            &Transition {
                state_changes: changes,
                events,
                effects: Vec::new(),
            },
            NOW,
        )
        .expect("seed");
}

fn launch_row(id: &str, caller: &CallerKey, phase: LaunchPhase) -> Launch {
    Launch {
        id: LaunchId(id.into()),
        caller: caller.clone(),
        project_root: ProjectRoot("/p".into()),
        idempotency_key: IdempotencyKey(id.into()),
        digest_version: 1,
        task_digest: Digest([0xcd; 32]),
        task: Task {
            objective: "o".into(),
            scope: "s".into(),
            done_when: vec!["d".into()],
            constraints: Vec::new(),
            tier: None,
            recovery_of: None,
            label: None,
            cwd: None,
        },
        phase,
        decision: None,
        config_version: None,
        outcome: None,
    }
}

fn run_row(id: &str, launch: &str, caller: &CallerKey) -> Run {
    Run {
        id: RunId(id.into()),
        launch: LaunchId(launch.into()),
        owner: caller.clone(),
        owner_generation: 0,
        version: 0,
        state: State::Active,
        prompt_certainty: None,
        child_name: format!("gov-{id}"),
        identity: None,
        operating_point: None,
        provider: None,
        tier_start: None,
        cwd: "/p".into(),
        base_commit: None,
        work_generation: 0,
        evidence_generation: 0,
        evidence_digest: None,
        child_status: None,
        idle_since: None,
        idle_deadline: None,
        repair_deadline: None,
        rejected_at: None,
        judgment_deadline: None,
        judging_digest: None,
        max_age_deadline: Timestamp(NOW.0 + 86_400_000),
        nudge_episode: 0,
        nudged_episode: None,
        blocked_episode: 0,
        settlement: None,
        settled_at: None,
    }
}

/// The launch records `evaluating` then `routed` + `ReserveRun` — the
/// routing write is a phase transition, not a blind upsert.
fn seed_run(store: &mut Store, caller: &CallerKey, run: &str, launch: &str) {
    seed(
        store,
        vec![StateChange::RecordLaunch(launch_row(
            launch,
            caller,
            LaunchPhase::Evaluating,
        ))],
        Vec::new(),
    );
    seed(
        store,
        vec![
            StateChange::RecordLaunch(launch_row(launch, caller, LaunchPhase::Routed)),
            StateChange::ReserveRun(run_row(run, launch, caller)),
        ],
        Vec::new(),
    );
}

fn seed_recovery(store: &mut Store, predecessor: &str) {
    seed(
        store,
        vec![StateChange::RecordRecovery(RecoveryObligation {
            predecessor: RunId(predecessor.into()),
            origin: RecoveryOrigin::ProviderLimit,
            status: RecoveryStatus::Pending,
            reason: None,
            successor_launch: None,
            expires_at: Timestamp(NOW.0 + 3_600_000),
        })],
        Vec::new(),
    );
}

fn seed_event(store: &mut Store, event: &str, run: &str) {
    seed(
        store,
        Vec::new(),
        vec![MailboxEvent {
            id: EventId(event.into()),
            dedup_key: DedupKey(format!("dedup-{event}")),
            subject: MailboxSubject::Run(RunId(run.into())),
            kind: MailboxEventKind::Stalled,
            body: "{}".into(),
        }],
    );
}

fn seed_cooldown(store: &mut Store, provider: &str) {
    seed(
        store,
        vec![StateChange::SetCooldown(Cooldown {
            provider: Provider(provider.into()),
            until: Timestamp(NOW.0 + 3_600_000),
            reason: "provider_limited".into(),
            source_run: None,
        })],
        Vec::new(),
    );
}

fn page_len(page: &Value) -> usize {
    serde_json::to_vec(page).map_or(usize::MAX, |bytes| bytes.len())
}

/// §4.12 — the traversal emits sections in the fixed order runs →
/// recoveries → unreadEventIds → cooldowns, each exactly once; a budget
/// that forces a cut per item still resumes correctly at the next key.
#[test]
fn status_page_sections_fixed_order() {
    let tmp = tempfile::tempdir().expect("tmp");
    let mut store = Store::open(&tmp.path().join("governor.db")).expect("store");
    let caller = bind(&mut store);
    seed_run(&mut store, &caller, "r-1", "l-1");
    seed_run(&mut store, &caller, "r-2", "l-2");
    seed_recovery(&mut store, "r-1");
    seed_event(&mut store, "ev-1", "r-1");
    seed_cooldown(&mut store, "prov-a");

    // The skeleton is ~280 bytes and a run item ~85: a 440-byte budget
    // cuts inside `runs` after the first row, so the sections land on
    // separate pages in their fixed order.
    let mut cursor: Option<String> = None;
    let mut seen_runs = 0_usize;
    let mut seen_recoveries = 0_usize;
    let mut seen_events = 0_usize;
    let mut seen_cooldowns = 0_usize;
    let mut order = Vec::new();
    for _guard in 0..16_u8 {
        let page =
            status::page(&store, &caller, None, cursor.as_deref(), 440, &view()).expect("page");
        let before = (seen_runs, seen_recoveries, seen_events, seen_cooldowns);
        seen_runs = seen_runs.saturating_add(page["runs"].as_array().map_or(0, Vec::len));
        seen_recoveries =
            seen_recoveries.saturating_add(page["recoveries"].as_array().map_or(0, Vec::len));
        seen_events =
            seen_events.saturating_add(page["unreadEventIds"].as_array().map_or(0, Vec::len));
        seen_cooldowns =
            seen_cooldowns.saturating_add(page["cooldowns"].as_array().map_or(0, Vec::len));
        let now = (seen_runs, seen_recoveries, seen_events, seen_cooldowns);
        if now != before {
            order.push(now);
        }
        match page.get("nextCursor") {
            Some(next) => cursor = Some(next.as_str().expect("string cursor").to_owned()),
            None => break,
        }
    }
    assert_eq!(
        (seen_runs, seen_recoveries, seen_events, seen_cooldowns),
        (2, 1, 1, 1),
        "every item traversed exactly once"
    );
    // The first page emits runs, then each section lands in order —
    // the monotone counts can only grow left-to-right.
    let first_emit = order.first().expect("items emitted");
    assert!(
        first_emit.0 > 0 && first_emit.1 == 0 && first_emit.2 == 0 && first_emit.3 == 0,
        "runs emit before the later sections: {order:?}"
    );
}

/// §4.12 — a resume inside a section continues strictly after the
/// recorded key; a malformed cursor is `REQUEST_INVALID` (never silently
/// rewound, never a panic).
#[test]
fn status_cursor_resume_and_malformed() {
    let tmp = tempfile::tempdir().expect("tmp");
    let mut store = Store::open(&tmp.path().join("governor.db")).expect("store");
    let caller = bind(&mut store);
    for index in 0..6_u8 {
        seed_run(
            &mut store,
            &caller,
            &format!("r-{index}"),
            &format!("l-{index}"),
        );
    }
    let bad = status::page(
        &store,
        &caller,
        None,
        Some("not-a-cursor!!"),
        BYTE_BUDGET,
        &view(),
    )
    .expect_err("a malformed cursor refuses");
    assert_eq!(
        bad.code,
        ToolError::REQUEST_INVALID,
        "bad cursors refuse, they do not rewind"
    );

    // A hand-built cursor naming run 4 → the page resumes strictly after.
    let cursor = status::encode_cursor(Section::Runs, "r-4");
    let page =
        status::page(&store, &caller, None, Some(&cursor), BYTE_BUDGET, &view()).expect("page");
    let runs = page["runs"].as_array().expect("runs");
    assert_eq!(
        runs.first().and_then(|run| run["runId"].as_str()),
        Some("r-5"),
        "the resume key is strictly-after"
    );
    assert!(
        page.get("nextCursor").is_none(),
        "the traversal finished after the last key"
    );
}

/// §4.12 — the largest legal page: a 64-provider cooldown set on 64-byte
/// names plus a full PAGE of runs, events and a recovery behind them —
/// no page of the traversal exceeds `BYTE_BUDGET`, and every item still
/// lands exactly once.
#[test]
fn status_page_never_exceeds_byte_budget() {
    let tmp = tempfile::tempdir().expect("tmp");
    let mut store = Store::open(&tmp.path().join("governor.db")).expect("store");
    let caller = bind(&mut store);
    for index in 0..64_u8 {
        seed_cooldown(&mut store, &format!("prov-{index:02}-{}", "p".repeat(58)));
    }
    for index in 0..200_u32 {
        let (run, launch) = (format!("r-{index:03}"), format!("l-{index:03}"));
        seed_run(&mut store, &caller, &run, &launch);
        seed_event(&mut store, &format!("ev-{index:03}"), &run);
    }
    seed_recovery(&mut store, "r-000");

    let mut totals = (0_usize, 0_usize, 0_usize, 0_usize);
    let mut cursor: Option<String> = None;
    for _guard in 0..32_u8 {
        let page = status::page(
            &store,
            &caller,
            None,
            cursor.as_deref(),
            BYTE_BUDGET,
            &view(),
        )
        .expect("page");
        assert!(
            page_len(&page) <= BYTE_BUDGET,
            "every page stays under {BYTE_BUDGET} bytes (was {})",
            page_len(&page)
        );
        totals.0 = totals
            .0
            .saturating_add(page["runs"].as_array().map_or(0, Vec::len));
        totals.1 = totals
            .1
            .saturating_add(page["recoveries"].as_array().map_or(0, Vec::len));
        totals.2 = totals
            .2
            .saturating_add(page["unreadEventIds"].as_array().map_or(0, Vec::len));
        totals.3 = totals
            .3
            .saturating_add(page["cooldowns"].as_array().map_or(0, Vec::len));
        match page.get("nextCursor") {
            Some(next) => cursor = Some(next.as_str().expect("string cursor").to_owned()),
            None => break,
        }
    }
    assert_eq!(
        totals,
        (200, 1, 200, 64),
        "the maximal load traversed losslessly"
    );
}

/// The rev1 name for the same bound — every page of a small-budget
/// traversal stays under it while the cursor walks all four sections
/// (the events section supplies the mid-section cut).
#[test]
fn status_pages_under_bound() {
    let tmp = tempfile::tempdir().expect("tmp");
    let mut store = Store::open(&tmp.path().join("governor.db")).expect("store");
    let caller = bind(&mut store);
    seed_run(&mut store, &caller, "r-1", "l-1");
    seed_recovery(&mut store, "r-1");
    for index in 0..10_u8 {
        seed_event(&mut store, &format!("ev-{index:02}"), "r-1");
    }
    seed_cooldown(&mut store, "prov-a");

    let mut cursor: Option<String> = None;
    let mut emitted = 0_usize;
    let mut pages = 0_usize;
    for _guard in 0..16_u8 {
        let page =
            status::page(&store, &caller, None, cursor.as_deref(), 500, &view()).expect("page");
        pages = pages.saturating_add(1);
        assert!(
            page_len(&page) <= 500,
            "the page stays under its budget (was {})",
            page_len(&page)
        );
        emitted = emitted
            .saturating_add(page["runs"].as_array().map_or(0, Vec::len))
            .saturating_add(page["recoveries"].as_array().map_or(0, Vec::len))
            .saturating_add(page["unreadEventIds"].as_array().map_or(0, Vec::len))
            .saturating_add(page["cooldowns"].as_array().map_or(0, Vec::len));
        match page.get("nextCursor") {
            Some(next) => cursor = Some(next.as_str().expect("string cursor").to_owned()),
            None => break,
        }
    }
    assert!(pages > 1, "the traversal paged ({pages})");
    assert_eq!(emitted, 13, "every section's items emitted once");
}

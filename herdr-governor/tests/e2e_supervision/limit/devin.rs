//! `devin` — the process-log provider-limit cases (§5 P1/P2/P3/P5/P7/P9
//! and the OQ-X stated-reset cooldown extension): the child runs
//! `devin`, its native evidence is a `devin_<stamp>_<pid>.log` under
//! `devin_log_dir`, and the committed fixture is the real stall line.

use governor_core::delivery::MailboxEventKind;
use governor_core::identity::{RunId, Timestamp};
use governor_core::lifecycle::{EffectState, Settlement, State};
use governor_core::recovery::{RecoveryOrigin, RecoveryStatus};

use super::world::{Opts, QUIET, SESSION, answers, caller, fixture_bytes, open_store, world};
use crate::support::daemon::{await_for, never};

/// P1 — the `idle` Devin child's session-bound stall line produces one
/// `limit:*` ask; Jev's `yes` settles `provider_limited` (cooldown row,
/// pending obligation, `recovery_pending`) and the pane is never closed.
#[tokio::test]
async fn p1_session_bound_limit_line_settles_provider_limited() {
    let w = world(&Opts::default());
    w.stage_log("devin_20261002-013259_4242.log", &fixture_bytes());
    w.jev.push_answers(answers(0.9));
    let daemon = w.start().await;
    await_for("the limit ask", || w.limit_rows().len() == 1).await;
    await_for("the ask on the wire", || !w.limit_asks().is_empty()).await;
    let [ask] = w.limit_asks().try_into().expect("one ask");
    let record = &ask["blocked"]["limitRecord"];
    assert_eq!(record["source"], "devin_process_log");
    assert_eq!(record["observedAt"], "2026-10-02T01:33:04.421Z");
    assert_eq!(record["resetAt"], "2026-10-02T01:59:04.421Z");

    await_for("the provider_limited settlement", || {
        w.run().settlement == Some(Settlement::ProviderLimited)
    })
    .await;
    assert_eq!(w.run().state, State::Settled);
    // The policy cooldown was written (`cooldown_secs = 60`; the fixture's
    // stated reset predates it — the OQ-X leg covers the longer one).
    let store = open_store(&w.dirs);
    let cooldowns = store.cooldowns().expect("cooldowns");
    let [cooldown] = cooldowns.try_into().expect("one cooldown");
    assert_eq!(cooldown.provider.0, "vendor-b");
    assert_eq!(cooldown.source_run.as_ref(), Some(&RunId("r1".into())));
    assert!(
        cooldown.until.0 > 1_790_812_800_000_i64,
        "a live window: {cooldown:?}"
    );
    let pending = store
        .recoveries_by_state(RecoveryStatus::Pending)
        .expect("recoveries");
    let [obligation] = pending.try_into().expect("one obligation");
    assert_eq!(obligation.predecessor, RunId("r1".into()));
    assert_eq!(obligation.origin, RecoveryOrigin::ProviderLimit);
    let kinds: Vec<MailboxEventKind> = store
        .mailbox_unacked(&caller(), None, 100)
        .expect("mailbox")
        .iter()
        .map(|event| event.kind)
        .collect();
    assert!(
        kinds.contains(&MailboxEventKind::RecoveryPending),
        "recovery_pending reached the owner: {kinds:?}"
    );
    drop(store);
    never("no pane.close", QUIET, || w.fake.saw("pane.close")).await;
    daemon.shutdown().await;
}

/// OQ-X (§13) — the stated reset outlasts the policy window: the
/// cooldown's `until` is the record's `reset_at`, not `now + cooldown`.
#[tokio::test]
async fn stated_reset_past_the_policy_window_extends_the_cooldown() {
    let w = world(&Opts::default());
    // 2040-01-01T00:00Z stall + "reset in 60 minutes" → reset_at
    // 2040-01-01T01:00:00.421Z, far past `now + cooldown_secs = 60`.
    w.stage_log(
        "devin_20400101-000000_4242.log",
        b"2040-01-01T00:00:00.000000Z  INFO run_acp_server:session_db_call:session_db_job: chisel_agent::session_db: Created new session: tidal-vase\n2040-01-01T00:00:00.421266Z ERROR affogato::agent::control_loop: attempts=3 error=Inference(ServerError(message=Reached free model rate limit. Upgrade to Max for higher limits, or switch to a different model. Your limit will reset in 60 minutes. (trace ID: 0))) Exhausted inference retries; stopping turn\n",
    );
    w.jev.push_answers(answers(0.9));
    let daemon = w.start().await;
    await_for("the settlement", || {
        w.run().settlement == Some(Settlement::ProviderLimited)
    })
    .await;
    let cooldowns = open_store(&w.dirs).cooldowns().expect("cooldowns");
    let [cooldown] = cooldowns.try_into().expect("one cooldown");
    assert_eq!(
        cooldown.until,
        Timestamp(2_208_992_400_421),
        "the stated reset wins: {cooldown:?}"
    );
    daemon.shutdown().await;
}

/// P2 — a stall line whose log binds a different session is foreign: no
/// `limit:` row is ever planned.
#[tokio::test]
async fn p2_limit_line_naming_another_session_plans_no_ask() {
    let w = world(&Opts::default());
    let foreign = String::from_utf8(fixture_bytes())
        .expect("utf8")
        .replace(SESSION, "other-pane");
    w.stage_log("devin_20261002-013259_4242.log", foreign.as_bytes());
    w.jev.push_answers(answers(0.1));
    let daemon = w.start().await;
    await_for("the first pass", || w.run().evidence_generation == 1).await;
    never("no limit ask for a foreign session", QUIET, || {
        !w.limit_rows().is_empty()
    })
    .await;
    daemon.shutdown().await;
}

/// P3 — a world-writable log is untrusted: the pass never reads it.
#[tokio::test]
async fn p3_world_writable_log_is_untrusted() {
    let w = world(&Opts::default());
    let path = w.stage_log("devin_20261002-013259_4242.log", &fixture_bytes());
    std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o666))
        .expect("chmod");
    w.jev.push_answers(answers(0.1));
    let daemon = w.start().await;
    await_for("the first pass", || w.run().evidence_generation == 1).await;
    never("no limit ask from an untrusted log", QUIET, || {
        !w.limit_rows().is_empty()
    })
    .await;
    daemon.shutdown().await;
}

/// P5 — Jev `no` leaves the Run active and the answered record never
/// re-asks; a *new* record (newer log, new `observed_at`) asks again.
#[tokio::test]
async fn p5_answered_no_never_reasks_a_new_record_asks_again() {
    let w = world(&Opts {
        status: "working",
        ..Opts::default()
    });
    w.stage_log("devin_20261002-013259_4242.log", &fixture_bytes());
    w.jev.push_answers(answers(0.1));
    let daemon = w.start().await;
    await_for("the first ask answered", || {
        w.limit_rows()
            .iter()
            .any(|effect| effect.state == EffectState::Acknowledged)
    })
    .await;
    assert_eq!(w.run().state, State::Active, "a `no` settles nothing");
    never("the answered record never re-asks", QUIET, || {
        w.limit_rows().len() > 1
    })
    .await;

    // A second bound log with a later stall line is a new record.
    let later = String::from_utf8(fixture_bytes())
        .expect("utf8")
        .replace("01:33:0", "02:00:0");
    w.stage_log("devin_20261002-015900_4243.log", later.as_bytes());
    await_for("the new record's ask", || w.limit_rows().len() == 2).await;
    assert!(
        w.limit_rows()
            .iter()
            .any(|effect| effect.key.0 == "run:r1:limit:devin_process_log:1790906404421"),
        "the new record keyed its own ask: {:?}",
        w.limit_rows()
            .iter()
            .map(|effect| effect.key.0.clone())
            .collect::<Vec<_>>()
    );
    daemon.shutdown().await;
}

/// P6 — the record lands while Herdr reports `blocked`: the episode's
/// `blocked:<ep>` ask already carries `limitRecord`, so the `limit:`
/// family plans nothing — one ask, one answer, settle.
#[tokio::test]
async fn p6_blocked_episode_ask_carries_the_record_once() {
    let w = world(&Opts {
        status: "blocked",
        ..Opts::default()
    });
    w.stage_log("devin_20261002-013259_4242.log", &fixture_bytes());
    w.jev.push_answers(answers(0.9));
    let daemon = w.start().await;
    await_for("the episode's ask", || !w.limit_asks().is_empty()).await;
    await_for("the settlement", || {
        w.run().settlement == Some(Settlement::ProviderLimited)
    })
    .await;
    assert_eq!(
        w.jev
            .requests()
            .iter()
            .filter(|request| request.state().is_some_and(|s| s.get("blocked").is_some()))
            .count(),
        1,
        "one blocked-state ask carried the record"
    );
    never("the limit family never asks", QUIET, || {
        !w.limit_rows().is_empty()
    })
    .await;
    daemon.shutdown().await;
}

/// P7 — the same phrase inside the Devin ATIF transcript is task output,
/// not a provider record: the transcript is never the source.
#[tokio::test]
async fn p7_the_phrase_in_the_transcript_is_not_a_record() {
    let w = world(&Opts::default());
    std::fs::write(
        &w.transcript,
        "{\"schema_version\":\"ATIF-v1.7\",\"session_id\":\"tidal-vase\",\"steps\":[{\"step_id\":1,\"source\":\"agent\",\"message\":\"stderr shows Reached free model rate limit; switching models\"}]}",
    )
    .expect("atif doc");
    w.jev.push_answers(answers(0.1));
    let daemon = w.start().await;
    await_for("the first pass", || w.run().evidence_generation == 1).await;
    never("a transcript phrase is no record", QUIET, || {
        !w.limit_rows().is_empty()
    })
    .await;
    daemon.shutdown().await;
}

/// P9 — a `.log.gz`-only naming produces no verdict and no ask; the
/// fixture copied as `.log` produces the ask.
#[tokio::test]
async fn p9_gz_archive_is_no_verdict_the_plain_log_asks() {
    let w = world(&Opts::default());
    w.stage_log("devin_20261002-013259_4242.log.gz", &fixture_bytes());
    w.jev.push_answers(answers(0.1));
    let daemon = w.start().await;
    await_for("the first pass", || w.run().evidence_generation == 1).await;
    never("the archive is never read", QUIET, || {
        !w.limit_rows().is_empty()
    })
    .await;
    w.stage_log("devin_20261002-013259_4242.log", &fixture_bytes());
    await_for("the plain log's ask", || w.limit_rows().len() == 1).await;
    daemon.shutdown().await;
}

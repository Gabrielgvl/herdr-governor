//! `cancel` — F20 over the wire: an unsettled Run settles `cancelled`
//! in the same transition, `closePane` plans the one verified
//! `run:<id>:close` and parks the reply behind its confirmation, and a
//! settled Run takes only the pane close. The literal
//! `herdr_run{cancel}` S31b: a cancel that lands while `prompt:task`'s
//! pre-dispatch `session.snapshot` is still on the wire makes
//! `DispatchCommit` skip — the prompt stays `planned` and
//! `agent.prompt` is never written.

use std::time::Duration;

use governor_core::identity::RunId;
use governor_core::lifecycle::{EffectState, Settlement, State};
use governor_core::task::LaunchPhase;
use serde_json::json;

use crate::support::daemon::{await_for, never};
use crate::support::fake_herdr::Fault;

use super::*;

/// A `cancel` args object.
fn cancel_args(run: &str, close_pane: bool) -> serde_json::Value {
    json!({"action": "cancel", "runId": run, "closePane": close_pane})
}

/// F20 — an identity-less Run settles `cancelled` with nothing to
/// close (no `close` member at all); a foreign or missing Run is
/// `NOT_OWNER`. The settlement is the immutable first write — a second
/// cancel without `closePane` changes nothing and reports the same
/// settlement.
#[tokio::test]
async fn f20_cancel_settles_once_and_reports() {
    let mut world = World::new(|_catalog| {});
    world.start().await;
    {
        let mut store = world.store();
        bind_caller(&mut store);
        seed_run(
            &mut store,
            &run_row("r1", "l-r1", State::Active),
            &launch_row("l-r1", LaunchPhase::Routed),
        );
    }

    let reply = world.run_call(&cancel_args("r1", false)).await;
    assert_eq!(
        tool_body(&reply),
        json!({"runId": "r1", "settlement": "cancelled", "settlementReason": null}),
        "{reply}"
    );
    let run = world
        .store()
        .run(&RunId("r1".into()))
        .expect("read")
        .expect("r1");
    assert_eq!(run.state, State::Settled);
    assert_eq!(run.settlement, Some(Settlement::Cancelled));

    // The repeat is a no-op carrying the same settlement — `settled`
    // takes nothing but the pane close.
    let again = world.run_call(&cancel_args("r1", false)).await;
    assert_eq!(tool_body(&again)["settlement"], "cancelled", "{again}");

    let missing = world.run_call(&cancel_args("r-missing", false)).await;
    assert_eq!(tool_code(&missing), "NOT_OWNER", "{missing}");
    let foreign = world.client_for(FOREIGN_PANE, FOREIGN_RELAY, world.project());
    let outsider = foreign
        .call_tool(json!(1), "herdr_run", cancel_args("r1", false))
        .await;
    assert_eq!(tool_code(&outsider), "NOT_OWNER", "{outsider}");
    world.shutdown().await;
}

/// F20/F10 — `closePane` on a Run with a captured child plans the one
/// verified `run:<id>:close`; the reply parks until the close's own
/// commit drains it — `confirmed` once `pane.close` is acknowledged.
/// A settled Run with `closePane` closes the same way, and a second
/// `closePane` cancel reports the already-journaled close.
#[tokio::test]
async fn f20_cancel_close_pane_parks_until_the_verified_close_commits() {
    let mut world = World::new(|_catalog| {});
    world.start().await;
    {
        let mut store = world.store();
        bind_caller(&mut store);
        let mut run = run_row("r1", "l-r1", State::Active);
        run.identity = Some(child_identity(
            world.fake(),
            world.child_terminal(),
            "gov-r1",
        ));
        seed_run(&mut store, &run, &launch_row("l-r1", LaunchPhase::Routed));
    }

    let reply = world.run_call(&cancel_args("r1", true)).await;
    let body = tool_body(&reply);
    assert_eq!(body["settlement"], "cancelled", "{body}");
    assert_eq!(
        body["close"],
        json!({"key": "run:r1:close", "state": "acknowledged", "confirmed": true}),
        "the parked reply drains on the close's own commit: {body}"
    );
    assert!(
        saw_wire(world.fake(), "pane.close"),
        "the verified close reached the wire"
    );

    // A settled Run + `closePane` is the only legal second move — but
    // the close is already journaled, so the reply reports it.
    let repeat = world.run_call(&cancel_args("r1", true)).await;
    let second = tool_body(&repeat);
    assert_eq!(second["settlement"], "cancelled", "{second}");
    assert_eq!(second["close"]["confirmed"], true, "{second}");
    let closes = wire_calls(world.fake(), "pane.close");
    assert_eq!(closes.len(), 1, "the close deduped — one wire call");
    world.shutdown().await;
}

/// F20/S31b — the literal `herdr_run{cancel}`: the cancel lands while
/// `prompt:task`'s pre-dispatch `session.snapshot` is still on the
/// wire; `DispatchCommit` re-reads the Run, sees the settlement, and
/// skips — the row stays `planned`, `dispatched_at` unset, and
/// `agent.prompt` is never written.
#[tokio::test]
async fn f20_run_cancel_during_pre_dispatch_snapshot_never_sends_the_prompt() {
    let mut world = World::new(|catalog| {
        catalog.points_toml = point("op-a", 0, "vendor-a", "--a");
        catalog.daemon_extra = "launch_wait_secs = 15\n".to_owned();
        // An inert tick: no reconcile snapshot can consume the armed
        // delay before the prompt's own verification does.
        catalog.reconcile_secs = 3600;
    });
    world.jev().push_answers(launch_eval("new"));
    world.start().await;
    qualify_start(&mut world.store(), "op-a", &["--a"]);
    // Hold the start ack long enough to arm the snapshot delay after
    // the request-time and tab-leg snapshots have passed — the next
    // `session.snapshot` on the wire is the task prompt's verification.
    world
        .fake()
        .fault("agent.start", Fault::Delay(Duration::from_millis(2_500)));

    let call = world.spawn_launch(&launch_args(&task(&[]), "k1"));
    await_for("the start on the wire", || {
        saw_wire(world.fake(), "agent.start")
    })
    .await;
    world
        .fake()
        .fault("session.snapshot", Fault::Delay(Duration::from_secs(5)));
    let snapshots = wire_calls(world.fake(), "session.snapshot").len();
    await_for("the prompt's verification in flight", || {
        wire_calls(world.fake(), "session.snapshot").len() > snapshots
    })
    .await;

    let run = run_for(&world.store(), &only_launch(&world.store()));
    let cancel = world.run_call(&cancel_args(&run.id.0, false)).await;
    assert_eq!(
        tool_body(&cancel)["settlement"],
        "cancelled",
        "the literal herdr_run{{cancel}}: {cancel}"
    );
    assert_eq!(
        tool_body(&call.await.expect("call joins"))["outcome"],
        "launched",
        "the start ack already answered"
    );

    // The delayed verify lands, the commit asks, the settled gate
    // skips: five seconds of delay plus margin, and the prompt is
    // never sent.
    never(
        "a prompt after the skipped commit",
        Duration::from_secs(4),
        || saw_wire(world.fake(), "agent.prompt"),
    )
    .await;
    let store = world.store();
    let prompt = effect_at(&store, &run_key(&run, "prompt:task"));
    assert_eq!(prompt.state, EffectState::Planned, "the commit skipped");
    assert_eq!(prompt.dispatched_at, None, "never marked dispatching");
    let settled = run_for(&store, &only_launch(&store));
    assert_eq!(settled.state, State::Settled);
    assert_eq!(settled.settlement, Some(Settlement::Cancelled));
    world.shutdown().await;
}

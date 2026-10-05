//! `ownership` — F4's `handover` and F19's `adopt` over the wire: the
//! request-time snapshot resolves the successor and proves the prior
//! owner gone, the named set moves atomically with `owner_generation`
//! bumped and reported, and every refusal — foreign, missing, a Run's
//! own child, a still-live prior owner — keeps the rows unmoved.

use governor_core::delivery::MailboxEventKind;
use governor_core::identity::RunId;
use governor_core::lifecycle::{Settlement, State, StateChange};
use governor_core::task::LaunchPhase;
use serde_json::json;

use super::*;

/// A `handover` args object for `runs` to `pane`.
fn handover_args(runs: &[&str], pane: &str) -> serde_json::Value {
    json!({
        "action": "handover",
        "runIds": runs,
        "successorPaneId": pane,
    })
}

/// An `adopt` args object.
fn adopt_args(runs: &[&str]) -> serde_json::Value {
    json!({"action": "adopt", "runIds": runs})
}

/// The current `run.owner` — the assertion every move lands on.
fn owner_of(world: &World, run: &str) -> governor_core::identity::CallerKey {
    world
        .store()
        .run(&RunId(run.into()))
        .expect("run read")
        .expect("the seeded run")
        .owner
}

/// A world with an `active` Run `r1` owned by the caller.
fn owned_world() -> World {
    let world = World::new(|_catalog| {});
    let mut store = world.store();
    bind_caller(&mut store);
    seed_run(
        &mut store,
        &run_row("r1", "l-r1", State::Active),
        &launch_row("l-r1", LaunchPhase::Routed),
    );
    world
}

/// Bind `pane`'s caller through its own first tool call — a refused
/// `observe` still rides `resolve_and_bind`, so the successor's
/// `callers` row exists for the `ChangeOwner` FK.
async fn bind_pane(world: &World, pane: &str) {
    let client = world.client_for(pane, pane_relay(pane), world.project());
    let reply = client
        .call_tool(
            json!(1),
            "herdr_run",
            json!({"action": "observe", "runId": "r-missing"}),
        )
        .await;
    assert_eq!(tool_code(&reply), "NOT_OWNER", "{reply}");
}

/// F4/F19 — `handover` moves the named set to the verified successor
/// atomically and reports each Run's bumped `owner_generation`; the
/// new owner's actions resolve and the prior owner's no longer do.
#[tokio::test]
async fn f4_handover_moves_the_set_and_reports_generations() {
    let mut world = owned_world();
    world.start().await;
    let mut store = world.store();
    seed_run(
        &mut store,
        &run_row("r2", "l-r2", State::Active),
        &launch_row("l-r2", LaunchPhase::Routed),
    );
    bind_pane(&world, SUCC_PANE).await;

    let reply = world
        .run_call(&handover_args(&["r1", "r2"], SUCC_PANE))
        .await;
    let body = tool_body(&reply);
    assert_eq!(
        body,
        json!({
            "runs": [
                {"runId": "r1", "ownerGeneration": 1},
                {"runId": "r2", "ownerGeneration": 1},
            ],
        }),
        "{body}"
    );
    assert_eq!(owner_of(&world, "r1"), succ_key());
    assert_eq!(owner_of(&world, "r2"), succ_key());

    // The new owner reads what the old owner no longer can.
    let succ = world.client_for(SUCC_PANE, SUCC_RELAY, world.project());
    let inherited = succ
        .call_tool(
            json!(1),
            "herdr_run",
            json!({"action": "observe", "runId": "r1"}),
        )
        .await;
    assert_eq!(
        tool_body(&inherited)["run"]["runId"],
        "r1",
        "the successor owns it now: {inherited}"
    );
    let former = world
        .run_call(&json!({"action": "observe", "runId": "r1"}))
        .await;
    assert_eq!(tool_code(&former), "NOT_OWNER", "{former}");
    world.shutdown().await;
}

/// F4 — the set moves atomically or not at all: one refusal in the
/// named set (a foreign Run, a missing one, an unresolvable successor)
/// answers and writes nothing. A Run is never handed to its own child
/// (`CALLER_IS_RUN`, H#24).
#[tokio::test]
async fn f4_handover_refusals_move_nothing() {
    let mut world = owned_world();
    world.start().await;
    bind_pane(&world, SUCC_PANE).await;
    {
        let mut store = world.store();
        seed(
            &mut store,
            vec![bind(&foreign_key(), FOREIGN_RELAY, FOREIGN_PANE)],
            Vec::new(),
        );
        let mut foreign_run = run_row("r-for", "l-for", State::Active);
        foreign_run.owner = foreign_key();
        seed_run(
            &mut store,
            &foreign_run,
            &launch_row("l-for", LaunchPhase::Routed),
        );
    }

    // A foreign member poisons the set — nothing moves, not even r1.
    let poisoned = world
        .run_call(&handover_args(&["r1", "r-for"], SUCC_PANE))
        .await;
    assert_eq!(tool_code(&poisoned), "NOT_OWNER", "{poisoned}");
    assert_eq!(owner_of(&world, "r1"), caller_key(), "atomic: unmoved");

    let missing = world
        .run_call(&handover_args(&["r-missing"], SUCC_PANE))
        .await;
    assert_eq!(tool_code(&missing), "NOT_OWNER", "{missing}");

    let unresolvable = world.run_call(&handover_args(&["r1"], "w1:p9")).await;
    assert_eq!(
        tool_code(&unresolvable),
        "CALLER_IDENTITY_MISSING",
        "{unresolvable}"
    );

    // The caller itself hands off — to its own supervised child on
    // `w1:p2`, the `CALLER_IS_RUN` lane (the child resolves to the
    // captured identity's own session).
    {
        let mut store = world.store();
        let mut owned = world
            .store()
            .run(&RunId("r1".into()))
            .expect("read")
            .expect("r1");
        owned.identity = Some(child_identity(
            world.fake(),
            world.child_terminal(),
            "gov-r1",
        ));
        store
            .apply(
                &changes(vec![StateChange::UpdateRun(
                    governor_core::lifecycle::RunUpdate {
                        expected_version: owned.version,
                        record: owned,
                    },
                )]),
                NOW,
            )
            .expect("capture the child identity");
    }
    bind_pane(&world, CHILD_PANE).await;
    let self_deal = world.run_call(&handover_args(&["r1"], CHILD_PANE)).await;
    assert_eq!(tool_code(&self_deal), "CALLER_IS_RUN", "{self_deal}");
    assert_eq!(owner_of(&world, "r1"), caller_key());
    world.shutdown().await;
}

/// F19 — `adopt` claims Runs whose owner session is provably gone on
/// the request-time snapshot: `ADOPT_OWNER_LIVE` while it lives, the
/// claim once it exits. A settled Run is claimable only for unread
/// events or a pending recovery — and never reopens.
#[tokio::test]
async fn f19_adopt_requires_a_gone_owner_and_keeps_settled_settled() {
    let mut world = World::new(|_catalog| {});
    world.start().await;
    {
        let mut store = world.store();
        bind_caller(&mut store);
        seed(
            &mut store,
            vec![bind(&foreign_key(), FOREIGN_RELAY, FOREIGN_PANE)],
            Vec::new(),
        );
        // An active Run the foreign caller owns — its `sess-foreign`
        // session is live on `w1:p4`.
        let mut live = run_row("r-foreign", "l-foreign", State::Active);
        live.owner = foreign_key();
        seed_run(
            &mut store,
            &live,
            &launch_row("l-foreign", LaunchPhase::Routed),
        );
        // A settled foreign-owned Run with nothing unread.
        let mut settled = run_row("r-done", "l-done", State::Settled);
        settled.owner = foreign_key();
        settled.settlement = Some(Settlement::Cancelled);
        settled.settled_at = Some(NOW);
        let mut writes = launch_chain(&done_launch("l-done", failed_outcome("r-done")));
        writes.push(StateChange::ReserveRun(settled));
        seed(&mut store, writes, Vec::new());
    }

    // The owner's session is still on the wire — no claim.
    let live = world.run_call(&adopt_args(&["r-foreign"])).await;
    assert_eq!(tool_code(&live), "ADOPT_OWNER_LIVE", "{live}");
    // Owning it already is no claim either — the caller's own session
    // is the live owner.
    {
        let mut store = world.store();
        seed_run(
            &mut store,
            &run_row("r-mine", "l-mine", State::Active),
            &launch_row("l-mine", LaunchPhase::Routed),
        );
    }
    let own = world.run_call(&adopt_args(&["r-mine"])).await;
    assert_eq!(tool_code(&own), "ADOPT_OWNER_LIVE", "{own}");

    // The occupant exits: the fresh snapshot no longer carries
    // `sess-foreign`, and the claim lands.
    world.fake().agent_exit(FOREIGN_PANE);
    let claimed = world.run_call(&adopt_args(&["r-foreign"])).await;
    assert_eq!(
        tool_body(&claimed),
        json!({"runs": [{"runId": "r-foreign", "ownerGeneration": 1}]}),
        "{claimed}"
    );
    assert_eq!(owner_of(&world, "r-foreign"), caller_key());

    // The settled Run has no unread events and no pending recovery —
    // adoption never reopens it.
    let bare = world.run_call(&adopt_args(&["r-done"])).await;
    assert_eq!(tool_code(&bare), "NOT_OWNER", "{bare}");
    // One unread event makes the settled Run claimable — owner moves,
    // settlement never does.
    {
        let mut store = world.store();
        seed_mailbox(
            &mut store,
            &RunId("r-done".into()),
            MailboxEventKind::Settled,
            None,
            "evt-done",
        );
    }
    let unread = world.run_call(&adopt_args(&["r-done"])).await;
    assert_eq!(
        tool_body(&unread),
        json!({"runs": [{"runId": "r-done", "ownerGeneration": 1}]}),
        "{unread}"
    );
    let run = world
        .store()
        .run(&RunId("r-done".into()))
        .expect("read")
        .expect("r-done");
    assert_eq!(run.state, State::Settled, "adoption never reopens");
    assert_eq!(run.settlement, Some(Settlement::Cancelled));
    world.shutdown().await;
}

/// F4/F19 — a Run's own child can never take it: `adopt` by the
/// captured identity's session is `CALLER_IS_RUN` whatever the snapshot
/// shows (the session IS the child's caller key).
#[tokio::test]
async fn f19_adopt_by_own_child_is_caller_is_run() {
    let mut world = World::new(|_catalog| {});
    world.start().await;
    {
        let mut store = world.store();
        bind_caller(&mut store);
        // A foreign-owned Run whose captured child is `gov-r1` —
        // `sess-r1`, the `w1:p2` occupant.
        seed(
            &mut store,
            vec![bind(&foreign_key(), FOREIGN_RELAY, FOREIGN_PANE)],
            Vec::new(),
        );
        let mut run = run_row("r1", "l-r1", State::Active);
        run.owner = foreign_key();
        run.identity = Some(child_identity(
            world.fake(),
            world.child_terminal(),
            "gov-r1",
        ));
        seed_run(&mut store, &run, &launch_row("l-r1", LaunchPhase::Routed));
    }
    // The foreign owner is gone — the child's adopt is the only gate
    // left, and it still loses.
    world.fake().agent_exit(FOREIGN_PANE);
    let child = world.client_for(CHILD_PANE, CHILD_RELAY, world.project());
    let reply = child
        .call_tool(json!(1), "herdr_run", adopt_args(&["r1"]))
        .await;
    assert_eq!(tool_code(&reply), "CALLER_IS_RUN", "{reply}");
    assert_eq!(owner_of(&world, "r1"), foreign_key(), "unmoved");
    world.shutdown().await;
}

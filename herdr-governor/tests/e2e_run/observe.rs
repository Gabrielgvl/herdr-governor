//! `observe` — F6's `herdr_run{observe}` over the wire: the Run page's
//! state/settlement/owner-generation fields, the outbox page's metadata
//! (never a body — N5), the opaque `nextCursor` resume, and the
//! owner-gated/unknown/malformed refusals. `message`'s F17 admission
//! lives here too — the two share the F6 owner gate.

use governor_core::identity::RunId;
use governor_core::lifecycle::{Settlement, State, StateChange};
use governor_core::task::LaunchPhase;
use serde_json::json;

use super::*;

/// An `observe` args object.
fn observe_args(run: &str, cursor: Option<&str>) -> serde_json::Value {
    let mut args = json!({"action": "observe", "runId": run});
    if let Some(resume) = cursor {
        args["cursor"] = json!(resume);
    }
    args
}

/// Seed `count` queued outbox entries on `run`, seq 1..=count.
fn seed_outbox_page(world: &World, run: &str, count: u64) {
    let mut store = world.store();
    for seq in 1..=count {
        seed_outbox(
            &mut store,
            &RunId(run.into()),
            seq,
            &format!("m-{seq}"),
            &format!("follow-up body {seq}"),
        );
    }
}

/// A world with an `active` Run `r1` (no identity — observe needs
/// none), its launch, and the caller bound.
fn observe_world() -> World {
    let world = World::new(|_catalog| {});
    let mut store = world.store();
    bind_caller(&mut store);
    let launch = launch_row("l-r1", LaunchPhase::Routed);
    seed_run(&mut store, &run_row("r1", "l-r1", State::Active), &launch);
    world
}

/// F6/§4.12 — the page carries the Run's fields and outbox metadata
/// (seq, key, state, body digest, byte length — never the body), 200
/// per page under an opaque hex cursor, resumed strictly after `seq`.
#[tokio::test]
async fn f6_observe_pages_metadata_only_under_an_opaque_cursor() {
    let mut world = observe_world();
    world.start().await;
    seed_outbox_page(&world, "r1", 205);

    let first = world.run_call(&observe_args("r1", None)).await;
    let body = tool_body(&first);
    assert_eq!(body["run"]["runId"], "r1", "{body}");
    assert_eq!(body["run"]["state"], "active", "{body}");
    assert_eq!(body["run"]["settlement"], serde_json::Value::Null);
    assert_eq!(body["run"]["ownerGeneration"], 0);
    assert_eq!(body["run"]["judgingDigest"], serde_json::Value::Null);
    assert_eq!(body["run"]["handoffPath"], serde_json::Value::Null);
    assert_eq!(body["acceptance"], json!([]));

    let items = body["outbox"]["items"].as_array().expect("items");
    assert_eq!(items.len(), 200, "the first page fills OUTBOX_PAGE");
    assert_eq!(items[0]["seq"], 1);
    assert_eq!(items[199]["seq"], 200);
    let item = &items[0];
    assert_eq!(item["messageKey"], "m-1");
    assert_eq!(item["state"], "queued");
    assert_eq!(
        item["bodyDigest"].as_str().expect("digest").len(),
        64,
        "a lowercase-hex sha256"
    );
    assert_eq!(item["bodyBytes"], json!("follow-up body 1".len()));
    for member in item.as_object().expect("item").keys() {
        assert!(
            !matches!(member.as_str(), "body" | "text" | "payload"),
            "bodies never ride the page: {member}"
        );
    }

    let cursor = body["outbox"]["nextCursor"]
        .as_str()
        .expect("a next page exists");
    assert!(
        !cursor.is_empty() && cursor.bytes().all(|b| b.is_ascii_hexdigit()),
        "an opaque hex cursor: {cursor}"
    );
    let second = world.run_call(&observe_args("r1", Some(cursor))).await;
    let page = tool_body(&second);
    let tail = page["outbox"]["items"].as_array().expect("items");
    assert_eq!(tail.len(), 5, "the resume reads strictly after seq 200");
    assert_eq!(tail[0]["seq"], 201);
    assert_eq!(tail[4]["seq"], 205);
    assert_eq!(page["outbox"]["nextCursor"], serde_json::Value::Null);
    world.shutdown().await;
}

/// F6/F4 — `observe` is owner-gated and its cursor is strict: another
/// caller's Run and an unknown `runId` both answer `NOT_OWNER` (F4 —
/// existence is never leaked), an unreadable cursor is
/// `REQUEST_INVALID`, never silently rewound.
#[tokio::test]
async fn f6_observe_owner_gates_and_cursor_refusals() {
    let mut world = observe_world();
    world.start().await;

    let missing = world.run_call(&observe_args("r-missing", None)).await;
    assert_eq!(tool_code(&missing), "NOT_OWNER", "{missing}");

    let foreign = world.client_for(FOREIGN_PANE, FOREIGN_RELAY, world.project());
    let outsider = foreign
        .call_tool(json!(1), "herdr_run", observe_args("r1", None))
        .await;
    assert_eq!(tool_code(&outsider), "NOT_OWNER", "{outsider}");

    for bad in ["not-hex", "zz", "7b226b223a7d", ""] {
        let reply = world.run_call(&observe_args("r1", Some(bad))).await;
        assert_eq!(
            tool_code(&reply),
            "REQUEST_INVALID",
            "cursor {bad:?}: {reply}"
        );
    }
    world.shutdown().await;
}

/// F17/F6 — `message` is owner-bound like every F6 action, idempotent
/// on `(run, messageKey)` by body digest, conflicting on a same-key
/// different body, and refused `RUN_SETTLED` on a settled Run.
#[tokio::test]
async fn f17_message_enqueues_dedups_and_refuses() {
    let mut world = observe_world();
    world.start().await;
    let args = |key: &str, text: &str| json!({"action": "message", "runId": "r1", "messageKey": key, "text": text});

    let queued = world.run_call(&args("m-1", "keep going")).await;
    assert_eq!(
        tool_body(&queued),
        json!({"seq": 1, "state": "queued"}),
        "{}",
        tool_body(&queued)
    );
    let replay = world.run_call(&args("m-1", "keep going")).await;
    assert_eq!(tool_body(&replay), json!({"seq": 1, "state": "queued"}));
    let conflict = world.run_call(&args("m-1", "different body")).await;
    assert_eq!(tool_code(&conflict), "MESSAGE_KEY_CONFLICT");

    let page = tool_body(&world.run_call(&observe_args("r1", None)).await);
    let items = page["outbox"]["items"].as_array().expect("items");
    assert_eq!(items.len(), 1, "the follow-up is in the outbox page");
    assert_eq!(items[0]["messageKey"], "m-1");
    assert_eq!(items[0]["state"], "queued");

    let foreign = world.client_for(FOREIGN_PANE, FOREIGN_RELAY, world.project());
    let outsider = foreign
        .call_tool(json!(1), "herdr_run", args("m-2", "not yours"))
        .await;
    assert_eq!(tool_code(&outsider), "NOT_OWNER", "{outsider}");

    let mut store = world.store();
    let mut settled_row = run_row("r2", "l-r2", State::Settled);
    settled_row.settlement = Some(Settlement::Cancelled);
    settled_row.settled_at = Some(NOW);
    let mut writes = launch_chain(&done_launch("l-r2", failed_outcome("r2")));
    writes.push(StateChange::ReserveRun(settled_row));
    seed(&mut store, writes, Vec::new());
    let mut settled_args = args("m-9", "too late");
    settled_args["runId"] = json!("r2");
    let late = world.run_call(&settled_args).await;
    assert_eq!(tool_code(&late), "RUN_SETTLED", "{late}");
    world.shutdown().await;
}

/// F6/F20 — a settled Run's page reports the settlement and — for
/// `unresolved` — its reason; settlement is the immutable first write,
/// so the page cannot move.
#[tokio::test]
async fn f6_observe_reports_the_settled_fields() {
    let mut world = observe_world();
    world.start().await;
    let mut store = world.store();
    let mut run = run_row("r2", "l-r2", State::Settled);
    run.settlement = Some(Settlement::Unresolved {
        reason: governor_core::lifecycle::UnresolvedReason::LaunchFailed,
    });
    run.settled_at = Some(NOW);
    let mut writes = launch_chain(&done_launch("l-r2", failed_outcome("r2")));
    writes.push(StateChange::ReserveRun(run));
    seed(&mut store, writes, Vec::new());

    let reply = world.run_call(&observe_args("r2", None)).await;
    let body = tool_body(&reply);
    assert_eq!(body["run"]["state"], "settled", "{body}");
    assert_eq!(body["run"]["settlement"], "unresolved");
    assert_eq!(body["run"]["settlementReason"], "launch_failed");
    world.shutdown().await;
}

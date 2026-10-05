//! `ack` — F18's idempotent mailbox stamp over the wire: a destined
//! event answers `acked`, a repeat answers the same (`acked_at` is a
//! once-write), and an absent or foreign event is `NOT_OWNER` — the
//! refusal never leaks another caller's events, not even their
//! existence (F4).

use governor_core::delivery::MailboxEventKind;
use governor_core::identity::RunId;
use governor_core::lifecycle::State;
use governor_core::task::LaunchPhase;
use serde_json::json;

use super::*;

/// An `ack` args object.
fn ack_args(event: &str) -> serde_json::Value {
    json!({"action": "ack", "eventId": event})
}

/// A world with an `active` Run `r1` (its owner is the ack's caller —
/// `MailboxSubject::Run` events are destined to the owner).
fn ack_world() -> World {
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

/// F18/F4 — `ack` stamps a destined event once and idempotently on the
/// repeat (the `acked_at IS NULL` once-write absorbs it), while an
/// event that does not exist and one destined to another caller both
/// answer `NOT_OWNER`.
#[tokio::test]
async fn f18_ack_stamps_once_and_never_leaks() {
    let mut world = ack_world();
    world.start().await;
    let mut store = world.store();
    seed_mailbox(
        &mut store,
        &RunId("r1".into()),
        MailboxEventKind::CooldownHit,
        None,
        "evt-mine",
    );
    // A foreign-owned Run's event — destined to the foreign caller,
    // invisible to this one.
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
    seed_mailbox(
        &mut store,
        &RunId("r-for".into()),
        MailboxEventKind::CooldownHit,
        None,
        "evt-foreign",
    );

    let reply = world.run_call(&ack_args("evt-mine")).await;
    assert_eq!(
        tool_body(&reply),
        json!({"eventId": "evt-mine", "acked": true}),
        "{reply}"
    );
    // The stamp landed: the event leaves the caller's unacked set.
    let view = world.store();
    assert!(
        caller_events(&view, MailboxEventKind::CooldownHit).is_empty(),
        "acked events no longer page"
    );
    // The repeat is the same honest answer — idempotent.
    let replay = world.run_call(&ack_args("evt-mine")).await;
    assert_eq!(
        tool_body(&replay),
        json!({"eventId": "evt-mine", "acked": true}),
        "{replay}"
    );

    let absent = world.run_call(&ack_args("evt-missing")).await;
    assert_eq!(tool_code(&absent), "NOT_OWNER", "{absent}");
    let foreign = world.run_call(&ack_args("evt-foreign")).await;
    assert_eq!(tool_code(&foreign), "NOT_OWNER", "{foreign}");
    world.shutdown().await;
}

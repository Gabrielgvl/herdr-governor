//! P4.1 — the `WriteFollowUp` arms: the insert-once `Enqueue`, the
//! forward-only `Dispatch`/`Resolve` compare-and-swap edges and their
//! `MalformedWrite` refusals, expiry after a dispatch, and the F2/F9
//! scenario end to end: a follow-up resolved `submitted` from persisted
//! state frees the queue its `unconfirmed` prompt effect would bar.

use governor_core::delivery::{
    ExpiryReason, FollowUpWrite, MessageBody, OutboxMessage, OutboxState, follow_up_effect,
    next_dispatchable_follow_up,
};
use governor_core::identity::{
    AgentKind, AgentName, ChildIdentity, ChildStatus, Digest, EffectId, HerdrIncarnation,
    MessageKey, PaneId, RunId, TerminalId,
};
use governor_core::lifecycle::{
    Effect, EffectResolution, EffectState, RunUpdate, State, StateChange,
};
use herdr_governor::store::{ApplyError, ConflictKind, Store};

use crate::support::{
    LATER, NOW, caller, changes, count, dispatch as dispatch_effect, result, run, seeded,
    transition,
};

fn run_id() -> RunId {
    RunId("r-1".into())
}

fn message(seq: u64, state: OutboxState) -> OutboxMessage {
    OutboxMessage {
        run: run_id(),
        seq,
        message_key: MessageKey(format!("m-{seq}")),
        sender: caller(1),
        body_digest: Digest([0x44; 32]),
        body: MessageBody::Inline("hi".into()),
        state,
        effect: None,
        expiry_reason: None,
    }
}

#[must_use]
pub fn enqueue(seq: u64) -> StateChange {
    StateChange::WriteFollowUp(FollowUpWrite::Enqueue(message(seq, OutboxState::Queued)))
}

fn identity() -> ChildIdentity {
    ChildIdentity {
        herdr_incarnation: HerdrIncarnation("inc-1".into()),
        terminal_id: TerminalId("term-1".into()),
        agent_kind: AgentKind("kind".into()),
        agent_name: AgentName("gov-r-1".into()),
        native_session: None,
        pane_id: PaneId("pane-2".into()),
    }
}

fn effect_id(seq: u64) -> EffectId {
    EffectId(format!("eff:run:r-1:outbox:{seq}"))
}

fn effect_key(seq: u64) -> String {
    format!("run:r-1:outbox:{seq}")
}

/// The `prompt` effect to the child dispatching seq `seq`, as the core
/// plans it.
fn outbox_effect(seq: u64) -> Effect {
    follow_up_effect(
        &message(seq, OutboxState::Queued),
        identity(),
        effect_id(seq),
        Digest([0x5a; 32]),
    )
}

fn dispatch(seq: u64) -> StateChange {
    StateChange::WriteFollowUp(FollowUpWrite::Dispatch {
        run: run_id(),
        seq,
        effect: effect_id(seq),
    })
}

fn resolve(seq: u64, state: OutboxState) -> StateChange {
    StateChange::WriteFollowUp(FollowUpWrite::Resolve {
        run: run_id(),
        seq,
        state,
    })
}

/// Plans seq's prompt effect (a transaction of its own: the FK is
/// immediate).
fn plan(store: &mut Store, seq: u64) {
    store
        .apply(&transition(vec![], vec![], vec![outbox_effect(seq)]), NOW)
        .unwrap();
}

/// Enqueue, plan and dispatch seq — the row lands `dispatching` with the
/// effect's own dispatch commit.
pub fn dispatched(store: &mut Store, seq: u64) {
    store.apply(&changes(vec![enqueue(seq)]), NOW).unwrap();
    plan(store, seq);
    store
        .apply(
            &changes(vec![dispatch_effect(&effect_key(seq)), dispatch(seq)]),
            NOW,
        )
        .unwrap();
}

/// `(state, effect, finished_at IS NOT NULL)` of seq, read back.
fn row(store: &Store, seq: u64) -> (OutboxState, Option<EffectId>, bool) {
    let message = store
        .outbox(&run_id())
        .unwrap()
        .into_iter()
        .find(|m| m.seq == seq)
        .expect("the row exists");
    let finished: Option<String> = store
        .conn()
        .query_row(
            "SELECT finished_at FROM outbox WHERE run_id = ?1 AND seq = ?2",
            rusqlite::params!["r-1", i64::try_from(seq).unwrap()],
            |r| r.get(0),
        )
        .unwrap();
    (message.state, message.effect, finished.is_some())
}

fn is_conflict(err: &ApplyError) -> bool {
    matches!(
        err,
        ApplyError::Conflict {
            kind: ConflictKind::FollowUp,
            ..
        }
    )
}

#[test]
fn enqueue_is_insert_once() {
    let (_dir, mut store) = seeded();
    store.apply(&changes(vec![enqueue(1)]), NOW).unwrap();
    assert_eq!(row(&store, 1), (OutboxState::Queued, None, false));
    // The same `(run, seq)` again, and a different seq under the same
    // message key: both are the row-exists conflict, never a second row.
    let replay = store.apply(&changes(vec![enqueue(1)]), LATER).unwrap_err();
    assert!(is_conflict(&replay), "{replay}");
    let mut same_key = message(2, OutboxState::Queued);
    same_key.message_key = MessageKey("m-1".into());
    let key_taken = store
        .apply(
            &changes(vec![StateChange::WriteFollowUp(FollowUpWrite::Enqueue(
                same_key,
            ))]),
            LATER,
        )
        .unwrap_err();
    assert!(is_conflict(&key_taken), "{key_taken}");
    assert_eq!(count(&store, "outbox"), 1);
}

#[test]
fn enqueue_refuses_non_queued() {
    let (_dir, mut store) = seeded();
    for state in [
        OutboxState::Dispatching,
        OutboxState::Submitted,
        OutboxState::Unconfirmed,
        OutboxState::Expired,
    ] {
        let mut message = message(1, state);
        message.effect = (state != OutboxState::Expired).then(|| effect_id(1));
        message.expiry_reason = (state == OutboxState::Expired).then_some(ExpiryReason::RunSettled);
        let err = store
            .apply(
                &changes(vec![StateChange::WriteFollowUp(FollowUpWrite::Enqueue(
                    message,
                ))]),
                NOW,
            )
            .unwrap_err();
        assert!(
            matches!(err, ApplyError::MalformedWrite { .. }),
            "{state:?}: {err}"
        );
    }
    assert_eq!(count(&store, "outbox"), 0, "refused before the transaction");
}

#[test]
fn dispatch_requires_queued() {
    let (_dir, mut store) = seeded();
    dispatched(&mut store, 1);
    assert_eq!(
        row(&store, 1),
        (OutboxState::Dispatching, Some(effect_id(1)), false),
        "queued → dispatching links the effect"
    );
    // A replay finds no `queued` row; an unknown `(run, seq)` finds none.
    let replay = store.apply(&changes(vec![dispatch(1)]), LATER).unwrap_err();
    assert!(is_conflict(&replay), "{replay}");
    let unknown = store.apply(&changes(vec![dispatch(9)]), LATER).unwrap_err();
    assert!(is_conflict(&unknown), "{unknown}");
    assert_eq!(
        row(&store, 1),
        (OutboxState::Dispatching, Some(effect_id(1)), false),
        "the losing writes changed nothing"
    );
}

#[test]
fn dispatch_needs_a_journaled_effect() {
    let (_dir, mut store) = seeded();
    store.apply(&changes(vec![enqueue(1)]), NOW).unwrap();
    // No effect `eff:run:r-1:outbox:1` is journaled: the immediate FOREIGN
    // KEY refuses the link.
    let err = store.apply(&changes(vec![dispatch(1)]), NOW).unwrap_err();
    assert!(matches!(err, ApplyError::Constraint { .. }), "{err}");
    assert_eq!(row(&store, 1), (OutboxState::Queued, None, false));
}

#[test]
fn resolve_is_forward_only() {
    let (_dir, mut store) = seeded();
    dispatched(&mut store, 1);
    dispatched(&mut store, 2);
    store.apply(&changes(vec![enqueue(3)]), NOW).unwrap();
    // dispatching → submitted stamps finished_at.
    store
        .apply(&changes(vec![resolve(1, OutboxState::Submitted)]), LATER)
        .unwrap();
    assert_eq!(
        row(&store, 1),
        (OutboxState::Submitted, Some(effect_id(1)), true)
    );
    // dispatching → unconfirmed, then unconfirmed → submitted (F9).
    store
        .apply(&changes(vec![resolve(2, OutboxState::Unconfirmed)]), LATER)
        .unwrap();
    assert_eq!(
        row(&store, 2),
        (OutboxState::Unconfirmed, Some(effect_id(2)), true)
    );
    store
        .apply(&changes(vec![resolve(2, OutboxState::Submitted)]), LATER)
        .unwrap();
    assert_eq!(
        row(&store, 2),
        (OutboxState::Submitted, Some(effect_id(2)), true)
    );
    // Backwards (submitted → unconfirmed) and skipping (queued → submitted)
    // match no row.
    let backwards = store
        .apply(&changes(vec![resolve(1, OutboxState::Unconfirmed)]), LATER)
        .unwrap_err();
    assert!(is_conflict(&backwards), "{backwards}");
    let skipping = store
        .apply(&changes(vec![resolve(3, OutboxState::Submitted)]), LATER)
        .unwrap_err();
    assert!(is_conflict(&skipping), "{skipping}");
    // A resolution to a non-terminal state is not an edge at all.
    for state in [
        OutboxState::Queued,
        OutboxState::Dispatching,
        OutboxState::Expired,
    ] {
        let err = store
            .apply(&changes(vec![resolve(1, state)]), LATER)
            .unwrap_err();
        assert!(
            matches!(err, ApplyError::MalformedWrite { .. }),
            "{state:?}: {err}"
        );
    }
    assert_eq!(
        row(&store, 1),
        (OutboxState::Submitted, Some(effect_id(1)), true)
    );
    assert_eq!(row(&store, 3), (OutboxState::Queued, None, false));
}

#[test]
fn expire_after_dispatch_keeps_state() {
    let (_dir, mut store) = seeded();
    dispatched(&mut store, 1);
    store
        .apply(&changes(vec![resolve(1, OutboxState::Submitted)]), NOW)
        .unwrap();
    store.apply(&changes(vec![enqueue(2)]), NOW).unwrap();
    store
        .apply(
            &changes(vec![StateChange::ExpireFollowUps {
                run: run_id(),
                reason: ExpiryReason::RunSettled,
            }]),
            LATER,
        )
        .unwrap();
    let states: Vec<(OutboxState, Option<ExpiryReason>)> = store
        .outbox(&run_id())
        .unwrap()
        .into_iter()
        .map(|m| (m.state, m.expiry_reason))
        .collect();
    assert_eq!(
        states,
        vec![
            (OutboxState::Submitted, None),
            (OutboxState::Expired, Some(ExpiryReason::RunSettled)),
        ],
        "only the never-dispatched entry expires (F17/F20)"
    );
}

#[test]
fn resolution_frees_the_queue_from_persisted_state() {
    // The F2 scenario end to end through `Store::apply` and the reads: the
    // prompt effect dispatching seq 1 is left `unconfirmed` in the journal,
    // transcript evidence resolves the outbox entry `submitted`, and seq 2
    // becomes dispatchable although the journal row never changes.
    let (_dir, mut store) = seeded();
    let mut active = run("r-1", "l-1");
    active.version = 1;
    active.state = State::Active;
    active.identity = Some(identity());
    active.child_status = Some(ChildStatus::Idle);
    store
        .apply(
            &changes(vec![StateChange::UpdateRun(RunUpdate {
                expected_version: 0,
                record: active,
            })]),
            NOW,
        )
        .unwrap();
    dispatched(&mut store, 1);
    store.apply(&changes(vec![enqueue(2)]), NOW).unwrap();
    // The dispatch is interrupted: the effect is marked `unconfirmed`
    // (F28 restart marking) and the entry with it.
    store
        .apply(
            &changes(vec![
                result(&effect_key(1), EffectResolution::Unconfirmed),
                resolve(1, OutboxState::Unconfirmed),
            ]),
            LATER,
        )
        .unwrap();
    let eligible = |persisted: &Store| {
        let run = persisted.run(&run_id()).unwrap().unwrap();
        let outbox = persisted.outbox(&run_id()).unwrap();
        let journal = persisted.journal(&run_id()).unwrap();
        next_dispatchable_follow_up(&run, &outbox, &journal, &[]).map(|m| m.seq)
    };
    assert_eq!(
        eligible(&store),
        None,
        "the unconfirmed prompt effect bars the queue (F9)"
    );
    // Transcript evidence: unconfirmed → submitted on the outbox row only.
    store
        .apply(&changes(vec![resolve(1, OutboxState::Submitted)]), LATER)
        .unwrap();
    assert_eq!(
        store.effect(&outbox_effect(1).key).unwrap().unwrap().state,
        EffectState::Unconfirmed,
        "the journal row keeps its wire fact"
    );
    assert_eq!(
        eligible(&store),
        Some(2),
        "the submitted entry lifts its effect's barrier; seq 2 dispatches (F9/F17)"
    );
}

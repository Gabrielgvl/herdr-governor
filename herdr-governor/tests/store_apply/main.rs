//! P4.S3 — `store::apply` contract tests (row writers): one transaction
//! per transition, compare-and-swap losers abandon the whole transaction,
//! phase-guarded Launch writes, the idempotent re-applies. The journal
//! (`WriteEffect`) arms live in `effects.rs`, the fixtures in `support.rs`.
//! State is built only through `apply` (the public writer); assertions read
//! through the typed read API plus `SELECT` counts.

#[cfg(test)]
mod dedup;
#[cfg(test)]
mod effects;
#[cfg(test)]
pub mod follow_up;
#[cfg(test)]
pub mod support;

#[cfg(test)]
mod tests {
    use governor_core::acceptance::FrozenHandoff;
    use governor_core::config::{Capability, OperatingPointId, Provider, Qualification};
    use governor_core::delivery::{ExpiryReason, OutboxState};
    use governor_core::identity::{Digest, EventId, LaunchId, RelayInstanceId, RunId, Timestamp};
    use governor_core::lifecycle::{OwnerChange, RunUpdate, Settlement, State, StateChange};
    use governor_core::recovery::{Cooldown, RecoveryObligation, RecoveryOrigin, RecoveryStatus};
    use governor_core::task::{LaunchOutcome, LaunchPhase};
    use herdr_governor::store::{ApplyError, ConflictKind, StoreError};

    use crate::follow_up::{dispatched, enqueue};
    use crate::support::{
        LATER, NOW, binding, caller, changes, count, effect, event, launch, run, seeded, store,
        transition,
    };

    #[test]
    fn apply_is_one_transaction() {
        // A losing write anywhere in the list commits nothing from before it.
        let (_dir, mut store) = seeded();
        let mut stale = run("r-1", "l-1");
        stale.state = State::Starting;
        let frozen = StateChange::FreezeHandoff(FrozenHandoff {
            run: RunId("r-1".into()),
            work_generation: 0,
            digest: Digest([1; 32]),
            frozen_path: "/f".into(),
            frozen_at: NOW,
            assessed: false,
        });
        let update = StateChange::UpdateRun(RunUpdate {
            expected_version: 7,
            record: stale,
        });
        let err = store
            .apply(
                &transition(vec![frozen, update], vec![event("ev-x", "l-1")], vec![]),
                LATER,
            )
            .unwrap_err();
        assert!(
            matches!(
                err,
                ApplyError::Conflict {
                    kind: ConflictKind::RunVersion,
                    ..
                }
            ),
            "{err}"
        );
        assert_eq!(
            count(&store, "handoffs"),
            0,
            "the earlier write rolled back"
        );
        assert_eq!(count(&store, "mailbox"), 0, "nothing after it ran either");
        assert_eq!(
            store.run(&RunId("r-1".into())).unwrap().unwrap().state,
            State::Reserved,
            "the Run is untouched"
        );
    }

    #[test]
    fn update_run_cas_and_settlement_guard() {
        let (_dir, mut store) = seeded();
        let update = |expected_version: u64, record| {
            changes(vec![StateChange::UpdateRun(RunUpdate {
                expected_version,
                record,
            })])
        };
        let mut next = run("r-1", "l-1");
        next.version = 1;
        next.state = State::Starting;
        store.apply(&update(0, next.clone()), LATER).unwrap();
        let stored = store.run(&RunId("r-1".into())).unwrap().unwrap();
        assert_eq!(stored, next, "the record lands verbatim, version as bumped");
        // Settle once: `settlement IS NULL` holds.
        let mut settled = next.clone();
        settled.version = 2;
        settled.state = State::Settled;
        settled.settlement = Some(Settlement::Cancelled);
        settled.settled_at = Some(LATER);
        store.apply(&update(1, settled.clone()), LATER).unwrap();
        // Settle again against the right version: first commit wins (F20).
        settled.version = 3;
        let err = store.apply(&update(2, settled.clone()), LATER).unwrap_err();
        assert!(
            matches!(
                err,
                ApplyError::Conflict {
                    kind: ConflictKind::RunVersion,
                    ..
                }
            ),
            "{err}"
        );
        // Un-settling trips the immutability trigger — surfaced, never
        // swallowed.
        let mut unsettled = settled;
        unsettled.state = State::Active;
        unsettled.settlement = None;
        unsettled.settled_at = None;
        let second = store.apply(&update(2, unsettled), LATER).unwrap_err();
        assert!(matches!(second, ApplyError::Constraint { .. }), "{second}");
    }

    #[test]
    fn launch_writes_are_phase_guarded() {
        let (_dir, mut store) = seeded();
        let record = |launch| changes(vec![StateChange::RecordLaunch(launch)]);
        // `launching` ← `routed` (the seeded phase) is legal.
        let launching = launch("l-1", LaunchPhase::Launching, None);
        store.apply(&record(launching.clone()), LATER).unwrap();
        assert_eq!(
            store
                .launch(&LaunchId("l-1".into()))
                .unwrap()
                .unwrap()
                .phase,
            LaunchPhase::Launching
        );
        // `routed` ← `launching` is not: the row stays where it is.
        let err = store
            .apply(&record(launch("l-1", LaunchPhase::Routed, None)), LATER)
            .unwrap_err();
        assert!(
            matches!(
                err,
                ApplyError::PhaseConflict {
                    phase: "routed",
                    ..
                }
            ),
            "{err}"
        );
        let done = launch("l-1", LaunchPhase::Done, Some(LaunchOutcome::Rejected));
        store.apply(&record(done.clone()), LATER).unwrap();
        // `done` is nobody's predecessor: terminal immutability.
        for again in [done, launching] {
            let second = store.apply(&record(again), LATER).unwrap_err();
            assert!(
                matches!(second, ApplyError::PhaseConflict { .. }),
                "{second}"
            );
        }
        // Admitting the same id twice is a conflict, not a second row.
        let third = store
            .apply(&record(launch("l-1", LaunchPhase::Evaluating, None)), LATER)
            .unwrap_err();
        assert!(
            matches!(
                third,
                ApplyError::Conflict {
                    kind: ConflictKind::Launch,
                    ..
                }
            ),
            "{third}"
        );
        assert_eq!(count(&store, "launches"), 1);
    }

    #[test]
    fn bind_caller_and_owner_change() {
        let (_dir, mut store) = seeded();
        store.apply(&changes(vec![binding(2)]), LATER).unwrap();
        assert_eq!(count(&store, "callers"), 2);
        // Re-binding caller 1 under a new relay id adds no caller row.
        let mut rebind = binding(1);
        if let StateChange::BindCaller(b) = &mut rebind {
            b.relay_instance = RelayInstanceId("relay-1b".into());
        }
        store.apply(&changes(vec![rebind]), LATER).unwrap();
        assert_eq!(count(&store, "callers"), 2);
        assert_eq!(count(&store, "relay_bindings"), 3);
        // The same relay id never rebinds.
        let err = store.apply(&changes(vec![binding(1)]), LATER).unwrap_err();
        assert!(
            matches!(
                err,
                ApplyError::Conflict {
                    kind: ConflictKind::RelayBinding,
                    ..
                }
            ),
            "{err}"
        );
        let owner = |expected: u8, next: u8| {
            changes(vec![StateChange::ChangeOwner(OwnerChange {
                run: RunId("r-1".into()),
                expected_owner: caller(expected),
                owner: caller(next),
            })])
        };
        store.apply(&owner(1, 2), LATER).unwrap();
        let stored = store.run(&RunId("r-1".into())).unwrap().unwrap();
        assert_eq!((stored.owner, stored.owner_generation), (caller(2), 1));
        // A stale expected owner, or one that was never bound, loses.
        for (expected, next) in [(1, 2), (9, 2)] {
            let second = store.apply(&owner(expected, next), LATER).unwrap_err();
            assert!(
                matches!(
                    second,
                    ApplyError::Conflict {
                        kind: ConflictKind::Owner,
                        ..
                    }
                ),
                "{second}"
            );
        }
    }

    #[test]
    fn stale_update_run_keeps_the_committed_owner() {
        // F4/F20 — ownership and lifecycle are orthogonal CAS domains: an
        // UpdateRun computed before a handover still matches `version`, but
        // it must never write owner_caller_id/owner_generation back — those
        // columns have exactly one writer, ChangeOwner.
        let (_dir, mut store) = seeded();
        store.apply(&changes(vec![binding(2)]), NOW).unwrap();
        let handover = changes(vec![StateChange::ChangeOwner(OwnerChange {
            run: RunId("r-1".into()),
            expected_owner: caller(1),
            owner: caller(2),
        })]);
        store.apply(&handover, LATER).unwrap();
        // The stale record: the Run as read at version 0 under caller 1,
        // carried into the update its version bump and state move.
        let mut stale = run("r-1", "l-1");
        stale.version = 1;
        stale.state = State::Starting;
        let mut expected = stale.clone();
        expected.owner = caller(2);
        expected.owner_generation = 1;
        store
            .apply(
                &changes(vec![StateChange::UpdateRun(RunUpdate {
                    expected_version: 0,
                    record: stale,
                })]),
                LATER,
            )
            .unwrap();
        let stored = store.run(&RunId("r-1".into())).unwrap().unwrap();
        assert_eq!(
            stored, expected,
            "the update lands its lifecycle fields; the committed handover stands"
        );
    }

    #[test]
    fn events_and_effects_replay_idempotent() {
        let (_dir, mut store) = seeded();
        let replan = transition(
            vec![],
            vec![event("ev-1", "l-1"), event("ev-2", "l-1")],
            vec![effect("run:r-1:prompt:task", None, Some("r-1"))],
        );
        store.apply(&replan, LATER).unwrap();
        store.apply(&replan, LATER).unwrap();
        assert_eq!(
            count(&store, "mailbox"),
            1,
            "dedup_key: the second event is a no-op"
        );
        assert_eq!(
            count(&store, "effects"),
            1,
            "effect_key: the replan is a no-op"
        );
        let ack = changes(vec![StateChange::AckEvent(EventId("ev-1".into()))]);
        store.apply(&ack, LATER).unwrap();
        store.apply(&ack, Timestamp(LATER.0 + 1)).unwrap();
        let acked: String = store
            .conn()
            .query_row("SELECT acked_at FROM mailbox", [], |row| row.get(0))
            .unwrap();
        assert_eq!(acked, "2026-10-01T00:01:00.000Z", "the first ack sticks");
        assert!(
            store
                .mailbox_unacked(&caller(1), None, 10)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn cooldown_never_shortens() {
        let (_dir, mut store) = seeded();
        let cooldown = |until: Timestamp| {
            changes(vec![StateChange::SetCooldown(Cooldown {
                provider: Provider("prov".into()),
                until,
                reason: "limited".into(),
                source_run: Some(RunId("r-1".into())),
            })])
        };
        store.apply(&cooldown(LATER), NOW).unwrap();
        store.apply(&cooldown(NOW), LATER).unwrap();
        let stored = store.cooldowns().unwrap();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].until, LATER, "a shorter until is ignored");
        store
            .apply(&cooldown(Timestamp(LATER.0 + 1)), LATER)
            .unwrap();
        assert_eq!(
            store.cooldowns().unwrap()[0].until,
            Timestamp(LATER.0 + 1),
            "a longer one lengthens"
        );
    }

    #[test]
    fn expire_only_queued() {
        let (_dir, mut store) = seeded();
        store.apply(&changes(vec![enqueue(1)]), NOW).unwrap();
        // seq 2 reaches `dispatching` the only way a row can: enqueued, its
        // prompt effect planned, then dispatched with that effect's own
        // dispatch commit.
        dispatched(&mut store, 2);
        let err = store.apply(&changes(vec![enqueue(1)]), NOW).unwrap_err();
        assert!(
            matches!(
                err,
                ApplyError::Conflict {
                    kind: ConflictKind::FollowUp,
                    ..
                }
            ),
            "{err}"
        );
        let expire = changes(vec![StateChange::ExpireFollowUps {
            run: RunId("r-1".into()),
            reason: ExpiryReason::RunSettled,
        }]);
        store.apply(&expire, LATER).unwrap();
        let states: Vec<(OutboxState, Option<ExpiryReason>)> = store
            .outbox(&RunId("r-1".into()))
            .unwrap()
            .into_iter()
            .map(|m| (m.state, m.expiry_reason))
            .collect();
        assert_eq!(
            states,
            vec![
                (OutboxState::Expired, Some(ExpiryReason::RunSettled)),
                (OutboxState::Dispatching, None)
            ],
            "queued expires, dispatching keeps its state"
        );
    }

    #[test]
    fn recovery_upsert_honors_pending_only() {
        let (_dir, mut store) = seeded();
        let obligation = |status: RecoveryStatus| {
            changes(vec![StateChange::RecordRecovery(RecoveryObligation {
                predecessor: RunId("r-1".into()),
                origin: RecoveryOrigin::ProviderLimit,
                status,
                reason: None,
                successor_launch: (status == RecoveryStatus::Dispatched)
                    .then(|| LaunchId("l-1".into())),
                expires_at: LATER,
            })])
        };
        store
            .apply(&obligation(RecoveryStatus::Pending), NOW)
            .unwrap();
        store
            .apply(&obligation(RecoveryStatus::Dispatched), LATER)
            .unwrap();
        let dispatched = store
            .recoveries_by_state(RecoveryStatus::Dispatched)
            .unwrap();
        assert_eq!(dispatched.len(), 1, "pending → dispatched");
        let err = store
            .apply(&obligation(RecoveryStatus::Blocked), LATER)
            .unwrap_err();
        assert!(
            matches!(
                err,
                ApplyError::Conflict {
                    kind: ConflictKind::Recovery,
                    ..
                }
            ),
            "{err}"
        );
        assert_eq!(count(&store, "recoveries"), 1);
    }

    #[test]
    fn handoff_freeze_is_idempotent_and_saturated_times_fail_closed() {
        let (_dir, mut store) = seeded();
        let frozen = StateChange::FreezeHandoff(FrozenHandoff {
            run: RunId("r-1".into()),
            work_generation: 0,
            digest: Digest([1; 32]),
            frozen_path: "/f".into(),
            frozen_at: NOW,
            assessed: false,
        });
        store.apply(&changes(vec![frozen.clone()]), NOW).unwrap();
        store.apply(&changes(vec![frozen]), LATER).unwrap();
        assert_eq!(store.handoffs(&RunId("r-1".into())).unwrap().len(), 1);
        // A saturated deadline has no RFC3339 spelling: typed, never truncated.
        let mut forever = run("r-2", "l-1");
        forever.max_age_deadline = Timestamp(i64::MAX);
        let err = store
            .apply(&changes(vec![StateChange::ReserveRun(forever)]), NOW)
            .unwrap_err();
        assert!(
            matches!(
                err,
                ApplyError::Encode(StoreError::CorruptRow {
                    column: "max_age_deadline",
                    ..
                })
            ),
            "{err}"
        );
        assert_eq!(count(&store, "runs"), 1);
    }

    #[test]
    fn record_qualification_replaces_the_verdict() {
        let (_dir, mut store) = store();
        let verdict = |passed: bool| Qualification {
            operating_point: OperatingPointId("op-1".into()),
            args_digest: Digest([0x66; 32]),
            capability: Capability(Capability::PROMPT_ACK.into()),
            passed,
            evidence: "{}".into(),
        };
        store.record_qualification(&verdict(true), NOW).unwrap();
        store.record_qualification(&verdict(false), LATER).unwrap();
        let stored = store
            .qualification(
                &OperatingPointId("op-1".into()),
                Digest([0x66; 32]),
                &Capability(Capability::PROMPT_ACK.into()),
            )
            .unwrap()
            .unwrap();
        assert!(!stored.passed, "the later verdict replaced the earlier");
        assert_eq!(count(&store, "qualifications"), 1);
    }
}

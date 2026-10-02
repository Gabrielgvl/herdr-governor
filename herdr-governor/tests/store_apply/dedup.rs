//! P4.S3 — the idempotent-insert contract (F18/N1/F1): a dedup insert
//! skips its row only when the table's idempotency key is already stored;
//! every other constraint violation — a CHECK, a NOT NULL, or a primary
//! key naming a different row — abandons the whole transaction as
//! `ApplyError::Constraint`, never a silent partial commit.

#[cfg(test)]
mod tests {
    use governor_core::identity::{
        CallerBinding, DedupKey, EffectId, EventId, PaneId, RelayInstanceId, RunId,
    };
    use governor_core::lifecycle::{RunUpdate, State, StateChange};
    use herdr_governor::store::ApplyError;

    use crate::support::{
        LATER, NOW, caller, changes, count, effect, event, run, seeded, transition,
    };

    #[test]
    fn a_constraint_violation_abandons_the_whole_transition() {
        // `apply`'s documented contract: a constraint abandons the whole
        // transaction — the state change ahead of the bad row must not be
        // visible afterwards.
        let (_dir, mut store) = seeded();
        let mut next = run("r-1", "l-1");
        next.version = 1;
        next.state = State::Starting;
        // `subject_launch_id`/`subject_run_id` both NULL violate the
        // Appendix-B `effects` CHECK.
        let unserved = effect("run:r-1:nudge:1", None, None);
        let err = store
            .apply(
                &transition(
                    vec![StateChange::UpdateRun(RunUpdate {
                        expected_version: 0,
                        record: next,
                    })],
                    vec![],
                    vec![unserved],
                ),
                LATER,
            )
            .unwrap_err();
        assert!(matches!(err, ApplyError::Constraint { .. }), "{err}");
        let stored = store.run(&RunId("r-1".into())).unwrap().unwrap();
        assert_eq!(
            (stored.version, stored.state),
            (0, State::Reserved),
            "the earlier state change rolled back with the failed insert"
        );
        assert_eq!(count(&store, "effects"), 1, "no partial row landed");
    }

    #[test]
    fn dedup_replays_are_no_ops() {
        let (_dir, mut store) = seeded();
        // mailbox: the replay is keyed on `dedup_key`, so an event id that
        // was never stored still skips.
        store
            .apply(&transition(vec![], vec![event("ev-1", "l-1")], vec![]), NOW)
            .unwrap();
        store
            .apply(
                &transition(vec![], vec![event("ev-9", "l-1")], vec![]),
                LATER,
            )
            .unwrap();
        assert_eq!(
            count(&store, "mailbox"),
            1,
            "same dedup_key under a new event_id is still the dedup"
        );
        assert!(
            store
                .mailbox_event(&EventId("ev-9".into()))
                .unwrap()
                .is_none(),
            "the duplicate never landed"
        );
        // callers: rebinding a bound caller under a fresh relay inserts no
        // caller row and keeps the first-seen stamp — a skip, not an
        // overwrite. (The same relay id is the F1 `RelayBinding` conflict
        // instead — asserted in `bind_caller_and_owner_change`.)
        let rebind = StateChange::BindCaller(CallerBinding {
            caller: caller(1),
            relay_instance: RelayInstanceId("relay-1x".into()),
            pane_at_bind: PaneId("pane-9".into()),
        });
        store.apply(&changes(vec![rebind]), LATER).unwrap();
        assert_eq!(count(&store, "callers"), 1);
        assert_eq!(
            store.caller(&caller(1)).unwrap(),
            Some(NOW),
            "first_seen_at keeps the original bind's stamp"
        );
    }

    #[test]
    fn a_row_id_collision_under_a_new_key_fails() {
        // Dedup is the idempotency key only: reusing the row's other unique
        // key under a new key is a constraint violation, not a silent skip.
        let (_dir, mut store) = seeded();
        store
            .apply(&transition(vec![], vec![event("ev-1", "l-1")], vec![]), NOW)
            .unwrap();
        let mut rogue_event = event("ev-1", "l-1");
        rogue_event.dedup_key = DedupKey("launch:l-1:settled".into());
        let err = store
            .apply(&transition(vec![], vec![rogue_event], vec![]), LATER)
            .unwrap_err();
        assert!(matches!(err, ApplyError::Constraint { .. }), "{err}");
        let mut rogue_effect = effect("run:r-1:nudge:2", None, Some("r-1"));
        rogue_effect.id = EffectId("eff:run:r-1:prompt:task".into());
        let second = store
            .apply(&transition(vec![], vec![], vec![rogue_effect]), LATER)
            .unwrap_err();
        assert!(matches!(second, ApplyError::Constraint { .. }), "{second}");
        assert_eq!(count(&store, "mailbox"), 1, "the rogue event is gone");
        assert_eq!(count(&store, "effects"), 1, "the rogue effect is gone");
    }
}

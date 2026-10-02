//! The F22 lifecycle proofs: the record/silence oracles the safety and
//! deadline properties assert against. The miniature store (`Sim`), the
//! freeze destination and the state-aware seeds live in `strategies`; the
//! literal Appendix C deadline table lives in `deadline_oracle`.

use governor_core::identity::Timestamp;
use governor_core::lifecycle::{
    DeadlineKind, EffectKind, EffectState, Event, Run, State, StateChange, Transition,
    periodic_review, transition,
};
use proptest::prelude::{ProptestConfig, prop_assert, prop_assert_eq, proptest};

use crate::deadline_oracle::{deadline_probe_journals, expected_deadline_settlement};
use crate::strategies::journal_strategies::arb_journal;
use crate::strategies::{self as arb, FREEZE_PATH, Sim};

/// The `Run` records a transition writes.
fn updated_records(transition: &Transition) -> Vec<&Run> {
    transition
        .state_changes
        .iter()
        .filter_map(|change| match change {
            StateChange::UpdateRun(update) => Some(&update.record),
            StateChange::BindCaller(_)
            | StateChange::RecordLaunch(_)
            | StateChange::ReserveRun(_)
            | StateChange::ChangeOwner(_)
            | StateChange::WriteEffect(_)
            | StateChange::WriteFollowUp(_)
            | StateChange::ExpireFollowUps { .. }
            | StateChange::RecordRecovery(_)
            | StateChange::SetCooldown(_)
            | StateChange::FreezeHandoff(_)
            | StateChange::AckEvent(_) => None,
        })
        .collect()
}

/// A transition that commits nothing (F20 — losing transitions and
/// ignored events produce the empty write set).
pub(crate) fn is_quiet(transition: &Transition) -> bool {
    transition.state_changes.is_empty()
        && transition.events.is_empty()
        && transition.effects.is_empty()
}

/// The Appendix B record invariants on every written record:
/// `settled` ⇔ `settlement` ⇔ `settled_at`.
fn assert_record_coherent(record: &Run) {
    assert_eq!(
        record.state == State::Settled,
        record.settlement.is_some(),
        "settled iff a settlement is recorded"
    );
    assert_eq!(
        record.settlement.is_some(),
        record.settled_at.is_some(),
        "settlement iff settled_at is recorded"
    );
}

proptest! {
    #![proptest_config({
        let mut config = ProptestConfig::with_cases(10_000);
        config.failure_persistence = None;
        config
    })]

    /// F22 — safety over arbitrary event prefixes: at most one
    /// settlement, never two prompts per effect key, no transition out
    /// of a settlement. Each prefix replays over a Run seeded into one
    /// of the six unsettled states by the real transitions —
    /// `launch_plan` moves `reserved` to `starting`, effect results walk
    /// it to `prompting`/`active`, a handoff freezes it into `judging`,
    /// a rejection into `repair` — so deep-state lanes are exercised,
    /// each event is stamped fresh at delivery or perturbed like a late
    /// async result, and the reconcile lane's `periodic_review` runs
    /// between events.
    #[test]
    fn f22_safety_over_event_prefixes(world in arb::arb_seeded_prefix()) {
        let policy = arb::test_policy();
        let mut sim = Sim {
            run: world.run,
            journal: world.journal,
            handoffs: world.handoffs,
            decision: world.decision,
        };
        let mut now = world.start;
        let mut settlements = 0_u32;
        for (event, spec, delta, owner_absent) in world.steps {
            now = Timestamp(now.0.saturating_add(i64::try_from(delta).unwrap_or(i64::MAX)));
            let was_settled = sim.run.state == State::Settled;
            let stamped = arb::stamped(&sim.run, event, spec);
            let outcome = transition(
                &sim.run,
                &stamped,
                now,
                &policy,
                (sim.decision.as_ref(), &sim.journal, &sim.handoffs),
                FREEZE_PATH,
            );

            // at most one settlement, and never out of one
            for record in updated_records(&outcome) {
                assert_record_coherent(record);
                if record.settlement.is_some() {
                    prop_assert!(
                        !was_settled,
                        "a second settlement must never be written"
                    );
                    settlements = settlements.saturating_add(1);
                }
                prop_assert!(
                    !was_settled,
                    "a settled Run accepts no row write"
                );
            }
            if was_settled {
                prop_assert!(
                    outcome.events.is_empty(),
                    "a settled Run emits no mailbox event"
                );
                prop_assert!(
                    outcome
                        .effects
                        .iter()
                        .all(|effect| effect.kind == EffectKind::Close),
                    "a settled Run plans only the cancel-closePane close"
                );
            }

            // never two prompts per effect key — a planned Prompt names
            // a key the journal does not already hold
            for effect in &outcome.effects {
                if effect.kind == EffectKind::Prompt {
                    prop_assert!(
                        !sim.journal.iter().any(|row| row.key == effect.key),
                        "a prompt must never be planned under a journaled key"
                    );
                }
                prop_assert!(
                    outcome
                        .effects
                        .iter()
                        .filter(|other| other.key == effect.key)
                        .count()
                        == 1,
                    "a transition never plans the same effect key twice"
                );
            }

            sim.apply(&outcome);

            // the reconcile lane between events (F23)
            if let Some(effect) =
                periodic_review(&sim.run, owner_absent, &sim.journal)
            {
                prop_assert!(
                    !sim.journal.iter().any(|row| row.key == effect.key),
                    "a review must never be planned under a journaled key"
                );
                sim.journal.push(effect);
            }
        }
        prop_assert!(
            settlements <= 1,
            "a Run settles at most once over any event prefix"
        );
    }

    /// F22 — liveness: once `now` is past every armed deadline, firing
    /// each applicable deadline settles the Run with its Appendix C
    /// settlement, and a full sweep settles it exactly once. Every
    /// unsettled Run carries `max_age_deadline`, so the sweep always
    /// terminates it. `deadline(repair)` on a `judging`/`repair` Run is
    /// additionally probed against a journal holding a qualifying
    /// in-window outbox dispatch — which holds the settle — and a
    /// non-qualifying one — which does not.
    #[test]
    fn f22_settles_past_every_deadline(
        run in arb::arb_unsettled_run(),
        overshoot in 0_u64..3_600_000,
    ) {
        let policy = arb::test_policy();
        let mut latest = run.max_age_deadline.0;
        for armed in [run.idle_deadline, run.repair_deadline, run.judgment_deadline]
            .into_iter()
            .flatten()
        {
            latest = latest.max(armed.0);
        }
        let now = Timestamp(latest.saturating_add(i64::try_from(overshoot).unwrap_or(i64::MAX)));

        let kinds = [
            DeadlineKind::Idle,
            DeadlineKind::Repair,
            DeadlineKind::Judgment,
            DeadlineKind::MaxAge,
        ];
        // each applicable deadline settles in isolation; every other
        // kind is quiet — a deadline only ever fires its own lane
        for kind in kinds {
            for journal in deadline_probe_journals(&run, kind, now) {
                let stamped =
                    arb::stamped(&run, Event::Deadline(kind), arb::StampSpec::Fresh);
                let outcome = transition(
                    &run,
                    &stamped,
                    now,
                    &policy,
                    (None, &journal, &[]),
                    FREEZE_PATH,
                );
                match expected_deadline_settlement(&run, kind, now, &journal) {
                    Some(expected) => {
                        let records = updated_records(&outcome);
                        prop_assert_eq!(records.len(), 1, "a due deadline writes the Run once");
                        let record = records.first().copied();
                        prop_assert_eq!(
                            record.and_then(|r| r.settlement),
                            Some(expected),
                            "the deadline settles with its Appendix C settlement"
                        );
                        prop_assert_eq!(
                            record.map(|r| r.state),
                            Some(State::Settled),
                            "the deadline leaves the Run settled"
                        );
                        prop_assert_eq!(
                            record.and_then(|r| r.settled_at),
                            Some(now),
                            "settled_at records the firing time"
                        );
                    }
                    None => {
                        prop_assert!(
                            is_quiet(&outcome),
                            "a deadline that does not apply commits nothing"
                        );
                    }
                }
            }
        }

        // the sweep in sequence: exactly one settlement, then absorbed
        let mut sim = Sim {
            run,
            journal: Vec::new(),
            handoffs: Vec::new(),
            decision: None,
        };
        let mut settlements = 0_u32;
        for kind in kinds {
            let stamped =
                arb::stamped(&sim.run, Event::Deadline(kind), arb::StampSpec::Fresh);
            let outcome = transition(
                &sim.run,
                &stamped,
                now,
                &policy,
                (None, &sim.journal, &sim.handoffs),
                FREEZE_PATH,
            );
            for record in updated_records(&outcome) {
                if record.settlement.is_some() {
                    settlements = settlements.saturating_add(1);
                }
            }
            sim.apply(&outcome);
        }
        prop_assert_eq!(settlements, 1, "the deadline sweep settles exactly once");
        prop_assert_eq!(sim.run.state, State::Settled, "the Run ends settled");
    }

    /// F22 — a restart never extends a deadline: its journal sweep marks
    /// `dispatching` effects `unconfirmed` and the only Run write it can
    /// emit (the F16 prompting path) touches no deadline.
    #[test]
    fn f22_restart_never_extends_a_deadline(
        run in arb::arb_run(),
        journal in arb_journal(),
        now in arb::arb_timestamp(),
    ) {
        let policy = arb::test_policy();
        let stamped = arb::stamped(&run, Event::Restart, arb::StampSpec::Fresh);
        let outcome = transition(
            &run,
            &stamped,
            now,
            &policy,
            (None, &journal, &[]),
            FREEZE_PATH,
        );
        for change in &outcome.state_changes {
            match change {
                StateChange::WriteEffect(write) => {
                    prop_assert_eq!(
                        write.state,
                        EffectState::Unconfirmed,
                        "restart rewrites only to unconfirmed"
                    );
                    prop_assert_eq!(write.certainty, None, "unconfirmed records no certainty");
                    prop_assert!(
                        journal
                            .iter()
                            .any(|row| row.key == write.key
                                && row.state == EffectState::Dispatching),
                        "restart rewrites only dispatching rows"
                    );
                }
                StateChange::UpdateRun(update) => {
                    let record = &update.record;
                    prop_assert_eq!(
                        record.idle_deadline, run.idle_deadline,
                        "restart must not move idle_deadline"
                    );
                    prop_assert_eq!(
                        record.repair_deadline, run.repair_deadline,
                        "restart must not move repair_deadline"
                    );
                    prop_assert_eq!(
                        record.judgment_deadline, run.judgment_deadline,
                        "restart must not move judgment_deadline"
                    );
                    prop_assert_eq!(
                        record.max_age_deadline, run.max_age_deadline,
                        "restart must not move max_age_deadline"
                    );
                    prop_assert_eq!(
                        record.settlement, run.settlement,
                        "restart never settles"
                    );
                    prop_assert_eq!(
                        record.settled_at, run.settled_at,
                        "restart never settles"
                    );
                }
                StateChange::BindCaller(_)
                | StateChange::RecordLaunch(_)
                | StateChange::ReserveRun(_)
                | StateChange::ChangeOwner(_)
                | StateChange::WriteFollowUp(_)
                | StateChange::ExpireFollowUps { .. }
                | StateChange::RecordRecovery(_)
                | StateChange::SetCooldown(_)
                | StateChange::FreezeHandoff(_)
                | StateChange::AckEvent(_) => {
                    prop_assert!(false, "restart writes only the journal sweep and the F16 path");
                }
            }
        }
    }
}

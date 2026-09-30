//! The F22 lifecycle proofs and their rig: the miniature of the store's
//! conditional-write contract, the record/silence oracles, and the deadline
//! settlement table they assert against.

use governor_core::acceptance::FrozenHandoff;
use governor_core::identity::Timestamp;
use governor_core::lifecycle::{
    DeadlineKind, Effect, EffectKind, EffectState, Event, Run, Settlement, State, StateChange,
    Transition, UnresolvedReason, periodic_review, transition,
};
use governor_core::routing::Decision;
use proptest::option;
use proptest::prelude::{ProptestConfig, prop_assert, prop_assert_eq, proptest};

use crate::strategies as arb;
use crate::strategies::journal_strategies::{arb_decision, arb_journal, arb_seed_topology};

/// The coordinator-supplied destination a new freeze writes (F24).
pub(crate) const FREEZE_PATH: &str = "/state/handoffs/new";

/// The store's conditional-write contract in miniature (Appendix B):
/// `UpdateRun` applies while `expected_version` still holds,
/// `WriteEffect` updates its journaled row, planned effects join the
/// journal, freezes join the handoffs.
struct Sim {
    run: Run,
    journal: Vec<Effect>,
    handoffs: Vec<FrozenHandoff>,
    decision: Option<Decision>,
}

impl Sim {
    fn apply(&mut self, transition: &Transition) {
        for change in &transition.state_changes {
            match change {
                StateChange::UpdateRun(update) => {
                    assert!(
                        update.expected_version == self.run.version,
                        "conditional run write must be computed against the live row"
                    );
                    self.run = update.record.clone();
                }
                StateChange::WriteEffect(write) => {
                    // an UPDATE against an unjournaled key matches no row
                    if let Some(row) = self.journal.iter_mut().find(|row| row.key == write.key) {
                        row.state = write.state;
                        row.certainty = write.certainty;
                        row.receipt = write.receipt.clone();
                    }
                }
                StateChange::FreezeHandoff(handoff) => self.handoffs.push(handoff.clone()),
                StateChange::BindCaller(_)
                | StateChange::RecordLaunch(_)
                | StateChange::ReserveRun(_)
                | StateChange::ChangeOwner(_)
                | StateChange::RecordFollowUp(_)
                | StateChange::ExpireFollowUps { .. }
                | StateChange::RecordRecovery(_)
                | StateChange::SetCooldown(_)
                | StateChange::AckEvent(_) => {}
            }
        }
        for effect in &transition.effects {
            self.journal.push(effect.clone());
        }
    }
}

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
            | StateChange::RecordFollowUp(_)
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

/// The settlement Appendix C attaches to a deadline kind firing at `now`,
/// or `None` when that deadline does not apply to this Run.
fn expected_deadline_settlement(
    run: &Run,
    kind: DeadlineKind,
    now: Timestamp,
) -> Option<Settlement> {
    let overdue = |deadline: Option<Timestamp>| deadline.is_some_and(|d| now >= d);
    if kind == DeadlineKind::MaxAge && now >= run.max_age_deadline {
        Some(Settlement::Unresolved {
            reason: UnresolvedReason::MaxAge,
        })
    } else if kind == DeadlineKind::Idle && run.state == State::Active && overdue(run.idle_deadline)
    {
        Some(Settlement::NoHandoff)
    } else if kind == DeadlineKind::Repair
        && (run.state == State::Repair || run.state == State::Judging)
        && overdue(run.repair_deadline)
    {
        Some(Settlement::Rejected)
    } else if kind == DeadlineKind::Judgment
        && run.state == State::Judging
        && overdue(run.judgment_deadline)
    {
        Some(Settlement::Unresolved {
            reason: UnresolvedReason::JudgmentUnavailable,
        })
    } else {
        None
    }
}

proptest! {
    #![proptest_config({
        let mut config = ProptestConfig::with_cases(10_000);
        config.failure_persistence = None;
        config
    })]

    /// F22 — safety over arbitrary event prefixes: at most one
    /// settlement, never two prompts per effect key, no transition out
    /// of a settlement. The prefix drives a freshly `reserved` Run whose
    /// journal holds only the launch plan's topology effects; each event
    /// is stamped fresh at delivery or perturbed like a late async
    /// result, and the reconcile lane's `periodic_review` runs between
    /// events.
    #[test]
    fn f22_safety_over_event_prefixes(
        (start, steps) in arb::arb_prefix(),
        run in arb::arb_reserved_run(),
        seed in arb_seed_topology(),
        decision in option::of(arb_decision()),
    ) {
        let policy = arb::test_policy();
        let mut sim = Sim {
            run,
            journal: seed,
            handoffs: Vec::new(),
            decision,
        };
        let mut now = start;
        let mut settlements = 0_u32;
        for (event, spec, delta, owner_absent) in steps {
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
    /// terminates it.
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
            let stamped = arb::stamped(&run, Event::Deadline(kind), arb::StampSpec::Fresh);
            let outcome = transition(
                &run,
                &stamped,
                now,
                &policy,
                (None, &[], &[]),
                FREEZE_PATH,
            );
            match expected_deadline_settlement(&run, kind, now) {
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
                | StateChange::RecordFollowUp(_)
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

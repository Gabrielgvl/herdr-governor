//! The independent deadline oracle for `f22_settles_past_every_deadline`:
//! Appendix C's `deadline(...)` rows transcribed as literal data — state,
//! armed deadline, hold, expected settlement — never derived from
//! `on_deadline`'s code. `deadline_probe_journals` supplies the journals
//! the hold row is exercised against.

use governor_core::identity::{EffectId, Timestamp};
use governor_core::lifecycle::{
    DeadlineKind, Effect, EffectKind, EffectState, EffectTarget, Run, Settlement, State,
    UnresolvedReason,
};

use crate::strategies::run_key;

/// Whether the row's settle is suppressed by an in-flight dispatch —
/// Appendix C | `deadline(repair) with a qualifying dispatch in flight` |
/// stays — the pending dispatch's result decides |.
enum Hold {
    /// The deadline settles unconditionally once armed and overdue.
    Never,
    /// A qualifying in-window outbox dispatch still `dispatching` holds it.
    QualifyingRepairDispatch,
}

/// Appendix C's deadline rules as literal data: the state column (the
/// spec's `unsettled` expands to one row per state), the deadline that
/// fired, the hold that suppresses the settle, and the mandated
/// settlement.
struct DeadlineRule {
    /// The state column.
    state: State,
    /// The `deadline(kind)` column.
    kind: DeadlineKind,
    /// The hold column.
    hold: Hold,
    /// The outcome column's settlement.
    settlement: Settlement,
}

/// | `unsettled` | `deadline(max_age)` | settle unresolved(max_age) | —
/// one row per unsettled state, plus each state's own armed deadline.
const DEADLINE_RULES: &[DeadlineRule] = &[
    DeadlineRule {
        state: State::Reserved,
        kind: DeadlineKind::MaxAge,
        hold: Hold::Never,
        settlement: Settlement::Unresolved {
            reason: UnresolvedReason::MaxAge,
        },
    },
    DeadlineRule {
        state: State::Starting,
        kind: DeadlineKind::MaxAge,
        hold: Hold::Never,
        settlement: Settlement::Unresolved {
            reason: UnresolvedReason::MaxAge,
        },
    },
    DeadlineRule {
        state: State::Prompting,
        kind: DeadlineKind::MaxAge,
        hold: Hold::Never,
        settlement: Settlement::Unresolved {
            reason: UnresolvedReason::MaxAge,
        },
    },
    DeadlineRule {
        state: State::Active,
        kind: DeadlineKind::MaxAge,
        hold: Hold::Never,
        settlement: Settlement::Unresolved {
            reason: UnresolvedReason::MaxAge,
        },
    },
    DeadlineRule {
        state: State::Judging,
        kind: DeadlineKind::MaxAge,
        hold: Hold::Never,
        settlement: Settlement::Unresolved {
            reason: UnresolvedReason::MaxAge,
        },
    },
    DeadlineRule {
        state: State::Repair,
        kind: DeadlineKind::MaxAge,
        hold: Hold::Never,
        settlement: Settlement::Unresolved {
            reason: UnresolvedReason::MaxAge,
        },
    },
    // | `active` | `deadline(idle)` | settle no_handoff |
    DeadlineRule {
        state: State::Active,
        kind: DeadlineKind::Idle,
        hold: Hold::Never,
        settlement: Settlement::NoHandoff,
    },
    // | `judging` | `deadline(judgment)` | settle unresolved(judgment_unavailable) |
    DeadlineRule {
        state: State::Judging,
        kind: DeadlineKind::Judgment,
        hold: Hold::Never,
        settlement: Settlement::Unresolved {
            reason: UnresolvedReason::JudgmentUnavailable,
        },
    },
    // | `judging` | `deadline(repair) armed and passed` | settle rejected |
    //   — held while a qualifying dispatch is in flight
    DeadlineRule {
        state: State::Judging,
        kind: DeadlineKind::Repair,
        hold: Hold::QualifyingRepairDispatch,
        settlement: Settlement::Rejected,
    },
    // | `repair` | `deadline(repair)` | settle rejected |
    //   — held while a qualifying dispatch is in flight
    DeadlineRule {
        state: State::Repair,
        kind: DeadlineKind::Repair,
        hold: Hold::QualifyingRepairDispatch,
        settlement: Settlement::Rejected,
    },
];

/// The armed deadline `kind` reads (Appendix C stores them absolute):
/// `idle` → `idle_deadline`, `repair` → `repair_deadline`, `judgment` →
/// `judgment_deadline`; `max_age` is always armed.
fn armed_deadline(run: &Run, kind: DeadlineKind) -> Option<Timestamp> {
    match kind {
        DeadlineKind::Idle => run.idle_deadline,
        DeadlineKind::Repair => run.repair_deadline,
        DeadlineKind::Judgment => run.judgment_deadline,
        DeadlineKind::MaxAge => Some(run.max_age_deadline),
    }
}

/// F24 — a "qualifying dispatch in flight" (Appendix C): a repair
/// follow-up — a `prompt` journaled under `run:<id>:outbox:` — still
/// `dispatching` whose `dispatched_at` lands inside the rejected
/// generation's window `[rejected_at, repair_deadline)`. Both edges are
/// read off the persisted row, never derived.
fn qualifying_dispatch_in_flight(run: &Run, journal: &[Effect]) -> bool {
    let Some((rejected, deadline)) = run.rejected_at.zip(run.repair_deadline) else {
        return false;
    };
    let prefix = format!("run:{}:outbox:", run.id.0);
    journal.iter().any(|effect| {
        effect.kind == EffectKind::Prompt
            && effect.state == EffectState::Dispatching
            && effect.key.0.starts_with(&prefix)
            && effect
                .dispatched_at
                .is_some_and(|at| rejected <= at && at < deadline)
    })
}

/// The settlement Appendix C mandates for `deadline(kind)` firing on `run`
/// at `now` — a lookup into `DEADLINE_RULES`, or `None` when no row covers
/// it (the deadline writes nothing, or a hold suppresses it).
#[must_use]
pub fn expected_deadline_settlement(
    run: &Run,
    kind: DeadlineKind,
    now: Timestamp,
    journal: &[Effect],
) -> Option<Settlement> {
    let rule = DEADLINE_RULES
        .iter()
        .find(|rule| rule.state == run.state && rule.kind == kind)?;
    if armed_deadline(run, kind).is_none_or(|deadline| now < deadline) {
        return None;
    }
    let held = match rule.hold {
        Hold::Never => false,
        Hold::QualifyingRepairDispatch => qualifying_dispatch_in_flight(run, journal),
    };
    if held { None } else { Some(rule.settlement) }
}

/// The journals `deadline(kind)` is probed against: the empty journal,
/// plus on a `judging`/`repair` Run for `deadline(repair)` — a qualifying
/// in-window dispatch when the Run's window admits one, and always a
/// non-qualifying one (dispatched at `now`, past the armed deadline).
/// Appendix C: with a qualifying dispatch in flight the deadline holds;
/// dispatched past the window it does not.
#[must_use]
pub fn deadline_probe_journals(run: &Run, kind: DeadlineKind, now: Timestamp) -> Vec<Vec<Effect>> {
    let mut journals = Vec::from([Vec::new()]);
    if kind != DeadlineKind::Repair || (run.state != State::Judging && run.state != State::Repair) {
        return journals;
    }
    let in_window = run
        .rejected_at
        .zip(run.repair_deadline)
        .is_some_and(|(rejected, deadline)| rejected < deadline);
    if in_window && let Some(rejected) = run.rejected_at {
        journals.push(Vec::from([outbox_probe(run, rejected)]));
    }
    journals.push(Vec::from([outbox_probe(run, now)]));
    journals
}

/// A repair follow-up the store committed `dispatching` at
/// `dispatched_at` — qualifying when it lands inside
/// `[rejected_at, repair_deadline)`, never when it doesn't.
fn outbox_probe(run: &Run, dispatched_at: Timestamp) -> Effect {
    let key = run_key("outbox:0");
    Effect {
        id: EffectId(format!("eff:{}", key.0)),
        key,
        kind: EffectKind::Prompt,
        subject_launch: Some(run.launch.clone()),
        subject_run: Some(run.id.clone()),
        target: run.identity.clone().map(EffectTarget::Child),
        payload_digest: None,
        state: EffectState::Dispatching,
        certainty: None,
        receipt: None,
        dispatched_at: Some(dispatched_at),
    }
}

//! F9 — `prompt_dispatchable`'s candidate and state-lane refusals: only a
//! `planned` prompt to the Run's captured `Child` identity in its lane may
//! dispatch at commit time.

use crate::delivery::prompt_dispatchable;
use crate::identity::{EffectKey, RunId};
use crate::lifecycle::{Effect, EffectKind, EffectState, State};

use super::builders::{child_prompt_effect, child_prompt_keyed, hint, run};

#[test]
fn f9_prompt_dispatchable_refuses_non_child_or_unplanned() {
    // A hint-keyed candidate is not a child prompt at all — wrong captured
    // identity — so the gate never judges one. Neither is a non-prompt row
    // or a prompt journaled for another Run.
    let jev = Effect {
        kind: EffectKind::JevEvaluate,
        target: None,
        ..child_prompt_keyed("review:0", EffectState::Planned)
    };
    let foreign = Effect {
        key: EffectKey("run:r2:outbox:1".into()),
        subject_run: Some(RunId("r2".into())),
        ..child_prompt_effect(EffectState::Planned)
    };
    for (effect, name) in [
        (hint(EffectState::Planned), "hint"),
        (jev, "jev_evaluate"),
        (foreign, "foreign run"),
    ] {
        let slice = [effect.clone()];
        assert!(
            !prompt_dispatchable(&run(), &[], &slice, &effect.key),
            "{name} is not this Run's child prompt (F9)"
        );
    }
    // A candidate absent from the slice, or present but not `planned`,
    // cannot dispatch.
    let absent = child_prompt_keyed("outbox:1", EffectState::Planned);
    assert!(
        !prompt_dispatchable(&run(), &[], &[], &absent.key),
        "an unjournaled candidate cannot dispatch"
    );
    for state in [
        EffectState::Dispatching,
        EffectState::Acknowledged,
        EffectState::Failed,
        EffectState::Unconfirmed,
    ] {
        let resolved = child_prompt_keyed("outbox:1", state);
        let slice = [resolved.clone()];
        assert!(
            !prompt_dispatchable(&run(), &[], &slice, &resolved.key),
            "only a planned candidate dispatches, not {state:?}"
        );
    }
}

#[test]
fn f9_earlier_siblings_hold_order_only_when_planned_child_prompts() {
    // Only another *planned* prompt to the child ahead in plan order holds
    // the order: a resolved one is done, and a planned non-prompt row is
    // outside the child's serialization entirely.
    let candidate = child_prompt_keyed("outbox:1", EffectState::Planned);
    let resolved_first = [
        child_prompt_keyed("outbox:0", EffectState::Acknowledged),
        candidate.clone(),
    ];
    assert!(
        prompt_dispatchable(&run(), &[], &resolved_first, &candidate.key),
        "a resolved prompt ahead never holds the order (F9)"
    );
    let planned_jev_first = [
        Effect {
            kind: EffectKind::JevEvaluate,
            target: None,
            ..child_prompt_keyed("blocked:0", EffectState::Planned)
        },
        candidate.clone(),
    ];
    assert!(
        prompt_dispatchable(&run(), &[], &planned_jev_first, &candidate.key),
        "a planned non-prompt ahead never holds the order (F9)"
    );
    // A dispatching child prompt holds the slot wherever it sits in the
    // slice — before or after the candidate.
    for ahead in [true, false] {
        let dispatching = child_prompt_keyed("outbox:2", EffectState::Dispatching);
        let slice = if ahead {
            [dispatching, candidate.clone()]
        } else {
            [candidate.clone(), dispatching]
        };
        assert!(
            !prompt_dispatchable(&run(), &[], &slice, &candidate.key),
            "a dispatching sibling holds the slot (F9)"
        );
    }
}

#[test]
fn f9_prompt_dispatchable_state_lanes() {
    // The Task prompt dispatches only in `prompting`; every other child
    // prompt only in `active`/`judging`/`repair`.
    let task = child_prompt_effect(EffectState::Planned);
    for state in [
        State::Reserved,
        State::Starting,
        State::Active,
        State::Judging,
        State::Repair,
        State::Settled,
    ] {
        let mut lane = run();
        lane.state = state;
        lane.settlement = None;
        let slice = [task.clone()];
        assert!(
            !prompt_dispatchable(&lane, &[], &slice, &task.key),
            "the Task prompt never dispatches in {state:?} (F9)"
        );
    }
    let nudge = child_prompt_keyed("nudge:0", EffectState::Planned);
    for state in [
        State::Prompting,
        State::Reserved,
        State::Starting,
        State::Settled,
    ] {
        let mut lane = run();
        lane.state = state;
        lane.settlement = None;
        let slice = [nudge.clone()];
        assert!(
            !prompt_dispatchable(&lane, &[], &slice, &nudge.key),
            "a non-Task prompt never dispatches in {state:?} (F9)"
        );
    }
}

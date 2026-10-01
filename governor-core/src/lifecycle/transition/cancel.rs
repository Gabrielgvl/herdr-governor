//! F20 — the `cancel` lane: an unsettled Run settles `cancelled`; a settled
//! one accepts only `cancel {closePane}`, which plans the one verified close
//! (F10).

use alloc::vec::Vec;

use crate::config::Policy;
use crate::identity::Timestamp;
use crate::lifecycle::{
    Effect, EffectKind, EffectTarget, Run, Settlement, State, Transition, effect_key, journaled,
    nothing, op_digest, planned_effect, settle,
};

pub(super) fn on_cancel(
    run: &Run,
    close_pane: bool,
    env: (Timestamp, &Policy),
    journal: &[Effect],
) -> Transition {
    let (now, policy) = env;
    // `settled` accepts only `cancel` with `closePane` — and only the pane
    // close is planned (F20).
    if run.state == State::Settled {
        if close_pane && let Some(effect) = close_effect(run, journal) {
            return Transition {
                state_changes: Vec::new(),
                events: Vec::new(),
                effects: Vec::from([effect]),
            };
        }
        return nothing();
    }
    let mut transition = settle(run, Settlement::Cancelled, now, policy);
    if close_pane && let Some(effect) = close_effect(run, journal) {
        transition.effects.push(effect);
    }
    transition
}

/// The verified close effect (F10) — planned once (`run:<id>:close` is the
/// dedup) and only while a captured identity exists to verify against.
fn close_effect(run: &Run, journal: &[Effect]) -> Option<Effect> {
    let key = effect_key(run, "close");
    if journaled(journal, &key) {
        return None;
    }
    run.identity.clone().map(|identity| {
        // `close` takes no params — the captured Child target is its whole
        // rendered form (OQ-15).
        let target = EffectTarget::Child(identity);
        planned_effect(
            run,
            EffectKind::Close,
            key,
            Some(target.clone()),
            Some(op_digest(EffectKind::Close, Some(&target), &[])),
        )
    })
}

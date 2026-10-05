//! §17 — the settled-mid-start orphan: a Run that settles while its
//! `agent.start` is in flight, and the start then acknowledges. The
//! `AgentStarted` receipt carries a live child identity nothing else
//! owns — the run row never captured it, so no later lane ever closes
//! that pane. `orphan_close` reads the journaled start receipts (plus
//! the in-flight result about to commit) and plans the verified `close`
//! for each orphan: one `<start_key>:close` per orphaned start, deduped
//! against every close already journaled for that identity.

use alloc::format;
use alloc::vec::Vec;

use crate::identity::{ChildIdentity, EffectKey};
use crate::lifecycle::{
    Effect, EffectKind, EffectReceipt, EffectResolution, EffectResult, EffectTarget, Run, State,
    op_digest, planned_effect,
};

/// The verified `close` effects a settled Run owes for orphaned start
/// receipts — empty while the Run lives, has a captured identity for
/// every ack'd start, or already plans/journaled the closes. `current`
/// is the result the calling commit is about to write: its receipt is
/// not yet in `journal`, so it is folded in separately.
#[must_use]
pub fn orphan_close(run: &Run, journal: &[Effect], current: &EffectResult) -> Vec<Effect> {
    if run.state != State::Settled {
        return Vec::new();
    }
    let mut planned = Vec::new();
    for effect in journal {
        if effect.kind == EffectKind::AgentStart
            && let Some(EffectReceipt::AgentStarted { identity }) = &effect.receipt
            && let Some(close) = close_for(run, journal, &planned, &effect.key, identity)
        {
            planned.push(close);
        }
    }
    if current.kind == EffectKind::AgentStart
        && let EffectResolution::Acknowledged {
            receipt: Some(EffectReceipt::AgentStarted { identity }),
        } = &current.resolution
        && let Some(close) = close_for(run, journal, &planned, &current.key, identity)
    {
        planned.push(close);
    }
    planned
}

/// One `<start_key>:close` for `identity`, or `None` when the child is
/// not an orphan (it is the run's captured identity or a close for it
/// is already journaled/planned).
fn close_for(
    run: &Run,
    journal: &[Effect],
    planned: &[Effect],
    start_key: &EffectKey,
    identity: &ChildIdentity,
) -> Option<Effect> {
    // The run's own captured identity is supervised elsewhere; an
    // orphan is always a *different* child.
    if run.identity.as_ref() == Some(identity) {
        return None;
    }
    if journal.iter().chain(planned.iter()).any(|effect| {
        effect.kind == EffectKind::Close
            && matches!(&effect.target, Some(EffectTarget::Child(i)) if i == identity)
    }) {
        return None;
    }
    let key = EffectKey(format!("{}:close", start_key.0));
    if journal.iter().chain(planned.iter()).any(|e| e.key == key) {
        return None;
    }
    let target = EffectTarget::Child(identity.clone());
    Some(planned_effect(
        run,
        EffectKind::Close,
        key,
        Some(target.clone()),
        Some(op_digest(EffectKind::Close, Some(&target), &[])),
    ))
}

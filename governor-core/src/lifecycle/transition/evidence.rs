//! F23 — the `evidence` lane: a transcript/git evidence digest that differs
//! from `evidence_digest` records it and bumps `evidence_generation`, so
//! periodic reviews re-ask under the new generation. In `judging` the bump
//! makes the pending acceptance ask's answer stale (F20), so the ask is
//! re-planned for `judging_digest` — the resume-judging write: no second
//! freeze row, `judgment_deadline` stands.

use alloc::vec::Vec;

use crate::config::Policy;
use crate::identity::{Digest, Timestamp};
use crate::lifecycle::{Run, State, Transition, nothing, update_if_changed, write_run};

use super::handoff::judging_write;

pub(super) fn on_evidence(run: &Run, digest: Digest, env: (Timestamp, &Policy)) -> Transition {
    // an unchanged digest proves nothing new — and `settled` answers nothing.
    if run.state == State::Settled || run.evidence_digest == Some(digest) {
        return nothing();
    }
    if run.state == State::Judging
        && let Some(judged) = run.judging_digest
    {
        // the bump stales the in-flight ask's answer (F20) — re-plan it for
        // the digest under judgment: `judging_write`'s fresh
        // `evidence_generation` keys it, the freeze row stands, and
        // `judgment_deadline` keeps bounding the wait.
        let (mut record, ask) = judging_write(run, judged, env);
        record.evidence_digest = Some(digest);
        return Transition {
            state_changes: Vec::from([write_run(run, record)]),
            events: Vec::new(),
            effects: Vec::from([ask]),
        };
    }
    update_if_changed(run, |next| {
        next.evidence_digest = Some(digest);
        next.evidence_generation = next.evidence_generation.saturating_add(1);
    })
}

//! §17 — `orphan_close`: a Run settled mid-start, whose `agent.start`
//! then acknowledged, owes a verified close for the child the receipt
//! captured — no other lane ever sees that identity. The planner must
//! not close the run's own captured child, re-plan a close already
//! journaled, or plan anything while the run still lives.

use alloc::vec::Vec;

use super::builders::{caller, child, closed, prompted, run, started};
use crate::identity::EffectKey;
use crate::lifecycle::{
    EffectKind, EffectReceipt, EffectResolution, EffectResult, EffectTarget, State,
};
use crate::recovery::orphan_close;

/// The `agent.start` result about to commit — the shape `orphan_close`'s
/// `current` takes.
fn start_result(key: &str, receipt: Option<EffectReceipt>) -> EffectResult {
    EffectResult {
        key: EffectKey(key.into()),
        kind: EffectKind::AgentStart,
        resolution: EffectResolution::Acknowledged { receipt },
    }
}

/// §17 — the journaled start receipt is an orphan once the run settled:
/// one `close`, keyed off that start, addressed to the receipt's child.
#[test]
fn settled_run_plans_close_for_orphaned_start() {
    let run = run("run-1", &caller(), None);
    let identity = child("p9");
    let start = started(&run.id, "start", &identity);
    let result = start_result("run:run-1:start", None);
    let planned = orphan_close(&run, &[start], &result);
    assert_eq!(planned.len(), 1);
    assert_eq!(planned[0].kind, EffectKind::Close);
    assert_eq!(planned[0].key.0, "run:run-1:start:close");
    assert_eq!(
        planned[0].target,
        Some(EffectTarget::Child(identity.clone()))
    );
    assert_eq!(planned[0].subject_run, Some(run.id.clone()));
}

/// §17 — the start acknowledgement carrying the receipt arrives in the
/// same commit that settles the race: `current` is not yet journaled,
/// so the orphan scan must read the result itself.
#[test]
fn in_flight_start_receipt_plans_the_close() {
    let run = run("run-1", &caller(), None);
    let identity = child("p9");
    let result = start_result(
        "run:run-1:start",
        Some(EffectReceipt::AgentStarted {
            identity: identity.clone(),
        }),
    );
    let planned = orphan_close(&run, &[], &result);
    assert_eq!(planned.len(), 1);
    assert_eq!(planned[0].target, Some(EffectTarget::Child(identity)));
}

/// A living run keeps its in-flight start — no orphan yet, whatever the
/// receipts say.
#[test]
fn live_run_plans_no_orphan_close() {
    let mut run = run("run-1", &caller(), None);
    run.state = State::Active;
    let identity = child("p9");
    let result = start_result(
        "run:run-1:start",
        Some(EffectReceipt::AgentStarted { identity }),
    );
    assert!(orphan_close(&run, &Vec::new(), &result).is_empty());
}

/// The run's own captured child is supervised — an orphan close must
/// never target the identity `runs.child_identity` already owns.
#[test]
fn captured_identity_is_not_an_orphan() {
    let mut run = run("run-1", &caller(), None);
    let identity = child("p9");
    run.identity = Some(identity.clone());
    let start = started(&run.id, "start", &identity);
    let result = start_result("run:run-1:start", None);
    assert!(orphan_close(&run, &[start], &result).is_empty());
}

/// A close already journaled for the child is not re-planned — the
/// exactly-once dedupe the §17 carry-over requires.
#[test]
fn journaled_close_is_not_replanned() {
    let run = run("run-1", &caller(), None);
    let identity = child("p9");
    let journal = Vec::from([
        started(&run.id, "start", &identity),
        closed(&run.id, "start:close", &identity),
    ]);
    let result = start_result("run:run-1:start", None);
    assert!(orphan_close(&run, &journal, &result).is_empty());
}

/// The by-target dedupe keys on the *child*, not the effect key: a
/// close already journaled for the orphan under a different key — the
/// cancel path's `run:<id>:close` — suppresses the `<start>:close` the
/// orphan lane would mint.
#[test]
fn differently_keyed_close_is_not_replanned() {
    let run = run("run-1", &caller(), None);
    let identity = child("p9");
    let journal = Vec::from([
        started(&run.id, "start", &identity),
        closed(&run.id, "close", &identity),
    ]);
    let result = start_result("run:run-1:start", None);
    assert!(orphan_close(&run, &journal, &result).is_empty());
}

/// The dedupe is a *close* aimed at the child, not any effect aimed at
/// it: a journaled `prompt` to the orphan leaves the `<start>:close`
/// owed.
#[test]
fn journaled_prompt_does_not_suppress_the_close() {
    let run = run("run-1", &caller(), None);
    let identity = child("p9");
    let journal = Vec::from([
        started(&run.id, "start", &identity),
        prompted(&run.id, "prompt:task", &identity),
    ]);
    let result = start_result("run:run-1:start", None);
    let planned = orphan_close(&run, &journal, &result);
    assert_eq!(planned.len(), 1);
    assert_eq!(planned[0].target, Some(EffectTarget::Child(identity)));
}

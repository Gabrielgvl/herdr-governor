//! P4.0 named tests — the Launch-row builders' contract asserted on public
//! `Transition` values; inputs come from `super::launch_row::fixtures`.

use alloc::vec::Vec;

use crate::config::ConfigVersion;
use crate::delivery::{MailboxEvent, MailboxEventKind, MailboxSubject};
use crate::identity::{
    ChildStatus, EffectKey, EventId, IdempotencyKey, LaunchId, Observation, PaneId, ProjectRoot,
    RunId, TabId, Timestamp, mint_agent_name,
};
use crate::lifecycle::{
    EffectCertainty, EffectKind, EffectOutcome, EffectReceipt, EffectResult, EffectState,
    EffectTarget, Event, Run, Settlement, State, StateChange, Transition, UnresolvedReason,
    VersionTriple, Versioned, launch_plan, op_digest, periodic_review, transition,
};
use crate::routing::{Decision, PlacementPlan};

use super::launch_row::fixtures::{
    NOW, abstain, caller, decision, ends, events, failed, identity, launch, launched, phased,
    policy, quiet, recorded, reserved, stranded_certainty, task, writes,
};
use super::{
    AbstainReason, Launch, LaunchOutcome, LaunchPhase, Task, admit, begin, decided, finish,
    new_launch,
};

use AbstainReason::{EvaluationFailed, InterruptedBeforeDecision, NoCandidates, NoHigherTier};
use EffectCertainty::{Absent, Unknown};
use EffectKind::{AgentStart, Close, JevEvaluate, PaneSplit, Prompt, TabCreate};
use EffectReceipt::{AgentStarted, TabCreated};
use EffectState::{Acknowledged, Dispatching, Failed as EvalFailed, Planned, Unconfirmed};
use LaunchOutcome::Rejected;
use LaunchPhase::{Done, Evaluating, Launching, Routed};
use MailboxEventKind::{LaunchAnswered, LaunchFailed};
use PlacementPlan::{ExistingTab, NewTab};
use Settlement::{Cancelled, Unresolved};
use State::{Active, Reserved, Settled, Starting};
use UnresolvedReason::LaunchNotStarted;

/// `value` applied to `run` through `transition()`, stamped against its
/// own version triple so the result is never stale (F20).
fn transact(run: &Run, value: Event, decision: Option<&Decision>) -> Transition {
    let stamped = Versioned {
        requested_against: VersionTriple {
            version: run.version,
            work_generation: run.work_generation,
            evidence_generation: run.evidence_generation,
        },
        value,
    };
    transition(run, &stamped, NOW, &policy(), (decision, &[], &[]), "/x")
}

fn run_in(state: State) -> Run {
    let mut run = reserved(&launch());
    run.state = state;
    run.identity = Some(identity());
    run
}

/// `value` with one field broken — the inconsistent-input cases.
fn broken<T: Clone>(value: &T, f: impl Fn(&mut T)) -> T {
    let mut clone = value.clone();
    f(&mut clone);
    clone
}

fn updates(t: &Transition) -> Vec<&Run> {
    t.state_changes
        .iter()
        .filter_map(|change| {
            let StateChange::UpdateRun(update) = change else {
                return None;
            };
            Some(&update.record)
        })
        .collect()
}

fn ack(key: &str, kind: EffectKind, receipt: EffectReceipt) -> Event {
    Event::EffectResult(EffectResult {
        key: EffectKey(key.into()),
        kind,
        outcome: EffectOutcome::Acknowledged,
        receipt: Some(receipt),
    })
}

/// `t` plans `kind` carrying a `payload_digest` (OQ-15).
fn digested(t: &Transition, kind: EffectKind) -> bool {
    t.effects
        .iter()
        .any(|e| e.kind == kind && e.payload_digest.is_some())
}

/// `(phase, outcome)` `finish` records for `l` — the terminal half of the
/// legality matrix cases.
fn finish_state(l: &Launch, o: LaunchOutcome) -> (LaunchPhase, Option<LaunchOutcome>) {
    let t = ends(l, o);
    let r = recorded(&t);
    (r.phase, r.outcome.clone())
}

#[test]
fn admit_writes_evaluating_row_and_plans_eval_effect() {
    let t = admit(&launch());
    let r = recorded(&t);
    assert_eq!(r.phase, Evaluating);
    assert!(r.outcome.is_none() && r.decision.is_none());
    assert!(r.config_version.is_none() && t.events.is_empty());
    let e = &t.effects[0];
    assert_eq!(t.effects.len(), 1);
    assert_eq!((e.kind, e.state), (JevEvaluate, Planned));
    assert_eq!(e.key.0, "launch:l-1:evaluate");
    assert_eq!(e.id.0, "eff:launch:l-1:evaluate");
    assert_eq!(e.subject_launch, Some(LaunchId("l-1".into())));
    assert!(e.subject_run.is_none() && e.target.is_none());
    assert!(e.payload_digest.is_none());
}

#[test]
fn admit_rejects_non_evaluating_launch() {
    for l in [Routed, Launching, Done].map(phased) {
        assert!(quiet(&admit(&l)));
    }
    for l in [
        broken(&launch(), |x| x.decision = Some(decision(1))),
        broken(&launch(), |x| {
            x.config_version = Some(ConfigVersion("c".into()));
        }),
        broken(&launch(), |x| x.outcome = Some(Rejected)),
    ] {
        assert!(quiet(&admit(&l)));
    }
}

#[test]
fn decided_writes_routed_launch_and_reserved_run() {
    let l = launch();
    let r = reserved(&l);
    let t = decided(&l, &decision(2), &r);
    let rec = recorded(&t);
    let cfg = ConfigVersion("cfg-1".into());
    assert_eq!(rec.phase, Routed);
    assert_eq!(rec.decision, Some(decision(2)));
    assert_eq!(rec.config_version, Some(cfg));
    assert!(rec.outcome.is_none());
    assert!(t.state_changes.contains(&StateChange::ReserveRun(r)));
}

#[test]
fn decided_requires_evaluating_and_consistent_run() {
    let d = decision(1);
    let r = reserved(&launch());
    for l in [Routed, Launching, Done].map(phased) {
        assert!(quiet(&decided(&l, &d, &r)));
    }
    let bad = [
        broken(&r, |x| x.launch = LaunchId("x".into())),
        broken(&r, |x| x.state = Starting),
        broken(&r, |x| x.child_name = "x".into()),
        broken(&r, |x| x.max_age_deadline = Timestamp(0)),
        broken(&r, |x| x.settlement = Some(Cancelled)),
    ];
    for b in &bad {
        assert!(quiet(&decided(&launch(), &d, b)));
    }
    assert!(quiet(&decided(&launch(), &decision(0), &r)));
}

#[test]
fn decision_is_persisted_immutably() {
    let routed = phased(Routed);
    let t = decided(&routed, &decision(2), &reserved(&routed));
    assert!(quiet(&t));
}

#[test]
fn begin_moves_routed_to_launching_keeping_decision() {
    let routed = phased(Routed);
    let t = begin(&routed);
    let rec = recorded(&t);
    assert_eq!(rec.phase, Launching);
    assert_eq!(rec.decision, routed.decision);
    assert_eq!(rec.config_version, routed.config_version);
    assert!(rec.outcome.is_none());
}

#[test]
fn begin_rejects_non_routed() {
    for l in [Evaluating, Launching, Done].map(phased) {
        assert!(quiet(&begin(&l)));
    }
}

#[test]
fn finish_evaluating_allows_abstain_and_reject_only() {
    let l = launch();
    for r in [EvaluationFailed, InterruptedBeforeDecision] {
        let o = abstain(r);
        assert_eq!(finish_state(&l, o.clone()), (Done, Some(o)));
    }
    for r in [NoHigherTier, NoCandidates] {
        let o = abstain(r);
        assert_eq!(finish_state(&l, o.clone()), (Done, Some(o)));
    }
    assert_eq!(finish_state(&l, Rejected), (Done, Some(Rejected)));
    for o in [launched(), failed(Absent)] {
        assert!(quiet(&ends(&l, o)));
    }
}

#[test]
fn finish_launching_allows_terminal_set() {
    let l = phased(Launching);
    for o in [launched(), abstain(NoCandidates)] {
        assert_eq!(finish_state(&l, o.clone()), (Done, Some(o)));
    }
    for o in [failed(Absent), failed(Unknown)] {
        assert_eq!(finish_state(&l, o.clone()), (Done, Some(o)));
    }
    assert!(quiet(&ends(&l, Rejected)));
}

#[test]
fn finish_routed_forbids_launched() {
    let l = phased(Routed);
    let r = reserved(&l);
    for o in [launched(), failed(Unknown), Rejected] {
        assert!(quiet(&finish(&l, o, None, Some(&r), NOW, &policy())));
    }
}

#[test]
fn finish_routed_settles_reserved_run() {
    let l = phased(Routed);
    let r = reserved(&l);
    for o in [abstain(NoCandidates), failed(Absent)] {
        let t = finish(&l, o, None, Some(&r), NOW, &policy());
        assert_eq!(recorded(&t).phase, Done);
        let u = updates(&t);
        let settled = Some(Unresolved {
            reason: LaunchNotStarted,
        });
        assert_eq!((u.len(), u[0].state), (1, Settled));
        assert_eq!(u[0].settlement, settled);
        assert_eq!(events(&t, MailboxEventKind::Settled).len(), 1);
    }
}

#[test]
fn finish_routed_requires_matching_reserved_run() {
    let l = phased(Routed);
    let r = reserved(&l);
    let bad = [
        None,
        Some(broken(&r, |x| x.launch = LaunchId("x".into()))),
        Some(broken(&r, |x| x.state = Starting)),
        Some(broken(&r, |x| x.settlement = Some(Cancelled))),
    ];
    let o = abstain(NoCandidates);
    for b in &bad {
        let t = finish(&l, o.clone(), None, b.as_ref(), NOW, &policy());
        assert!(quiet(&t));
    }
}

#[test]
fn finish_done_is_absorbing() {
    let d = phased(Done);
    for o in [launched(), abstain(NoCandidates)] {
        assert!(quiet(&ends(&d, o)));
    }
    for o in [Rejected, failed(Unknown)] {
        assert!(quiet(&ends(&d, o)));
    }
}

#[test]
fn finish_failed_emits_launch_failed_event() {
    let t = ends(&phased(Launching), failed(Absent));
    let f = events(&t, LaunchFailed);
    let want = MailboxSubject::Launch(LaunchId("l-1".into()));
    assert_eq!((f.len(), &f[0].subject), (1, &want));
    assert_eq!(f[0].dedup_key.0, "launch:l-1:launch_failed");
}

#[test]
fn finish_any_outcome_emits_launch_answered() {
    let cases: [(Launch, LaunchOutcome, usize); 4] = [
        (launch(), abstain(EvaluationFailed), 1),
        (launch(), Rejected, 1),
        (phased(Launching), launched(), 1),
        (phased(Launching), failed(Unknown), 2),
    ];
    let want = MailboxSubject::Launch(LaunchId("l-1".into()));
    for (l, o, n) in cases {
        let s = o.as_str();
        let t = ends(&l, o);
        let a = events(&t, LaunchAnswered);
        assert_eq!((a.len(), t.events.len(), &a[0].subject), (1, n, &want));
        assert!(a[0].dedup_key.0 == "launch:l-1:launch_answered" && a[0].body.contains(s));
    }
}

#[test]
fn launch_answered_binds_launch_only() {
    let emitted =
        |s, q| MailboxEvent::emitted(EventId("e".into()), s, LaunchAnswered, q, "{}".into());
    let ok = MailboxSubject::Launch(LaunchId("l-1".into()));
    let run = MailboxSubject::Run(RunId("r-1".into()));
    assert!(emitted(run, None).is_none());
    assert!(emitted(ok.clone(), Some(3)).is_none());
    let got = emitted(ok, None).map(|e| e.dedup_key.0);
    assert_eq!(got.as_deref(), Some("launch:l-1:launch_answered"));
}

#[test]
fn finish_evaluating_abstain_fails_stranded_eval() {
    let l = launch();
    for r in [EvaluationFailed, InterruptedBeforeDecision] {
        for s in [Planned, Dispatching, Unconfirmed] {
            let t = finish(&l, abstain(r), Some(s), None, NOW, &policy());
            let w = writes(&t);
            assert_eq!(w.len(), 1);
            assert_eq!(w[0].key.0, "launch:l-1:evaluate");
            assert_eq!(w[0].state, EvalFailed);
            assert!(w[0].certainty.is_some());
        }
    }
    let routed = phased(Routed);
    let run = reserved(&routed);
    let no_writes = |dl: &Launch, o: LaunchOutcome, rr: Option<&Run>| {
        writes(&finish(dl, o, Some(Planned), rr, NOW, &policy())).is_empty()
    };
    assert!(no_writes(&l, Rejected, None));
    assert!(no_writes(&routed, abstain(NoCandidates), Some(&run)));
    assert!(no_writes(&phased(Launching), abstain(NoCandidates), None));
}

#[test]
fn stranded_eval_planned_writes_absent() {
    assert_eq!(stranded_certainty(Some(Planned)), Some(Absent));
}

#[test]
fn stranded_eval_unconfirmed_writes_unknown() {
    assert_eq!(stranded_certainty(Some(Unconfirmed)), Some(Unknown));
}

#[test]
fn stranded_eval_dispatching_writes_unknown() {
    assert_eq!(stranded_certainty(Some(Dispatching)), Some(Unknown));
}

#[test]
fn stranded_eval_terminal_or_missing_writes_none() {
    let o = abstain(EvaluationFailed);
    for s in [Some(Acknowledged), Some(EvalFailed), None] {
        assert_eq!(stranded_certainty(s), None);
        let t = finish(&launch(), o.clone(), s, None, NOW, &policy());
        assert_eq!(recorded(&t).phase, Done);
    }
}

#[test]
fn op_digest_is_canonical_and_recomputable() {
    let target = EffectTarget::ExistingTab(TabId("w6:t1".into()));
    let other = EffectTarget::ExistingTab(TabId("w6:t2".into()));
    let d = op_digest(PaneSplit, Some(&target), &[]);
    assert_eq!(d, op_digest(PaneSplit, Some(&target), &[]));
    assert_ne!(d, op_digest(TabCreate, Some(&target), &[]));
    assert_ne!(d, op_digest(PaneSplit, None, &[]));
    assert_ne!(d, op_digest(PaneSplit, Some(&target), b"p"));
    assert_ne!(d, op_digest(PaneSplit, Some(&other), &[]));
    let plan = ExistingTab {
        tab: TabId("w6:t1".into()),
    };
    let pane = PaneId("w0:p0".into());
    let t = launch_plan(&run_in(Reserved), &decision(1), &plan, &pane);
    let e = &t.effects[0];
    let want = op_digest(e.kind, e.target.as_ref(), &[]);
    assert_eq!(e.payload_digest, Some(want));
}

#[test]
fn planned_effect_stores_digest_for_herdr_kinds() {
    let d = decision(1);
    let pane = PaneId("w0:p0".into());
    let tab = TabId("w6:t1".into());
    let run = run_in(Reserved);
    for (plan, kind) in [
        (NewTab, TabCreate),
        (ExistingTab { tab: tab.clone() }, PaneSplit),
    ] {
        let t = launch_plan(&run, &d, &plan, &pane);
        assert!(digested(&t, kind));
    }
    let starting = run_in(Starting);
    let receipt = TabCreated { tab, pane };
    let ev_tab = ack("run:r-1:tab", TabCreate, receipt);
    let on_tab = transact(&starting, ev_tab, Some(&d));
    assert!(digested(&on_tab, AgentStart));
    let started = AgentStarted {
        identity: identity(),
    };
    let ev_start = ack("run:r-1:start:0", AgentStart, started);
    let on_start = transact(&starting, ev_start, Some(&d));
    assert!(digested(&on_start, Prompt));
    let obs = Event::Obs {
        observation: Observation::Unique {
            status: Some(ChildStatus::Idle),
            pane: PaneId("w0:p1".into()),
            native_session: None,
        },
        handoff_reading: None,
    };
    let on_idle = transact(&run_in(Active), obs, None);
    assert!(digested(&on_idle, Prompt));
    let ev_cancel = Event::Cancel { close_pane: true };
    let on_cancel = transact(&run_in(Settled), ev_cancel, None);
    assert!(digested(&on_cancel, Close));
    let eval = &admit(&launch()).effects[0];
    assert!(eval.payload_digest.is_none());
    let review = periodic_review(&run_in(Active), false, &[]);
    assert!(review.is_some_and(|e| e.payload_digest.is_none()));
}

#[test]
fn outcome_and_reason_columns() {
    for (o, s) in [
        (launched(), "launched"),
        (abstain(NoCandidates), "abstained"),
        (Rejected, "rejected"),
        (failed(Absent), "failed"),
    ] {
        assert_eq!(o.as_str(), s);
    }
    for r in [EvaluationFailed, InterruptedBeforeDecision] {
        assert_eq!(abstain(r).reason_str(), Some(r.as_str()));
    }
    for r in [NoHigherTier, NoCandidates] {
        assert_eq!(abstain(r).reason_str(), Some(r.as_str()));
    }
    for o in [launched(), Rejected, failed(Absent)] {
        assert_eq!(o.reason_str(), None);
    }
}

#[test]
fn admit_then_decided_then_begin_then_finish_compose() {
    let l = launch();
    assert_eq!(recorded(&admit(&l)).phase, Evaluating);
    let t = decided(&l, &decision(1), &reserved(&l));
    let routed = recorded(&t).clone();
    assert_eq!(routed.phase, Routed);
    let launching = recorded(&begin(&routed)).clone();
    assert_eq!(launching.phase, Launching);
    let done = ends(&launching, launched());
    let terminal = recorded(&done);
    assert_eq!(terminal.phase, Done);
    let s = terminal.outcome.as_ref().map(LaunchOutcome::as_str);
    assert_eq!(s, Some("launched"));
    assert_eq!(terminal.decision, Some(decision(1)));
    assert_eq!(events(&done, LaunchAnswered).len(), 1);
}

#[test]
fn reserved_run_is_well_formed() {
    let l = launch();
    let r = reserved(&l);
    assert_eq!((r.state, r.version), (Reserved, 0));
    assert_eq!(r.launch, l.id);
    assert_eq!(r.child_name, mint_agent_name(&r.id).0);
    assert_eq!(r.max_age_deadline, NOW.after(policy().max_age));
    assert_eq!(r.owner, l.caller);
    assert!(r.settlement.is_none() && r.settled_at.is_none());
    assert!(r.identity.is_none());
}

#[test]
fn new_launch_is_evaluating_with_key_scope() {
    let task = task();
    let l = new_launch(
        LaunchId("l-1".into()),
        caller(),
        ProjectRoot("/repo".into()),
        IdempotencyKey("key-1".into()),
        task.clone(),
    );
    assert_eq!(l.phase, Evaluating);
    assert!(l.outcome.is_none() && l.decision.is_none());
    assert!(l.config_version.is_none());
    assert_eq!(l.digest_version, Task::DIGEST_VERSION);
    assert_eq!(l.task_digest, task.digest());
    assert_eq!(l.caller, caller());
    assert_eq!(l.project_root, ProjectRoot("/repo".into()));
    assert_eq!(l.idempotency_key, IdempotencyKey("key-1".into()));
}

//! F30 — the retirement proof gate and the bounded `retire` close
//! family: a `close` plans only for an `accepted` Run whose whole §4.16
//! proof chain holds, at most three attempts under
//! `run:<id>:retire[:<n>]`.

use alloc::format;
use alloc::vec::Vec;

use crate::identity::{
    AgentKind, AgentName, CallerKey, ChildIdentity, EffectId, EffectKey, HerdrIncarnation,
    LaunchId, NativeSession, PaneId, RunId, TerminalId, Timestamp,
};
use crate::lifecycle::{
    Effect, EffectKind, EffectState, EffectTarget, Run, Settlement, State, UnresolvedReason,
};
use crate::retirement::{
    AnchorProof, ArtifactProof, CallerProof, ComposerProof, IdentityBinding, LaneObservation,
    RETIRE_MAX_ATTEMPTS, RetirementProof, StabilityProof, TraceProof, retire_close,
};
use crate::task::Retention;

fn run(settlement: Option<Settlement>) -> Run {
    Run {
        id: RunId("r-1".into()),
        launch: LaunchId("l-1".into()),
        owner: CallerKey {
            agent_kind: AgentKind("caller-kind".into()),
            native_session: NativeSession("caller-sess".into()),
        },
        owner_generation: 0,
        version: 9,
        state: State::Settled,
        prompt_certainty: None,
        child_name: "gov-00000001".into(),
        identity: Some(ChildIdentity {
            herdr_incarnation: HerdrIncarnation("inc-1".into()),
            terminal_id: TerminalId("term-1".into()),
            agent_kind: AgentKind("kind-1".into()),
            agent_name: AgentName("gov-r1".into()),
            native_session: Some(NativeSession("sess-1".into())),
            pane_id: PaneId("w0:p1".into()),
        }),
        operating_point: None,
        provider: None,
        tier_start: None,
        cwd: "/repo".into(),
        base_commit: None,
        work_generation: 1,
        evidence_generation: 0,
        evidence_digest: None,
        child_status: None,
        idle_since: None,
        idle_deadline: None,
        repair_deadline: None,
        rejected_at: None,
        judgment_deadline: None,
        judging_digest: None,
        max_age_deadline: Timestamp(1_000),
        nudge_episode: 0,
        nudged_episode: None,
        blocked_episode: 0,
        settlement,
        settled_at: settlement.map(|_| Timestamp(900)),
    }
}

/// The §4.16 chain all-green.
fn holding() -> RetirementProof {
    RetirementProof {
        retention: Retention::Retire,
        binding: IdentityBinding::Bound,
        anchor: AnchorProof::Held,
        observation: LaneObservation::IdleOrDone,
        callers: CallerProof::Clear,
        stability: StabilityProof::Elapsed,
        artifact: ArtifactProof::Held,
        trace: TraceProof::Held,
        composer: ComposerProof::Held,
        enabled: true,
    }
}

/// `retire_close` with `edit` applied to a holding proof.
fn broken(edit: impl FnOnce(&mut RetirementProof)) -> Option<Effect> {
    let mut proof = holding();
    edit(&mut proof);
    retire_close(&run(Some(Settlement::Accepted)), &proof, &[])
}

/// A journaled `close` row under the retire family.
fn close_row(suffix: &str, state: EffectState) -> Effect {
    let key = EffectKey(format!("run:r-1:{suffix}"));
    Effect {
        id: EffectId(format!("eff:{}", key.0)),
        key,
        kind: EffectKind::Close,
        subject_launch: Some(LaunchId("l-1".into())),
        subject_run: Some(RunId("r-1".into())),
        target: None,
        payload_digest: None,
        state,
        certainty: None,
        receipt: None,
        dispatched_at: None,
    }
}

/// One row per §4.16 fail-closed reason — each breaks exactly one proof
/// field of the all-green chain.
type FailEdit = fn(&mut RetirementProof);
const FAIL_CLOSED: &[(&str, FailEdit)] = &[
    ("retention opted out", |p| {
        p.retention = Retention::Keep;
    }),
    ("identity unbound", |p| {
        p.binding = IdentityBinding::Unbound;
    }),
    ("unsupported kind", |p| {
        p.binding = IdentityBinding::UnsupportedKind;
    }),
    ("anchor missing", |p| {
        p.anchor = AnchorProof::AnchorMissing;
    }),
    ("history missing", |p| {
        p.anchor = AnchorProof::HistoryMissing;
    }),
    ("follow-up at freeze", |p| {
        p.anchor = AnchorProof::FollowUpSeen;
    }),
    ("child absent", |p| {
        p.observation = LaneObservation::Absent;
    }),
    ("lane deferred", |p| {
        p.observation = LaneObservation::Defer;
    }),
    ("pane is a caller", |p| {
        p.callers = CallerProof::PaneIsCaller;
    }),
    ("child is a caller", |p| {
        p.callers = CallerProof::ChildIsCaller;
    }),
    ("still watching", |p| {
        p.stability = StabilityProof::Watching;
    }),
    ("artifact changed", |p| {
        p.artifact = ArtifactProof::Changed;
    }),
    ("artifact missing", |p| {
        p.artifact = ArtifactProof::Missing;
    }),
    ("trace follow-up", |p| {
        p.trace = TraceProof::FollowUp;
    }),
    ("trace rewritten", |p| {
        p.trace = TraceProof::SourceRewritten;
    }),
    ("trace budget", |p| {
        p.trace = TraceProof::SourceExceedsBudget;
    }),
    ("trace malformed", |p| {
        p.trace = TraceProof::SourceMalformed;
    }),
    ("trace unreadable", |p| {
        p.trace = TraceProof::SourceUnreadable;
    }),
    ("trace pending tail", |p| {
        p.trace = TraceProof::PendingTail;
    }),
    ("trace ambiguous", |p| {
        p.trace = TraceProof::Ambiguous;
    }),
    ("composer queued", |p| {
        p.composer = ComposerProof::Queued;
    }),
    ("composer draft", |p| {
        p.composer = ComposerProof::Draft;
    }),
    ("composer unreadable", |p| {
        p.composer = ComposerProof::Unreadable;
    }),
    ("dry-run only", |p| {
        p.enabled = false;
    }),
];

#[test]
fn f30_retire_close_plans_only_when_every_proof_holds() {
    let settled = run(Some(Settlement::Accepted));
    let Some(close) = retire_close(&settled, &holding(), &[]) else {
        panic!("a complete proof on an accepted Run plans the close")
    };
    assert_eq!(close.kind, EffectKind::Close, "retirement plans a close");
    assert_eq!(close.key.0, "run:r-1:retire", "the first attempt is retire");
    assert_eq!(close.state, EffectState::Planned, "effects land planned");
    assert_eq!(
        close.target,
        Some(EffectTarget::Child(
            settled
                .identity
                .clone()
                .expect("a settled run has identity")
        )),
        "the close targets the Run's captured identity"
    );
    assert_eq!(
        close.subject_run,
        Some(RunId("r-1".into())),
        "the effect binds the Run"
    );
    for (name, edit) in FAIL_CLOSED {
        assert_eq!(broken(*edit), None, "check {name} fails closed (§4.16)");
    }
}

#[test]
fn f30_retire_close_attempts_are_bounded_to_three() {
    assert_eq!(RETIRE_MAX_ATTEMPTS, 3, "the retire family is bounded at 3");
    let settled = run(Some(Settlement::Accepted));
    let first = retire_close(&settled, &holding(), &[]).expect("attempt zero plans");
    assert_eq!(first.key.0, "run:r-1:retire");

    let mut journal = Vec::from([close_row("retire", EffectState::Failed)]);
    let second = retire_close(&settled, &holding(), &journal).expect("attempt one plans");
    assert_eq!(second.key.0, "run:r-1:retire:1", "a failed attempt re-keys");

    journal.push(close_row("retire:1", EffectState::Unconfirmed));
    let third = retire_close(&settled, &holding(), &journal).expect("attempt two plans");
    assert_eq!(
        third.key.0, "run:r-1:retire:2",
        "an unconfirmed attempt re-keys too"
    );

    journal.push(close_row("retire:2", EffectState::Failed));
    assert_eq!(
        retire_close(&settled, &holding(), &journal),
        None,
        "the third terminal attempt ends the family — never a fourth"
    );

    for state in [
        EffectState::Planned,
        EffectState::Dispatching,
        EffectState::Acknowledged,
    ] {
        let in_flight = Vec::from([close_row("retire", state)]);
        assert_eq!(
            retire_close(&settled, &holding(), &in_flight),
            None,
            "a {state:?} close suppresses a new attempt"
        );
    }
    // An unrelated family in flight does not disturb the retire plan.
    let mut other = close_row("blocked:0", EffectState::Planned);
    other.kind = EffectKind::JevEvaluate;
    let other_journal = Vec::from([other]);
    assert_eq!(
        retire_close(&settled, &holding(), &other_journal)
            .map(|effect| effect.key.0)
            .as_deref(),
        Some("run:r-1:retire"),
        "another family in flight still plans retire"
    );
}

#[test]
fn f30_retire_close_never_plans_for_non_accepted_settlements() {
    for settlement in [
        Settlement::Rejected,
        Settlement::NoHandoff,
        Settlement::PaneLost,
        Settlement::Cancelled,
        Settlement::ProviderLimited,
        Settlement::Unresolved {
            reason: UnresolvedReason::MaxAge,
        },
    ] {
        assert_eq!(
            retire_close(&run(Some(settlement)), &holding(), &[]),
            None,
            "{settlement:?} never retires — accepted Runs only (ADR-0003)"
        );
    }
    assert_eq!(
        retire_close(&run(None), &holding(), &[]),
        None,
        "an unsettled Run never retires"
    );
    // A `keep` Task's Run never retires even when everything else holds.
    assert_eq!(
        retire_close(
            &run(Some(Settlement::Accepted)),
            &RetirementProof {
                retention: Retention::Keep,
                ..holding()
            },
            &[]
        ),
        None,
        "the retention opt-out holds even at the proof boundary"
    );
}

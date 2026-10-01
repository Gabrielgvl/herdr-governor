//! `Run` generators: arbitrary but well-typed lifecycle states, settlements,
//! child identities and the record the deadline/safety proofs drive.

use governor_core::config::{OperatingPointId, Provider, Tier};
use governor_core::identity::{
    AgentKind, AgentName, CallerKey, ChildIdentity, ChildStatus, HerdrIncarnation, LaunchId,
    NativeSession, PaneId, RunId, TerminalId, Timestamp,
};
use governor_core::lifecycle::{PromptCertainty, Run, Settlement, State, UnresolvedReason};
use proptest::option;
use proptest::prelude::{Strategy, prop_oneof};

use super::common_strategies::{RUN_ID, arb_digest, arb_timestamp, pick, text};

fn arb_state() -> impl Strategy<Value = State> {
    pick(&[
        State::Reserved,
        State::Starting,
        State::Prompting,
        State::Active,
        State::Judging,
        State::Repair,
        State::Settled,
    ])
}

fn arb_settlement() -> impl Strategy<Value = Settlement> {
    prop_oneof![
        pick(&[
            Settlement::Accepted,
            Settlement::Rejected,
            Settlement::NoHandoff,
            Settlement::PaneLost,
            Settlement::Cancelled,
            Settlement::ProviderLimited,
        ]),
        pick(&[
            UnresolvedReason::LaunchNotStarted,
            UnresolvedReason::LaunchFailed,
            UnresolvedReason::JudgmentUnavailable,
            UnresolvedReason::IdentityUnprovable,
            UnresolvedReason::MaxAge,
        ])
        .prop_map(|reason| Settlement::Unresolved { reason }),
    ]
}

pub(crate) fn arb_child_status() -> impl Strategy<Value = ChildStatus> {
    pick(&[
        ChildStatus::Working,
        ChildStatus::Idle,
        ChildStatus::Done,
        ChildStatus::Blocked,
    ])
}

pub fn arb_identity() -> impl Strategy<Value = ChildIdentity> {
    (0_u8..4, option::of(0_u8..4)).prop_map(|(pane, session)| ChildIdentity {
        herdr_incarnation: HerdrIncarnation(text("inc-1")),
        terminal_id: TerminalId(text("term-1")),
        agent_kind: AgentKind(text("kind-1")),
        agent_name: AgentName(text("gov-r1")),
        native_session: session.map(|s| NativeSession(format!("sess-{s}"))),
        pane_id: PaneId(format!("w0:p{pane}")),
    })
}

/// The fixed-field `Run` the generators overlay — identifiers are shared
/// constants; `arb_run_in` sets the lifecycle-relevant fields.
fn base_run(state: State) -> Run {
    Run {
        id: RunId(text(RUN_ID)),
        launch: LaunchId(text("l-1")),
        owner: CallerKey {
            agent_kind: AgentKind(text("caller-kind")),
            native_session: NativeSession(text("caller-sess")),
        },
        owner_generation: 0,
        version: 0,
        state,
        prompt_certainty: None,
        child_name: text("gov-00000001"),
        identity: None,
        operating_point: Some(OperatingPointId(text("op-1"))),
        provider: Some(Provider(text("prov-1"))),
        tier_start: Some(Tier(text("t1"))),
        cwd: text("/repo"),
        base_commit: None,
        work_generation: 0,
        evidence_generation: 0,
        evidence_digest: None,
        child_status: None,
        idle_since: None,
        idle_deadline: None,
        repair_deadline: None,
        rejected_at: None,
        judgment_deadline: None,
        judging_digest: None,
        max_age_deadline: Timestamp(0),
        nudge_episode: 0,
        nudged_episode: None,
        blocked_episode: 0,
        settlement: None,
        settled_at: None,
    }
}

fn arb_run_in(states: impl Strategy<Value = State>) -> impl Strategy<Value = Run> {
    (
        states,
        0_u64..64,                                  // version
        (0_u64..8, 0_u64..8),                       // work_generation, evidence_generation
        (0_u64..8, option::of(0_u64..8), 0_u64..8), // nudge_episode, nudged_episode, blocked_episode
        (
            option::of(arb_timestamp()),
            option::of(arb_timestamp()),
            option::of(arb_timestamp()),
            option::of(arb_timestamp()),
            option::of(arb_timestamp()),
            arb_timestamp(),
        ), // idle_since, idle/repair/judgment deadlines, rejected_at, max_age
        (
            option::of(arb_child_status()),
            option::of(pick(&[
                PromptCertainty::Acknowledged,
                PromptCertainty::Unconfirmed,
            ])),
            arb_settlement(),
            arb_timestamp(),
            arb_identity(),
            option::of((0_u8..4).prop_map(|i| Provider(format!("prov-{i}")))),
            option::of(arb_digest()),
        ),
    )
        .prop_map(|(state, version, gens, episodes, times, rest)| {
            let (work_generation, evidence_generation) = gens;
            let (nudge_episode, nudged_episode, blocked_episode) = episodes;
            let (
                idle_since,
                idle_deadline,
                repair_deadline,
                rejected_at,
                judgment_deadline,
                max_age,
            ) = times;
            let (
                child_status,
                gen_certainty,
                gen_settlement,
                gen_settled_at,
                gen_identity,
                provider,
                evidence_digest,
            ) = rest;
            // the record invariants: identity exists once a start could have
            // been acknowledged, prompt_certainty once the prompt could have
            // resolved, settlement iff `settled` (the Appendix B checks)
            let (identity, prompt_certainty) = match state {
                State::Reserved | State::Starting => (None, None),
                State::Prompting => (Some(gen_identity), None),
                State::Active | State::Judging | State::Repair | State::Settled => {
                    (Some(gen_identity), gen_certainty)
                }
            };
            let (settlement, settled_at) = if state == State::Settled {
                (Some(gen_settlement), Some(gen_settled_at))
            } else {
                (None, None)
            };
            Run {
                version,
                work_generation,
                evidence_generation,
                evidence_digest,
                nudge_episode,
                nudged_episode,
                blocked_episode,
                idle_since,
                idle_deadline,
                repair_deadline,
                rejected_at,
                judgment_deadline,
                max_age_deadline: max_age,
                child_status,
                provider,
                identity,
                prompt_certainty,
                settlement,
                settled_at,
                ..base_run(state)
            }
        })
}

/// An arbitrary well-typed `Run` in any lifecycle state.
pub fn arb_run() -> impl Strategy<Value = Run> {
    arb_run_in(arb_state())
}

/// An arbitrary well-typed `Run` that has not settled — the liveness
/// property's domain.
pub fn arb_unsettled_run() -> impl Strategy<Value = Run> {
    arb_run_in(arb_state().prop_filter("unsettled", |state| *state != State::Settled))
}

/// The `reserved` Run a launch starts from: empty journal, only
/// `max_age_deadline` armed (F13/F22).
pub fn arb_reserved_run() -> impl Strategy<Value = Run> {
    (0_u64..4, arb_timestamp()).prop_map(|(version, max_age_deadline)| {
        let mut run = base_run(State::Reserved);
        run.version = version;
        run.max_age_deadline = max_age_deadline;
        run
    })
}

//! Test fixtures for the delivery module — constructed inputs, no I/O.

use alloc::vec::Vec;

use crate::config::Capability;
use crate::delivery::{MessageBody, OutboxMessage, OutboxState};
use crate::identity::{
    AgentKind, AgentName, CallerKey, ChildIdentity, ChildStatus, Digest, EffectId, EffectKey,
    HerdrIncarnation, LaunchId, MessageKey, NativeSession, Observation, PaneId, RunId, TerminalId,
    Timestamp,
};
use crate::lifecycle::{
    Effect, EffectKind, EffectState, EffectTarget, PromptCertainty, Run, Settlement, State,
};

pub(super) fn digest(byte: u8) -> Digest {
    Digest([byte; 32])
}

pub(super) fn caller() -> CallerKey {
    CallerKey {
        agent_kind: AgentKind("kind-owner".into()),
        native_session: NativeSession("sess-owner".into()),
    }
}

pub(super) fn stranger() -> CallerKey {
    CallerKey {
        agent_kind: AgentKind("kind-stranger".into()),
        native_session: NativeSession("sess-stranger".into()),
    }
}

pub(super) fn child_identity() -> ChildIdentity {
    ChildIdentity {
        herdr_incarnation: HerdrIncarnation("inc-1".into()),
        terminal_id: TerminalId("term-1".into()),
        agent_kind: AgentKind("kind-child".into()),
        agent_name: AgentName("gov-deadbeef".into()),
        native_session: Some(NativeSession("sess-child".into())),
        pane_id: PaneId("w6:p2".into()),
    }
}

/// An `active` run with an acknowledged Task prompt and an idle child —
/// the neutral base each test perturbs one field of.
pub(super) fn run() -> Run {
    Run {
        id: RunId("r1".into()),
        launch: LaunchId("l1".into()),
        owner: caller(),
        owner_generation: 0,
        version: 3,
        state: State::Active,
        prompt_certainty: Some(PromptCertainty::Acknowledged),
        child_name: "gov-deadbeef".into(),
        identity: Some(child_identity()),
        operating_point: None,
        provider: None,
        tier_start: None,
        cwd: "/proj".into(),
        base_commit: None,
        work_generation: 0,
        evidence_generation: 0,
        evidence_digest: None,
        child_status: Some(ChildStatus::Idle),
        idle_since: None,
        idle_deadline: None,
        repair_deadline: None,
        rejected_at: None,
        judgment_deadline: None,
        judging_digest: None,
        max_age_deadline: Timestamp(86_400_000),
        nudge_episode: 0,
        nudged_episode: None,
        blocked_episode: 0,
        settlement: None,
        settled_at: None,
    }
}

/// A queued outbox entry on `run()` — mutate `state`/`effect` for the
/// dispatched states (a dispatched row carries its `effect_id`).
pub(super) fn message(seq: u64, key: &str) -> OutboxMessage {
    OutboxMessage {
        run: RunId("r1".into()),
        seq,
        message_key: MessageKey(key.into()),
        sender: caller(),
        body_digest: digest(7),
        body: MessageBody::Inline("body".into()),
        state: OutboxState::Queued,
        effect: None,
        expiry_reason: None,
    }
}

pub(super) fn dispatched(seq: u64, key: &str, state: OutboxState) -> OutboxMessage {
    OutboxMessage {
        state,
        effect: Some(EffectId("eff".into())),
        ..message(seq, key)
    }
}

pub(super) fn settled_run() -> Run {
    Run {
        state: State::Settled,
        settlement: Some(Settlement::NoHandoff),
        settled_at: Some(Timestamp(9_000)),
        ..run()
    }
}

/// A `prompt` effect to the child's captured identity, in `state`.
pub(super) fn child_prompt_effect(state: EffectState) -> Effect {
    Effect {
        id: EffectId("e1".into()),
        key: EffectKey("run:r1:prompt:task".into()),
        kind: EffectKind::Prompt,
        subject_launch: None,
        subject_run: Some(RunId("r1".into())),
        target: Some(EffectTarget::Child(child_identity())),
        payload_digest: None,
        state,
        certainty: None,
        receipt: None,
        dispatched_at: None,
    }
}

pub(super) fn qualified(caps: &[&'static str]) -> Vec<Capability> {
    caps.iter().map(|name| Capability((*name).into())).collect()
}

pub(super) fn owner_pane(
    status: Option<ChildStatus>,
    session: Option<NativeSession>,
) -> Observation {
    Observation::Unique {
        status,
        pane: PaneId("w6:p1".into()),
        native_session: session,
    }
}

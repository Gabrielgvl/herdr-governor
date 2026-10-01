mod appendix_c;
mod builders;
mod f20;
mod f20_settle;
mod f21;
mod f22;
mod f22_active;
mod f22_judging;
mod f22_launch_plan;
mod f22_prompting;
mod f22_repair;
mod f22_reserved;
mod f22_starting;
mod f23;
mod f23_review;
mod f24;
mod f24_repair_window;
mod f25;

use super::{
    DeadlineKind, EffectCertainty, EffectKind, EffectReceipt, EffectState, EffectTarget,
    PromptCertainty, Settlement, State, UnresolvedReason,
};
use crate::identity::{
    AgentKind, AgentName, ChildIdentity, HerdrIncarnation, NativeSession, PaneId, TabId, TerminalId,
};
use crate::routing::PlacementPlan;

#[test]
fn appendix_c_state_spellings() {
    assert_eq!(State::Reserved.as_str(), "reserved");
    assert_eq!(State::Starting.as_str(), "starting");
    assert_eq!(State::Prompting.as_str(), "prompting");
    assert_eq!(State::Active.as_str(), "active");
    assert_eq!(State::Judging.as_str(), "judging");
    assert_eq!(State::Repair.as_str(), "repair");
    assert_eq!(State::Settled.as_str(), "settled");
}

#[test]
fn f16_prompt_certainty_spellings() {
    assert_eq!(PromptCertainty::Acknowledged.as_str(), "acknowledged");
    assert_eq!(PromptCertainty::Unconfirmed.as_str(), "unconfirmed");
}

#[test]
fn appendix_c_deadline_kind_spellings() {
    assert_eq!(DeadlineKind::Idle.as_str(), "idle");
    assert_eq!(DeadlineKind::Repair.as_str(), "repair");
    assert_eq!(DeadlineKind::Judgment.as_str(), "judgment");
    assert_eq!(DeadlineKind::MaxAge.as_str(), "max_age");
}

#[test]
fn f20_unresolved_reason_spellings() {
    assert_eq!(
        UnresolvedReason::LaunchNotStarted.as_str(),
        "launch_not_started"
    );
    assert_eq!(UnresolvedReason::LaunchFailed.as_str(), "launch_failed");
    assert_eq!(
        UnresolvedReason::JudgmentUnavailable.as_str(),
        "judgment_unavailable"
    );
    assert_eq!(
        UnresolvedReason::IdentityUnprovable.as_str(),
        "identity_unprovable"
    );
    assert_eq!(UnresolvedReason::MaxAge.as_str(), "max_age");
}

#[test]
fn f20_settlement_spellings() {
    assert_eq!(Settlement::Accepted.as_str(), "accepted");
    assert_eq!(Settlement::Rejected.as_str(), "rejected");
    assert_eq!(Settlement::NoHandoff.as_str(), "no_handoff");
    assert_eq!(Settlement::PaneLost.as_str(), "pane_lost");
    assert_eq!(Settlement::Cancelled.as_str(), "cancelled");
    assert_eq!(Settlement::ProviderLimited.as_str(), "provider_limited");
    // `unresolved` never carries its reason in the settlement spelling —
    // the reason rides `runs.settlement_reason` (Appendix B).
    for reason in [
        UnresolvedReason::LaunchNotStarted,
        UnresolvedReason::LaunchFailed,
        UnresolvedReason::JudgmentUnavailable,
        UnresolvedReason::IdentityUnprovable,
        UnresolvedReason::MaxAge,
    ] {
        assert_eq!(Settlement::Unresolved { reason }.as_str(), "unresolved");
    }
}

#[test]
fn f8_effect_kind_spellings() {
    assert_eq!(EffectKind::JevEvaluate.as_str(), "jev_evaluate");
    assert_eq!(EffectKind::TabCreate.as_str(), "tab_create");
    assert_eq!(EffectKind::PaneSplit.as_str(), "pane_split");
    assert_eq!(EffectKind::AgentStart.as_str(), "agent_start");
    assert_eq!(EffectKind::Prompt.as_str(), "prompt");
    assert_eq!(EffectKind::Close.as_str(), "close");
}

#[test]
fn f8_effect_state_spellings() {
    assert_eq!(EffectState::Planned.as_str(), "planned");
    assert_eq!(EffectState::Dispatching.as_str(), "dispatching");
    assert_eq!(EffectState::Acknowledged.as_str(), "acknowledged");
    assert_eq!(EffectState::Failed.as_str(), "failed");
    assert_eq!(EffectState::Unconfirmed.as_str(), "unconfirmed");
}

#[test]
fn f8_effect_certainty_spellings() {
    assert_eq!(EffectCertainty::Absent.as_str(), "absent");
    assert_eq!(EffectCertainty::Unknown.as_str(), "unknown");
}

#[test]
fn f8_f14_effect_target_matches_kind() {
    let identity = ChildIdentity {
        herdr_incarnation: HerdrIncarnation("inc-1".into()),
        terminal_id: TerminalId("term-1".into()),
        agent_kind: AgentKind("kind-1".into()),
        agent_name: AgentName("gov-deadbeef".into()),
        native_session: Some(NativeSession("sess-1".into())),
        pane_id: PaneId("w6:p2".into()),
    };
    // Every variant pairs with the spec kind that addresses it — the
    // exhaustive match is the shape pin (F8/F10/F14).
    let kind_of = |target: &EffectTarget| match target {
        EffectTarget::ExistingTab(_) => EffectKind::PaneSplit,
        EffectTarget::CallerContext(_) => EffectKind::TabCreate,
        EffectTarget::AgentPane(_) => EffectKind::AgentStart,
        EffectTarget::Child(_) => EffectKind::Prompt,
    };
    let cases = [
        (
            EffectTarget::ExistingTab(TabId("t1".into())),
            EffectKind::PaneSplit,
        ),
        (
            EffectTarget::CallerContext(PaneId("w6:p1".into())),
            EffectKind::TabCreate,
        ),
        (
            EffectTarget::AgentPane(PlacementPlan::ExistingTab {
                tab: TabId("t1".into()),
            }),
            EffectKind::AgentStart,
        ),
        // A NewTab placement's agent pane comes from tab_create's
        // initial pane — it never maps to pane_split (F14/H#102).
        (
            EffectTarget::AgentPane(PlacementPlan::NewTab),
            EffectKind::AgentStart,
        ),
        (EffectTarget::Child(identity), EffectKind::Prompt),
    ];
    for (target, kind) in cases {
        assert_eq!(
            kind_of(&target),
            kind,
            "target must pair with its spec kind"
        );
    }
}

#[test]
fn f14_new_tab_initial_pane_is_used() {
    // F14/H#102 — tab_create's receipt reports the initial pane so a
    // NewTab plan's agent_start launches into it (no pane_split).
    let receipt = EffectReceipt::TabCreated {
        tab: TabId("t9".into()),
        pane: PaneId("t9:p1".into()),
    };
    let initial = match receipt {
        EffectReceipt::TabCreated { tab: _, pane } => Some(pane),
        EffectReceipt::AgentStarted { .. }
        | EffectReceipt::Judgments(_)
        | EffectReceipt::PaneCreated { .. } => None,
    };
    assert_eq!(initial, Some(PaneId("t9:p1".into())));
}

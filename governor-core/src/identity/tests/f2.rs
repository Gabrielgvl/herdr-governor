//! F2 — child identity: the minted agent name and the pre-prompt match.

use alloc::vec::Vec;

use super::builders::{child, occupied};
use crate::identity::{
    AgentName, ChildStatus, HerdrIncarnation, Observation, PaneId, RunId, classify, mint_agent_name,
};

#[test]
fn f2_agent_name_mints_gov_run_prefix() {
    let name = mint_agent_name(&RunId("018f3c2a-7b1d-7e90-8abc-0123456789ab".into()));
    assert_eq!(
        name,
        AgentName("gov-018f3c2a".into()),
        "F2 — the name is gov-<runId[0..8]> (H#52)"
    );
    let short = mint_agent_name(&RunId("r1".into()));
    assert_eq!(
        short,
        AgentName("gov-r1".into()),
        "F2 — the mint takes at most eight characters, never panics"
    );
}

#[test]
fn f2_first_four_parts_suffice_before_prompt() {
    // F2 — before the first prompt, incarnation/terminal/kind/name are
    // enough: a sessionless identity still matches its pane.
    let identity = child("w6:p9", None);
    let agents = Vec::from([occupied(
        "w6:p3",
        "term-1",
        "kind-1",
        "gov-deadbeef",
        None,
        Some(ChildStatus::Working),
    )]);
    let inc = HerdrIncarnation("inc-1".into());
    assert_eq!(
        classify(&identity, Some(&inc), &agents),
        Observation::Unique {
            status: Some(ChildStatus::Working),
            pane: PaneId("w6:p3".into()),
            native_session: None,
        },
        "F2/F3 — four parts match before any session is reported"
    );
}

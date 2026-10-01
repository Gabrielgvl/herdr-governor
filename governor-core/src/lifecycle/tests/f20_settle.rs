//! F20 — the settle transaction's provider-limit share and a verdict that
//! outlives a version bump.

use crate::delivery::MailboxEventKind;
use crate::identity::{ChildStatus, Digest, Timestamp};
use crate::lifecycle::{Event, JudgmentVerdict, Settlement, State, Versioned, settle, transition};
use crate::recovery::provider_limited;

use super::builders::{
    EMPTY_READ, NOW, event_kinds, obs_unique, run_in, settlement_of, stamped, test_policy, triple,
    updated_run,
};

#[test]
fn f20_provider_limited_settlement_is_the_recovery_share() {
    // F21 — exactly one implementation: the settle transaction's
    // provider_limited share IS `recovery::provider_limited`'s output plus
    // the terminal `settled` event. A divergent inline share is the defect
    // this test names.
    let run = run_in(State::Active);
    let policy = test_policy();
    let shared = provider_limited(&run, None, NOW, &policy);
    let via_settle = settle(&run, Settlement::ProviderLimited, NOW, &policy);
    for change in &shared.state_changes {
        assert!(
            via_settle.state_changes.contains(change),
            "the settle transaction carries the share's {change:?}"
        );
    }
    for event in &shared.events {
        assert!(
            via_settle.events.contains(event),
            "the settle transaction carries the share's {event:?}"
        );
    }
    assert_eq!(
        event_kinds(&via_settle).last(),
        Some(&MailboxEventKind::Settled),
        "the terminal event closes the shared share"
    );
}

#[test]
fn f20_verdict_asked_before_a_version_bump_still_settles() {
    // F20 — the ask's stamp binds generations, not `version`: an
    // observation landing between the acceptance ask and Jev's answer
    // bumps the row's version, and the answer still applies. (The c1
    // sequence: handoff → idle obs → accept.)
    let mut run = run_in(State::Active);
    run.evidence_generation = 0;
    let digest = Digest([9; 32]);
    let t_freeze = transition(
        &run,
        &stamped(&run, Event::Handoff { digest }),
        NOW,
        &test_policy(),
        EMPTY_READ,
        "/fp",
    );
    run = updated_run(&t_freeze).clone();
    assert_eq!(run.state, State::Judging);
    let ask_stamp = triple(&run); // the acceptance ask was requested against this
    // a status-changing observation bumps `version` while the ask flies.
    let t_obs = transition(
        &run,
        &stamped(&run, obs_unique(Some(ChildStatus::Idle))),
        Timestamp(600),
        &test_policy(),
        EMPTY_READ,
        "/fp",
    );
    run = updated_run(&t_obs).clone();
    assert!(
        run.version > ask_stamp.version,
        "the observation moved the row's version"
    );
    // Jev's accept still names the ask's generations → it settles.
    let t_verdict = transition(
        &run,
        &Versioned {
            requested_against: ask_stamp,
            value: Event::Judgment(JudgmentVerdict::Accept),
        },
        Timestamp(700),
        &test_policy(),
        EMPTY_READ,
        "/fp",
    );
    assert_eq!(
        settlement_of(updated_run(&t_verdict)),
        Some(Settlement::Accepted),
        "a verdict stamped before the version bump is not stale (F20)"
    );
}

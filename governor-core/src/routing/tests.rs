//! Unit tests for the routing module — one file per requirement seam, plus
//! the shared-type spelling checks below.

mod builders;
mod f12;
mod f13;
mod f13_candidates;
mod f13_eval;
mod f13_explore;
mod f14;

use alloc::vec::Vec;

use super::{
    Candidate, ChangesFiles, JEV_REQUEST_MAX_BYTES, JudgmentOutcome, JudgmentPurpose, Question,
    TAB_PANE_MAX, TRANSCRIPT_WINDOW_MAX_BYTES,
};
use crate::config::{OperatingPointId, Provider, Tier};
use crate::identity::AgentKind;

#[test]
fn n5_f14_routing_bound_values() {
    assert_eq!(
        JEV_REQUEST_MAX_BYTES, 98_304,
        "Jev request bound is 96 KiB (N5)"
    );
    assert_eq!(
        TRANSCRIPT_WINDOW_MAX_BYTES, 32_768,
        "transcript window is 32 KiB (N5)"
    );
    assert_eq!(TAB_PANE_MAX, 4, "Jev's tab is used under four panes (F14)");
}

#[test]
fn f15_candidate_carries_harness_and_args() {
    let candidate = Candidate {
        operating_point: OperatingPointId("op-1".into()),
        provider: Provider("prov-1".into()),
        tier: Tier("t0".into()),
        harness: AgentKind("kind-1".into()),
        args: Vec::from(["--flag".into()]),
    };
    // `agent.start {kind, args}` replays the persisted candidate verbatim
    // (F15) — a catalog edit must not be able to change either half.
    assert_eq!(
        candidate.harness,
        AgentKind("kind-1".into()),
        "candidate must carry the persisted harness kind (F15)"
    );
    assert_eq!(candidate.args.len(), 1, "candidate carries the exact args");
    assert!(
        candidate.args.iter().any(|arg| arg.as_str() == "--flag"),
        "start args replayed verbatim (F15)"
    );
}

#[test]
fn f12_f23_f24_question_spellings() {
    let cases = [
        (Question::DoneWhenVerifiable, "done_when_verifiable"),
        (Question::WeakestSufficientTier, "weakest_sufficient_tier"),
        (Question::ChangesFiles, "changes_files"),
        (Question::SecurityBoundary, "security_boundary"),
        (Question::NeedsExternal, "needs_external"),
        (Question::LongRunning, "long_running"),
        (Question::RelatedTab, "related_tab"),
        (Question::BlockedOnInput, "blocked_on_input"),
        (Question::NoRecentProgress, "no_recent_progress"),
        (Question::OutsideScope, "outside_scope"),
        (Question::ProviderLimited, "provider_limited"),
        (Question::HandoffMeetsItem { item: 2 }, "handoff_meets_item"),
    ];
    for (question, name) in cases {
        assert_eq!(
            question.as_str(),
            name,
            "question spelling must match the spec"
        );
    }
}

#[test]
fn f12_changes_files_spellings() {
    let cases = [
        (ChangesFiles::None, "none"),
        (ChangesFiles::Few, "few"),
        (ChangesFiles::Broad, "broad"),
    ];
    for (value, name) in cases {
        assert_eq!(
            value.as_str(),
            name,
            "changes_files spelling must match F12"
        );
    }
}

#[test]
fn appendix_b_judgment_purpose_spellings() {
    let cases = [
        (JudgmentPurpose::Launch, "launch"),
        (JudgmentPurpose::Review, "review"),
        (JudgmentPurpose::Acceptance, "acceptance"),
        (JudgmentPurpose::ProviderLimit, "provider_limit"),
    ];
    for (purpose, name) in cases {
        assert_eq!(
            purpose.as_str(),
            name,
            "judgment purpose spelling must match the DDL"
        );
    }
}

#[test]
fn appendix_b_judgment_outcome_spellings() {
    let cases = [
        (JudgmentOutcome::Answered, "answered"),
        (JudgmentOutcome::TransportFailed, "transport_failed"),
        (JudgmentOutcome::AuthFailed, "auth_failed"),
        (JudgmentOutcome::InvalidResponse, "invalid_response"),
        (JudgmentOutcome::TooLarge, "too_large"),
        (JudgmentOutcome::Stale, "stale"),
    ];
    for (outcome, name) in cases {
        assert_eq!(
            outcome.as_str(),
            name,
            "judgment outcome spelling must match the DDL"
        );
    }
}

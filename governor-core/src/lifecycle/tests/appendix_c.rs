//! F22 — Appendix C traceability: every row of `TRANSITION_RULES` names the
//! unit test that proves it. The matrix is data — a rule row without a named
//! proof fails `f22_every_appendix_c_row_has_a_named_proof`, and a proof
//! entry naming no rule fails it in the other direction. Rows no existing
//! unit test proved get their named proof here, cited like any other.

use crate::lifecycle::{
    Event, JudgmentVerdict, Settlement, State, StateChange, TRANSITION_RULES, UnresolvedReason,
    settle,
};

use super::{
    builders::{
        NOW, is_quiet, run_in, settlement_of, stale_stamped, stamped, test_policy, transact,
        updated_run,
    },
    f20, f22, f22_active, f22_judging, f22_prompting, f22_repair, f22_reserved, f22_starting, f24,
    f25,
};

/// `(state, event)` in the spec's spellings → the named unit test that drives
/// that row through `transition`/`settle` and asserts its outcome. A row
/// whose outcome carries several clauses lists one entry per proving test.
const ROW_PROOFS: &[(&str, &str, fn())] = &[
    (
        "settled",
        "cancel(closePane)",
        f20::f20_settled_accepts_only_cancel_with_close_pane,
    ),
    (
        "settled",
        "any other event",
        f20::f20_settled_accepts_only_cancel_with_close_pane,
    ),
    (
        "*",
        "obs(invalid)",
        f22::f22_obs_invalid_changes_nothing_in_every_state,
    ),
    (
        "unsettled",
        "deadline(max_age)",
        f22::f22_max_age_settles_any_unsettled_run,
    ),
    (
        "unsettled",
        "cancel",
        f20::f20_cancel_on_unsettled_settles_cancelled,
    ),
    (
        "unsettled",
        "cancel",
        f20::f20_cancel_with_close_pane_plans_one_verified_close,
    ),
    (
        "*",
        "restart",
        f22::f22_restart_converts_dispatching_to_unconfirmed,
    ),
    (
        "*",
        "restart",
        f22::f22_restart_re_derives_prompting_from_the_journal,
    ),
    ("*", "restart", f22::f22_restart_never_changes_deadlines),
    (
        "unsettled",
        "provider_limited",
        f21_provider_limited_event_settles_an_unsettled_run,
    ),
    (
        "unsettled",
        "provider_limited",
        f20::f20_provider_limited_settlement_records_recovery_and_cooldown,
    ),
    (
        "reserved",
        "obs(absent)",
        f22_reserved::f22_reserved_absent_is_launch_not_started,
    ),
    (
        "reserved",
        "topology effect planned",
        f22_reserved::f22_starting_topology_acknowledgement_plans_first_start,
    ),
    (
        "reserved",
        "launch abstains or fails before any effect",
        f22_reserved_launch_abstain_settles_not_started,
    ),
    (
        "starting",
        "start acknowledged",
        f22_starting::f22_starting_start_acknowledged_goes_prompting,
    ),
    (
        "starting",
        "start acknowledged",
        f22_starting::f22_starting_ack_uses_the_matched_candidates_index,
    ),
    (
        "starting",
        "typed pre-interactive failure with another candidate",
        f22_starting::f22_starting_pre_interactive_failure_tries_next_candidate,
    ),
    (
        "starting",
        "failure with no candidate, or unconfirmed",
        f22_starting::f22_starting_failure_with_no_candidates_stays,
    ),
    (
        "starting",
        "failure with no candidate, or unconfirmed",
        f22_starting::f22_starting_unconfirmed_and_failed_stay_starting,
    ),
    (
        "starting",
        "obs(absent)",
        f22_starting::f22_starting_absent_is_launch_failed,
    ),
    (
        "prompting",
        "prompt acknowledged",
        f22_prompting::f22_prompting_acknowledged_goes_active,
    ),
    (
        "prompting",
        "prompt unconfirmed or failed",
        f22_prompting::f22_prompting_unconfirmed_goes_active_unconfirmed,
    ),
    (
        "prompting",
        "obs(absent)",
        f22_prompting::f22_prompting_absent_is_pane_lost,
    ),
    (
        "active",
        "obs(working)",
        f22_active::f22_active_working_ends_the_episode,
    ),
    (
        "active",
        "obs(working)",
        f22_active::f22_active_working_without_episode_only_records_status,
    ),
    (
        "active",
        "obs(working)",
        f22_active::f22_working_after_a_consumed_stall_nudge_ends_the_episode,
    ),
    (
        "active",
        "obs(idle|done) with no handoff",
        f25::f25_idle_episode_opens_nudge_and_deadline,
    ),
    (
        "active",
        "obs(idle|done) with no handoff",
        f25::f25_repeated_idle_does_not_renudge_or_extend,
    ),
    (
        "active",
        "obs(idle|done) with no handoff",
        f25::f25_stall_then_idle_shares_one_episode,
    ),
    (
        "active",
        "obs(blocked)",
        f22_active::f22_active_blocked_asks_the_supervision_questions,
    ),
    (
        "active",
        "obs(blocked)",
        f22_active::f22_blocked_observation_records_status_and_writes_once,
    ),
    (
        "active",
        "deadline(idle)",
        f25::f25_idle_deadline_settles_no_handoff,
    ),
    (
        "active",
        "handoff(valid)",
        f22_active::f22_active_handoff_freezes_and_judges,
    ),
    (
        "active",
        "handoff(valid)",
        f22_active::f22_refreeze_preserves_an_armed_judgment_deadline,
    ),
    (
        "active",
        "obs(absent)",
        f22_active::f22_active_absent_reads_the_handoff_once,
    ),
    (
        "active",
        "obs(absent)",
        f22_active::f22_active_absent_without_handoff_is_pane_lost,
    ),
    (
        "active",
        "obs(absent)",
        f22_active::f22_active_absent_with_frozen_handoff_judges,
    ),
    (
        "active",
        "obs(absent)",
        f22_active::f22_active_absent_with_frozen_handoff_arms_deadline,
    ),
    (
        "judging",
        "judgment(accept)",
        f22_judging::f22_judging_accept_settles_accepted,
    ),
    (
        "judging",
        "judgment(reject)",
        f22_judging::f22_judging_reject_enters_repair_and_arms_deadline_once,
    ),
    (
        "judging",
        "judgment(unavailable)",
        f22_judging::f22_judging_unavailable_stays_until_deadline,
    ),
    (
        "judging",
        "deadline(judgment)",
        f22_judging::f22_judging_deadlines,
    ),
    (
        "judging",
        "deadline(repair) armed and passed",
        f22_judging::f22_judging_deadlines,
    ),
    (
        "judging",
        "deadline(repair) with a qualifying dispatch in flight",
        f24::f24_judging_repair_deadline_waits_on_a_pending_dispatch,
    ),
    (
        "judging",
        "repair follow-up dispatched before repair_deadline",
        f24::f24_judging_repair_followup_in_window_advances_generation,
    ),
    (
        "judging",
        "repair follow-up resolved past the deadline without qualifying",
        f24::f24_judging_late_unqualifying_result_settles_rejected,
    ),
    (
        "judging",
        "obs(absent)",
        f22_judging::f22_judging_absent_stays_judging,
    ),
    (
        "judging",
        "handoff(new digest)",
        f22_judging::f22_judging_new_digest_refreezes_same_digest_is_ignored,
    ),
    (
        "judging",
        "handoff(frozen digest, assessed)",
        f22_judging::f22_judging_new_digest_refreezes_same_digest_is_ignored,
    ),
    (
        "judging",
        "handoff(frozen digest, ask in flight)",
        f24::f24_repeated_handoff_while_its_ask_is_in_flight_is_ignored,
    ),
    (
        "judging",
        "handoff(frozen digest, unassessed)",
        f24::f24_other_unassessed_digest_resumes_judging,
    ),
    // The version guard runs before state dispatch (F20): a stale judgment
    // produces nothing in `judging` because it produces nothing everywhere —
    // `f20_stamped_events_drop_when_versions_moved` lists `Event::Judgment`,
    // and `f20_stale_judgment_in_judging_changes_nothing` drives the state.
    (
        "judging",
        "stale judgment",
        f20::f20_stamped_events_drop_when_versions_moved,
    ),
    (
        "judging",
        "stale judgment",
        f20_stale_judgment_in_judging_changes_nothing,
    ),
    (
        "repair",
        "repair follow-up dispatched before repair_deadline",
        f22_repair::f22_repair_dispatch_before_deadline_advances_generation,
    ),
    (
        "repair",
        "repair follow-up resolved past the deadline without qualifying",
        f22_repair::f24_repair_absent_result_past_deadline_settles_rejected,
    ),
    (
        "repair",
        "handoff(new digest)",
        f22_repair::f22_repair_new_handoff_freezes_keeping_deadline,
    ),
    (
        "repair",
        "handoff(frozen digest, assessed)",
        f24::f24_assessed_digest_stays_suppressed,
    ),
    (
        "repair",
        "handoff(frozen digest, unassessed)",
        f24::f24_rewritten_unassessed_handoff_resumes_judging,
    ),
    (
        "repair",
        "deadline(repair)",
        f22_repair::f22_repair_deadline_settles_rejected,
    ),
    (
        "repair",
        "deadline(repair) with a qualifying dispatch in flight",
        f22_repair::f24_repair_deadline_waits_on_a_pending_dispatch,
    ),
    (
        "repair",
        "obs(absent)",
        f22_repair::f22_repair_absent_stays_repair,
    ),
];

/// The matrix and the rule list carry exactly the same `(state, event)` set
/// in both directions — no row lacks a named proof and no proof dangles.
#[test]
fn f22_every_appendix_c_row_has_a_named_proof() {
    for &(state, event, _) in TRANSITION_RULES {
        assert!(
            ROW_PROOFS
                .iter()
                .any(|&(rule_state, rule_event, _)| rule_state == state && rule_event == event),
            "Appendix C row ({state}, {event}) names no proof"
        );
    }
    for &(state, event, _) in ROW_PROOFS {
        assert!(
            TRANSITION_RULES
                .iter()
                .any(|&(rule_state, rule_event, _)| rule_state == state && rule_event == event),
            "proof for ({state}, {event}) names no Appendix C row"
        );
    }
}

/// The named proof for `("unsettled", "provider_limited")`: the
/// provider_limited event settles any unsettled Run through the F21
/// transaction — the recovery obligation rides the same write.
#[test]
fn f21_provider_limited_event_settles_an_unsettled_run() {
    for state in [
        State::Reserved,
        State::Starting,
        State::Prompting,
        State::Active,
        State::Judging,
        State::Repair,
    ] {
        let run = run_in(state);
        let t = transact(&run, &stamped(&run, Event::ProviderLimited));
        assert_eq!(
            settlement_of(updated_run(&t)),
            Some(Settlement::ProviderLimited),
            "provider_limited settles any unsettled Run (F21)"
        );
        assert!(
            t.state_changes
                .iter()
                .any(|c| matches!(c, StateChange::RecordRecovery(_))),
            "the recovery obligation rides the settle (F21)"
        );
    }
}

/// The named proof for `("reserved", "launch abstains or fails before any
/// effect")`: the Launch reports its outcome through `settle` — the Run
/// closes `unresolved(launch_not_started)` and the terminal event's body
/// carries the reason.
#[test]
fn f22_reserved_launch_abstain_settles_not_started() {
    let run = run_in(State::Reserved);
    let t = settle(
        &run,
        Settlement::Unresolved {
            reason: UnresolvedReason::LaunchNotStarted,
        },
        NOW,
        &test_policy(),
    );
    let record = updated_run(&t);
    assert_eq!(record.state, State::Settled);
    assert_eq!(
        settlement_of(record),
        Some(Settlement::Unresolved {
            reason: UnresolvedReason::LaunchNotStarted
        }),
        "a launch that never ran an effect settles launch_not_started"
    );
    assert_eq!(
        t.events.last().map(|event| event.body.as_str()),
        Some("{\"settlement\":\"unresolved\",\"reason\":\"launch_not_started\"}"),
        "the Launch's reported outcome rides the settle event"
    );
}

/// The named proof for `("judging", "stale judgment")`: a `judging` Run
/// whose `judgment` stamp no longer holds produces nothing — the version
/// guard drops it before the state dispatch, so there is no state change, no
/// settlement, no plan and no write (F20).
#[test]
fn f20_stale_judgment_in_judging_changes_nothing() {
    let run = run_in(State::Judging);
    let t = transact(
        &run,
        &stale_stamped(&run, Event::Judgment(JudgmentVerdict::Accept)),
    );
    assert!(
        is_quiet(&t),
        "a stale judgment produces nothing in judging (F20)"
    );
}

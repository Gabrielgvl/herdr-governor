//! F22 — the launch lane of the total transition function: `prompting`.

use alloc::vec::Vec;

use crate::delivery::MailboxEventKind;
use crate::identity::Observation;
use crate::lifecycle::{
    EffectCertainty, EffectKind, EffectOutcome, EffectState, Event, PromptCertainty, Settlement,
    State,
};

use super::builders::{
    effect_writes, event_kinds, run_in, run_result, settlement_of, stamped, transact,
    updated_records, updated_run,
};
#[test]
pub(super) fn f22_prompting_acknowledged_goes_active() {
    let run = run_in(State::Prompting);
    let t = transact(
        &run,
        &stamped(
            &run,
            run_result(
                &run,
                "prompt:task",
                EffectKind::Prompt,
                EffectOutcome::Acknowledged,
                None,
            ),
        ),
    );
    let record = updated_run(&t);
    assert_eq!(record.state, State::Active);
    assert_eq!(record.prompt_certainty, Some(PromptCertainty::Acknowledged));
}

#[test]
pub(super) fn f22_prompting_unconfirmed_goes_active_unconfirmed() {
    for outcome in [
        EffectOutcome::Unconfirmed,
        EffectOutcome::PreInteractiveFailed,
        EffectOutcome::Failed {
            certainty: EffectCertainty::Unknown,
        },
    ] {
        let run = run_in(State::Prompting);
        let t = transact(
            &run,
            &stamped(
                &run,
                run_result(&run, "prompt:task", EffectKind::Prompt, outcome, None),
            ),
        );
        let record = updated_run(&t);
        assert_eq!(record.state, State::Active);
        assert_eq!(
            record.prompt_certainty,
            Some(PromptCertainty::Unconfirmed),
            "a possibly-consumed prompt records unconfirmed and never resubmits"
        );
        assert_eq!(
            event_kinds(&t),
            Vec::from([MailboxEventKind::PromptUnconfirmed])
        );
    }
}

#[test]
pub(super) fn f22_prompting_absent_is_pane_lost() {
    let run = run_in(State::Prompting);
    let t = transact(
        &run,
        &stamped(
            &run,
            Event::Obs {
                observation: Observation::Absent,
                handoff_reading: None,
            },
        ),
    );
    assert_eq!(settlement_of(updated_run(&t)), Some(Settlement::PaneLost));
}

#[test]
fn f22_prompting_ignores_non_task_prompt_results() {
    let run = run_in(State::Prompting);
    // a prompt effect that is not the task prompt (kind matches, key does
    // not) must not promote the Run — the `||` guard keeps it out.
    let t = transact(
        &run,
        &stamped(
            &run,
            run_result(
                &run,
                "nudge:0",
                EffectKind::Prompt,
                EffectOutcome::Acknowledged,
                None,
            ),
        ),
    );
    assert!(
        updated_records(&t).is_empty(),
        "only the task prompt's resolution promotes"
    );
    assert_eq!(
        effect_writes(&t),
        Vec::from([("run:r-1:nudge:0", EffectState::Acknowledged)]),
        "the journal write still commits"
    );
    // and a non-prompt effect under the task key stays out too.
    let t_kind = transact(
        &run,
        &stamped(
            &run,
            run_result(
                &run,
                "prompt:task",
                EffectKind::Close,
                EffectOutcome::Acknowledged,
                None,
            ),
        ),
    );
    assert!(updated_records(&t_kind).is_empty());
}

//! F8/F28 — the `restart` lane: `dispatching` effects without a receipt
//! become `unconfirmed` and are never dispatched again, and an interrupted
//! task prompt goes `active` as `unconfirmed`; deadlines are unchanged.

use alloc::string::String;
use alloc::vec::Vec;

use crate::delivery::MailboxEventKind;
use crate::lifecycle::{
    Effect, EffectResolution, EffectState, EffectWrite, PromptCertainty, Run, State, StateChange,
    Transition, edited, effect_key, mailbox_event, write_run,
};

pub(super) fn on_restart(run: &Run, journal: &[Effect]) -> Transition {
    // F8 — dispatching without a receipt becomes unconfirmed and is never
    // dispatched again; `planned` effects still may. No deadline changes.
    let mut state_changes = Vec::new();
    for effect in journal {
        if effect.state == EffectState::Dispatching {
            state_changes.push(StateChange::WriteEffect(EffectWrite::Result {
                key: effect.key.clone(),
                resolution: EffectResolution::Unconfirmed,
            }));
        }
    }
    let mut events = Vec::new();
    // F16 — a task prompt that was dispatching becomes unconfirmed: possibly
    // consumed, no resubmission, the Run goes `active` with certainty
    // recorded and the caller is notified.
    if run.state == State::Prompting {
        let prompt_key = effect_key(run, "prompt:task");
        let interrupted = journal
            .iter()
            .any(|e| e.key == prompt_key && e.state == EffectState::Dispatching);
        if interrupted {
            let record = edited(run, |next| {
                next.state = State::Active;
                next.prompt_certainty = Some(PromptCertainty::Unconfirmed);
            });
            state_changes.push(write_run(run, record));
            events.push(mailbox_event(
                run,
                MailboxEventKind::PromptUnconfirmed,
                "prompt_unconfirmed",
                String::from("{\"prompt\":\"unconfirmed\"}"),
            ));
        }
    }
    Transition {
        state_changes,
        events,
        effects: Vec::new(),
    }
}

//! `runner/render` — the `RenderContext` the coordinator fills from the
//! store at hand-off (§4.4's frozen hand-off contract: the runner holds
//! no store handle), plus the pure text renders — the envelope wrap, the
//! file pointer and the hint/nudge bodies. Nothing here performs wire I/O.

use governor_core::delivery::{MailboxEvent, MessageBody};
use governor_core::identity::{
    CallerKey, DeliveryId, Digest, EffectKey, EventId, PaneId, RunId, mint_agent_name,
};
use governor_core::lifecycle::{Effect, EffectKind, EffectReceipt, EffectState, EffectTarget};
use governor_core::routing::PlacementPlan;
use governor_core::task::Envelope;

use crate::adapters::config::{DaemonSettings, LoadedConfig};
use crate::adapters::herdr::SessionSnapshot;
use crate::daemon::paths::Paths;
use crate::daemon::{FileRef, FollowUpBody, RenderContext, TopologyTarget};
use crate::store::Store;

/// The `RenderContext` for one ready effect, built against the store at
/// hand-off — `None` leaves the row `planned` (re-offered on the next
/// apply): the honest answer when the persisted data cannot render an
/// operation (a missing outbox row, an unlanded topology leg, a body file
/// that vanished).
///
/// `JevEvaluate` renders the launch ask through `launch::eval_context`
/// (B2): `State::Task` plus the question catalog over the freshest
/// snapshot's `open_tabs`. Run-bound asks (`review:*`, `accept:*`,
/// `blocked:*`, `limit:*`) stay `planned` until C3/C4 land theirs.
pub(in crate::daemon) fn context_for(
    store: &Store,
    paths: &Paths,
    effect: &Effect,
    snapshot: Option<&SessionSnapshot>,
    loaded: &LoadedConfig,
    daemon: &DaemonSettings,
) -> Option<RenderContext> {
    let suffix = super::seam::suffix_of(&effect.key).to_string();
    match effect.kind {
        EffectKind::Prompt => prompt_context(store, paths, effect, &suffix),
        EffectKind::AgentStart => agent_start_context(store, effect, &suffix),
        EffectKind::TabCreate | EffectKind::PaneSplit => topology_context(store, effect),
        EffectKind::Close => match &effect.target {
            Some(EffectTarget::Child(identity)) => Some(RenderContext::Close {
                target: identity.clone(),
            }),
            _ => None,
        },
        EffectKind::JevEvaluate => {
            crate::daemon::launch::eval_context(store, snapshot, effect, loaded, daemon)
        }
    }
}

/// `prompt` contexts by key suffix: the task prompt (F16), the F25 episode
/// nudge, the F17 outbox follow-up and the F18 owner hint.
fn prompt_context(
    store: &Store,
    paths: &Paths,
    effect: &Effect,
    suffix: &str,
) -> Option<RenderContext> {
    if suffix == "prompt:task" {
        return task_prompt_context(store, paths, effect);
    }
    if suffix.starts_with("nudge:") {
        return nudge_context(store, paths, effect);
    }
    if let Some(seq) = suffix
        .strip_prefix("outbox:")
        .and_then(|text| text.parse::<u64>().ok())
    {
        return follow_up_context(store, effect, seq);
    }
    if let Some(event) = suffix
        .strip_prefix("event:")
        .and_then(|e| e.strip_suffix(":hint"))
    {
        return hint_context(store, effect, event);
    }
    None
}

/// F16 — the task prompt: `Task::render_prompt` wraps the rendered Task in
/// the provenance envelope; the sender is the Launch's caller, its pane the
/// pane the caller last bound from (H#23).
fn task_prompt_context(store: &Store, paths: &Paths, effect: &Effect) -> Option<RenderContext> {
    let run_id = effect.subject_run.as_ref()?;
    let run = store.run(run_id).ok().flatten()?;
    let launch = store.launch(&run.launch).ok().flatten()?;
    let pane = store.caller_pane(&launch.caller).ok().flatten()?;
    let target = match &effect.target {
        Some(EffectTarget::Child(identity)) => identity.clone(),
        _ => return None,
    };
    let envelope_text = launch.task.render_prompt(
        &DeliveryId(effect.key.0.clone()),
        &launch.caller,
        &pane,
        &run.id,
        &handoff_path(paths, &run.id),
    )?;
    Some(RenderContext::TaskPrompt {
        envelope_text,
        target,
    })
}

/// F25 — the episode's one nudge: an enveloped reminder naming the handoff
/// path. The envelope's sender is the Run's owner; the child reads it as an
/// owner-authored prod (`kind: nudge`).
fn nudge_context(store: &Store, paths: &Paths, effect: &Effect) -> Option<RenderContext> {
    let run_id = effect.subject_run.as_ref()?;
    let run = store.run(run_id).ok().flatten()?;
    let pane = store.caller_pane(&run.owner).ok().flatten()?;
    let target = match &effect.target {
        Some(EffectTarget::Child(identity)) => identity.clone(),
        _ => return None,
    };
    let body = format!(
        "still working? when this assignment is done, blocked, cancelled, or failed, \
         write exactly one Markdown file at {} — read the task prompt for the contract.",
        handoff_path(paths, &run.id)
    );
    let text = Envelope {
        delivery_id: DeliveryId(effect.key.0.clone()),
        sender: run.owner.clone(),
        pane,
        payload: body,
    }
    .render("nudge")?;
    Some(RenderContext::Nudge { text, target })
}

/// F17 — an outbox follow-up: the row's `seq` selects its body; a `File`
/// body carries `body_digest` plus the size the coordinator observed at
/// hand-off — the commit arm re-verifies both before `Go`.
fn follow_up_context(store: &Store, effect: &Effect, seq: u64) -> Option<RenderContext> {
    let run_id = effect.subject_run.as_ref()?;
    let message = store
        .outbox(run_id)
        .ok()?
        .into_iter()
        .find(|message| message.seq == seq)?;
    let target = match &effect.target {
        Some(EffectTarget::Child(identity)) => identity.clone(),
        _ => return None,
    };
    let sender_pane = store.caller_pane(&message.sender).ok().flatten()?;
    let body = match &message.body {
        MessageBody::Inline(text) => FollowUpBody::Inline(text.clone()),
        MessageBody::File { path } => {
            let size = std::fs::metadata(path).ok()?.len();
            FollowUpBody::File(FileRef {
                path: path.clone(),
                size,
                digest: message.body_digest,
            })
        }
    };
    Some(RenderContext::FollowUp {
        body,
        target,
        sender: message.sender.clone(),
        sender_pane,
    })
}

/// F18 — the owner hint: a one-line prod naming the event kind. `owner`
/// is the subject's owner — the pane the prompt lands on re-resolves
/// fresh at the wire (the `CallerContext` pane `hint_eligible` returned
/// may have moved; the session is the stable lookup).
fn hint_context(store: &Store, effect: &Effect, event_id: &str) -> Option<RenderContext> {
    let owner = subject_owner(store, effect)?;
    let event = store
        .mailbox_event(&EventId(event_id.to_string()))
        .ok()
        .flatten()?;
    Some(RenderContext::Hint {
        text: hint_text(&event),
        owner,
    })
}

/// The owner a subject binds: the Run's current owner (adoption moves it),
/// the Launch's caller for launch-only subjects.
fn subject_owner(store: &Store, effect: &Effect) -> Option<CallerKey> {
    if let Some(run_id) = &effect.subject_run {
        return store.run(run_id).ok().flatten().map(|run| run.owner);
    }
    store
        .launch(effect.subject_launch.as_ref()?)
        .ok()
        .flatten()
        .map(|launch| launch.caller)
}

/// F15 — `start:<i>` resolves the persisted candidate and the pane the
/// acknowledged topology effect produced: `AgentPane` names the placement,
/// the journal's acked `tab`/`split` receipt names the pane. No acked
/// topology yet → `None` (the start waits `planned`).
fn agent_start_context(store: &Store, effect: &Effect, suffix: &str) -> Option<RenderContext> {
    let index = suffix.strip_prefix("start:")?.parse::<usize>().ok()?;
    let run_id = effect.subject_run.as_ref()?;
    let run = store.run(run_id).ok().flatten()?;
    let launch = store.launch(&run.launch).ok().flatten()?;
    let candidate = launch.decision.as_ref()?.candidates.get(index)?;
    let Some(EffectTarget::AgentPane(plan)) = &effect.target else {
        return None;
    };
    let topology_key = match plan {
        PlacementPlan::NewTab => EffectKey(format!("run:{}:tab", run.id.0)),
        PlacementPlan::ExistingTab { .. } => EffectKey(format!("run:{}:split", run.id.0)),
    };
    let pane = store
        .journal(run_id)
        .ok()?
        .into_iter()
        .find(|e| e.key == topology_key && e.state == EffectState::Acknowledged)
        .and_then(|e| match e.receipt {
            Some(EffectReceipt::TabCreated { pane, .. } | EffectReceipt::PaneCreated { pane }) => {
                Some(pane)
            }
            _ => None,
        })?;
    Some(RenderContext::AgentStart {
        name: mint_agent_name(&run.id),
        kind: candidate.harness.clone(),
        args: candidate.args.clone(),
        pane,
    })
}

/// F14 — topology kinds: the persisted `target` is the whole descriptor;
/// the runner resolves pane→workspace or tab→pane fresh at the wire.
/// `cwd` is the Run's directory — the child's pane starts there; `label`
/// is the Task's display label (`tab_create` only — a split labels
/// nothing).
fn topology_context(store: &Store, effect: &Effect) -> Option<RenderContext> {
    let run = store.run(effect.subject_run.as_ref()?).ok().flatten()?;
    let launch = store.launch(&run.launch).ok().flatten()?;
    let target = match &effect.target {
        Some(EffectTarget::CallerContext(_)) => TopologyTarget::CallerPane {
            owner: run.owner.clone(),
        },
        Some(EffectTarget::ExistingTab(tab)) => TopologyTarget::ExistingTab(tab.clone()),
        _ => return None,
    };
    Some(RenderContext::Topology {
        target,
        cwd: run.cwd,
        label: launch.task.label.clone(),
    })
}

/// `<state>/handoffs/<runId>/handoff.md` — the path every `prompt:task`
/// envelope instructs the child to write (F16/F24).
fn handoff_path(paths: &Paths, run: &RunId) -> String {
    paths
        .handoffs()
        .join(&run.0)
        .join("handoff.md")
        .to_string_lossy()
        .into_owned()
}

/// The hint body — the event kind and where to read it; the body stays in
/// the mailbox, the hint only wakes the reader.
fn hint_text(event: &MailboxEvent) -> String {
    format!(
        "herdr-governor: {} event for your attention — read your governor mailbox via herdr_status",
        event.kind.as_str()
    )
}

/// The `follow-up` envelope wrap (F16/F17): a File body never crosses the
/// wire — the child reads the verified file via this pointer line; an
/// Inline body's text goes verbatim.
pub(super) fn follow_up_wire(
    delivery: &EffectKey,
    sender: &CallerKey,
    pane: &PaneId,
    body: &str,
) -> Option<String> {
    Envelope {
        delivery_id: DeliveryId(delivery.0.clone()),
        sender: sender.clone(),
        pane: pane.clone(),
        payload: body.to_string(),
    }
    .render("follow-up")
}

/// The pointer body a `File` follow-up renders — path, size and digest, so
/// the child can find and verify the published bytes itself.
pub(super) fn file_pointer(path: &str, size: u64, digest: &Digest) -> String {
    format!(
        "attached file: {path} ({size} bytes, sha256 {})",
        hex(digest)
    )
}

/// Lowercase hex for a `Digest` — the pointer's `sha256` field.
fn hex(digest: &Digest) -> String {
    use std::fmt::Write as _;
    digest.0.iter().fold(String::new(), |mut out, byte| {
        let _unused = write!(out, "{byte:02x}");
        out
    })
}

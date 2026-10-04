//! `runner/herdr` — the runner's Herdr half: the fresh verifications the
//! §4.4 pipeline performs before `pre_dispatch` (F10's snapshot+classify
//! for child-bound kinds, the caller/tab re-resolve for caller-context
//! and split legs), the wire dispatch itself, the receipts and the
//! §4.4 `HerdrError` → `EffectResolution` map.

use governor_core::identity::{
    AgentKind, AgentName, CallerKey, ChildIdentity, ChildStatus, NativeSession, Observation,
    PaneId, TabId, TerminalId, classify,
};
use governor_core::lifecycle::{
    Effect, EffectCertainty, EffectReceipt, EffectResolution, FailureCause,
};

use crate::adapters::herdr::types::SplitDirection;
use crate::adapters::herdr::{
    AgentPromptParams, AgentStartParams, HerdrError, PaneSplitParams, TabCreateParams,
};
use crate::daemon::identity::{agent_rows, incarnation};

use super::{RunnerEnv, Verify, render};
use crate::daemon::{FollowUpBody, RenderContext, TopologyTarget};

/// §4.4 step 2 — the fresh verification each kind performs before the
/// `pre_dispatch` checkpoint. `None` leaves the row `planned`: nothing
/// honest can be wired (identity `absent`/`invalid`, a `blocked` child,
/// a caller or target tab gone). `Detached` kinds need no fresh read —
/// `agent_start` replays a journaled pane (its own error map covers a
/// vanished one), and Jev asks carry no Herdr target.
pub(super) async fn verify(env: &RunnerEnv, context: &RenderContext) -> Option<Verify> {
    match context {
        RenderContext::TaskPrompt { target, .. }
        | RenderContext::Nudge { target, .. }
        | RenderContext::FollowUp { target, .. } => {
            child(env, target, /* prompts */ true).await
        }
        RenderContext::Close { target } => child(env, target, false).await,
        RenderContext::Hint { owner, .. } => caller(env, owner).await,
        RenderContext::Topology { target, .. } => match target {
            TopologyTarget::CallerPane { owner } => caller(env, owner).await,
            TopologyTarget::ExistingTab(tab) => in_tab(env, tab).await,
        },
        RenderContext::AgentStart { .. } | RenderContext::Jev { .. } => Some(Verify::Detached),
    }
}

/// F10 — one fresh `session.snapshot`, the child classified against its
/// captured identity: `unique` carries the *current* pane; `absent`/
/// `invalid` cannot dispatch. `prompt` adds the never-while-`blocked`
/// rule (H#17); `close` asks only for the locator.
async fn child(env: &RunnerEnv, identity: &ChildIdentity, prompt: bool) -> Option<Verify> {
    let observed = env.herdr.session_snapshot(env.herdr_op).await.ok()?;
    let agents = agent_rows(&observed.value);
    let incarnation = incarnation(&observed.epoch);
    match classify(identity, Some(&incarnation), &agents) {
        Observation::Unique { status, pane, .. } => {
            if prompt && status == Some(ChildStatus::Blocked) {
                return None;
            }
            Some(Verify::Child { pane })
        }
        Observation::Absent | Observation::Invalid => None,
    }
}

/// The caller's *current* pane and its workspace: the occupant row whose
/// `(agent_kind, native_session)` is `owner`'s — a moved pane renumbers
/// its locator, so the session is the stable lookup, never the pane id.
async fn caller(env: &RunnerEnv, owner: &CallerKey) -> Option<Verify> {
    let observed = env.herdr.session_snapshot(env.herdr_op).await.ok()?;
    let row = agent_rows(&observed.value).into_iter().find(|row| {
        row.2.as_ref() == Some(&owner.agent_kind) && row.4.as_ref() == Some(&owner.native_session)
    })?;
    let workspace = observed
        .value
        .panes
        .iter()
        .find(|pane| pane.pane_id == row.0.0)?
        .workspace_id
        .clone();
    Some(Verify::Caller {
        pane: row.0,
        workspace,
    })
}

/// A pane currently inside `tab` — the `pane_split` target.
async fn in_tab(env: &RunnerEnv, tab: &TabId) -> Option<Verify> {
    let observed = env.herdr.session_snapshot(env.herdr_op).await.ok()?;
    let pane = observed
        .value
        .panes
        .iter()
        .find(|pane| pane.tab_id == tab.0)?;
    Some(Verify::InTab {
        pane: PaneId(pane.pane_id.clone()),
    })
}

/// §4.4 steps 5–6 — the wire write for one committed effect, from the
/// frozen context and the fresh verify only. The `HerdrError` map is
/// §4.4's resolution table verbatim.
pub(super) async fn wire(
    env: &RunnerEnv,
    effect: &Effect,
    context: &RenderContext,
    verify: &Verify,
) -> EffectResolution {
    match context {
        RenderContext::TaskPrompt {
            envelope_text,
            target,
        }
        | RenderContext::Nudge {
            text: envelope_text,
            target,
        } => {
            let prompt = verify_child_pane(verify);
            match prompt {
                Some(pane) => prompt_child(env, pane, envelope_text, target).await,
                None => lane_mismatch(),
            }
        }
        RenderContext::FollowUp {
            body,
            target,
            sender,
            sender_pane,
            ..
        } => {
            let Some(pane) = verify_child_pane(verify) else {
                return lane_mismatch();
            };
            let body_text = match body {
                FollowUpBody::Inline(text) => text.clone(),
                FollowUpBody::File(file) => {
                    render::file_pointer(&file.path, file.size, &file.digest)
                }
            };
            let Some(text) = render::follow_up_wire(&effect.key, sender, sender_pane, &body_text)
            else {
                return failed_absent("unrenderable_follow_up");
            };
            prompt_child(env, pane, &text, target).await
        }
        RenderContext::Hint { text, owner } => {
            let Verify::Caller { pane, .. } = verify else {
                return lane_mismatch();
            };
            prompt_caller(env, pane, text, owner).await
        }
        RenderContext::AgentStart {
            name,
            kind,
            args,
            pane,
        } => agent_start(env, name, kind, args, pane).await,
        RenderContext::Topology { target, cwd, label } => match (target, verify) {
            (TopologyTarget::CallerPane { .. }, Verify::Caller { workspace, .. }) => {
                tab_create(env, workspace, cwd, label.as_deref()).await
            }
            (TopologyTarget::ExistingTab(_), Verify::InTab { pane }) => {
                pane_split(env, pane, cwd).await
            }
            _ => lane_mismatch(),
        },
        RenderContext::Close { .. } => match verify {
            Verify::Child { pane } => close(env, pane).await,
            Verify::Caller { .. } | Verify::InTab { .. } | Verify::Detached => lane_mismatch(),
        },
        RenderContext::Jev { .. } => failed_absent("jev_dispatch_on_herdr_lane"),
    }
}

/// The prompt leg to a child pane: `agent.prompt` plus the F16 ack-match
/// — the acknowledgement must name the captured identity it was sent to,
/// else the write went somewhere else and `unknown` is the only honest
/// certainty.
async fn prompt_child(
    env: &RunnerEnv,
    pane: &PaneId,
    text: &str,
    target: &ChildIdentity,
) -> EffectResolution {
    let params = AgentPromptParams {
        target: pane.0.clone(),
        text: text.to_string(),
    };
    match env.herdr.agent_prompt(&params, env.herdr_op).await {
        Ok(observed) => {
            let agent = &observed.value.agent;
            let session = agent.agent_session.as_ref().map(|s| s.value.as_str());
            // F16 — the same identity fields `classify`'s `unique` pins:
            // a re-minted native session (F1's `a4_native_new_replaces_session`
            // shape) is a different agent even when name/kind/terminal carry
            // over. A session-less captured identity cannot constrain the
            // ack's session — the harness never reported one.
            let matches = agent.terminal_id == target.terminal_id.0
                && agent.agent.as_deref() == Some(target.agent_kind.0.as_str())
                && agent.name.as_deref() == Some(target.agent_name.0.as_str())
                && match &target.native_session {
                    Some(expected) => session == Some(expected.0.as_str()),
                    None => true,
                };
            if matches {
                EffectResolution::Acknowledged { receipt: None }
            } else {
                failed_unknown("ack_identity_mismatch")
            }
        }
        Err(error) => resolve(&error),
    }
}

/// The prompt leg to a caller-context pane (hints): the ack's agent
/// session must be the owner's — the F16 rule against a re-targeted
/// write applies to the owner too.
async fn prompt_caller(
    env: &RunnerEnv,
    pane: &PaneId,
    text: &str,
    owner: &CallerKey,
) -> EffectResolution {
    let params = AgentPromptParams {
        target: pane.0.clone(),
        text: text.to_string(),
    };
    match env.herdr.agent_prompt(&params, env.herdr_op).await {
        Ok(observed) => {
            let session = observed
                .value
                .agent
                .agent_session
                .as_ref()
                .map(|s| s.value.as_str());
            if session == Some(owner.native_session.0.as_str()) {
                EffectResolution::Acknowledged { receipt: None }
            } else {
                failed_unknown("ack_identity_mismatch")
            }
        }
        Err(error) => resolve(&error),
    }
}

/// F15 — `agent.start` on the topology-produced pane: the reply's agent
/// record plus the connection's minted incarnation are the captured F2
/// identity the `AgentStarted` receipt journals.
async fn agent_start(
    env: &RunnerEnv,
    name: &AgentName,
    kind: &AgentKind,
    args: &[String],
    pane: &PaneId,
) -> EffectResolution {
    // The schema bounds `timeout_ms` to (3000, 300000]; the op deadline
    // must cover connect+write+the server-side wait, so it adds the
    // normal op margin on top.
    let server_ms = u64::try_from(env.agent_start.as_millis())
        .unwrap_or(u64::MAX)
        .clamp(3_001, 300_000);
    let deadline = env.agent_start.saturating_add(env.herdr_op);
    let params = AgentStartParams {
        name: name.0.clone(),
        kind: kind.0.clone(),
        pane_id: pane.0.clone(),
        args: args.to_vec(),
        timeout_ms: Some(server_ms),
    };
    match env.herdr.agent_start(&params, deadline).await {
        Ok(observed) => {
            let agent = observed.value.agent;
            let identity = ChildIdentity {
                herdr_incarnation: incarnation(&observed.epoch),
                terminal_id: TerminalId(agent.terminal_id),
                agent_kind: kind.clone(),
                agent_name: name.clone(),
                native_session: agent
                    .agent_session
                    .map(|session| NativeSession(session.value)),
                pane_id: PaneId(agent.pane_id),
            };
            EffectResolution::Acknowledged {
                receipt: Some(EffectReceipt::AgentStarted { identity }),
            }
        }
        Err(error) => resolve(&error),
    }
}

/// F14 — `tab.create` into the caller's current workspace; the reply's
/// `root_pane` is the `NewTab` placement's start target (H#102).
async fn tab_create(
    env: &RunnerEnv,
    workspace: &str,
    cwd: &str,
    label: Option<&str>,
) -> EffectResolution {
    let params = TabCreateParams {
        workspace_id: Some(workspace.to_string()),
        label: label.map(str::to_string),
        cwd: Some(cwd.to_string()),
        env: None,
        focus: false,
    };
    match env.herdr.tab_create(&params, env.herdr_op).await {
        Ok(observed) => EffectResolution::Acknowledged {
            receipt: Some(EffectReceipt::TabCreated {
                tab: TabId(observed.value.tab.tab_id),
                pane: PaneId(observed.value.root_pane.pane_id),
            }),
        },
        Err(error) => resolve(&error),
    }
}

/// F14 — `pane.split` right inside the resolved tab pane (H#53).
async fn pane_split(env: &RunnerEnv, pane: &PaneId, cwd: &str) -> EffectResolution {
    let params = PaneSplitParams {
        direction: SplitDirection::Right,
        target_pane_id: Some(pane.0.clone()),
        workspace_id: None,
        cwd: Some(cwd.to_string()),
        env: None,
        focus: false,
        ratio: None,
    };
    match env.herdr.pane_split(&params, env.herdr_op).await {
        Ok(observed) => EffectResolution::Acknowledged {
            receipt: Some(EffectReceipt::PaneCreated {
                pane: PaneId(observed.value.pane_id),
            }),
        },
        Err(error) => resolve(&error),
    }
}

/// F20 — `pane.close` on the re-verified locator.
async fn close(env: &RunnerEnv, pane: &PaneId) -> EffectResolution {
    match env.herdr.pane_close(&pane.0, env.herdr_op).await {
        Ok(_) => EffectResolution::Acknowledged { receipt: None },
        Err(error) => resolve(&error),
    }
}

/// §4.4 — the exhaustive `HerdrError` → `EffectResolution` map: the
/// typed pre-flight refusal, the provably-never-ran class, and the
/// may-have-run class.
pub(super) fn resolve(error: &HerdrError) -> EffectResolution {
    let cause = || Some(FailureCause(error.to_string()));
    match error {
        HerdrError::AgentPaneBusy { .. } => {
            EffectResolution::PreInteractiveFailed { cause: cause() }
        }
        HerdrError::AgentNotFound { .. }
        | HerdrError::PaneNotFound { .. }
        | HerdrError::Uncorrelated { .. }
        | HerdrError::Connect(_) => EffectResolution::Failed {
            certainty: EffectCertainty::Absent,
            cause: cause(),
        },
        HerdrError::Timeout { .. }
        | HerdrError::DeadlineExceeded
        | HerdrError::Io(_)
        | HerdrError::StreamClosed
        | HerdrError::Malformed { .. }
        | HerdrError::FrameTooLarge
        | HerdrError::SubscriptionFailed { .. }
        | HerdrError::Server { .. } => EffectResolution::Failed {
            certainty: EffectCertainty::Unknown,
            cause: cause(),
        },
    }
}

/// The verify product the prompt legs need — a child `unique` pane.
fn verify_child_pane(verify: &Verify) -> Option<&PaneId> {
    match verify {
        Verify::Child { pane } => Some(pane),
        Verify::Caller { .. } | Verify::InTab { .. } | Verify::Detached => None,
    }
}

/// `Failed{Absent}` with a static cause — render-side impossibilities
/// (a context the wire lane cannot use) never ran by construction.
fn failed_absent(cause: &'static str) -> EffectResolution {
    EffectResolution::Failed {
        certainty: EffectCertainty::Absent,
        cause: Some(FailureCause(cause.into())),
    }
}

/// `Failed{Unknown}` with a static cause — the write may have landed.
fn failed_unknown(cause: &'static str) -> EffectResolution {
    EffectResolution::Failed {
        certainty: EffectCertainty::Unknown,
        cause: Some(FailureCause(cause.into())),
    }
}

/// A verify/context mismatch the drive pipeline's ordering makes
/// unreachable: reported `Failed{Absent}` rather than panicking — the
/// mutation provably never ran because it was never written.
fn lane_mismatch() -> EffectResolution {
    failed_absent("verify_lane_mismatch")
}

#[cfg(test)]
mod tests;

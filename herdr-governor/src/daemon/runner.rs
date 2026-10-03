//! `runner` — §4.4's dispatch pipeline: one task per handed-off effect,
//! holding nothing but the coordinator's frozen `Dispatch` — a store
//! handle never crosses this boundary, so every revalidation the commit
//! arm makes reads the authoritative row, never a runner-side copy.
//!
//! The pipeline, in order: the kind's fresh verification (F10's
//! snapshot+classify for child-bound effects, the caller re-resolve for
//! caller-context ones) → `pre_dispatch` → `Msg::DispatchCommit` (the
//! coordinator's authoritative revalidation) → `dispatch_committed` →
//! the §4.14 shutdown gate → the wire op → `wire_returned` →
//! `Msg::EffectResult`. `result_committed` fires on the coordinator side
//! after the apply lands.

mod herdr;
/// `jev` — the wire leg for `RenderContext::Jev` asks. Nothing renders
/// that variant yet (the question catalog is PR C's OQ-J, and B2's
/// admission path supplies the context explicitly), so the shipped
/// binary keeps no Jev client: linking the HTTP stack would grow the
/// relay child's resident pages past N4's bound for a lane that cannot
/// fire. The leg stays exercised by its unit tests; when contexts become
/// producible this gate flips and `RunnerEnv` gains the client.
#[cfg(test)]
mod jev;
mod render;
/// `seam` — the checkpoint matcher the coordinator's `result_committed`
/// checkpoint shares with the runner's three.
pub(super) mod seam;

use std::sync::Arc;
use std::time::Duration;

use governor_core::identity::{
    AgentKind, AgentName, CallerKey, ChildIdentity, Digest, PaneId, TabId,
};
use governor_core::lifecycle::{
    Effect, EffectCertainty, EffectResolution, EffectResult, FailureCause,
};
use governor_core::routing::JudgmentSet;
use tokio::sync::{mpsc, oneshot, watch};

use crate::adapters::herdr::Client as HerdrClient;
use crate::adapters::jev::{QuestionSpec, wire};

use super::coordinator::Msg;
use super::seam::{Boundary, SeamConfig};

pub(super) use render::context_for;

/// A published governor file the wire side sends by pointer: the path,
/// plus the `size`/`digest` the coordinator recorded at hand-off — the
/// commit arm re-verifies both before `Go` (a file that moved under the
/// effect is refused, never sent).
#[derive(Debug, Clone, PartialEq, Eq)]
#[expect(
    clippy::redundant_pub_crate,
    reason = "pub(crate) is the honest ceiling — `pub` would satisfy this lint but trip `unreachable_pub` through the private module"
)]
pub(crate) struct FileRef {
    /// The file's path.
    pub path: String,
    /// The size recorded at hand-off.
    pub size: u64,
    /// The expected sha256.
    pub digest: Digest,
}

/// The body of an outbox follow-up (F17): inline text renders directly;
/// a file body crosses as a pointer — the bytes never go on the wire.
#[derive(Debug, Clone, PartialEq, Eq)]
#[expect(
    clippy::redundant_pub_crate,
    reason = "pub(crate) is the honest ceiling — `pub` would satisfy this lint but trip `unreachable_pub` through the private module"
)]
pub(crate) enum FollowUpBody {
    /// `body_inline` verbatim.
    Inline(String),
    /// `body_path` — the child reads the file itself via the pointer.
    File(FileRef),
}

/// The fresh-resolution target of a topology effect (F14).
#[derive(Debug, Clone, PartialEq, Eq)]
#[expect(
    clippy::redundant_pub_crate,
    reason = "pub(crate) is the honest ceiling — `pub` would satisfy this lint but trip `unreachable_pub` through the private module"
)]
pub(crate) enum TopologyTarget {
    /// `tab_create` — the caller's pane locates the workspace the new tab
    /// opens in; resolved fresh at dispatch (the captured `CallerContext`
    /// pane may have moved).
    CallerPane {
        /// The owner whose current pane answers the workspace question.
        owner: CallerKey,
    },
    /// `pane_split` — right-split inside `tab` (H#53, no focus); a pane
    /// inside it is resolved fresh at the wire.
    ExistingTab(TabId),
}

/// §4.4 — everything the wire write needs, frozen by the coordinator at
/// hand-off: rendered text, captured identities, resolved pans for the
/// pipeline legs. The runner never rebuilds these from a store read —
/// there is no store on this side of the seam.
#[derive(Debug, Clone)]
#[expect(
    clippy::redundant_pub_crate,
    reason = "pub(crate) is the honest ceiling — `pub` would satisfy this lint but trip `unreachable_pub` through the private module"
)]
pub(crate) enum RenderContext {
    /// A Jev ask: the semantic state, the asked questions, and the set
    /// frame the receipt's `JudgmentRecord` stamps (F12/F20/F24). `frozen`
    /// carries the acceptance ask's handoff file — its digest is
    /// re-verified at commit (`frozen_digest_mismatch` otherwise).
    #[expect(
        dead_code,
        reason = "B2's launch-admission path constructs the ask context; the wire lane is complete on this side"
    )]
    Jev {
        /// The catalog's `jev_model` at hand-off.
        model: String,
        /// The semantic state judged.
        state: wire::State,
        /// The asked questions, in order.
        questions: Vec<QuestionSpec>,
        /// The set frame: id/purpose/subject/versions/digests/versions —
        /// the wire fills `model` and `outcome`. Boxed — the record dwarfs
        /// the other variants' payloads.
        set: Box<JudgmentSet>,
        /// The acceptance ask's frozen handoff, when the ask binds one.
        frozen: Option<FileRef>,
    },
    /// `prompt:task` — the rendered task envelope (F16).
    TaskPrompt {
        /// `Task::render_prompt` output, frozen.
        envelope_text: String,
        /// The captured child identity — the prompt's target.
        target: ChildIdentity,
    },
    /// `nudge:<episode>` — the episode's one prod (F25).
    Nudge {
        /// The rendered nudge envelope.
        text: String,
        /// The captured child identity.
        target: ChildIdentity,
    },
    /// `outbox:<seq>` — an F17 follow-up.
    FollowUp {
        /// The message body.
        body: FollowUpBody,
        /// The captured child identity.
        target: ChildIdentity,
        /// The sender — the envelope's `from`.
        sender: CallerKey,
        /// The sender's pane at hand-off — the envelope's `pane` header.
        sender_pane: PaneId,
    },
    /// `event:<id>:hint` — the F18 owner prod; `owner` re-resolves the
    /// destination pane fresh at dispatch (the bound pane may have moved).
    Hint {
        /// The rendered hint text.
        text: String,
        /// The owner the pane re-resolution looks up.
        owner: CallerKey,
    },
    /// `start:<i>` — an F15 `agent.start` leg.
    AgentStart {
        /// The minted agent name.
        name: AgentName,
        /// The candidate's harness kind.
        kind: AgentKind,
        /// The candidate's exact argv — replayed verbatim.
        args: Vec<String>,
        /// The pane the acknowledged topology leg produced.
        pane: PaneId,
    },
    /// `tab`/`split` — the launch pipeline's topology legs (F14).
    Topology {
        /// Where the leg lands.
        target: TopologyTarget,
        /// The child's start directory.
        cwd: String,
        /// The tab label (`tab_create` only) — the Task's `label`.
        label: Option<String>,
    },
    /// `close` — an explicit `cancel {closePane}` close (F20).
    Close {
        /// The captured child identity.
        target: ChildIdentity,
    },
}

impl RenderContext {
    /// The frozen files this context commits the wire to naming — the
    /// commit arm's `frozen_digest_mismatch` audit re-verifies each
    /// before `Go`.
    pub(in crate::daemon) fn files(&self) -> Vec<&FileRef> {
        match self {
            Self::Jev { frozen, .. } => frozen.iter().collect(),
            Self::FollowUp {
                body: FollowUpBody::File(file),
                ..
            } => Vec::from([file]),
            Self::TaskPrompt { .. }
            | Self::Nudge { .. }
            | Self::FollowUp {
                body: FollowUpBody::Inline(_),
                ..
            }
            | Self::Hint { .. }
            | Self::AgentStart { .. }
            | Self::Topology { .. }
            | Self::Close { .. } => Vec::new(),
        }
    }
}

/// The coordinator → runner hand-off: the journaled row plus its frozen
/// render context. Shared by `Arc` — `DispatchCommit` carries a reference
/// back so the commit arm audits exactly what the runner holds.
#[derive(Debug, Clone)]
pub(in crate::daemon) struct Dispatch {
    /// The journaled effect row as the coordinator read it at hand-off.
    pub effect: Effect,
    /// The frozen render context.
    pub context: Arc<RenderContext>,
}

/// What the runner's fresh verification resolved — the wire step's
/// current locator, re-derived from a fresh `session.snapshot` (never the
/// journaled one — F10).
#[derive(Debug, Clone)]
enum Verify {
    /// The child identity re-verified `unique` — `pane` is its *current*
    /// locator (a move is followed, never a loss — H#75).
    Child {
        /// The current pane locator.
        pane: PaneId,
    },
    /// The owner/caller re-resolved — `pane` its current locator,
    /// `workspace` the pane's workspace (the `tab_create` answer).
    Caller {
        /// The caller's current pane.
        pane: PaneId,
        /// That pane's workspace.
        workspace: String,
    },
    /// A pane currently inside the target tab — the `pane_split` target.
    InTab {
        /// The pane the split targets.
        pane: PaneId,
    },
    /// No fresh identity needed — `agent_start`'s journaled pane (F10's
    /// re-verify is a child-identity contract) and Jev asks.
    Detached,
}

/// The `DispatchCommit` arm's answer (§4.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[expect(
    clippy::redundant_pub_crate,
    reason = "pub(crate) is the honest ceiling — `pub` would satisfy this lint but trip `unreachable_pub` through the private module"
)]
pub(crate) enum CommitVerdict {
    /// `[WriteEffect::Dispatch]` committed — the wire write may proceed.
    Go,
    /// The effect no longer qualifies — left `planned`; the runner exits
    /// without posting anything (the next hand-off re-offers it).
    Skip,
    /// The coordinator committed the refused resolution itself — the
    /// composed `[Dispatch]+transition(EffectResult)` single apply; the
    /// runner exits (a second result is out of contract by construction).
    Refused,
}

/// Everything a runner task needs beyond its `Dispatch` — adapter
/// handles, configured deadlines, the coordinator mailbox, the shutdown
/// `watch` receiver (§4.14's pre-wire gate) and the armed seam.
#[derive(Clone)]
pub(in crate::daemon) struct RunnerEnv {
    /// The Herdr session client.
    pub herdr: HerdrClient,
    /// One Herdr op's deadline.
    pub herdr_op: Duration,
    /// `agent.start`'s deadline (the server-side `timeout_ms` plus the
    /// op margin).
    pub agent_start: Duration,
    /// The coordinator mailbox — `DispatchCommit`/`EffectResult` post here.
    pub tx: mpsc::Sender<Msg>,
    /// §4.14 step-1's flag — the pre-wire gate reads it.
    pub shutdown: watch::Receiver<bool>,
    /// The armed fault seam, if any.
    pub seam: Option<SeamConfig>,
}

/// One effect through the §4.4 pipeline. Every early return leaves the
/// journal honest: pre-commit exits leave the row `planned` (re-offered
/// by the next hand-off); post-commit exits have posted their result or
/// had it committed by the coordinator.
pub(super) async fn drive(env: RunnerEnv, dispatch: Dispatch) {
    let Dispatch { effect, context } = dispatch;
    // Step 2 — the kind's fresh verification. `None` means nothing honest
    // can go on the wire (absent/invalid identity, a blocked child, a
    // vanished caller or tab); the row stays `planned`, and the subject
    // claim the coordinator took at hand-off is released so the next
    // pass re-offers it.
    let Some(verify) = herdr::verify(&env, &context).await else {
        let _dropped = env
            .tx
            .send(Msg::ReleaseSubject {
                key: effect.key.clone(),
            })
            .await;
        return;
    };
    // The common pre-dispatch checkpoint (F18/F24's seam boundary).
    seam::checkpoint(&effect.key, Boundary::PreDispatch, env.seam.as_ref()).await;

    // Step 3 — the authoritative commit: the coordinator re-reads the row
    // and revalidates every gate against durable state.
    let (reply, answered) = oneshot::channel();
    let commit = Msg::DispatchCommit {
        key: effect.key.clone(),
        context: Arc::clone(&context),
        reply,
    };
    if env.tx.send(commit).await.is_err() {
        return;
    }
    let Ok(verdict) = answered.await else {
        return;
    };
    if verdict != CommitVerdict::Go {
        return;
    }

    seam::checkpoint(&effect.key, Boundary::DispatchCommitted, env.seam.as_ref()).await;

    // Step 4 — §4.14's gate: a flag raised between the commit and the
    // write means the mutation never ran; the refused result still
    // commits (the drain serves it inside `shutdown_grace`).
    let resolution = if *env.shutdown.borrow() {
        EffectResolution::Failed {
            certainty: EffectCertainty::Absent,
            cause: Some(FailureCause("shutdown_before_wire".into())),
        }
    } else {
        let resolution = match &*context {
            // `context_for` renders no `Jev` context until PR C lands the
            // question catalog, so this arm cannot fire — but if a
            // dispatch ever carried one, the wire op provably never ran:
            // `Absent` is the honest certainty.
            RenderContext::Jev { .. } => EffectResolution::Failed {
                certainty: EffectCertainty::Absent,
                cause: Some(FailureCause("jev_lane_unwired".into())),
            },
            RenderContext::TaskPrompt { .. }
            | RenderContext::Nudge { .. }
            | RenderContext::FollowUp { .. }
            | RenderContext::Hint { .. }
            | RenderContext::AgentStart { .. }
            | RenderContext::Topology { .. }
            | RenderContext::Close { .. } => herdr::wire(&env, &effect, &context, &verify).await,
        };
        seam::checkpoint(&effect.key, Boundary::WireReturned, env.seam.as_ref()).await;
        resolution
    };

    let result = EffectResult {
        key: effect.key.clone(),
        kind: effect.kind,
        resolution,
    };
    let _dropped = env.tx.send(Msg::EffectResult(Box::new(result))).await;
}

//! The run-state vocabulary (Appendix C/B): the lifecycle `State`s, the
//! prompt `certainty`, the deadline kinds, the `unresolved` reasons and the
//! F20 settlements — each with its stored spelling.

/// Appendix C — the lifecycle states: `reserved` → `starting` → `prompting` →
/// `active` → `judging` ⇄ `repair` → `settled`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum State {
    /// `reserved` — decision persisted, Run row created, no effect yet.
    Reserved,
    /// `starting` — topology planned; `agent.start` in flight or falling
    /// back across candidates.
    Starting,
    /// `prompting` — started; the Task prompt effect is in flight.
    Prompting,
    /// `active` — the Run is supervised (F23/F25).
    Active,
    /// `judging` — a frozen handoff is being assessed (F24).
    Judging,
    /// `repair` — a rejected work generation; `repair_deadline` runs (F24).
    Repair,
    /// `settled` — terminal; immutable (F20).
    Settled,
}

impl State {
    /// Appendix B — the stored spelling of the state (`runs.state` CHECK).
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Reserved => "reserved",
            Self::Starting => "starting",
            Self::Prompting => "prompting",
            Self::Active => "active",
            Self::Judging => "judging",
            Self::Repair => "repair",
            Self::Settled => "settled",
        }
    }
}

/// Appendix B `runs.prompt_certainty` — what the prompt's acknowledgement
/// proved (F16).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PromptCertainty {
    /// `acknowledged` — the ack matched the captured identity (H#30).
    Acknowledged,
    /// `unconfirmed` — possibly consumed: no resubmission, relaunch or
    /// cleanup; supervision continues (H#29, H#31).
    Unconfirmed,
}

impl PromptCertainty {
    /// Appendix B — the stored spelling of the certainty.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Acknowledged => "acknowledged",
            Self::Unconfirmed => "unconfirmed",
        }
    }
}

/// Appendix C `deadline(...)` / F22 — the deadline kinds, stored as absolute
/// times and never reset by observations, restarts or paused reviews.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum DeadlineKind {
    /// `idle` — the F25 idle episode bound (`idle_deadline`).
    Idle,
    /// `repair` — the F24 repair window (`repair_deadline`).
    Repair,
    /// `judgment` — the Jev-unavailable bound (`judgment_deadline`).
    Judgment,
    /// `max_age` — the per-Run lifetime bound (`max_age_deadline`); in any
    /// unsettled state it settles `unresolved(max_age)`.
    MaxAge,
}

impl DeadlineKind {
    /// Appendix C — the spec spelling of the deadline.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Repair => "repair",
            Self::Judgment => "judgment",
            Self::MaxAge => "max_age",
        }
    }
}

/// F20 — the specific reasons a Run can settle `unresolved`
/// (`runs.settlement_reason`; Appendix B requires it for `unresolved`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum UnresolvedReason {
    /// `launch_not_started` — the Launch abstained or failed before any
    /// effect (Appendix C `reserved`).
    LaunchNotStarted,
    /// `launch_failed` — start failed with no candidate, or went
    /// `unconfirmed`, and the pane was observed `absent` (Appendix C
    /// `starting`).
    LaunchFailed,
    /// `judgment_unavailable` — Jev stayed unavailable past
    /// `judgment_deadline` (F24).
    JudgmentUnavailable,
    /// `identity_unprovable` — a Herdr incarnation change left a Run without
    /// `native_session` unable to re-prove its identity (A4/F28).
    IdentityUnprovable,
    /// `max_age` — `max_age_deadline` passed (F22/F25).
    MaxAge,
}

impl UnresolvedReason {
    /// F20 — the stored spelling of the reason (`runs.settlement_reason`).
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::LaunchNotStarted => "launch_not_started",
            Self::LaunchFailed => "launch_failed",
            Self::JudgmentUnavailable => "judgment_unavailable",
            Self::IdentityUnprovable => "identity_unprovable",
            Self::MaxAge => "max_age",
        }
    }
}

/// F20 — the settlements: first-commit-wins and immutable (`settlement IS
/// NULL AND version = :v` guards the write; a trigger aborts any rewrite).
/// The governor never invents an `accepted`/`rejected` verdict (F25).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Settlement {
    /// `accepted` — every doneWhen item met (F24).
    Accepted,
    /// `rejected` — `repair_deadline` passed with no qualifying repair (F24).
    Rejected,
    /// `no_handoff` — `idle_deadline` passed on an idle episode (F25).
    NoHandoff,
    /// `pane_lost` — the child's identity went `absent` with no frozen
    /// handoff (F25).
    PaneLost,
    /// `cancelled` — `cancel` on an unsettled Run (F20).
    Cancelled,
    /// `provider_limited` — the provider-limit judgment cleared the policy
    /// threshold (F21).
    ProviderLimited,
    /// `unresolved(reason)` — the governor could not prove an outcome
    /// (F20/F25).
    Unresolved {
        /// The specific reason — never absent on `unresolved` (Appendix B
        /// CHECK).
        reason: UnresolvedReason,
    },
}

impl Settlement {
    /// Appendix B — the stored spelling of the settlement (`runs.settlement`
    /// CHECK); the reason rides `runs.settlement_reason`.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Rejected => "rejected",
            Self::NoHandoff => "no_handoff",
            Self::PaneLost => "pane_lost",
            Self::Cancelled => "cancelled",
            Self::ProviderLimited => "provider_limited",
            Self::Unresolved { reason: _ } => "unresolved",
        }
    }
}

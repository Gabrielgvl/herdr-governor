//! Appendix C/F20 — the event vocabulary: the `(version, work_generation,
//! evidence_generation)` triple every async result is stamped with, the
//! `Versioned` envelope it arrives in, the judgment verdicts and the
//! `Event` enum the total function matches.

use crate::acceptance::HandoffReading;
use crate::identity::{Digest, Observation};

use super::{DeadlineKind, EffectResult};

/// F20 — the `(version, work_generation, evidence_generation)` triple every
/// Jev result, observation and deadline carries; it applies only while all
/// three still hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct VersionTriple {
    /// `runs.version` — the row version.
    pub version: u64,
    /// `runs.work_generation` — the generation of accepted work (repair
    /// dispatches advance it, F24).
    pub work_generation: u64,
    /// `runs.evidence_generation` — the generation of judged evidence
    /// (freezes advance it, F24).
    pub evidence_generation: u64,
}

/// F20 — an async result stamped with the versions it was requested against;
/// the transition checks the stamp before applying.
#[derive(Debug, Clone, PartialEq)]
pub struct Versioned<T> {
    /// The versions the result was requested against.
    pub requested_against: VersionTriple,
    /// The result payload.
    pub value: T,
}

/// Appendix C `judgment(...)` — how a completed acceptance assessment rules
/// on the frozen handoff.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum JudgmentVerdict {
    /// `accept` — every doneWhen item met → `accepted`.
    Accept,
    /// `reject` — some item unmet → `repair` (F24).
    Reject,
    /// `unavailable` — the request could not complete; the Run waits on
    /// `judgment_deadline` (F24).
    Unavailable,
}

/// Appendix C — the events the total transition function matches: every
/// state against every one of these, no wildcards (F22). Stamped through
/// `Versioned`: the F20 triple they were requested against.
#[expect(
    clippy::large_enum_variant,
    reason = "EffectResult carries its typed receipt unboxed — the lanes match on the payload directly"
)]
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// `obs(unique|absent|invalid)` — the child status rides inside
    /// `Observation::Unique`. `handoff_reading` carries the one-shot
    /// marked-file read Appendix C performs on `obs(absent)` in `active`
    /// (`None` in every other state and class).
    Obs {
        /// The target-local read of a fresh snapshot (F3).
        observation: Observation,
        /// The one-shot handoff read, when the transition asked for it.
        handoff_reading: Option<HandoffReading>,
    },
    /// `handoff` — a valid marked file observed while `active`; the digest
    /// names the bytes to freeze (F24).
    Handoff {
        /// The marked file's digest.
        digest: Digest,
    },
    /// `judgment(accept|reject|unavailable)` — the verdict of a completed
    /// acceptance assessment (F24); a stale judgment is ignored (Appendix C).
    Judgment(JudgmentVerdict),
    /// `deadline(idle|repair|judgment|max_age)` — the stored absolute
    /// deadline fired (F22).
    Deadline(DeadlineKind),
    /// `cancel` — `cancel {runId, closePane?}` (F20); on a settled Run it
    /// only closes the pane.
    Cancel {
        /// Whether to dispatch a verified close effect (F10).
        close_pane: bool,
    },
    /// `provider_limited` — the provider-limit judgment cleared the policy
    /// threshold (F21/F23).
    ProviderLimited,
    /// `evidence(digest)` — the transcript/git evidence digest changed
    /// (F23); a digest that differs from `evidence_digest` records it and
    /// bumps `evidence_generation`, so the pending Jev answers go stale
    /// (F20) and reviews re-ask under the new generation.
    Evidence {
        /// The newly observed evidence digest.
        digest: Digest,
    },
    /// `effect_result` — a journaled effect resolved (F8).
    EffectResult(EffectResult),
    /// `restart` — the daemon restarted and re-derived the Run (F28);
    /// `dispatching` effects become `unconfirmed`, deadlines unchanged.
    Restart,
}

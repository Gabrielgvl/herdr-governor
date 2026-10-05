//! F30 — retirement: the pure core of the post-`accepted` pane close.
//! The daemon sweeps the §4.16 proof chain and gathers each check's
//! outcome as a `RetirementProof` value (every input a value — no I/O
//! here); `retire_close` plans the one verified `close` effect only
//! when every check holds — fail closed on every field — bounded to
//! `RETIRE_MAX_ATTEMPTS` journal attempts under `run:<id>:retire[:<n>]`.
//! Only `accepted` Runs are candidates (ADR-0003: a `provider_limited`
//! pane is never closed by the governor, and the other settlements never
//! produce one either).

use alloc::format;

use crate::identity::EffectKey;
use crate::lifecycle::{
    Effect, EffectKind, EffectState, EffectTarget, Run, Settlement, effect_key, op_digest,
    planned_effect,
};
use crate::task::Retention;

/// F30 — at most three close attempts per Run (`retire`, `retire:1`,
/// `retire:2`): a `failed`/`unconfirmed` attempt leaves the next under
/// the `:<n>` suffix; an acknowledged one retires the family forever.
pub const RETIRE_MAX_ATTEMPTS: u64 = 3;

/// §4.16 check 2 — the captured identity is bound to a `native_session`
/// and the harness kind has a positional transcript reader.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentityBinding {
    /// Holds — the session is captured and the kind readable.
    Bound,
    /// `identity_unbound` — no captured identity or no `native_session`.
    Unbound,
    /// `trace_unsupported_kind` — no positional reader for the kind.
    UnsupportedKind,
}

/// §4.16 check 3 — the retirement anchor row and its transcript
/// fingerprint, captured at freeze.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnchorProof {
    /// Holds — the anchor row exists with a fingerprint and
    /// `follow_up_seen = 0`.
    Held,
    /// `trace_anchor_missing` — no anchor row for `(run, work_generation,
    /// judging_digest)`.
    AnchorMissing,
    /// `trace_history_missing` — the fingerprint capture failed at freeze.
    HistoryMissing,
    /// `trace_follow_up` — a user turn sat in the freeze-time tail.
    FollowUpSeen,
}

/// §4.16 check 4 — the fresh snapshot's classification of the child.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaneObservation {
    /// `unique` with status `idle` or `done`.
    IdleOrDone,
    /// `absent` — nothing left to close (`skipped`, `child_absent`).
    Absent,
    /// `invalid`, `working`, `blocked` or unreported — defer and reset the
    /// stability clock.
    Defer,
}

/// §4.16 check 5 — neither the pane nor the child's own caller key is a
/// live caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallerProof {
    /// Neither link is a caller.
    Clear,
    /// `pane_is_caller` — the pane is a bound caller's pane.
    PaneIsCaller,
    /// `child_is_caller` — the child's own caller key owns live work or
    /// unacked events.
    ChildIsCaller,
}

/// §4.16 check 6 — the stability clock: `state_change_seq` and the screen
/// digest unchanged, status idle/done, for the whole `retire_grace_secs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StabilityProof {
    /// The grace elapsed uninterrupted.
    Elapsed,
    /// `watching` — the lane is still being timed.
    Watching,
}

/// §4.16 check 7 — the marked file still reads the judged digest and the
/// frozen copy still hashes to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactProof {
    /// Both verifications hold.
    Held,
    /// `artifact_changed` — either digest moved.
    Changed,
    /// `artifact_missing` — the marked file or frozen copy is gone.
    Missing,
}

/// §4.16 check 8 — the trace delta since the freeze-time cursor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TraceProof {
    /// The delta holds no user turn and nothing ambiguous.
    Held,
    /// `trace_follow_up` — a `UserTurn` appears in the delta.
    FollowUp,
    /// `trace_source_rewritten` — truncation below the cursor, an anchor
    /// mismatch or a replaced path (rollback/compaction/recycled file).
    SourceRewritten,
    /// `trace_source_exceeds_budget` — the window read hit its bound.
    SourceExceedsBudget,
    /// `trace_source_malformed` — the source cannot be parsed.
    SourceMalformed,
    /// `trace_source_unreadable` — the source cannot be opened (a defer).
    SourceUnreadable,
    /// `trace_pending_tail` — unterminated bytes after the last record.
    PendingTail,
    /// `trace_ambiguous:*` — a compaction or session drift in the delta.
    Ambiguous,
}

/// §4.16 check 9 — the composer guard for harnesses whose input
/// composer can queue a draft (kinds without one hold trivially).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComposerProof {
    /// Nothing queued or drafted.
    Held,
    /// `composer_queued` — a queued glyph is visible.
    Queued,
    /// `composer_draft` — an unsent draft is present.
    Draft,
    /// `composer_unreadable` — the frame cannot be parsed (a defer).
    Unreadable,
}

/// §4.16 — one sweep's gathered proof: every check's outcome as a typed
/// field, the fail-closed vocabulary as variants (each failing variant's
/// doc names its `retirements.reason` spelling — bounded, never free
/// text). The daemon gathers the values (I/O); the core decides.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetirementProof {
    /// Check 1 — the Launch's `Task.retention` resolved against the
    /// default; `keep` opts out (`kept`, terminal).
    pub retention: Retention,
    /// Check 2 — the identity and transcript-kind binding.
    pub binding: IdentityBinding,
    /// Check 3 — the retirement anchor.
    pub anchor: AnchorProof,
    /// Check 4 — the snapshot's read of the child.
    pub observation: LaneObservation,
    /// Check 5 — the caller links.
    pub callers: CallerProof,
    /// Check 6 — the stability clock.
    pub stability: StabilityProof,
    /// Check 7 — the artifact.
    pub artifact: ArtifactProof,
    /// Check 8 — the trace delta.
    pub trace: TraceProof,
    /// Check 9 — the composer guard.
    pub composer: ComposerProof,
    /// Check 10 — `retire_enabled`: `false` runs the dry-run (`disabled`,
    /// `would_retire` journaled once per decision change); to the core it
    /// means only that no close may be planned.
    pub enabled: bool,
}

impl RetirementProof {
    /// The §4.16 fail-closed table as one conjunction — every check
    /// holds. `retire_close` plans nothing otherwise.
    #[must_use]
    pub fn holds(&self) -> bool {
        self.retention == Retention::Retire
            && self.binding == IdentityBinding::Bound
            && self.anchor == AnchorProof::Held
            && self.observation == LaneObservation::IdleOrDone
            && self.callers == CallerProof::Clear
            && self.stability == StabilityProof::Elapsed
            && self.artifact == ArtifactProof::Held
            && self.trace == TraceProof::Held
            && self.composer == ComposerProof::Held
            && self.enabled
    }
}

/// F30 — plan the retirement close: `Some` of a `close` effect only for
/// an `accepted` Run whose full proof chain holds, keyed
/// `run:<id>:retire[:<n>]` and bounded to [`RETIRE_MAX_ATTEMPTS`] journal
/// attempts — the ask-family rule applied to the close: an in-flight or
/// acknowledged member suppresses planning, a `failed`/`unconfirmed` one
/// leaves the next attempt under `:<n>`. Every other settlement —
/// `provider_limited` included (ADR-0003) — plans nothing.
#[must_use]
pub fn retire_close(run: &Run, proof: &RetirementProof, journal: &[Effect]) -> Option<Effect> {
    if run.settlement != Some(Settlement::Accepted) || !proof.holds() {
        return None;
    }
    let identity = run.identity.clone()?;
    let key = next_retire_key(journal, &effect_key(run, "retire"))?;
    let target = EffectTarget::Child(identity);
    // `close` takes no params — the captured Child target is its whole
    // rendered form (OQ-15); the same op_digest the `cancel` close plans.
    Some(planned_effect(
        run,
        EffectKind::Close,
        key,
        Some(target.clone()),
        Some(op_digest(EffectKind::Close, Some(&target), &[])),
    ))
}

/// The retire family's next key — `retire`, then `retire:1`, `retire:2`
/// — or `None` when an attempt is in flight (`planned`/`dispatching`),
/// an attempt was acknowledged (the pane provably closed — the family is
/// done), or [`RETIRE_MAX_ATTEMPTS`] terminal attempts already stand.
fn next_retire_key(journal: &[Effect], base: &EffectKey) -> Option<EffectKey> {
    let prefix = format!("{}:", base.0);
    let mut attempts = 0_u64;
    for effect in journal {
        if effect.key != *base && !effect.key.0.starts_with(&prefix) {
            continue;
        }
        match effect.state {
            EffectState::Planned | EffectState::Dispatching | EffectState::Acknowledged => {
                return None;
            }
            EffectState::Failed | EffectState::Unconfirmed => {
                attempts = attempts.saturating_add(1);
            }
        }
    }
    if attempts >= RETIRE_MAX_ATTEMPTS {
        return None;
    }
    Some(match attempts {
        0 => base.clone(),
        count => EffectKey(format!("{}:{count}", base.0)),
    })
}

#[cfg(test)]
mod tests;

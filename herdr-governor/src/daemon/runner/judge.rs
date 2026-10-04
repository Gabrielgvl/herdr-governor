//! `runner/judge` — the runner's Jev half: one `judge` call over a
//! frozen `JudgeParams`, the result stamped into the `JudgmentSet`
//! frame as the `Judgments` receipt. A failed call still acknowledges —
//! the set row *is* the honest record of the attempt, its `outcome` the
//! `JevError::outcome()` class (F12/F24); only a dispatch that never ran
//! (the shutdown gate, a refused commit) produces no set.
//!
//! Live since B2: `context_for` renders the launch `evaluate` ask
//! (`RenderContext::Jev`) and `RunnerEnv` carries the client, key and
//! deadline (`daemon::run` wires them from `[daemon]` + the §19
//! credential). Run-bound asks (`review:*`, `accept:*`, `blocked:*`,
//! `limit:*`) render their contexts with C3/C4. The `#[cfg(test)] mod
//! jev` sibling holds this lane's tests — keeping them there leaves the
//! module's test gate an addition, never a removal.

use governor_core::lifecycle::{EffectReceipt, EffectResolution};
use governor_core::routing::{JudgmentOutcome, JudgmentRecord, JudgmentSet};

use crate::adapters::jev::{ApiKey, Client as JevClient, JudgeParams};

/// §4.4's Jev leg — the `JudgeParams` come straight out of the frozen
/// context; `params.timeout` is the per-request deadline.
pub(super) async fn wire(
    client: &JevClient,
    key: &ApiKey,
    params: &JudgeParams,
    set: &JudgmentSet,
) -> EffectResolution {
    match client.judge(key, params).await {
        Ok(judged) => record(
            set,
            JudgmentOutcome::Answered,
            judged.model,
            judged.judgments,
        ),
        Err(error) => record(set, error.outcome(), params.model.clone(), Vec::new()),
    }
}

/// Stamp the set's two wire-resolved fields — `outcome` and the model
/// that answered (the requested one when nothing did) — and wrap the
/// answers in the receipt.
pub(super) fn record(
    set: &JudgmentSet,
    outcome: JudgmentOutcome,
    model: String,
    judgments: Vec<governor_core::routing::Judgment>,
) -> EffectResolution {
    let mut stamped = set.clone();
    stamped.outcome = outcome;
    stamped.model = model;
    EffectResolution::Acknowledged {
        receipt: Some(EffectReceipt::Judgments(JudgmentRecord {
            set: stamped,
            judgments,
        })),
    }
}

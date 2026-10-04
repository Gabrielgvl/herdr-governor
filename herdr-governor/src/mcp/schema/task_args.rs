//! F5 — the `task` DTO of `herdr_launch`, split from `schema` by the
//! 500-line file cap; `schema`'s field-set test still pins it to the
//! `inputSchema` `task` subschema.

use governor_core::config::Tier;
use governor_core::identity::RunId;
use governor_core::task::{Retention, Task};
use serde::{Deserialize, Serialize};

/// The `task` member of [`super::LaunchArgs`] — F5's field set and bounds:
/// `objective`/`scope`/`doneWhen` required (`doneWhen` 1–8 items),
/// `constraints` optional (0–8), `tier`/`recoveryOf`/`label`/`cwd`/
/// `retention` optional.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(in crate::mcp) struct TaskArgs {
    /// The work to do (non-empty, F5).
    pub objective: String,
    /// Where the work is allowed to happen (non-empty, F5).
    pub scope: String,
    /// 1–8 verifiable items.
    pub done_when: Vec<String>,
    /// 0–8 items; the spec default is `[]`, never a missing key.
    #[serde(default)]
    pub constraints: Vec<String>,
    /// The caller's uplift input to routing (F13 step 3).
    #[serde(default)]
    pub tier: Option<String>,
    /// F21 — the settled predecessor to continue.
    #[serde(default)]
    pub recovery_of: Option<String>,
    /// Presentation-only display label (H#41).
    #[serde(default)]
    pub label: Option<String>,
    /// Canonical realpath inside `projectRoot`, else the root itself.
    #[serde(default)]
    pub cwd: Option<String>,
    /// F30 — the retirement opt-out (`retire`/`keep`); absent is `retire`.
    #[serde(default)]
    pub retention: Option<RetentionArg>,
}

/// F30 — `task.retention` on the wire: `retire`/`keep` are the whole
/// vocabulary, so a third spelling refuses at decode like an unknown
/// field — the core `Retention` carries no serde.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::mcp) enum RetentionArg {
    /// `retire` — the accepted Run's pane closes automatically.
    Retire,
    /// `keep` — the pane stays open.
    Keep,
}

impl TaskArgs {
    /// The core Task the DTO carries — the field map only; every value
    /// bound (non-empty text, the item bounds, `cwd` canonical and inside
    /// the root, the 64 KiB render bound) is `Task::violations`' (F5).
    #[must_use]
    pub(in crate::mcp) fn into_task(self) -> Task {
        Task {
            objective: self.objective,
            scope: self.scope,
            done_when: self.done_when,
            constraints: self.constraints,
            tier: self.tier.map(Tier),
            recovery_of: self.recovery_of.map(RunId),
            label: self.label,
            cwd: self.cwd,
            retention: self.retention.map(|retention| match retention {
                RetentionArg::Retire => Retention::Retire,
                RetentionArg::Keep => Retention::Keep,
            }),
        }
    }
}

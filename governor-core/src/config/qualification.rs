//! F26 — qualification evidence: the capability names an operating point
//! claims, the pass rows keyed by `(operating point, args digest)`, and the
//! current-pass predicate routing and delivery consult.

use alloc::string::String;

use crate::identity::Digest;

use super::{OperatingPoint, OperatingPointId};

/// F26 — a qualified property an operating point offers; the catalog claims
/// it, `herdr-governor qualify` proves it, routing and delivery use only
/// current passes.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Capability(pub String);

impl Capability {
    /// F26 — the canned Task starts the harness.
    pub const START: &'static str = "start";
    /// F26 — the harness acknowledges a prompt.
    pub const PROMPT_ACK: &'static str = "prompt_ack";
    /// F26 — `handoff_write`: the harness can write the handoff path.
    pub const HANDOFF_WRITE: &'static str = "handoff_write";
    /// F26 — `followup_read`: the harness reads a follow-up.
    pub const FOLLOWUP_READ: &'static str = "followup_read";
    /// F17 — `mid_turn_input`: follow-ups may be sent while the child works.
    pub const MID_TURN_INPUT: &'static str = "mid_turn_input";
    /// F18 — `hint_consumption`: the owner's pane consumes hint prompts.
    pub const HINT_CONSUMPTION: &'static str = "hint_consumption";

    /// The capability name as written in the catalog and qualifications.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// F26/Appendix B `qualifications` — one capability's pass or fail for an
/// operating point, keyed by `(operating point, args digest)`; an args change
/// re-keys the row and invalidates the old pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Qualification {
    /// The operating point that was qualified.
    pub operating_point: OperatingPointId,
    /// Digest of the exact arguments the qualification ran against.
    pub args_digest: Digest,
    /// The capability exercised.
    pub capability: Capability,
    /// Whether the point passed.
    pub passed: bool,
    /// `evidence_json` — the qualification's evidence payload.
    pub evidence: String,
}

/// F26 — THE args digest: sha-256 over each part length-prefixed (`u64`
/// big-endian length then bytes, in order) — platform-independent, and
/// unambiguous (`["a", "bc"]` and `["ab", "c"]` never share a digest).
/// `qualify` records each pass under this digest and routing's step-6 check
/// reads the same value; F13 step 5's exploration seed shares the framing.
#[must_use]
pub fn args_digest<'p>(parts: impl Iterator<Item = &'p str>) -> Digest {
    use sha2::Digest as _;
    let mut hasher = sha2::Sha256::new();
    for part in parts {
        hasher.update(u64::try_from(part.len()).unwrap_or(u64::MAX).to_be_bytes());
        hasher.update(part.as_bytes());
    }
    Digest(hasher.finalize().into())
}

impl OperatingPoint {
    /// F26 — the digest a qualification row binds: `args_digest` over the
    /// point's exact launch arguments. Editing `args` re-keys the rows and
    /// orphans every earlier pass — an args change invalidates the
    /// qualification.
    #[must_use]
    pub fn args_digest(&self) -> Digest {
        args_digest(self.args.iter().map(String::as_str))
    }

    /// F26 — the 'current pass' predicate routing (F13) and delivery
    /// (F17/F18) use: `capability` counts only while the catalog claims it
    /// AND a passed `Qualification` exists keyed by `(operating point, args
    /// digest)` for the CURRENT args. An args edit re-keys and so invalidates
    /// the pass, and a point that never qualified offers nothing, however it
    /// is marked up.
    #[must_use]
    pub fn has_current_pass(
        &self,
        capability: &Capability,
        qualifications: &[Qualification],
    ) -> bool {
        if !self.capabilities.contains(capability) {
            return false;
        }
        let args_digest = self.args_digest();
        qualifications.iter().any(|qualification| {
            qualification.passed
                && qualification.operating_point == self.id
                && qualification.capability == *capability
                && qualification.args_digest == args_digest
        })
    }
}

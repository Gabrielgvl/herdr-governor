//! `seam` — the fault-injection seam value (§4.4, → F18/F24/F16).
//! `GOV_DAEMON_SEAM=<suffix>@<boundary>:<action>` is the env spelling child
//! tests set; the `SeamConfig` value `daemon::run` takes is what in-process
//! tests pass. A1 carries the parsed value through to the coordinator;
//! `runner/seam.rs` (P5.B1) matches it at each dispatch boundary.
//! Production code with a test-only effect, disclosed per OQ-T.

use std::time::Duration;

/// The seam's env var (read once at `daemon::run`, after the explicit
/// `SeamConfig` argument — argv/`--seam` outranks env).
const SEAM_ENV: &str = "GOV_DAEMON_SEAM";

/// Which dispatch boundary the seam fires at (§4.4's boundary table).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Boundary {
    /// Before the runner's wire write — a kill leaves the row `planned`.
    PreDispatch,
    /// After `[WriteEffect::Dispatch]` commits — a kill leaves
    /// `dispatching`, marked `unconfirmed` at restart, never on the wire.
    DispatchCommitted,
    /// After the Herdr/Jev call returned — the fake applied it, the result
    /// was never journaled.
    WireReturned,
    /// After `transition(Event::EffectResult)` committed.
    ResultCommitted,
}

impl Boundary {
    /// The `<boundary>` spelling of the seam spec.
    fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "pre_dispatch" => Self::PreDispatch,
            "dispatch_committed" => Self::DispatchCommitted,
            "wire_returned" => Self::WireReturned,
            "result_committed" => Self::ResultCommitted,
            _ => return None,
        })
    }
}

/// What the seam does at its boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeamAction {
    /// `std::process::abort()` — the F8 kill: no cleanup, no flush.
    Abort,
    /// `tokio::time::sleep(ms)` — the F16/F29 pre-wire shutdown race,
    /// paused-clock-driven in-process.
    Pause(Duration),
}

/// One armed fault seam: `suffix` is the effect-key suffix matcher
/// (`prompt:task`, `start:*`, `outbox:1`, … — ids are minted at run time,
/// so tests match the stable suffix), `boundary` where it fires, `action`
/// what it does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeamConfig {
    /// The effect-key suffix matcher (`start:*` matches any attempt).
    pub suffix: String,
    /// The dispatch boundary it fires at.
    pub boundary: Boundary,
    /// What it does there.
    pub action: SeamAction,
}

/// Malformed seam spec — reported through the usage exit (2).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("malformed seam spec: {0}")]
pub struct SeamError(pub String);

impl SeamConfig {
    /// Parse the `GOV_DAEMON_SEAM` env var when set and non-empty;
    /// `Ok(None)` means unarmed. A malformed value is an error — a silent
    /// no-op seam would let an F8 test pass on the wrong reason.
    pub fn from_env() -> Result<Option<Self>, SeamError> {
        match std::env::var(SEAM_ENV) {
            Ok(spec) if !spec.is_empty() => Self::parse(&spec).map(Some),
            _ => Ok(None),
        }
    }

    /// `<suffix>@<boundary>:<action>` — e.g. `prompt:task@wire_returned:abort`
    /// or `start:*@pre_dispatch:pause:250`. The suffix itself may contain
    /// `:`; only `@` separates it from the boundary.
    pub fn parse(spec: &str) -> Result<Self, SeamError> {
        let bad = || SeamError(spec.to_string());
        let (suffix, rest) = spec.split_once('@').ok_or_else(bad)?;
        if suffix.is_empty() {
            return Err(bad());
        }
        let (boundary_text, action_text) = rest.split_once(':').ok_or_else(bad)?;
        let boundary = Boundary::parse(boundary_text).ok_or_else(bad)?;
        let action = match action_text {
            "abort" => SeamAction::Abort,
            pause => {
                let ms = pause
                    .strip_prefix("pause:")
                    .and_then(|digits| digits.parse::<u64>().ok())
                    .ok_or_else(bad)?;
                SeamAction::Pause(Duration::from_millis(ms))
            }
        };
        Ok(Self {
            suffix: suffix.to_string(),
            boundary,
            action,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{Boundary, SeamAction, SeamConfig};

    /// The spec grammar round-trips: suffixes with their own `:` (`prompt:task`),
    /// `abort` and `pause:<ms>` actions, and every boundary spelling.
    #[test]
    fn seam_parses_suffix_boundary_and_action() {
        let seam = SeamConfig::parse("prompt:task@wire_returned:abort").expect("spec");
        assert_eq!(seam.suffix, "prompt:task");
        assert_eq!(seam.boundary, Boundary::WireReturned);
        assert_eq!(seam.action, SeamAction::Abort);

        let pause_seam = SeamConfig::parse("start:*@pre_dispatch:pause:250").expect("spec");
        assert_eq!(pause_seam.suffix, "start:*");
        assert_eq!(pause_seam.boundary, Boundary::PreDispatch);
        assert_eq!(
            pause_seam.action,
            SeamAction::Pause(Duration::from_millis(250))
        );

        for (text, boundary) in [
            ("pre_dispatch", Boundary::PreDispatch),
            ("dispatch_committed", Boundary::DispatchCommitted),
            ("wire_returned", Boundary::WireReturned),
            ("result_committed", Boundary::ResultCommitted),
        ] {
            let parsed = SeamConfig::parse(&format!("close@{text}:abort")).expect("spec");
            assert_eq!(parsed.boundary, boundary);
        }
    }

    /// Malformed specs are errors, not silent no-ops.
    #[test]
    fn seam_rejects_malformed_specs() {
        for spec in [
            "",
            "prompt:task",                      // no @boundary:action
            "@pre_dispatch:abort",              // empty suffix
            "prompt:task@nope:abort",           // unknown boundary
            "prompt:task@pre_dispatch:nope",    // unknown action
            "prompt:task@pre_dispatch:pause:",  // missing ms
            "prompt:task@pre_dispatch:pause:x", // non-numeric ms
        ] {
            assert!(SeamConfig::parse(spec).is_err(), "rejects {spec:?}");
        }
    }
}

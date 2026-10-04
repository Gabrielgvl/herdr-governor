//! The §4.4 `HerdrError` → `EffectResolution` map — every variant's class.

use governor_core::lifecycle::{EffectCertainty, EffectOutcome};

use super::resolve;
use crate::adapters::herdr::HerdrError;

/// §4.4's map verbatim: `agent_pane_busy` is the only
/// `PreInteractiveFailed`; the never-ran class is `absent`; every
/// other failure is `unknown`.
#[test]
fn resolution_map_covers_every_herdr_error() {
    let cases: Vec<(HerdrError, EffectOutcome)> = vec![
        (
            HerdrError::AgentPaneBusy {
                message: "busy".into(),
            },
            EffectOutcome::PreInteractiveFailed,
        ),
        (
            HerdrError::AgentNotFound {
                message: "gone".into(),
            },
            EffectOutcome::Failed {
                certainty: EffectCertainty::Absent,
            },
        ),
        (
            HerdrError::PaneNotFound {
                message: "gone".into(),
            },
            EffectOutcome::Failed {
                certainty: EffectCertainty::Absent,
            },
        ),
        (
            HerdrError::Uncorrelated {
                code: "x".into(),
                message: "y".into(),
            },
            EffectOutcome::Failed {
                certainty: EffectCertainty::Absent,
            },
        ),
        (
            HerdrError::Connect(std::io::Error::from(std::io::ErrorKind::NotFound)),
            EffectOutcome::Failed {
                certainty: EffectCertainty::Absent,
            },
        ),
        (
            HerdrError::Timeout {
                message: "t".into(),
            },
            EffectOutcome::Failed {
                certainty: EffectCertainty::Unknown,
            },
        ),
        (
            HerdrError::DeadlineExceeded,
            EffectOutcome::Failed {
                certainty: EffectCertainty::Unknown,
            },
        ),
        (
            HerdrError::Io(std::io::Error::from(std::io::ErrorKind::BrokenPipe)),
            EffectOutcome::Failed {
                certainty: EffectCertainty::Unknown,
            },
        ),
        (
            HerdrError::StreamClosed,
            EffectOutcome::Failed {
                certainty: EffectCertainty::Unknown,
            },
        ),
        (
            HerdrError::Malformed { detail: "d".into() },
            EffectOutcome::Failed {
                certainty: EffectCertainty::Unknown,
            },
        ),
        (
            HerdrError::FrameTooLarge,
            EffectOutcome::Failed {
                certainty: EffectCertainty::Unknown,
            },
        ),
        (
            HerdrError::Server {
                code: "internal".into(),
                message: "x".into(),
            },
            EffectOutcome::Failed {
                certainty: EffectCertainty::Unknown,
            },
        ),
    ];
    for (error, want) in cases {
        let got = resolve(&error).outcome();
        assert_eq!(got, want, "resolution for the error");
    }
}

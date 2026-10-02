//! P4.1 — the OQ-13 terminal write's kind, asserted on the public
//! `EffectWrite` shape. Lives beside `launch_row_tests.rs`, which sits at
//! the 500-line hygiene cap; inputs come from `super::launch_row::fixtures`.

use crate::identity::EffectKey;
use crate::lifecycle::{EffectCertainty, EffectState, EffectWrite};

use super::launch_row::fixtures::{NOW, abstain, launch, policy, writes};
use super::{AbstainReason, finish};

#[test]
fn finish_emits_terminal_not_result() {
    // OQ-13: the stranded eval write is the terminal kind — never a result
    // commit (which only ever follows `dispatching`) and never a dispatch —
    // carrying the certainty the stranded state earns.
    for (state, certainty) in [
        (EffectState::Planned, EffectCertainty::Absent),
        (EffectState::Dispatching, EffectCertainty::Unknown),
        (EffectState::Unconfirmed, EffectCertainty::Unknown),
    ] {
        let t = finish(
            &launch(),
            abstain(AbstainReason::EvaluationFailed),
            Some(state),
            None,
            NOW,
            &policy(),
        );
        let expected = EffectWrite::Terminal {
            key: EffectKey("launch:l-1:evaluate".into()),
            certainty,
        };
        assert_eq!(writes(&t), [&expected], "{state:?}");
    }
}

//! The launch/run/judgment/effect round-trips and their corrupt-row cases,
//! on the `tests` fixtures and the `SELECT`-bound row helper.

use rusqlite::Connection;
use serde_json::Value;

use governor_core::identity::{ChildIdentity, JudgmentSetId, PaneId, TabId};
use governor_core::lifecycle::{
    Effect, EffectCertainty, EffectReceipt, EffectState, EffectTarget, Settlement, UnresolvedReason,
};
use governor_core::routing::PlacementPlan;
use governor_core::task::{AbstainReason, Launch, LaunchOutcome, LaunchPhase, Task};

use super::{
    NOW, caller, decision, effect, identity, judgment_set, launch, launch_outcomes, poison, record,
    run_bare, run_full, task, via_sqlite,
};
use crate::store::StoreError;
use crate::store::rows::effect::EffectRow;
use crate::store::rows::judgment::{JudgmentRow, JudgmentSetRow};
use crate::store::rows::launch::{LaunchRow, decision_to_json};
use crate::store::rows::run::RunRow;

#[test]
fn launch_round_trips_every_outcome() {
    let open = launch(LaunchPhase::Routed, None);
    let open_row = LaunchRow::from_core(&open, 7, NOW).unwrap();
    let open_back = via_sqlite(&open_row.params(), LaunchRow::read).unwrap();
    assert_eq!(
        open_back.to_core(caller()).unwrap(),
        open,
        "open launch round-trip"
    );
    for outcome in launch_outcomes() {
        let done = launch(LaunchPhase::Done, Some(outcome));
        let row = LaunchRow::from_core(&done, 7, NOW).unwrap();
        let back = via_sqlite(&row.params(), LaunchRow::read).unwrap();
        assert_eq!(
            back.to_core(caller()).unwrap(),
            done,
            "done launch round-trip"
        );
    }
    let minimal = Launch {
        decision: None,
        config_version: None,
        task: Task {
            tier: None,
            label: None,
            retention: None,
            ..task()
        },
        ..launch(LaunchPhase::Evaluating, None)
    };
    let min_row = LaunchRow::from_core(&minimal, 7, NOW).unwrap();
    let min_back = via_sqlite(&min_row.params(), LaunchRow::read).unwrap();
    assert_eq!(
        min_back.to_core(caller()).unwrap(),
        minimal,
        "all-None launch round-trip"
    );
}

#[test]
fn corrupt_enum_text_is_typed_error_not_panic() {
    let mut params = LaunchRow::from_core(&launch(LaunchPhase::Routed, None), 7, NOW)
        .unwrap()
        .params();
    poison(&mut params, "phase", "bogus");
    let err = via_sqlite(&params, LaunchRow::read)
        .unwrap()
        .to_core(caller())
        .unwrap_err();
    assert!(
        matches!(
            err,
            StoreError::CorruptRow {
                table: "launches",
                column: "phase",
                ..
            }
        ),
        "unknown phase text must be CorruptRow, got {err}"
    );
}

#[test]
fn launch_outcome_mirror_columns_must_agree() {
    let done = launch(
        LaunchPhase::Done,
        Some(LaunchOutcome::Abstained {
            reason: AbstainReason::NoCandidates,
        }),
    );
    let mut params = LaunchRow::from_core(&done, 7, NOW).unwrap().params();
    poison(&mut params, "outcome", "rejected");
    let err = via_sqlite(&params, LaunchRow::read)
        .unwrap()
        .to_core(caller())
        .unwrap_err();
    assert!(matches!(err, StoreError::CorruptRow { .. }), "{err}");
}

#[test]
fn decision_json_carries_exploration_at_view_path() {
    // The `outcomes` view reads `$.exploration.assigned`/`.executed` with
    // `json_extract`: the codec's own text must answer at that path.
    let conn = Connection::open_in_memory().unwrap();
    let text = serde_json::to_string(&decision_to_json(&decision())).unwrap();
    let (assigned, executed): (Option<i64>, Option<i64>) = conn
        .query_row(
            "SELECT json_extract(?1, '$.exploration.assigned'), \
             json_extract(?1, '$.exploration.executed')",
            [&text],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        (assigned, executed),
        (Some(1), Some(0)),
        "view path must resolve"
    );
    let value: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        value["candidates"][0]["args"],
        serde_json::json!(["--flag", "x"])
    );
}

#[test]
fn run_round_trips_bare_and_full() {
    for run in [
        run_bare(),
        run_full(Settlement::Accepted),
        run_full(Settlement::Unresolved {
            reason: UnresolvedReason::MaxAge,
        }),
    ] {
        let row = RunRow::from_core(&run, 7, NOW).unwrap();
        let back = via_sqlite(&row.params(), RunRow::read).unwrap();
        assert_eq!(back.to_core(caller()).unwrap(), run, "run round-trip");
    }
}

#[test]
fn run_partial_identity_group_is_corrupt() {
    let mut params = RunRow::from_core(&run_full(Settlement::Cancelled), 7, NOW)
        .unwrap()
        .params();
    let slot = params
        .iter_mut()
        .find(|(name, _)| *name == "terminal_id")
        .unwrap();
    slot.1 = rusqlite::types::Value::Null;
    let err = via_sqlite(&params, RunRow::read)
        .unwrap()
        .to_core(caller())
        .unwrap_err();
    assert!(
        matches!(
            err,
            StoreError::CorruptRow {
                column: "identity",
                ..
            }
        ),
        "{err}"
    );
}

#[test]
fn judgment_rows_round_trip() {
    for set in [judgment_set(None), record().set] {
        let row = JudgmentSetRow::from_core(&set, NOW, Some(NOW)).unwrap();
        let back = via_sqlite(&row.params(), JudgmentSetRow::read).unwrap();
        assert_eq!(back.to_core().unwrap(), set, "judgment_sets round-trip");
    }
    for judgment in record().judgments {
        let row = JudgmentRow::from_core(&JudgmentSetId("js-1".into()), &judgment).unwrap();
        let back = via_sqlite(&row.params(), JudgmentRow::read).unwrap();
        assert_eq!(back.to_core().unwrap(), judgment, "judgments round-trip");
    }
    let mut params = JudgmentRow::from_core(&JudgmentSetId("js-1".into()), &record().judgments[1])
        .unwrap()
        .params();
    poison(&mut params, "probabilities_json", r#"{"yes": 1.5}"#);
    let err = via_sqlite(&params, JudgmentRow::read)
        .unwrap()
        .to_core()
        .unwrap_err();
    assert!(matches!(err, StoreError::CorruptRow { .. }), "{err}");
}

#[test]
fn effect_round_trips_every_target_and_receipt() {
    let targets = [
        None,
        Some(EffectTarget::ExistingTab(TabId("tab-1".into()))),
        Some(EffectTarget::CallerContext(PaneId("p-0".into()))),
        Some(EffectTarget::AgentPane(PlacementPlan::NewTab)),
        Some(EffectTarget::AgentPane(PlacementPlan::ExistingTab {
            tab: TabId("tab-1".into()),
        })),
        Some(EffectTarget::Child(identity())),
    ];
    let receipts = [
        None,
        Some(EffectReceipt::AgentStarted {
            identity: ChildIdentity {
                native_session: None,
                ..identity()
            },
        }),
        Some(EffectReceipt::Judgments(record())),
        Some(EffectReceipt::TabCreated {
            tab: TabId("tab-1".into()),
            pane: PaneId("p-1".into()),
        }),
        Some(EffectReceipt::PaneCreated {
            pane: PaneId("p-2".into()),
        }),
    ];
    for target in &targets {
        for receipt in &receipts {
            let value = effect(target.clone(), receipt.clone());
            let row = EffectRow::from_core(&value, NOW, Some(NOW)).unwrap();
            let back = via_sqlite(&row.params(), EffectRow::read).unwrap();
            assert_eq!(back.to_core().unwrap(), value, "effect round-trip");
        }
    }
}

#[test]
fn effect_failure_cause_is_not_a_receipt_and_needs_certainty() {
    let failed = Effect {
        state: EffectState::Failed,
        certainty: Some(EffectCertainty::Absent),
        ..effect(None, None)
    };
    let mut params = EffectRow::from_core(&failed, NOW, None).unwrap().params();
    poison(&mut params, "result_json", r#"{"error": "busy pane"}"#);
    let back = via_sqlite(&params, EffectRow::read)
        .unwrap()
        .to_core()
        .unwrap();
    assert_eq!(
        back.receipt, None,
        "a failure cause is not a receipt (OQ-11)"
    );
    let slot = params
        .iter_mut()
        .find(|(name, _)| *name == "certainty")
        .unwrap();
    slot.1 = rusqlite::types::Value::Null;
    let err = via_sqlite(&params, EffectRow::read)
        .unwrap()
        .to_core()
        .unwrap_err();
    assert!(
        matches!(
            err,
            StoreError::CorruptRow {
                column: "certainty",
                ..
            }
        ),
        "{err}"
    );
}

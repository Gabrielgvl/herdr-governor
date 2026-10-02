//! P4.S3 — `store::apply` contract tests for the journal (`WriteEffect`)
//! kinds: the dispatch commit, the result commit (restart marking, the
//! OQ-11 cause, the `Judgments` receipt), the OQ-13 terminal write, the
//! plan-time insert, and the composed abstained finish.

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use governor_core::config::ConfigVersion;
    use governor_core::identity::{Digest, EffectKey, EventId, JudgmentSetId, RunId};
    use governor_core::lifecycle::{
        EffectCertainty, EffectKind, EffectReceipt, EffectResolution, EffectState, FailureCause,
        StateChange,
    };
    use governor_core::routing::{
        Judgment, JudgmentOutcome, JudgmentPurpose, JudgmentRecord, JudgmentSet, Probability,
        Question, QuestionVersion,
    };
    use governor_core::task::{AbstainReason, LaunchOutcome, LaunchPhase};
    use herdr_governor::store::{ApplyError, ConflictKind, Store};

    use crate::support::{
        LATER, NOW, binding, caller, changes, count, dispatch, effect, event, launch, result,
        seeded, store, terminal, transition,
    };

    const KEY: &str = "run:r-1:prompt:task";

    fn effect_cols(store: &Store, key: &str) -> (String, Option<String>, Option<String>) {
        store
            .conn()
            .query_row(
                "SELECT state, certainty, completed_at FROM effects WHERE effect_key = ?1",
                [key],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap()
    }

    fn result_json_of(store: &Store, key: &str) -> Option<String> {
        store
            .conn()
            .query_row(
                "SELECT result_json FROM effects WHERE effect_key = ?1",
                [key],
                |row| row.get(0),
            )
            .unwrap()
    }

    fn failed(certainty: EffectCertainty, cause: &str) -> EffectResolution {
        EffectResolution::Failed {
            certainty,
            cause: Some(FailureCause(cause.into())),
        }
    }

    fn assert_effect_conflict(err: &ApplyError) {
        assert!(
            matches!(
                err,
                ApplyError::Conflict {
                    kind: ConflictKind::Effect,
                    ..
                }
            ),
            "{err}"
        );
    }

    #[test]
    fn effect_arms_follow_the_journal_protocol() {
        let (_dir, mut store) = seeded();
        // A result commit follows `dispatching` only: on a planned row every
        // resolution — the restart marking and a provably-absent failure
        // included — is a conflict.
        for resolution in [
            EffectResolution::Unconfirmed,
            EffectResolution::Failed {
                certainty: EffectCertainty::Absent,
                cause: None,
            },
        ] {
            let err = store
                .apply(&changes(vec![result(KEY, resolution)]), LATER)
                .unwrap_err();
            assert_effect_conflict(&err);
        }
        assert_eq!(effect_cols(&store, KEY).0, "planned");
        store.apply(&changes(vec![dispatch(KEY)]), LATER).unwrap();
        let dispatched = store.effect(&EffectKey(KEY.into())).unwrap().unwrap();
        assert_eq!(dispatched.dispatched_at, Some(LATER));
        assert_eq!(
            dispatched.payload_digest,
            Some(Digest([0x5a; 32])),
            "the plan-time digest is untouched"
        );
        // A second dispatch commit loses: planned is the only source.
        let second = store
            .apply(&changes(vec![dispatch(KEY)]), LATER)
            .unwrap_err();
        assert_effect_conflict(&second);
        store
            .apply(
                &changes(vec![result(KEY, EffectResolution::Unconfirmed)]),
                LATER,
            )
            .unwrap();
        // unconfirmed never re-dispatches and never acknowledges.
        for write in [
            dispatch(KEY),
            result(KEY, EffectResolution::Acknowledged { receipt: None }),
        ] {
            let third = store.apply(&changes(vec![write]), LATER).unwrap_err();
            assert_effect_conflict(&third);
        }
        assert_eq!(effect_cols(&store, KEY).0, "unconfirmed");
    }

    #[test]
    fn terminal_write_planned_persists_verbatim() {
        let (_dir, mut store) = seeded();
        let absent = terminal(KEY, EffectCertainty::Absent);
        store.apply(&changes(vec![absent]), LATER).unwrap();
        let (state, certainty, completed) = effect_cols(&store, KEY);
        assert_eq!(
            (state.as_str(), certainty.as_deref()),
            ("failed", Some("absent"))
        );
        assert_eq!(completed.as_deref(), Some("2026-10-01T00:01:00.000Z"));
    }

    #[test]
    fn terminal_write_unconfirmed_persists_verbatim() {
        let (_dir, mut store) = seeded();
        store.apply(&changes(vec![dispatch(KEY)]), NOW).unwrap();
        store
            .apply(
                &changes(vec![result(KEY, EffectResolution::Unconfirmed)]),
                NOW,
            )
            .unwrap();
        // The core chose `unknown`; the store derives nothing.
        let unknown = terminal(KEY, EffectCertainty::Unknown);
        store.apply(&changes(vec![unknown]), LATER).unwrap();
        let (state, certainty, _) = effect_cols(&store, KEY);
        assert_eq!(
            (state.as_str(), certainty.as_deref()),
            ("failed", Some("unknown"))
        );
    }

    #[test]
    fn terminal_write_acknowledged_conflicts() {
        let (_dir, mut store) = seeded();
        store.apply(&changes(vec![dispatch(KEY)]), NOW).unwrap();
        store
            .apply(
                &changes(vec![result(
                    KEY,
                    EffectResolution::Acknowledged { receipt: None },
                )]),
                NOW,
            )
            .unwrap();
        let absent = terminal(KEY, EffectCertainty::Absent);
        let err = store.apply(&changes(vec![absent]), LATER).unwrap_err();
        assert_effect_conflict(&err);
        let (state, certainty, _) = effect_cols(&store, KEY);
        assert_eq!((state.as_str(), certainty), ("acknowledged", None));
    }

    #[test]
    fn failed_write_with_none_certainty_rejected() {
        // Repurposed (P4.1): a `failed` journal write without a certainty
        // and a `planned` journal write are unrepresentable in
        // `EffectWrite` now, so the surviving `MalformedWrite` refusal for
        // the journal is the plan-time insert of an effect that is not
        // `planned` — refused before the transaction, nothing moves.
        let (_dir, mut store) = seeded();
        for state in [
            EffectState::Dispatching,
            EffectState::Acknowledged,
            EffectState::Failed,
            EffectState::Unconfirmed,
        ] {
            let mut stray = effect("run:r-1:nudge:9", None, Some("r-1"));
            stray.state = state;
            stray.certainty = (state == EffectState::Failed).then_some(EffectCertainty::Absent);
            let malformed = transition(
                vec![StateChange::AckEvent(EventId("none".into()))],
                vec![],
                vec![stray],
            );
            let err = store.apply(&malformed, LATER).unwrap_err();
            assert!(
                matches!(err, ApplyError::MalformedWrite { .. }),
                "{state:?}: {err}"
            );
        }
        assert_eq!(count(&store, "effects"), 1, "nothing was inserted");
        assert_eq!(effect_cols(&store, KEY).0, "planned", "nothing moved");
    }

    #[test]
    fn failed_result_persists_its_cause() {
        let (_dir, mut store) = seeded();
        store.apply(&changes(vec![dispatch(KEY)]), NOW).unwrap();
        store
            .apply(
                &changes(vec![result(
                    KEY,
                    failed(EffectCertainty::Unknown, "timeout"),
                )]),
                LATER,
            )
            .unwrap();
        let (state, certainty, completed) = effect_cols(&store, KEY);
        assert_eq!(
            (state.as_str(), certainty.as_deref()),
            ("failed", Some("unknown"))
        );
        assert_eq!(completed.as_deref(), Some("2026-10-01T00:01:00.000Z"));
        assert_eq!(
            result_json_of(&store, KEY).as_deref(),
            Some(r#"{"error":"timeout"}"#),
            "the OQ-11 cause is the row's result_json"
        );
        let row = store.effect(&EffectKey(KEY.into())).unwrap().unwrap();
        assert_eq!(row.receipt, None, "a cause is not a receipt");
        assert_eq!(row.certainty, Some(EffectCertainty::Unknown));
    }

    #[test]
    fn pre_interactive_failure_persists_absent_and_cause() {
        let (_dir, mut store) = seeded();
        store.apply(&changes(vec![dispatch(KEY)]), NOW).unwrap();
        store
            .apply(
                &changes(vec![result(
                    KEY,
                    EffectResolution::PreInteractiveFailed {
                        cause: Some(FailureCause("agent_pane_busy".into())),
                    },
                )]),
                LATER,
            )
            .unwrap();
        let (state, certainty, _) = effect_cols(&store, KEY);
        assert_eq!(
            (state.as_str(), certainty.as_deref()),
            ("failed", Some("absent")),
            "a pre-interactive failure provably never ran (F15)"
        );
        assert_eq!(
            result_json_of(&store, KEY).as_deref(),
            Some(r#"{"error":"agent_pane_busy"}"#)
        );
        assert_eq!(
            store
                .effect(&EffectKey(KEY.into()))
                .unwrap()
                .unwrap()
                .receipt,
            None
        );
    }

    #[test]
    fn terminal_write_leaves_result_json_null() {
        let (_dir, mut store) = seeded();
        store.apply(&changes(vec![dispatch(KEY)]), NOW).unwrap();
        store
            .apply(
                &changes(vec![terminal(KEY, EffectCertainty::Unknown)]),
                LATER,
            )
            .unwrap();
        assert_eq!(
            result_json_of(&store, KEY),
            None,
            "no cause, no receipt (OQ-B)"
        );
        assert_eq!(
            effect_cols(&store, KEY),
            (
                "failed".into(),
                Some("unknown".into()),
                Some("2026-10-01T00:01:00.000Z".into())
            )
        );
    }

    #[test]
    fn judgments_receipt_writes_both_tables() {
        let (_dir, mut store) = seeded();
        let record = record();
        dispatch_and_commit(KEY, EffectReceipt::Judgments(record.clone()), &mut store);
        let stored = store
            .judgment_record(&JudgmentSetId("js-1".into()))
            .unwrap()
            .unwrap();
        assert_eq!(stored, record, "both tables hold the receipt");
        let stamps: (String, Option<String>) = store
            .conn()
            .query_row(
                "SELECT requested_at, answered_at FROM judgment_sets",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            stamps,
            (
                "2026-10-01T00:00:00.000Z".into(),
                Some("2026-10-01T00:01:00.000Z".into())
            ),
            "requested at dispatch, answered at the commit"
        );
        // Redelivery: the row write loses (the effect is acknowledged), so
        // nothing re-inserts.
        let again = changes(vec![result(
            KEY,
            EffectResolution::Acknowledged {
                receipt: Some(EffectReceipt::Judgments(record)),
            },
        )]);
        let err = store.apply(&again, LATER).unwrap_err();
        assert_effect_conflict(&err);
        assert_eq!(
            (count(&store, "judgment_sets"), count(&store, "judgments")),
            (1, 1)
        );
    }

    /// The receipt payload the `judgments` result commit persists.
    fn record() -> JudgmentRecord {
        let mut probabilities = BTreeMap::new();
        probabilities.insert("yes".to_owned(), Probability(0.75));
        JudgmentRecord {
            set: JudgmentSet {
                id: JudgmentSetId("js-1".into()),
                purpose: JudgmentPurpose::Acceptance,
                launch: None,
                run: Some(RunId("r-1".into())),
                versions: None,
                task_digest: Digest([0xab; 32]),
                handoff_digest: None,
                evidence_digest: None,
                model: "m".into(),
                question_version: QuestionVersion("q1".into()),
                policy_version: ConfigVersion("cfg-1".into()),
                outcome: JudgmentOutcome::Answered,
            },
            judgments: vec![Judgment {
                question: Question::HandoffMeetsItem { item: 0 },
                probabilities,
                answer: "yes".into(),
                threshold: Some(0.6),
            }],
        }
    }

    /// `dispatching` ← `planned`, then the `acknowledged` result commit
    /// carrying `receipt`.
    fn dispatch_and_commit(key: &str, receipt: EffectReceipt, store: &mut Store) {
        store.apply(&changes(vec![dispatch(key)]), NOW).unwrap();
        let commit = changes(vec![result(
            key,
            EffectResolution::Acknowledged {
                receipt: Some(receipt),
            },
        )]);
        store.apply(&commit, LATER).unwrap();
    }

    #[test]
    fn judgments_replay_under_a_second_effect_dedups() {
        // Idempotent redelivery: a second effect committing the same
        // receipt re-inserts neither the set nor its rows — the dedup
        // keys are `judgment_sets.set_id` and `judgments.(set_id, question)`.
        let (_dir, mut store) = seeded();
        dispatch_and_commit(KEY, EffectReceipt::Judgments(record()), &mut store);
        let second = "run:r-1:nudge:1";
        store
            .apply(
                &transition(vec![], vec![], vec![effect(second, None, Some("r-1"))]),
                NOW,
            )
            .unwrap();
        dispatch_and_commit(second, EffectReceipt::Judgments(record()), &mut store);
        assert_eq!(
            (count(&store, "judgment_sets"), count(&store, "judgments")),
            (1, 1),
            "the replayed receipt committed without duplicating its rows"
        );
        assert_eq!(
            store
                .judgment_record(&JudgmentSetId("js-1".into()))
                .unwrap()
                .unwrap(),
            record(),
            "the stored record is verbatim"
        );
    }

    #[test]
    fn planned_insert_carries_payload_digest() {
        let (_dir, mut store) = seeded();
        let mut undigested = effect("launch:l-1:evaluate", Some("l-1"), None);
        undigested.kind = EffectKind::JevEvaluate;
        undigested.payload_digest = None;
        store
            .apply(&transition(vec![], vec![], vec![undigested]), LATER)
            .unwrap();
        let digests: Vec<Option<String>> = store
            .conn()
            .prepare("SELECT payload_digest FROM effects ORDER BY effect_key")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(
            digests,
            vec![None, Some("5a".repeat(32))],
            "verbatim: NULL for jev_evaluate, the hex digest otherwise"
        );
    }

    #[test]
    fn abstained_finish_shape_lands_whole() {
        // OQ-13/OQ-12: `done` + the stranded eval write + `launch_answered`
        // in one transaction — or nothing.
        let (_dir, mut store) = store();
        let admit = transition(
            vec![
                binding(1),
                StateChange::RecordLaunch(launch("l-2", LaunchPhase::Evaluating, None)),
            ],
            vec![],
            vec![effect("launch:l-2:evaluate", Some("l-2"), None)],
        );
        store.apply(&admit, NOW).unwrap();
        let done = launch(
            "l-2",
            LaunchPhase::Done,
            Some(LaunchOutcome::Abstained {
                reason: AbstainReason::EvaluationFailed,
            }),
        );
        let finish = transition(
            vec![
                StateChange::RecordLaunch(done),
                terminal("launch:l-2:evaluate", EffectCertainty::Absent),
            ],
            vec![event("ev-a", "l-2")],
            vec![],
        );
        store.apply(&finish, LATER).unwrap();
        assert_eq!(effect_cols(&store, "launch:l-2:evaluate").0, "failed");
        assert!(store.ready_effects().unwrap().is_empty());
        assert_eq!(
            store.mailbox_unacked(&caller(1), None, 10).unwrap().len(),
            1
        );
        // Replaying the finish is a phase conflict and leaves everything as is.
        let err = store.apply(&finish, LATER).unwrap_err();
        assert!(matches!(err, ApplyError::PhaseConflict { .. }), "{err}");
        assert_eq!(count(&store, "mailbox"), 1);
    }
}

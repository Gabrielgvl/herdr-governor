//! P4.S3 — `store::apply` contract tests for the journal (`WriteEffect`)
//! arms: dispatch commit, restart marking, the OQ-13 terminal write, the
//! result commit with its `Judgments` receipt, the plan-time insert, and
//! the composed abstained finish.

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use governor_core::config::ConfigVersion;
    use governor_core::identity::{Digest, EffectKey, EventId, JudgmentSetId, RunId};
    use governor_core::lifecycle::{
        EffectCertainty, EffectKind, EffectReceipt, EffectState, EffectWrite, StateChange,
    };
    use governor_core::routing::{
        Judgment, JudgmentOutcome, JudgmentPurpose, JudgmentRecord, JudgmentSet, Probability,
        Question, QuestionVersion,
    };
    use governor_core::task::{AbstainReason, LaunchOutcome, LaunchPhase};
    use herdr_governor::store::{ApplyError, ConflictKind, Store};

    use crate::support::{
        LATER, NOW, binding, caller, changes, count, effect, event, launch, seeded, store,
        transition, write,
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
        // unconfirmed ← dispatching only: a planned row is a conflict.
        let err = store
            .apply(
                &changes(vec![write(KEY, EffectState::Unconfirmed, None)]),
                LATER,
            )
            .unwrap_err();
        assert_effect_conflict(&err);
        store
            .apply(
                &changes(vec![write(KEY, EffectState::Dispatching, None)]),
                LATER,
            )
            .unwrap();
        let dispatched = store.effect(&EffectKey(KEY.into())).unwrap().unwrap();
        assert_eq!(dispatched.dispatched_at, Some(LATER));
        assert_eq!(
            dispatched.payload_digest,
            Some(Digest([0x5a; 32])),
            "the plan-time digest is untouched"
        );
        // A second dispatch commit loses: planned is the only source.
        let second = store
            .apply(
                &changes(vec![write(KEY, EffectState::Dispatching, None)]),
                LATER,
            )
            .unwrap_err();
        assert_effect_conflict(&second);
        store
            .apply(
                &changes(vec![write(KEY, EffectState::Unconfirmed, None)]),
                LATER,
            )
            .unwrap();
        // unconfirmed never re-dispatches and never acknowledges.
        for state in [EffectState::Dispatching, EffectState::Acknowledged] {
            let third = store
                .apply(&changes(vec![write(KEY, state, None)]), LATER)
                .unwrap_err();
            assert_effect_conflict(&third);
        }
    }

    #[test]
    fn terminal_write_planned_persists_verbatim() {
        let (_dir, mut store) = seeded();
        let absent = write(KEY, EffectState::Failed, Some(EffectCertainty::Absent));
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
        store
            .apply(
                &changes(vec![write(KEY, EffectState::Dispatching, None)]),
                NOW,
            )
            .unwrap();
        store
            .apply(
                &changes(vec![write(KEY, EffectState::Unconfirmed, None)]),
                NOW,
            )
            .unwrap();
        // The core chose `unknown`; the store derives nothing.
        let unknown = write(KEY, EffectState::Failed, Some(EffectCertainty::Unknown));
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
        store
            .apply(
                &changes(vec![write(KEY, EffectState::Dispatching, None)]),
                NOW,
            )
            .unwrap();
        store
            .apply(
                &changes(vec![write(KEY, EffectState::Acknowledged, None)]),
                NOW,
            )
            .unwrap();
        let absent = write(KEY, EffectState::Failed, Some(EffectCertainty::Absent));
        let err = store.apply(&changes(vec![absent]), LATER).unwrap_err();
        assert_effect_conflict(&err);
        let (state, certainty, _) = effect_cols(&store, KEY);
        assert_eq!((state.as_str(), certainty), ("acknowledged", None));
    }

    #[test]
    fn failed_write_with_none_certainty_rejected() {
        let (_dir, mut store) = seeded();
        let malformed = changes(vec![
            StateChange::AckEvent(EventId("none".into())),
            write(KEY, EffectState::Failed, None),
        ]);
        let err = store.apply(&malformed, LATER).unwrap_err();
        assert!(matches!(err, ApplyError::MalformedWrite { .. }), "{err}");
        let second = store
            .apply(
                &changes(vec![write(KEY, EffectState::Planned, None)]),
                LATER,
            )
            .unwrap_err();
        assert!(
            matches!(second, ApplyError::MalformedWrite { .. }),
            "{second}"
        );
        assert_eq!(effect_cols(&store, KEY).0, "planned", "nothing moved");
    }

    #[test]
    fn judgments_receipt_writes_both_tables() {
        let (_dir, mut store) = seeded();
        let mut probabilities = BTreeMap::new();
        probabilities.insert("yes".to_owned(), Probability(0.75));
        let record = JudgmentRecord {
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
        };
        store
            .apply(
                &changes(vec![write(KEY, EffectState::Dispatching, None)]),
                NOW,
            )
            .unwrap();
        let commit = changes(vec![StateChange::WriteEffect(EffectWrite {
            key: EffectKey(KEY.into()),
            state: EffectState::Acknowledged,
            certainty: None,
            receipt: Some(EffectReceipt::Judgments(record.clone())),
        })]);
        store.apply(&commit, LATER).unwrap();
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
        let err = store.apply(&commit, LATER).unwrap_err();
        assert_effect_conflict(&err);
        assert_eq!(
            (count(&store, "judgment_sets"), count(&store, "judgments")),
            (1, 1)
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
                write(
                    "launch:l-2:evaluate",
                    EffectState::Failed,
                    Some(EffectCertainty::Absent),
                ),
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

//! P5.B1 §4.2 — the governor-refused commit shape: `WriteEffect::Dispatch`
//! and `WriteEffect::Result` in ONE apply against a `planned` row land
//! `planned → dispatching → failed` atomically. The Dispatch leg still
//! stamps `dispatched_at`, so a post-crash scan distinguishes "refused
//! after the commit point" (the dispatching transition ran) from "never
//! dispatched", and the Result leg journals the refusal's cause and
//! certainty.
//!
//! The legs' order is load-bearing: a `Result` write against a `planned`
//! row is a conflict, so the dispatch must precede it inside the same
//! transaction.

#[cfg(test)]
mod tests {
    use governor_core::identity::EffectKey;
    use governor_core::lifecycle::{
        EffectCertainty, EffectResolution, EffectState, FailureCause, Transition,
    };
    use herdr_governor::store::{ApplyError, ConflictKind, Store};

    use crate::support::{LATER, NOW, changes, dispatch, effect, result, seeded, transition};

    const KEY: &str = "run:r-1:prompt:task";

    fn refusal(key: &str, cause: &str) -> Transition {
        changes(vec![
            dispatch(key),
            result(
                key,
                EffectResolution::Failed {
                    certainty: EffectCertainty::Absent,
                    cause: Some(FailureCause(cause.into())),
                },
            ),
        ])
    }

    fn cols(store: &Store, key: &str) -> (String, Option<String>, Option<String>, Option<String>) {
        store
            .conn()
            .query_row(
                "SELECT state, certainty, dispatched_at, completed_at \
                 FROM effects WHERE effect_key = ?1",
                [key],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
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

    #[test]
    fn refusal_commits_dispatch_and_failure_in_one_apply() {
        let (_dir, mut store) = seeded();
        store
            .apply(
                &transition(vec![], vec![], vec![effect(KEY, None, Some("r-1"))]),
                NOW,
            )
            .unwrap();

        store
            .apply(&refusal(KEY, "qualification_lapsed"), LATER)
            .unwrap();

        let row = store.effect(&EffectKey(KEY.into())).unwrap().unwrap();
        assert_eq!(row.state, EffectState::Failed);
        assert_eq!(row.certainty, Some(EffectCertainty::Absent));
        assert_eq!(
            cols(&store, KEY),
            (
                "failed".to_owned(),
                Some("absent".to_owned()),
                Some("2026-10-01T00:01:00.000Z".to_owned()),
                Some("2026-10-01T00:01:00.000Z".to_owned())
            ),
            "both legs stamped: the dispatch point and the completion"
        );
        let json = result_json_of(&store, KEY).expect("the refusal journals a cause");
        assert!(
            json.contains("qualification_lapsed"),
            "the OQ-11 cause is on the row: {json}"
        );
        assert!(row.receipt.is_none(), "a refusal carries no receipt");
    }

    #[test]
    fn refusal_leg_order_is_load_bearing() {
        let (_dir, mut store) = seeded();
        store
            .apply(
                &transition(vec![], vec![], vec![effect(KEY, None, Some("r-1"))]),
                NOW,
            )
            .unwrap();

        // `[Result, Dispatch]` on a `planned` row: the result leg names
        // `dispatching`, matches nothing, and the whole apply rolls back —
        // the refusal can never write `failed` without `dispatching` first.
        let reversed = changes(vec![
            result(
                KEY,
                EffectResolution::Failed {
                    certainty: EffectCertainty::Absent,
                    cause: Some(FailureCause("reversed".into())),
                },
            ),
            dispatch(KEY),
        ]);
        let err = store.apply(&reversed, LATER).unwrap_err();
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
        assert_eq!(
            cols(&store, KEY),
            ("planned".to_owned(), None, None, None),
            "the losing apply committed nothing — the row is untouched"
        );
    }

    #[test]
    fn refusal_replay_conflicts_and_changes_nothing() {
        let (_dir, mut store) = seeded();
        store
            .apply(
                &transition(vec![], vec![], vec![effect(KEY, None, Some("r-1"))]),
                NOW,
            )
            .unwrap();
        store
            .apply(&refusal(KEY, "qualification_lapsed"), LATER)
            .unwrap();

        // The composed write is not idempotent: a replayed commit attempt
        // (a retried arm, a stale runner) conflicts on the Dispatch leg —
        // `dispatching` is not `planned` — and the row's truth stands.
        let err = store
            .apply(&refusal(KEY, "qualification_lapsed"), LATER)
            .unwrap_err();
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
        assert_eq!(cols(&store, KEY).0, "failed");
        assert_eq!(
            result_json_of(&store, KEY),
            Some("{\"error\":\"qualification_lapsed\"}".to_owned()),
            "the first refusal's cause is the one that landed"
        );
    }
}

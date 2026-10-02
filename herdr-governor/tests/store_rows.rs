//! P4.S2 read-API tests that need lifecycle fixture rows. They live outside
//! `src/` because I10 confines INSERT/UPDATE/DELETE on the lifecycle tables
//! to `store/transitions/` or `tests/`. The fixtures spell the persisted
//! shapes by hand (RFC3339-millis times, lowercase-hex digests, the
//! hand-mapped JSON), so they also pin what the codecs must keep accepting.

#[cfg(test)]
mod tests {
    use governor_core::config::{Capability, OperatingPointId};
    use governor_core::delivery::{MailboxEventKind, MailboxSubject};
    use governor_core::identity::{
        AgentKind, CallerKey, Digest, EffectKey, EventId, IdempotencyKey, LaunchId, NativeSession,
        ProjectRoot, RelayInstanceId, RunId, Timestamp,
    };
    use governor_core::lifecycle::{EffectState, State};
    use governor_core::recovery::RecoveryStatus;
    use governor_core::task::LaunchPhase;
    use herdr_governor::store::Store;
    use tempfile::{TempDir, tempdir};

    const T0: &str = "2026-10-01T00:00:00.000Z";
    const T1: &str = "2026-10-01T00:01:30.000Z";
    const HEX: &str = "abababababababababababababababababababababababababababababababab";
    const TASK: &str = r#"{"objective":"o","scope":"s","done_when":["d"],"constraints":[],"tier":null,"recovery_of":null,"label":null,"cwd":null}"#;
    const DECISION: &str = r#"{"judged_tier":"mid","requested_tier":null,"policy_cap":null,"policy_floor":null,"caller_uplift":null,"recovery_minimum":null,"exploration":{"assigned":true,"executed":false},"start_tier":"mid","candidates":[],"config_version":"cfg-1"}"#;

    fn caller() -> CallerKey {
        CallerKey {
            agent_kind: AgentKind("kind-a".into()),
            native_session: NativeSession("sess-1".into()),
        }
    }

    /// A store with one caller, two launches (`l-open` routed, `l-done`
    /// done), one settled run on `l-open`, and one planned effect per launch.
    fn fixture() -> (TempDir, Store) {
        let dir = tempdir().unwrap();
        let store = Store::open(&dir.path().join("store.db")).unwrap();
        store
            .conn()
            .execute_batch(&format!(
                "INSERT INTO callers (caller_id, agent_kind, native_session, first_seen_at)
                 VALUES (1, 'kind-a', 'sess-1', '{T0}'), (2, 'kind-b', 'sess-2', '{T0}');
                 INSERT INTO relay_bindings VALUES ('{HEX}', 1, 'pane-1', '{T0}');
                 INSERT INTO launches (launch_id, caller_id, project_root, idempotency_key,
                     digest_version, task_digest, task_json, phase, decision_json,
                     config_version, created_at, updated_at)
                 VALUES ('l-open', 1, '/p', 'k-1', 1, '{HEX}', '{TASK}', 'routed',
                         '{DECISION}', 'cfg-1', '{T0}', '{T0}');
                 INSERT INTO launches (launch_id, caller_id, project_root, idempotency_key,
                     digest_version, task_digest, task_json, phase, outcome, result_json,
                     created_at, updated_at)
                 VALUES ('l-done', 1, '/p', 'k-2', 1, '{HEX}', '{TASK}', 'done', 'rejected',
                         '{{\"outcome\":\"rejected\"}}', '{T0}', '{T0}');
                 INSERT INTO runs (run_id, launch_id, owner_caller_id, state, child_name, cwd,
                     max_age_deadline, settlement, settled_at, created_at, updated_at)
                 VALUES ('r-1', 'l-open', 2, 'settled', 'w1:r-1', '/p', '{T1}', 'accepted',
                         '{T1}', '{T0}', '{T1}');
                 INSERT INTO effects (effect_id, effect_key, kind, subject_launch_id, state,
                     planned_at)
                 VALUES ('e-open', 'launch:l-open:evaluate', 'jev_evaluate', 'l-open',
                         'planned', '{T0}'),
                        ('e-done', 'launch:l-done:evaluate', 'jev_evaluate', 'l-done',
                         'planned', '{T0}');
                 INSERT INTO effects (effect_id, effect_key, kind, subject_run_id, state,
                     planned_at)
                 VALUES ('e-run', 'run:r-1:prompt:task', 'prompt', 'r-1', 'planned', '{T1}');
                 INSERT INTO mailbox (event_id, dedup_key, run_id, kind, body_json, created_at)
                 VALUES ('ev-1', 'run:r-1:settled', 'r-1', 'settled', '{{}}', '{T0}'),
                        ('ev-2', 'run:r-1:stalled:1', 'r-1', 'stalled', '{{}}', '{T1}');
                 INSERT INTO mailbox (event_id, dedup_key, launch_id, kind, body_json,
                     acked_at, created_at)
                 VALUES ('ev-3', 'launch:l-done:launch_answered', 'l-done', 'launch_answered',
                         '{{}}', '{T1}', '{T0}');
                 INSERT INTO handoffs VALUES ('r-1', 0, '{HEX}', '/frozen', '{T1}');
                 INSERT INTO judgment_sets (set_id, purpose, run_id, run_version,
                     work_generation, evidence_generation, task_digest, handoff_digest, model,
                     question_version, policy_version, outcome, requested_at)
                 VALUES ('js-1', 'acceptance', 'r-1', 1, 0, 0, '{HEX}', '{HEX}', 'm', 'q1',
                         'cfg-1', 'answered', '{T1}');
                 INSERT INTO recoveries (predecessor_run_id, origin, state, expires_at,
                     created_at, updated_at)
                 VALUES ('r-1', 'provider_limit', 'pending', '{T1}', '{T1}', '{T1}');
                 INSERT INTO cooldowns VALUES ('prov', '{T1}', 'provider_limited', 'r-1', '{T1}');
                 INSERT INTO qualifications VALUES ('op-1', '{HEX}', 'prompt_ack', 1, '{{}}', '{T0}');"
            ))
            .unwrap();
        (dir, store)
    }

    #[test]
    fn ready_effects_excludes_done_launches() {
        // F10 read-side guard: a `planned` effect on a `done` launch is never
        // handed to a dispatcher; run-subject and open-launch effects are.
        let (_dir, store) = fixture();
        let keys: Vec<String> = store
            .ready_effects()
            .unwrap()
            .into_iter()
            .map(|effect| effect.key.0)
            .collect();
        assert_eq!(
            keys,
            ["launch:l-open:evaluate", "run:r-1:prompt:task"],
            "the done launch's effect must be excluded, plan order kept"
        );
        assert_eq!(
            store.effects_in_state(EffectState::Planned).unwrap().len(),
            3,
            "the state query itself still sees every planned row"
        );
    }

    #[test]
    fn decision_json_carries_exploration_at_view_path() {
        // The pinned shape resolves at the `outcomes` view's `json_extract`
        // path and decodes through the read API to the same flags.
        let (_dir, store) = fixture();
        let (assigned, executed): (i64, i64) = store
            .conn()
            .query_row(
                "SELECT explored_assigned, explored_executed FROM outcomes WHERE run_id = 'r-1'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!((assigned, executed), (1, 0), "view must read the flags");
        let launch = store.launch(&LaunchId("l-open".into())).unwrap().unwrap();
        let exploration = launch.decision.unwrap().exploration;
        assert!(
            exploration.assigned && !exploration.executed,
            "decode must agree"
        );
    }

    #[test]
    fn time_columns_survive_julianday() {
        let (_dir, store) = fixture();
        let seconds: Option<f64> = store
            .conn()
            .query_row(
                "SELECT seconds_to_settle FROM outcomes WHERE run_id = 'r-1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let elapsed = seconds.expect("julianday must parse the stored time columns");
        assert!(
            (elapsed - 90.0).abs() < 1e-3,
            "90s to settle, got {elapsed}"
        );
    }

    #[test]
    fn launch_and_run_reads_resolve_callers_through_the_join() {
        let (_dir, store) = fixture();
        let by_key = store
            .launch_by_idempotency(
                &caller(),
                &ProjectRoot("/p".into()),
                &IdempotencyKey("k-1".into()),
            )
            .unwrap()
            .unwrap();
        assert_eq!(by_key.caller, caller());
        assert_eq!(by_key.phase, LaunchPhase::Routed);
        assert_eq!(store.launches_in_phase(LaunchPhase::Done).unwrap().len(), 1);
        let other = CallerKey {
            native_session: NativeSession("nobody".into()),
            ..caller()
        };
        assert!(
            store
                .launch_by_idempotency(
                    &other,
                    &ProjectRoot("/p".into()),
                    &IdempotencyKey("k-1".into())
                )
                .unwrap()
                .is_none(),
            "an unknown caller owns no launches"
        );
        let run = store.run(&RunId("r-1".into())).unwrap().unwrap();
        assert_eq!(
            run.owner.native_session.0, "sess-2",
            "owner is the joined caller"
        );
        assert_eq!(run.state, State::Settled);
        assert_eq!(
            store.run_by_launch(&LaunchId("l-open".into())).unwrap(),
            Some(run)
        );
        assert!(store.unsettled_runs().unwrap().is_empty());
        assert_eq!(store.journal(&RunId("r-1".into())).unwrap().len(), 1);
        assert!(
            store
                .effect(&EffectKey("launch:l-done:evaluate".into()))
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn mailbox_pages_by_derived_destination_and_cursor() {
        let (_dir, store) = fixture();
        let owner = CallerKey {
            agent_kind: AgentKind("kind-b".into()),
            native_session: NativeSession("sess-2".into()),
        };
        let page = store.mailbox_unacked(&owner, None, 10).unwrap();
        let ids: Vec<&str> = page.iter().map(|event| event.id.0.as_str()).collect();
        assert_eq!(
            ids,
            ["ev-1", "ev-2"],
            "run events go to the run's owner, in order"
        );
        assert_eq!(page[1].kind, MailboxEventKind::Stalled);
        assert_eq!(page[0].subject, MailboxSubject::Run(RunId("r-1".into())));
        let after = store
            .mailbox_unacked(&owner, Some(&EventId("ev-1".into())), 10)
            .unwrap();
        assert_eq!(after.len(), 1, "the page starts after the cursor");
        assert_eq!(after[0].id.0, "ev-2");
        assert_eq!(
            store.mailbox_unacked(&owner, None, 1).unwrap().len(),
            1,
            "limit"
        );
        assert!(
            store
                .mailbox_unacked(&caller(), None, 10)
                .unwrap()
                .is_empty(),
            "the launch-only event is acked; the launch caller has nothing unacked"
        );
        let acked = store
            .mailbox_event(&EventId("ev-3".into()))
            .unwrap()
            .unwrap();
        assert_eq!(
            acked.subject,
            MailboxSubject::Launch(LaunchId("l-done".into()))
        );
    }

    #[test]
    fn remaining_reads_return_typed_rows() {
        let (_dir, store) = fixture();
        let handoffs = store.handoffs(&RunId("r-1".into())).unwrap();
        assert_eq!(handoffs.len(), 1);
        assert!(
            handoffs[0].assessed,
            "an answered acceptance set marks it assessed"
        );
        assert_eq!(handoffs[0].frozen_at, Timestamp(1_790_812_890_000));
        assert!(store.outbox(&RunId("r-1".into())).unwrap().is_empty());
        assert!(store.outbox_pending().unwrap().is_empty());
        let cooldowns = store.cooldowns().unwrap();
        assert_eq!(cooldowns[0].source_run, Some(RunId("r-1".into())));
        let pending = store.recoveries_by_state(RecoveryStatus::Pending).unwrap();
        assert_eq!(pending[0].predecessor, RunId("r-1".into()));
        assert!(
            store
                .recoveries_by_state(RecoveryStatus::Failed)
                .unwrap()
                .is_empty()
        );
        let qualification = store
            .qualification(
                &OperatingPointId("op-1".into()),
                Digest([0xab; 32]),
                &Capability(Capability::PROMPT_ACK.into()),
            )
            .unwrap()
            .unwrap();
        assert!(qualification.passed);
        assert_eq!(
            store.caller(&caller()).unwrap(),
            Some(Timestamp(1_790_812_800_000))
        );
        let binding = store
            .relay_binding(&RelayInstanceId(HEX.into()))
            .unwrap()
            .unwrap();
        assert_eq!(binding.caller, caller());
        assert_eq!(binding.pane_at_bind.0, "pane-1");
        assert!(
            store
                .relay_binding(&RelayInstanceId("none".into()))
                .unwrap()
                .is_none()
        );
    }
}

//! The named crash matrix: `crash_<scenario>_<k>` kills the probe after
//! statement `k` of the scenario's transaction (`0` = before `BEGIN`) and
//! asserts the reopened store is exactly the seed state and that a replay
//! then commits; `crash_<scenario>_after_commit` kills right after
//! `COMMIT` and asserts the whole transaction survived. Every scenario ×
//! boundary is listed by name so coverage is visible in the inventory;
//! `matrix.rs` pins the counts these names assume.

#[cfg(test)]
mod tests {
    use governor_core::config::Policy;
    use governor_core::identity::RunId;
    use governor_core::lifecycle::{EffectState, Event, VersionTriple, Versioned, transition};
    use herdr_governor::store::Store;

    use crate::support::crash::{NOW, assert_after_commit, assert_pre_commit, probe, seeded};

    macro_rules! boundary {
        ($name:ident, $scenario:literal, after_commit) => {
            #[test]
            fn $name() {
                assert_after_commit($scenario);
            }
        };
        ($name:ident, $scenario:literal, $k:literal) => {
            #[test]
            fn $name() {
                assert_pre_commit($scenario, $k);
            }
        };
    }

    // bind_caller: 2 statement boundaries
    boundary!(crash_bind_caller_0, "bind_caller", 0);
    boundary!(crash_bind_caller_1, "bind_caller", 1);
    boundary!(crash_bind_caller_2, "bind_caller", 2);
    boundary!(crash_bind_caller_after_commit, "bind_caller", after_commit);

    // admit_launch: 2 statement boundaries
    boundary!(crash_admit_launch_0, "admit_launch", 0);
    boundary!(crash_admit_launch_1, "admit_launch", 1);
    boundary!(crash_admit_launch_2, "admit_launch", 2);
    boundary!(
        crash_admit_launch_after_commit,
        "admit_launch",
        after_commit
    );

    // route: 2 statement boundaries
    boundary!(crash_route_0, "route", 0);
    boundary!(crash_route_1, "route", 1);
    boundary!(crash_route_2, "route", 2);
    boundary!(crash_route_after_commit, "route", after_commit);

    // plan_effect: 1 statement boundaries
    boundary!(crash_plan_effect_0, "plan_effect", 0);
    boundary!(crash_plan_effect_1, "plan_effect", 1);
    boundary!(crash_plan_effect_after_commit, "plan_effect", after_commit);

    // dispatch_effect: 1 statement boundaries
    boundary!(crash_dispatch_effect_0, "dispatch_effect", 0);
    boundary!(crash_dispatch_effect_1, "dispatch_effect", 1);
    boundary!(
        crash_dispatch_effect_after_commit,
        "dispatch_effect",
        after_commit
    );

    // effect_result: 2 statement boundaries
    boundary!(crash_effect_result_0, "effect_result", 0);
    boundary!(crash_effect_result_1, "effect_result", 1);
    boundary!(crash_effect_result_2, "effect_result", 2);
    boundary!(
        crash_effect_result_after_commit,
        "effect_result",
        after_commit
    );

    // enqueue_follow_up: 1 statement boundaries
    boundary!(crash_enqueue_follow_up_0, "enqueue_follow_up", 0);
    boundary!(crash_enqueue_follow_up_1, "enqueue_follow_up", 1);
    boundary!(
        crash_enqueue_follow_up_after_commit,
        "enqueue_follow_up",
        after_commit
    );

    // settle: 7 statement boundaries
    boundary!(crash_settle_0, "settle", 0);
    boundary!(crash_settle_1, "settle", 1);
    boundary!(crash_settle_2, "settle", 2);
    boundary!(crash_settle_3, "settle", 3);
    boundary!(crash_settle_4, "settle", 4);
    boundary!(crash_settle_5, "settle", 5);
    boundary!(crash_settle_6, "settle", 6);
    boundary!(crash_settle_7, "settle", 7);
    boundary!(crash_settle_after_commit, "settle", after_commit);

    // handover: 1 statement boundaries
    boundary!(crash_handover_0, "handover", 0);
    boundary!(crash_handover_1, "handover", 1);
    boundary!(crash_handover_after_commit, "handover", after_commit);

    // recovery_dispatch: 3 statement boundaries
    boundary!(crash_recovery_dispatch_0, "recovery_dispatch", 0);
    boundary!(crash_recovery_dispatch_1, "recovery_dispatch", 1);
    boundary!(crash_recovery_dispatch_2, "recovery_dispatch", 2);
    boundary!(crash_recovery_dispatch_3, "recovery_dispatch", 3);
    boundary!(
        crash_recovery_dispatch_after_commit,
        "recovery_dispatch",
        after_commit
    );

    // freeze_handoff: 2 statement boundaries
    boundary!(crash_freeze_handoff_0, "freeze_handoff", 0);
    boundary!(crash_freeze_handoff_1, "freeze_handoff", 1);
    boundary!(crash_freeze_handoff_2, "freeze_handoff", 2);
    boundary!(
        crash_freeze_handoff_after_commit,
        "freeze_handoff",
        after_commit
    );

    // record_evidence: 1 statement boundaries
    boundary!(crash_record_evidence_0, "record_evidence", 0);
    boundary!(crash_record_evidence_1, "record_evidence", 1);
    boundary!(
        crash_record_evidence_after_commit,
        "record_evidence",
        after_commit
    );

    // launch_begin: 3 statement boundaries
    boundary!(crash_launch_begin_0, "launch_begin", 0);
    boundary!(crash_launch_begin_1, "launch_begin", 1);
    boundary!(crash_launch_begin_2, "launch_begin", 2);
    boundary!(crash_launch_begin_3, "launch_begin", 3);
    boundary!(
        crash_launch_begin_after_commit,
        "launch_begin",
        after_commit
    );

    // launch_finish_done: 4 statement boundaries
    boundary!(crash_launch_finish_done_0, "launch_finish_done", 0);
    boundary!(crash_launch_finish_done_1, "launch_finish_done", 1);
    boundary!(crash_launch_finish_done_2, "launch_finish_done", 2);
    boundary!(crash_launch_finish_done_3, "launch_finish_done", 3);
    boundary!(crash_launch_finish_done_4, "launch_finish_done", 4);
    boundary!(
        crash_launch_finish_done_after_commit,
        "launch_finish_done",
        after_commit
    );

    // launch_finish_routed_abandon: 5 statement boundaries
    boundary!(
        crash_launch_finish_routed_abandon_0,
        "launch_finish_routed_abandon",
        0
    );
    boundary!(
        crash_launch_finish_routed_abandon_1,
        "launch_finish_routed_abandon",
        1
    );
    boundary!(
        crash_launch_finish_routed_abandon_2,
        "launch_finish_routed_abandon",
        2
    );
    boundary!(
        crash_launch_finish_routed_abandon_3,
        "launch_finish_routed_abandon",
        3
    );
    boundary!(
        crash_launch_finish_routed_abandon_4,
        "launch_finish_routed_abandon",
        4
    );
    boundary!(
        crash_launch_finish_routed_abandon_5,
        "launch_finish_routed_abandon",
        5
    );
    boundary!(
        crash_launch_finish_routed_abandon_after_commit,
        "launch_finish_routed_abandon",
        after_commit
    );

    // launch_finish_abstain: 3 statement boundaries
    boundary!(crash_launch_finish_abstain_0, "launch_finish_abstain", 0);
    boundary!(crash_launch_finish_abstain_1, "launch_finish_abstain", 1);
    boundary!(crash_launch_finish_abstain_2, "launch_finish_abstain", 2);
    boundary!(crash_launch_finish_abstain_3, "launch_finish_abstain", 3);
    boundary!(
        crash_launch_finish_abstain_after_commit,
        "launch_finish_abstain",
        after_commit
    );

    /// F28 convergence: a `dispatching` effect survives a post-commit kill
    /// *as* `dispatching` (the store never claims more durability than it
    /// has), and the restart transition the daemon drives on reopen moves
    /// it to `unconfirmed`; the Run's persisted absolute deadlines are
    /// unchanged (F22).
    #[test]
    fn crash_dispatch_effect_after_commit_f28_restart_converges() {
        let (_dir, db) = seeded("dispatch_effect");
        let out = probe("dispatch_effect", &db, &["abort-after-commit"], &[]);
        assert!(!out.status.success(), "the probe must have aborted");
        let mut store = Store::open(&db).expect("reopen");
        let run_id = RunId("r-1".into());
        let run = store.run(&run_id).expect("read run").expect("run r-1");
        let journal = store.journal(&run_id).expect("read journal");
        let dispatching: Vec<_> = journal
            .iter()
            .filter(|e| e.state == EffectState::Dispatching)
            .collect();
        assert_eq!(
            dispatching.len(),
            1,
            "the dispatching effect survives as dispatching"
        );
        let restart = Versioned {
            requested_against: VersionTriple {
                version: run.version,
                work_generation: run.work_generation,
                evidence_generation: run.evidence_generation,
            },
            value: Event::Restart,
        };
        let policy = Policy {
            tiers: vec![],
            no_change_cap: None,
            security_floor: None,
            broad_change_floor: None,
            provider_limit_threshold: 0.7,
            exploration_rate: 0.05,
            recovery_expiry: core::time::Duration::from_hours(24),
            cooldown: core::time::Duration::from_hours(1),
            max_age: core::time::Duration::from_hours(24),
            repair_window: core::time::Duration::from_mins(15),
            judgment_window: core::time::Duration::from_mins(30),
            idle_window: core::time::Duration::from_mins(15),
        };
        let marking = transition(&run, &restart, NOW, &policy, (None, &journal, &[]), "/x");
        assert!(
            !marking.state_changes.is_empty(),
            "restart marks the dispatching effect"
        );
        store.apply(&marking, NOW).expect("apply restart marking");
        let after = store.journal(&run_id).expect("re-read journal");
        assert!(
            after.iter().all(|e| e.state != EffectState::Dispatching),
            "no effect is still dispatching after restart"
        );
        assert_eq!(
            after
                .iter()
                .filter(|e| e.state == EffectState::Unconfirmed)
                .count(),
            1,
            "the interrupted dispatch is unconfirmed, never re-dispatched"
        );
        let run_after = store.run(&run_id).expect("re-read run").expect("run r-1");
        assert_eq!(
            run_after.max_age_deadline, run.max_age_deadline,
            "F22: persisted absolute deadlines are unchanged by restart"
        );
    }
}

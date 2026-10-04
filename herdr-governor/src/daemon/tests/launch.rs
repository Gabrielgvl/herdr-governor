//! `launch` — §4.5/F1's `decided` apply over a real tempdir store: a
//! `Conflict{Run}` (a taken `run_id`, or a taken `gov-<runId[0..8]>`
//! child name) re-mints the v4 id on the next attempt instead of
//! replaying the colliding one, bounded at three attempts; and §4.4's
//! ready set once the Launch is `done` (the start-ack compose makes a
//! launched Launch `done` before its Run's task prompt dispatches).

use std::io;

use governor_core::config::{ConfigVersion, OperatingPointId, Provider, Tier};
use governor_core::identity::{AgentKind, LaunchId, RunId};
use governor_core::lifecycle::{CreatedTopology, EffectCertainty, EffectKind, StateChange};
use governor_core::routing::{Candidate, Decision, Exploration};
use governor_core::task::{LaunchOutcome, LaunchPhase};

use crate::daemon::coordinator::apply::ApplyOutcome;
use crate::daemon::launch::apply_decided;
use crate::store::Store;

use super::coordinator::{
    NOW, bind_caller, changes, effect, launch_row, plan, policy, run_row_on, seed_run, store_in,
};

/// The Run already holding both keys: `run_id` TAKEN, child name
/// `gov-aaaaaaaa` (the first eight hex of TAKEN — `mint_agent_name`'s
/// spelling).
const TAKEN: &str = "aaaaaaaa-0000-4000-8000-000000000001";
/// A different id whose first eight hex collide with TAKEN's — the
/// child-name half of `Conflict{Run}`.
const PREFIX_TWIN: &str = "aaaaaaaa-1111-4111-8111-111111111111";
/// A collision-free id.
const FRESH: &str = "bbbbbbbb-2222-4222-8222-222222222222";

fn decision() -> Decision {
    Decision {
        judged_tier: Tier("standard".into()),
        requested_tier: None,
        policy_cap: None,
        policy_floor: None,
        caller_uplift: None,
        recovery_minimum: None,
        exploration: Exploration {
            assigned: false,
            executed: false,
        },
        start_tier: Tier("standard".into()),
        candidates: vec![Candidate {
            operating_point: OperatingPointId("op-a".into()),
            provider: Provider("vendor-a".into()),
            tier: Tier("standard".into()),
            harness: AgentKind("kind-a".into()),
            args: vec!["--a".into()],
        }],
        config_version: ConfigVersion("v".into()),
    }
}

/// A store with TAKEN reserved and `evaluating` Launches `l-new` and
/// `l-bound` awaiting their decision.
fn seeded(dir: &std::path::Path) -> Store {
    let mut store = store_in(dir);
    bind_caller(&mut store);
    let mut taken = run_row_on(TAKEN, "l-taken");
    taken.child_name = "gov-aaaaaaaa".into();
    seed_run(&mut store, &taken);
    for id in ["l-new", "l-bound"] {
        store
            .apply(
                &changes(vec![StateChange::RecordLaunch(launch_row(
                    id,
                    LaunchPhase::Evaluating,
                ))]),
                NOW,
            )
            .expect("record evaluating");
    }
    store
}

/// `apply_decided` with a scripted mint; returns the outcome and every
/// id the mint handed out, in order.
fn decide_with(store: &mut Store, launch: &str, script: &[&str]) -> (ApplyOutcome, Vec<String>) {
    let mut ids = script.iter();
    let mut minted = Vec::new();
    let outcome = apply_decided(
        store,
        (NOW, &policy()),
        &LaunchId(launch.into()),
        &decision(),
        ("/p", None),
        || {
            let id = ids
                .next()
                .ok_or_else(|| io::Error::other("script exhausted"))?;
            minted.push((*id).to_owned());
            Ok(RunId((*id).to_owned()))
        },
    )
    .expect("the apply never hard-errors");
    (outcome, minted)
}

/// F1 — every attempt reserves under a fresh mint: a taken `run_id` and
/// then a taken child-name prefix each lose the CAS, the third id lands
/// (Launch `routed`, the Run under the fresh id and its own name); a
/// mint that keeps colliding stops at the three-attempt bound, writing
/// nothing — the Launch stays `evaluating` for the converge row.
#[test]
fn reserve_conflict_remints_run_id_up_to_three_times() {
    let tmp = tempfile::tempdir().expect("tmp");
    let mut store = seeded(tmp.path());

    let (outcome, minted) = decide_with(&mut store, "l-new", &[TAKEN, PREFIX_TWIN, FRESH]);
    assert_eq!(outcome, ApplyOutcome::Applied { attempts: 3 });
    assert_eq!(minted, [TAKEN, PREFIX_TWIN, FRESH], "one mint per attempt");
    let run = store
        .run_by_launch(&LaunchId("l-new".into()))
        .expect("read")
        .expect("the third attempt reserved");
    assert_eq!(run.id.0, FRESH, "the colliding ids were never replayed");
    assert_eq!(run.child_name, "gov-bbbbbbbb", "the fresh id's own name");
    assert_eq!(
        store
            .launch(&LaunchId("l-new".into()))
            .expect("read")
            .map(|launch| launch.phase),
        Some(LaunchPhase::Routed),
        "the decision persisted with the reservation"
    );

    let (bounded, tries) = decide_with(&mut store, "l-bound", &[TAKEN, TAKEN, TAKEN, FRESH]);
    assert_eq!(bounded, ApplyOutcome::Dropped { attempts: 3 });
    assert_eq!(tries.len(), 3, "the fourth id is never minted");
    assert!(
        store
            .run_by_launch(&LaunchId("l-bound".into()))
            .expect("read")
            .is_none(),
        "a dropped decide reserves nothing"
    );
    assert_eq!(
        store
            .launch(&LaunchId("l-bound".into()))
            .expect("read")
            .map(|launch| launch.phase),
        Some(LaunchPhase::Evaluating),
        "the Launch stays evaluating for the converge row"
    );
}

/// §4.4 — a Run-bound effect carries its Launch as a subject too
/// (`planned_effect`), and a launched Launch is `done` for the Run's
/// whole supervised life: `ready_effects` gates a Run-bound effect by
/// its Run alone, while a launch-bound effect of a `done` Launch stays
/// out.
#[test]
fn ready_effects_passes_run_effects_of_a_done_launch() {
    let tmp = tempfile::tempdir().expect("tmp");
    let mut store = store_in(tmp.path());
    bind_caller(&mut store);
    seed_run(&mut store, &run_row_on("r-1", "l-1"));
    let mut done = launch_row("l-1", LaunchPhase::Done);
    done.outcome = Some(LaunchOutcome::Failed {
        certainty: EffectCertainty::Absent,
        run: Some(RunId("r-1".into())),
        created_topology: CreatedTopology {
            tab: None,
            panes: Vec::new(),
        },
    });
    store
        .apply(
            &changes(vec![
                StateChange::RecordLaunch(launch_row("l-1", LaunchPhase::Launching)),
                StateChange::RecordLaunch(done),
            ]),
            NOW,
        )
        .expect("launching → done");
    for planned in [
        effect(
            "run:r-1:prompt:task",
            EffectKind::Prompt,
            Some("r-1"),
            Some("l-1"),
        ),
        effect(
            "launch:l-1:evaluate",
            EffectKind::JevEvaluate,
            None,
            Some("l-1"),
        ),
    ] {
        store.apply(&plan(planned), NOW).expect("plan");
    }
    let ready: Vec<String> = store
        .ready_effects()
        .expect("ready read")
        .into_iter()
        .map(|row| row.key.0)
        .collect();
    assert_eq!(
        ready,
        ["run:r-1:prompt:task"],
        "the live Run's prompt passes; the done launch-bound eval does not"
    );
}

//! `matrix` — S1's kill matrix: the four effect families (`evaluate`,
//! the `tab`/`split` topology leg, `start:0`, `prompt:task`) each killed
//! at the four seam boundaries (`pre_dispatch`, `dispatch_committed`,
//! `wire_returned`, `result_committed`). Every cell: the seamed child
//! dies `SIGABRT` with the `seam hit` marker, a restarted daemon gives
//! the §4.5/F8 outcome the boundary prescribes — `planned` rows resume
//! exactly once, `dispatching` rows turn `unconfirmed` and drive the
//! convergence/settlement rows, `acknowledged` rows continue — with no
//! duplicate wire request and every deadline field untouched.

mod evaluate;
mod prompt;
mod settled;
mod split;
mod start;
mod tab;

use std::os::unix::process::ExitStatusExt as _;
use std::time::Duration;

use governor_core::delivery::MailboxEventKind;
use governor_core::identity::{PaneId, TabId, Timestamp};
use governor_core::lifecycle::{
    EffectCertainty, EffectKind, EffectReceipt, EffectState, EffectTarget, PromptCertainty, Run,
    Settlement, State, UnresolvedReason, settle,
};
use governor_core::task::{AbstainReason, Launch, LaunchOutcome, LaunchPhase};
use herdr_governor::daemon::{Boundary, SeamAction, SeamConfig};
use herdr_governor::store::Store;

use crate::support::daemon::{TestDaemon, never};
use crate::support::fake_herdr::FakeHerdr;

use super::*;

/// The task objective's marker — `agent.prompt` calls carrying it are
/// task prompts; a `nudge` ("still working?") never contains it, so the
/// count survives an idle-armed nudge on a post-restart `active` Run.
const OBJECTIVE: &str = "land the green refactor";

/// What one cell's kill leaves behind, read back through a fresh store
/// handle before the restart: the world (fakes + dirs), the admitted
/// launch row and the run the pipeline reached (none while the pipeline
/// is still pre-`begin`).
struct Cell {
    world: World,
    launch: Launch,
    run: Option<Run>,
}

/// Drive one launch until `suffix` hits `boundary`'s abort: the seamed
/// child dies mid-dispatch, the marker and `SIGABRT` prove the seam,
/// and the durable rows the kill left are captured for the restart's
/// assertions.
async fn kill_cell(suffix: &str, boundary: Boundary, related_tab: &str) -> Cell {
    let world = World::new(
        &point("op-a", 0, "vendor-a", "--a"),
        "launch_wait_secs = 15\n",
    );
    world.jev().push_answers(launch_eval(related_tab));
    let settings = world.dirs().settings();
    let child = TestDaemon::spawn_child(
        &settings,
        Some(SeamConfig {
            suffix: suffix.to_owned(),
            boundary,
            action: SeamAction::Abort,
        }),
    )
    .await;
    qualify_start(&mut world.store(), "op-a", &["--a"]);
    let _call = world.fire_launch(&launch_args(&task(&[]), "k1"));

    let (status, stderr) = child.wait().await;
    assert_eq!(
        status.signal(),
        Some(6),
        "the seam's abort is a SIGABRT: {stderr}"
    );
    assert!(
        stderr.contains(&format!("seam hit {suffix}@{}", boundary.as_str())),
        "the seam-hit marker names the boundary: {stderr}"
    );

    let store = world.store();
    let launch = only_launch(&store);
    let run = store.run_by_launch(&launch.id).ok().flatten();
    drop(store);
    Cell { world, launch, run }
}

/// `agent.prompt` calls carrying the task objective — the task-prompt
/// count `nudge` renders can't pollute.
fn task_prompts(fake: &FakeHerdr) -> usize {
    wire_calls(fake, "agent.prompt")
        .iter()
        .filter(|params| {
            params["text"]
                .as_str()
                .is_some_and(|text| text.contains(OBJECTIVE))
        })
        .count()
}

/// The deadline fields a restart lane must never touch (`idle_*` are
/// supervision-armed episodes, legitimately set by the running daemon).
fn deadlines(run: &Run) -> (Option<Timestamp>, Option<Timestamp>, Timestamp) {
    (
        run.repair_deadline,
        run.judgment_deadline,
        run.max_age_deadline,
    )
}

/// The run's deadline fields are bit-identical across the restart.
fn assert_deadlines_unchanged(before: Option<&Run>, store: &Store) {
    let Some(run) = before else {
        return;
    };
    let after = store.run(&run.id).expect("run read").expect("run row");
    assert_eq!(
        deadlines(&after),
        deadlines(run),
        "a restart never touches a Run's deadlines"
    );
}

/// The run's `active` row — the launched pipeline's terminal state for
/// cells that resume cleanly. Polling is the honest wait: `world.start()`
/// on a killed world can return on the stale socket the abort left
/// behind, so immediate post-start reads race the successor's startup.
async fn active_run(world: &World, launch: &Launch) -> Run {
    wait_store(&world.state(), "the run to go active", |store| {
        store
            .run_by_launch(&launch.id)
            .ok()
            .flatten()
            .filter(|run| run.state == State::Active)
    })
    .await
}

/// The run once `settlement` lands — the `launch_failed`/`absent` row a
/// topology-failure cell converges to.
async fn settled_run(world: &World, run: &Run) -> Run {
    wait_store(&world.state(), "the run to settle", |store| {
        store
            .run(&run.id)
            .ok()
            .flatten()
            .filter(|row| row.settlement.is_some())
    })
    .await
}

/// Assert `Settlement::Unresolved{LaunchFailed}` — every topology/start
/// leg that dies `dispatching` settles the identity-less run this way.
fn assert_launch_failed(run: &Run) {
    assert_eq!(
        run.settlement,
        Some(Settlement::Unresolved {
            reason: UnresolvedReason::LaunchFailed
        }),
        "{:?}",
        run.settlement
    );
}

/// The launch's `Failed{unknown}` outcome — the §4.5 row every
/// `dispatching`-at-kill topology/start leg converges to; `created`
/// counts the acknowledged receipts `createdTopology` must report.
fn assert_failed_unknown(launch: &Launch, run_id: &str, created: usize) {
    let Some(LaunchOutcome::Failed {
        certainty,
        run,
        created_topology,
    }) = &launch.outcome
    else {
        panic!("a failed launch: {:?}", launch.outcome);
    };
    assert_eq!(*certainty, EffectCertainty::Unknown);
    assert_eq!(run.as_ref().map(|id| id.0.as_str()), Some(run_id));
    let count = created_topology
        .panes
        .len()
        .saturating_add(usize::from(created_topology.tab.is_some()));
    assert_eq!(
        count, created,
        "createdTopology reports only the acknowledged receipts: {created_topology:?}"
    );
}

/// Poll the launch row until `done` and hand the outcome back.
async fn finished_launch(world: &World) -> Launch {
    launch_done(&world.state()).await
}

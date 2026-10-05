//! `seed` — the durable-row builders and store reads the launch e2e
//! cases share: `changes` wraps bare `StateChange`s into a `Transition`,
//! `launch_row`/`run_row`/`launch_chain` build the seeded shapes, the
//! store reads and bounded polls inspect the daemon's `governor.db`
//! through a side connection, and the git fixtures build the F6 repos.
//! Re-exported into the `e2e_launch` root.

use std::path::Path;
use std::time::{Duration, Instant};

use governor_core::delivery::{MailboxEvent, MailboxEventKind};
use governor_core::identity::{EffectKey, IdempotencyKey, LaunchId, ProjectRoot, RunId};
use governor_core::lifecycle::{Effect, Run, State, StateChange, Transition};
use governor_core::task::{Launch, LaunchPhase, Task};
use herdr_governor::store::Store;

use super::{DEADLINE, FAR, caller_key};

pub(crate) fn changes(state_changes: Vec<StateChange>) -> Transition {
    Transition {
        state_changes,
        events: Vec::new(),
        effects: Vec::new(),
    }
}

/// The `Task` a `launch_row` carries.
pub(crate) fn seeded_task(objective: &str) -> Task {
    Task {
        objective: objective.into(),
        scope: "src/".into(),
        done_when: vec!["cargo test passes".into()],
        constraints: Vec::new(),
        tier: None,
        recovery_of: None,
        label: None,
        cwd: None,
        retention: None,
    }
}

/// A `launches` row for seeding — `phase`, caller, root `/p`, key `k-<id>`.
pub(crate) fn launch_row(id: &str, phase: LaunchPhase) -> Launch {
    let task = seeded_task("land the green refactor");
    Launch {
        id: LaunchId(id.into()),
        caller: caller_key(),
        project_root: ProjectRoot("/p".into()),
        idempotency_key: IdempotencyKey(format!("k-{id}")),
        digest_version: Task::DIGEST_VERSION,
        task_digest: task.digest(),
        task,
        phase,
        decision: None,
        config_version: None,
        outcome: None,
    }
}

/// The `RecordLaunch` chain a non-`evaluating` phase seed needs — every
/// later phase is a guarded UPDATE on a legal predecessor (P4.0 matrix),
/// so the seed walks the path rather than jumping.
pub(crate) fn launch_chain(launch: &Launch) -> Vec<StateChange> {
    if launch.phase == LaunchPhase::Evaluating {
        return vec![StateChange::RecordLaunch(launch.clone())];
    }
    let mut first = launch.clone();
    first.phase = LaunchPhase::Evaluating;
    first.decision = None;
    first.config_version = None;
    first.outcome = None;
    vec![
        StateChange::RecordLaunch(first),
        StateChange::RecordLaunch(launch.clone()),
    ]
}

/// A `runs` row for seeding — `state`, owner the caller, no identity.
pub(crate) fn run_row(id: &str, launch: &str, state: State) -> Run {
    Run {
        id: RunId(id.into()),
        launch: LaunchId(launch.into()),
        owner: caller_key(),
        owner_generation: 0,
        version: 0,
        state,
        prompt_certainty: None,
        child_name: format!("gov-{id}"),
        identity: None,
        operating_point: None,
        provider: None,
        tier_start: None,
        cwd: "/p".into(),
        base_commit: None,
        work_generation: 0,
        evidence_generation: 0,
        evidence_digest: None,
        child_status: None,
        idle_since: None,
        idle_deadline: None,
        repair_deadline: None,
        rejected_at: None,
        judgment_deadline: None,
        judging_digest: None,
        max_age_deadline: FAR,
        nudge_episode: 0,
        nudged_episode: None,
        blocked_episode: 0,
        settlement: None,
        settled_at: None,
    }
}

// — Store reads ————————————————————————————————————————————————————————

pub(crate) fn open_store(state: &Path) -> Store {
    Store::open(&state.join("governor.db")).expect("store opens")
}

/// Every launch row, any phase.
pub(crate) fn all_launches(store: &Store) -> Vec<Launch> {
    [
        LaunchPhase::Evaluating,
        LaunchPhase::Routed,
        LaunchPhase::Launching,
        LaunchPhase::Done,
    ]
    .iter()
    .flat_map(|phase| store.launches_in_phase(*phase).expect("launches read"))
    .collect()
}

/// The one launch whose idempotency key starts with `prefix`.
pub(crate) fn launch_at(store: &Store, prefix: &str) -> Launch {
    let found: Vec<Launch> = all_launches(store)
        .into_iter()
        .filter(|launch| launch.idempotency_key.0.starts_with(prefix))
        .collect();
    assert_eq!(found.len(), 1, "exactly one launch keyed {prefix}*");
    found.into_iter().next().expect("one match")
}

/// The one launch the test's store holds.
pub(crate) fn only_launch(store: &Store) -> Launch {
    launch_at(store, "")
}

pub(crate) fn run_for(store: &Store, launch: &Launch) -> Run {
    store
        .run_by_launch(&launch.id)
        .expect("run read")
        .expect("the launch reserved a run")
}

pub(crate) fn effect_at(store: &Store, key: &str) -> Effect {
    store
        .effect(&EffectKey(key.into()))
        .expect("effect read")
        .unwrap_or_else(|| panic!("effect row {key}"))
}

/// `run:<id>:<suffix>` — the journal-key spelling `launch_plan` and
/// `plan_agent_start` use.
pub(crate) fn run_key(run: &Run, suffix: &str) -> String {
    format!("run:{}:{suffix}", run.id.0)
}

/// The caller's unacked mailbox events of `kind`.
pub(crate) fn caller_events(store: &Store, kind: MailboxEventKind) -> Vec<MailboxEvent> {
    store
        .mailbox_unacked(&caller_key(), None, 500)
        .expect("mailbox read")
        .into_iter()
        .filter(|event| event.kind == kind)
        .collect()
}

// — Bounded polls —————————————————————————————————————————————————————

/// Poll a fresh side-store view until `until` yields or `DEADLINE`
/// passes — `tokio::time::sleep` yields to the daemon and fakes on the
/// same runtime.
pub(crate) async fn wait_store<T>(
    state: &Path,
    what: &str,
    mut until: impl FnMut(&Store) -> Option<T>,
) -> T {
    let deadline = Instant::now().checked_add(DEADLINE).expect("deadline");
    while Instant::now() < deadline {
        let store = open_store(state);
        let value = until(&store);
        drop(store);
        if let Some(found) = value {
            return found;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("timed out waiting for {what}");
}

/// The first `done` launch — the poll a convergence case ends on.
pub(crate) async fn launch_done(state: &Path) -> Launch {
    wait_store(state, "a launch to finish", |store| {
        store
            .launches_in_phase(LaunchPhase::Done)
            .expect("done launches")
            .into_iter()
            .next()
    })
    .await
}

// — Git fixtures (F6) ——————————————————————————————————————————————————

/// `git -C root <args>` with an isolated config; returns trimmed stdout.
pub(crate) fn git(root: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout)
        .expect("utf8")
        .trim()
        .to_owned()
}

/// A git repository at `dir` with one commit — returns the head sha.
pub(crate) fn git_repo(dir: &Path) -> String {
    std::fs::create_dir_all(dir).expect("repo dir");
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["commit", "-q", "--allow-empty", "-m", "init"]);
    git(dir, &["rev-parse", "HEAD"])
}

/// `git init` without a commit — an unborn HEAD.
pub(crate) fn git_unborn(dir: &Path) {
    std::fs::create_dir_all(dir).expect("repo dir");
    git(dir, &["init", "-q", "-b", "main"]);
}

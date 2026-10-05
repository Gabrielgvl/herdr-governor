//! `coordinator/effects` — the §4.4 commit arm driven through
//! `Msg::DispatchCommit` (the subject gates, the generation staleness
//! check, the frozen-file audit, and the composed governor-refused write)
//! plus `hand_out`'s one-in-flight-per-subject serialization observed on
//! the runner mailbox.

use std::sync::Arc;
use std::time::Duration;

use sha2::Digest as _;

use governor_core::config::{ConfigVersion, OperatingPointId, Provider, Tier};
use governor_core::identity::{
    AgentKind, AgentName, ChildIdentity, Digest, EffectKey, HerdrIncarnation, PaneId, RunId, TabId,
    TerminalId,
};
use governor_core::lifecycle::{
    EffectCertainty, EffectKind, EffectReceipt, EffectResolution, EffectState, EffectTarget,
    EffectWrite, RunUpdate, Settlement, State, StateChange, op_digest, settle,
};
use governor_core::routing::{Candidate, Decision, Exploration, PlacementPlan};
use governor_core::task::LaunchPhase;
use tokio::sync::oneshot;

use crate::daemon::coordinator::{Coordinator, Msg};
use crate::daemon::runner::RunnerEnv;
use crate::daemon::{CommitVerdict, FileRef, FollowUpBody, RenderContext};
use crate::store::Store;

use super::coordinator::{
    NOW, bind_caller, caller, changes, coordinator_with, effect, launch_row, plan, policy, run_row,
    seed_run, store_in,
};

fn child_identity() -> ChildIdentity {
    ChildIdentity {
        herdr_incarnation: HerdrIncarnation("inc-1".into()),
        terminal_id: TerminalId("term-1".into()),
        agent_kind: AgentKind("kind-a".into()),
        agent_name: AgentName("gov-r-1".into()),
        native_session: None,
        pane_id: PaneId("pane-9".into()),
    }
}

/// A `launch` carrying the two-candidate `Decision` the `start:0`/`start:1`
/// effect keys index into (F15's commit-side recheck reads it live).
fn decided_launch() -> governor_core::task::Launch {
    let mut launch = launch_row("l-1", LaunchPhase::Routed);
    let candidate = |i: usize| Candidate {
        operating_point: OperatingPointId(format!("op-{i}")),
        provider: Provider(format!("prov-{i}")),
        tier: Tier("standard".into()),
        harness: AgentKind("kind-a".into()),
        args: vec![format!("--arg-{i}")],
    };
    launch.decision = Some(Decision {
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
        candidates: vec![candidate(0), candidate(1)],
        config_version: ConfigVersion("v".into()),
    });
    launch
}

/// Run `r-1` on launch `l-1`, in `starting`, with the two-candidate
/// decision routed and the `tab` leg journaled `acknowledged` — the state
/// a `start:` dispatch finds mid-launch.
fn seed_starting_run(store: &mut Store) {
    // The `start:` gate reads `run.launch`'s decision live, and `routed`
    // only upserts from `evaluating` — `seed_run`'s decision-less write
    // can't take the decision later, so route `l-1` with it attached.
    store
        .apply(
            &changes(vec![StateChange::RecordLaunch(launch_row(
                "l-1",
                LaunchPhase::Evaluating,
            ))]),
            NOW,
        )
        .expect("admit launch");
    store
        .apply(
            &changes(vec![
                StateChange::RecordLaunch(decided_launch()),
                StateChange::ReserveRun(run_row("r-1")),
            ]),
            NOW,
        )
        .expect("route + reserve");
    let mut run = store.run(&RunId("r-1".into())).expect("read").expect("run");
    run.state = State::Starting;
    let expected = run.version;
    run.version = expected.saturating_add(1);
    store
        .apply(
            &changes(vec![StateChange::UpdateRun(RunUpdate {
                expected_version: expected,
                record: run,
            })]),
            NOW,
        )
        .expect("start the run");

    // The journaled `tab` ack whose pane the starts reuse — seeded
    // through the journal's own planned → dispatching → acknowledged
    // writes.
    store
        .apply(
            &plan(effect(
                "run:r-1:tab",
                EffectKind::TabCreate,
                Some("r-1"),
                None,
            )),
            NOW,
        )
        .expect("plan tab");
    for write in [
        EffectWrite::Dispatch {
            key: EffectKey("run:r-1:tab".into()),
        },
        EffectWrite::Result {
            key: EffectKey("run:r-1:tab".into()),
            resolution: EffectResolution::Acknowledged {
                receipt: Some(EffectReceipt::TabCreated {
                    tab: TabId("tab-1".into()),
                    pane: PaneId("pane-9".into()),
                }),
            },
        },
    ] {
        store
            .apply(&changes(vec![StateChange::WriteEffect(write)]), NOW)
            .expect("journal tab leg");
    }
}

/// A `close` context — the lightest `RenderContext` to build. Where the
/// arm under test never consults the context's fields (the subject and
/// generation gates precede the file audit, and B1 can render no `Jev`
/// context), a `Close` stands in honestly: the gate reads nothing of it.
fn close_context() -> Arc<RenderContext> {
    Arc::new(RenderContext::Close {
        target: child_identity(),
    })
}

/// Drive one `DispatchCommit` through the coordinator and take the verdict.
async fn commit(
    coordinator: &mut Coordinator,
    key: &str,
    context: Arc<RenderContext>,
) -> CommitVerdict {
    let (reply, answered) = oneshot::channel();
    coordinator
        .handle(Msg::DispatchCommit {
            key: EffectKey(key.into()),
            context,
            reply,
        })
        .await;
    answered.await.expect("the arm always answers")
}

/// A second handle on the same store file — the commit arm's writes are
/// committed before `handle` returns, so a probe connection sees them.
fn probe(dir: &std::path::Path) -> Store {
    store_in(dir)
}

// — §4.4 step 3, subject gate —————————————————————————————————————————

/// A `settled` Run cannot dispatch — except `close` (and `event:` hints),
/// which are exempt by design (READY_EFFECTS' exclusions mirrored into the
/// commit arm). The prompt stays `planned`; the close commits.
#[tokio::test]
async fn settled_subject_skips_but_close_commits() {
    let tmp = tempfile::tempdir().expect("tmp");
    let mut store = store_in(tmp.path());
    bind_caller(&mut store);
    seed_run(&mut store, &run_row("r-1"));
    let run = store.run(&RunId("r-1".into())).expect("read").expect("run");
    store
        .apply(&settle(&run, Settlement::NoHandoff, NOW, &policy()), NOW)
        .expect("settle");

    let target = EffectTarget::Child(child_identity());
    let mut prompt = effect("run:r-1:prompt:task", EffectKind::Prompt, Some("r-1"), None);
    prompt.target = Some(target.clone());
    let mut close = effect("run:r-1:close", EffectKind::Close, Some("r-1"), None);
    close.target = Some(target.clone());
    // The F8 audit runs past the subject gate only for `close`: give the
    // row the honest digest so `Go` is reachable.
    close.payload_digest = Some(op_digest(EffectKind::Close, Some(&target), &[]));
    store.apply(&plan(prompt), NOW).expect("plan prompt");
    store.apply(&plan(close), NOW).expect("plan close");

    let mut coordinator = coordinator_with(store, tmp.path());
    let probe = probe(tmp.path());

    assert_eq!(
        commit(&mut coordinator, "run:r-1:prompt:task", close_context()).await,
        CommitVerdict::Skip,
        "a settled subject's prompt can never dispatch"
    );
    let skipped = probe
        .effect(&EffectKey("run:r-1:prompt:task".into()))
        .expect("read")
        .expect("row");
    assert_eq!(skipped.state, EffectState::Planned, "Skip writes nothing");

    assert_eq!(
        commit(&mut coordinator, "run:r-1:close", close_context()).await,
        CommitVerdict::Go,
        "close is exempt — it dispatches against a settled run"
    );
    let committed = probe
        .effect(&EffectKey("run:r-1:close".into()))
        .expect("read")
        .expect("row");
    assert_eq!(committed.state, EffectState::Dispatching);
    assert!(committed.dispatched_at.is_some(), "the commit stamps it");
}

// — §4.4 step 3, generation staleness —————————————————————————————————

/// A run-bound ask is stale the moment the generation its key carries no
/// longer matches the Run's live numbers (F20): `review:<eg>` names the
/// evidence generation, `accept:<wg>:<eg>` both. Three asks under one Run:
/// the stale pair stays `planned`, the current one commits.
#[tokio::test]
async fn stale_generation_ask_skips_current_commits() {
    let tmp = tempfile::tempdir().expect("tmp");
    let mut store = store_in(tmp.path());
    bind_caller(&mut store);
    seed_run(&mut store, &run_row("r-1"));

    for key in ["run:r-1:review:0", "run:r-1:review:1", "run:r-1:accept:1:0"] {
        let mut ask = effect(key, EffectKind::JevEvaluate, Some("r-1"), None);
        // `jev_evaluate` digests no dispatch descriptor — `None` is its
        // honest NULL.
        ask.payload_digest = None;
        store.apply(&plan(ask), NOW).expect("plan ask");
    }

    // The Run was minted at work_generation 0 / evidence_generation 0;
    // moving the work generation makes `accept:1:0` current and every
    // `accept:0:*` stale. `review` keys name the evidence generation,
    // which has not moved: `review:0` is current, `review:1` names a
    // generation that never was.
    let mut run = store.run(&RunId("r-1".into())).expect("read").expect("run");
    run.work_generation = run.work_generation.saturating_add(1);
    let expected = run.version;
    run.version = expected.saturating_add(1);
    store
        .apply(
            &changes(vec![StateChange::UpdateRun(RunUpdate {
                expected_version: expected,
                record: run,
            })]),
            NOW,
        )
        .expect("bump work generation");

    let mut coordinator = coordinator_with(store, tmp.path());
    let probe = probe(tmp.path());

    assert_eq!(
        commit(&mut coordinator, "run:r-1:review:1", close_context()).await,
        CommitVerdict::Skip,
        "evidence_generation 1 never was"
    );
    assert_eq!(
        commit(&mut coordinator, "run:r-1:review:0", close_context()).await,
        CommitVerdict::Go,
        "evidence_generation 0 still holds"
    );
    assert_eq!(
        commit(&mut coordinator, "run:r-1:accept:1:0", close_context()).await,
        CommitVerdict::Go,
        "work_generation 1 + evidence_generation 0 both hold"
    );

    let probe_row = |key: &str| {
        probe
            .effect(&EffectKey(key.into()))
            .expect("read")
            .expect("row")
    };
    assert_eq!(probe_row("run:r-1:review:1").state, EffectState::Planned);
    assert_eq!(
        probe_row("run:r-1:review:0").state,
        EffectState::Dispatching
    );
    assert_eq!(
        probe_row("run:r-1:accept:1:0").state,
        EffectState::Dispatching
    );
}

// — §4.4 step 3, frozen-file audit ————————————————————————————————————

/// A `File` body the context commits to must still carry the size+digest
/// the coordinator recorded at hand-off: a rewritten file refuses the
/// commit — the composed `[Dispatch]+EffectResult` writes
/// `dispatching → failed{absent}` in one apply (§4.2).
#[tokio::test]
async fn frozen_file_mismatch_refuses_and_commits_the_refusal() {
    let tmp = tempfile::tempdir().expect("tmp");
    let mut store = store_in(tmp.path());
    bind_caller(&mut store);
    for launch in ["l-1", "l-2"] {
        store
            .apply(
                &changes(vec![StateChange::RecordLaunch(launch_row(
                    launch,
                    LaunchPhase::Evaluating,
                ))]),
                NOW,
            )
            .expect("record launch");
    }

    // The frozen body, exactly as a `follow_up_context` hand-off records it.
    let body_path = tmp.path().join("body.md");
    std::fs::write(&body_path, "frozen body").expect("write body");
    let bytes = std::fs::read(&body_path).expect("read body");
    let file = FileRef {
        path: body_path.to_string_lossy().into_owned(),
        size: u64::try_from(bytes.len()).expect("a file fits u64"),
        digest: Digest(sha2::Sha256::digest(&bytes).into()),
    };
    // The file audit reads `context.files()`; `FollowUp` carries the one
    // file-bearing body B1 can construct (the `Jev` variant renders only
    // once PR C's question catalog lands).
    let context = Arc::new(RenderContext::FollowUp {
        body: FollowUpBody::File(file),
        target: child_identity(),
        sender: caller(1),
        sender_pane: PaneId("pane-1".into()),
    });
    for launch in ["l-1", "l-2"] {
        let mut ask = effect(
            &format!("launch:{launch}:evaluate"),
            EffectKind::JevEvaluate,
            None,
            Some(launch),
        );
        ask.payload_digest = None;
        store.apply(&plan(ask), NOW).expect("plan ask");
    }

    let mut coordinator = coordinator_with(store, tmp.path());
    let probe = probe(tmp.path());

    // Control: an intact frozen file commits.
    assert_eq!(
        commit(
            &mut coordinator,
            "launch:l-1:evaluate",
            Arc::clone(&context)
        )
        .await,
        CommitVerdict::Go
    );

    // The file moves between hand-off and commit.
    std::fs::write(&body_path, "rewritten body — different bytes").expect("rewrite");
    assert_eq!(
        commit(&mut coordinator, "launch:l-2:evaluate", context).await,
        CommitVerdict::Refused,
        "the frozen bytes no longer match — the commit refuses"
    );
    let refused = probe
        .effect(&EffectKey("launch:l-2:evaluate".into()))
        .expect("read")
        .expect("row");
    assert_eq!(
        (refused.state, refused.certainty),
        (EffectState::Failed, Some(EffectCertainty::Absent)),
        "the composed write lands dispatch+failed in one apply"
    );
    assert!(
        refused.dispatched_at.is_some(),
        "the Dispatch leg of the composed write stamped it"
    );
}

// — §4.4 hand-off: one in flight per subject ——————————————————————————

/// Two planned `start:` legs on one Run: `hand_out` spawns the first and
/// holds the second until the first's commit frees the subject — the
/// mailbox traffic shows the serialization, and the first's
/// `qualification_lapsed` refusal (F15: no `passed` qualification row)
/// doubles as the composed-refusal shape check.
#[tokio::test]
async fn hand_out_holds_one_effect_per_subject() {
    let tmp = tempfile::tempdir().expect("tmp");
    let mut store = store_in(tmp.path());
    bind_caller(&mut store);
    seed_starting_run(&mut store);

    // The two `start:` legs — same subject, distinct candidates. The
    // F8 audit never runs for them: the F15 gate refuses first, so the
    // honest `None` digest is fine.
    for index in 0..2_usize {
        let mut start = effect(
            &format!("run:r-1:start:{index}"),
            EffectKind::AgentStart,
            Some("r-1"),
            None,
        );
        start.target = Some(EffectTarget::AgentPane(PlacementPlan::NewTab));
        start.payload_digest = None;
        store.apply(&plan(start), NOW).expect("plan start");
    }

    let mut coordinator = coordinator_with(store, tmp.path());
    let (tx, mut rx) = tokio::sync::mpsc::channel(16);
    // The Jev leg never fires here — the effects under test are all
    // Herdr legs — but the env fields are real values.
    let (jev, jev_key) = super::jev_env(tmp.path()).await;
    coordinator.arm_runner(RunnerEnv {
        herdr: crate::adapters::herdr::Client::new(tmp.path().join("no.sock")),
        herdr_op: Duration::from_millis(50),
        agent_start: Duration::from_millis(50),
        jev,
        jev_key,
        jev_timeout: Duration::from_millis(50),
        tx,
        shutdown: coordinator.shutdown_receiver(),
        seam: None,
    });

    // First pass: only `start:0` is handed out — `start:1` waits for the
    // subject's in-flight slot to free.
    coordinator.hand_out();
    let first = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("the first dispatch posts its commit")
        .expect("channel open");
    let Msg::DispatchCommit { key, .. } = &first else {
        panic!("a runner's first post is its DispatchCommit");
    };
    assert_eq!(key.0, "run:r-1:start:0", "planned order hands out first");

    // A second pass must not hand `start:1` out while `start:0` is in
    // flight — the timeout yields to spawned tasks, so a leaked second
    // spawn would post its own commit.
    coordinator.hand_out();
    assert!(
        tokio::time::timeout(Duration::from_millis(100), rx.recv())
            .await
            .is_err(),
        "one effect per subject — the second holds planned"
    );

    // The commit arm refuses `start:0` (no `passed` qualification row —
    // F15's live recheck) and frees the subject.
    coordinator.handle(first).await;
    let second_pending = probe(tmp.path())
        .effect(&EffectKey("run:r-1:start:1".into()))
        .expect("read")
        .expect("row");
    assert_eq!(second_pending.state, EffectState::Planned);
    let refused = probe(tmp.path())
        .effect(&EffectKey("run:r-1:start:0".into()))
        .expect("read")
        .expect("row");
    assert_eq!(
        (refused.state, refused.certainty),
        (EffectState::Failed, Some(EffectCertainty::Absent)),
        "the governor-refused result commits dispatch+failed atomically"
    );

    // Freed: the next pass hands `start:1` out.
    coordinator.hand_out();
    let second = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("the freed subject's next effect dispatches")
        .expect("channel open");
    let Msg::DispatchCommit {
        key: second_key, ..
    } = &second
    else {
        panic!("a runner's first post is its DispatchCommit");
    };
    assert_eq!(second_key.0, "run:r-1:start:1");
}

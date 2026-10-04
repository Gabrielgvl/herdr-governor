//! `pipeline` — F5 admission end to end: the start-ack `launched`
//! answer, every outcome class on the wire, the F6 base-commit variants
//! (git head pinned; plain dir and unborn HEAD the legal `None`; a git
//! that cannot run, or fails, the typed refusal before admission), the
//! `cwd` and rendered-size refusals, and ten concurrent reservations.

use std::collections::BTreeSet;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use governor_core::delivery::MailboxEventKind;
use governor_core::lifecycle::{EffectState, State};
use governor_core::task::{LaunchOutcome, LaunchPhase};
use serde_json::json;

use crate::support::fake_herdr::Fault as HerdrFault;
use crate::support::fake_jev::{Answer, Fault};
use crate::support::mcp_client::{McpClient, caller_envelope, status_call, status_page};

use super::*;

/// Nothing durable exists — no launch row of any phase.
fn assert_nothing_recorded(world: &World) {
    assert!(
        all_launches(&world.store()).is_empty(),
        "a refusal records no launch row"
    );
    assert!(world.jev().requests().is_empty(), "no Jev ask was made");
    assert!(
        !saw_wire(world.fake(), "tab.create") && !saw_wire(world.fake(), "agent.start"),
        "no topology effect ran"
    );
}

/// §4.5/F15 — the start ack itself answers the parked caller: with the
/// reconcile tick an hour away, admission → eval → `decided` → `begin`
/// → `tab.create` → `agent.start` ack → `finish(Launched)` in the same
/// commit; the parked reply carries the §6.2 body, the Run pins the
/// admission-time HEAD, and `launch_answered` lands exactly once.
#[tokio::test]
async fn f5_launch_answers_launched_at_the_start_ack() {
    let mut world = World::build(caller_topology(), |catalog| {
        catalog.reconcile_secs = 3600;
        catalog.points_toml = point("op-a", 0, "vendor-a", "--a");
        catalog.daemon_extra = "launch_wait_secs = 15\n".to_owned();
    });
    world.jev().push_answers(launch_eval("new"));
    let head = git_repo(world.project());
    world.start().await;
    qualify_start(&mut world.store(), "op-a", &["--a"]);

    let body = tool_body(&world.launch(&launch_args(&task(&[]), "k1")).await);
    assert_eq!(body["outcome"], "launched", "answered at the ack: {body}");
    assert_eq!(body["operatingPointId"], "op-a");
    assert!(
        body.get("requestedOperatingPointId").is_none(),
        "no fallback, no requested point: {body}"
    );
    assert_eq!(body["tierEvidence"]["startTier"], "standard");
    assert_eq!(body["tierEvidence"]["judgedTier"], "standard");

    let store = world.store();
    let launch = only_launch(&store);
    let run = run_for(&store, &launch);
    assert_eq!(body["runId"], run.id.0.as_str());
    assert!(
        matches!(launch.outcome, Some(LaunchOutcome::Launched { .. })),
        "the stored outcome: {:?}",
        launch.outcome
    );
    assert!(
        run.child_name.starts_with("gov-") && run.child_name.len() == 12,
        "gov-<runId[0..8]>: {}",
        run.child_name
    );
    assert_eq!(
        run.base_commit.as_deref(),
        Some(head.as_str()),
        "the admission-time probe pinned HEAD"
    );
    assert_eq!(
        effect_at(&store, &format!("launch:{}:evaluate", launch.id.0)).state,
        EffectState::Acknowledged
    );
    assert_eq!(
        caller_events(&store, MailboxEventKind::LaunchAnswered).len(),
        1,
        "launch_answered exactly once"
    );
    assert_eq!(world.jev().requests().len(), 1, "one evaluation asked");
    world.shutdown().await;
}

/// F5 — every outcome class through one daemon: `launched`; `rejected`
/// (doneWhen not verifiable); `abstained` (a Jev HTTP failure);
/// `failed` with `effectCertainty`, `runId` and an empty
/// `createdTopology` (the `tab.create` leg timed out); `pending
/// {launchId}` when the evaluation outlives `launch_wait`.
#[tokio::test]
async fn f5_outcomes_pending_launched_abstained_rejected_failed() {
    let mut world = World::new(
        &point("op-a", 0, "vendor-a", "--a"),
        "launch_wait_secs = 3\njev_timeout_secs = 6\nshutdown_grace_secs = 1\n",
    );
    world.start().await;
    qualify_start(&mut world.store(), "op-a", &["--a"]);

    world.jev().push_answers(launch_eval("new"));
    let launched = tool_body(&world.launch(&launch_args(&task(&[]), "k-launched")).await);
    assert_eq!(launched["outcome"], "launched", "{launched}");

    world.jev().push_answers(with_answer(
        launch_eval("new"),
        "done_when_verifiable",
        &Answer::noul(0.1),
    ));
    let rejected = tool_body(&world.launch(&launch_args(&task(&[]), "k-rejected")).await);
    assert_eq!(rejected, json!({"outcome": "rejected"}));

    world.jev().push_fault(Fault::Status {
        status: 503,
        error_type: None,
        retry_after_ms: None,
    });
    let abstained = tool_body(&world.launch(&launch_args(&task(&[]), "k-abstained")).await);
    assert_eq!(
        abstained,
        json!({"outcome": "abstained", "reason": "evaluation_failed"})
    );

    world.jev().push_answers(launch_eval("new"));
    world.fake().fault(
        "tab.create",
        HerdrFault::TimeoutAfter(Duration::from_millis(50)),
    );
    let failed = tool_body(&world.launch(&launch_args(&task(&[]), "k-failed")).await);
    assert_eq!(failed["outcome"], "failed", "{failed}");
    assert_eq!(failed["effectCertainty"], "unknown");
    assert!(failed["runId"].is_string(), "the reserved run: {failed}");
    assert_eq!(failed["createdTopology"], json!({"tab": null, "panes": []}));

    world.jev().push_fault(Fault::Silent);
    let pending = tool_body(&world.launch(&launch_args(&task(&[]), "k-pending")).await);
    assert_eq!(pending["outcome"], "pending", "{pending}");
    let launch_id = pending["launchId"].as_str().expect("launchId");
    assert_eq!(
        launch_at(&world.store(), "k-pending").id.0,
        launch_id,
        "pending names the in-flight launch"
    );
    world.shutdown().await;
}

/// F6 — a cwd outside any repository pins no base: a plain project root
/// (`NotARepo`) and an unborn HEAD subdirectory (`UnbornHead`) both
/// launch with `base_commit = NULL`.
#[tokio::test]
async fn f5_plain_directory_cwd_launches_with_no_base_commit() {
    let mut world = World::new(
        &point("op-a", 0, "vendor-a", "--a"),
        "launch_wait_secs = 15\n",
    );
    world.jev().push_answers(launch_eval("new"));
    git_unborn(&world.project().join("unborn"));
    let unborn = canonical(&world.project().join("unborn"));
    world.start().await;
    qualify_start(&mut world.store(), "op-a", &["--a"]);

    let plain = world.launch(&launch_args(&task(&[]), "k-plain")).await;
    assert_eq!(tool_body(&plain)["outcome"], "launched");
    let cwd = json!(unborn.to_str().expect("utf8"));
    let unborn_reply = world
        .launch(&launch_args(&task(&[("cwd", cwd)]), "k-unborn"))
        .await;
    assert_eq!(tool_body(&unborn_reply)["outcome"], "launched");

    let store = world.store();
    for key in ["k-plain", "k-unborn"] {
        let run = run_for(&store, &launch_at(&store, key));
        assert_eq!(run.base_commit, None, "{key}: nothing to pin");
    }
    world.shutdown().await;
}

/// Kills the hand-spawned daemon even when an assertion unwinds first.
struct KillOnDrop(Child);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        self.0.kill().ok();
        self.0.wait().ok();
    }
}

/// F6 — `git` that cannot run (the daemon's PATH holds no binary) is the
/// typed `GIT_EVIDENCE_UNAVAILABLE` tool error, and nothing is recorded:
/// no launch row, no Jev ask, no topology effect.
#[tokio::test]
async fn f5_git_unavailable_is_a_typed_tool_error_before_admission() {
    let world = World::new(
        &point("op-a", 0, "vendor-a", "--a"),
        "launch_wait_secs = 15\n",
    );
    world.jev().push_answers(launch_eval("new"));
    let empty_path = world.outside();
    let daemon = KillOnDrop(
        Command::new(env!("CARGO_BIN_EXE_herdr-governor"))
            .arg("daemon")
            .arg("--state-dir")
            .arg(world.dirs().state_dir())
            .arg("--config-dir")
            .arg(world.dirs().config_dir())
            .env("PATH", &empty_path)
            .env_remove("GOV_DAEMON_SEAM")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("daemon child spawns"),
    );
    wait_for_socket(&world.dirs().socket_path()).await;

    let reply = world.launch(&launch_args(&task(&[]), "k1")).await;
    assert_eq!(tool_code(&reply), "GIT_EVIDENCE_UNAVAILABLE");
    assert_nothing_recorded(&world);
    drop(daemon);
}

/// The hand-spawned child's bind — a bounded poll on the socket path.
async fn wait_for_socket(sock: &Path) {
    let deadline = Instant::now().checked_add(DEADLINE).expect("deadline");
    while !sock.exists() {
        assert!(Instant::now() < deadline, "the daemon never bound");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// F6 — a git failure outside the two legal-`None` classes (a corrupt
/// `.git/config`: `rev-parse` exits `fatal: bad config`) is the same
/// typed refusal, before admission.
#[tokio::test]
async fn f5_git_failed_status_is_a_typed_tool_error_before_admission() {
    let mut world = World::new(
        &point("op-a", 0, "vendor-a", "--a"),
        "launch_wait_secs = 15\n",
    );
    world.jev().push_answers(launch_eval("new"));
    git_repo(world.project());
    std::fs::write(world.project().join(".git/config"), "not config\n").expect("corrupt config");
    world.start().await;

    let reply = world.launch(&launch_args(&task(&[]), "k1")).await;
    assert_eq!(tool_code(&reply), "GIT_EVIDENCE_UNAVAILABLE");
    assert_nothing_recorded(&world);
    world.shutdown().await;
}

/// F5 — `cwd` must resolve inside `projectRoot` and be its own realpath:
/// a directory outside the root and a symlink spelling inside it are
/// both `TASK_INVALID`, before anything is recorded.
#[tokio::test]
async fn f5_cwd_must_be_inside_root() {
    let mut world = World::new(
        &point("op-a", 0, "vendor-a", "--a"),
        "launch_wait_secs = 15\n",
    );
    world.jev().push_answers(launch_eval("new"));
    let real = world.project().join("real");
    std::fs::create_dir_all(&real).expect("real dir");
    std::os::unix::fs::symlink(&real, world.project().join("link")).expect("symlink");
    world.start().await;

    let outside = json!(world.outside().to_str().expect("utf8"));
    let refused = world
        .launch(&launch_args(&task(&[("cwd", outside)]), "k-outside"))
        .await;
    assert_eq!(tool_code(&refused), "TASK_INVALID");
    let message = tool_body(&refused)["message"].to_string();
    assert!(message.contains("cwd_outside_root"), "{message}");

    let link = json!(world.project().join("link").to_str().expect("utf8"));
    let symlinked = world
        .launch(&launch_args(&task(&[("cwd", link)]), "k-link"))
        .await;
    assert_eq!(tool_code(&symlinked), "TASK_INVALID");
    assert_nothing_recorded(&world);
    world.shutdown().await;
}

/// F5/N5 — a Task whose rendering exceeds 64 KiB is `TASK_INVALID`
/// (`rendered_too_large`) before anything is recorded.
#[tokio::test]
async fn f5_rendered_task_over_64k_refused() {
    let mut world = World::new(
        &point("op-a", 0, "vendor-a", "--a"),
        "launch_wait_secs = 15\n",
    );
    world.start().await;

    let big = json!("x".repeat(70 * 1024));
    let reply = world
        .launch(&launch_args(&task(&[("objective", big)]), "k1"))
        .await;
    assert_eq!(tool_code(&reply), "TASK_INVALID");
    let message = tool_body(&reply)["message"].to_string();
    assert!(message.contains("rendered_too_large"), "{message}");
    assert_nothing_recorded(&world);
    world.shutdown().await;
}

/// F2/F1 — ten concurrent reservations: every parked caller launches,
/// the v4 Run ids and `gov-<runId[0..8]>` child names are pairwise
/// distinct, and the wire shows ten independent starts.
#[tokio::test]
async fn f2_ten_concurrent_reservations_mint_distinct_names() {
    let mut world = World::new(
        &point("op-a", 0, "vendor-a", "--a"),
        "launch_wait_secs = 15\n",
    );
    world.jev().push_answers(launch_eval("new"));
    git_repo(world.project());
    world.start().await;
    qualify_start(&mut world.store(), "op-a", &["--a"]);

    let calls: Vec<_> = (0..10_u8)
        .map(|index| world.spawn_launch(&launch_args(&task(&[]), &format!("k{index}"))))
        .collect();
    let mut run_ids = BTreeSet::new();
    for call in calls {
        let body = tool_body(&call.await.expect("call joins"));
        assert_eq!(body["outcome"], "launched", "{body}");
        assert!(
            run_ids.insert(body["runId"].as_str().expect("runId").to_owned()),
            "run ids are distinct"
        );
    }

    let store = world.store();
    let launches = store
        .launches_in_phase(LaunchPhase::Done)
        .expect("done launches");
    let names: BTreeSet<String> = launches
        .iter()
        .map(|launch| run_for(&store, launch))
        .inspect(|run| {
            assert!(run.base_commit.is_some(), "every run pinned the base");
            assert!(
                matches!(run.state, State::Prompting | State::Active),
                "{:?}",
                run.state
            );
        })
        .map(|run| run.child_name)
        .collect();
    assert_eq!(names.len(), 10, "ten distinct child names");
    assert_eq!(wire_calls(world.fake(), "agent.start").len(), 10);
    assert_eq!(world.jev().requests().len(), 10, "ten evaluations asked");
    world.shutdown().await;
}

/// `/proc/<pid>/status`'s `VmHWM` — the child's peak RSS in KiB; it
/// never falls, so a read after the load covers the whole window.
fn peak_rss_kib(pid: u32) -> u64 {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).expect("proc status");
    status
        .lines()
        .find(|line| line.starts_with("VmHWM:"))
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|kib| kib.parse().ok())
        .expect("VmHWM in KiB")
}

/// S32/N3/N4 — the combined load: a real child daemon answers fifty
/// concurrent `herdr_status` calls and ten concurrent `herdr_launch`
/// calls — every caller answered, ten distinct `gov-` child names — and
/// the daemon's peak RSS stays under the 176 MiB bound.
#[tokio::test]
async fn s32_fifty_status_calls_and_ten_launches_answer_under_rss() {
    let world = World::new(
        &point("op-a", 0, "vendor-a", "--a"),
        "launch_wait_secs = 15\n",
    );
    world.jev().push_answers(launch_eval("new"));
    let daemon = KillOnDrop(
        Command::new(env!("CARGO_BIN_EXE_herdr-governor"))
            .arg("daemon")
            .arg("--state-dir")
            .arg(world.dirs().state_dir())
            .arg("--config-dir")
            .arg(world.dirs().config_dir())
            .env_remove("GOV_DAEMON_SEAM")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("daemon child spawns"),
    );
    let pid = daemon.0.id();
    wait_for_socket(&world.dirs().socket_path()).await;
    qualify_start(&mut world.store(), "op-a", &["--a"]);

    let statuses: Vec<_> = {
        let status_client = Arc::new(McpClient::new(
            &world.dirs().socket_path(),
            caller_envelope(CALLER_PANE, world.project().to_str().expect("utf8"), RELAY),
        ));
        (0..50_u32)
            .map(|index| {
                let client = Arc::clone(&status_client);
                tokio::spawn(async move { client.call(&status_call(json!(index))).await })
            })
            .collect()
    };
    let launches: Vec<_> = (0..10_u8)
        .map(|index| world.spawn_launch(&launch_args(&task(&[]), &format!("s32-{index}"))))
        .collect();

    for call in statuses {
        let _page = status_page(&call.await.expect("status joins"));
    }
    let mut run_ids = BTreeSet::new();
    for call in launches {
        let body = tool_body(&call.await.expect("call joins"));
        assert_eq!(body["outcome"], "launched", "{body}");
        assert!(
            run_ids.insert(body["runId"].as_str().expect("runId").to_owned()),
            "run ids are distinct"
        );
    }

    let store = world.store();
    let names: BTreeSet<String> = all_launches(&store)
        .iter()
        .map(|launch| run_for(&store, launch).child_name)
        .collect();
    assert_eq!(names.len(), 10, "ten distinct child names");

    let hwm = peak_rss_kib(pid);
    assert!(
        hwm <= 176 * 1024,
        "daemon peak RSS {hwm} KiB exceeds the 176 MiB bound"
    );
    drop(daemon);
}

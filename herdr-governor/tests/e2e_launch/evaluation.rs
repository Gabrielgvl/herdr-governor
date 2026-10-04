//! `evaluation` — F12 on the wire: every evaluation failure class
//! (silent, transport, auth, HTTP, malformed, oversize) abstains
//! `evaluation_failed` with zero effects; an evaluation left
//! `dispatching` by a kill abstains `interrupted_before_decision` on
//! restart, never re-asked; and the request Jev receives names no
//! operating point, provider, argument, tier request, label or cwd.

use governor_core::delivery::MailboxEventKind;
use governor_core::lifecycle::{EffectCertainty, EffectState};
use governor_core::task::{AbstainReason, LaunchOutcome};
use herdr_governor::daemon::{Boundary, SeamAction, SeamConfig};
use serde_json::json;

use crate::support::fake_jev::{Answer, Fault};

use super::*;

/// F12 — silent (the ask is never answered and the deadline fires),
/// transport (connection dropped mid-call), auth (401), HTTP (500),
/// malformed (a choice outside its own distribution) and oversize (a
/// request over 96 KiB, refused before any socket write) each abstain
/// `evaluation_failed`: no Run reserved, no topology, each caller
/// answered once, and the oversize ask never reaches Jev.
#[tokio::test]
async fn f12_transport_auth_http_malformed_oversize_abstain_with_zero_effects() {
    let mut world = World::new(
        &point("op-a", 0, "vendor-a", "--a"),
        "launch_wait_secs = 15\njev_timeout_secs = 1\n",
    );
    world.start().await;
    qualify_start(&mut world.store(), "op-a", &["--a"]);

    let status = |code| Fault::Status {
        status: code,
        error_type: None,
        retry_after_ms: None,
    };
    world.jev().push_fault(Fault::Silent);
    world.jev().push_fault(Fault::Abort);
    world.jev().push_fault(status(401));
    world.jev().push_fault(status(500));
    world.jev().push_answers(with_answer(
        launch_eval("new"),
        "weakest_sufficient_tier",
        &Answer::choice("standard", &[("elsewhere", 1.0)]),
    ));
    // 60,000 quote characters render to ~60 KB (inside F5's 64 KiB) but
    // serialize escaped to ~120 KB — past the 96 KiB request bound.
    let oversize = task(&[("objective", json!("\"".repeat(60_000)))]);
    let cases = [
        ("k-silent", task(&[])),
        ("k-transport", task(&[])),
        ("k-auth", task(&[])),
        ("k-http", task(&[])),
        ("k-malformed", task(&[])),
        ("k-oversize", oversize),
    ];
    for (key, case) in cases {
        let body = tool_body(&world.launch(&launch_args(&case, key)).await);
        assert_eq!(
            body,
            json!({"outcome": "abstained", "reason": "evaluation_failed"}),
            "{key}"
        );
        let store = world.store();
        let launch = launch_at(&store, key);
        assert!(
            store.run_by_launch(&launch.id).expect("read").is_none(),
            "{key}: an abstention reserves no Run"
        );
    }
    assert_eq!(
        world.jev().requests().len(),
        5,
        "the oversize request never left the daemon"
    );
    assert!(
        !saw_wire(world.fake(), "tab.create")
            && !saw_wire(world.fake(), "pane.split")
            && !saw_wire(world.fake(), "agent.start")
            && !saw_wire(world.fake(), "agent.prompt"),
        "zero Herdr effects"
    );
    assert_eq!(
        caller_events(&world.store(), MailboxEventKind::LaunchAnswered).len(),
        6,
        "every abstained caller is answered exactly once"
    );
    world.shutdown().await;
}

/// F12/F28 — a kill between the eval's dispatch commit and its wire leg
/// leaves the ask `dispatching`; the restart marks it `unconfirmed` and
/// the Launch abstains `interrupted_before_decision` — the stranded ask
/// closes `failed/unknown` and is never evaluated twice.
#[tokio::test]
async fn f12_dispatching_eval_abstains_interrupted_on_restart() {
    let mut world = World::new(
        &point("op-a", 0, "vendor-a", "--a"),
        "launch_wait_secs = 15\n",
    );
    world.jev().push_answers(launch_eval("new"));
    world
        .spawn_child(SeamConfig {
            suffix: "evaluate".to_owned(),
            boundary: Boundary::DispatchCommitted,
            action: SeamAction::Abort,
        })
        .await;
    let _call = world.fire_launch(&launch_args(&task(&[]), "k1"));
    let stderr = world.wait_child().await;
    assert!(
        stderr.contains("seam hit evaluate@dispatch_committed"),
        "{stderr}"
    );

    world.start().await;
    let launch = launch_done(&world.state()).await;
    assert_eq!(
        launch.outcome,
        Some(LaunchOutcome::Abstained {
            reason: AbstainReason::InterruptedBeforeDecision,
        }),
        "the unconfirmed ask abstains, never retries"
    );
    let eval = effect_at(&world.store(), &format!("launch:{}:evaluate", launch.id.0));
    assert_eq!(eval.state, EffectState::Failed);
    assert_eq!(eval.certainty, Some(EffectCertainty::Unknown));
    assert!(world.jev().requests().is_empty(), "never asked at all");
    world.shutdown().await;
}

/// F12/H#41 — Jev sees the semantic Task and the question set only: no
/// operating point id, provider, catalog argument, requested tier,
/// label, cwd or project path in the request it receives.
#[tokio::test]
async fn f12_jev_never_sees_operating_points() {
    let mut world = World::new(
        &point("op-secret", 0, "vendor-secret", "--secret-arg"),
        "launch_wait_secs = 15\n",
    );
    world.jev().push_answers(launch_eval("new"));
    world.start().await;
    qualify_start(&mut world.store(), "op-secret", &["--secret-arg"]);

    let cwd = world.project().join("sub");
    std::fs::create_dir_all(&cwd).expect("sub dir");
    let extras = [
        ("tier", json!("standard")),
        ("label", json!("secret-label")),
        ("cwd", json!(cwd.to_str().expect("utf8"))),
    ];
    let reply = world.launch(&launch_args(&task(&extras), "k1")).await;
    assert_eq!(tool_body(&reply)["outcome"], "launched");

    let requests = world.jev().requests();
    let [request] = requests.as_slice() else {
        panic!("one evaluation: {requests:?}");
    };
    let wire = request.body.to_string();
    for secret in [
        "op-secret",
        "vendor-secret",
        "--secret-arg",
        "secret-label",
        world.project().to_str().expect("utf8"),
    ] {
        assert!(!wire.contains(secret), "Jev saw {secret}: {wire}");
    }
    let state = request.state().expect("a state member");
    let projected: std::collections::BTreeSet<&String> = state["task"]
        .as_object()
        .expect("the task state")
        .keys()
        .collect();
    assert_eq!(
        projected
            .into_iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["constraints", "doneWhen", "objective", "scope"],
        "the four semantic fields and nothing else: {state}"
    );
    assert_eq!(state["task"]["objective"], "land the green refactor");
    world.shutdown().await;
}

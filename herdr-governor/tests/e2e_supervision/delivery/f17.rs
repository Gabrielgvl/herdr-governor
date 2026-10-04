//! F17 — follow-up admission and the outbox lifecycle: dedup, refusals,
//! file publication, mid-turn gating, settlement expiry + the retention
//! sweep, and the S1-half restart leg (dispatch commit lost before the
//! wire write → `unconfirmed` + `follow_up_unconfirmed`).

use super::*;

/// F17 — `(run, messageKey)` dedups on the body digest: an identical
/// retry returns the existing `seq` (never a second row); a different
/// body under the same key refuses `MESSAGE_KEY_CONFLICT`.
#[tokio::test]
async fn f17_same_key_same_digest_returns_seq() {
    let mut run = run_row("r1", caller(1));
    run.state = State::Active;
    let fake = FakeHerdr::start(topology(&[("gov-r1", "kind-b", "sess-child-1", "idle")]));
    run.identity = Some(ChildIdentity {
        herdr_incarnation: HerdrIncarnation(socket_incarnation(fake.socket_path())),
        terminal_id: TerminalId(terminal_of(&fake.state().topology, "w1:p2")),
        agent_kind: AgentKind("kind-b".into()),
        agent_name: AgentName("gov-r1".into()),
        native_session: Some(NativeSession("sess-child-1".into())),
        pane_id: PaneId("w1:p2".into()),
    });
    let dirs = fixture(&catalog(&fake));
    bind_caller(&mut open_store(&dirs), 1, "w1:p1");
    seed_run(&mut open_store(&dirs), &run);
    let daemon = TestDaemon::start_in_process(&dirs.settings(), None).await;
    let client = run_client(&dirs, &daemon, "w1:p1");

    let first = message(&client, 1, "r1", "k", "same body").await;
    assert_eq!(first["result"]["isError"], false, "served: {first}");
    assert_eq!(tool_body(&first)["seq"], 1);
    let retry = message(&client, 2, "r1", "k", "same body").await;
    assert_eq!(
        tool_body(&retry)["seq"],
        1,
        "identical retry returns the existing seq: {retry}"
    );
    let changed = message(&client, 3, "r1", "k", "a different body").await;
    assert_eq!(tool_code(&changed), "MESSAGE_KEY_CONFLICT");
    let rows = outbox(&dirs, "r1");
    assert_eq!(rows.len(), 1, "one row across the retry: {rows:?}");

    daemon.shutdown().await;
}

/// F17 — settlement closes the queue: `RUN_SETTLED` for a settled Run;
/// `NOT_OWNER` for a Run the caller does not own and for a Run the store
/// has never heard of (it has no owner to match).
#[tokio::test]
async fn f17_run_settled_refused() {
    let settled = settled_run("r-settled");
    let mut foreign = run_row("r-foreign", caller(2));
    foreign.state = State::Active;
    let fake = FakeHerdr::start(topology(&[]));
    let dirs = fixture(&catalog(&fake));
    let mut store = open_store(&dirs);
    bind_caller(&mut store, 1, "w1:p1");
    bind_caller(&mut store, 2, "w1:p1");
    seed_run(&mut store, &settled);
    seed_run(&mut store, &foreign);
    let daemon = TestDaemon::start_in_process(&dirs.settings(), None).await;
    let client = run_client(&dirs, &daemon, "w1:p1");

    assert_eq!(
        tool_code(&message(&client, 1, "r-settled", "k", "x").await),
        "RUN_SETTLED"
    );
    assert_eq!(
        tool_code(&message(&client, 2, "r-foreign", "k", "x").await),
        "NOT_OWNER"
    );
    assert_eq!(
        tool_code(&message(&client, 3, "r-unknown", "k", "x").await),
        "NOT_OWNER"
    );

    daemon.shutdown().await;
}

/// F17 — a body over the 16 KiB inline bound publishes first:
/// `<state>/followups/<run>/<seq>.md` mode 0600 under a 0700 dir, the
/// row's `body_path`, and the wire carries only the `attached file:`
/// pointer (never the bytes).
#[tokio::test]
async fn f17_large_body_published_0600_fsync_rename_verified() {
    let mut run = run_row("r1", caller(1));
    run.state = State::Active;
    let fake = FakeHerdr::start(topology(&[("gov-r1", "kind-b", "sess-child-1", "idle")]));
    run.identity = Some(ChildIdentity {
        herdr_incarnation: HerdrIncarnation(socket_incarnation(fake.socket_path())),
        terminal_id: TerminalId(terminal_of(&fake.state().topology, "w1:p2")),
        agent_kind: AgentKind("kind-b".into()),
        agent_name: AgentName("gov-r1".into()),
        native_session: Some(NativeSession("sess-child-1".into())),
        pane_id: PaneId("w1:p2".into()),
    });
    let dirs = fixture(&catalog(&fake));
    bind_caller(&mut open_store(&dirs), 1, "w1:p1");
    seed_run(&mut open_store(&dirs), &run);
    let daemon = TestDaemon::start_in_process(&dirs.settings(), None).await;
    let client = run_client(&dirs, &daemon, "w1:p1");

    let body = "large-body-".repeat(2_000); // 22 KiB, over the inline bound.
    let reply = message(&client, 1, "r1", "k", &body).await;
    assert_eq!(reply["result"]["isError"], false, "served: {reply}");
    let published = dirs.state_dir().join("followups").join("r1").join("1.md");
    assert_eq!(
        std::fs::read_to_string(&published).expect("published body"),
        body,
        "the verified bytes land verbatim"
    );
    assert_eq!(
        std::fs::metadata(&published)
            .expect("file stat")
            .permissions()
            .mode()
            & 0o777,
        0o600,
        "the body file is 0600"
    );
    assert_eq!(
        std::fs::metadata(published.parent().expect("run dir"))
            .expect("dir stat")
            .permissions()
            .mode()
            & 0o777,
        0o700,
        "the followups/<run> dir is 0700"
    );
    let row = &outbox(&dirs, "r1")[0];
    let MessageBody::File { path } = &row.body else {
        panic!("a 22 KiB body publishes as File: {row:?}");
    };
    assert_eq!(Path::new(path), published.as_path());

    // The dispatch renders the pointer line — the bytes never cross the wire.
    await_for("file-pointer prompt", || {
        prompts_on(&fake, "w1:p2")
            .iter()
            .any(|(_, text)| text.contains("attached file:") && text.contains(path.as_str()))
    })
    .await;

    daemon.shutdown().await;
}

/// F17/S12 — publication fails closed: with `followups/r1` impossible
/// the 20 KiB body earns `FOLLOWUP_PUBLISH_FAILED` and nothing is
/// enqueued (no row, no file, no `.tmp` debris).
#[tokio::test]
async fn f17_publication_failure_enqueues_nothing() {
    let mut run = run_row("r1", caller(1));
    run.state = State::Active;
    let fake = FakeHerdr::start(topology(&[("gov-r1", "kind-b", "sess-child-1", "idle")]));
    run.identity = Some(ChildIdentity {
        herdr_incarnation: HerdrIncarnation(socket_incarnation(fake.socket_path())),
        terminal_id: TerminalId(terminal_of(&fake.state().topology, "w1:p2")),
        agent_kind: AgentKind("kind-b".into()),
        agent_name: AgentName("gov-r1".into()),
        native_session: Some(NativeSession("sess-child-1".into())),
        pane_id: PaneId("w1:p2".into()),
    });
    let dirs = fixture(&catalog(&fake));
    // A file where `publish_body` wants `followups/r1/` — `create_dir_all`
    // fails `ENOTDIR` no matter who asks (startup re-pins `0700` on the
    // tree root, so a chmod injection would not survive `Paths::create`).
    let followups = dirs.state_dir().join("followups");
    std::fs::create_dir_all(&followups).expect("followups dir");
    std::fs::write(followups.join("r1"), "not a dir").expect("plant file");
    bind_caller(&mut open_store(&dirs), 1, "w1:p1");
    seed_run(&mut open_store(&dirs), &run);
    let daemon = TestDaemon::start_in_process(&dirs.settings(), None).await;
    let client = run_client(&dirs, &daemon, "w1:p1");

    let body = "b".repeat(20 * 1024);
    let reply = message(&client, 1, "r1", "k", &body).await;
    assert_eq!(tool_code(&reply), "FOLLOWUP_PUBLISH_FAILED");
    assert!(
        outbox(&dirs, "r1").is_empty(),
        "a failed publication enqueues nothing"
    );
    assert_eq!(
        std::fs::read_dir(&followups).expect("list").count(),
        1,
        "no body file or .tmp debris was left behind"
    );

    daemon.shutdown().await;
}

/// F17 — `mid_turn_input` (a current F26 pass on the Run's point) sends
/// while the child reads `working`; without it the head waits for
/// `idle`/`done`. Asserted through the wire: the qualified Run's prompt
/// lands first, the unqualified one's only after the pane goes idle.
#[tokio::test]
async fn f17_mid_turn_qualified_sends_immediately_else_at_idle() {
    let fake = FakeHerdr::start(topology(&[
        ("gov-r1", "kind-b", "sess-child-1", "working"),
        ("gov-r2", "kind-b", "sess-child-2", "working"),
    ]));
    let mut qualified = run_row("r1", caller(1));
    qualified.state = State::Active;
    qualified.operating_point = Some(OperatingPointId("pt-a".into()));
    qualified.identity = Some(ChildIdentity {
        herdr_incarnation: HerdrIncarnation(socket_incarnation(fake.socket_path())),
        terminal_id: TerminalId(terminal_of(&fake.state().topology, "w1:p2")),
        agent_kind: AgentKind("kind-b".into()),
        agent_name: AgentName("gov-r1".into()),
        native_session: Some(NativeSession("sess-child-1".into())),
        pane_id: PaneId("w1:p2".into()),
    });
    let mut unqualified = run_row("r2", caller(1));
    unqualified.state = State::Active;
    unqualified.identity = Some(ChildIdentity {
        herdr_incarnation: HerdrIncarnation(socket_incarnation(fake.socket_path())),
        terminal_id: TerminalId(terminal_of(&fake.state().topology, "w1:p3")),
        agent_kind: AgentKind("kind-b".into()),
        agent_name: AgentName("gov-r2".into()),
        native_session: Some(NativeSession("sess-child-2".into())),
        pane_id: PaneId("w1:p3".into()),
    });
    let dirs = fixture(&catalog(&fake));
    let mut store = open_store(&dirs);
    bind_caller(&mut store, 1, "w1:p1");
    seed_run(&mut store, &qualified);
    seed_run(&mut store, &unqualified);
    qualify(&mut store, "pt-a", &["--cap"], "mid_turn_input");
    let daemon = TestDaemon::start_in_process(&dirs.settings(), None).await;
    let client = run_client(&dirs, &daemon, "w1:p1");

    assert_eq!(
        message(&client, 1, "r1", "k", "send now").await["result"]["isError"],
        false
    );
    assert_eq!(
        message(&client, 2, "r2", "k", "wait for idle").await["result"]["isError"],
        false
    );

    // The qualified Run's prompt lands while its child reads `working` —
    // proved submitted, not merely written to the pane.
    await_for("qualified follow-up submitted", || {
        outbox(&dirs, "r1")
            .first()
            .is_some_and(|row| row.state == OutboxState::Submitted)
    })
    .await;
    assert!(
        prompts_on(&fake, "w1:p3").is_empty(),
        "the unqualified head waited: {:?}",
        prompts(&fake)
    );

    fake.set_agent_status("w1:p3", "idle");
    await_for("unqualified follow-up at idle", || {
        prompts_on(&fake, "w1:p3")
            .iter()
            .any(|(_, text)| text.contains("wait for idle"))
    })
    .await;

    daemon.shutdown().await;
}

/// F17/F20 — settlement expires the still-`queued` entries only, emits
/// `follow_up_expired` (same transaction, dedup'd by seq), and the
/// retention sweep then reaps the expired row's published file plus any
/// `.tmp`/unreferenced debris under `followups/<run>/`. The `submitted`
/// row and — per `follow_up_file_retained` — any live row's file stay.
#[tokio::test]
async fn f17_settle_expires_only_queued() {
    let fake = FakeHerdr::start(topology(&[("gov-r1", "kind-b", "sess-child-1", "idle")]));
    let mut run = run_row("r1", caller(1));
    run.state = State::Active;
    run.max_age_deadline = PAST; // the deadline sweep settles it on tick one
    run.identity = Some(ChildIdentity {
        herdr_incarnation: HerdrIncarnation(socket_incarnation(fake.socket_path())),
        terminal_id: TerminalId(terminal_of(&fake.state().topology, "w1:p2")),
        agent_kind: AgentKind("kind-b".into()),
        agent_name: AgentName("gov-r1".into()),
        native_session: Some(NativeSession("sess-child-1".into())),
        pane_id: PaneId("w1:p2".into()),
    });
    let dirs = fixture(&catalog(&fake));
    let mut store = open_store(&dirs);
    bind_caller(&mut store, 1, "w1:p1");
    seed_run(&mut store, &run);
    // seq 1: queued with a published file; seq 2: submitted (terminal effect).
    let run_dir = dirs.state_dir().join("followups").join("r1");
    std::fs::create_dir_all(&run_dir).expect("run followups dir");
    let body_path = run_dir.join("1.md");
    std::fs::write(&body_path, "published").expect("body file");
    std::fs::write(run_dir.join("2.md.tmp"), "partial").expect("tmp debris");
    std::fs::write(run_dir.join("99.md"), "orphan").expect("unreferenced file");
    seed_outbox_pair(&mut store, &run.id, 1, Some(body_path.clone()));
    let daemon = TestDaemon::start_in_process(&dirs.settings(), None).await;

    await_for("the settle + expiry", || {
        let rows = outbox(&dirs, "r1");
        rows.len() == 2
            && rows[0].state == OutboxState::Expired
            && rows[1].state == OutboxState::Submitted
    })
    .await;
    let rows = outbox(&dirs, "r1");
    assert_eq!(rows[0].expiry_reason, Some(ExpiryReason::RunSettled));
    assert!(
        unacked(&dirs, 1)
            .iter()
            .any(|event| event.kind == MailboxEventKind::FollowUpExpired
                && event.dedup_key.0 == "run:r1:follow_up_expired:1"),
        "follow_up_expired composed into the settle apply: {:?}",
        unacked(&dirs, 1)
    );

    // The retention sweep: the expired row's file, the `.tmp` and the
    // unreferenced `99.md` all reap; `followups/` itself stays.
    await_for("the sweep", || {
        !body_path.exists() && !run_dir.join("2.md.tmp").exists() && !run_dir.join("99.md").exists()
    })
    .await;

    daemon.shutdown().await;
}

/// F17/S1-half — a daemon killed between the outbox dispatch commit and
/// the wire write leaves `dispatching`; the next start's §4.3 step-5
/// marking resolves the entry `unconfirmed` and owes the owner a
/// `follow_up_unconfirmed` event (the C2 half — S3's transcript lift is
/// C5). Asserted end to end: the event is minted *and* its hint reaches
/// the owner's pane.
#[tokio::test]
async fn f17_restart_after_follow_up_dispatch_commit_resolves_outbox_unconfirmed_and_notifies() {
    let fake = FakeHerdr::start(topology(&[("gov-r1", "kind-b", "sess-child-1", "idle")]));
    let mut run = run_row("r1", caller(1));
    run.state = State::Active;
    run.identity = Some(ChildIdentity {
        herdr_incarnation: HerdrIncarnation(socket_incarnation(fake.socket_path())),
        terminal_id: TerminalId(terminal_of(&fake.state().topology, "w1:p2")),
        agent_kind: AgentKind("kind-b".into()),
        agent_name: AgentName("gov-r1".into()),
        native_session: Some(NativeSession("sess-child-1".into())),
        pane_id: PaneId("w1:p2".into()),
    });
    let dirs = fixture(&catalog(&fake));
    let mut store = open_store(&dirs);
    bind_caller(&mut store, 1, "w1:p1");
    seed_run(&mut store, &run);
    qualify(&mut store, "pt-a", &["--cap"], "hint_consumption");

    let seam = SeamConfig {
        suffix: "outbox:*".to_owned(),
        boundary: Boundary::DispatchCommitted,
        action: SeamAction::Abort,
    };
    let daemon = TestDaemon::spawn_child(&dirs.settings(), Some(seam)).await;
    let client = run_client(&dirs, &daemon, "w1:p1");
    assert_eq!(
        message(&client, 1, "r1", "k", "lost on the floor").await["result"]["isError"],
        false
    );
    let (status, stderr) = daemon.wait().await;
    assert!(!status.success(), "the seam aborts the child: {stderr}");
    assert!(
        stderr.contains("seam hit outbox:*@dispatch_committed"),
        "the kill landed at the commit boundary: {stderr}"
    );

    let restarted = TestDaemon::spawn_child(&dirs.settings(), None).await;
    await_for("the restart resolution", || {
        outbox(&dirs, "r1")
            .first()
            .is_some_and(|row| row.state == OutboxState::Unconfirmed)
            && unacked(&dirs, 1)
                .iter()
                .any(|event| event.kind == MailboxEventKind::FollowUpUnconfirmed)
    })
    .await;
    // The wire never saw the body — the kill landed between the journal's
    // dispatch commit and the write (any nudge the reconcile pass emitted
    // to the same pane is its own effect, not this one).
    assert!(
        prompts_on(&fake, "w1:p2")
            .iter()
            .all(|(_, text)| !text.contains("lost on the floor")),
        "killed between commit and write: {:?}",
        prompts(&fake)
    );
    // …and the owner is told — the `follow_up_unconfirmed` hint lands on
    // the owner's own pane (the subject-independent path, F18).
    await_for("the unconfirmed hint", || {
        prompts_on(&fake, "w1:p1")
            .iter()
            .any(|(_, text)| text.contains("follow_up_unconfirmed event"))
    })
    .await;

    restarted.shutdown().await;
}

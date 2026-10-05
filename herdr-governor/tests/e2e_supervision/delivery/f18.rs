//! F18 — the mailbox events and the subject-independent hints: dedup,
//! owner re-resolution across adoption, the once-per-event /
//! once-per-5s journal rule (S25), `handoff_accepted` to a settled Run's
//! owner, and `launch_answered` to the Launch's caller.

use super::*;

/// F18 — `dedup_key` makes a repeat emission a no-op: two emissions of
/// the same `handoff_accepted` for one Run land exactly one row.
#[tokio::test]
async fn f18_events_dedup_on_repeat() {
    let fake = FakeHerdr::start(topology(&[]));
    let dirs = fixture(&catalog(&fake));
    let mut store = open_store(&dirs);
    bind_caller(&mut store, 1, "w1:p1");
    seed_run(&mut store, &settled_run("r1"));
    let subject = MailboxSubject::Run(RunId("r1".into()));
    emit(
        &mut store,
        mailbox_event(
            "evt:h-a",
            subject.clone(),
            MailboxEventKind::HandoffAccepted,
            None,
        ),
    );
    emit(
        &mut store,
        mailbox_event("evt:h-b", subject, MailboxEventKind::HandoffAccepted, None),
    );
    let daemon = TestDaemon::start_in_process(&dirs.settings(), None).await;

    await_for("the single dedup'd row", || {
        let events = unacked(&dirs, 1);
        events
            .iter()
            .filter(|event| event.kind == MailboxEventKind::HandoffAccepted)
            .count()
            == 1
    })
    .await;
    assert_eq!(
        unacked(&dirs, 1).len(),
        1,
        "the repeat never landed: {:?}",
        unacked(&dirs, 1)
    );

    daemon.shutdown().await;
}

/// F18 — the destination owner is re-derived, never captured: an event
/// emitted under one owner reaches the *successor* after `ChangeOwner`
/// (H#84/H#91 — adoption redirects unread events, hints included).
#[tokio::test]
async fn f18_destination_follows_adoption() {
    let fake = FakeHerdr::start(topology(&[
        ("successor-b", "kind-a", "sess-caller-2", "idle"),
        ("gov-r1", "kind-b", "sess-child-1", "idle"),
    ]));
    let dirs = fixture(&catalog(&fake));
    let mut store = open_store(&dirs);
    bind_caller(&mut store, 1, "w1:p1");
    bind_caller(&mut store, 2, "w1:p2");
    let mut run = run_row("r1", caller(1));
    run.state = State::Active;
    seed_run(&mut store, &run);
    qualify(&mut store, "pt-a", &["--cap"], "hint_consumption");
    emit(
        &mut store,
        mailbox_event(
            "evt:prompt-unconf",
            MailboxSubject::Run(RunId("r1".into())),
            MailboxEventKind::PromptUnconfirmed,
            None,
        ),
    );
    let daemon = TestDaemon::start_in_process(&dirs.settings(), None).await;

    // The owner moves *after* the event was minted — the successor's
    // pane is where the hint goes.
    seed(
        &mut store,
        vec![StateChange::ChangeOwner(OwnerChange {
            run: RunId("r1".into()),
            expected_owner: caller(1),
            owner: caller(2),
        })],
        Vec::new(),
    );
    await_for("the successor's hint", || {
        prompts_on(&fake, "w1:p2")
            .iter()
            .any(|(_, text)| text.contains("prompt_unconfirmed event"))
    })
    .await;
    assert!(
        prompts_on(&fake, "w1:p1").is_empty(),
        "the previous owner's pane gets nothing: {:?}",
        prompts(&fake)
    );
    assert_eq!(
        unacked(&dirs, 2).len(),
        1,
        "the mailbox read re-derives the owner too: {:?}",
        unacked(&dirs, 2)
    );

    daemon.shutdown().await;
}

/// F18/S25 — one hint per owner per `HINT_MIN_INTERVAL` (5 s), journaled
/// once per event: two events yield two hints ~5 s apart (measured, not
/// slept), and a hint whose ack is lost is *never* re-sent — the
/// `event:<id>:hint` key is terminal, whatever the outcome.
#[tokio::test]
async fn f18_hint_only_idle_qualified_owner_pane_once_per_5s() {
    let fake = FakeHerdr::start(topology(&[("gov-r1", "kind-b", "sess-child-1", "idle")]));
    let mut catalog = catalog(&fake);
    catalog.daemon_extra = OP_TIMEOUT.to_owned();
    let dirs = fixture(&catalog);
    let mut store = open_store(&dirs);
    bind_caller(&mut store, 1, "w1:p1");
    seed_run(&mut store, &settled_run("r1"));
    qualify(&mut store, "pt-a", &["--cap"], "hint_consumption");
    let subject = MailboxSubject::Run(RunId("r1".into()));
    emit(
        &mut store,
        mailbox_event(
            "evt:rate-1",
            subject.clone(),
            MailboxEventKind::PromptUnconfirmed,
            None,
        ),
    );
    emit(
        &mut store,
        mailbox_event(
            "evt:rate-2",
            subject.clone(),
            MailboxEventKind::BlockedOnInput,
            Some(7),
        ),
    );
    let daemon = TestDaemon::start_in_process(&dirs.settings(), None).await;

    await_for("the first hint", || !prompts_on(&fake, "w1:p1").is_empty()).await;
    let first_at = Instant::now();
    await_for("the second hint — the interval later", || {
        prompts_on(&fake, "w1:p1").len() >= 2
    })
    .await;
    let gap = first_at.elapsed();
    assert!(
        gap >= Duration::from_secs(4),
        "the second hint waited the 5 s interval, got {gap:?}"
    );

    // A lost ack: the write lands (the fake applies it), the reply never
    // does — `failed` with `unknown` certainty, journaled once, never
    // re-sent.
    fake.fault(
        "agent.prompt",
        crate::support::fake_herdr::Fault::DropResponse,
    );
    emit(
        &mut store,
        mailbox_event(
            "evt:rate-3",
            subject,
            MailboxEventKind::OutsideScope,
            Some(2),
        ),
    );
    await_for("the dropped-ack hint", || {
        prompts_on(&fake, "w1:p1").len() >= 3
    })
    .await;
    await_for("the unknown-certainty result commits", || {
        journal(&dirs, "r1").iter().any(|effect| {
            effect.key.0 == "event:evt:rate-3:hint" && effect.state == EffectState::Failed
        })
    })
    .await;
    // Past the interval and the result: still exactly three prompts —
    // a fourth would be a retry, and hints never retry.
    await_for("the hint effects settle", || {
        journal(&dirs, "r1")
            .iter()
            .filter(|effect| effect.key.0.ends_with(":hint"))
            .all(|effect| {
                matches!(
                    effect.state,
                    EffectState::Acknowledged | EffectState::Unconfirmed | EffectState::Failed
                )
            })
            && journal(&dirs, "r1")
                .iter()
                .filter(|effect| effect.key.0.ends_with(":hint"))
                .count()
                == 3
    })
    .await;
    assert_eq!(
        prompts_on(&fake, "w1:p1").len(),
        3,
        "one prompt per event — nothing re-sent: {:?}",
        prompts(&fake)
    );

    daemon.shutdown().await;
}

/// F18 — `handoff_accepted`'s subject is the Run, but the destination is
/// its *owner*: the hint still reaches the owner pane after the Run has
/// settled (the subject-independent rule — a settled subject cannot
/// dispatch to a child but still owes the owner the notice).
#[tokio::test]
async fn f18_handoff_accepted_hint_reaches_owner_after_settlement() {
    let fake = FakeHerdr::start(topology(&[("gov-r1", "kind-b", "sess-child-1", "idle")]));
    let dirs = fixture(&catalog(&fake));
    let mut store = open_store(&dirs);
    bind_caller(&mut store, 1, "w1:p1");
    seed_run(&mut store, &settled_run("r1"));
    qualify(&mut store, "pt-a", &["--cap"], "hint_consumption");
    emit(
        &mut store,
        mailbox_event(
            "evt:handoff",
            MailboxSubject::Run(RunId("r1".into())),
            MailboxEventKind::HandoffAccepted,
            None,
        ),
    );
    let daemon = TestDaemon::start_in_process(&dirs.settings(), None).await;

    await_for("the handoff hint", || {
        prompts_on(&fake, "w1:p1")
            .iter()
            .any(|(_, text)| text.contains("handoff_accepted event"))
    })
    .await;

    daemon.shutdown().await;
}

/// F18 — a launch-bound event's destination is the Launch's *caller*
/// (the subject-independent rule's second arm): `launch_answered` on a
/// `done` Launch hints the caller's pane.
#[tokio::test]
async fn f18_launch_answered_hint_dispatches_for_done_launch() {
    let fake = FakeHerdr::start(topology(&[]));
    let dirs = fixture(&catalog(&fake));
    let mut store = open_store(&dirs);
    bind_caller(&mut store, 1, "w1:p1");
    // `done` is an update phase (P4.0): the admission row lands
    // `evaluating` first, then the terminal write.
    let mut launch = launch_row("l1", &caller(1), LaunchPhase::Done);
    launch.outcome = Some(LaunchOutcome::Rejected);
    seed(
        &mut store,
        vec![
            StateChange::RecordLaunch(launch_row("l1", &caller(1), LaunchPhase::Evaluating)),
            StateChange::RecordLaunch(launch),
        ],
        Vec::new(),
    );
    qualify(&mut store, "pt-a", &["--cap"], "hint_consumption");
    emit(
        &mut store,
        mailbox_event(
            "evt:answered",
            MailboxSubject::Launch(LaunchId("l1".into())),
            MailboxEventKind::LaunchAnswered,
            None,
        ),
    );
    let daemon = TestDaemon::start_in_process(&dirs.settings(), None).await;

    await_for("the answered hint", || {
        prompts_on(&fake, "w1:p1")
            .iter()
            .any(|(_, text)| text.contains("launch_answered event"))
    })
    .await;

    daemon.shutdown().await;
}

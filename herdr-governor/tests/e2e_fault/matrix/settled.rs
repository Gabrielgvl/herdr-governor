// — the §4.5 settled-before-prompting row under a restart ————————————

use super::*;

/// S1's settled-before-prompting arm: a `launching` launch whose
/// `starting` Run settled before any prompt was planned converges
/// `Failed` at restart — `absent` certainty (the acknowledged tab is
/// the only launch leg), `createdTopology` reporting the journaled
/// `TabCreated` receipt, the caller answered exactly once. A sibling
/// `dispatching` row is what restart's global scan eats; here every
/// row is already terminal, so the outcome is `absent`, not `unknown`.
#[tokio::test]
async fn s1_settled_run_launch_converges_failed() {
    let mut world = World::build(caller_topology(), |catalog| {
        catalog.points_toml = point("op-a", 0, "vendor-a", "--a");
    });
    {
        let mut store = world.store();
        bind_caller(&mut store);
        let run = run_row("r-sp", "l-sp", State::Starting);
        seed_run(&mut store, &run, LaunchPhase::Launching);
        seed_acknowledged(
            &mut store,
            &run_effect(
                "run:r-sp:tab",
                EffectKind::TabCreate,
                "r-sp",
                Some(&EffectTarget::CallerContext(PaneId("w1:p1".into()))),
            ),
            EffectReceipt::TabCreated {
                tab: TabId("w1:t9".into()),
                pane: PaneId("w1:p9".into()),
            },
        );
        let stored = read_run(&store, "r-sp");
        store
            .apply(&settle(&stored, Settlement::Cancelled, NOW, &policy()), NOW)
            .expect("settle");
    }

    world.start().await;
    let launch = finished_launch(&world).await;
    let Some(LaunchOutcome::Failed {
        certainty,
        run: settled_id,
        created_topology,
    }) = &launch.outcome
    else {
        panic!(
            "the settled run's launch converges failed: {:?}",
            launch.outcome
        );
    };
    assert_eq!(*certainty, EffectCertainty::Absent);
    assert_eq!(settled_id.as_ref().map(|id| id.0.as_str()), Some("r-sp"));
    assert_eq!(
        created_topology.tab.as_ref().map(|tab| tab.0.as_str()),
        Some("w1:t9"),
        "the acknowledged tab reports in createdTopology"
    );
    assert_eq!(created_topology.panes.len(), 1);
    let store = world.store();
    assert_eq!(
        caller_events(&store, MailboxEventKind::LaunchAnswered).len(),
        1,
        "the launch's caller is answered exactly once"
    );
    drop(store);
    world.shutdown().await;
}

//! The world assembly: random bijective renamings of the catalog's harness
//! kinds and operating-point ids, and `world` — the complete metamorphic
//! case drawing the journal first so the event's effect keys correlate.

use std::collections::BTreeMap;

use governor_core::acceptance::FrozenHandoff;
use governor_core::config::{Capability, Config, Provider};
use governor_core::identity::Timestamp;
use governor_core::lifecycle::{Run, Settlement, State};
use proptest::collection::vec as prop_vec;
use proptest::option::of as opt_of;
use proptest::prelude::{Just, Strategy as _, any};
use proptest::strategy::BoxedStrategy;

use super::input_strategies::{
    CAPABILITY_NAMES, PROVIDER_NAMES, any_digest, config, evaluation, launch, named, pick,
    qualifications,
};
use super::run_strategies::{base_run, event, journal_effect, run};
use super::{Rename, World};

// A salt orders a random permutation per element; a flag picks the image —
// permuted old name or fresh name. Bijective either way.
fn images(domain: Vec<String>, prefix: &'static str) -> BoxedStrategy<Vec<String>> {
    prop_vec((any::<u64>(), any::<bool>()), domain.len())
        .prop_map(move |marks| {
            let mut order = Vec::from_iter(0..domain.len());
            order.sort_by_key(|index| marks.get(*index).map_or(0, |(salt, _)| *salt));
            Vec::from_iter(domain.iter().enumerate().map(|(index, name)| {
                let fresh = marks.get(index).is_some_and(|(_, fresh)| *fresh);
                match (fresh, order.get(index)) {
                    (true, _) => format!("{prefix}-{index}"),
                    (false, Some(position)) => domain
                        .get(*position)
                        .map_or_else(|| name.clone(), String::clone),
                    (false, None) => name.clone(),
                }
            }))
        })
        .boxed()
}

fn rename(config: &Config) -> BoxedStrategy<Rename> {
    let mut kinds = Vec::from_iter(
        config
            .catalog
            .operating_points
            .iter()
            .map(|point| point.harness.0.clone()),
    );
    kinds.sort();
    kinds.dedup();
    let ops = Vec::from_iter(
        config
            .catalog
            .operating_points
            .iter()
            .map(|p| p.id.0.clone()),
    );
    (images(kinds.clone(), "rk"), images(ops.clone(), "ro"))
        .prop_map(move |(kind_images, op_images)| Rename {
            kinds: BTreeMap::from_iter(kinds.iter().cloned().zip(kind_images)),
            ops: BTreeMap::from_iter(ops.iter().cloned().zip(op_images)),
        })
        .boxed()
}

/// One metamorphic case: the generated world plus its renaming. The journal
/// draws first so the event's effect keys correlate with it.
pub fn world() -> BoxedStrategy<World> {
    config()
        .prop_flat_map(|config| {
            run(&config).prop_flat_map(move |run| {
                let cfg = config.clone();
                prop_vec(journal_effect(&run, &config), 0..=4).prop_flat_map(move |journal| {
                    let run_id = run.id.clone();
                    let predecessor = opt_of((
                        pick(&cfg.catalog.operating_points),
                        opt_of(pick(&cfg.policy.tiers)),
                    ))
                    .prop_map(|pair| {
                        pair.map(|(point, tier)| Run {
                            provider: Some(point.provider.clone()),
                            operating_point: Some(point.id.clone()),
                            tier_start: tier,
                            settlement: Some(Settlement::ProviderLimited),
                            settled_at: Some(Timestamp(1_000)),
                            ..base_run(State::Settled)
                        })
                    });
                    (
                        (
                            Just(cfg.clone()),
                            Just(run.clone()),
                            Just(journal.clone()),
                            prop_vec((0u64..=3, any_digest()), 0..=2).prop_map(move |rows| {
                                rows.into_iter()
                                    .map(|(work_generation, digest)| FrozenHandoff {
                                        run: run_id.clone(),
                                        work_generation,
                                        digest,
                                        frozen_path: String::from("/state/handoffs/a"),
                                        frozen_at: Timestamp(100),
                                    })
                                    .collect()
                            }),
                            event(&run, &journal, &cfg),
                            (0i64..=3_600_000).prop_map(Timestamp),
                            pick(&["/state/h/a", "/state/h/b"]).prop_map(String::from),
                            any::<bool>(),
                        ),
                        (
                            launch(&cfg),
                            predecessor,
                            evaluation(&cfg),
                            prop_vec(named(CAPABILITY_NAMES, Capability), 0..=3),
                            qualifications(&cfg),
                            prop_vec(named(PROVIDER_NAMES, Provider), 0..=2),
                            rename(&cfg),
                        ),
                    )
                })
            })
        })
        .prop_map(
            |(
                (config, run, journal, handoffs, event, now, freeze_path, owner_absent),
                (launch, predecessor, evaluation, required, qualifications, cooling, rename),
            )| World {
                config,
                launch,
                predecessor,
                evaluation,
                required,
                qualifications,
                cooling,
                run,
                journal,
                handoffs,
                event,
                now,
                freeze_path,
                owner_absent,
                rename,
            },
        )
        .boxed()
}

#[cfg(test)]
mod self_check {
    use crate::strategies::{World, world};
    use proptest::strategy::{Strategy as _, ValueTree as _};
    use proptest::test_runner::TestRunner;

    // This file also compiles as its own test crate: generate a world to
    // keep the helpers used and pin the renaming's bijection.
    #[test]
    fn world_renaming_is_bijective_on_the_domain() {
        let mut runner = TestRunner::default();
        let world: World = world()
            .new_tree(&mut runner)
            .expect("the world strategy must produce a value")
            .current();
        let mut images: Vec<&String> = world.rename.kinds.values().collect();
        images.sort();
        images.dedup();
        assert_eq!(
            images.len(),
            world.rename.kinds.len(),
            "the renaming is a bijection on the declared harness kinds"
        );
    }
}

//! The `route`-side inputs: shared name pools, primitive helpers, the
//! arbitrary `Config` (catalog + policy), `Launch`, `Evaluation` and
//! `Qualification` rows, and the `ChildIdentity` drawn from the catalog's
//! harness kinds.

use std::time::Duration;

use governor_core::config::{
    Capability, Catalog, Config, ConfigVersion, CostClass, OperatingPoint, OperatingPointId,
    Policy, Provider, Qualification, Tier,
};
use governor_core::identity::{
    AgentKind, AgentName, CallerKey, ChildIdentity, Digest, HerdrIncarnation, IdempotencyKey,
    LaunchId, NativeSession, PaneId, ProjectRoot, RunId, TabId, TerminalId, Timestamp,
};
use governor_core::routing::{ChangesFiles, Evaluation, Probability, TabChoice};
use governor_core::task::{Launch, LaunchPhase, Task};
use proptest::collection::vec as prop_vec;
use proptest::option::of as opt_of;
use proptest::prelude::{Just, Strategy, any, prop_oneof};
use proptest::sample::select;
use proptest::strategy::BoxedStrategy;

// Real harness names appear because the property proves they carry no
// special behaviour (I9 keeps them out of src; tests/ may name them).
const HARNESS_NAMES: &[&str] = &[
    "claude", "codex", "gemini", "devin", "alpha", "beta", "hook",
];
pub(crate) const PROVIDER_NAMES: &[&str] = &["pv-east", "pv-west", "pv-north"];
const CALLER_KINDS: &[&str] = &["caller-alpha", "caller-beta"];
pub(crate) const CAPABILITY_NAMES: &[&str] = &[
    Capability::START,
    Capability::PROMPT_ACK,
    Capability::HANDOFF_WRITE,
    Capability::FOLLOWUP_READ,
    Capability::MID_TURN_INPUT,
    Capability::HINT_CONSUMPTION,
    "cap-extra",
    "cap-side",
];

pub(crate) fn pick<T: Clone + std::fmt::Debug + 'static>(
    pool: &[T],
) -> impl Strategy<Value = T> + use<T> {
    select(Vec::from(pool))
}
pub(crate) fn named<T: std::fmt::Debug + 'static>(
    pool: &[&'static str],
    wrap: fn(String) -> T,
) -> impl Strategy<Value = T> + use<T> {
    select(Vec::from(pool)).prop_map(move |name| wrap(String::from(name)))
}
pub(crate) fn any_session() -> impl Strategy<Value = NativeSession> {
    named(&["sess-0", "sess-1", "sess-2"], NativeSession)
}
pub(crate) fn any_pane() -> impl Strategy<Value = PaneId> {
    named(&["w0:p0", "w0:p1", "w1:p0"], PaneId)
}
pub(crate) fn any_tab() -> impl Strategy<Value = TabId> {
    named(&["w0:t0", "w0:t1"], TabId)
}
pub(crate) fn any_digest() -> impl Strategy<Value = Digest> {
    pick(&[1u8, 2, 3]).prop_map(|tag| Digest([tag; 32]))
}
pub(crate) fn opt_timestamp() -> impl Strategy<Value = Option<Timestamp>> {
    opt_of((0i64..=3_600_000).prop_map(Timestamp))
}
pub(crate) fn probability() -> impl Strategy<Value = Probability> {
    (0.0f64..=1.0).prop_map(Probability)
}
pub(crate) fn child_identity(config: &Config) -> BoxedStrategy<ChildIdentity> {
    let kinds = Vec::from_iter(
        config
            .catalog
            .operating_points
            .iter()
            .map(|p| p.harness.clone()),
    );
    (
        prop_oneof![4 => select(kinds), 1 => named(HARNESS_NAMES, AgentKind)],
        opt_of(any_session()),
        any_pane(),
    )
        .prop_map(|(kind, session, pane)| ChildIdentity {
            herdr_incarnation: HerdrIncarnation("inc-1".into()),
            terminal_id: TerminalId("term-1".into()),
            agent_kind: kind,
            agent_name: AgentName("gov-r0".into()),
            native_session: session,
            pane_id: pane,
        })
        .boxed()
}
pub(crate) fn config() -> BoxedStrategy<Config> {
    let point_params = |tiers: &Vec<Tier>| {
        (
            named(HARNESS_NAMES, AgentKind),
            prop_vec(
                pick(&["--fast", "--safe", "--verbose"]).prop_map(String::from),
                0..=3,
            ),
            pick(tiers),
            prop_vec(named(CAPABILITY_NAMES, Capability), 0..=4),
            (0u32..=3).prop_map(CostClass),
            named(PROVIDER_NAMES, Provider),
        )
    };
    (1usize..=4)
        .prop_flat_map(move |count| {
            let tiers = Vec::from_iter((0..count).map(|i| Tier(format!("t{i}"))));
            (
                prop_vec(point_params(&tiers), 1..=6),
                (
                    (
                        opt_of(pick(&tiers)),
                        opt_of(pick(&tiers)),
                        opt_of(pick(&tiers)),
                        0.0f64..=1.0,
                        0.0f64..=1.0,
                    ),
                    (
                        (1u64..=86_400_000).prop_map(Duration::from_millis),
                        (1u64..=86_400_000).prop_map(Duration::from_millis),
                        (1u64..=86_400_000).prop_map(Duration::from_millis),
                        (1u64..=86_400_000).prop_map(Duration::from_millis),
                        (1u64..=86_400_000).prop_map(Duration::from_millis),
                        (1u64..=86_400_000).prop_map(Duration::from_millis),
                    ),
                )
                    .prop_map(move |(first, second)| {
                        let (cap, security, broad, threshold, rate) = first;
                        let (expiry, cool, age, repair, judge, idle) = second;
                        Policy {
                            tiers: tiers.clone(),
                            no_change_cap: cap,
                            security_floor: security,
                            broad_change_floor: broad,
                            provider_limit_threshold: threshold,
                            exploration_rate: rate,
                            recovery_expiry: expiry,
                            cooldown: cool,
                            max_age: age,
                            repair_window: repair,
                            judgment_window: judge,
                            idle_window: idle,
                        }
                    }),
            )
        })
        .prop_map(|(params, policy)| Config {
            version: ConfigVersion("cfg-1".into()),
            catalog: Catalog {
                operating_points: params
                    .into_iter()
                    .enumerate()
                    .map(
                        |(i, (harness, args, tier, capabilities, cost_class, provider))| {
                            OperatingPoint {
                                id: OperatingPointId(format!("op-{i}")),
                                harness,
                                args,
                                tier,
                                capabilities,
                                cost_class,
                                provider,
                            }
                        },
                    )
                    .collect(),
            },
            policy,
        })
        .boxed()
}
pub(crate) fn launch(config: &Config) -> BoxedStrategy<Launch> {
    (
        named(CALLER_KINDS, AgentKind).prop_map(|agent_kind| CallerKey {
            agent_kind,
            native_session: NativeSession("sess-c".into()),
        }),
        (
            opt_of(prop_oneof![
                4 => pick(&config.policy.tiers),
                1 => Just(Tier("tier-elsewhere".into())),
            ]),
            opt_of(named(&["run-0", "run-1"], RunId)),
            prop_vec(
                pick(&["it works", "lint green", "handoff written"]).prop_map(String::from),
                1..=3,
            ),
            prop_vec(
                pick(&["no api changes", "small diff"]).prop_map(String::from),
                0..=2,
            ),
        )
            .prop_map(|(tier, recovery_of, done_when, constraints)| Task {
                objective: "do the thing".into(),
                scope: "the repo".into(),
                done_when,
                constraints,
                tier,
                recovery_of,
                label: None,
                cwd: None,
            }),
        named(&["ik-0", "ik-1", "ik-2", "ik-3"], IdempotencyKey),
    )
        .prop_map(|(caller, work, key)| Launch {
            id: LaunchId("launch-0".into()),
            caller,
            project_root: ProjectRoot("/proj".into()),
            idempotency_key: key,
            digest_version: Task::DIGEST_VERSION,
            task_digest: work.digest(),
            task: work,
            phase: LaunchPhase::Evaluating,
            decision: None,
            config_version: None,
            outcome: None,
        })
        .boxed()
}
pub(crate) fn evaluation(config: &Config) -> BoxedStrategy<Evaluation> {
    (
        probability(),
        prop_oneof![9 => pick(&config.policy.tiers), 1 => Just(Tier("tier-elsewhere".into()))],
        pick(&[ChangesFiles::None, ChangesFiles::Few, ChangesFiles::Broad]),
        (probability(), probability(), probability()),
        opt_of(prop_oneof![
            3 => any_tab().prop_map(TabChoice::Tab),
            1 => Just(TabChoice::New),
        ]),
    )
        .prop_map(
            |(verifiable, tier, changes, (boundary, external, long), tab)| Evaluation {
                done_when_verifiable: verifiable,
                weakest_sufficient_tier: tier,
                changes_files: changes,
                security_boundary: boundary,
                needs_external: external,
                long_running: long,
                related_tab: tab,
            },
        )
        .boxed()
}
pub(crate) fn qualifications(config: &Config) -> BoxedStrategy<Vec<Qualification>> {
    let pairs = Vec::from_iter(config.catalog.operating_points.iter().flat_map(|point| {
        point
            .capabilities
            .iter()
            .map(|capability| (point.clone(), capability.clone()))
            .collect::<Vec<(OperatingPoint, Capability)>>()
    }));
    prop_vec((any::<bool>(), any::<bool>()), pairs.len())
        .prop_map(move |marks| {
            let mut rows = Vec::new();
            for ((point, capability), (pass, stale)) in pairs.iter().cloned().zip(marks) {
                rows.push(Qualification {
                    operating_point: point.id.clone(),
                    args_digest: if pass || !stale {
                        point.args_digest()
                    } else {
                        Digest([0; 32])
                    },
                    capability,
                    passed: pass || stale,
                    evidence: String::new(),
                });
            }
            rows
        })
        .boxed()
}

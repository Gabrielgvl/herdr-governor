//! Journal-side generators split out of the property proofs: the F8 journal
//! row states, arbitrary effect journals, the launch seed topology, frozen
//! handoffs, the persisted routing `Decision` and the Jev-result input tuple.

use governor_core::acceptance::FrozenHandoff;
use governor_core::config::{ConfigVersion, OperatingPointId, Provider, Tier};
use governor_core::identity::{AgentKind, ChildIdentity, EffectId, LaunchId, PaneId, RunId, TabId};
use governor_core::lifecycle::{
    Effect, EffectCertainty, EffectKind, EffectOutcome, EffectState, EffectTarget,
};
use governor_core::routing::{Candidate, Decision, Exploration, Judgment, PlacementPlan};
use proptest::collection::vec as prop_vec;
use proptest::prelude::{Just, Strategy};
use proptest::sample::subsequence;

use crate::strategies as arb;

/// The F8 journal row states; `failed` carries its required certainty
/// (Appendix B CHECK).
fn arb_journal_state() -> impl Strategy<Value = (EffectState, Option<EffectCertainty>)> {
    proptest::prop_oneof![
        2 => Just((EffectState::Planned, None)),
        3 => Just((EffectState::Dispatching, None)),
        2 => Just((EffectState::Acknowledged, None)),
        1 => arb::arb_certainty()
            .prop_map(|certainty| (EffectState::Failed, Some(certainty))),
        1 => Just((EffectState::Unconfirmed, None)),
    ]
}

/// The generated bits of a journal row that do not depend on its kind.
type JournalBits = (EffectState, Option<EffectCertainty>);

/// The kind-independent pieces an `EffectTarget` is built from: a plan
/// choice, a tab index, a pane index and a captured identity.
type TargetBits = (u8, u8, u8, ChildIdentity);

fn arb_target_bits() -> impl Strategy<Value = TargetBits> {
    (0_u8..4, 0_u8..4, 0_u8..4, arb::arb_identity())
}

/// The captured target an effect kind journals (Appendix B `target_json`).
fn target_of(kind: EffectKind, bits: &TargetBits) -> Option<EffectTarget> {
    let (plan_choice, tab, pane, identity) = bits;
    match kind {
        EffectKind::JevEvaluate => None,
        EffectKind::TabCreate => Some(EffectTarget::CallerContext(PaneId(format!("w0:p{pane}")))),
        EffectKind::PaneSplit => Some(EffectTarget::ExistingTab(TabId(format!("tab-{tab}")))),
        EffectKind::AgentStart => Some(EffectTarget::AgentPane(if *plan_choice == 0 {
            PlacementPlan::NewTab
        } else {
            PlacementPlan::ExistingTab {
                tab: TabId(format!("tab-{tab}")),
            }
        })),
        EffectKind::Prompt | EffectKind::Close => Some(EffectTarget::Child(identity.clone())),
    }
}

fn effect_of(suffix: &str, kind: EffectKind, journal: JournalBits, target: &TargetBits) -> Effect {
    let (state, certainty) = journal;
    let key = arb::run_key(suffix);
    Effect {
        id: EffectId(format!("eff:{}", key.0)),
        key,
        kind,
        subject_launch: Some(LaunchId(arb::text("l-1"))),
        subject_run: Some(RunId(arb::text(arb::RUN_ID))),
        target: target_of(kind, target),
        payload_digest: None,
        state,
        certainty,
        receipt: None,
    }
}

/// A journal built from `keys` — unique keys — with row states from
/// `bits`.
fn arb_journal_from(
    keys: &'static [(&'static str, EffectKind)],
    bits: fn() -> proptest::strategy::BoxedStrategy<JournalBits>,
) -> impl Strategy<Value = Vec<Effect>> {
    subsequence(Vec::from(keys), 0..=keys.len()).prop_flat_map(move |picked| {
        let len = picked.len();
        (
            Just(picked),
            prop_vec((bits(), arb_target_bits()), len..=len),
        )
            .prop_map(|(entries, rows)| {
                entries
                    .into_iter()
                    .zip(rows)
                    .map(|((suffix, kind), (journal, target))| {
                        effect_of(suffix, kind, journal, &target)
                    })
                    .collect()
            })
    })
}

/// The journaled effects the transition itself can plan.
const JOURNAL_KEYS: &[(&str, EffectKind)] = &[
    ("tab", EffectKind::TabCreate),
    ("split", EffectKind::PaneSplit),
    ("start:0", EffectKind::AgentStart),
    ("start:1", EffectKind::AgentStart),
    ("prompt:task", EffectKind::Prompt),
    ("nudge:0", EffectKind::Prompt),
    ("nudge:1", EffectKind::Prompt),
    ("outbox:0", EffectKind::Prompt),
    ("outbox:1", EffectKind::Prompt),
    ("review:0", EffectKind::JevEvaluate),
    ("review:1", EffectKind::JevEvaluate),
    ("accept:0:1", EffectKind::JevEvaluate),
    ("close", EffectKind::Close),
];

/// An arbitrary effect journal — unique keys, well-typed rows.
pub(crate) fn arb_journal() -> impl Strategy<Value = Vec<Effect>> {
    arb_journal_from(JOURNAL_KEYS, || arb_journal_state().boxed())
}

/// The topology effects a launch plan journals (Appendix C `reserved` →
/// `starting`): `tab_create`/`pane_split`, `dispatching`.
const SEED_TOPOLOGY_KEYS: &[(&str, EffectKind)] = &[
    ("tab", EffectKind::TabCreate),
    ("split", EffectKind::PaneSplit),
];

/// A fresh Run's seed journal — the topology rows the launch plan write
/// committed before any `transition` ran.
pub(crate) fn arb_seed_topology() -> impl Strategy<Value = Vec<Effect>> {
    arb_journal_from(SEED_TOPOLOGY_KEYS, || {
        Just((EffectState::Dispatching, None)).boxed()
    })
}

/// Arbitrary frozen handoffs for a Run — a small work-generation space
/// so generated digests collide with `handoff` events.
pub(crate) fn arb_handoffs() -> impl Strategy<Value = Vec<FrozenHandoff>> {
    prop_vec(
        (0_u64..4, arb::arb_digest(), arb::arb_timestamp()).prop_map(|(generation, digest, at)| {
            FrozenHandoff {
                run: RunId(arb::text(arb::RUN_ID)),
                work_generation: generation,
                digest,
                frozen_path: arb::text("/state/handoffs/f"),
                frozen_at: at,
            }
        }),
        0..4,
    )
}

/// A persisted routing decision — the candidate list the launch
/// pipeline walks on pre-interactive failures.
pub(crate) fn arb_decision() -> impl Strategy<Value = Decision> {
    prop_vec(arb_candidate(), 0..4).prop_map(|candidates| Decision {
        judged_tier: Tier(arb::text("t0")),
        requested_tier: None,
        policy_cap: None,
        policy_floor: None,
        caller_uplift: None,
        recovery_minimum: None,
        exploration: Exploration {
            assigned: false,
            executed: false,
        },
        start_tier: Tier(arb::text("t0")),
        candidates,
        config_version: ConfigVersion(arb::text("cfg-1")),
    })
}

fn arb_candidate() -> impl Strategy<Value = Candidate> {
    (0_u8..4).prop_map(|index| Candidate {
        operating_point: OperatingPointId(format!("op-{index}")),
        provider: Provider(format!("prov-{index}")),
        tier: Tier(format!("t{index}")),
        harness: AgentKind(arb::text("kind-1")),
        args: Vec::new(),
    })
}

/// An arbitrary Jev result input — `(key suffix, outcome, judgments,
/// stamp spec)`; `f20_stale_async_result_never_applies` rewrites the
/// receipt's `set.versions` against the concrete Run's stamp.
pub(crate) fn arb_jev_result()
-> impl Strategy<Value = (&'static str, EffectOutcome, Vec<Judgment>, arb::StampSpec)> {
    (
        arb::pick(arb::RESULT_SUFFIXES),
        arb::arb_outcome(),
        prop_vec(arb::arb_judgment(), 0..4),
        arb::arb_stale_spec(),
    )
}

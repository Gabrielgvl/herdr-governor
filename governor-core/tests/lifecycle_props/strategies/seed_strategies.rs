//! State-aware seeds for the F22 prefix proofs: each generated world is
//! driven into its unsettled state by the real public transitions (the
//! scripted drive lives in `seed_drive_strategies`), so the journal, the
//! frozen handoffs and every record field are consistent with how the
//! state is actually reached. A `reserved` Run never carries journaled
//! topology effects — the plan write is atomic. `arb_seeded_prefix`
//! pairs such a world with an arbitrary well-typed event prefix.

use governor_core::acceptance::FrozenHandoff;
use governor_core::identity::{
    ChildIdentity, ChildStatus, Digest, NativeSession, PaneId, TabId, Timestamp,
};
use governor_core::lifecycle::{Effect, EffectOutcome, Run};
use governor_core::routing::{Decision, PlacementPlan};
use proptest::option;
use proptest::prelude::{Just, Strategy, any, prop_oneof};

use super::common_strategies::{arb_digest, arb_timestamp, pick};
use super::event_strategies::{PrefixStep, arb_outcome, arb_steps};
use super::journal_strategies::arb_launch_decision;
use super::run_strategies::{arb_child_status, arb_identity, arb_reserved_run};
use super::seed_drive_strategies::seed_world;

/// The unsettled state a seed is driven to — declared in lifecycle order
/// so `>=` reads "at least this deep" (Appendix C `reserved → … →
/// repair`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SeedTarget {
    /// `reserved` — decision persisted, no effect journaled yet.
    Reserved,
    /// `starting` — topology planned and dispatched, `agent.start` in
    /// flight or mid-fallback.
    Starting,
    /// `prompting` — `agent.start` captured the identity; the task prompt
    /// is planned or dispatching.
    Prompting,
    /// `active` — the task prompt resolved either way (F16).
    Active,
    /// `judging` — a frozen handoff under assessment (F24).
    Judging,
    /// `repair` — a rejected work generation inside its window (F24).
    Repair,
}

/// How far the launch pipeline has run while still `starting`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchDepth {
    /// The topology effect is `dispatching`, its result outstanding.
    TopologyInFlight,
    /// The topology acknowledged; `start:0` is in flight.
    TopologyAcked,
    /// `start:0` took a pre-interactive failure — the walk planned
    /// `start:1` when a second candidate exists (F15).
    StartWalked,
}

/// Whether the seed's journal holds a repair follow-up the store already
/// committed `dispatching`, and where its `dispatched_at` lands against
/// the repair window `[rejected_at, repair_deadline)`.
#[derive(Debug, Clone, Copy)]
pub enum OutboxSeed {
    /// No repair follow-up journaled.
    Absent,
    /// Dispatched at `rejected_at` — the window's lower edge; qualifying.
    InWindow,
    /// Dispatched at `repair_deadline` — the upper edge; never qualifying.
    PastWindow,
}

/// The random draws a seed is built from — `seed_world` replays them
/// through the public transitions, so the result is consistent by
/// construction rather than by field-level guesswork.
#[derive(Debug, Clone)]
pub struct SeedInputs {
    /// The state to stop at.
    pub target: SeedTarget,
    /// The fresh `reserved` Run the drive starts from.
    pub reserved: Run,
    /// The persisted routing decision (at least one candidate — F13
    /// abstains without one).
    pub decision: Decision,
    /// The launch's placement plan.
    pub plan: PlacementPlan,
    /// The caller pane a `NewTab` plan writes as caller context.
    pub caller_pane: PaneId,
    /// The pane the scripted observations and receipts report.
    pub pane: PaneId,
    /// The tab an `ExistingTab` plan or `TabCreated` receipt names.
    pub tab: TabId,
    /// The identity `agent.start` captures (F2).
    pub identity: ChildIdentity,
    /// The native session an observation may report.
    pub session: Option<NativeSession>,
    /// The digest the first freeze writes.
    pub digest: Digest,
    /// The digest a `judging`-via-`repair` re-freeze writes.
    pub re_digest: Digest,
    /// How the task prompt resolves — every outcome moves `prompting` to
    /// `active` (F16).
    pub prompt_outcome: EffectOutcome,
    /// Whether the task prompt's dispatch commit has landed — a still
    /// `planned` prompt survives `restart` in place.
    pub prompt_dispatched: bool,
    /// An optional `unique` observation in `active`: `idle`/`done` opens
    /// the idle episode (`idle_deadline` arms, one nudge), `blocked` opens
    /// a blocked episode's ask.
    pub obs_status: Option<ChildStatus>,
    /// How far `starting` has progressed — forced at least
    /// `TopologyAcked` once the target is deeper.
    pub start_depth: LaunchDepth,
    /// `judging` seeds only: reach it through `repair` — the re-freeze
    /// keeps the armed `repair_deadline` (Appendix C).
    pub via_repair: bool,
    /// `repair` seeds (and `judging` via repair): a journaled outbox
    /// dispatch inside or past its window.
    pub outbox: OutboxSeed,
    /// Flip the newest frozen row's read-side `assessed` flag.
    pub assessed: bool,
    /// The drive's base time.
    pub start: Timestamp,
    /// The seed-to-prefix gap — a gap past an armed deadline means the
    /// prefix opens with it already overdue.
    pub post_ms: u64,
    /// The event prefix the proofs replay over the seeded world.
    pub steps: Vec<PrefixStep>,
}

/// A world driven to a real lifecycle state plus the event prefix the
/// proofs replay over it.
#[derive(Debug, Clone)]
pub struct SeededPrefix {
    /// The Run row — the seed's state with the fields its history wrote.
    pub run: Run,
    /// The effect journal — every row the transitions and the dispatch
    /// commits produced.
    pub journal: Vec<Effect>,
    /// The frozen handoffs — the read-side `assessed` flag may be set.
    pub handoffs: Vec<FrozenHandoff>,
    /// The persisted routing decision.
    pub decision: Option<Decision>,
    /// The `now` the prefix starts from — after every journaled time.
    pub start: Timestamp,
    /// The prefix: event, stamp spec, `now` delta, owner-absence flag.
    pub steps: Vec<PrefixStep>,
}

/// The seed target, weighted so the coverage floor holds: deep states keep
/// escaping to deeper ones (or settling) across a long prefix, so
/// `prompting`, `active` and `judging` carry the extra share while
/// `reserved` and `repair` are sticky by construction.
fn arb_seed_target() -> impl Strategy<Value = SeedTarget> {
    prop_oneof![
        12 => Just(SeedTarget::Reserved),
        12 => Just(SeedTarget::Starting),
        20 => Just(SeedTarget::Prompting),
        28 => Just(SeedTarget::Active),
        16 => Just(SeedTarget::Judging),
        10 => Just(SeedTarget::Repair),
    ]
}

fn arb_pane() -> impl Strategy<Value = PaneId> {
    (0_u8..4).prop_map(|pane| PaneId(format!("w0:p{pane}")))
}

fn arb_tab() -> impl Strategy<Value = TabId> {
    (0_u8..4).prop_map(|tab| TabId(format!("tab-{tab}")))
}

/// The placement plan the launch commits (F14).
fn arb_plan() -> impl Strategy<Value = PlacementPlan> {
    prop_oneof![
        Just(PlacementPlan::NewTab),
        arb_tab().prop_map(|tab| PlacementPlan::ExistingTab { tab }),
    ]
}

const OUTBOX_SEEDS: &[OutboxSeed] = &[
    OutboxSeed::Absent,
    OutboxSeed::InWindow,
    OutboxSeed::PastWindow,
];

const LAUNCH_DEPTHS: &[LaunchDepth] = &[
    LaunchDepth::TopologyInFlight,
    LaunchDepth::TopologyAcked,
    LaunchDepth::StartWalked,
];

/// The full `SeedInputs` space — the drive consumes it deterministically.
fn arb_inputs() -> impl Strategy<Value = SeedInputs> {
    (
        arb_seed_target(),
        arb_reserved_run(),
        arb_launch_decision(),
        (arb_plan(), arb_pane(), arb_pane(), arb_tab()),
        (
            arb_identity(),
            option::of((0_u8..4).prop_map(|s| NativeSession(format!("sess-{s}")))),
            arb_digest(),
            arb_digest(),
        ),
        (arb_outcome(), any::<bool>(), option::of(arb_child_status())),
        (pick(LAUNCH_DEPTHS), any::<bool>()),
        (pick(OUTBOX_SEEDS), any::<bool>()),
        (arb_timestamp(), 0_u64..3_600_000, arb_steps()),
    )
        .prop_map(|tuple| {
            let (
                target,
                reserved,
                decision,
                placement,
                child,
                prompt,
                launch_knobs,
                repair_knobs,
                timing,
            ) = tuple;
            let (plan, caller_pane, pane, tab) = placement;
            let (identity, session, digest, re_digest) = child;
            let (prompt_outcome, prompt_dispatched, obs_status) = prompt;
            let (start_depth, via_repair) = launch_knobs;
            let (outbox, assessed) = repair_knobs;
            let (start, post_ms, steps) = timing;
            SeedInputs {
                target,
                reserved,
                decision,
                plan,
                caller_pane,
                pane,
                tab,
                identity,
                session,
                digest,
                re_digest,
                prompt_outcome,
                prompt_dispatched,
                obs_status,
                start_depth,
                via_repair,
                outbox,
                assessed,
                start,
                post_ms,
                steps,
            }
        })
}

/// The seeded world plus its arbitrary prefix — a Run driven into one of
/// the six unsettled states through the public lifecycle, then an event
/// prefix replayed over it.
pub fn arb_seeded_prefix() -> impl Strategy<Value = SeededPrefix> {
    arb_inputs().prop_map(seed_world)
}

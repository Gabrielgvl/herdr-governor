//! N8 — the metamorphic input space (spec §7). `world` generates a
//! catalog/policy plus everything `route` (F13) and `transition` (Appendix
//! C) consume, and a random bijective `Rename` of the catalog's harness
//! kinds and operating-point ids. Shared name pools make renamings swap,
//! collide and mint fresh names; tiers, capabilities, providers, cost
//! classes, args, catalog order and caller identity all stay put. The
//! `Rename` application methods live in `../rename.rs`.

use std::collections::BTreeMap;

use governor_core::acceptance::FrozenHandoff;
use governor_core::config::{Capability, Config, Provider, Qualification};
use governor_core::identity::Timestamp;
use governor_core::lifecycle::{Effect, Event, Run, Versioned};
use governor_core::routing::Evaluation;
use governor_core::task::Launch;

/// A bijective renaming over harness kinds and operating-point ids;
/// undeclared names pass through.
#[derive(Debug, Clone)]
pub struct Rename {
    /// `old harness name -> new harness name` / `old op id -> new id`.
    pub kinds: BTreeMap<String, String>,
    pub ops: BTreeMap<String, String>,
}

/// One metamorphic case: everything `route` and `transition` read, plus
/// the renaming.
#[derive(Debug)]
pub struct World {
    pub config: Config,
    pub launch: Launch,
    pub predecessor: Option<Run>,
    pub evaluation: Evaluation,
    pub required: Vec<Capability>,
    pub qualifications: Vec<Qualification>,
    pub cooling: Vec<Provider>,
    pub run: Run,
    pub journal: Vec<Effect>,
    pub handoffs: Vec<FrozenHandoff>,
    pub event: Versioned<Event>,
    pub now: Timestamp,
    pub freeze_path: String,
    pub owner_absent: bool,
    pub rename: Rename,
}

pub mod input_strategies;
pub mod run_strategies;
pub mod world_strategies;

pub use world_strategies::world;

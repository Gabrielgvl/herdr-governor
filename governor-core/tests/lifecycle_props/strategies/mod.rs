//! F20/F22 — proptest strategies for the spec §10 Phase-3 lifecycle property
//! proofs: arbitrary but well-typed `Run`s, `Event`s, effect journals, frozen
//! handoffs and version stamps, built only on governor-core's public API.
//! The standalone `lifecycle_strategies.rs` target's items are split across
//! the sibling modules by input family; the trailing smoke test lives in
//! `event_strategies.rs`.

pub mod common_strategies;
pub mod event_strategies;
pub mod journal_strategies;
pub mod run_strategies;
pub mod stamp_strategies;

pub use common_strategies::{RUN_ID, arb_digest, arb_timestamp, pick, test_policy, text};
pub use event_strategies::{
    PrefixStep, RESULT_SUFFIXES, arb_certainty, arb_event, arb_judgment, arb_outcome, arb_prefix,
    arb_stamped_event, judgment_record, run_key,
};
pub use run_strategies::{arb_identity, arb_reserved_run, arb_run, arb_unsettled_run};
pub use stamp_strategies::{StampSpec, TripleField, arb_stale_spec, stamp, stamped, triple_of};

//! Shared constants and primitive generators every sibling module composes:
//! the fixed Run id, the test policy, uniform picks and the small timestamp
//! and digest spaces.

use std::fmt::Debug;
use std::time::Duration;

use governor_core::config::{Policy, Tier};
use governor_core::identity::{Digest, Timestamp};
use proptest::prelude::Strategy;
use proptest::sample::select;

/// The Run id every generated value shares — fixed so generated effect keys
/// collide with the real journal on purpose.
pub const RUN_ID: &str = "r-1";

#[must_use]
pub fn text(value: &str) -> String {
    String::from(value)
}

/// The fixed policy the proofs drive — mirrors the in-crate test policy.
#[must_use]
pub fn test_policy() -> Policy {
    Policy {
        tiers: Vec::from([Tier(text("t0")), Tier(text("t1"))]),
        no_change_cap: None,
        security_floor: None,
        broad_change_floor: None,
        provider_limit_threshold: 0.7,
        exploration_rate: 0.05,
        recovery_expiry: Duration::from_hours(24),
        cooldown: Duration::from_hours(1),
        max_age: Duration::from_hours(24),
        repair_window: Duration::from_mins(15),
        judgment_window: Duration::from_mins(30),
        idle_window: Duration::from_mins(15),
    }
}

/// Uniform pick from a finite table.
pub fn pick<T: Clone + Debug + 'static>(items: &'static [T]) -> impl Strategy<Value = T> {
    select(Vec::from(items))
}

/// Small timestamp space so generated `now`s land near armed deadlines.
pub fn arb_timestamp() -> impl Strategy<Value = Timestamp> {
    (0_i64..10_000_000_i64).prop_map(Timestamp)
}

/// A generated digest from a deliberately tiny space — repeats collide with
/// frozen handoffs, which is what the digest dedup rules consume.
pub fn arb_digest() -> impl Strategy<Value = Digest> {
    (0_u8..4).prop_map(|byte| Digest([byte; 32]))
}

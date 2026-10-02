//! Shared constants, the miniature-store rig and the primitive generators
//! every sibling module composes: the fixed Run id, the test policy, uniform
//! picks, the small timestamp and digest spaces, the coordinator's freeze
//! destination and `Sim` — the conditional-write contract the seeds drive
//! and the proofs assert against.

use std::fmt::Debug;
use std::time::Duration;

use governor_core::acceptance::FrozenHandoff;
use governor_core::config::{Policy, Tier};
use governor_core::identity::{Digest, Timestamp};
use governor_core::lifecycle::{Effect, Run, StateChange, Transition};
use governor_core::routing::Decision;
use proptest::prelude::Strategy;
use proptest::sample::select;

/// The Run id every generated value shares — fixed so generated effect keys
/// collide with the real journal on purpose.
pub const RUN_ID: &str = "r-1";

/// The coordinator-supplied destination a new freeze writes (F24).
pub const FREEZE_PATH: &str = "/state/handoffs/new";

/// The store's conditional-write contract in miniature (Appendix B):
/// `UpdateRun` applies while `expected_version` still holds,
/// `WriteEffect` updates its journaled row, planned effects join the
/// journal, freezes join the handoffs.
pub(crate) struct Sim {
    /// The live Run row.
    pub run: Run,
    /// The effect journal.
    pub journal: Vec<Effect>,
    /// The frozen handoffs.
    pub handoffs: Vec<FrozenHandoff>,
    /// The persisted routing decision.
    pub decision: Option<Decision>,
}

impl Sim {
    /// Commit a transition the way `store::apply` does.
    pub(crate) fn apply(&mut self, transition: &Transition) {
        for change in &transition.state_changes {
            match change {
                StateChange::UpdateRun(update) => {
                    assert!(
                        update.expected_version == self.run.version,
                        "conditional run write must be computed against the live row"
                    );
                    self.run.clone_from(&update.record);
                }
                StateChange::WriteEffect(write) => {
                    // an UPDATE against an unjournaled key matches no row
                    if let Some(row) = self.journal.iter_mut().find(|row| row.key == write.key) {
                        row.state = write.state;
                        row.certainty = write.certainty;
                        row.receipt.clone_from(&write.receipt);
                    }
                }
                StateChange::FreezeHandoff(handoff) => self.handoffs.push(handoff.clone()),
                StateChange::BindCaller(_)
                | StateChange::RecordLaunch(_)
                | StateChange::ReserveRun(_)
                | StateChange::ChangeOwner(_)
                | StateChange::WriteFollowUp(_)
                | StateChange::ExpireFollowUps { .. }
                | StateChange::RecordRecovery(_)
                | StateChange::SetCooldown(_)
                | StateChange::AckEvent(_) => {}
            }
        }
        for effect in &transition.effects {
            self.journal.push(effect.clone());
        }
    }
}

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

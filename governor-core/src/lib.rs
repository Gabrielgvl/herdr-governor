#![no_std]

// `governor-core` is the pure domain crate (spec §9). No I/O: every input
// arrives as a value, including time and entropy; every output is a
// `lifecycle::Transition` the store commits through `store::apply`. Modules
// are the spec §9 split. (Line comments, not `//!` docs — the purity
// selftest injects items directly after `#![no_std]` and inner doc comments
// may not follow items.)

extern crate alloc;

/// F24 — handoff reading, freezing and per-item assessment binding.
pub mod acceptance;
/// F27 — typed catalog values: operating-point catalog, routing policy,
/// tiers.
pub mod config;
/// F9/F17 — the outbox and message states; F18 — mailbox events and the
/// hint rule.
pub mod delivery;
/// F1 — caller identity; F2 — child identity; F3 — observation classes; plus
/// the shared id, digest and time primitives.
pub mod identity;
/// Appendix C — the lifecycle transition vocabulary: states, events,
/// settlement, deadlines, the version triple, the F8 effect journal and the
/// `Transition` value.
pub mod lifecycle;
/// F21 — recovery obligations and provider cooldowns (ADR-0003).
pub mod recovery;
/// F12 — judgments and question kinds; F13 — the persisted decision; F14 —
/// the placement plan.
pub mod routing;
/// F5 — the Task, the provenance envelope, the Launch record and the
/// refusal codes.
pub mod task;

/// Herdr protocol revision this build of the governor speaks.
#[must_use]
pub fn herdr_protocol() -> u32 {
    22
}

#[cfg(test)]
mod tests {
    use super::herdr_protocol;

    #[test]
    fn herdr_protocol_is_22() {
        assert_eq!(herdr_protocol(), 22, "protocol revision must be 22");
    }
}

//! F27 — the typed values of `catalog.toml`: the operating-point catalog and
//! the routing policy, plus the qualification evidence that gates capability
//! claims (F26). `Config::validate` is the F27 typed check; `OperatingPoint::
//! has_current_pass` is the F26 predicate routing and delivery consult.
//! Harness kinds are catalog data here — never literals (N8, ADR-0002).

mod catalog;
mod qualification;

pub use catalog::{
    Catalog, Config, ConfigError, ConfigVersion, CostClass, DEFAULT_EXPLORATION_RATE,
    DEFAULT_IDLE_WINDOW, DEFAULT_JUDGMENT_WINDOW, DEFAULT_MAX_AGE, DEFAULT_RECOVERY_EXPIRY,
    DEFAULT_REPAIR_WINDOW, MAX_POLICY_WINDOW, OperatingPoint, OperatingPointId,
    PROVIDER_NAME_MAX_BYTES, Policy, Provider, Tier,
};
pub use qualification::{Capability, Qualification, args_digest};

#[cfg(test)]
mod tests {
    use super::{
        Capability, DEFAULT_EXPLORATION_RATE, DEFAULT_IDLE_WINDOW, DEFAULT_JUDGMENT_WINDOW,
        DEFAULT_MAX_AGE, DEFAULT_RECOVERY_EXPIRY, DEFAULT_REPAIR_WINDOW,
    };

    #[test]
    fn f13_f21_f22_f24_f25_default_values() {
        assert_eq!(
            DEFAULT_EXPLORATION_RATE.to_bits(),
            0x3fa9_9999_9999_999a,
            "exploration rate is 0.05 (F13)"
        );
        assert_eq!(
            DEFAULT_RECOVERY_EXPIRY.as_secs(),
            86_400,
            "recovery expiry is 24 hours (F21)"
        );
        assert_eq!(
            DEFAULT_MAX_AGE.as_secs(),
            86_400,
            "max age is 24 hours (F22)"
        );
        assert_eq!(
            DEFAULT_REPAIR_WINDOW.as_secs(),
            900,
            "repair window is 15 minutes (F24)"
        );
        assert_eq!(
            DEFAULT_JUDGMENT_WINDOW.as_secs(),
            1_800,
            "judgment window is 30 minutes (F24)"
        );
        assert_eq!(
            DEFAULT_IDLE_WINDOW.as_secs(),
            900,
            "idle window is 15 minutes (F25)"
        );
    }

    #[test]
    fn f26_capability_spellings() {
        let consts = [
            (Capability::START, "start"),
            (Capability::PROMPT_ACK, "prompt_ack"),
            (Capability::HANDOFF_WRITE, "handoff_write"),
            (Capability::FOLLOWUP_READ, "followup_read"),
            (Capability::MID_TURN_INPUT, "mid_turn_input"),
            (Capability::HINT_CONSUMPTION, "hint_consumption"),
        ];
        for (constant, name) in consts {
            assert_eq!(constant, name, "capability spelling must match F26");
        }
        let capability = Capability(Capability::HANDOFF_WRITE.into());
        assert_eq!(
            capability.as_str(),
            "handoff_write",
            "as_str returns the capability's catalog name"
        );
    }
}

#[cfg(test)]
mod f26_f27_tests;
#[cfg(test)]
mod f27_bounds_tests;

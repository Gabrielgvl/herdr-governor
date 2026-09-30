//! F21 — recovery obligations and provider cooldowns (ADR-0003): a
//! `provider_limited` settlement creates exactly one obligation; dispatch
//! waits for fresh proof the predecessor's identity is absent. Panes are
//! never closed automatically.

mod admission;
mod cooldown;
mod obligation;
mod settle;

pub use admission::{
    RECOVERY_KEY_PREFIX, caller_admission, dispatch_ready, successor_key, successor_task,
};
pub use cooldown::Cooldown;
pub use obligation::{RecoveryObligation, RecoveryOrigin, RecoveryStatus};
pub use settle::provider_limited;

#[cfg(test)]
mod tests {
    mod builders;
    mod f21_admission;
    mod f21_cooldown;
    mod f21_obligation;
    mod f21_settle;
}

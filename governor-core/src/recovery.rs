//! F21 — recovery obligations and provider cooldowns (ADR-0003): a
//! `provider_limited` settlement creates exactly one obligation; dispatch
//! waits for fresh proof the predecessor's identity is absent. Panes are
//! never closed automatically.

mod admission;
mod cooldown;
mod obligation;
mod settle;

use alloc::string::String;

pub use admission::{
    RECOVERY_KEY_PREFIX, caller_admission, dispatch_ready, successor_key, successor_task,
};
pub use cooldown::Cooldown;
pub use obligation::{RecoveryObligation, RecoveryOrigin, RecoveryStatus};
pub use settle::provider_limited;

/// Minimal JSON string escaping for mailbox `body_json` interpolation —
/// quotes, backslashes and control characters are escaped so a free-form
/// `Provider` or `RunId` value cannot break the event body. Shared with
/// the lifecycle settle path's `cooldown_hit` body (F21).
pub(crate) fn json_str(value: &str) -> String {
    let mut out = String::with_capacity(value.len().saturating_add(2));
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if c < '\u{20}' => {
                let code = u32::from(c);
                out.push_str("\\u00");
                out.push(char::from_digit(code >> 4, 16).unwrap_or('0'));
                out.push(char::from_digit(code & 0xf, 16).unwrap_or('0'));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    mod builders;
    mod f21_admission;
    mod f21_cooldown;
    mod f21_obligation;
    mod f21_settle;
}

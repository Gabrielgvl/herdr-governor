//! Integration-test helpers shared across `herdr-governor/tests/*.rs`
//! (conditional-protected once created — see `docs/guardrails.md`).
//! Each test crate pulls this in with `#[cfg(test)] pub mod support;`.

pub mod crash;
pub mod e2e;
pub mod fake_herdr;

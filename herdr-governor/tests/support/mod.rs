//! Integration-test helpers shared across `herdr-governor/tests/*.rs`
//! (conditional-protected once created — see `docs/guardrails.md`).
//! Each test crate pulls this in with `#[cfg(test)] pub mod support;`.

pub mod fake_herdr;

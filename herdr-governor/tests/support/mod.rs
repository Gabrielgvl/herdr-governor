//! Integration-test helpers shared across `herdr-governor/tests/*.rs`
//! (conditional-protected once created — see `docs/guardrails.md`).
//! Each test crate pulls this in with `#[cfg(test)] pub mod support;`.

pub mod crash;
pub mod daemon;
pub mod e2e;
pub mod fake_herdr;
pub mod fake_jev;
pub mod mcp_client;

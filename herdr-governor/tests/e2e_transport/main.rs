//! `e2e_transport` — the child-process daemon tests (F27/F28): the real
//! `herdr-governor` binary spawned with `--state-dir`/`--config-dir`
//! fixtures, asserting startup's one-sanitized-line refusal, the
//! lock-probe refusal of a second instance, and kill → stale-socket →
//! rebind — plus the in-process MCP transport set (P5.M2): `tools/list`,
//! concurrent connections and `herdr_status` over the v1 relay frame on
//! the real socket. `identity`/`status` are the F1/F7 end-to-end checks
//! through the daemon's public boundary. The remaining F1 transport set
//! lands with P5.A4 on the real relay.

#[cfg(test)]
mod identity;
#[cfg(test)]
mod startup;
#[cfg(test)]
mod status;
#[cfg(test)]
#[path = "../support/mod.rs"]
pub mod support;
#[cfg(test)]
mod transport;

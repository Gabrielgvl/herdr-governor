//! `e2e_transport` — the child-process daemon tests (F27/F28): the real
//! `herdr-governor` binary spawned with `--state-dir`/`--config-dir`
//! fixtures, asserting startup's one-sanitized-line refusal, the
//! lock-probe refusal of a second instance, and kill → stale-socket →
//! rebind — plus the in-process MCP transport set (P5.M2): `tools/list`
//! and concurrent connections over the v1 relay frame. The F1/relay set
//! lands with P5.A4 on the real relay.

#[cfg(test)]
mod startup;
#[cfg(test)]
mod transport;

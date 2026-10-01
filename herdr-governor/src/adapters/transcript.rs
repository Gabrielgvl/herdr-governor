//! `transcript` — the per-harness session-transcript adapter (ADR-0002):
//! pointer resolution and deterministic tail-window reads. Harness literals
//! are legal only under `transcript/` (I9), so this root carries
//! `mod`/`pub use` declarations and no harness-aware prose. Filled by
//! P4.T1.

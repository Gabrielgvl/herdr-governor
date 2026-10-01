//! `adapters` — the I/O edges behind typed interfaces (spec §9, §10
//! Phase 4): `config` — catalog load and reload (F27); `git` — worktree
//! evidence; `herdr` — the Herdr client over the confirmed protocol subset;
//! `jev` — the Jev judgment client; `transcript` — per-harness session
//! transcript parsers (ADR-0002; the only subtree where harness literals are
//! legal, I9).

pub mod config;
pub mod git;
pub mod herdr;
pub mod jev;
pub mod transcript;

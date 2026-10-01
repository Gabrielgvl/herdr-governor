//! `config` — `catalog.toml` decode into the core `Config`: fail-closed
//! load, a content-derived `ConfigVersion`, last-good reload semantics
//! (F27), and credential-file reads. Filled by P4.C1, which owns this root
//! and its `config/` children.

//! `tests` — the daemon's in-process contract: `run`'s bring-up, serve and
//! teardown over real files; the bounded apply-retry; the §4.3 step-5
//! restart marking (including the [r2] settled-Run case); `check_config`'s
//! exit codes; and the lexical tripwire that keeps `tracing::` call sites
//! to ids, sizes and digests (§4.18).

mod coordinator;
mod process;

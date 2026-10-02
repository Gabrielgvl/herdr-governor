//! `store_probe` — the P4.S4 crash-suite child process. It opens a store,
//! applies a canned scenario's transitions through the public
//! `Store::apply`, and either survives or dies at a chosen statement
//! boundary via the S3 crash-checkpoint seam (`GOV_STORE_CRASH_AT`) — a
//! real `abort()`, no ROLLBACK, no WAL flush: power loss, not a clean drop.
//!
//! ```text
//! store_probe <scenario> <db> seed                 apply the pre-state
//! store_probe <scenario> <db> apply                apply the target, no crash
//! store_probe <scenario> <db> count                apply the target and print the
//!                                                  boundary count (needs
//!                                                  GOV_STORE_CRASH_COUNT=1)
//! store_probe <scenario> <db> abort-after <k>      abort after statement k (k=0:
//!                                                  before the transaction; k>0
//!                                                  needs GOV_STORE_CRASH_AT=k)
//! store_probe <scenario> <db> abort-after-commit   apply, then abort
//! ```
//!
//! Exit codes: 0 ok · 1 store error · 2 usage · 3 typed `Conflict` /
//! `PhaseConflict` (the target was already applied — the idempotent-replay
//! answer) · 4 the boundary was never reached (the matrix is stale).
//!
//! The environment is set by the parent: `std::env::set_var` is unsound
//! mid-process (edition 2024), so the probe only verifies the knob
//! matches its argv.

pub mod scenarios;

use std::error::Error;
use std::io::Write as _;
use std::path::Path;
use std::process::{ExitCode, abort};

use herdr_governor::store::{ApplyError, Store, crash_checkpoint_count};

use crate::scenarios::{NOW, scenario};

const USAGE: &str = "usage: store_probe <scenario> <db> \
    (seed | apply | count | abort-after <k> | abort-after-commit)";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Seed,
    Apply,
    Count,
    AbortAfter(usize),
    AbortAfterCommit,
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(code) => code,
        Err(err) => {
            complain(&format!("store_probe: {err}"));
            ExitCode::from(1)
        }
    }
}

/// Writes `message` to stderr; a failed write has nowhere better to go.
fn complain(message: &str) {
    let _ignored = writeln!(std::io::stderr().lock(), "{message}");
}

fn parse(args: &[String]) -> Option<Mode> {
    let mode = match args.get(2).map(String::as_str)? {
        "seed" if args.len() == 3 => Mode::Seed,
        "apply" if args.len() == 3 => Mode::Apply,
        "count" if args.len() == 3 => Mode::Count,
        "abort-after-commit" if args.len() == 3 => Mode::AbortAfterCommit,
        "abort-after" if args.len() == 4 => Mode::AbortAfter(args.get(3)?.parse().ok()?),
        _ => return None,
    };
    Some(mode)
}

/// The knob the parent must have exported for `mode`, when one is needed.
fn env_matches(mode: Mode) -> bool {
    let var = |name: &str| std::env::var(name).ok();
    match mode {
        Mode::Count => var("GOV_STORE_CRASH_COUNT").as_deref() == Some("1"),
        Mode::AbortAfter(k) if k > 0 => var("GOV_STORE_CRASH_AT") == Some(k.to_string()),
        Mode::Seed | Mode::Apply | Mode::AbortAfter(_) | Mode::AbortAfterCommit => true,
    }
}

fn run(args: &[String]) -> Result<ExitCode, Box<dyn Error>> {
    let (Some(mode), Some(name), Some(db)) = (parse(args), args.first(), args.get(1)) else {
        complain(USAGE);
        return Ok(ExitCode::from(2));
    };
    let Some((seed, target)) = scenario(name) else {
        complain(&format!("store_probe: unknown scenario {name}"));
        return Ok(ExitCode::from(2));
    };
    if !env_matches(mode) {
        complain("store_probe: GOV_STORE_CRASH_AT/GOV_STORE_CRASH_COUNT do not match the mode");
        return Ok(ExitCode::from(2));
    }
    let mut store = Store::open(Path::new(db))?;
    if mode == Mode::Seed {
        for transition in &seed {
            store.apply(transition, NOW)?;
        }
        return Ok(ExitCode::SUCCESS);
    }
    if mode == Mode::AbortAfter(0) {
        // Boundary 0: the process dies before BEGIN IMMEDIATE ever runs.
        abort();
    }
    match store.apply(&target, NOW) {
        Ok(()) => {}
        Err(ApplyError::Conflict { .. } | ApplyError::PhaseConflict { .. }) => {
            return Ok(ExitCode::from(3));
        }
        Err(err) => return Err(err.into()),
    }
    match mode {
        Mode::Count => writeln!(std::io::stdout().lock(), "{}", crash_checkpoint_count())?,
        Mode::AbortAfterCommit => abort(),
        // The seam never fired: the statement list is shorter than the
        // matrix believes.
        Mode::AbortAfter(_) => return Ok(ExitCode::from(4)),
        Mode::Seed | Mode::Apply => {}
    }
    Ok(ExitCode::SUCCESS)
}

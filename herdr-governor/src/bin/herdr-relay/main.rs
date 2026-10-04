//! `herdr-relay` — the standalone relay binary (OQ-R fallback): the
//! std-only transport split out of the fat daemon binary so the linker
//! drops the daemon/store/Jev code it never calls and the per-session
//! process holds the N4 ≤ 8 MB RSS bound. `herdr-governor relay` keeps
//! dispatching to the same `relay::run` for compatibility — the
//! production harness spawns this name. Auto-discovered bin (OQ-6): no
//! manifest entry.

use std::env;
use std::process::ExitCode;

fn main() -> ExitCode {
    herdr_governor::relay::run(&env::args().skip(1).collect::<Vec<_>>())
}

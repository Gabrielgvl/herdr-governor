//! `relay_probe` — the P5.R1 child-process entry for
//! `tests/relay_e2e.rs` (the `store_probe` precedent): it runs
//! `relay::run` so the e2e suite exercises the relay over real pipes and
//! a real unix socket. The production spawn is `herdr-governor relay`
//! wired in `main.rs` — sibling lane A1's edit; both paths call the same
//! `run`, so the tests stay valid once the subcommand lands.
//! Auto-discovered bin: no manifest entry.

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    herdr_governor::relay::run(&args)
}

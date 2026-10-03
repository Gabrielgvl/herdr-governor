//! `herdr-governor` — the hand-rolled subcommand entry (spec §9: no
//! CLI-parsing crate). `daemon` and `check-config` are A1's; the `relay`
//! arm is the P5.R1 integration point (the sibling lane owns
//! `src/relay.rs` and the `relay::run` this arm will call).

use std::env;
use std::io::{self, Write as _};
use std::process::ExitCode;

const USAGE: &str = "usage: herdr-governor <daemon|check-config|relay> [--help]";

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    cli(env::args().skip(1).collect()).await
}

/// The argv dispatch: a recognized subcommand runs, bare/`--help` prints
/// usage, anything else is a usage error (exit 2).
async fn cli(argv: Vec<String>) -> ExitCode {
    match argv.first().map(String::as_str) {
        Some("daemon") => herdr_governor::daemon::cli(argv.get(1..).unwrap_or_default()).await,
        Some("check-config") => {
            herdr_governor::daemon::check_cli(argv.get(1..).unwrap_or_default()).await
        }
        Some("relay") => relay(argv.get(1..).unwrap_or_default()),
        Some("-h" | "--help" | "help") | None => {
            drop(writeln!(io::stdout().lock(), "{USAGE}"));
            ExitCode::SUCCESS
        }
        Some(_) => {
            drop(writeln!(io::stderr().lock(), "{USAGE}"));
            ExitCode::from(2)
        }
    }
}

/// `herdr-governor relay …` — P5.R1 lands `crate::relay::run`; until the
/// module exists the arm refuses with one fixed line (the subcommand is
/// declared in the usage, it is never "unknown").
fn relay(_args: &[String]) -> ExitCode {
    drop(writeln!(
        io::stderr().lock(),
        "relay: not yet available in this build"
    ));
    ExitCode::from(2)
}

#[cfg(test)]
mod tests {
    use std::process::ExitCode;

    use super::cli;

    /// `cli` is the `main` body under the argv the OS hands it: a bare or
    /// `--help` invocation prints usage and exits 0; an unknown subcommand
    /// and the unwired `relay` arm both exit 2.
    #[test]
    fn main_returns_success() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        assert_eq!(rt.block_on(cli(Vec::new())), ExitCode::SUCCESS);
        assert_eq!(
            rt.block_on(cli(vec!["--help".to_string()])),
            ExitCode::SUCCESS,
            "--help usage"
        );
        assert_eq!(
            rt.block_on(cli(vec!["bogus".to_string()])),
            ExitCode::from(2),
            "unknown subcommand"
        );
        assert_eq!(
            rt.block_on(cli(vec!["relay".to_string()])),
            ExitCode::from(2),
            "relay is declared but not yet available"
        );
    }
}

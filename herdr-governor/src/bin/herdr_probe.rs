//! `herdr_probe check <socket>` — the P4.H3 real-leg probe: run the
//! `adapters::herdr::probe` battery through the H1 client against the
//! named socket and print the per-op JSON report on stdout. Exit 0 only
//! when every op passed, so a dead or drifting leg fails closed.
//!
//! Isolation: this bin exists to be pointed at a *dedicated, isolated* named
//! probe session (`herdr --session <name> server`). It refuses the
//! inherited live endpoint (`HERDR_SOCKET_PATH`) and any socket outside a
//! `sessions/<name>/` directory — the owner's default session lives at
//! `<config>/herdr.sock`, never under `sessions/`. Auto-discovered bin
//! (OQ-6): no manifest entry.

use std::error::Error;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use herdr_governor::adapters::herdr::{Client, probe};

const USAGE: &str = "usage: herdr_probe check <session-socket>";
const OP_DEADLINE: Duration = Duration::from_secs(10);

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(code) => code,
        Err(e) => {
            std::io::stderr()
                .write_all(format!("herdr_probe: {e}\n").as_bytes())
                .ok();
            ExitCode::FAILURE
        }
    }
}

fn run(args: &[String]) -> Result<ExitCode, Box<dyn Error>> {
    let [cmd, socket_arg] = args else {
        return Err(USAGE.into());
    };
    if cmd != "check" {
        return Err(USAGE.into());
    }
    let socket = PathBuf::from(socket_arg);
    refuse_live(&socket)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let report = runtime.block_on(probe::battery(&Client::new(&socket), OP_DEADLINE));
    let mut out = serde_json::to_vec_pretty(&report)?;
    out.push(b'\n');
    std::io::stdout().lock().write_all(&out)?;
    Ok(if report.all_ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

/// The never-the-live-session guard: the socket must sit under a
/// `sessions/<name>/` directory and must not be the inherited
/// `HERDR_SOCKET_PATH` (compared after canonicalization, so a symlink to
/// the live socket is refused too).
fn refuse_live(socket: &Path) -> Result<(), Box<dyn Error>> {
    let under_sessions = socket
        .parent()
        .and_then(Path::parent)
        .and_then(Path::file_name)
        .is_some_and(|d| d == "sessions");
    if !under_sessions {
        return Err(format!(
            "refusing {}: not a named-session socket (expected .../sessions/<name>/<socket>)",
            socket.display()
        )
        .into());
    }
    let canonical = socket.canonicalize()?;
    let is_live = std::env::var_os("HERDR_SOCKET_PATH")
        .and_then(|live| PathBuf::from(live).canonicalize().ok())
        .is_some_and(|live| live == canonical);
    if is_live {
        return Err(format!(
            "refusing {}: it is the live HERDR_SOCKET_PATH",
            socket.display()
        )
        .into());
    }
    Ok(())
}

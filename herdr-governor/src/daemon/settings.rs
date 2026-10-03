//! `settings` — the `daemon` subcommand's argv (§4.3 step 1): the hand-rolled
//! parser for `--state-dir`, `--config-dir`, `--herdr-socket`,
//! `--reconcile-secs` and the test-only `--seam` override (OQ-T disclosed).
//! Hand-rolled per spec §9 — no CLI-parsing crate.

use std::path::PathBuf;
use std::time::Duration;

use super::paths::{default_config_dir, default_state_dir};
use super::seam::{SeamConfig, SeamError};
use super::{DaemonError, sanitize};

/// The `daemon` subcommand's invocation: the resolved `Settings` plus the
/// `--seam` override when given (env is still read when the flag is absent
/// — `daemon::run` resolves that).
#[derive(Debug)]
pub(super) struct Invocation {
    /// The merged settings.
    pub settings: Settings,
    /// The `--seam` override, when the flag was given.
    pub seam: Option<SeamConfig>,
}

/// What the daemon needs beyond `catalog.toml` (§4.3 step 1): the two
/// dirs plus the argv overrides that win over `[daemon]` keys. `Option`
/// fields are "not overridden" — `None` keeps the catalog value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    /// The state dir (§4.15: `~/.local/state/herdr-governor` default).
    pub state_dir: PathBuf,
    /// The config dir holding `catalog.toml` + `credentials`.
    pub config_dir: PathBuf,
    /// `--herdr-socket` — wins over `[daemon].herdr_socket`.
    pub herdr_socket: Option<PathBuf>,
    /// `--reconcile-secs` — wins over `[daemon].reconcile_secs`.
    pub reconcile_secs: Option<u64>,
}

/// The argv overrides re-applied on every (re)load — the catalog value for
/// a key not overridden.
#[derive(Debug, Clone)]
pub(super) struct Resolved {
    /// The effective Herdr socket path.
    pub herdr_socket: PathBuf,
    /// The effective reconcile interval.
    pub reconcile: Duration,
}

/// Merge `settings`' argv overrides over `daemon` settings.
pub(super) fn resolve(
    settings: &Settings,
    daemon: &crate::adapters::config::DaemonSettings,
) -> Resolved {
    Resolved {
        herdr_socket: settings
            .herdr_socket
            .clone()
            .unwrap_or_else(|| daemon.herdr_socket.clone()),
        reconcile: settings
            .reconcile_secs
            .map_or(daemon.reconcile, Duration::from_secs),
    }
}

/// The argv cursor `take` consumes through.
type Args<'a> = std::iter::Peekable<std::slice::Iter<'a, String>>;

/// `--flag value` / `--flag=value` — the next argument or the `=` suffix.
/// `Ok(None)` when the head arg is not `flag` (nothing consumed).
fn take<'a>(flag: &str, args: &mut Args<'a>) -> Result<Option<&'a str>, DaemonError> {
    let Some(arg) = args.peek().copied() else {
        return Ok(None);
    };
    if let Some(value) = arg.strip_prefix(&format!("{flag}=")) {
        args.next();
        return Ok(Some(value));
    }
    if arg == flag {
        args.next();
        return args
            .next()
            .map(String::as_str)
            .ok_or_else(|| DaemonError::usage(format!("{flag} needs a value")))
            .map(Some);
    }
    Ok(None)
}

/// `herdr-governor daemon [args]` → `Invocation`. Unknown flags and missing
/// values are `Usage` — the usage line already names every flag.
pub(super) fn parse_daemon_args(args: &[String]) -> Result<Invocation, DaemonError> {
    let mut state_dir_opt = None;
    let mut config_dir_opt = None;
    let mut herdr_socket = None;
    let mut reconcile_secs = None;
    let mut seam = None;
    let mut it = args.iter().peekable();
    while it.peek().is_some() {
        let matched = if let Some(value) = take("--state-dir", &mut it)? {
            state_dir_opt = Some(PathBuf::from(value));
            true
        } else if let Some(value) = take("--config-dir", &mut it)? {
            config_dir_opt = Some(PathBuf::from(value));
            true
        } else if let Some(value) = take("--herdr-socket", &mut it)? {
            herdr_socket = Some(PathBuf::from(value));
            true
        } else if let Some(value) = take("--reconcile-secs", &mut it)? {
            reconcile_secs = Some(value.parse::<u64>().map_err(|parse| {
                DaemonError::usage(format!(
                    "--reconcile-secs {value:?} is not seconds: {parse}"
                ))
            })?);
            true
        } else if let Some(value) = take("--seam", &mut it)? {
            seam = Some(SeamConfig::parse(value).map_err(|SeamError(spec)| {
                DaemonError::usage(format!("--seam {spec:?} is malformed"))
            })?);
            true
        } else {
            false
        };
        if !matched {
            let unknown = it.next().map_or("", String::as_str);
            return Err(DaemonError::usage(format!(
                "unknown argument: {}",
                sanitize(unknown)
            )));
        }
    }
    let Some(state_dir) = state_dir_opt.or_else(default_state_dir) else {
        return Err(DaemonError::usage(
            "no --state-dir and no HOME to default it from",
        ));
    };
    let Some(config_dir) = config_dir_opt.or_else(default_config_dir) else {
        return Err(DaemonError::usage(
            "no --config-dir and no HOME to default it from",
        ));
    };
    Ok(Invocation {
        settings: Settings {
            state_dir,
            config_dir,
            herdr_socket,
            reconcile_secs,
        },
        seam,
    })
}

/// `herdr-governor check-config [--config-dir <dir>]` → the config dir.
pub(super) fn parse_check_config_args(args: &[String]) -> Result<PathBuf, DaemonError> {
    let mut config_dir_opt = None;
    let mut it = args.iter().peekable();
    while it.peek().is_some() {
        if let Some(value) = take("--config-dir", &mut it)? {
            config_dir_opt = Some(PathBuf::from(value));
        } else {
            let unknown = it.next().map_or("", String::as_str);
            return Err(DaemonError::usage(format!(
                "unknown argument: {}",
                sanitize(unknown)
            )));
        }
    }
    match config_dir_opt.or_else(default_config_dir) {
        Some(dir) => Ok(dir),
        None => Err(DaemonError::usage(
            "no --config-dir and no HOME to default it from",
        )),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{parse_check_config_args, parse_daemon_args};

    fn args(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| (*w).to_string()).collect()
    }

    /// `--flag value` and `--flag=value` both parse; unknown args and a
    /// missing value are Usage errors.
    #[test]
    fn settings_parse_flags_and_rejects_unknown() {
        let invocation = parse_daemon_args(&args(&[
            "--state-dir",
            "/tmp/state",
            "--config-dir=/tmp/conf",
            "--herdr-socket",
            "/tmp/herdr.sock",
            "--reconcile-secs",
            "45",
            "--seam",
            "prompt:task@wire_returned:abort",
        ]))
        .expect("parses");
        let settings = invocation.settings;
        assert_eq!(settings.state_dir, PathBuf::from("/tmp/state"));
        assert_eq!(settings.config_dir, PathBuf::from("/tmp/conf"));
        assert_eq!(
            settings.herdr_socket,
            Some(PathBuf::from("/tmp/herdr.sock"))
        );
        assert_eq!(settings.reconcile_secs, Some(45));
        assert_eq!(invocation.seam.expect("seam").suffix, "prompt:task");

        for bad in [
            args(&["--bogus"]),
            args(&["--state-dir"]),           // missing value
            args(&["--reconcile-secs", "x"]), // not a number
            args(&["--seam", "nope"]),        // malformed seam
            args(&["positional"]),
        ] {
            assert!(parse_daemon_args(&bad).is_err(), "rejects {bad:?}");
        }
    }

    /// `check-config` takes only `--config-dir`; the default derivation
    /// lands on a path ending `herdr-governor` (or a Usage error when no
    /// HOME exists — either shape is contract).
    #[test]
    fn check_config_takes_config_dir_only() {
        assert_eq!(
            parse_check_config_args(&args(&["--config-dir", "/tmp/c"])).expect("parses"),
            PathBuf::from("/tmp/c")
        );
        parse_check_config_args(&args(&["--state-dir", "/tmp/s"])).unwrap_err();
        match parse_check_config_args(&args(&[])) {
            Ok(dir) => assert_eq!(dir.file_name().unwrap(), "herdr-governor"),
            Err(err) => {
                assert_eq!(err.code(), std::process::ExitCode::from(2));
            }
        }
    }
}

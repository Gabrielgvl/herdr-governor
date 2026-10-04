//! `log` — the daemon's structured logging (§4.18, OQ-B decided): in-repo
//! `tracing` calls emitting `event` on `stderr`, one line each, INFO and
//! above. No `tracing-subscriber`/`log`/`env_logger` dependency — the
//! subscriber below is the ~60 lines it takes to satisfy
//! `tracing::Subscriber` ourselves (ponytail: ceiling is the fixed event
//! vocabulary; upgrade path is `tracing-subscriber` if the event surface
//! ever needs filtering beyond the level).
//!
//! **The safety contract — ids, sizes, digests only.** The helpers below
//! take only `&'static str`, `bool`, `usize`, `u64`, `&Path` (state-dir
//! paths), or `&str` that is itself an id/digest/test-spec vocabulary —
//! and the tests pin it: `log_helpers_take_only_ids_sizes_digests` scans the
//! daemon sources lexically for a field named `text`/`body`/`content`/
//! `payload`/`argv`/`env`/`raw`/`message` on a `tracing::` call and fails.
//! Fields are `Debug`-formatted by the collector.

use std::fmt;
use std::io::Write;
use std::path::Path;
use std::sync::Mutex;

use tracing::field::{Field, Visit};
use tracing::subscriber::Interest;
use tracing::{Event, Id, Level, Metadata, Subscriber};

/// `set_global_default` failed — the subscriber can only be installed once
/// per process; `daemon` is a `main()`-only path so a failure here is a
/// programming error, not a config one.
#[derive(Debug, thiserror::Error)]
#[error("log subscriber already installed: {0}")]
pub(super) struct LogInitError(#[from] tracing::subscriber::SetGlobalDefaultError);

/// Install the stderr subscriber at INFO. Called once by `main`'s
/// `daemon` arm before `daemon::run`; `check-config` runs silent (its
/// verdict is the stdout line / the exit code, per §4.3 step 2).
pub(super) fn init() -> Result<(), LogInitError> {
    tracing::subscriber::set_global_default(StderrLog {
        max: Level::INFO,
        sink: Mutex::new(Box::new(std::io::stderr())),
    })?;
    Ok(())
}

/// The minimal subscriber: writes `LEVEL target: message field…` lines to
/// `sink` (stderr in the daemon, an owned buffer in tests). Spans are
/// accepted-and-ignored — the daemon's events carry every value as fields
/// (the §4.18 contract); `new_span` still returns a real `Id` so `tracing`
/// never falls back to `Id::disabled()`.
struct StderrLog {
    max: Level,
    sink: Mutex<Box<dyn Write + Send>>,
}

/// Collects `event` fields into `name = debug` pairs; `message` (the
/// format-arg field) renders first without its name.
#[derive(Default)]
struct Fields {
    message: Option<String>,
    fields: Vec<String>,
}

impl Visit for Fields {
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        if field.name() == "message" {
            self.message = Some(format!("{value:?}"));
        } else {
            self.fields.push(format!("{}={value:?}", field.name()));
        }
    }
}

impl Subscriber for StderrLog {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        metadata.level() <= &self.max
    }

    fn register_callsite(&self, _meta: &'static Metadata<'static>) -> Interest {
        // `sometimes` — `enabled` is consulted per event so the level
        // bound actually filters (`always` would short-circuit it).
        Interest::sometimes()
    }

    fn new_span(&self, _span: &tracing::span::Attributes<'_>) -> Id {
        Id::from_u64(1)
    }

    fn record(&self, _span: &Id, _values: &tracing::span::Record<'_>) {}

    fn record_follows_from(&self, _span: &Id, _follows: &Id) {}

    fn event(&self, event: &Event<'_>) {
        let mut fields = Fields::default();
        event.record(&mut fields);
        let mut line = format!(
            "{} {}:",
            event.metadata().level(),
            event.metadata().target()
        );
        if let Some(message) = fields.message {
            line.push(' ');
            line.push_str(&message);
        }
        for field in &fields.fields {
            line.push(' ');
            line.push_str(field);
        }
        line.push('\n');
        if let Ok(mut sink) = self.sink.lock() {
            let _unused = sink.write_all(line.as_bytes());
        }
    }

    fn enter(&self, _span: &Id) {}

    fn exit(&self, _span: &Id) {}
}

// — The safe helpers (the pinned surface) ————————————————————————————

/// A named startup step reached: `state`, `config`, `lock`, `socket`,
/// `store`, `restart`, `bind` — vocabulary only; the step name is a
/// `&'static str`.
pub(super) fn step(name: &'static str) {
    tracing::info!(step = name, "startup step");
}

/// The state dir in use — a daemon-owned path, safe to print.
pub(super) fn state_dir(path: &Path) {
    tracing::info!(state_dir = ?path, "state dir");
}

/// The stale-socket cleanup: the lock proved no live owner, so the file
/// was unlinked before bind (§4.3 step 3). `present` says a file was there.
pub(super) fn stale_socket_removed(present: bool) {
    tracing::info!(stale_socket = present, "removed stale socket");
}

/// Restart marking (§4.3 step 5): counts of dispatching effects
/// reclassified `unconfirmed` — effect/run/eval counts only, never keys.
pub(super) fn restart_marks(effects: usize, runs: usize, evals: usize) {
    tracing::info!(
        dispatching_effects = effects,
        runs_restarted = runs,
        evals_abstained = evals,
        "restart marked dispatching effects unconfirmed"
    );
}

/// One coordinator apply: how many state changes / events / effects the
/// `Transition` carried and which attempt landed it. Sizes only.
pub(super) fn applied(attempts: usize, changes: usize, events: usize, effects: usize) {
    tracing::debug!(
        attempts,
        state_changes = changes,
        events,
        effects,
        "transition applied"
    );
}

/// A coordinator apply dropped after the bounded retry — the attempt count
/// and an error *kind* (`conflict`, `apply`, `store`), never the payload.
pub(super) fn apply_dropped(attempts: usize, error_kind: &'static str) {
    tracing::warn!(
        attempts,
        error_kind,
        "transition dropped after bounded retry"
    );
}

/// A restart-lost `base_commit` pin whose re-probe failed (§4.5/F6) —
/// the Launch abstains; the git error (paths, stderr) is never printed.
pub(super) fn base_reprobe_failed() {
    tracing::warn!("base_commit re-probe failed; launch abstains");
}

/// A `SIGHUP` reload that was retained: `kind` names the refusal category
/// (`decode`/`read`/`invalid`/`missing-daemon`), never the error text.
pub(super) fn config_retained(kind: &'static str) {
    tracing::warn!(error_kind = kind, "config reload retained previous catalog");
}

/// A `SIGHUP` reload adopted: the new catalog's config-version digest.
pub(super) fn config_adopted(version: &str) {
    tracing::info!(config_version = version, "config reload adopted");
}

/// The adopted catalog changed `[daemon]` settings — the spawned tasks
/// keep the startup table until restart, so the line says the settings
/// are deferred rather than the false "retained previous catalog".
pub(super) fn config_daemon_deferred() {
    tracing::info!("config reload adopted; daemon settings take effect on restart");
}

/// One tick: whether the Herdr snapshot answered, and how many panes the
/// snapshot carries — a size, never pane contents.
pub(super) fn tick(answered: bool, panes: usize) {
    tracing::debug!(herdr_answered = answered, panes, "herdr tick");
}

/// The fault seam arming state — the matcher suffix and boundary are
/// test-specified vocabulary, safe to print (§4.4 discloses the seam).
pub(super) fn seam_armed(suffix: &str, boundary: &'static str, action: &'static str) {
    tracing::info!(seam = suffix, boundary, action, "fault seam armed");
}

/// The socket path the listener bound — a daemon-owned path.
pub(super) fn bound(path: &Path) {
    tracing::info!(socket = ?path, "listener bound");
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::sync::{Arc, Mutex};

    use tracing::subscriber::with_default;

    use super::{StderrLog, init};

    /// A sink the test subscriber writes into so a real `tracing::event!`
    /// can be asserted end to end.
    #[derive(Clone, Default)]
    struct SharedSink(Arc<Mutex<Vec<u8>>>);

    impl Write for SharedSink {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            let mut guard = self
                .0
                .lock()
                .map_err(|poisoned| std::io::Error::other(poisoned.to_string()))?;
            guard.write(bytes)
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// One real event through the subscriber lands as one line:
    /// `INFO target: "message" named=debug` — the `message` field is the
    /// format-arg and renders first; named fields are `name = debug`.
    #[test]
    fn subscriber_writes_level_message_and_debug_fields() {
        let sink = SharedSink::default();
        let seen = Arc::clone(&sink.0);
        let log = StderrLog {
            max: tracing::Level::INFO,
            sink: Mutex::new(Box::new(sink)),
        };
        with_default(log, || {
            tracing::info!(run_id = "abc123", n = 2usize, "applied");
            tracing::debug!("filtered below max");
        });
        let out = String::from_utf8(seen.lock().expect("sink").clone()).expect("utf8");
        assert_eq!(out.lines().count(), 1, "only the enabled event: {out}");
        assert!(
            out.contains("INFO") && out.contains("applied"),
            "level + message: {out}"
        );
        assert!(
            out.contains("run_id=\"abc123\"") && out.contains("n=2"),
            "named debug fields: {out}"
        );
    }

    /// Installing the global subscriber twice fails the second time — the
    /// double-init path is the only failure mode. (`set_global_default` is
    /// process-global; whichever test installs first owns it, and the
    /// second install is the typed `LogInitError`.)
    #[test]
    fn log_init_installs_once() {
        let _first = init();
        assert!(init().is_err(), "second global install always fails");
    }
}

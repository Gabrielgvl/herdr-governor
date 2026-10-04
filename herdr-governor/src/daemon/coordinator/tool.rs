//! `tool` — the `Msg::Tool` arm's work (F1 + F7 + §4.5): the caller
//! envelope resolves over the request-time snapshot, a first-use
//! `BindCaller` journals, and the call dispatches — `herdr_status`
//! answers; `herdr_launch` admits or parks.

use governor_core::identity::{CallerEnvelope, CallerKey, Timestamp};
use governor_core::lifecycle::{StateChange, Transition};

use crate::adapters::herdr::{HerdrError, Observed, SessionSnapshot};
use crate::daemon::api::{ToolCall, ToolError, ToolPrepared, ToolRequest};
use crate::daemon::launch::Admission;
use crate::daemon::{identity, status};

use super::Coordinator;
use super::apply::apply_with_retry;

impl Coordinator {
    /// The `Msg::Tool` arm's work: F1 resolves and binds the caller
    /// (`resolve_and_bind`), then the call dispatches — `herdr_status`
    /// answers directly; `herdr_launch` either answers (a refusal, an
    /// idempotent terminal replay) or `Park`s, and the arm puts the
    /// reply under the Launch's waiters.
    pub(in crate::daemon) fn tool(
        &mut self,
        request: ToolRequest,
        snapshot: Result<Observed<SessionSnapshot>, HerdrError>,
        prepared: ToolPrepared,
    ) -> Admission {
        let now = self.clock.now();
        let caller = match self.resolve_and_bind(
            &request.caller,
            snapshot,
            prepared.resolved_root.as_deref(),
        ) {
            Ok(caller) => caller,
            Err(error) => return Admission::Answer(Err(error)),
        };
        match request.call {
            ToolCall::Status { event, cursor } => Admission::Answer(status::page(
                &self.store,
                &caller,
                event.as_ref(),
                cursor.as_deref(),
                status::BYTE_BUDGET,
                &self.status_view(now),
            )),
            ToolCall::Launch { key, task } => {
                let project_root = request.caller.project_root.clone();
                self.launch_admit(&caller, &key, task, &project_root, prepared)
            }
            ToolCall::Run(_) => Admission::Answer(Err(ToolError::new(
                ToolError::TOOL_UNKNOWN,
                "herdr_run lands with C2",
            ))),
        }
    }

    /// The `Msg::VerifyCaller` arm (F1): the framed requests `mcp::serve`
    /// answers locally — `initialize`, `ping`, `tools/list`, unknown
    /// methods — carry the relay's `relayInstanceId` too, so the first
    /// one binds it and every later one verifies it exactly as a tool
    /// call does; the reply is the verdict alone.
    pub(in crate::daemon) fn verify(
        &mut self,
        envelope: &CallerEnvelope,
        snapshot: Result<Observed<SessionSnapshot>, HerdrError>,
        resolved_root: Option<&str>,
    ) -> Result<(), ToolError> {
        self.resolve_and_bind(envelope, snapshot, resolved_root)
            .map(|_caller| ())
    }

    /// F1 (§4.6) — the work `tool` and `verify` share: validate the
    /// envelope, resolve the caller over the request-time snapshot and
    /// verify or mint the persisted `relayInstanceId` binding —
    /// journaled here before the request proceeds; a bound id
    /// re-resolving to a different native session refuses
    /// `CALLER_IDENTITY_MISMATCH`.
    ///
    /// A failed snapshot is `DAEMON_UNAVAILABLE` — an internal fault,
    /// never an identity verdict; a `BindCaller` apply that keeps losing
    /// the CAS drops (the next request re-mints the same binding).
    fn resolve_and_bind(
        &mut self,
        envelope: &CallerEnvelope,
        snapshot: Result<Observed<SessionSnapshot>, HerdrError>,
        resolved_root: Option<&str>,
    ) -> Result<CallerKey, ToolError> {
        let now = self.clock.now();
        // The request-time read is a real Herdr read — it feeds the one
        // health record (§4.7) like the tick/observation reads do, Err
        // included; the verdict then answers `DAEMON_UNAVAILABLE` as
        // before, never an identity verdict.
        self.health.record(&snapshot, now);
        let Ok(observed) = snapshot else {
            return Err(ToolError::new(
                ToolError::DAEMON_UNAVAILABLE,
                "request-time herdr snapshot unavailable",
            ));
        };
        let agents = identity::agent_rows(&observed.value);
        // The freshest view the coordinator holds — `open_tabs`,
        // placement counts and caller-pane resolution read it (§4.5).
        self.latest_snapshot = Some(observed);
        let (caller, binding) = identity::resolve(&self.store, envelope, resolved_root, &agents)?;
        if let Some(fresh) = binding {
            apply_with_retry(&mut self.store, now, |_| Transition {
                state_changes: vec![StateChange::BindCaller(fresh.clone())],
                events: Vec::new(),
                effects: Vec::new(),
            })
            .map_err(|error| {
                ToolError::new(
                    ToolError::DAEMON_UNAVAILABLE,
                    format!("caller bind apply: {error}"),
                )
            })?;
        }
        Ok(caller)
    }

    /// The last good snapshot's record — test-only: `health` is written
    /// by the Tick arm and every tool call's request-time read, and a
    /// status page always renders the latter, so the tick arm's own
    /// write is observable only through this read (F13).
    #[cfg(test)]
    pub(in crate::daemon) fn herdr_seen(
        &self,
    ) -> Option<(Timestamp, governor_core::identity::HerdrIncarnation)> {
        self.health
            .last_ok()
            .zip(self.health.incarnation().cloned())
    }

    /// F7 — what `status::page` needs of coordinator state, as values.
    fn status_view(&self, now: Timestamp) -> status::StatusView {
        status::StatusView {
            now,
            pid: std::process::id(),
            uptime_secs: u64::try_from(now.0.saturating_sub(self.started_at.0))
                .unwrap_or(0)
                .saturating_div(1_000),
            version: env!("CARGO_PKG_VERSION"),
            herdr: self
                .health
                .last_ok()
                .zip(self.health.incarnation())
                .map(|(at, incarnation)| status::HerdrHealth {
                    at,
                    incarnation: incarnation.0.clone(),
                }),
            config: status::ConfigHealth {
                valid: self.config_last_error.is_none(),
                version: self.loaded.version.clone(),
                last_good_at: self.config_adopted_at,
                last_error: self.config_last_error.map(str::to_owned),
            },
        }
    }
}

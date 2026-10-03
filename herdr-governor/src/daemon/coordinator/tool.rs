//! `tool` — the `Msg::Tool` arm's work (F1 + F7): the caller envelope
//! resolves over the request-time snapshot, a first-use `BindCaller`
//! journals, and the call dispatches — `herdr_status` is PR A's only
//! tool (§7).

use governor_core::identity::Timestamp;
use governor_core::lifecycle::{StateChange, Transition};

use crate::adapters::herdr::{HerdrError, Observed, SessionSnapshot};
use crate::daemon::api::{ToolCall, ToolError, ToolRequest, ToolResponse};
use crate::daemon::{identity, status};

use super::{Coordinator, apply_with_retry};

impl Coordinator {
    /// The `Msg::Tool` arm's work: F1 validates the envelope, resolves
    /// the caller over the request-time snapshot and verifies or mints
    /// the persisted `relayInstanceId` binding (journaled here before
    /// the call runs); then the call dispatches — `herdr_status` is
    /// PR A's only tool (§7).
    ///
    /// A failed snapshot is `DAEMON_UNAVAILABLE` — an internal fault,
    /// never an identity verdict; a `BindCaller` apply that keeps losing
    /// the CAS drops (the next request re-mints the same binding).
    pub(in crate::daemon) fn tool(
        &mut self,
        request: ToolRequest,
        snapshot: Result<Observed<SessionSnapshot>, HerdrError>,
        resolved_root: Option<&str>,
    ) -> ToolResponse {
        let now = self.clock.now();
        let Ok(observed) = snapshot else {
            return Err(ToolError::new(
                ToolError::DAEMON_UNAVAILABLE,
                "request-time herdr snapshot unavailable",
            ));
        };
        self.herdr_seen = Some((now, identity::incarnation(&observed.epoch)));
        let agents = identity::agent_rows(&observed.value);
        let (caller, binding) =
            identity::resolve(&self.store, &request.caller, resolved_root, &agents)?;
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
        match request.call {
            ToolCall::Status { event, cursor } => status::page(
                &self.store,
                &caller,
                event.as_ref(),
                cursor.as_deref(),
                status::BYTE_BUDGET,
                &self.status_view(now),
            ),
            ToolCall::Launch { .. } | ToolCall::Run(_) => Err(ToolError::new(
                ToolError::TOOL_UNKNOWN,
                "PR A serves herdr_status only",
            )),
        }
    }

    /// The last good snapshot's record — test-only: `herdr_seen` is
    /// written by the Tick arm and every tool call's request-time read,
    /// and a status page always renders the latter, so the tick arm's
    /// own write is observable only through this read (F13).
    #[cfg(test)]
    pub(in crate::daemon) fn herdr_seen(
        &self,
    ) -> Option<(Timestamp, governor_core::identity::HerdrIncarnation)> {
        self.herdr_seen.clone()
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
                .herdr_seen
                .as_ref()
                .map(|(at, incarnation)| status::HerdrHealth {
                    at: *at,
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

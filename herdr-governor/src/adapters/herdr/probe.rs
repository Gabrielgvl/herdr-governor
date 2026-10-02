//! The `herdr_probe` op battery (P4.H3): the confirmed protocol-22 subset
//! driven through the real H1 `Client` against whatever socket the caller
//! names — the fake in the H2 suite, a dedicated isolated probe session on
//! the live leg. Every op lands as one [`OpReport`] with a verdict the
//! Python contract suite asserts on; nothing here inspects the
//! environment or chooses a socket (the bin owns that policy, including
//! the never-the-live-socket guard).
//!
//! The battery is read-only until the topology leg, which creates one tab
//! (`tab.create`), splits it, reads it, arms a subscription on it, proves
//! the typed `agent_not_found` refusals on a shell pane, and closes every
//! pane it created — so the session it ran in is left as it was found.
//! `agent.start` is deliberately absent: it would spawn a real harness
//! from a probe (I9 forbids naming one here anyway); the H2 suite proves
//! that decode path against the fake.

use std::time::Duration;

use serde::Serialize;
use serde_json::{Value, json};

use super::codec::HerdrError;
use super::conn::Client;
use super::ops::{AgentPromptParams, PaneSplitParams, ReadOpts, TabCreateParams};
use super::types::{Observed, ReadSource, SplitDirection, SubscriptionSpec};

/// One op's verdict: `ok` is the contract check (a reply decoded to the
/// expected type, or the expected typed error), `detail` the redacted
/// evidence — ids, counts and lengths, never pane text or paths.
#[derive(Debug, Clone, Serialize)]
pub struct OpReport {
    /// The wire method, or `<method>:<leg>` for a typed-error check.
    pub op: String,
    /// The verdict.
    pub ok: bool,
    /// Redacted evidence for the verdict.
    pub detail: Value,
}

/// The whole battery's report, as the bin prints it.
#[derive(Debug, Clone, Serialize)]
pub struct Report {
    /// The per-op verdicts in run order.
    pub ops: Vec<OpReport>,
    /// Every op passed.
    pub all_ok: bool,
}

/// The label the probe's tab carries so an operator can spot a leftover.
const TAB_LABEL: &str = "gov-probe";
/// An agent name nothing registers — the `agent_not_found` leg.
const ABSENT_AGENT: &str = "gov-probe-absent";

/// Run the battery against `client`, every op under `deadline`.
pub async fn battery(client: &Client, deadline: Duration) -> Report {
    let mut ops = Vec::new();
    let pong = record(
        &mut ops,
        "ping",
        client.ping(deadline).await,
        |p| json!({"version": p.version, "protocol": p.protocol}),
    );
    let snapshot = client.session_snapshot(deadline).await;
    record(&mut ops, "session.snapshot", snapshot, |s| {
        json!({"protocol": s.protocol, "workspaces": s.workspaces.len(),
               "tabs": s.tabs.len(), "panes": s.panes.len(),
               "agents": s.agents.len()})
    });
    record(
        &mut ops,
        "agent.list",
        client.agent_list(deadline).await,
        |a| json!({"agents": a.len()}),
    );
    if pong.is_some() {
        topology_leg(client, deadline, &mut ops).await;
    }
    let all_ok = ops.iter().all(|r| r.ok);
    Report { ops, all_ok }
}

/// The mutating leg: one tab in, every created pane closed on the way out.
/// A failed step records its verdict and ends the leg — later steps would
/// only compound the failure, and a half-built tab is still torn down.
async fn topology_leg(client: &Client, deadline: Duration, ops: &mut Vec<OpReport>) {
    let params = TabCreateParams {
        label: Some(TAB_LABEL.to_owned()),
        ..TabCreateParams::default()
    };
    let Some(tab) = record(
        ops,
        "tab.create",
        client.tab_create(&params, deadline).await,
        |t| json!({"tab_id": t.tab.tab_id, "root_pane": t.root_pane.pane_id}),
    ) else {
        return;
    };
    let root = tab.root_pane.pane_id;
    record(
        ops,
        "pane.get",
        client.pane_get(&root, deadline).await,
        |p| {
            json!({"pane_id": p.pane_id, "terminal_id": p.terminal_id,
               "agent_status": p.agent_status})
        },
    );
    let mut split = PaneSplitParams::new(SplitDirection::Right);
    split.target_pane_id = Some(root.clone());
    let new_pane = record(
        ops,
        "pane.split",
        client.pane_split(&split, deadline).await,
        |p| json!({"pane_id": p.pane_id, "tab_id": p.tab_id}),
    )
    .map(|p| p.pane_id);
    let read = client
        .pane_read(&root, ReadSource::Visible, &ReadOpts::default(), deadline)
        .await;
    record(ops, "pane.read", read, |r| {
        json!({"revision": r.revision, "text_len": r.text.len(),
               "truncated": r.truncated, "source": r.source})
    });
    let spec = SubscriptionSpec::ScrollChanged {
        pane_id: root.clone(),
    };
    match client.subscribe(vec![spec], deadline).await {
        Ok(sub) => ops.push(pass(
            "events.subscribe",
            json!({"epoch_seq": sub.epoch().seq}),
        )),
        Err(e) => ops.push(fail("events.subscribe", &e)),
    }
    expect_not_found(
        ops,
        "agent.get:absent",
        client.agent_get(ABSENT_AGENT, deadline).await,
    );
    let prompt = AgentPromptParams {
        target: root.clone(),
        text: TAB_LABEL.to_owned(),
    };
    expect_not_found(
        ops,
        "agent.prompt:shell-pane",
        client.agent_prompt(&prompt, deadline).await,
    );
    for pane in new_pane.iter().chain(std::iter::once(&root)) {
        record(
            ops,
            "pane.close",
            client.pane_close(pane, deadline).await,
            |()| json!({"pane_id": pane}),
        );
    }
}

/// Record one op: `Ok` → pass with the caller's redacted detail, `Err` →
/// fail with the typed error's display. Returns the value for chaining.
fn record<T>(
    ops: &mut Vec<OpReport>,
    op: &str,
    result: Result<Observed<T>, HerdrError>,
    detail: impl FnOnce(&T) -> Value,
) -> Option<T> {
    match result {
        Ok(observed) => {
            ops.push(pass(op, detail(&observed.value)));
            Some(observed.value)
        }
        Err(e) => {
            ops.push(fail(op, &e));
            None
        }
    }
}

/// Record a leg whose contract is the typed `agent_not_found` refusal.
fn expect_not_found<T>(ops: &mut Vec<OpReport>, op: &str, result: Result<T, HerdrError>) {
    let report = match result {
        Err(HerdrError::AgentNotFound { message }) => {
            pass(op, json!({"error": "agent_not_found", "message": message}))
        }
        Err(e) => fail(op, &e),
        Ok(_) => OpReport {
            op: op.to_owned(),
            ok: false,
            detail: json!({"unexpected": "ok reply where agent_not_found was owed"}),
        },
    };
    ops.push(report);
}

fn pass(op: &str, detail: Value) -> OpReport {
    OpReport {
        op: op.to_owned(),
        ok: true,
        detail,
    }
}

fn fail(op: &str, error: &HerdrError) -> OpReport {
    OpReport {
        op: op.to_owned(),
        ok: false,
        detail: json!({"error": error.to_string()}),
    }
}

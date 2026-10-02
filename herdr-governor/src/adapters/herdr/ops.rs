//! The typed operations of the confirmed protocol-22 subset — exactly the
//! node-contract list and no speculative methods: `ping`,
//! `session.snapshot`, `pane.{get,read,split,close}`, `tab.create`,
//! `agent.{start,prompt,get,list,read}`, `events.subscribe`. `events.wait`
//! stays out — armed subscriptions cover the need (recorded: it rejects
//! every match kind but pane agent-status anyway).
//!
//! The param structs mirror the schema's `*Params` shapes; optional wire
//! fields are `Option` and omitted when `None` (server defaults ride).
//! Params the subset never uses are left out rather than serialized
//! (`pane.split`'s `right_click`, `agent.prompt`'s `wait`/`until` —
//! unexercised in evidence, trivially added when a caller needs them).

use std::collections::BTreeMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::mpsc;

use super::codec::{self, HerdrError, malformed, typed};
use super::conn::{self, Client, Subscription};
use super::types::{
    AgentInfo, Observed, PaneInfo, PaneRead, ReadFormat, ReadSource, SessionSnapshot,
    SplitDirection, SubscriptionSpec, TabInfo,
};

/// `ping` reply — `type:"pong"` carries `version`, `protocol`, and the
/// opaque `capabilities` block (`type` itself is checked by the op before
/// this decodes).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Pong {
    /// The server version string.
    pub version: String,
    /// The protocol revision (22 in the pinned schema).
    pub protocol: u32,
    /// `capabilities` — opaque; nothing in the subset consumes it.
    #[serde(default)]
    pub capabilities: Option<Value>,
}

/// `tab.create` reply (`type:"tab_created"`).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct TabCreated {
    /// The new tab.
    pub tab: TabInfo,
    /// The tab's root pane.
    pub root_pane: PaneInfo,
}

/// `agent.start` reply (`type:"agent_started"`): the registered agent
/// record plus the argv the server ran.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct AgentStarted {
    /// The agent record — `interactive_ready`/`agent_status`/`agent_session`
    /// are the readiness surface (advisory, per the A3 ruling).
    pub agent: AgentInfo,
    /// The argv the server invoked.
    pub argv: Vec<String>,
}

/// `agent.prompt` reply (`type:"agent_prompted"`): the ack is a
/// delivery/target snapshot — proof the prompt was delivered to the pane,
/// never that it was consumed (a3 evidence: `agent_status` inside still
/// reads pre-dispatch; no prompt-id or delivery sequence exists).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct AgentPrompted {
    /// The target's agent record at dispatch time.
    pub agent: AgentInfo,
}

/// `pane.split` params (`PaneSplitParams`): `direction` is required, the
/// rest ride server defaults when `None`.
#[derive(Debug, Clone, Serialize)]
pub struct PaneSplitParams {
    /// `direction` — `right`/`down`.
    pub direction: SplitDirection,
    /// `target_pane_id` — split this pane rather than the focused one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_pane_id: Option<String>,
    /// `workspace_id`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    /// `cwd` — the new pane's directory (a bad path does not fail; the
    /// pane reports its fallback `cwd` — a3 evidence).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// `env` — extra environment for the new pane.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env: Option<BTreeMap<String, String>>,
    /// `focus` — focus the new pane (wire default false).
    pub focus: bool,
    /// `ratio` — the split ratio.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ratio: Option<f64>,
}

impl PaneSplitParams {
    /// A split with every option at the wire default.
    #[must_use]
    pub fn new(direction: SplitDirection) -> Self {
        Self {
            direction,
            target_pane_id: None,
            workspace_id: None,
            cwd: None,
            env: None,
            focus: false,
            ratio: None,
        }
    }
}

/// `tab.create` params (`TabCreateParams`) — every field optional.
#[derive(Debug, Clone, Default, Serialize)]
pub struct TabCreateParams {
    /// `workspace_id`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    /// `label`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// `cwd`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// `env`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env: Option<BTreeMap<String, String>>,
    /// `focus` (wire default false).
    pub focus: bool,
}

/// `agent.start` params (`AgentStartParams`).
#[derive(Debug, Clone, Serialize)]
pub struct AgentStartParams {
    /// `name` — the minted agent name.
    pub name: String,
    /// `kind` — the harness kind (opaque catalog data).
    pub kind: String,
    /// `pane_id` — the shell pane to start on.
    pub pane_id: String,
    /// `args` — extra argv after the harness binary.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    /// `timeout_ms` — the server-side startup bound (schema: >3000 and
    /// ≤300000). Distinct from the op's `deadline`, which should exceed it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
}

/// `agent.prompt` params (`AgentPromptParams`).
#[derive(Debug, Clone, Serialize)]
pub struct AgentPromptParams {
    /// `target` — the agent (name or pane).
    pub target: String,
    /// `text` — the prompt body.
    pub text: String,
}

/// The optional fields `pane.read`/`agent.read` share (`lines`, `format`,
/// `strip_ansi`); `None` rides the server defaults (`text`, stripped).
#[derive(Debug, Clone, Default, Serialize)]
pub struct ReadOpts {
    /// `lines` — the tail-line bound.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lines: Option<u32>,
    /// `format` — `text`/`ansi`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format: Option<ReadFormat>,
    /// `strip_ansi`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strip_ansi: Option<bool>,
}

#[derive(Serialize)]
struct SubscribeParams<'a> {
    subscriptions: &'a [SubscriptionSpec],
}

#[derive(Serialize)]
struct PaneTarget<'a> {
    pane_id: &'a str,
}

#[derive(Serialize)]
struct AgentTarget<'a> {
    target: &'a str,
}

#[derive(Serialize)]
struct PaneReadParams<'a> {
    pane_id: &'a str,
    source: ReadSource,
    #[serde(flatten)]
    opts: &'a ReadOpts,
}

#[derive(Serialize)]
struct AgentReadParams<'a> {
    target: &'a str,
    source: ReadSource,
    #[serde(flatten)]
    opts: &'a ReadOpts,
}

#[derive(Deserialize)]
struct SnapshotReply {
    snapshot: SessionSnapshot,
}

#[derive(Deserialize)]
struct PaneReply {
    pane: PaneInfo,
}

#[derive(Deserialize)]
struct ReadReply {
    read: PaneRead,
}

#[derive(Deserialize)]
struct AgentReply {
    agent: AgentInfo,
}

#[derive(Deserialize)]
struct AgentsReply {
    agents: Vec<AgentInfo>,
}

impl Client {
    /// `ping` — `type:"pong"`.
    pub async fn ping(&self, deadline: Duration) -> Result<Observed<Pong>, HerdrError> {
        let reply = self
            .request("ping", &serde_json::Map::new(), deadline)
            .await?;
        typed::<Pong>(reply, "pong")
    }

    /// `session.snapshot` — the full topology record.
    pub async fn session_snapshot(
        &self,
        deadline: Duration,
    ) -> Result<Observed<SessionSnapshot>, HerdrError> {
        let reply = self
            .request("session.snapshot", &serde_json::Map::new(), deadline)
            .await?;
        typed::<SnapshotReply>(reply, "session_snapshot").map(|o| o.map(|r| r.snapshot))
    }

    /// `pane.get` — one pane record; the reply const is `pane_info`.
    pub async fn pane_get(
        &self,
        pane_id: &str,
        deadline: Duration,
    ) -> Result<Observed<PaneInfo>, HerdrError> {
        let reply = self
            .request("pane.get", &PaneTarget { pane_id }, deadline)
            .await?;
        typed::<PaneReply>(reply, "pane_info").map(|o| o.map(|r| r.pane))
    }

    /// `pane.read` — a bounded terminal read of the pane.
    pub async fn pane_read(
        &self,
        pane_id: &str,
        source: ReadSource,
        opts: &ReadOpts,
        deadline: Duration,
    ) -> Result<Observed<PaneRead>, HerdrError> {
        self.read_op(
            "pane.read",
            &PaneReadParams {
                pane_id,
                source,
                opts,
            },
            deadline,
        )
        .await
    }

    /// `pane.split` — the reply is a `pane_info` result naming the new
    /// pane (a46 evidence), not a dedicated `pane_split` type.
    pub async fn pane_split(
        &self,
        params: &PaneSplitParams,
        deadline: Duration,
    ) -> Result<Observed<PaneInfo>, HerdrError> {
        let reply = self.request("pane.split", params, deadline).await?;
        typed::<PaneReply>(reply, "pane_info").map(|o| o.map(|r| r.pane))
    }

    /// `pane.close` — the reply is the bare `ok` result.
    pub async fn pane_close(
        &self,
        pane_id: &str,
        deadline: Duration,
    ) -> Result<Observed<()>, HerdrError> {
        let reply = self
            .request("pane.close", &PaneTarget { pane_id }, deadline)
            .await?;
        let got = reply.value.get("type").and_then(Value::as_str);
        if got != Some("ok") {
            return Err(malformed(format!("expected \"ok\", got {got:?}")));
        }
        Ok(reply.map(|_| ()))
    }

    /// `tab.create` — `type:"tab_created"` carries the new tab and its
    /// root pane.
    pub async fn tab_create(
        &self,
        params: &TabCreateParams,
        deadline: Duration,
    ) -> Result<Observed<TabCreated>, HerdrError> {
        let reply = self.request("tab.create", params, deadline).await?;
        typed::<TabCreated>(reply, "tab_created")
    }

    /// `agent.start` — blocks server-side to readiness (the ~3 s
    /// `interactive_ready` return). Every runtime startup failure reports
    /// back as wire `timeout` — typed `HerdrError::Timeout`, the F15 "any
    /// other outcome"; the only candidate-fallback shape is the pre-flight
    /// `AgentPaneBusy`.
    pub async fn agent_start(
        &self,
        params: &AgentStartParams,
        deadline: Duration,
    ) -> Result<Observed<AgentStarted>, HerdrError> {
        let reply = self.request("agent.start", params, deadline).await?;
        typed::<AgentStarted>(reply, "agent_started")
    }

    /// `agent.prompt` — the `agent_prompted` ack proves delivery to the
    /// pane, never consumption.
    pub async fn agent_prompt(
        &self,
        params: &AgentPromptParams,
        deadline: Duration,
    ) -> Result<Observed<AgentPrompted>, HerdrError> {
        let reply = self.request("agent.prompt", params, deadline).await?;
        typed::<AgentPrompted>(reply, "agent_prompted")
    }

    /// `agent.get` — one agent record (`type:"agent_info"`).
    pub async fn agent_get(
        &self,
        target: &str,
        deadline: Duration,
    ) -> Result<Observed<AgentInfo>, HerdrError> {
        let reply = self
            .request("agent.get", &AgentTarget { target }, deadline)
            .await?;
        typed::<AgentReply>(reply, "agent_info").map(|o| o.map(|r| r.agent))
    }

    /// `agent.list` — every registered agent (`type:"agent_list"`). During
    /// the start window a row lacks `interactive_ready` — it stays `None`.
    pub async fn agent_list(
        &self,
        deadline: Duration,
    ) -> Result<Observed<Vec<AgentInfo>>, HerdrError> {
        let reply = self
            .request("agent.list", &serde_json::Map::new(), deadline)
            .await?;
        typed::<AgentsReply>(reply, "agent_list").map(|o| o.map(|r| r.agents))
    }

    /// `agent.read` — a terminal read of the agent's pane; the wire answer
    /// is the same `pane_read` result (`agent_read` does not exist in the
    /// schema's result union — fixture over intuition).
    pub async fn agent_read(
        &self,
        target: &str,
        source: ReadSource,
        opts: &ReadOpts,
        deadline: Duration,
    ) -> Result<Observed<PaneRead>, HerdrError> {
        self.read_op(
            "agent.read",
            &AgentReadParams {
                target,
                source,
                opts,
            },
            deadline,
        )
        .await
    }

    /// `events.subscribe` — arm the subscriptions and stream typed events
    /// on a held-open connection until EOF. The deadline covers connect +
    /// arm only; an armed stream stays live past it (recorded: idle 8 s
    /// still delivers). A failed arm arrives under the derived id
    /// `<request>:sub:<i>:probe` — typed `SubscriptionFailed`.
    pub async fn subscribe(
        &self,
        subscriptions: Vec<SubscriptionSpec>,
        deadline: Duration,
    ) -> Result<Subscription, HerdrError> {
        let call = async {
            let mut conn = self.connect().await?;
            let id = format!("gov:{}", conn.epoch().seq);
            let frame = codec::encode_request(
                &id,
                "events.subscribe",
                &SubscribeParams {
                    subscriptions: &subscriptions,
                },
            )?;
            conn.send_request(&frame).await?;
            let reply = conn.read_reply(&id).await?;
            let got = reply.value.get("type").and_then(Value::as_str);
            if got != Some("subscription_started") {
                return Err(malformed(format!(
                    "expected \"subscription_started\", got {got:?}"
                )));
            }
            Ok((conn, id))
        };
        let (conn, id) = match tokio::time::timeout(deadline, call).await {
            Ok(result) => result?,
            Err(_) => return Err(HerdrError::DeadlineExceeded),
        };
        let epoch = conn.epoch();
        let (tx, rx) = mpsc::channel(64);
        let task = tokio::spawn(conn::drive(conn, id, tx));
        Ok(Subscription::armed(
            rx,
            task,
            epoch,
            self.clone(),
            subscriptions,
            deadline,
        ))
    }

    /// The shared tail of `pane.read`/`agent.read` — both answer
    /// `pane_read`.
    async fn read_op<P: Serialize>(
        &self,
        method: &str,
        params: &P,
        deadline: Duration,
    ) -> Result<Observed<PaneRead>, HerdrError> {
        let reply = self.request(method, params, deadline).await?;
        typed::<ReadReply>(reply, "pane_read").map(|o| o.map(|r| r.read))
    }
}

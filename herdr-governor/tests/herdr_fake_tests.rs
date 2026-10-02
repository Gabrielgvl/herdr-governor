//! P4.H2 contract tests: the real H1 client against the fake Herdr server
//! — happy paths per op and one test per fault knob proving the adapter
//! sees the evidence-shaped outcome. Every name carries the
//! `contract_fake_herdr_` prefix H3's fake leg filters on. Delays run on
//! the paused Tokio clock; nothing sleeps on the wall clock.

#[cfg(test)]
pub mod support;

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use herdr_governor::adapters::herdr::types::SplitDirection;
    use herdr_governor::adapters::herdr::{
        AgentInfo, AgentPromptParams, AgentStartParams, AgentStatus, Client, HerdrError,
        OutputMatch, PaneInfo, PaneRead, PaneSplitParams, ReadOpts, ReadSource, SessionSnapshot,
        SubEvent, Subscription, SubscriptionSpec, TabCreateParams,
    };
    use serde_json::{Value, json};
    use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
    use tokio::net::UnixStream;

    use crate::support::fake_herdr::{FakeHerdr, Fault, Topology};

    const TRACE: &str = include_str!("../../tests/fixtures/contract/a2-tools-daemon-trace.jsonl");
    const D: Duration = Duration::from_secs(5);

    fn start_params(name: &str, pane_id: &str) -> AgentStartParams {
        AgentStartParams {
            name: name.to_owned(),
            kind: "harness-x".to_owned(),
            pane_id: pane_id.to_owned(),
            args: vec!["--flag".to_owned()],
            timeout_ms: Some(30_000),
        }
    }

    fn marker_spec(pane_id: &str, marker: &str) -> SubscriptionSpec {
        SubscriptionSpec::OutputMatched {
            pane_id: pane_id.to_owned(),
            source: ReadSource::Recent,
            output_match: OutputMatch::Substring {
                value: marker.to_owned(),
            },
            lines: None,
            strip_ansi: None,
        }
    }

    fn scroll_spec(pane_id: &str) -> SubscriptionSpec {
        SubscriptionSpec::ScrollChanged {
            pane_id: pane_id.to_owned(),
        }
    }

    async fn start(client: &Client, name: &str, pane_id: &str) -> Result<AgentInfo, HerdrError> {
        let started = client.agent_start(&start_params(name, pane_id), D).await?;
        assert_eq!(started.value.argv, vec!["harness-x", "--flag"]);
        Ok(started.value.agent)
    }

    async fn snapshot(client: &Client) -> SessionSnapshot {
        client.session_snapshot(D).await.expect("snapshot").value
    }

    async fn agents(client: &Client) -> Vec<AgentInfo> {
        client.agent_list(D).await.expect("agent.list").value
    }

    async fn pane(client: &Client, pane_id: &str) -> Result<PaneInfo, HerdrError> {
        client.pane_get(pane_id, D).await.map(|o| o.value)
    }

    async fn read(client: &Client, pane_id: &str, source: ReadSource) -> PaneRead {
        let opts = ReadOpts::default();
        let got = client.pane_read(pane_id, source, &opts, D).await;
        got.expect("pane.read").value
    }

    async fn next(sub: &mut Subscription) -> SubEvent {
        sub.next().await.expect("stream open").expect("event")
    }

    async fn closed(sub: &mut Subscription) -> bool {
        matches!(sub.next().await, Some(Err(HerdrError::StreamClosed)))
    }

    /// Raw peer: one line in, reply lines out — until the fake closes, or
    /// just the first one (an armed subscription never closes).
    async fn raw_exchange(fake: &FakeHerdr, line: &[u8], until_eof: bool) -> Vec<Value> {
        let path = fake.socket_path();
        let mut reader = BufReader::new(UnixStream::connect(path).await.expect("connect"));
        reader.get_mut().write_all(line).await.expect("write");
        let mut replies = Vec::new();
        let mut buf = Vec::new();
        while reader.read_until(b'\n', &mut buf).await.expect("read") > 0 {
            replies.push(serde_json::from_slice(&buf).expect("reply json"));
            buf.clear();
            if !until_eof {
                break;
            }
        }
        replies
    }

    #[tokio::test]
    async fn contract_fake_herdr_ping_snapshot_and_agent_rows() {
        let fake = FakeHerdr::start(Topology::single_shell());
        let client = fake.client();
        assert_eq!(client.ping(D).await.expect("pong").value.protocol, 22);
        assert_eq!(fake.requests()[0].0, "ping");
        let empty = snapshot(&client).await;
        assert_eq!(empty.panes[0].pane_id, "w1:p1");
        assert!(empty.agent_rows().is_empty());
        start(&client, "a-1", "w1:p1").await.expect("start");
        let rows = snapshot(&client).await.agent_rows();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name.as_deref(), Some("a-1"));
        assert_eq!(rows[0].native_session.as_deref(), Some("a-1-session"));
        assert_eq!(rows[0].status, Some(AgentStatus::Idle));
    }

    #[tokio::test]
    async fn contract_fake_herdr_tab_split_get_close() {
        let fake = FakeHerdr::start(Topology::single_shell());
        let client = fake.client();
        let params = TabCreateParams::default();
        let tab = client.tab_create(&params, D).await.expect("tab").value;
        assert_eq!(tab.tab.tab_id, "w1:t2");
        assert_eq!(tab.root_pane.pane_id, "w1:p2");
        let mut split = PaneSplitParams::new(SplitDirection::Right);
        split.target_pane_id = Some("w1:p2".to_owned());
        let new = client.pane_split(&split, D).await.expect("split").value;
        assert_eq!(new.pane_id, "w1:p3");
        assert_eq!(new.tab_id, "w1:t2");
        assert_ne!(new.terminal_id, tab.root_pane.terminal_id);
        let got = pane(&client, "w1:p3").await.expect("get");
        assert_eq!(got.agent_status, AgentStatus::Unknown);
        client.pane_close("w1:p3", D).await.expect("close");
        let err = pane(&client, "w1:p3").await.expect_err("closed pane");
        assert!(matches!(err, HerdrError::PaneNotFound { .. }), "{err:?}");
    }

    #[tokio::test]
    async fn contract_fake_herdr_agent_start_prompt_get_list_read() {
        let fake = FakeHerdr::start(Topology::single_shell());
        let client = fake.client();
        let started = start(&client, "a-1", "w1:p1").await.expect("start");
        assert_eq!(started.interactive_ready, Some(true));
        let prompt = AgentPromptParams {
            target: "a-1".to_owned(),
            text: "ALPHA".to_owned(),
        };
        let ack = client.agent_prompt(&prompt, D).await.expect("ack").value;
        assert_eq!(ack.agent.name.as_deref(), Some("a-1"));
        assert_eq!(ack.agent.pane_id, "w1:p1");
        let got = client.agent_get("w1:p1", D).await.expect("get").value;
        assert_eq!(got.name.as_deref(), Some("a-1"));
        assert_eq!(agents(&client).await.len(), 1);
        let opts = ReadOpts::default();
        let via_agent = client
            .agent_read("a-1", ReadSource::Visible, &opts, D)
            .await
            .expect("agent.read")
            .value;
        assert_eq!(via_agent.text, "ALPHA\n");
        assert_eq!(via_agent.source, ReadSource::Visible);
        assert_eq!(read(&client, "w1:p1", ReadSource::Recent).await.revision, 1);
        let err = client.agent_get("nobody", D).await.expect_err("unknown");
        assert!(matches!(err, HerdrError::AgentNotFound { .. }), "{err:?}");
    }

    #[tokio::test]
    async fn contract_fake_herdr_subscribe_streams_events() {
        let fake = FakeHerdr::start(Topology::single_shell());
        let client = fake.client();
        let specs = vec![
            marker_spec("w1:p1", "GOV-MARKER"),
            scroll_spec("w1:p1"),
            SubscriptionSpec::AgentStatusChanged {
                pane_id: "w1:p1".to_owned(),
                agent_status: None,
            },
        ];
        let mut sub = client.subscribe(specs, D).await.expect("armed");
        fake.write_output("w1:p1", "noise\nGOV-MARKER here\n");
        fake.write_output("w1:p1", "GOV-MARKER again\n");
        let position = json!({"max_offset_from_bottom": 267, "offset_from_bottom": 120,
                              "viewport_rows": 40});
        fake.scroll("w1:p1", &position);
        start(&client, "a-1", "w1:p1").await.expect("start");
        fake.set_agent_status("w1:p1", "working");
        let SubEvent::OutputMatched {
            matched_line, read, ..
        } = next(&mut sub).await
        else {
            panic!("output_matched first")
        };
        assert_eq!(
            (matched_line.as_str(), read.revision),
            ("GOV-MARKER here", 1)
        );
        // One-shot: the second marker write never fires; scroll is next.
        let SubEvent::ScrollChanged { scroll, .. } = next(&mut sub).await else {
            panic!("scroll_changed second")
        };
        assert_eq!(scroll.offset_from_bottom, 120);
        let SubEvent::AgentStatusChanged {
            agent_status,
            agent,
            ..
        } = next(&mut sub).await
        else {
            panic!("agent_status_changed third")
        };
        assert_eq!(agent_status, AgentStatus::Working);
        assert_eq!(agent.as_deref(), Some("harness-x"));
    }

    #[tokio::test]
    async fn contract_fake_herdr_subscribe_unknown_pane_derived_id() {
        let fake = FakeHerdr::start(Topology::single_shell());
        let specs = vec![scroll_spec("w9:p9")];
        let err = fake.client().subscribe(specs, D).await.expect_err("bogus");
        let HerdrError::SubscriptionFailed { index, code, .. } = err else {
            panic!("{err:?}")
        };
        assert_eq!((index, code.as_str()), (0, "pane_not_found"));
    }

    /// Lost ack: the prompt lands on the pane, the reply never comes, the
    /// client's deadline elapses → `DeadlineExceeded` (F8: unconfirmed,
    /// never absent) and a later read proves delivery.
    #[tokio::test(start_paused = true)]
    async fn contract_fake_herdr_lost_ack_surfaces_unconfirmed() {
        let fake = FakeHerdr::start(Topology::single_shell());
        let client = fake.client();
        start(&client, "a-1", "w1:p1").await.expect("start");
        fake.fault("agent.prompt", Fault::DropResponse);
        let prompt = AgentPromptParams {
            target: "a-1".to_owned(),
            text: "BRAVO".to_owned(),
        };
        let short = Duration::from_millis(500);
        let err = client.agent_prompt(&prompt, short).await.expect_err("lost");
        assert!(matches!(err, HerdrError::DeadlineExceeded), "{err:?}");
        let landed = fake.requests();
        let prompted = landed
            .iter()
            .any(|(m, p)| m == "agent.prompt" && p["text"] == "BRAVO");
        assert!(prompted, "the request landed although its ack was lost");
        let text = read(&client, "w1:p1", ReadSource::Recent).await.text;
        assert_eq!(text, "BRAVO\n", "delivered to the pane");
    }

    /// Typed busy: the scripted knob and the natural race (second start on
    /// an occupied pane) both surface `AgentPaneBusy`, and neither starts.
    #[tokio::test]
    async fn contract_fake_herdr_busy_is_typed() {
        let fake = FakeHerdr::start(Topology::single_shell());
        let client = fake.client();
        fake.fault("agent.start", Fault::Busy);
        let err = start(&client, "a-1", "w1:p1").await.expect_err("busy");
        assert!(matches!(err, HerdrError::AgentPaneBusy { .. }), "{err:?}");
        assert!(agents(&client).await.is_empty());
        start(&client, "a-1", "w1:p1").await.expect("winner");
        let loser = start(&client, "a-2", "w1:p1").await.expect_err("loser");
        let HerdrError::AgentPaneBusy { message } = loser else {
            panic!("{loser:?}")
        };
        assert_eq!(message, "agent target pane w1:p1 is not an available shell");
        assert_eq!(agents(&client).await.len(), 1);
    }

    /// Ambiguous start: the server's own `timeout` after its startup
    /// bound → typed `Timeout` (not `DeadlineExceeded`), the agent is gone
    /// and the pane is a shell again (a3 `inflight_kill`).
    #[tokio::test(start_paused = true)]
    async fn contract_fake_herdr_ambiguous_timeout_after_delay() {
        let fake = FakeHerdr::start(Topology::single_shell());
        let client = fake.client();
        fake.fault("agent.start", Fault::TimeoutAfter(Duration::from_secs(30)));
        let t0 = tokio::time::Instant::now();
        let params = start_params("a-1", "w1:p1");
        let deadline = Duration::from_secs(60);
        let got = client.agent_start(&params, deadline).await;
        let err = got.expect_err("timeout");
        assert!(matches!(err, HerdrError::Timeout { .. }), "{err:?}");
        assert!(t0.elapsed() >= Duration::from_secs(30), "clock advanced");
        assert!(agents(&client).await.is_empty());
        let shell = pane(&client, "w1:p1").await.expect("pane");
        assert_eq!(shell.agent_status, AgentStatus::Unknown);
    }

    #[tokio::test(start_paused = true)]
    async fn contract_fake_herdr_delayed_response_within_deadline() {
        let fake = FakeHerdr::start(Topology::single_shell());
        fake.fault("ping", Fault::Delay(Duration::from_secs(2)));
        let t0 = tokio::time::Instant::now();
        fake.client().ping(D).await.expect("inside the deadline");
        assert!(t0.elapsed() >= Duration::from_secs(2));
    }

    /// Pane replacement: the old locator is gone, the new pane has a new
    /// `terminal_id` and no occupant (a46 `closed-pane`/`recreated-pane`).
    #[tokio::test]
    async fn contract_fake_herdr_pane_replacement_changes_terminal() {
        let fake = FakeHerdr::start(Topology::single_shell());
        let client = fake.client();
        let before = start(&client, "a-1", "w1:p1").await.expect("start");
        let new_id = fake.replace_pane("w1:p1");
        assert_eq!(new_id, "w1:p2");
        let err = pane(&client, "w1:p1").await.expect_err("old locator");
        assert!(matches!(err, HerdrError::PaneNotFound { .. }), "{err:?}");
        let after = pane(&client, &new_id).await.expect("new pane");
        assert_ne!(after.terminal_id, before.terminal_id);
        assert_eq!(after.agent_status, AgentStatus::Unknown);
        assert_eq!(after.agent, None);
        assert!(snapshot(&client).await.agent_rows().is_empty());
    }

    /// Workspace move: new `pane_id`, same `terminal_id`, same agent and
    /// native session (a46 `after-workspace-move`).
    #[tokio::test]
    async fn contract_fake_herdr_workspace_move_keeps_terminal() {
        let fake = FakeHerdr::start(Topology::single_shell());
        let client = fake.client();
        let before = start(&client, "a-1", "w1:p1").await.expect("start");
        let ws = fake.state().topology.create_workspace("second");
        fake.state().topology.create_tab(&ws);
        let new_id = fake.move_to_workspace("w1:p1", &ws);
        assert_eq!(new_id, "w2:p2");
        let rows = snapshot(&client).await.agent_rows();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].pane_id, new_id);
        assert_eq!(rows[0].terminal_id, before.terminal_id);
        let session = before.agent_session.map(|s| s.value);
        assert_eq!(rows[0].native_session, session);
        let moved = client.agent_get("a-1", D).await.expect("get").value;
        assert_eq!(moved.workspace_id, "w2");
    }

    /// Restart: the armed stream ends `StreamClosed`, connects fail until
    /// the daemon relistens under a new incarnation, and the re-arm catches
    /// up on the marker written meanwhile (a2 `fires_immediately`). The
    /// socket inode is not asserted: Linux re-bound it on the same inode
    /// within one tick (measured) — only the daemon-minted epoch proves a
    /// fast restart (OQ-8).
    #[tokio::test]
    async fn contract_fake_herdr_restart_rearm_catchup() {
        let mut fake = FakeHerdr::start(Topology::single_shell());
        let client = fake.client();
        let specs = vec![marker_spec("w1:p1", "GOV-DONE")];
        let mut sub = client.subscribe(specs, D).await.expect("armed");
        let first_epoch = sub.epoch();
        fake.shutdown();
        assert!(closed(&mut sub).await, "silent EOF");
        let gone = sub.rearm().await.expect_err("server gone");
        assert!(matches!(gone, HerdrError::Connect(_)), "{gone:?}");
        fake.write_output("w1:p1", "GOV-DONE\n");
        fake.relisten();
        assert_eq!(fake.incarnation(), 2);
        let mut again = sub.rearm().await.expect("re-armed");
        assert!(again.epoch().seq > first_epoch.seq, "a later connect epoch");
        let SubEvent::OutputMatched { matched_line, .. } = next(&mut again).await else {
            panic!("catch-up fire")
        };
        assert_eq!(matched_line, "GOV-DONE");
        fake.restart();
        assert_eq!(fake.incarnation(), 3);
        assert_eq!(snapshot(&client).await.panes.len(), 1);
    }

    /// Subscription EOF mid-stream: silent close → `StreamClosed`, channel
    /// ends, and an immediate re-arm is accepted (`subscription_started`).
    #[tokio::test]
    async fn contract_fake_herdr_subscription_eof_mid_stream() {
        let fake = FakeHerdr::start(Topology::single_shell());
        let specs = vec![scroll_spec("w1:p1")];
        let mut sub = fake.client().subscribe(specs, D).await.expect("armed");
        fake.close_subscriptions();
        assert!(closed(&mut sub).await, "silent EOF");
        assert!(sub.next().await.is_none());
        sub.rearm().await.expect("immediate re-arm after teardown");
    }

    /// Scripted divergence (`Fault::Raw`): a malformed reply line is typed
    /// `Malformed`, an over-bound one `FrameTooLarge`; the next request is
    /// unaffected (one-shot connections).
    #[tokio::test]
    async fn contract_fake_herdr_malformed_and_oversized_reply_are_typed() {
        let fake = FakeHerdr::start(Topology::single_shell());
        let client = fake.client();
        fake.fault("ping", Fault::malformed());
        let err = client.ping(D).await.expect_err("malformed");
        assert!(matches!(err, HerdrError::Malformed { .. }), "{err:?}");
        fake.fault("ping", Fault::oversized());
        let big = client.ping(D).await.expect_err("oversized");
        assert!(matches!(big, HerdrError::FrameTooLarge), "{big:?}");
        client
            .ping(D)
            .await
            .expect("the next connection is unaffected");
    }

    /// The fake's server discipline, driven by a raw peer: malformed or
    /// over-bound request → `id:""` + `invalid_request` + EOF; two
    /// pipelined unary frames → one reply + EOF (a2 `no_multiplex`).
    #[tokio::test]
    async fn contract_fake_herdr_malformed_request_closes_with_uncorrelated_error() {
        let fake = FakeHerdr::start(Topology::single_shell());
        let bad: [&[u8]; 5] = [
            b"not json\n",
            b"[1]\n",
            b"{\"id\":\"x\",\"method\":\"ping\"}\n",
            b"{\"id\":\"x\",\"method\":\"ping\",\"params\":[]}\n",
            b"{\"id\":\"x\",\"method\":\"a2.stats\",\"params\":{}}\n",
        ];
        for line in bad {
            let replies = raw_exchange(&fake, line, true).await;
            assert_eq!(replies.len(), 1, "{line:?}");
            assert_eq!(replies[0]["id"], "");
            assert_eq!(replies[0]["error"]["code"], "invalid_request");
        }
        let mut huge = vec![b'x'; 2 * 1024 * 1024 + 1];
        huge.push(b'\n');
        let over = raw_exchange(&fake, &huge, true).await;
        assert_eq!(over[0]["error"]["message"], "api request line is too large");
        let two = b"{\"id\":\"a\",\"method\":\"ping\",\"params\":{}}\n\
                    {\"id\":\"b\",\"method\":\"ping\",\"params\":{}}\n";
        let pipelined = raw_exchange(&fake, two, true).await;
        assert_eq!(pipelined.len(), 1, "one reply, then the connection closes");
        assert_eq!(pipelined[0]["id"], "a");
    }

    /// Fixture replay: every recorded `tx` frame goes into the fake (its
    /// two well-formed protocol-22 requests answered, the tools-daemon
    /// dialect rejected `invalid_request`), every `rx` frame comes out of
    /// it into the real client, typed by envelope — never parse-and-continue.
    #[tokio::test]
    async fn contract_fake_herdr_fixture_replay() {
        let fake = FakeHerdr::start(Topology::single_shell());
        let client = fake.client();
        let (mut tx, mut results, mut errors, mut malformed) = (0u32, 0u32, 0u32, 0u32);
        let mut seq = 0u64;
        for line in TRACE.lines() {
            let rec: Value = serde_json::from_str(line).expect("trace record");
            if let Some(frame) = rec.get("tx").and_then(Value::as_str) {
                let req: Value = serde_json::from_str(frame).unwrap_or(Value::Null);
                let well_formed = req["id"].is_string() && req["params"].is_object();
                let armed = well_formed && req["method"] == "events.subscribe";
                let replies = raw_exchange(&fake, frame.as_bytes(), !armed).await;
                assert_eq!(replies.len(), 1, "{frame}");
                if armed {
                    assert_eq!(replies[0]["id"], req["id"], "{frame}");
                    assert_eq!(replies[0]["result"]["type"], "subscription_started");
                } else if well_formed && req["method"] == "session.snapshot" {
                    assert_eq!(replies[0]["id"], req["id"], "{frame}");
                    assert_eq!(replies[0]["result"]["type"], "session_snapshot");
                } else {
                    assert_eq!(replies[0]["id"], "", "{frame}");
                    assert_eq!(replies[0]["error"]["code"], "invalid_request", "{frame}");
                }
                tx = tx.saturating_add(1);
            }
            let Some(frame) = rec.get("rx").and_then(Value::as_str) else {
                continue;
            };
            let mut raw: Value = serde_json::from_str(frame).expect("rx json");
            if let Some(id) = raw.get_mut("id") {
                *id = json!(format!("gov:{seq}"));
            }
            seq = seq.saturating_add(1);
            fake.fault("ping", Fault::Raw(format!("{raw}\n").into_bytes()));
            let outcome = client.ping(D).await.expect_err("no trace frame is a pong");
            if raw.get("id").is_none() {
                assert!(matches!(outcome, HerdrError::Malformed { .. }), "{frame}");
                malformed = malformed.saturating_add(1);
            } else if raw.get("error").is_some() {
                assert!(matches!(outcome, HerdrError::Server { .. }), "{frame}");
                errors = errors.saturating_add(1);
            } else {
                let HerdrError::Malformed { detail } = outcome else {
                    panic!("{frame} → {outcome:?}")
                };
                assert!(detail.contains("expected result type"), "{detail}");
                results = results.saturating_add(1);
            }
        }
        let counts = (tx, results, errors, malformed);
        assert!(
            tx >= 30 && results >= 15 && errors >= 1 && malformed >= 5,
            "{counts:?}"
        );
    }
}

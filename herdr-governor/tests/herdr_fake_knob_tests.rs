//! P5.H4 contract tests — one `contract_fake_herdr_<knob>` per Phase-5
//! fault knob: `replace_occupant`, `agent_exit`, `remove_pane`,
//! `set_agent_session`, `session_dir`, `snapshot_fault`,
//! `set_state_change_seq`, `set_pane_text`, `requests_since`. A sibling
//! target because `herdr_fake_tests.rs` is at the 500-line bound
//! (`just hygiene`); the `contract_fake_herdr_` prefix is what H3's
//! fake-leg filter selects.

#[cfg(test)]
pub mod support;

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::time::Duration;

    use herdr_governor::adapters::herdr::{
        AgentInfo, AgentStartParams, AgentStatus, Client, HerdrError, PaneInfo, PaneRead, ReadOpts,
        ReadSource, SessionKind, SessionSnapshot, SubEvent, Subscription, SubscriptionSpec,
        TabCreateParams,
    };

    use crate::support::fake_herdr::{FakeHerdr, Topology};

    const D: Duration = Duration::from_secs(5);

    fn start_params(name: &str, pane_id: &str) -> AgentStartParams {
        AgentStartParams {
            name: name.to_owned(),
            kind: "harness-x".to_owned(),
            pane_id: pane_id.to_owned(),
            args: Vec::new(),
            timeout_ms: Some(30_000),
        }
    }

    async fn start(client: &Client, name: &str, pane_id: &str) -> AgentInfo {
        client
            .agent_start(&start_params(name, pane_id), D)
            .await
            .expect("start")
            .value
            .agent
    }

    async fn snapshot(client: &Client) -> SessionSnapshot {
        client.session_snapshot(D).await.expect("snapshot").value
    }

    async fn pane(client: &Client, pane_id: &str) -> Result<PaneInfo, HerdrError> {
        client.pane_get(pane_id, D).await.map(|o| o.value)
    }

    async fn read(client: &Client, pane_id: &str, source: ReadSource) -> PaneRead {
        client
            .pane_read(pane_id, source, &ReadOpts::default(), D)
            .await
            .expect("pane.read")
            .value
    }

    async fn next(sub: &mut Subscription) -> SubEvent {
        sub.next().await.expect("stream open").expect("event")
    }

    /// `replace_occupant` (F1, `a4_native_new_replaces_session`): the
    /// same `pane_id` and `terminal_id`, a fresh `native_session` — and
    /// a second replacement mints yet another, never a live one.
    #[tokio::test]
    async fn contract_fake_herdr_replace_occupant() {
        let fake = FakeHerdr::start(Topology::single_shell());
        let client = fake.client();
        let before = start(&client, "a-1", "w1:p1").await;
        fake.replace_occupant("w1:p1");
        let rows = snapshot(&client).await.agent_rows();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].pane_id, "w1:p1");
        assert_eq!(rows[0].terminal_id, before.terminal_id);
        let old = before.agent_session.map(|s| s.value);
        assert_ne!(rows[0].native_session, old);
        fake.replace_occupant("w1:p1");
        let again = snapshot(&client).await.agent_rows();
        assert_ne!(again[0].native_session, rows[0].native_session);
    }

    /// `agent_exit`: the pane reverts to a shell in place — no agent
    /// row, `agent_not_found` on `agent.get`, `unknown` on `pane.get`,
    /// and the armed stream sees the status change.
    #[tokio::test]
    async fn contract_fake_herdr_agent_exit() {
        let fake = FakeHerdr::start(Topology::single_shell());
        let client = fake.client();
        start(&client, "a-1", "w1:p1").await;
        let specs = vec![SubscriptionSpec::AgentStatusChanged {
            pane_id: "w1:p1".to_owned(),
            agent_status: None,
        }];
        let mut sub = client.subscribe(specs, D).await.expect("armed");
        fake.agent_exit("w1:p1");
        let SubEvent::AgentStatusChanged {
            agent_status,
            agent,
            ..
        } = next(&mut sub).await
        else {
            panic!("agent_status_changed on exit")
        };
        assert_eq!(agent_status, AgentStatus::Unknown);
        assert_eq!(agent, None);
        assert!(snapshot(&client).await.agent_rows().is_empty());
        let err = client.agent_get("a-1", D).await.expect_err("gone");
        assert!(matches!(err, HerdrError::AgentNotFound { .. }), "{err:?}");
        let shell = pane(&client, "w1:p1").await.expect("shell");
        assert_eq!(shell.agent_status, AgentStatus::Unknown);
    }

    /// `remove_pane`: the row is gone — `pane_not_found` on the wire,
    /// absent from the snapshot, and no `pane.close` request was logged
    /// (S4b's gone-before-the-tick absence).
    #[tokio::test]
    async fn contract_fake_herdr_remove_pane() {
        let fake = FakeHerdr::start(Topology::single_shell());
        let client = fake.client();
        start(&client, "a-1", "w1:p1").await;
        fake.remove_pane("w1:p1");
        let err = pane(&client, "w1:p1").await.expect_err("gone");
        assert!(matches!(err, HerdrError::PaneNotFound { .. }), "{err:?}");
        let snap = snapshot(&client).await;
        assert!(snap.panes.is_empty());
        assert!(snap.agent_rows().is_empty());
        assert!(
            fake.requests().iter().all(|(m, _)| m != "pane.close"),
            "absence without a close request"
        );
    }

    /// `set_agent_session`: `Some` rewrites the reported session value
    /// (kind preserved), `None` makes the occupant sessionless — S7b's
    /// `identity_unprovable` input.
    #[tokio::test]
    async fn contract_fake_herdr_set_agent_session() {
        let fake = FakeHerdr::start(Topology::single_shell());
        let client = fake.client();
        start(&client, "a-1", "w1:p1").await;
        fake.set_agent_session("w1:p1", Some("sess-b"));
        let got = client.agent_get("a-1", D).await.expect("get").value;
        let session = got.agent_session.expect("session");
        assert_eq!(
            (session.kind, session.value.as_str()),
            (SessionKind::Id, "sess-b")
        );
        fake.set_agent_session("w1:p1", None);
        let cleared = client.agent_get("a-1", D).await.expect("get").value;
        assert!(cleared.agent_session.is_none());
        let rows = snapshot(&client).await.agent_rows();
        assert_eq!(rows[0].native_session, None);
    }

    /// `session_dir`: starts after the knob report `kind:"path"`
    /// sessions under the directory (a46 `--session-dir`), the
    /// transcript file exists for the pointer resolver, and each start
    /// mints a distinct file.
    #[tokio::test]
    async fn contract_fake_herdr_session_dir() {
        let dir = tempfile::tempdir().expect("dir");
        let fake = FakeHerdr::start(Topology::single_shell());
        let client = fake.client();
        fake.session_dir(dir.path());
        let started = start(&client, "a-1", "w1:p1").await;
        let session = started.agent_session.expect("session");
        assert_eq!(session.kind, SessionKind::Path);
        let path = Path::new(&session.value);
        assert_eq!(path.parent(), Some(dir.path()));
        assert!(path.exists(), "the transcript file is materialized");
        client
            .tab_create(&TabCreateParams::default(), D)
            .await
            .expect("tab");
        let second = start(&client, "a-2", "w1:p2")
            .await
            .agent_session
            .expect("session")
            .value;
        assert_ne!(second, session.value);
        assert!(Path::new(&second).exists());
    }

    /// `snapshot_fault`: while latched every `session.snapshot` fails
    /// `unavailable` — the request is still accepted and logged; clearing
    /// the knob restores the snapshot (S7b's unavailable snapshot).
    #[tokio::test]
    async fn contract_fake_herdr_snapshot_fault() {
        let fake = FakeHerdr::start(Topology::single_shell());
        let client = fake.client();
        snapshot(&client).await;
        fake.snapshot_fault(true);
        for _ in 0..3 {
            let err = client.session_snapshot(D).await.expect_err("faulted");
            let HerdrError::Server { code, .. } = err else {
                panic!("{err:?}")
            };
            assert_eq!(code, "unavailable");
        }
        fake.snapshot_fault(false);
        snapshot(&client).await;
        let snaps = fake
            .requests()
            .iter()
            .filter(|(m, _)| m == "session.snapshot")
            .count();
        assert_eq!(snaps, 5, "faulted reads were still accepted requests");
    }

    /// `set_state_change_seq`: the agent surface reports the scripted
    /// seq verbatim, independent of the content `revision` (R7's probe).
    #[tokio::test]
    async fn contract_fake_herdr_set_state_change_seq() {
        let fake = FakeHerdr::start(Topology::single_shell());
        let client = fake.client();
        let started = start(&client, "a-1", "w1:p1").await;
        assert_eq!(started.state_change_seq, Some(0));
        fake.set_state_change_seq("w1:p1", 41);
        let got = client.agent_get("a-1", D).await.expect("get").value;
        assert_eq!(got.state_change_seq, Some(41));
        assert_eq!(got.revision, 0, "the seq knob moves no revision");
    }

    /// `set_pane_text`: a per-source override — `detection`/`visible`
    /// reads see the scripted text while `recent` still reads the pane's
    /// screen (the retirement clock and the composer guard reads).
    #[tokio::test]
    async fn contract_fake_herdr_set_pane_text() {
        let fake = FakeHerdr::start(Topology::single_shell());
        let client = fake.client();
        fake.write_output("w1:p1", "screen\n");
        fake.set_pane_text("w1:p1", ReadSource::Detection, "detector-view\n");
        fake.set_pane_text("w1:p1", ReadSource::Visible, "viewport\n");
        let detection = read(&client, "w1:p1", ReadSource::Detection).await;
        assert_eq!(detection.text, "detector-view\n");
        assert_eq!(detection.source, ReadSource::Detection);
        let visible = read(&client, "w1:p1", ReadSource::Visible).await;
        assert_eq!(visible.text, "viewport\n");
        assert_eq!(
            read(&client, "w1:p1", ReadSource::Recent).await.text,
            "screen\n"
        );
    }

    /// `requests_since(n)`: a cursor over the accepted-request log —
    /// `requests_since(len)` and beyond are empty, `requests_since(0)`
    /// is the whole log.
    #[tokio::test]
    async fn contract_fake_herdr_requests_since() {
        let fake = FakeHerdr::start(Topology::single_shell());
        let client = fake.client();
        let cursor = fake.requests().len();
        client.ping(D).await.expect("pong");
        client.ping(D).await.expect("pong");
        let delta = fake.requests_since(cursor);
        let methods: Vec<&str> = delta.iter().map(|(m, _)| m.as_str()).collect();
        assert_eq!(methods, ["ping", "ping"]);
        assert_eq!(fake.requests_since(0).len(), 2);
        assert!(fake.requests_since(fake.requests().len()).is_empty());
        assert!(fake.requests_since(usize::MAX).is_empty());
    }
}

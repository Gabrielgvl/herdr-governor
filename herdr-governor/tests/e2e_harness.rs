//! `e2e_harness` — the P5.T1 phase-1 self-test: every shared harness
//! piece exercised end to end so none is dead code. `TestDaemon` in
//! both bring-ups (in-process with a seam config, and the real
//! `herdr-governor daemon` child) serves `herdr_status` against
//! `FakeHerdr`; `McpClient` frames the call directly while `RelayClient`
//! rides the real `herdr-governor relay`; `FakeJev` proves its scripted
//! answers, its three fault classes and its decoded-state capture
//! against the real `jev::Client`.

#[cfg(test)]
pub mod support;

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;
    use std::time::Duration;

    use governor_core::routing::Question;
    use herdr_governor::adapters::jev::{
        ApiKey, Client as JevClient, JevError, JudgeParams, Kind, QuestionSpec, State as JevState,
        TaskState,
    };
    use herdr_governor::daemon::{Boundary, SeamAction, SeamConfig};
    use serde_json::json;

    use crate::support::daemon::{Catalog, TestDaemon, fixture};
    use crate::support::fake_herdr::FakeHerdr;
    use crate::support::fake_herdr::topology::occupied_topology;
    use crate::support::fake_jev::{Answer, FakeJev, Fault};
    use crate::support::mcp_client::{
        McpClient, RelayClient, caller_envelope, canonical, status_call, status_page,
    };

    /// A 32-hex `relayInstanceId` for the direct client's envelope (the
    /// validator's pinned shape — the relay's own id is minted).
    const RELAY_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    #[tokio::test]
    async fn in_process_daemon_serves_status_through_both_clients() {
        let herdr = FakeHerdr::start(occupied_topology());
        let jev = FakeJev::start();
        let dirs = fixture(&Catalog::new(herdr.socket_path(), jev.base_url()));
        // A pause seam rides the in-process signature ([r2]); nothing
        // dispatches in this test so it never fires.
        let seam = SeamConfig {
            suffix: "harness".to_owned(),
            boundary: Boundary::PreDispatch,
            action: SeamAction::Pause(Duration::from_millis(1)),
        };
        let daemon = TestDaemon::start_in_process(&dirs.settings(), Some(seam)).await;

        // The direct v1-frame client.
        let client = McpClient::new(
            &daemon.socket_path(),
            caller_envelope("w1:p1", &canonical(dirs.root()), RELAY_A),
        );
        let direct = client
            .call_tool(json!("d1"), "herdr_status", json!({}))
            .await;
        let page = status_page(&direct);
        assert!(
            page["health"]["daemon"]["pid"].is_u64(),
            "status page: {page}"
        );
        assert_eq!(page["runs"], json!([]), "status page: {page}");

        // The same call through the real relay child — the envelope is
        // the relay's own derivation (pane env + canonical cwd).
        let mut relay = RelayClient::spawn(&daemon.socket_path(), dirs.root(), Some("w1:p1"));
        let piped = relay.call(&status_call("r1")).await;
        let piped_page = status_page(&piped);
        assert!(
            piped_page["health"]["daemon"]["pid"].is_u64(),
            "status page: {piped_page}"
        );
        let (status, stderr) = relay.close().await;
        assert!(status.success(), "the relay exits on stdin EOF: {stderr}");

        let sock = daemon.socket_path();
        daemon.shutdown().await;
        assert!(!sock.exists(), "the socket is removed at shutdown");
    }

    #[tokio::test]
    async fn child_daemon_serves_status_and_signals_clean() {
        let herdr = FakeHerdr::start(occupied_topology());
        let jev = FakeJev::start();
        let dirs = fixture(&Catalog::new(herdr.socket_path(), jev.base_url()));
        let daemon = TestDaemon::spawn_child(&dirs.settings(), None).await;
        assert_eq!(dirs.state_dir(), daemon.state_dir());
        assert_eq!(dirs.config_dir(), daemon.config_dir());
        assert_eq!(dirs.socket_path(), daemon.socket_path());
        assert!(dirs.config_dir().join("catalog.toml").exists());
        assert!(
            daemon.store_path().exists(),
            "the daemon's store exists under the fixture state dir"
        );

        let client = McpClient::new(
            &daemon.socket_path(),
            caller_envelope("w1:p1", &canonical(dirs.root()), RELAY_A),
        );
        let reply = client.call(&status_call("c1")).await;
        let page = status_page(&reply);
        assert!(
            page["health"]["daemon"]["pid"].is_u64(),
            "status page: {page}"
        );

        // The kill-matrix leg: SIGTERM the child, reap it through
        // `wait`, assert the graceful exit.
        daemon.signal("-TERM");
        let (status, stderr) = daemon.wait().await;
        assert!(status.success(), "the child exits on SIGTERM: {stderr}");
    }

    #[tokio::test]
    async fn child_daemon_shutdown_stops_gracefully() {
        let herdr = FakeHerdr::start(occupied_topology());
        let jev = FakeJev::start();
        let dirs = fixture(&Catalog::new(herdr.socket_path(), jev.base_url()));
        let daemon = TestDaemon::spawn_child(&dirs.settings(), None).await;
        daemon.shutdown().await;
    }

    #[tokio::test]
    async fn fake_jev_scripts_faults_and_captures_decoded_state() {
        let jev = FakeJev::start();
        jev.push_answers([
            ("done_when_verifiable", Answer::noul(0.9)),
            (
                "weakest_sufficient_tier",
                Answer::choice(
                    "fast",
                    &[("fast", 0.7), ("standard", 0.2), ("frontier", 0.1)],
                ),
            ),
        ]);
        let dir = tempfile::tempdir().expect("tempdir");
        let credentials = dir.path().join("credentials");
        std::fs::write(&credentials, "test-token\n").expect("credentials");
        std::fs::set_permissions(&credentials, std::fs::Permissions::from_mode(0o600))
            .expect("credentials mode");
        let key = ApiKey::read_0600(&credentials)
            .await
            .expect("a 0600 credential reads");
        let client = JevClient::new(jev.base_url()).expect("client builds");
        let params = JudgeParams {
            model: "jev-test".into(),
            state: JevState::Task(TaskState {
                objective: "prove the harness".into(),
                scope: "the jev leg".into(),
                done_when: vec!["answers decode".into()],
                constraints: vec![],
            }),
            questions: vec![
                QuestionSpec {
                    question: Question::DoneWhenVerifiable,
                    kind: Kind::Noul { threshold: None },
                    instructions: "is it verifiable".into(),
                    criteria: vec![("yes".into(), "it holds".into())],
                },
                QuestionSpec {
                    question: Question::WeakestSufficientTier,
                    kind: Kind::Choice,
                    instructions: "pick the tier".into(),
                    criteria: vec![
                        ("fast".into(), "f".into()),
                        ("standard".into(), "s".into()),
                        ("frontier".into(), "x".into()),
                    ],
                },
            ],
            timeout: Duration::from_millis(500),
        };

        // Scripted answers decode into judgments, and the ask is captured
        // decoded — `state` included (F31).
        let judged = client.judge(&key, &params).await.expect("answered");
        assert_eq!(judged.judgments.len(), 2);
        assert_eq!(judged.judgments[0].answer, "yes");
        assert_eq!(judged.judgments[1].answer, "fast");
        let requests = jev.requests();
        assert_eq!(requests.len(), 1);
        let state = requests[0].state().expect("a captured state");
        assert_eq!(state["task"]["objective"], "prove the harness");
        assert_eq!(requests[0].body["model"], "jev-test");

        // A status fault — typed `error_type` + the recorded component.
        jev.push_fault(Fault::Status {
            status: 503,
            error_type: Some("overloaded".to_owned()),
            retry_after_ms: Some(120),
        });
        match client.judge(&key, &params).await {
            Err(JevError::Http {
                status, component, ..
            }) => {
                assert_eq!(status, 503);
                assert_eq!(component, "http_503_overloaded");
            }
            other => panic!("expected an http error: {other:?}"),
        }

        // Abort — a mid-call transport death, no status.
        jev.push_fault(Fault::Abort);
        assert!(
            matches!(
                client.judge(&key, &params).await,
                Err(JevError::Transport(_))
            ),
            "abort is a transport failure"
        );

        // Silent — the client's own deadline is what fires.
        jev.push_fault(Fault::Silent);
        let quiet = JudgeParams {
            timeout: Duration::from_millis(150),
            ..params
        };
        assert!(
            matches!(client.judge(&key, &quiet).await, Err(JevError::Timeout)),
            "silent times out"
        );

        // Every ask — answered or faulted — was captured.
        assert_eq!(jev.requests().len(), 4);
    }
}

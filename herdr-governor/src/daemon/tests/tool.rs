//! `tool` — the `Msg::Tool` arm in-process: F1 resolves the caller over
//! the request-time snapshot, `BindCaller` journals, `herdr_status`
//! pages, and the typed refusals surface. (`Msg::Tool` itself is
//! constructed by M2's connection task, so tests drive
//! `Coordinator::tool`, the function the arm calls, and `handle` for
//! the Tick half.)

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use governor_core::config::{Catalog, Config, ConfigVersion, Policy, Tier};
use governor_core::identity::{CallerEnvelope, PaneId, ProjectRoot, RelayInstanceId};
use serde_json::json;

use crate::adapters::config::{DaemonSettings, LoadedConfig};
use crate::adapters::herdr::{
    AgentInfo, AgentSession, AgentStatus, ConnEpoch, HerdrError, Observed, SessionKind,
    SessionSnapshot,
};
use crate::daemon::api::{RunAction, ToolCall, ToolError, ToolRequest};
use crate::daemon::clock::Clock;
use crate::daemon::coordinator::{Coordinator, CoordinatorArgs, Msg, Signal};
use crate::daemon::paths::Paths;
use crate::store::Store;

const RELAY: &str = "abababababababababababababababab";

// — Fixture builders (constructed inputs, no I/O beyond tempfiles) ———————

fn canonical(dir: &Path) -> String {
    std::fs::canonicalize(dir)
        .expect("realpath")
        .to_str()
        .expect("utf8")
        .to_owned()
}

fn envelope(pane: &str, root: &str) -> CallerEnvelope {
    CallerEnvelope {
        pane_id: PaneId(pane.into()),
        project_root: ProjectRoot(root.into()),
        relay_instance_id: RelayInstanceId(RELAY.into()),
    }
}

fn agent(pane: &str, session: Option<&str>) -> AgentInfo {
    AgentInfo {
        pane_id: pane.into(),
        tab_id: "w1:t1".into(),
        workspace_id: "w1".into(),
        terminal_id: format!("term-{pane}"),
        revision: 1,
        focused: false,
        agent_status: AgentStatus::Working,
        agent: Some("kind-a".into()),
        agent_session: session.map(|value| AgentSession {
            agent: "kind-a".into(),
            kind: SessionKind::Id,
            source: "herdr:test".into(),
            value: value.into(),
        }),
        cwd: None,
        display_agent: None,
        foreground_cwd: None,
        label: None,
        scroll: None,
        state_labels: BTreeMap::new(),
        terminal_title: None,
        terminal_title_stripped: None,
        title: None,
        tokens: None,
        name: Some(format!("a-{pane}")),
        interactive_ready: Some(true),
        launch_pending: None,
        screen_detection_skipped: None,
        state_change_seq: Some(1),
    }
}

fn snapshot(agents: Vec<AgentInfo>) -> SessionSnapshot {
    SessionSnapshot {
        version: "herdr-test".into(),
        protocol: 22,
        workspaces: Vec::new(),
        tabs: Vec::new(),
        panes: Vec::new(),
        layouts: Vec::new(),
        agents,
        focused_workspace_id: None,
        focused_tab_id: None,
        focused_pane_id: None,
    }
}

fn observed(value: SessionSnapshot) -> Observed<SessionSnapshot> {
    Observed {
        epoch: ConnEpoch {
            seq: 1,
            socket_inode: 7,
            socket_mtime_secs: 1_790_000_000,
            socket_mtime_nsecs: 5,
        },
        value,
    }
}

fn policy() -> Policy {
    Policy {
        tiers: vec![Tier("standard".into())],
        no_change_cap: None,
        security_floor: None,
        broad_change_floor: None,
        provider_limit_threshold: 0.6,
        exploration_rate: 0.05,
        recovery_expiry: Duration::from_hours(24),
        cooldown: Duration::from_hours(1),
        max_age: Duration::from_hours(24),
        repair_window: Duration::from_mins(15),
        judgment_window: Duration::from_mins(30),
        idle_window: Duration::from_mins(15),
    }
}

fn daemon_settings() -> DaemonSettings {
    DaemonSettings {
        herdr_socket: PathBuf::from("/nonexistent/herdr.sock"),
        jev_base_url: "http://127.0.0.1:9".into(),
        jev_model: "jev-test".into(),
        jev_timeout: Duration::from_secs(20),
        herdr_op_timeout: Duration::from_millis(200),
        agent_start_timeout: Duration::from_secs(30),
        reconcile: Duration::from_hours(1),
        review_interval: Duration::from_mins(5),
        launch_wait: Duration::from_mins(1),
        shutdown_grace: Duration::from_secs(10),
        retire_enabled: true,
        retire_grace: Duration::from_mins(15),
        transcript_data_dirs: None,
        transcript_project_dirs: None,
        devin_log_dir: None,
    }
}

fn coordinator_at(store: Store, catalog_path: &Path, dir: &Path) -> Coordinator {
    Coordinator::new(
        store,
        CoordinatorArgs {
            loaded: LoadedConfig {
                config: Config {
                    version: ConfigVersion("v".into()),
                    catalog: Catalog {
                        operating_points: Vec::new(),
                    },
                    policy: policy(),
                },
                version: ConfigVersion("v".into()),
                daemon: Some(daemon_settings()),
            },
            daemon: daemon_settings(),
            catalog_path: catalog_path.to_path_buf(),
            clock: Clock::new(),
            seam: None,
            paths: Paths::create(&dir.join("state")).expect("paths"),
        },
    )
}

fn coordinator_with(store: Store, dir: &Path) -> Coordinator {
    coordinator_at(store, Path::new("/nonexistent/catalog.toml"), dir)
}

fn store_in(dir: &Path) -> Store {
    Store::open(&dir.join("governor.db")).expect("store opens")
}

fn status_request(envelope: &CallerEnvelope) -> ToolRequest {
    ToolRequest {
        caller: envelope.clone(),
        call: ToolCall::Status {
            event: None,
            cursor: None,
        },
    }
}

// — The `Msg::Tool` arm (A2) ———————————————————————————————————————————

/// F1 + F7 end-to-end through `Coordinator::tool`: the first valid call
/// resolves the caller, journals `BindCaller` and answers a status page;
/// a repeat call verifies the persisted binding instead of re-journaling.
#[test]
fn tool_status_resolves_binds_and_pages() {
    let tmp = tempfile::tempdir().expect("tmp");
    let root = canonical(tmp.path());
    let mut coordinator = coordinator_with(store_in(tmp.path()), tmp.path());
    let agents = vec![agent("w1:p1", Some("sess-1"))];
    let request = status_request(&envelope("w1:p1", &root));

    let page = coordinator
        .tool(
            request.clone(),
            Ok(observed(snapshot(agents.clone()))),
            Some(&root),
        )
        .expect("a status page answers");
    assert_eq!(page["runs"], json!([]), "a fresh caller owns no runs");
    assert!(
        page["health"]["daemon"]["pid"].is_u64(),
        "the health section renders"
    );
    assert_eq!(
        page["health"]["herdr"]["incarnation"], "7:1790000000.000000005",
        "the request-time snapshot's epoch records the incarnation"
    );

    let check = store_in(tmp.path());
    let binding = check
        .relay_binding(&RelayInstanceId(RELAY.into()))
        .expect("binding read")
        .expect("the bind journaled");
    assert_eq!(
        binding.caller.native_session.0, "sess-1",
        "the persisted caller is the resolved one"
    );

    let again = coordinator
        .tool(request, Ok(observed(snapshot(agents))), Some(&root))
        .expect("the bound caller still serves");
    assert_eq!(
        again["unreadEventIds"],
        json!([]),
        "the second page answers"
    );
}

/// F5 — a SIGHUP whose catalog lost its `[daemon]` table is a REFUSED
/// reload: `config.valid` flips false and `lastError` records the
/// refusal, while the last-good version and stamp stay put — the page
/// can never claim a file `check-config` rejects is the live config.
#[tokio::test]
async fn reload_without_daemon_table_records_the_refusal() {
    let tmp = tempfile::tempdir().expect("tmp");
    let root = canonical(tmp.path());
    // Decodes and core-validates; the `[daemon]` table is absent.
    let catalog_path = tmp.path().join("catalog.toml");
    std::fs::write(
        &catalog_path,
        "[policy]\ntiers = [\"fast\"]\nprovider_limit_threshold = 0.6\ncooldown_secs = 60\n\n\
         [catalog]\noperating_points = []\n",
    )
    .expect("catalog");
    let mut coordinator = coordinator_at(store_in(tmp.path()), &catalog_path, tmp.path());
    let request = status_request(&envelope("w1:p1", &root));
    let agents = || vec![agent("w1:p1", Some("sess-1"))];

    let before = coordinator
        .tool(
            request.clone(),
            Ok(observed(snapshot(agents()))),
            Some(&root),
        )
        .expect("the startup page answers");
    assert_eq!(
        before["config"]["valid"], true,
        "the startup config is valid"
    );
    let last_good_at = before["config"]["lastGoodAt"].clone();
    let last_good_version = before["config"]["version"].clone();

    coordinator.handle(Msg::Signal(Signal::Reload)).await;

    let after = coordinator
        .tool(request, Ok(observed(snapshot(agents()))), Some(&root))
        .expect("the page still answers on the last-good config");
    assert_eq!(
        after["config"]["valid"], false,
        "a daemonless catalog is a refused reload"
    );
    assert_eq!(
        after["config"]["lastError"], "invalid",
        "the refused attempt records its class"
    );
    assert_eq!(
        after["config"]["version"], last_good_version,
        "the last-good version stays live"
    );
    assert_eq!(
        after["config"]["lastGoodAt"], last_good_at,
        "the last-good stamp is preserved"
    );
}

/// A tick's good snapshot feeds `health.herdr` — the tick arm's own
/// write is observed BEFORE a tool call's request-time read replaces it
/// (the F13 repair: the tool call always records its own snapshot, so a
/// page alone can never prove the tick arm wrote anything). A failed
/// tick keeps the last good evidence.
#[tokio::test(start_paused = true)]
async fn tool_status_reports_tick_health() {
    let tmp = tempfile::tempdir().expect("tmp");
    let root = canonical(tmp.path());
    let mut coordinator = coordinator_with(store_in(tmp.path()), tmp.path());
    coordinator
        .handle(Msg::Tick {
            snapshot: Ok(observed(snapshot(Vec::new()))),
        })
        .await;

    // The tick's own write — the evidence a page alone cannot isolate.
    let (at, incarnation) = coordinator
        .herdr_seen()
        .expect("the tick arm records the good snapshot");
    assert_eq!(
        incarnation.0, "7:1790000000.000000005",
        "the tick's epoch records the incarnation"
    );
    assert!(at.0 > 0, "the tick's stamp is a real timestamp");

    // A failed tick keeps the last good evidence — never erases it.
    coordinator
        .handle(Msg::Tick {
            snapshot: Err(HerdrError::FrameTooLarge),
        })
        .await;
    assert_eq!(
        coordinator.herdr_seen().map(|(_, seen)| seen.0).as_deref(),
        Some("7:1790000000.000000005"),
        "a failed tick never erases the last good"
    );

    // The request-time read then reports on the page — a different epoch
    // proves the tool call's own snapshot is what renders.
    let mut later = observed(snapshot(vec![agent("w1:p1", Some("sess-1"))]));
    later.epoch.socket_inode = 8;
    let page = coordinator
        .tool(
            status_request(&envelope("w1:p1", &root)),
            Ok(later),
            Some(&root),
        )
        .expect("a status page answers");
    assert_eq!(
        page["health"]["herdr"]["incarnation"], "8:1790000000.000000005",
        "the request-time snapshot's epoch reports"
    );
    assert_eq!(
        page["health"]["herdr"]["freshSecsAgo"], 0,
        "the same clock read is zero-age"
    );
    assert_eq!(
        page["health"]["daemon"]["version"],
        env!("CARGO_PKG_VERSION"),
        "the daemon version reports"
    );
}

/// The refusals surface as typed `ToolError` codes: an envelope that
/// cannot resolve, a `projectRoot` that fails the realpath check, a
/// failed request-time snapshot, and a tool PR A does not serve.
#[test]
fn tool_refusals_are_typed() {
    let tmp = tempfile::tempdir().expect("tmp");
    let root = canonical(tmp.path());
    let mut coordinator = coordinator_with(store_in(tmp.path()), tmp.path());

    let request = status_request(&envelope("w1:p1", &root));
    let sessionless = coordinator
        .tool(
            request.clone(),
            Ok(observed(snapshot(vec![agent("w1:p1", None)]))),
            Some(&root),
        )
        .expect_err("a sessionless occupant refuses");
    assert_eq!(
        sessionless.code, "CALLER_IDENTITY_SESSIONLESS",
        "no native session is sessionless"
    );
    let bad_root = coordinator
        .tool(
            request.clone(),
            Ok(observed(snapshot(vec![agent("w1:p1", Some("s"))]))),
            Some("/else"),
        )
        .expect_err("a foreign realpath refuses");
    assert_eq!(
        bad_root.code, "CALLER_IDENTITY_INVALID",
        "projectRoot is never re-anchored"
    );
    let no_snapshot = coordinator
        .tool(request, Err(HerdrError::FrameTooLarge), Some(&root))
        .expect_err("a failed snapshot is unavailable");
    assert_eq!(
        no_snapshot.code,
        ToolError::DAEMON_UNAVAILABLE,
        "an internal fault is never an identity verdict"
    );

    let unknown = coordinator
        .tool(
            ToolRequest {
                caller: envelope("w1:p1", &root),
                call: ToolCall::Run(RunAction::Adopt { runs: Vec::new() }),
            },
            Ok(observed(snapshot(vec![agent("w1:p1", Some("sess-1"))]))),
            Some(&root),
        )
        .expect_err("a non-status tool refuses");
    assert_eq!(
        unknown.code,
        ToolError::TOOL_UNKNOWN,
        "PR A serves herdr_status only"
    );
}

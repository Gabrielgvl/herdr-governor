//! `signals` — the F27/F29 *child* lifecycle tests (P5.A4): a real
//! `herdr-governor daemon` process signalled for reload and ordered
//! shutdown, `FakeHerdr` logging every Herdr request it ever takes,
//! and a real `relay` riding the socket across the boundary. Where
//! `startup` proves the child refuses and recovers, these prove the
//! two signal contracts: SIGHUP's adopt/retain through the status
//! page's `config` health, SIGTERM's drain-and-teardown order observed
//! from the outside.

use std::fs;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::support::daemon::{
    Catalog, DEADLINE, TestDaemon, await_for, check_config_version, fixture, probe,
};
use crate::support::fake_herdr::FakeHerdr;
use crate::support::fake_herdr::topology::occupied_topology;
use crate::support::mcp_client::{RelayClient, status_call, status_page, tool_code};

/// Poll `herdr_status` through the relay until `config` satisfies
/// `until` or the deadline passes — the reload races the status call,
/// so convergence is the only honest wait. Returns the `config` block.
async fn poll_config(
    relay: &mut RelayClient,
    call: &Value,
    until: impl Fn(&Value) -> bool,
) -> Option<Value> {
    let deadline = Instant::now().checked_add(DEADLINE).expect("deadline");
    while Instant::now() < deadline {
        let reply = relay.call(call).await;
        let config = status_page(&reply)["config"].clone();
        if until(&config) {
            return Some(config);
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    None
}

/// F27 — `SIGHUP` reloads the catalog in place: a valid rewrite is
/// adopted (the status page's `config.version` moves to the new
/// digest, `valid`/`lastError` stay clean), a malformed rewrite is
/// retained (the last-good version stays live while `valid:false` and
/// the error class report the refused attempt), and a fixed rewrite
/// adopts again — a retained reload wedges nothing. The page is the
/// caller's own view of F7's config health through the real relay; the
/// digest is `check-config`'s, computed on the same catalog.
#[tokio::test]
async fn f27_sighup_reload_adopts_or_retains_last_good() {
    let fake = FakeHerdr::start(occupied_topology());
    let mut catalog = Catalog::inert(fake.socket_path(), "http://127.0.0.1:9");
    let dirs = fixture(&catalog);
    let catalog_path = dirs.config_dir().join("catalog.toml");
    let cwd = dirs.root().join("project");
    fs::create_dir_all(&cwd).expect("project dir");

    let daemon = TestDaemon::spawn_child(&dirs.settings(), None).await;
    let mut relay = RelayClient::spawn(&daemon.socket_path(), &cwd, Some("w1:p1"));
    let first = relay.call(&status_call(1)).await;
    let config0 = status_page(&first)["config"].clone();
    let v0 = config0["version"]
        .as_str()
        .expect("config version")
        .to_owned();
    assert_eq!(config0["valid"], true, "the fixture catalog is valid");
    assert!(config0["lastError"].is_null(), "no refused reload yet");
    assert_eq!(
        v0,
        check_config_version(dirs.config_dir()),
        "the page's digest is check-config's"
    );

    // A valid rewrite is adopted.
    catalog.cooldown_secs = 61;
    fs::write(&catalog_path, catalog.toml()).expect("rewrite catalog");
    daemon.signal("-HUP");
    let adopted = poll_config(&mut relay, &status_call(2), |cfg| {
        cfg["version"].as_str() != Some(v0.as_str())
    })
    .await
    .expect("the valid catalog was adopted");
    assert_eq!(adopted["valid"], true, "an adopted reload stays valid");
    assert!(adopted["lastError"].is_null(), "no refused attempt rides");
    let v1 = adopted["version"]
        .as_str()
        .expect("config version")
        .to_owned();
    assert_eq!(
        v1,
        check_config_version(dirs.config_dir()),
        "the adopted digest is the rewritten catalog's"
    );

    // A malformed rewrite is retained: last-good stays live and the
    // refusal surfaces as valid:false + the decode class.
    fs::write(&catalog_path, "not = [toml\n").expect("broken catalog");
    daemon.signal("-HUP");
    let retained = poll_config(&mut relay, &status_call(3), |cfg| cfg["valid"] == false)
        .await
        .expect("the broken catalog was retained");
    assert_eq!(retained["lastError"], "decode");
    assert_eq!(retained["version"], v1, "the last-good catalog stays live");

    // A fixed rewrite adopts again — a retained reload wedges nothing.
    fs::write(&catalog_path, catalog.toml()).expect("restore catalog");
    daemon.signal("-HUP");
    let healed = poll_config(&mut relay, &status_call(4), |cfg| cfg["valid"] == true)
        .await
        .expect("the retained reload recovered");
    assert_eq!(
        healed["version"], v1,
        "the same catalog adopts back to its digest"
    );

    daemon.signal("-TERM");
    let (status, daemon_err) = daemon.wait().await;
    assert!(
        status.success(),
        "SIGTERM exits cleanly: {status}\n{daemon_err}"
    );
    let (exit, relay_err) = relay.close().await;
    assert!(
        exit.success(),
        "relay exits on stdin EOF: {exit} {relay_err}"
    );
}

/// F29 — `SIGTERM` drains in order, observed from outside the child:
/// admission served right up to the signal, nothing issued to Herdr
/// after it (the tick is stopped before the drain begins and every
/// request the fake ever logged was a `session.snapshot` read — PR A
/// issues no writes at all), the socket file removed and the instance
/// lock released so a second daemon binds the same state dir, and a
/// caller meeting the torn-down socket gets `DAEMON_UNAVAILABLE`.
#[tokio::test]
async fn f29_sigterm_order_no_herdr_writes_after_admission_stops() {
    let fake = FakeHerdr::start(occupied_topology());
    let dirs = fixture(&Catalog::inert(fake.socket_path(), "http://127.0.0.1:9"));
    let sock = dirs.socket_path();
    let cwd = dirs.root().join("project");
    fs::create_dir_all(&cwd).expect("project dir");

    let daemon = TestDaemon::spawn_child(&dirs.settings(), None).await;

    // Admission is live — a caller's status call is served.
    let mut relay = RelayClient::spawn(&daemon.socket_path(), &cwd, Some("w1:p1"));
    let live = relay.call(&status_call(1)).await;
    assert_eq!(
        live["result"]["isError"], false,
        "admission served before the signal: {live}"
    );
    // The startup tick fires one `session.snapshot` then sleeps the
    // 3600s reconcile — with the call's request-time snapshot, the
    // fake has logged exactly two requests when this settles.
    await_for("the startup tick", || fake.requests().len() >= 2).await;
    let seen = fake.requests().len();

    daemon.signal("-TERM");
    let (status, daemon_err) = daemon.wait().await;
    assert!(
        status.success(),
        "SIGTERM exits cleanly: {status}\n{daemon_err}"
    );
    assert!(!sock.exists(), "teardown removed the socket file");

    let after = fake.requests_since(seen);
    assert!(
        after.is_empty(),
        "no Herdr request issued after the signal: {after:?}"
    );
    assert!(
        fake.requests()
            .iter()
            .all(|(method, _)| method == "session.snapshot"),
        "no Herdr writes — reads only"
    );

    // The lock released: a second daemon binds the same state dir.
    let second = TestDaemon::spawn_raw(&dirs.settings(), None);
    await_for("the successor's bind", || probe(&sock)).await;
    second.signal("-TERM");
    let (status2, second_err) = second.wait().await;
    assert!(
        status2.success(),
        "the successor also exits cleanly: {status2}\n{second_err}"
    );

    // A caller meeting the torn-down socket gets DAEMON_UNAVAILABLE.
    let dead = relay.call(&status_call(2)).await;
    assert_eq!(
        tool_code(&dead),
        "DAEMON_UNAVAILABLE",
        "admission stopped — the relay maps the missing socket"
    );
    let (exit, relay_err) = relay.close().await;
    assert!(
        exit.success(),
        "relay exits on stdin EOF: {exit} {relay_err}"
    );
}

/// F11 — a SIGHUP whose catalog adopted with CHANGED `[daemon]` settings
/// logs what actually happened: the catalog was adopted (`config.valid`
/// stays true, the digest moves) and the new daemon settings take effect
/// on restart — never the false WARN "config reload retained previous
/// catalog" a operator would misread as a refusal.
#[tokio::test]
async fn f27_sighup_adopted_daemon_change_logs_adopted_not_retained() {
    let fake = FakeHerdr::start(occupied_topology());
    let catalog = Catalog::inert(fake.socket_path(), "http://127.0.0.1:9");
    let dirs = fixture(&catalog);
    let catalog_path = dirs.config_dir().join("catalog.toml");
    let cwd = dirs.root().join("project");
    fs::create_dir_all(&cwd).expect("project dir");

    let daemon = TestDaemon::spawn_child(&dirs.settings(), None).await;
    let mut relay = RelayClient::spawn(&daemon.socket_path(), &cwd, Some("w1:p1"));
    let first = relay.call(&status_call(1)).await;
    let v0 = status_page(&first)["config"]["version"]
        .as_str()
        .expect("config version")
        .to_owned();

    // Same policy/catalog, a different `[daemon]` key — an adopt whose
    // daemon settings changed (they take effect on restart).
    let baseline = catalog.toml();
    let changed = baseline.replace("reconcile_secs = 3600", "reconcile_secs = 3601");
    assert_ne!(changed, baseline, "the rewrite moves a daemon setting");
    fs::write(&catalog_path, changed).expect("rewrite catalog");
    daemon.signal("-HUP");
    let adopted = poll_config(&mut relay, &status_call(2), |cfg| {
        cfg["version"].as_str() != Some(v0.as_str())
    })
    .await
    .expect("the changed catalog adopted");
    assert_eq!(adopted["valid"], true, "an adopted reload stays valid");
    assert!(adopted["lastError"].is_null(), "no refused attempt rides");

    daemon.signal("-TERM");
    let (status, daemon_err) = daemon.wait().await;
    assert!(
        status.success(),
        "SIGTERM exits cleanly: {status}\n{daemon_err}"
    );
    assert!(
        !daemon_err.contains("retained previous catalog"),
        "an adopted reload never logs the retained refusal: {daemon_err}"
    );
    assert!(
        daemon_err.contains("daemon settings take effect on restart"),
        "the deferred-effect note is logged honestly: {daemon_err}"
    );

    let (exit, relay_err) = relay.close().await;
    assert!(
        exit.success(),
        "relay exits on stdin EOF: {exit} {relay_err}"
    );
}

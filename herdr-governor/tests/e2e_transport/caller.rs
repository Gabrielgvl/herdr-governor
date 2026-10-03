//! `caller` — the F1 transport set through the real relay subprocess
//! (P5.A4): the envelope the shipped `relay` actually mints —
//! `HERDR_PANE_ID`, the realpath'd project root, a fresh 128-bit
//! `relayInstanceId` — against a real `daemon::run` and a `FakeHerdr`.
//! `identity` owns `resolve`'s in-process matrix; these tests own the
//! composition the harness rides: real derived envelopes, a minted id
//! that survives one process's requests, real `relay_bindings` rows in
//! the store — and the refusals the same wire produces.

use std::fs;

use serde_json::json;

use crate::support::daemon::{Catalog, DaemonDirs, TestDaemon, bindings, fixture};
use crate::support::fake_herdr::FakeHerdr;
use crate::support::fake_herdr::topology::{occupant, occupied_topology};
use crate::support::mcp_client::{RelayClient, git_init, request, status_call, tool_code};

/// A fixture daemon against `fake` — the inert catalog points the
/// daemon's Herdr client at the fake's socket.
fn dirs_for(fake: &FakeHerdr) -> DaemonDirs {
    fixture(&Catalog::inert(fake.socket_path(), "http://127.0.0.1:9"))
}

/// F1/S30 — the first request through a real relay binds, and the
/// binding persists across a daemon restart. The relay runs inside a
/// worktree subdirectory: its `projectRoot` is the git toplevel's
/// realpath, accepted by the daemon's own canonicalize-and-compare —
/// R1 pinned the derived string, this is the daemon's half of that
/// handshake. The minted `relayInstanceId` is asserted in the store:
/// 32 lowercase hex, journaled once on first use, verified (never
/// re-bound) after restart.
#[tokio::test]
async fn f1_first_request_binds_and_persists_across_restart() {
    let fake = FakeHerdr::start(occupied_topology());
    let dirs = dirs_for(&fake);
    let daemon = TestDaemon::start_in_process(&dirs.settings(), None).await;

    let repo = dirs.root().join("repo");
    fs::create_dir_all(repo.join("sub/dir")).expect("worktree subdir");
    git_init(&repo);
    let mut relay = RelayClient::spawn(&daemon.socket_path(), &repo.join("sub/dir"), Some("w1:p1"));
    let first = relay.call(&status_call(1)).await;
    assert_eq!(
        first["result"]["isError"], false,
        "the derived root + minted id bind: {first}"
    );

    daemon.shutdown().await;
    let bound = bindings(&dirs.store_path());
    let [(relay_id, pane_at_bind, bound_at, session)] = bound.as_slice() else {
        panic!("one relay bound on first use: {bound:?}");
    };
    assert_eq!(pane_at_bind, "w1:p1");
    assert_eq!(
        session, "sess-1",
        "the bound caller is the occupant's session"
    );
    assert_eq!(
        relay_id.len(),
        32,
        "the minted id is 128-bit hex: {relay_id}"
    );
    assert!(
        relay_id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "lowercase hex only: {relay_id}"
    );
    let stamped = bound_at.clone();

    // The daemon restarts; the same relay process keeps its minted id.
    let restarted = TestDaemon::start_in_process(&dirs.settings(), None).await;
    assert!(
        restarted.socket_path().exists(),
        "the listener re-bound on the same path"
    );
    let second = relay.call(&status_call(2)).await;
    assert_eq!(
        second["result"]["isError"], false,
        "the persisted binding verifies — not re-minted: {second}"
    );
    restarted.shutdown().await;

    let after = bindings(&dirs.store_path());
    assert_eq!(after.len(), 1, "no second binding was journaled");
    assert_eq!(
        after[0].2, stamped,
        "the original row is untouched — verify, never re-bind"
    );
    let (status, stderr) = relay.close().await;
    assert!(
        status.success(),
        "relay exits on stdin EOF: {status} {stderr}"
    );
}

/// F1/S5 — the occupant on a bound pane is replaced (H4's
/// `replace_occupant` mints a fresh native session under the same
/// pane/terminal): the bound relay's next request resolves to a
/// different caller and refuses `CALLER_IDENTITY_MISMATCH`, and the
/// refusal holds on retry — the drift is never silently re-bound, the
/// journaled row still names the first occupant.
#[tokio::test]
async fn f1_replaced_occupant_same_pane_is_mismatch() {
    let fake = FakeHerdr::start(occupied_topology());
    let dirs = dirs_for(&fake);
    let daemon = TestDaemon::start_in_process(&dirs.settings(), None).await;
    let cwd = dirs.root().join("project");
    fs::create_dir_all(&cwd).expect("project dir");

    let mut relay = RelayClient::spawn(&daemon.socket_path(), &cwd, Some("w1:p1"));
    let first = relay.call(&status_call(1)).await;
    assert_eq!(
        first["result"]["isError"], false,
        "the first request binds: {first}"
    );

    let new_session = fake.replace_occupant("w1:p1");
    assert_ne!(
        new_session, "sess-1",
        "the replacement minted a new native session"
    );

    let second = relay.call(&status_call(2)).await;
    assert_eq!(
        tool_code(&second),
        "CALLER_IDENTITY_MISMATCH",
        "a bound relay re-resolving to a new session is a mismatch"
    );
    let third = relay.call(&status_call(3)).await;
    assert_eq!(
        tool_code(&third),
        "CALLER_IDENTITY_MISMATCH",
        "the drift keeps refusing — never a silent re-bind"
    );

    daemon.shutdown().await;
    let bound = bindings(&dirs.store_path());
    assert_eq!(bound.len(), 1, "the refused re-resolve journaled nothing");
    assert_eq!(
        bound[0].3, "sess-1",
        "the binding still names the first occupant"
    );
    let (status, stderr) = relay.close().await;
    assert!(
        status.success(),
        "relay exits on stdin EOF: {status} {stderr}"
    );
}

/// F1/S5 — the binding rides the first FRAMED request, not the first
/// `tools/call`: a relay whose `initialize` bound while occupant A held
/// the pane must refuse `CALLER_IDENTITY_MISMATCH` once B replaces it —
/// binding at the first tool call would silently re-register it to B.
/// A refused non-tool method answers a `-32000` JSON-RPC error carrying
/// the typed code: `initialize`/`ping` have no tool-result channel.
#[tokio::test]
async fn f1_first_framed_request_binds_replaced_occupant_refuses() {
    let fake = FakeHerdr::start(occupied_topology());
    let dirs = dirs_for(&fake);
    let daemon = TestDaemon::start_in_process(&dirs.settings(), None).await;
    let cwd = dirs.root().join("project");
    fs::create_dir_all(&cwd).expect("project dir");

    let mut relay = RelayClient::spawn(&daemon.socket_path(), &cwd, Some("w1:p1"));
    let init = relay
        .call(&request(
            1,
            "initialize",
            &json!({"protocolVersion": "2025-06-18"}),
        ))
        .await;
    assert_eq!(
        init["result"]["protocolVersion"], "2025-06-18",
        "initialize answered — and bound the relay"
    );

    let new_session = fake.replace_occupant("w1:p1");
    assert_ne!(
        new_session, "sess-1",
        "the replacement minted a new native session"
    );

    let refused = relay.call(&status_call(2)).await;
    assert_eq!(
        tool_code(&refused),
        "CALLER_IDENTITY_MISMATCH",
        "initialize's binding refuses the replaced occupant"
    );
    let pong = relay.call(&request(3, "ping", &json!({}))).await;
    assert_eq!(pong["error"]["code"], -32000);
    assert_eq!(
        pong["error"]["message"], "CALLER_IDENTITY_MISMATCH",
        "a non-tool refusal is a JSON-RPC error carrying the code: {pong}"
    );

    daemon.shutdown().await;
    let bound = bindings(&dirs.store_path());
    assert_eq!(bound.len(), 1, "one binding — journaled by initialize");
    assert_eq!(
        bound[0].3, "sess-1",
        "the binding still names the first occupant"
    );
    let (status, stderr) = relay.close().await;
    assert!(
        status.success(),
        "relay exits on stdin EOF: {status} {stderr}"
    );
}

/// F1 — the resolution refusals, each through its own real relay:
/// a pane no snapshot row claims is `CALLER_IDENTITY_MISSING`, two
/// rows claiming one pane is `CALLER_IDENTITY_DUPLICATE`, an occupant
/// without a native session is `CALLER_IDENTITY_SESSIONLESS` — and no
/// refusal journals a binding.
#[tokio::test]
async fn f1_missing_duplicate_sessionless_refused() {
    let mut topology = occupied_topology();
    // A second row claims `w1:p1` under another terminal+session —
    // two rows, one pane: DUPLICATE. `w1:p2`'s occupant carries no
    // native session at all: SESSIONLESS. `w9:p9` is absent: MISSING.
    topology.create_tab("w1");
    let mut duplicate = topology.panes[0].clone();
    duplicate.terminal_id = "term_dup".to_owned();
    duplicate.agent = Some(occupant("agent-b", "sess-2"));
    topology.panes.push(duplicate);
    let mut sessionless = occupant("agent-c", "sess-3");
    sessionless.session = None;
    topology.panes[1].agent = Some(sessionless);
    let fake = FakeHerdr::start(topology);
    let dirs = dirs_for(&fake);
    let daemon = TestDaemon::start_in_process(&dirs.settings(), None).await;
    let cwd = dirs.root().join("project");
    fs::create_dir_all(&cwd).expect("project dir");

    for (pane, code) in [
        ("w9:p9", "CALLER_IDENTITY_MISSING"),
        ("w1:p1", "CALLER_IDENTITY_DUPLICATE"),
        ("w1:p2", "CALLER_IDENTITY_SESSIONLESS"),
    ] {
        let mut relay = RelayClient::spawn(&daemon.socket_path(), &cwd, Some(pane));
        let replies = relay.exchange_all(&[status_call(1)], 1).await;
        assert_eq!(tool_code(&replies[0]), code, "{pane} refuses as {code}");
        let (status, stderr) = relay.close().await;
        assert!(
            status.success(),
            "relay exits on stdin EOF: {status} {stderr}"
        );
    }

    daemon.shutdown().await;
    assert!(
        bindings(&dirs.store_path()).is_empty(),
        "refusals journal no bindings"
    );
}

/// F1 — an envelope that fails the daemon's realpath read refuses
/// `CALLER_IDENTITY_INVALID` and is never re-anchored (H#3). Two legs:
/// the cwd the relay derived is gone by request time, so the daemon's
/// `canonicalize` cannot produce the envelope's root; and no
/// `HERDR_PANE_ID` at all, where the relay honestly ships `paneId:""`
/// — it never fabricates a pane — and the daemon refuses the envelope.
/// The live `ping` in between binds honestly — F1's first-framed-request
/// rule — while every refused leg journals nothing.
#[tokio::test]
async fn f1_invalid_project_root_refused_never_reanchored() {
    let fake = FakeHerdr::start(occupied_topology());
    let dirs = dirs_for(&fake);
    let daemon = TestDaemon::start_in_process(&dirs.settings(), None).await;

    // The relay derives projectRoot at start — a ping proves it ran and,
    // under F1, binds the relay to the occupant it resolves — then the
    // derived root disappears: the daemon's realpath fails.
    let project = dirs.root().join("project");
    fs::create_dir_all(&project).expect("project dir");
    let mut relay = RelayClient::spawn(&daemon.socket_path(), &project, Some("w1:p1"));
    let pong = relay.call(&request(1, "ping", &json!({}))).await;
    assert_eq!(pong["result"], json!({}), "the relay is live and derived");
    fs::remove_dir_all(&project).expect("remove the derived root");
    let gone = relay.call(&status_call(2)).await;
    assert_eq!(
        tool_code(&gone),
        "CALLER_IDENTITY_INVALID",
        "an unresolvable root refuses — never re-anchored"
    );
    let (status, stderr) = relay.close().await;
    assert!(
        status.success(),
        "relay exits on stdin EOF: {status} {stderr}"
    );

    // No HERDR_PANE_ID at all: `paneId:""` goes out verbatim and the
    // daemon refuses the envelope.
    let cwd = dirs.root().join("plain");
    fs::create_dir_all(&cwd).expect("plain dir");
    let mut nopane = RelayClient::spawn(&daemon.socket_path(), &cwd, None);
    let replies = nopane.exchange_all(&[status_call(1)], 1).await;
    assert_eq!(
        tool_code(&replies[0]),
        "CALLER_IDENTITY_INVALID",
        "an empty paneId refuses"
    );
    let (status2, stderr2) = nopane.close().await;
    assert!(
        status2.success(),
        "relay exits on stdin EOF: {status2} {stderr2}"
    );

    daemon.shutdown().await;
    let bound = bindings(&dirs.store_path());
    assert_eq!(
        bound.len(),
        1,
        "only the verified ping journaled — refused legs mint nothing"
    );
    assert_eq!(bound[0].3, "sess-1");
}

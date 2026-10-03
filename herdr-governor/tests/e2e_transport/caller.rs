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
use tempfile::tempdir;

use crate::support::e2e::{
    bindings, close_relay, exchange_on, fixture, git_init, occupant, occupied_topology,
    relay_conversation, request, spawn_relay, start_daemon, status_call, stop_daemon, tool_code,
};
use crate::support::fake_herdr::FakeHerdr;

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
    let tmp = tempdir().expect("tmp");
    let fake = FakeHerdr::start(occupied_topology());
    let (state, config) = fixture(tmp.path(), fake.socket_path());
    let (sock, stop, daemon) = start_daemon(&state, &config).await;

    let repo = tmp.path().join("repo");
    fs::create_dir_all(repo.join("sub/dir")).expect("worktree subdir");
    git_init(&repo);
    let mut relay = Some(spawn_relay(&sock, &repo.join("sub/dir"), Some("w1:p1")));
    let first = exchange_on(&mut relay, &status_call(1)).await;
    assert_eq!(
        first["result"]["isError"], false,
        "the derived root + minted id bind: {first}"
    );

    stop_daemon(stop, daemon).await;
    let bound = bindings(&state);
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
    let (sock2, stop2, daemon2) = start_daemon(&state, &config).await;
    assert!(sock2.exists(), "the listener re-bound on the same path");
    let second = exchange_on(&mut relay, &status_call(2)).await;
    assert_eq!(
        second["result"]["isError"], false,
        "the persisted binding verifies — not re-minted: {second}"
    );
    stop_daemon(stop2, daemon2).await;

    let after = bindings(&state);
    assert_eq!(after.len(), 1, "no second binding was journaled");
    assert_eq!(
        after[0].2, stamped,
        "the original row is untouched — verify, never re-bind"
    );
    let (status, stderr) = close_relay(relay.take().expect("relay")).await;
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
    let tmp = tempdir().expect("tmp");
    let fake = FakeHerdr::start(occupied_topology());
    let (state, config) = fixture(tmp.path(), fake.socket_path());
    let (sock, stop, daemon) = start_daemon(&state, &config).await;
    let cwd = tmp.path().join("project");
    fs::create_dir_all(&cwd).expect("project dir");

    let mut relay = Some(spawn_relay(&sock, &cwd, Some("w1:p1")));
    let first = exchange_on(&mut relay, &status_call(1)).await;
    assert_eq!(
        first["result"]["isError"], false,
        "the first request binds: {first}"
    );

    let new_session = fake.replace_occupant("w1:p1");
    assert_ne!(
        new_session, "sess-1",
        "the replacement minted a new native session"
    );

    let second = exchange_on(&mut relay, &status_call(2)).await;
    assert_eq!(
        tool_code(&second),
        "CALLER_IDENTITY_MISMATCH",
        "a bound relay re-resolving to a new session is a mismatch"
    );
    let third = exchange_on(&mut relay, &status_call(3)).await;
    assert_eq!(
        tool_code(&third),
        "CALLER_IDENTITY_MISMATCH",
        "the drift keeps refusing — never a silent re-bind"
    );

    stop_daemon(stop, daemon).await;
    let bound = bindings(&state);
    assert_eq!(bound.len(), 1, "the refused re-resolve journaled nothing");
    assert_eq!(
        bound[0].3, "sess-1",
        "the binding still names the first occupant"
    );
    let (status, stderr) = close_relay(relay.take().expect("relay")).await;
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
    let tmp = tempdir().expect("tmp");
    let fake = FakeHerdr::start(occupied_topology());
    let (state, config) = fixture(tmp.path(), fake.socket_path());
    let (sock, stop, daemon) = start_daemon(&state, &config).await;
    let cwd = tmp.path().join("project");
    fs::create_dir_all(&cwd).expect("project dir");

    let mut relay = Some(spawn_relay(&sock, &cwd, Some("w1:p1")));
    let init = exchange_on(
        &mut relay,
        &request(1, "initialize", &json!({"protocolVersion": "2025-06-18"})),
    )
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

    let refused = exchange_on(&mut relay, &status_call(2)).await;
    assert_eq!(
        tool_code(&refused),
        "CALLER_IDENTITY_MISMATCH",
        "initialize's binding refuses the replaced occupant"
    );
    let pong = exchange_on(&mut relay, &request(3, "ping", &json!({}))).await;
    assert_eq!(pong["error"]["code"], -32000);
    assert_eq!(
        pong["error"]["message"], "CALLER_IDENTITY_MISMATCH",
        "a non-tool refusal is a JSON-RPC error carrying the code: {pong}"
    );

    stop_daemon(stop, daemon).await;
    let bound = bindings(&state);
    assert_eq!(bound.len(), 1, "one binding — journaled by initialize");
    assert_eq!(
        bound[0].3, "sess-1",
        "the binding still names the first occupant"
    );
    let (status, stderr) = close_relay(relay.take().expect("relay")).await;
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
    let tmp = tempdir().expect("tmp");
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
    let (state, config) = fixture(tmp.path(), fake.socket_path());
    let (sock, stop, daemon) = start_daemon(&state, &config).await;
    let cwd = tmp.path().join("project");
    fs::create_dir_all(&cwd).expect("project dir");

    for (pane, code) in [
        ("w9:p9", "CALLER_IDENTITY_MISSING"),
        ("w1:p1", "CALLER_IDENTITY_DUPLICATE"),
        ("w1:p2", "CALLER_IDENTITY_SESSIONLESS"),
    ] {
        let (relay, replies) =
            relay_conversation(&sock, &cwd, Some(pane), &[status_call(1)], 1).await;
        assert_eq!(tool_code(&replies[0]), code, "{pane} refuses as {code}");
        let (status, stderr) = close_relay(relay).await;
        assert!(
            status.success(),
            "relay exits on stdin EOF: {status} {stderr}"
        );
    }

    stop_daemon(stop, daemon).await;
    assert!(bindings(&state).is_empty(), "refusals journal no bindings");
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
    let tmp = tempdir().expect("tmp");
    let fake = FakeHerdr::start(occupied_topology());
    let (state, config) = fixture(tmp.path(), fake.socket_path());
    let (sock, stop, daemon) = start_daemon(&state, &config).await;

    // The relay derives projectRoot at start — a ping proves it ran and,
    // under F1, binds the relay to the occupant it resolves — then the
    // derived root disappears: the daemon's realpath fails.
    let project = tmp.path().join("project");
    fs::create_dir_all(&project).expect("project dir");
    let mut relay = Some(spawn_relay(&sock, &project, Some("w1:p1")));
    let pong = exchange_on(&mut relay, &request(1, "ping", &json!({}))).await;
    assert_eq!(pong["result"], json!({}), "the relay is live and derived");
    fs::remove_dir_all(&project).expect("remove the derived root");
    let gone = exchange_on(&mut relay, &status_call(2)).await;
    assert_eq!(
        tool_code(&gone),
        "CALLER_IDENTITY_INVALID",
        "an unresolvable root refuses — never re-anchored"
    );
    let (status, stderr) = close_relay(relay.take().expect("relay")).await;
    assert!(
        status.success(),
        "relay exits on stdin EOF: {status} {stderr}"
    );

    // No HERDR_PANE_ID at all: `paneId:""` goes out verbatim and the
    // daemon refuses the envelope.
    let cwd = tmp.path().join("plain");
    fs::create_dir_all(&cwd).expect("plain dir");
    let (nopane, replies) = relay_conversation(&sock, &cwd, None, &[status_call(1)], 1).await;
    assert_eq!(
        tool_code(&replies[0]),
        "CALLER_IDENTITY_INVALID",
        "an empty paneId refuses"
    );
    let (status2, stderr2) = close_relay(nopane).await;
    assert!(
        status2.success(),
        "relay exits on stdin EOF: {status2} {stderr2}"
    );

    stop_daemon(stop, daemon).await;
    let bound = bindings(&state);
    assert_eq!(
        bound.len(),
        1,
        "only the verified ping journaled — refused legs mint nothing"
    );
    assert_eq!(bound[0].3, "sess-1");
}

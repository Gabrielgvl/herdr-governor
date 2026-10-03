//! `identity` — F1 end-to-end over the daemon boundary: the same
//! `daemon::identity::resolve` the coordinator's `Msg::Tool` arm calls,
//! plus the `BindCaller` journal write, driven in-process against a real
//! tempdir store. The socket-level leg — the v1 frame that carries the
//! envelope into `Msg::Tool` — is `transport`'s
//! `transport_herdr_status_serves_over_the_real_socket` (P5.M2's
//! listener).

use std::path::Path;

use governor_core::identity::{
    AgentKind, AgentName, CallerBinding, CallerEnvelope, CallerKey, ChildStatus, NativeSession,
    PaneId, ProjectRoot, RelayInstanceId, TerminalId, Timestamp,
};
use governor_core::lifecycle::{StateChange, Transition};
use herdr_governor::daemon::identity::{self, AgentRow};
use herdr_governor::store::Store;
use tempfile::{TempDir, tempdir};

/// A well-formed `relayInstanceId` (128-bit lowercase hex).
const RELAY: &str = "abababababababababababababababab";
const NOW: Timestamp = Timestamp(1_790_812_800_000);

fn canonical(dir: &Path) -> String {
    std::fs::canonicalize(dir)
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned()
}

fn envelope(pane: &str, root: &str, relay: &str) -> CallerEnvelope {
    CallerEnvelope {
        pane_id: PaneId(pane.into()),
        project_root: ProjectRoot(root.into()),
        relay_instance_id: RelayInstanceId(relay.into()),
    }
}

/// One occupied pane row — the shape `resolve` consumes.
fn row(pane: &str, terminal: &str, session: Option<&str>) -> AgentRow {
    (
        PaneId(pane.into()),
        TerminalId(terminal.into()),
        Some(AgentKind("kind-a".into())),
        Some(AgentName(format!("agent-{pane}"))),
        session.map(|s| NativeSession(s.to_owned())),
        Some(ChildStatus::Working),
    )
}

/// Journal the binding `resolve` produced — what the coordinator does on
/// first use before dispatching the call.
fn bind(store: &mut Store, binding: &CallerBinding) {
    store
        .apply(
            &Transition {
                state_changes: vec![StateChange::BindCaller(binding.clone())],
                events: Vec::new(),
                effects: Vec::new(),
            },
            NOW,
        )
        .expect("bind caller");
}

fn store_in(dir: &TempDir) -> Store {
    Store::open(&dir.path().join("governor.db")).unwrap()
}

/// F1 — the first request mints and journals the relay binding; after a
/// store reopen (the daemon's restart) the same request verifies against
/// the persisted row instead of re-binding.
#[test]
fn f1_first_request_binds_and_persists_across_restart() {
    let tmp = tempdir().unwrap();
    let root = canonical(tmp.path());
    let mut store = store_in(&tmp);
    let agents = vec![row("w1:p1", "term-1", Some("sess-1"))];
    let envelope = envelope("w1:p1", &root, RELAY);

    let (caller, binding) = identity::resolve(&store, &envelope, Some(&root), &agents).unwrap();
    let fresh = binding.expect("first use mints a binding to journal");
    assert_eq!(
        fresh.caller,
        CallerKey {
            agent_kind: AgentKind("kind-a".into()),
            native_session: NativeSession("sess-1".into()),
        }
    );
    bind(&mut store, &fresh);

    drop(store);
    let reopened = store_in(&tmp);
    let (again, rebind) = identity::resolve(&reopened, &envelope, Some(&root), &agents).unwrap();
    assert_eq!(again, caller, "the same caller re-resolves");
    assert!(
        rebind.is_none(),
        "the persisted binding verified — nothing to re-journal"
    );
    assert_eq!(
        reopened
            .relay_binding(&envelope.relay_instance_id)
            .unwrap()
            .unwrap()
            .caller,
        caller,
        "the binding persisted across the reopen"
    );
}

/// F1 — a bound relay re-resolving to a different native session (the
/// pane's occupant was replaced) is `CALLER_IDENTITY_MISMATCH`, never a
/// re-bind.
#[test]
fn f1_replaced_occupant_same_pane_is_mismatch() {
    let tmp = tempdir().unwrap();
    let root = canonical(tmp.path());
    let mut store = store_in(&tmp);
    let envelope = envelope("w1:p1", &root, RELAY);
    let bound = vec![row("w1:p1", "term-1", Some("sess-1"))];
    let (_, binding) = identity::resolve(&store, &envelope, Some(&root), &bound).unwrap();
    bind(&mut store, &binding.unwrap());

    let replaced = vec![row("w1:p1", "term-1", Some("sess-2"))];
    let error = identity::resolve(&store, &envelope, Some(&root), &replaced).unwrap_err();
    assert_eq!(
        error.code, "CALLER_IDENTITY_MISMATCH",
        "a replaced occupant on a bound pane is a mismatch"
    );
}

/// F1 — the three unresolvable-caller refusals, each under its own code.
#[test]
fn f1_missing_duplicate_sessionless_refused() {
    let tmp = tempdir().unwrap();
    let root = canonical(tmp.path());
    let store = store_in(&tmp);
    let envelope = envelope("w1:p1", &root, RELAY);

    // No pane in the snapshot → CALLER_IDENTITY_MISSING.
    let missing = identity::resolve(&store, &envelope, Some(&root), &[]).unwrap_err();
    assert_eq!(missing.code, "CALLER_IDENTITY_MISSING");

    // Two rows claim the same pane → CALLER_IDENTITY_DUPLICATE.
    let duplicated = vec![
        row("w1:p1", "term-1", Some("sess-1")),
        row("w1:p1", "term-2", Some("sess-2")),
    ];
    let duplicate = identity::resolve(&store, &envelope, Some(&root), &duplicated).unwrap_err();
    assert_eq!(duplicate.code, "CALLER_IDENTITY_DUPLICATE");

    // An occupant with no native session → CALLER_IDENTITY_SESSIONLESS.
    let sessionless = vec![row("w1:p1", "term-1", None)];
    let refused = identity::resolve(&store, &envelope, Some(&root), &sessionless).unwrap_err();
    assert_eq!(refused.code, "CALLER_IDENTITY_SESSIONLESS");
}

/// F1 — a `projectRoot` that fails the realpath check (absent or
/// different) is `CALLER_IDENTITY_INVALID` and journals nothing — a bad
/// root is refused, never re-anchored (H#3).
#[test]
fn f1_invalid_project_root_refused_never_reanchored() {
    let tmp = tempdir().unwrap();
    let root = canonical(tmp.path());
    let store = store_in(&tmp);
    let agents = vec![row("w1:p1", "term-1", Some("sess-1"))];
    let envelope = envelope("w1:p1", &root, RELAY);

    for realpath in [None, Some("/elsewhere")] {
        let error = identity::resolve(&store, &envelope, realpath, &agents).unwrap_err();
        assert_eq!(
            error.code, "CALLER_IDENTITY_INVALID",
            "realpath {realpath:?} refuses invalid"
        );
    }
    assert!(
        store
            .relay_binding(&envelope.relay_instance_id)
            .unwrap()
            .is_none(),
        "no binding was journalled — the root was never re-anchored"
    );
}

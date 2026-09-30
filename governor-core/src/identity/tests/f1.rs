//! F1 — caller identity: envelope validation, pane resolution and the
//! relay-instance binding.

use alloc::vec::Vec;

use super::builders::{RELAY_A, RELAY_B, RELAY_C, agent_row, envelope, key, occupied};
use crate::identity::{
    AgentRow, CallerBinding, PaneId, RelayInstanceId, resolve_caller, resolve_caller_key,
    validate_caller_envelope,
};
use crate::task::Refusal;

#[test]
fn f1_invalid_envelope_refused_invalid() {
    let no_pane = envelope("", "/work/repo", RELAY_A);
    assert_eq!(
        validate_caller_envelope(&no_pane),
        Err(Refusal::CallerIdentityInvalid),
        "F1 — an empty paneId makes the envelope malformed: CALLER_IDENTITY_INVALID"
    );
    let pane = envelope("w6:p1", "/work/repo", RELAY_A);
    assert_eq!(
        validate_caller_envelope(&pane),
        Ok(()),
        "F1 — a present paneId passes"
    );
}

#[test]
fn f1_project_root_absolute_single_line_canonical() {
    let pane = |root: &str| envelope("w6:p1", root, RELAY_A);
    let invalid = Err(Refusal::CallerIdentityInvalid);
    for (root, verdict) in [
        ("/work/repo", Ok(())),
        ("/w", Ok(())),
        ("/work/repo/sub", Ok(())),
        ("work/repo", invalid), // relative
        ("", invalid),          // empty
        ("/", invalid),         // root itself is refused (F1)
        ("//work", invalid),    // doubled separator is not canonical
        ("/work//repo", invalid),
        ("/work/", invalid),       // trailing separator
        ("/work/./repo", invalid), // dot component
        ("/work/../repo", invalid),
        ("/work\n/repo", invalid), // line break — not single-line
        ("/work\r\n/repo", invalid),
        ("/work/\0repo", invalid), // NUL — never a realpath output
    ] {
        assert_eq!(
            validate_caller_envelope(&pane(root)),
            verdict,
            "F1 — projectRoot {root:?} canonical-as-given verdict"
        );
    }
}

#[test]
fn f1_relay_instance_id_must_be_128_bit_hex() {
    let env = |relay: &str| envelope("w6:p1", "/work/repo", relay);
    let invalid = Err(Refusal::CallerIdentityInvalid);
    for (relay, verdict) in [
        ("018f3c2a7b1d7e908abc0123456789ab", Ok(())), // 128 bits, lowercase hex
        ("", invalid),                                // missing id
        ("abc123", invalid),                          // too short
        ("018f3c2a7b1d7e908abc0123456789abc", invalid), // 33 chars
        ("018F3C2A7B1D7E908ABC0123456789AB", invalid), // uppercase hex
        ("018f3c2a7b1d7e908abc0123456789ag", invalid), // non-hex tail
        ("relay-1", invalid),                         // a label, not hex
    ] {
        assert_eq!(
            validate_caller_envelope(&env(relay)),
            verdict,
            "F1/ADR-0004 — relayInstanceId {relay:?} must be 32 lowercase hex chars"
        );
    }
}

#[test]
fn f1_resolve_missing_duplicate_sessionless() {
    let agents: Vec<AgentRow> = Vec::from([
        occupied("w6:p1", "t1", "kind-a", "caller", Some("sess-a"), None),
        occupied("w6:p2", "t2", "kind-a", "caller", None, None),
        occupied("w6:p3", "t3", "kind-b", "caller", Some("sess-b"), None),
        occupied("w6:p3", "t3", "kind-b", "caller", Some("sess-b"), None),
        agent_row("w6:p4", "t4", None, None, None, None),
    ]);
    assert_eq!(
        resolve_caller_key(&PaneId("w6:p9".into()), &agents),
        Err(Refusal::CallerIdentityMissing),
        "F1 — no pane for the envelope is CALLER_IDENTITY_MISSING"
    );
    assert_eq!(
        resolve_caller_key(&PaneId("w6:p3".into()), &agents),
        Err(Refusal::CallerIdentityDuplicate),
        "F1 — two panes for the id is CALLER_IDENTITY_DUPLICATE"
    );
    assert_eq!(
        resolve_caller_key(&PaneId("w6:p2".into()), &agents),
        Err(Refusal::CallerIdentitySessionless),
        "F1 — an occupant without a native session is CALLER_IDENTITY_SESSIONLESS"
    );
    assert_eq!(
        resolve_caller_key(&PaneId("w6:p4".into()), &agents),
        Err(Refusal::CallerIdentityMissing),
        "F1 — an empty pane resolves no caller"
    );
    let session_no_kind = Vec::from([agent_row("w6:p5", "t5", None, None, Some("sess-x"), None)]);
    assert_eq!(
        resolve_caller_key(&PaneId("w6:p5".into()), &session_no_kind),
        Err(Refusal::CallerIdentityMissing),
        "F1 — a session without an agent kind keys no caller"
    );
    assert_eq!(
        resolve_caller_key(&PaneId("w6:p1".into()), &agents),
        Ok(key("kind-a", "sess-a")),
        "F1 — a unique occupied pane resolves its caller key"
    );
}

#[test]
fn f1_unbound_relay_binds_on_first_use() {
    let agents = Vec::from([occupied(
        "w6:p1",
        "t1",
        "kind-a",
        "caller",
        Some("sess-a"),
        None,
    )]);
    let env = envelope("w6:p1", "/work/repo", RELAY_A);
    assert_eq!(
        resolve_caller(&env, None, &agents),
        Ok((
            key("kind-a", "sess-a"),
            Some(CallerBinding {
                caller: key("kind-a", "sess-a"),
                relay_instance: RelayInstanceId(RELAY_A.into()),
                pane_at_bind: PaneId("w6:p1".into()),
            })
        )),
        "F1 — the first request resolves the caller and yields the binding to persist"
    );
}

#[test]
fn f1_bound_relay_reresolves_to_same_session() {
    let agents = Vec::from([occupied(
        "w6:p1",
        "t1",
        "kind-a",
        "caller",
        Some("sess-a"),
        None,
    )]);
    let bound = CallerBinding {
        caller: key("kind-a", "sess-a"),
        relay_instance: RelayInstanceId(RELAY_A.into()),
        pane_at_bind: PaneId("w6:p1".into()),
    };
    let env = envelope("w6:p1", "/work/repo", RELAY_A);
    assert_eq!(
        resolve_caller(&env, Some(&bound), &agents),
        Ok((key("kind-a", "sess-a"), None)),
        "F1 — a bound id that re-resolves to the same caller produces no new binding"
    );
}

#[test]
fn f1_bound_relay_refuses_session_drift() {
    // a4_native_new_replaces_session — same pane, replaced occupant.
    let agents = Vec::from([occupied(
        "w6:p1",
        "t1",
        "kind-a",
        "caller",
        Some("sess-new"),
        None,
    )]);
    let bound = CallerBinding {
        caller: key("kind-a", "sess-a"),
        relay_instance: RelayInstanceId(RELAY_A.into()),
        pane_at_bind: PaneId("w6:p1".into()),
    };
    let env = envelope("w6:p1", "/work/repo", RELAY_A);
    assert_eq!(
        resolve_caller(&env, Some(&bound), &agents),
        Err(Refusal::CallerIdentityMismatch),
        "F1 — re-resolving to a different native session is CALLER_IDENTITY_MISMATCH"
    );
    let kind_drift = Vec::from([occupied(
        "w6:p1",
        "t1",
        "kind-b",
        "caller",
        Some("sess-a"),
        None,
    )]);
    assert_eq!(
        resolve_caller(&env, Some(&bound), &kind_drift),
        Err(Refusal::CallerIdentityMismatch),
        "F1 — a different caller key is drift even with the same session"
    );
    let wrong_binding = CallerBinding {
        caller: key("kind-a", "sess-a"),
        relay_instance: RelayInstanceId(RELAY_C.into()),
        pane_at_bind: PaneId("w6:p1".into()),
    };
    assert_eq!(
        resolve_caller(&env, Some(&wrong_binding), &agents),
        Err(Refusal::CallerIdentityMismatch),
        "F1 — a binding for another relay id cannot verify this request"
    );
    let gone: Vec<AgentRow> = Vec::new();
    assert_eq!(
        resolve_caller(&env, Some(&bound), &gone),
        Err(Refusal::CallerIdentityMissing),
        "F1 — a bound id whose locator now resolves to nothing is missing"
    );
}

#[test]
fn f1_respawned_relay_binds_afresh() {
    let agents = Vec::from([occupied(
        "w6:p1",
        "t1",
        "kind-a",
        "caller",
        Some("sess-a"),
        None,
    )]);
    let old = CallerBinding {
        caller: key("kind-a", "sess-a"),
        relay_instance: RelayInstanceId(RELAY_A.into()),
        pane_at_bind: PaneId("w6:p1".into()),
    };
    let env = envelope("w6:p1", "/work/repo", RELAY_B);
    // ADR-0004 — a respawned relay mints a new id; the daemon looks it
    // up, finds no binding (None), and binds afresh.
    match resolve_caller(&env, None, &agents) {
        Ok((caller, Some(binding))) => {
            assert_eq!(caller, old.caller, "F1 — same caller key rebinds");
            assert_eq!(
                binding.relay_instance,
                RelayInstanceId(RELAY_B.into()),
                "F1 — the new binding carries the new relay id"
            );
        }
        other => panic!("F1 — a fresh relay id must bind, got {other:?}"),
    }
}

#[test]
fn f1_resolution_refuses_invalid_envelope() {
    let agents = Vec::from([occupied(
        "w6:p1",
        "t1",
        "kind-a",
        "caller",
        Some("sess-a"),
        None,
    )]);
    let env = envelope("w6:p1", "relative/path", RELAY_A);
    assert_eq!(
        resolve_caller(&env, None, &agents),
        Err(Refusal::CallerIdentityInvalid),
        "F1 — resolution validates the envelope first"
    );
    let bound = CallerBinding {
        caller: key("kind-a", "sess-a"),
        relay_instance: RelayInstanceId(RELAY_A.into()),
        pane_at_bind: PaneId("w6:p1".into()),
    };
    assert_eq!(
        resolve_caller(&env, Some(&bound), &agents),
        Err(Refusal::CallerIdentityInvalid),
        "F1 — a malformed envelope is refused before the binding check"
    );
}

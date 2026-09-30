#!/usr/bin/env python3
"""Phase 2 contract suite (spec §10 Phase 2; docs/research/contract-discovery.md).

Every test names one behavior the wave-1 evidence reports confirmed and
checks it against the committed fixture data in tests/fixtures/contract/ —
no live system is contacted. Confirmed-negative and needs-owner-decision
items are documented in the research file and intentionally have no test.

Fail closed: a missing, unreadable, or malformed fixture errors the test
that needs it, so the suite cannot silently pass on absent evidence.

Run via `just contract` (or directly: python3 -B tests/contract/contract_tests.py).
"""

import hashlib
import json
import os
import re
import tempfile
import unittest
from pathlib import Path

FIXTURES = Path(__file__).resolve().parents[1] / "fixtures" / "contract"
SAMPLES = FIXTURES / "a5-samples"


def fixture_bytes(rel: str) -> bytes:
    path = FIXTURES / rel
    if not path.is_file():
        raise FileNotFoundError(f"missing contract fixture: {path}")
    return path.read_bytes()


def fixture_json(rel: str):
    return json.loads(fixture_bytes(rel))


def a1_trace_records():
    return [json.loads(line)
            for line in fixture_bytes("a1-transport-trace.jsonl")
            .decode().splitlines() if line.strip()]


def a1_executor_bearer():
    return fixture_json("a1-executor-bearer-requests.json")


def a1_executor_reconnect():
    return fixture_json("a1-executor-reconnect-requests.json")


def wire_requests(records):
    """HTTP-level records (mcp_request + plain request), in order."""
    return [r for r in records if r["kind"] in ("mcp_request", "request")]


def rpc_methods(records):
    return [r["method"] for r in records if r["kind"] == "mcp_method"]


def call_segment(records, n):
    """Records between the `call-<n>-start*` and `call-<n>-end` markers."""
    out, inside = [], False
    for r in records:
        if r["kind"] == "marker":
            if r["label"].startswith(f"call-{n}-start"):
                inside = True
            elif inside and r["label"] == f"call-{n}-end":
                break
            continue
        if inside:
            out.append(r)
    return out


def registration_segment(records):
    """Everything before the `call-1-start` marker (post-listening health
    handshake performed at connection create)."""
    out = []
    for r in records:
        if r["kind"] == "marker" and r["label"] == "call-1-start":
            break
        out.append(r)
    return out


def a2_sub_evidence():
    return fixture_json("a2-subscription-evidence.json")


def trace_records():
    return [json.loads(line)
            for line in fixture_bytes("a2-tools-daemon-trace.jsonl")
            .decode().splitlines() if line.strip()]


def peer_frames(records, peer):
    return [r for r in records
            if r.get("peer") == peer and ("tx" in r or "rx" in r or "event" in r)]


def tx_of(frames):
    """Peer tx entries in order; the oversized frame logs a generative dict."""
    return [f["tx"] for f in frames if "tx" in f]


def rx_json_of(frames):
    """Peer rx entries parsed as JSON (connection-error envelopes too)."""
    out = []
    for f in frames:
        if "rx" in f:
            out.append(json.loads(f["rx"]))
    return out


def captures():
    return fixture_json("a46-identity-evidence.json")


def commands(ev):
    return ev["commands"]


def command_stdout(ev, needle):
    """Parsed stdout of the first recorded command containing `needle`."""
    for rec in commands(ev):
        if needle in rec["command"]:
            return json.loads(rec["stdout"]), rec
    raise AssertionError(f"no recorded command containing {needle!r}")


def get_pane(capture):
    """The pane-get result pane object, or None when get returned an error."""
    result = capture["get"].get("result") or {}
    return result.get("pane")


def snapshot_pane(capture, pane_id):
    for pane in capture["snapshot"]["panes"]:
        if pane["pane_id"] == pane_id:
            return pane
    return None


def agent_row(capture, pane_id):
    for row in capture["agents"]:
        if row.get("pane_id") == pane_id:
            return row
    return None


def child_identity(capture):
    """The (snapshot pane row, pane-get pane, agent-list row) for the pane
    this capture's pane-get resolved — the three surfaces the reports join."""
    pane = get_pane(capture)
    if pane is None:
        return None, None, None
    pid = pane["pane_id"]
    return snapshot_pane(capture, pid), pane, agent_row(capture, pid)


def case(outcomes, case_id):
    for entry in outcomes["results"]:
        if entry["id"] == case_id:
            return entry
    raise AssertionError(f"no recorded a5 outcome {case_id!r}")


def jsonl_scan(data: bytes):
    """Spec-contract JSONL scan: only LF-terminated, well-formed JSON records
    are consumed. Returns (records, consumed, failure) with failure =
    ("source_malformed", offset) at the first malformed complete line; an
    unterminated tail stays pending without events or failure."""
    records, consumed = [], 0
    while True:
        nl = data.find(b"\n", consumed)
        if nl == -1:
            return records, consumed, None
        raw = data[consumed:nl]
        try:
            records.append(json.loads(raw))
        except (json.JSONDecodeError, UnicodeDecodeError):
            return records, consumed, ("source_malformed", consumed)
        consumed = nl + 1


class A1ExecutorTransport(unittest.TestCase):
    """contract-a1.md — Streamable HTTP transport trial against a
    purpose-built probe server. Gateway-identity correction (A1 fold,
    2026-09-29): this trial registered the probe through pi's MCP adapter
    (`pi-mcp-adapter`, retired in pi 0.99), not the Executor catalog
    daemon real callers use — these checks are kept as pi-adapter
    evidence; Executor catalog-path and pi-native measurements live in
    A1ExecutorCatalogTransport / A1PiNativeTransport. Wire frames come
    from the probe server's own log (two runs, split at its `listening`
    records); adapter-side strings come from the recorded observation
    fixture. The wave-2 OAuth trial closed the adapter's authenticated
    sub-case confirmed-negative; it has no test by design."""

    def setUp(self):
        self.records = a1_trace_records()
        self.obs = fixture_json("a1-gateway-observations.json")

    def runs(self):
        """Server runs in order; element 0 of each run is its listening record."""
        runs, current = [], None
        for rec in self.records:
            if rec["kind"] == "listening":
                current = [rec]
                runs.append(current)
            elif current is not None:
                current.append(rec)
        return runs

    def requests(self, run):
        return [r["detail"] for r in run if r["kind"] == "request"]

    def test_a1_register_streamable_http_source(self):
        install = self.obs["install"]
        self.assertEqual(install["result"], "Installed and connected")
        self.assertEqual(install["url"], "http://127.0.0.1:8877/mcp")
        self.assertEqual(install["tool_exposed"], "local_probe_echo")
        self.assertEqual(install["protocol_version_accepted"], "2025-06-18")

    def test_a1_handshake_sequence(self):
        methods = [r["method"] for r in self.requests(self.runs()[0])]
        self.assertEqual(methods, ["initialize", "notifications/initialized",
                                   "tools/list", "tools/call"])

    def test_a1_caller_arguments_forwarded_verbatim(self):
        fwd = self.obs["caller_forwarding"]
        self.assertEqual(fwd["request"],
                         {"text": "caller-forwarding-probe-1",
                          "nested": {"k": 42}})
        self.assertEqual(fwd["echoed"], "byte-identical")

    def test_a1_sse_stream_optional(self):
        run = self.runs()[0]
        kinds = [r["kind"] for r in run]
        get_at = kinds.index("get")
        following = run[get_at + 1]
        self.assertEqual(following["kind"], "request")
        self.assertEqual(following["detail"]["method"], "tools/list")
        sse = self.obs["sse_validation_get"]
        self.assertTrue(sse["gateway_issued"])
        self.assertEqual(sse["server_status"], 405)
        self.assertTrue(sse["session_continued"])

    def test_a1_stateless_session(self):
        for run in self.runs():
            for detail in self.requests(run):
                self.assertNotIn("mcp-session-id", detail["headers"])

    def test_a1_reconnect_transparent_after_restart(self):
        runs = self.runs()
        self.assertEqual(len(runs), 2)
        pids = [run[0]["detail"]["pid"] for run in runs]
        self.assertNotEqual(pids[0], pids[1])
        self.assertEqual(self.requests(runs[1])[0]["method"], "tools/call")

    def test_a1_offline_is_untyped_fetch_failure(self):
        off = self.obs["offline_failure"]
        self.assertEqual(off["observed_error"],
                         "Failed to call tool: fetch failed")
        self.assertIsNone(off["typed_error_class"])

    def test_a1_no_per_caller_process_recreation(self):
        runs = self.runs()
        self.assertEqual(len(runs), 2)
        for run in runs:
            self.assertEqual(sum(1 for r in run if r["kind"] == "listening"), 1)
            for detail in self.requests(run):
                self.assertEqual(detail["headers"]["user-agent"], "undici")
        pm = self.obs["process_model"]
        self.assertTrue(pm["persistent_server_process_for_all_calls"])
        self.assertEqual(pm["child_spawns_observed"], 0)

    def test_a1_non_loopback_requires_https(self):
        self.assertTrue(self.obs["auth"]["non_loopback_http_refused"])

    def test_a1_auth_round_trip_confirmed_negative(self):
        """The pi adapter names the RFC 8414 metadata URL in its error but
        never issues the request (verified server-side): its OAuth
        metadata loader is unimplemented, so an authenticated source
        cannot complete registration on that adapter — loopback + no-auth
        was the only supported shape there."""
        on401 = self.obs["auth"]["on_401"]
        self.assertEqual(on401["www_authenticate"], "Bearer")
        self.assertIn("/.well-known/oauth-authorization-server",
                      on401["named_metadata_url"])
        self.assertIn("gateway-local", on401["observed_error_template"])
        self.assertFalse(on401["metadata_request_actually_issued"])
        self.assertTrue(on401["full_round_trip"].startswith(
            "confirmed-negative"))

    def test_a1_stale_loopback_blocks_reregistration(self):
        auth = self.obs["auth"]
        self.assertFalse(auth["uninstall_verb_present"])
        self.assertEqual(auth["loopback_name_derivation"],
                         "every loopback variant derives the source name "
                         "local-mcp")


class A1ExecutorCatalogTransport(unittest.TestCase):
    """contract-a1-bearer.md + reconnect/contract-a1-reconnect.md — the A1
    transport measured on the real caller gateway (Executor's catalog
    daemon), 2026-09-29: apiKey `headers` template + file-provider
    credential, session lifecycle across restarts. Fixtures are
    mechanical distillations of the probe server's request logs —
    method/path/header-name/auth/session fields only."""

    def setUp(self):
        self.bearer = a1_executor_bearer()
        self.recon = a1_executor_reconnect()

    def test_a1_executor_registration_declarative(self):
        """Registration performs no tool calls: the connection-create
        segment is exactly one health handshake (discover → initialize →
        initialized → GET → tools/list), every request authenticated."""
        seg = registration_segment(self.recon["records"])
        self.assertEqual(rpc_methods(seg), ["server/discover", "initialize",
                                            "notifications/initialized",
                                            "tools/list"])
        reqs = wire_requests(seg)
        self.assertEqual(len(reqs), 5)
        self.assertTrue(all(r["auth_match"] for r in reqs))

    def test_a1_executor_addserver_requires_name(self):
        self.assertEqual(
            self.recon["observations"]["addserver_missing_name_rejected"],
            "invalid_tool_arguments")

    def test_a1_executor_static_bearer_every_request(self):
        """apiKey headers template + file-provider item: every request on
        the authenticated path carries `Authorization: Bearer` and
        matches the configured credential."""
        reqs = wire_requests(self.bearer["executor"]["authenticated"])
        self.assertGreater(len(reqs), 0)
        for r in reqs:
            self.assertTrue(r["auth_present"])
            self.assertEqual(r["auth_scheme"], "bearer")
            self.assertTrue(r["auth_match"])
            self.assertIn("Authorization", r["header_names"])
        self.assertIn("tools/call",
                      rpc_methods(self.bearer["executor"]["authenticated"]))

    def test_a1_executor_connection_address_camelized(self):
        obs = self.bearer["observations"]
        self.assertEqual(obs["connection_address"],
                         "tools.gov-a1-bearer-probe.org.govA1BearerProbe")
        self.assertEqual(obs["credential_provider"], "file")
        self.assertEqual(
            obs["credential_item_id"],
            "connection:org:gov-a1-bearer-probe:gov-a1-bearer-probe:token")

    def test_a1_executor_authenticated_no_oauth_discovery(self):
        """OAuth never engages on the authenticated path: zero
        `/.well-known` fetches and zero unauthenticated requests."""
        seg = self.bearer["executor"]["authenticated"]
        self.assertFalse(
            any(".well-known" in r.get("path", "") for r in seg))
        self.assertTrue(all(r["auth_match"] for r in wire_requests(seg)))

    def test_a1_executor_401_triggers_oauth_discovery(self):
        """An unauthenticated probe is 401'd and still triggers RFC 8414 /
        OIDC metadata discovery; metadata 501s yield the recorded verdict."""
        probe = self.bearer["executor"]["probe_unauthenticated"]
        paths = [r["path"] for r in probe if r["kind"] == "request"]
        self.assertEqual(
            paths, ["/.well-known/oauth-authorization-server/mcp",
                    "/.well-known/openid-configuration/mcp",
                    "/mcp/.well-known/openid-configuration"])
        self.assertTrue(
            all(not r["auth_present"] for r in wire_requests(probe)))
        self.assertEqual(
            self.bearer["observations"]["probe_endpoint_verdict"],
            {"connected": False, "requiresAuthentication": True,
             "requiresOAuth": False})

    def test_a1_executor_reconnect_after_restart(self):
        """First call on a new server process re-initializes: a fresh
        handshake presents no session id and ends at tools/call."""
        recs = self.recon["records"]
        call3 = call_segment(recs, 3)
        reqs = wire_requests(call3)
        self.assertIsNone(reqs[0]["session_id"])
        self.assertEqual(rpc_methods(call3),
                         ["server/discover", "initialize",
                          "notifications/initialized", "tools/call"])
        self.assertTrue(all(r["auth_match"] for r in reqs))

    def test_a1_executor_dead_session_evicted(self):
        """A transport failure evicts the cached session: after failed
        call-2 the next call presents no id from the dead session."""
        recs = self.recon["records"]
        dead = {r["session_id"] for r in wire_requests(call_segment(recs, 1))
                if r["session_id"] is not None}
        self.assertEqual(len(dead), 1)
        presented = [r["session_id"]
                     for r in wire_requests(call_segment(recs, 3))]
        self.assertNotIn(next(iter(dead)), presented)
        # same eviction after the failed --log-level call-2b
        dead2 = {r["session_id"] for r in wire_requests(call_segment(recs, 4))
                 if r["session_id"] is not None}
        presented5 = [r["session_id"]
                      for r in wire_requests(call_segment(recs, 5))]
        for sid in dead2:
            self.assertNotIn(sid, presented5)

    def test_a1_executor_stale_session_404_reinitialize(self):
        """Stale `Mcp-Session-Id` → 404 → re-initialize → retry, inside the
        same call; the caller never sees the 404."""
        recs = self.recon["records"]
        seg = call_segment(recs, 7)
        reqs = wire_requests(seg)
        self.assertEqual(rpc_methods(seg),
                         ["tools/call", "server/discover", "initialize",
                          "notifications/initialized", "tools/call"])
        # the first tools/call presented a session id the new process
        # does not know (stale) — the server logged a session_rejected
        self.assertFalse(reqs[0]["session_known"])
        rejected = [r for r in seg if r["kind"] == "session_rejected"
                    and r["session_id"] == reqs[0]["session_id"]]
        self.assertTrue(
            any(r["method"] == "tools/call" for r in rejected))
        # the retried call carries the freshly issued session id
        self.assertIsNotNone(reqs[-1]["session_id"])
        self.assertTrue(reqs[-1]["session_known"])
        self.assertNotEqual(reqs[-1]["session_id"], reqs[0]["session_id"])

    def test_a1_executor_session_reused_between_calls(self):
        """Steady state: one `tools/call` per call carrying the session id
        the preceding handshake established."""
        recs = self.recon["records"]
        for warm_n, cold_n in ((4, 3), (6, 5), (8, 7)):
            warm = call_segment(recs, warm_n)
            self.assertEqual(rpc_methods(warm), ["tools/call"])
            reqs = wire_requests(warm)
            self.assertEqual(len(reqs), 1)
            known = {r["session_id"]
                     for r in wire_requests(call_segment(recs, cold_n))
                     if r["session_known"]}
            self.assertIn(reqs[0]["session_id"], known)

    def test_a1_executor_server_down_error_untyped(self):
        """Server down → caller sees an untyped `Internal tool error
        [<correlation-id>]`, exit 1; the server received zero requests."""
        obs = self.recon["observations"]
        self.assertEqual(obs["server_down_caller_error"],
                         "Internal tool error [4fce215a]")
        self.assertEqual(obs["server_down_exit_code"], 1)
        self.assertFalse(obs["server_down_error_envelope_typed"])
        self.assertEqual(call_segment(self.recon["records"], 2), [])

    def test_a1_executor_no_oauth_rediscovery(self):
        """Across restarts: every request auth-matched, zero 401s, zero
        `.well-known` fetches — restart does not re-trigger discovery."""
        recs = self.recon["records"]
        self.assertTrue(all(r["auth_match"] for r in wire_requests(recs)))
        self.assertFalse(
            any(".well-known" in r.get("path", "") for r in recs))


class A1PiNativeTransport(unittest.TestCase):
    """pi 0.99 native `pi mcp` client leg of contract-a1-bearer.md —
    measured on the same loopback probe server (pi's own MCP client; a
    different registry from the retired pi-mcp-adapter that hosted the
    wave-1 trial)."""

    def setUp(self):
        self.bearer = a1_executor_bearer()

    def test_a1_pi_native_static_headers_sent(self):
        """Configured static headers ride every request (the env-var
        indirection keeps the token out of mcp.json)."""
        seg = self.bearer["pi_native"]
        authed = [r for r in wire_requests(seg) if r["auth_present"]]
        self.assertGreaterEqual(len(authed), 4)
        for r in authed:
            self.assertIn("Authorization", r["header_names"])
            self.assertEqual(r["auth_scheme"], "bearer")
            self.assertTrue(r["auth_match"])
        self.assertEqual(rpc_methods(seg),
                         ["initialize", "notifications/initialized",
                          "tools/list"])

    def test_a1_pi_native_401_defers_oauth(self):
        """On 401 pi marks the server `needs sign-in` and defers OAuth to
        `pi mcp login` — exactly one unauthenticated request and zero
        `.well-known` fetches."""
        seg = self.bearer["pi_native"]
        unauthed = [r for r in wire_requests(seg) if not r["auth_present"]]
        self.assertEqual(len(unauthed), 1)
        self.assertFalse(
            any(".well-known" in r.get("path", "") for r in seg))
        self.assertEqual(
            self.bearer["observations"]["pi_list_status_unauthenticated"],
            "needs sign-in")


class A2HerdrSubscription(unittest.TestCase):
    """contract-a2.md § Real-Herdr subscription leg — protocol-22 socket
    evidence from the isolated named session `herdr-governor-contract`
    (raw NDJSON only, no live-socket traffic). Confirmed behaviors only;
    the three need-owner-decision items (scroll-changed delivery, silent
    stream teardown on malformed input, schema drift) have no test by
    design. The raw JSONL carries owner prompt lines and stays in the
    artifact bundle — this fixture is the scrubbed distillation."""

    def setUp(self):
        self.ev = a2_sub_evidence()

    def test_a2sub_isolation_socket_scoped(self):
        iso = self.ev["isolation"]
        self.assertEqual(iso["connect_calls_total"], 67)
        self.assertEqual(iso["connect_calls_to_session_socket"], 67)
        self.assertEqual(iso["connect_calls_to_live_socket"], 0)
        self.assertEqual(iso["gov_a2_request_id_hits_live_server_log"], 0)
        self.assertGreater(iso["gov_a2_request_id_hits_session_server_log"], 0)
        self.assertTrue(iso["spawned_shell_env_carries_session_socket"])
        self.assertTrue(iso["live_socket_stat_unchanged_before_after"])
        self.assertEqual(iso["session_snapshot_final_workspaces"], 0)
        # the two stated caveats stay stated, not absorbed
        self.assertIn("not a /tmp root", iso["caveat_not_a_temp_root"])
        self.assertIn("carries the live endpoint", iso["caveat_server_env"])

    def test_a2sub_one_request_per_connection(self):
        framing = self.ev["framing"]
        self.assertTrue(framing["one_request_per_connection"])
        self.assertEqual(framing["pipelined_second_frame"], "dropped")
        self.assertTrue(framing["subscription_connection_stays_open"])

    def test_a2sub_subscription_ack(self):
        sub = self.ev["subscription"]
        self.assertEqual(sub["ack_type"], "subscription_started")
        self.assertTrue(sub["event_envelope"]["carries_data"])

    def test_a2sub_subscription_pane_filter(self):
        self.assertEqual(self.ev["subscription"]["unsubscribed_pane_leaked"], [])

    def test_a2sub_output_match_one_shot(self):
        one = self.ev["subscription"]["one_shot"]
        self.assertEqual(one["same_marker_again_after_clear"], [])
        self.assertTrue(one["connection_still_open_after_fire"])
        self.assertTrue(
            self.ev["subscription"]["armed_while_marker_on_screen_fires_immediately"])

    def test_a2sub_subscribe_empty_ok(self):
        empty = self.ev["subscription"]["subscribe_variants"]["empty_list"]
        self.assertFalse(empty["serverClosed"])
        self.assertTrue(empty["idMatches"])

    def test_a2sub_subscribe_error_id_derived(self):
        """Confirmed-negative: per-sub failure reports a DERIVED id and closes."""
        bogus = self.ev["subscription"]["subscribe_variants"]["bogus_pane"]
        self.assertTrue(bogus["replyIdSuffix"].endswith(":sub:0:probe"))
        self.assertTrue(bogus["serverClosed"])
        self.assertFalse(bogus["idMatches"])
        live = self.ev["subscription"]["subscribe_variants"]["live_workspace_pane_id"]
        self.assertEqual(live["error"], "pane_not_found")

    def test_a2sub_concurrent_connections_independent(self):
        con = self.ev["concurrency"]
        self.assertEqual(con["unary_during_armed_subscription"],
                         ["pong", "session_snapshot", "pane_read", "ok"])
        self.assertEqual(con["four_unary_plus_one_event_elapsed_ms"], 201)
        self.assertTrue(con["same_id_on_two_connections_both_answered"])

    def test_a2sub_no_multiplex(self):
        nm = self.ev["no_multiplex"]
        self.assertIsNone(nm["second_frame_on_subscription_connection"]["reply"])
        self.assertTrue(nm["second_frame_on_subscription_connection"]["connectionClosed"])
        self.assertEqual(nm["pipelined_two_unary_one_connection"]["replies"], 1)
        self.assertTrue(nm["pipelined_two_unary_one_connection"]["connectionClosed"])

    def test_a2sub_reconnect_no_replay_state_catchup(self):
        rec = self.ev["reconnect"]
        self.assertTrue(rec["no_replay_no_buffer"])
        self.assertEqual(rec["marker_cleared_before_resubscribe"],
                         "NO EVENT (lost)")
        self.assertTrue(rec["state_catchup_when_marker_on_screen"])
        self.assertTrue(rec["subscribe_has_no_cursor_or_revision_param"])
        self.assertEqual(rec["read_revision_in_observed_events"], 0)

    def test_a2sub_malformed_closes_and_empty_error_id(self):
        mal = self.ev["malformed"]
        self.assertEqual(len(mal["cases_closed_connection"]), 9)
        self.assertEqual(mal["fresh_connection_error_id"], "")
        self.assertEqual(mal["error_code"], "invalid_request")
        self.assertIn("too large", mal["oversize_session_log_warning"])

    def test_a2sub_line_bound_oversize_rejected(self):
        mal = self.ev["malformed"]
        self.assertIn("oversize_2mib", mal["cases_closed_connection"])

    def test_a2sub_peer_failure_containment(self):
        mal = self.ev["malformed"]
        self.assertTrue(mal["on_armed_subscription_no_error_frame"])
        self.assertTrue(self.ev["timeouts"]["server_healthy_after_abandoned_wait"])

    def test_a2sub_partial_frame_reassembled(self):
        self.assertTrue(self.ev["malformed"]["partial_frame_reassembled"])

    def test_a2sub_wait_server_timeout(self):
        to = self.ev["timeouts"]
        self.assertEqual(to["wait_timeout_code"], "timeout")
        self.assertTrue(to["wait_timeout_closes_connection"])
        self.assertTrue(to["concurrent_ping_answered_during_wait"])
        self.assertEqual(to["pane_wait_for_output_timeout_code"], "timeout")

    def test_a2sub_wait_no_timeout_blocks(self):
        self.assertFalse(
            self.ev["timeouts"]["wait_without_timeout_ms_replied_within_3s"])

    def test_a2sub_subscription_survives_idle_8s(self):
        self.assertTrue(
            self.ev["timeouts"]["subscription_idle_8s_still_delivers"])

    def test_a2sub_events_wait_agent_status_only(self):
        """Confirmed-negative: every non-agent-status EventMatch is refused."""
        self.assertEqual(self.ev["timeouts"]["wait_unsupported_match_code"],
                         "unsupported_event_wait_match")

    def test_a2sub_scroll_changed_delivered(self):
        sc = self.ev["scroll_changed"]
        d = sc["delivered_on_explicit_scroll"]
        self.assertEqual(d["offset_from_bottom"], 120)
        self.assertEqual(d["viewport_rows"], 40)
        self.assertTrue(sc["delivered_on_return_to_bottom"])
        self.assertEqual(sc["delivered_on_output_growth"]["max_offset_grew_to"], 310)
        self.assertIn("no-op", sc["prior_leg_non_delivery_cause"])


class A2ToolsDaemonTransport(unittest.TestCase):
    """contract-a2.md — isolated herdr-tools daemon socket evidence. These are
    the TOOLS-daemon contracts; the Herdr subscription leg is unverified and
    has no test by design."""

    def setUp(self):
        self.records = trace_records()

    def test_a2_recorded_probe_passed(self):
        verdicts = [r for r in self.records if r.get("evidence") == "assertions"]
        self.assertEqual([v["result"] for v in verdicts], ["PASS"])
        live = [r for r in self.records
                if r.get("evidence") == "live_comparison"]
        self.assertEqual(live[0]["checkoutChanged"], [])
        self.assertTrue(live[0]["socketUnchanged"])

    def test_a2_hello_required(self):
        frames = peer_frames(self.records, "no-hello")
        tx = tx_of(frames)
        self.assertTrue(tx[0].startswith('{"id":"raw","method":"session.snapshot"'))
        rx = rx_json_of(frames)
        self.assertEqual(rx, [{"type": "error", "error": {
            "code": "DAEMON_PROTOCOL_ERROR",
            "message": "daemon socket expects a hello"}}])
        self.assertTrue(any(f.get("event") == "closed" for f in frames))

    def test_a2_version_refusal(self):
        frames = peer_frames(self.records, "wrong-version")
        self.assertEqual(tx_of(frames), ['{"type":"hello","version":22}\n'])
        self.assertEqual(rx_json_of(frames), [{"type": "error", "error": {
            "code": "PROTOCOL_MISMATCH",
            "message": "daemon protocol version is not supported"}}])
        self.assertTrue(any(f.get("event") == "closed" for f in frames))

    def test_a2_same_connection_multiplex(self):
        frames = peer_frames(self.records, "subscription")
        tx = tx_of(frames)
        hold_i = tx.index('{"id":"hold","method":"a2.hold","params":{}}\n')
        same_i = tx.index('{"id":"same","method":"a2.stats","params":{}}\n')
        self.assertLess(hold_i, same_i)
        rx = rx_json_of(frames)
        same_rx = next(r for r in rx if r.get("id") == "same")
        hold_rx = next(r for r in rx if r.get("id") == "hold")
        # the later request completed while 'hold' was still pending
        self.assertLess(rx.index(same_rx), rx.index(hold_rx))
        self.assertEqual(same_rx["result"], {"held": ["hold"], "completed": []})
        self.assertEqual(hold_rx["result"], {"released": "hold"})

    def test_a2_cross_connection_isolation(self):
        frames = peer_frames(self.records, "concurrent")
        rx = rx_json_of(frames)
        parallel = next(r for r in rx if r.get("id") == "parallel")
        # answered on the second connection while 'hold' stayed pending on the first
        self.assertEqual(parallel["result"]["held"], ["hold"])

    def test_a2_request_error_nonfatal(self):
        frames = peer_frames(self.records, "concurrent")
        rx = rx_json_of(frames)
        bad = next(r for r in rx if r.get("id") == "identity")
        self.assertEqual(bad["error"]["code"], "CALLER_IDENTITY_MALFORMED")
        release = next(r for r in rx if r.get("id") == "release")
        self.assertEqual(release["result"], {"completed": ["hold"]})
        self.assertLess(rx.index(bad), rx.index(release))

    def test_a2_disconnect_does_not_cancel(self):
        sub = peer_frames(self.records, "subscription")
        pending = next(r for r in rx_json_of(sub) if r.get("id") == "pending")
        self.assertEqual(pending["result"]["held"], ["disconnect"])
        self.assertTrue(any(f.get("event") == "closed" for f in sub))
        re = peer_frames(self.records, "reconnect")
        rx = rx_json_of(re)
        self.assertEqual(rx[0], {"type": "ack", "version": 1})
        after = next(r for r in rx if r.get("id") == "after-disconnect")
        self.assertEqual(after["result"]["held"], ["disconnect"])
        finished = next(r for r in rx if r.get("id") == "finished")
        self.assertIn("disconnect", finished["result"]["completed"])

    def _protocol_error_close(self, peer, code_fragment):
        frames = peer_frames(self.records, peer)
        rx = rx_json_of(frames)
        self.assertEqual(len(rx), 2, f"{peer}: expected ack + one error")
        self.assertEqual(rx[0], {"type": "ack", "version": 1})
        err = rx[1]
        self.assertEqual(err["type"], "error")
        self.assertNotIn("id", err)
        self.assertEqual(err["error"]["code"], "DAEMON_PROTOCOL_ERROR")
        self.assertIn(code_fragment, err["error"]["message"])
        self.assertTrue(any(f.get("event") == "closed" for f in frames))
        return err

    def test_a2_malformed_json_close(self):
        frames = peer_frames(self.records, "invalid-json")
        self.assertIn("{\n", tx_of(frames))
        self._protocol_error_close("invalid-json", "not JSON")

    def test_a2_malformed_params_close(self):
        frames = peer_frames(self.records, "bad-params")
        self.assertIn('{"id":"bad","method":"status","params":[]}\n',
                      tx_of(frames))
        self._protocol_error_close("bad-params", "params are malformed")

    def test_a2_oversize_close(self):
        frames = peer_frames(self.records, "oversized")
        tx = tx_of(frames)[-1]
        self.assertEqual(tx["bytes"], 262145)
        payload = b"x" * tx["bytes"]
        self.assertEqual(hashlib.sha256(payload).hexdigest(), tx["sha256"])
        self._protocol_error_close("oversized", "exceeds the accepted bound")
        consts = next(r for r in self.records
                      if r.get("evidence") == "built_constants")
        self.assertEqual(consts["maxLineBytes"], tx["bytes"] - 1)

    def test_a2_peer_failure_containment(self):
        con = rx_json_of(peer_frames(self.records, "concurrent"))
        for name in ("invalid-json", "bad-params", "no-hello",
                     "wrong-version", "oversized"):
            reply = next(r for r in con if r.get("id") == f"health-{name}")
            self.assertIn("result", reply)
            self.assertIn("completed", reply["result"])

    def test_a2_client_timeout_nonfatal(self):
        ev = next(r for r in self.records
                  if r.get("evidence") == "request_timeout")
        self.assertEqual(ev["code"], "DAEMON_UNAVAILABLE")
        self.assertFalse(ev["isClosed"])
        idx = self.records.index(ev)
        held = next(r for r in self.records[idx:]
                    if r.get("rx", "").startswith('{"id":"timeout-pending"'))
        self.assertIn("herdr-daemon-1",
                      json.loads(held["rx"])["result"]["held"])

    def test_a2_late_reply_inert_after_timeout(self):
        # The client's rejection of herdr-daemon-1 is recorded BEFORE the
        # late reply lands on the wire — a settled rejection cannot be
        # re-resolved, so the frame is inert and the next request resolves
        # to its own response. The client's pending map is not observable
        # on the wire, so discard is proven as inertness, not by
        # inspecting client internals.
        timeout_i = self.records.index(next(
            r for r in self.records
            if r.get("evidence") == "request_timeout"))
        late_i = self.records.index(next(
            r for r in self.records
            if r.get("peer") == "official-client"
            and r.get("rx", "").startswith(
                '{"id":"herdr-daemon-1","result":{"released"')))
        self.assertLess(timeout_i, late_i)
        rx = rx_json_of(peer_frames(self.records, "official-client"))
        late = next(r for r in rx
                    if r.get("id") == "herdr-daemon-1"
                    and "released" in r.get("result", {}))
        follow = next(r for r in rx if r.get("id") == "herdr-daemon-2")
        self.assertLess(rx.index(late), rx.index(follow))
        self.assertEqual(follow["result"]["held"], [])
        # the release really happened daemon-side — real work the client
        # no longer awaited, not a fabricated frame
        self.assertIn("herdr-daemon-1", follow["result"]["completed"])

    def test_a2_client_close_rejects_pending(self):
        ev = next(r for r in self.records
                  if r.get("evidence") == "pending_client_close")
        self.assertEqual(ev["code"], "DAEMON_UNAVAILABLE")
        self.assertIn("closed by the client", ev["message"])
        con = rx_json_of(peer_frames(self.records, "concurrent"))
        rel = next(r for r in con if r.get("id") == "client-close-release")
        self.assertIn("herdr-daemon-1", rel["result"]["completed"])


class A3StartPrompt(unittest.TestCase):
    """contract-a3.md — `agent start` / `agent prompt` semantics probed live
    against pi, devin, agy, claude on dedicated panes in the isolated named
    session `herdr-governor-contract` (herdr 0.9.1). The committed fixture is
    the scrubbed distillation; the raw report's verbatim CLI output stays in
    the artifact bundle. The readiness-fidelity gap is a recorded owner
    decision (readiness is advisory), and the claude provider-limit start
    shape stayed unreproduced in-window — both documented, no test by
    design."""

    def setUp(self):
        self.ev = fixture_json("a3-start-prompt-evidence.json")

    def test_a3_start_ready_01(self):
        pi = self.ev["start"]["pi"]
        result = pi["envelope"]["result"]
        self.assertEqual(result["type"], "agent_started")
        agent = result["agent"]
        self.assertTrue(agent["interactive_ready"])
        self.assertEqual(agent["agent_status"], "idle")
        self.assertEqual(agent["agent_session"]["kind"], "path")
        self.assertEqual(agent["agent_session"]["source"], "herdr:pi")
        self.assertTrue(agent["screen_detection_skipped"])
        self.assertTrue(pi["session_file_existed"])
        # start blocks until reported readiness: ~3.0 s on every pi start
        self.assertGreaterEqual(pi["elapsed_ms"], 3000)
        self.assertEqual(pi["repeat_elapsed_ms"], [3024, 3024])
        for name in ("devin_trusted", "claude"):
            ret = self.ev["start"][name]
            self.assertEqual(ret["returned"]["type"], "agent_started")
            self.assertTrue(ret["returned"]["interactive_ready"])
            self.assertEqual(ret["returned"]["agent_status"], "idle")
            self.assertEqual(ret["agent_session"]["kind"], "id")
        self.assertEqual(self.ev["start"]["devin_trusted"]
                         ["agent_session"]["source"], "herdr:devin")
        self.assertEqual(self.ev["start"]["claude"]
                         ["agent_session"]["source"], "herdr:claude")
        self.assertTrue(self.ev["start"]["claude"]["session_file_existed"])

    def test_a3_start_ready_02(self):
        win = self.ev["registration_window"]
        row = win["during_start_list_row"]
        self.assertEqual(row["agent_status"], "unknown")
        self.assertFalse(row["interactive_ready_key_present"])
        self.assertEqual(row["terminal_title"], "pi")
        # observable window: status flips to idle before interactive_ready
        # appears in list records; the start response's flag is authoritative
        self.assertLess(
            win["idle_in_list_before_interactive_ready_at_ms_approx"],
            win["start_response_interactive_ready_at_ms_approx"])

    def test_a3_start_timeout_01(self):
        rf = self.ev["runtime_failures"]
        self.assertEqual(rf["envelope"],
                         {"error": {"code": "timeout",
                                    "message": "timed out waiting for agent "
                                               "startup"},
                          "id": "cli:agent:start"})
        self.assertFalse(rf["caller_can_distinguish_causes"])
        self.assertEqual({c["injection"] for c in rf["cases"]},
                         {"missing_binary", "agent_arg_rejected",
                          "mid_start_sigkill"})
        for c in rf["cases"]:
            # every runtime failure runs out the full --timeout
            self.assertGreaterEqual(c["elapsed_ms"], c["timeout_ms"])
            self.assertIn("shell", c["pane_fallback"])

    def test_a3_start_busy_01(self):
        busy = self.ev["pre_flight_errors"]["start_on_occupied_pane"]
        self.assertEqual(busy["error"]["code"], "agent_pane_busy")
        self.assertEqual(busy["id"], "cli:agent:start")
        race = self.ev["concurrent_starts_one_pane"]
        self.assertEqual(race["winner"]["type"], "agent_started")
        self.assertEqual(race["loser"]["error"]["code"], "agent_pane_busy")
        self.assertFalse(race["double_launch"])

    def test_a3_start_inflight_kill_01(self):
        k = self.ev["inflight_kill"]
        self.assertFalse(k["start_call_returned_early"])
        self.assertEqual(k["start_call_result_code"], "timeout")
        self.assertTrue(k["agent_absent_from_list_immediately"])
        self.assertEqual(k["pane_after"]["agent_status"], "unknown")
        self.assertTrue(k["pane_after"]["terminal_title_back_to_shell"])
        post = k["post_start_death_deregisters"]
        self.assertEqual(post["observed_for"], ["pi", "devin", "agy"])
        self.assertIn("unknown", post["pane_settles"])

    def test_a3_prompt_ack_01(self):
        pa = self.ev["prompt_acks"]
        self.assertEqual(pa["ack_type"], "agent_prompted")
        self.assertEqual(len(pa["concurrent"]), 2)
        for leg in pa["concurrent"]:
            # the ack carries the agent record — concurrent acks map
            # unambiguously to their targets
            self.assertIn("name", leg["ack"])
            self.assertEqual(leg["ack"]["pane_id"], leg["delivered_on"])
            self.assertIn("agent_session_kind", leg["ack"])
        names = {leg["ack"]["name"] for leg in pa["concurrent"]}
        self.assertEqual(names, {"a3-pi-1", "a3-pi-inflight"})
        devin = pa["cross_harness"]["devin"]
        self.assertEqual(devin["agent_session_kind"], "id")
        self.assertEqual(devin["agent_session_value"], "trail-passenger")
        # the recorded agy/claude entries carry only the delivery word and
        # pane_id; for agy the absent agent_session fields are structural —
        # agy has no agent_session on any surface (a3_agy_session_01)
        self.assertEqual(pa["cross_harness"]["agy"],
                         {"delivered_word": "ECHO", "pane_id": "w3:p5"})
        self.assertEqual(pa["cross_harness"]["claude"],
                         {"delivered_word": "FOXTROT", "pane_id": "w3:p2"})
        snap = pa["ack_is_delivery_snapshot"]
        self.assertEqual(snap["agent_status_in_ack"], "idle")
        self.assertFalse(snap["has_prompt_id"])
        self.assertFalse(snap["has_delivery_sequence"])

    def test_a3_prompt_notfound_01(self):
        pf = self.ev["pre_flight_errors"]
        self.assertEqual(pf["prompt_to_shell_pane"]["error"]["code"],
                         "agent_not_found")
        self.assertEqual(pf["prompt_to_shell_pane"]["id"],
                         "cli:agent:prompt")
        self.assertEqual(pf["get_unknown_name"]["error"]["code"],
                         "agent_not_found")

    def test_a3_agy_session_01(self):
        """Confirmed-negative: Herdr surfaces no agent_session for agy on any
        surface; the transcript source is qualified out-of-band under
        ~/.gemini/antigravity-cli/ (resolves the A5 AGY gap)."""
        agy = self.ev["agy_transcript"]
        self.assertEqual(agy["agent_session_absent_on"],
                         ["start response", "agent get", "agent list"])
        self.assertIsNone(agy["herdr_session_source_for_agy"])
        uuid = agy["conversation_uuid"]
        # all three stores share the conversation uuid
        self.assertIn(uuid, agy["sqlite"]["path"])
        self.assertIn(uuid, agy["jsonl"]["brain_logs_dir"])
        self.assertIn("presence/<uuid>.lock",
                      agy["presence_lock"]["path_glob"])
        self.assertEqual(agy["presence_lock"]["bytes"], 0)
        self.assertTrue(agy["presence_lock"]["present_while_live"])
        self.assertIn("steps", agy["sqlite"]["tables"])
        self.assertIn("trajectory_meta", agy["sqlite"]["tables"])
        self.assertIn("transcript.jsonl", agy["jsonl"]["files"])
        self.assertIn("step_index", agy["jsonl"]["record_shape"])
        self.assertIn("content", agy["jsonl"]["record_shape"])
        self.assertEqual(agy["artifacts_appear"],
                         "after_first_turn_not_at_tui_open")
        self.assertIn("conversation uuid", agy["discovery_correlation"])

    def test_a3_pane_cwd_01(self):
        fb = self.ev["pane_cwd_fallback"]
        self.assertFalse(fb["failed"])
        self.assertEqual(fb["resulting_cwd"], "/home/user")
        self.assertIn("cwd", fb["only_tell"])


class A4A6Identity(unittest.TestCase):
    """contract-a4-a6.md — child identity across moves, replacement and
    crashes, on protocol 22. Confirmed-negative (incarnation proof) and the
    needs-owner-decision mechanism items carry no tests by design."""

    def setUp(self):
        self.ev = captures()
        self.cap = self.ev["captures"]
        self.hist = fixture_json("a46-restart-history.json")
        self.subset = fixture_json("protocol22-subset.json")

    STABLE = ("tagged-before-move", "after-tab-move", "after-workspace-move",
              "after-native-new-settled", "replacement-native",
              "before-native-kill", "third-native", "after-same-tab-swap")

    def test_a46_surfaces_agree(self):
        for name in self.STABLE:
            with self.subTest(capture=name):
                snap, pane, agent = child_identity(self.cap[name])
                self.assertIsNotNone(snap, name)
                self.assertIsNotNone(agent, name)
                for field in ("pane_id", "tab_id", "terminal_id",
                              "agent_session", "tokens"):
                    self.assertEqual(snap[field], pane[field])
                    self.assertEqual(pane[field], agent[field])
                self.assertIn("name", agent)
                self.assertNotIn("name", pane)
                self.assertIn("label", pane)
                self.assertNotIn("label", agent)

    def test_a4_tab_move_preserves_identity(self):
        before = child_identity(self.cap["tagged-before-move"])
        after = child_identity(self.cap["after-tab-move"])
        for field in ("pane_id", "terminal_id", "agent_session", "label",
                      "tokens"):
            self.assertEqual(before[1][field], after[1][field])
        self.assertEqual(before[2]["name"], after[2]["name"])
        self.assertEqual(before[1]["tab_id"], "w1:t1")
        self.assertEqual(after[1]["tab_id"], "w1:t2")

    def test_a4_cross_tab_swap_noop(self):
        out, rec = command_stdout(
            self.ev, "pane swap --source-pane w1:p1 --target-pane w1:p3")
        swap = out["result"]["swap"]
        self.assertFalse(swap["changed"])
        self.assertEqual(swap["reason"], "cross_tab")
        self.assertEqual(swap["source_pane_id"], "w1:p1")
        self.assertEqual(swap["target_pane_id"], "w1:p3")

    def test_a4_workspace_move_new_locator(self):
        out, _ = command_stdout(
            self.ev, "pane move w1:p1 --new-workspace")
        res = out["result"]["move_result"]
        self.assertTrue(res["changed"])
        self.assertEqual(res["previous_pane_id"], "w1:p1")
        bsnap, bpane, bagent = child_identity(self.cap["after-cross-tab-swap"])
        snap, pane, agent = child_identity(self.cap["after-workspace-move"])
        self.assertEqual(pane["pane_id"], "w2:p1")
        self.assertEqual(pane["workspace_id"], "w2")
        # label and tokens are preserved across the move on every identity
        # surface — captured before, compared against after
        for field in ("terminal_id", "agent_session", "label", "tokens"):
            self.assertEqual(bpane[field], pane[field])
            self.assertEqual(bsnap[field], snap[field])
        self.assertEqual(bagent["tokens"], agent["tokens"])
        self.assertEqual(bagent["name"], agent["name"])

    def test_a4_old_locator_apis_diverge(self):
        rec = next(r for r in commands(self.ev)
                   if "agent get w1:p1" in r["command"])
        err = json.loads(rec["stderr"])
        self.assertEqual(err["error"]["code"], "agent_not_found")
        stale = [r for r in commands(self.ev)
                 if " pane get w1:p1" in r["command"]
                 and json.loads(r["stdout"]).get("result", {}).get(
                     "pane", {}).get("pane_id") != "w1:p1"]
        self.assertEqual(len(stale), 1)
        self.assertEqual(json.loads(stale[0]["stdout"])["result"]["pane"]
                         ["pane_id"], "w2:p1")

    def test_a4_same_tab_swap_geometry_only(self):
        out, _ = command_stdout(
            self.ev, "pane swap --source-pane w2:p1 --target-pane w2:p2")
        self.assertTrue(out["result"]["swap"]["changed"])
        cap = self.cap["after-same-tab-swap"]
        rects = {}
        for layout in cap["snapshot"]["layouts"]:
            for p in layout["panes"]:
                rects[p["pane_id"]] = p["rect"]
        self.assertEqual(rects["w2:p1"]["x"], 60)
        self.assertEqual(rects["w2:p2"]["x"], 0)
        _, pane, agent = child_identity(cap)
        self.assertEqual(pane["terminal_id"], "term_65c9180ad9c2f1")
        self.assertEqual(agent["name"], "a46-first")

    def test_a4_native_new_replaces_session(self):
        moved = get_pane(self.cap["after-workspace-move"])
        settled = get_pane(self.cap["after-native-new-settled"])
        self.assertNotEqual(moved["agent_session"]["value"],
                            settled["agent_session"]["value"])
        for field in ("pane_id", "tab_id", "terminal_id", "label", "tokens"):
            self.assertEqual(moved[field], settled[field])

    def test_a4_native_replace_new_session(self):
        exited = self.cap["graceful-exit-shell"]
        self.assertEqual(exited["agents"], [])
        self.assertIsNone(get_pane(exited).get("agent_session"))
        old = get_pane(self.cap["after-native-new-settled"])
        repl = get_pane(self.cap["replacement-native"])
        self.assertNotEqual(old["agent_session"]["value"],
                            repl["agent_session"]["value"])
        for field in ("pane_id", "terminal_id", "label", "tokens"):
            self.assertEqual(old[field], repl[field])
        self.assertEqual(agent_row(self.cap["replacement-native"],
                                   "w2:p1")["name"], "a46-first")

    def test_a4_pane_replacement_fields(self):
        closed = self.cap["closed-pane"]
        self.assertEqual(closed["get"]["error"]["code"], "pane_not_found")
        self.assertEqual(closed["agents"], [])
        terms = {p["terminal_id"] for p in closed["snapshot"]["panes"]}
        self.assertNotIn("term_65c9180ad9c2f1", terms)
        rec = self.cap["recreated-pane"]
        _, pane, _ = child_identity(rec)
        self.assertEqual(pane["pane_id"], "w2:p3")
        self.assertNotIn(pane["terminal_id"], terms)

    def test_a6_kill_stale_read(self):
        before = child_identity(self.cap["before-native-kill"])
        immediate = child_identity(self.cap["after-native-kill-immediate"])
        # the first post-kill read still advertises the dead session
        self.assertEqual(before[1]["agent_session"],
                         immediate[1]["agent_session"])
        self.assertEqual(immediate[2]["agent_status"], "idle")
        settled = self.cap["after-native-kill-settled"]
        self.assertEqual(settled["agents"], [])

    def test_a6_kill_pane_survives(self):
        before = self.cap["before-native-kill"]
        settled = self.cap["after-native-kill-settled"]
        bpane = get_pane(before)
        spane = snapshot_pane(settled, "w2:p1")
        self.assertIsNotNone(spane)
        for field in ("pane_id", "terminal_id", "label", "tokens"):
            self.assertEqual(bpane[field], spane[field])
        self.assertIsNone(spane.get("agent_session"))
        self.assertIsNone(get_pane(settled).get("agent_session"))
        self.assertEqual(settled["agents"], [])

    def test_a6_shell_kill_race(self):
        immediate = self.cap["after-shell-kill-immediate"]
        self.assertIsNotNone(snapshot_pane(immediate, "w1:p3"))
        self.assertEqual(immediate["get"]["error"]["code"], "pane_not_found")

    def test_a6_shell_kill_absent(self):
        later = self.cap["third-native"]
        self.assertIsNone(snapshot_pane(later, "w1:p3"))
        terms = {p["terminal_id"] for p in later["snapshot"]["panes"]}
        self.assertNotIn("term_65c918a9af7563", terms)

    def test_a6_split_env_not_metadata(self):
        split_cmd = [r for r in commands(self.ev)
                     if "GOV_RUN_ID=a46-atomic-env" in r["command"]]
        self.assertEqual(len(split_cmd), 1)
        self.assertIn("--env GOV_RUN_ID=", split_cmd[0]["command"])
        pane = get_pane(self.cap["recreated-pane"])
        self.assertEqual(pane["pane_id"], "w2:p3")
        for field in ("env", "label", "tokens", "agent_session"):
            self.assertIsNone(pane.get(field))

    def test_a6_tags_nonunique(self):
        cap = self.cap["duplicate-tags"]
        tagged = [p for p in cap["snapshot"]["panes"]
                  if p.get("label") == "a46-pane-tag"
                  and (p.get("tokens") or {}).get("gov_run") == "a46-probe-run"]
        self.assertGreaterEqual(len(tagged), 2)
        self.assertEqual(len({p["terminal_id"] for p in tagged}),
                         len(tagged))

    def test_a6_session_ref_not_file(self):
        rec = next(r for r in self.ev["records"]
                   if "advertised_native_path" in r)
        self.assertFalse(rec["exists_after_kill"])

    def test_a6_creation_tag_surface_absent_in_split(self):
        split = self.subset["objects"]["PaneSplitParams"]["request"]
        self.assertNotIn("label", split["properties"])
        self.assertNotIn("tokens", split["properties"])
        node_pane = next(v for v in
                         self.subset["objects"]["LayoutNode"]["request"]
                         ["oneOf"] if v["const"] == "pane")
        for field in ("label", "cwd", "env", "command", "pane_id"):
            self.assertIn(field, node_pane["properties"])

    def test_a4_history_single_restart_window(self):
        h = self.hist
        self.assertEqual(h["matching_1912_utc_records"], 0)
        win = h["windows"]
        self.assertEqual(len(win), 1)
        self.assertEqual(win[0]["from"], "2026-09-27T22:12:45.640Z")
        self.assertEqual(win[0]["to"],
                         h["daemon_runtime_state"]["startedAt"])
        # the documented local time is the same instant as the UTC window
        self.assertEqual(h["date_conversion_stdout"].strip(),
                         "2026-09-27T19:12:50-03:00")

    def test_a4_history_job_terminal_not_death(self):
        h = self.hist
        terminal = [r["record"] for r in h["records"]
                    if r["record"].get("kind") == "job_terminal"]
        self.assertTrue(terminal)
        pending = [r for r in terminal
                   if (r.get("handoff") or {}).get("state")
                   == "recovery_pending"]
        self.assertTrue(pending)
        for rec in pending:
            self.assertIn("cancelled", rec["actions"])
        for run in h["current_persisted_run_identities"]:
            self.assertEqual(run["lifecycle"]["state"], "handed_off")
            self.assertEqual(run["lifecycle"]["detail"],
                             "daemon_restart_reattached")


class A5Transcript(unittest.TestCase):
    """contract-a5.md — transcript resolution and failure contracts, probed
    against the reference readers on synthetic inputs. Fixtures carry both
    the samples and the recorded outcomes; tests check they agree."""

    def setUp(self):
        self.out = fixture_json("a5-probe-outcomes.json")
        self.obs = fixture_json("a5-native-observations.json")

    def sample(self, name) -> bytes:
        path = SAMPLES / name
        if not path.is_file():
            raise FileNotFoundError(f"missing transcript sample: {path}")
        return path.read_bytes()

    def test_a5_run_binding_pinned_session(self):
        ident = self.obs["identity"]
        self.assertNotEqual(ident["run_marker"], ident["native_pi_id"])
        pi = self.obs["native"]["pi"]
        self.assertIn(ident["native_pi_id"], pi["path"])
        self.assertNotIn(ident["run_marker"], pi["path"])
        self.assertTrue(pi["header_id_matches_filename"])
        self.assertTrue(pi["header_id_matches_environment"])

    def test_a5_pi_exact_path_no_glob(self):
        a = self.sample("pi-duplicate-a.jsonl")
        b = self.sample("pi-duplicate-b.jsonl")
        # both files advertise the same native header id but differ in content
        self.assertEqual(json.loads(a.split(b"\n")[0])["id"],
                         json.loads(b.split(b"\n")[0])["id"])
        self.assertNotEqual(a.split(b"\n")[1], b.split(b"\n")[1])
        rec = case(self.out, "pi-duplicate-id-files")
        self.assertEqual(rec["candidates"], 2)
        self.assertTrue(rec["exact_path_selected_a"])
        self.assertEqual(rec["id_only"]["failure"]["kind"],
                         "session_pointer_invalid")
        self.assertEqual(rec["id_only"]["failure"]["detail"]["reason"],
                         "kind_not_path")

    def test_a5_partial_write_rejected(self):
        data = self.sample("pi-header-plus-partial-utf8.jsonl")
        records, consumed, failure = jsonl_scan(data)
        self.assertEqual(len(records), 1)  # header only
        self.assertEqual(consumed, 82)
        self.assertIsNone(failure)
        rec = case(self.out, "pi-partial-utf8")
        self.assertEqual(rec["first"]["cursor"], consumed)
        self.assertEqual(rec["first"]["events"], 1)
        # completing the record emits it — the pending tail was retained
        full = data + b"\xa9\"}]}}\n"
        records, consumed, failure = jsonl_scan(full)
        self.assertEqual(len(records), 2)
        self.assertEqual(consumed, 172)
        self.assertEqual(rec["after_append"]["cursor"], 172)

    def test_a5_complete_json_without_newline_pending(self):
        data = self.sample("pi-complete-record-no-newline.jsonl")
        json.loads(data)  # the tail really is complete JSON, only the LF is missing
        records, consumed, failure = jsonl_scan(data)
        self.assertEqual((records, consumed, failure), ([], 0, None))
        rec = case(self.out, "pi-complete-json-without-newline")
        self.assertEqual(rec["events"], 0)
        self.assertEqual(rec["cursor"], 0)
        self.assertIsNone(rec["failure"])

    def test_a5_malformed_record_fails_window(self):
        data = self.sample("pi-header-plus-malformed.jsonl")
        records, consumed, failure = jsonl_scan(data)
        self.assertEqual(failure, ("source_malformed", 82))
        rec = case(self.out, "pi-malformed-complete-record")
        self.assertEqual(rec["failure"]["kind"], "source_malformed")
        self.assertEqual(rec["failure"]["detail"]["offset"], failure[1])
        self.assertEqual(rec["events"], 0)  # the failed window emits nothing

    def test_a5_header_identity_not_validated_by_reference(self):
        head = json.loads(self.sample("pi-foreign-header.jsonl")
                          .split(b"\n")[0])
        self.assertEqual(head["id"], "different-native-id")
        self.assertEqual(head["cwd"], "/different")
        rec = case(self.out, "pi-header-identity-not-validated")
        self.assertEqual(rec["events"], 1)
        self.assertIsNone(rec["failure"])
        # documented gap: the governor's adapter must validate the header
        # itself (proposed A5-PI-HEADER); path selection is not identity proof

    def test_a5_devin_partial_document_retryable(self):
        partial = self.sample("devin-session-truncated.json")
        with self.assertRaises(json.JSONDecodeError):
            json.loads(partial)
        full = json.loads(self.sample("devin-session.json"))
        self.assertEqual(full["session_id"], "native-synthetic")
        rec = case(self.out, "devin-partial-document")
        self.assertEqual(rec["partial"]["failure"]["kind"], "source_malformed")
        self.assertEqual(rec["partial"]["failure"]["detail"]["reason"],
                         "invalid_json")
        self.assertEqual(rec["completed"]["events"], 1)
        self.assertEqual(rec["completed"]["cursor"], 1)

    def test_a5_devin_identity_guards(self):
        foreign = json.loads(self.sample("devin-foreign-id.json"))
        self.assertNotEqual(foreign["session_id"], "native-synthetic")
        rec = case(self.out, "devin-identity-guards")
        self.assertEqual(rec["content_mismatch"]["failure"]["detail"]["reason"],
                         "session_id_mismatch")
        self.assertEqual(rec["traversal"]["failure"]["kind"],
                         "session_pointer_invalid")
        self.assertEqual(rec["traversal"]["failure"]["detail"]["reason"],
                         "id_not_filename_safe")
        # the unsafe component really is unsafe
        self.assertRegex("../escape", r"[/.]")

    def test_a5_devin_duplicate_root_exact(self):
        for root, msg in (("devin-duplicate-a", "a"), ("devin-duplicate-b", "b")):
            doc = json.loads(self.sample(f"{root}/native-synthetic.json"))
            self.assertEqual(doc["session_id"], "native-synthetic")
            self.assertEqual(doc["steps"][0]["message"], msg)
        rec = case(self.out, "devin-duplicate-id-files")
        self.assertEqual(rec["candidates"], 2)
        self.assertTrue(rec["exact_root_selected_a"])

    def test_a5_devin_xdg_root_resolution(self):
        xdg = json.loads(self.sample(
            "devin-xdg/devin/cli/transcripts/native-synthetic.json"))
        self.assertEqual(xdg["session_id"], "native-synthetic")
        rec = case(self.out, "devin-xdg-resolution")
        self.assertTrue(rec["resolved_synthetic_root"])
        self.assertEqual(rec["events"], 1)

    def test_a5_claude_ambiguous_candidates(self):
        uuid = "00000000-0000-4000-8000-000000000001"
        found = sorted((SAMPLES / "claude-projects")
                       .glob(f"*/{uuid}.jsonl"))
        self.assertEqual(len(found), 2)
        for path in found:
            first = json.loads(path.read_bytes().split(b"\n")[0])
            self.assertEqual(first["sessionId"], uuid)
        rec = case(self.out, "claude-ambiguous-id-files")
        self.assertEqual(rec["candidates"], 2)
        self.assertFalse(rec["general_ambiguity_resolver_present"])
        self.assertTrue(rec["fixed_slug_result"]["zeroProgressProven"])
        self.assertFalse(rec["alternate_only_result"])

    def test_a5_claude_partial_corrupt_quota(self):
        with self.assertRaises(json.JSONDecodeError):
            json.loads(self.sample("claude-quota-partial.jsonl"))
        corrupt = self.sample("claude-malformed-then-quota.jsonl")
        recs, consumed, failure = jsonl_scan(corrupt)
        # the malformed first line fails the scan before the quota record
        self.assertEqual(recs, [])
        self.assertEqual(failure, ("source_malformed", 0))
        self.assertEqual(json.loads(corrupt.split(b"\n")[1])
                         ["apiErrorStatus"], 429)
        tail = self.sample("claude-quota-plus-partial-tool.jsonl")
        recs, consumed, failure = jsonl_scan(tail)
        self.assertEqual(len(recs), 1)  # quota record consumed; tail pending
        rec = case(self.out, "claude-partial-and-corrupt-records")
        self.assertFalse(rec["only_partial"])
        self.assertTrue(rec["completed"]["zeroProgressProven"])
        self.assertTrue(rec["partial_tool_record_after_quota"]
                        ["zeroProgressProven"])
        self.assertTrue(rec["malformed_complete_record_before_quota"]
                        ["zeroProgressProven"])
        # contract note: a corrupt tail must defeat no-progress proofs even
        # though this quota reader still returns them — A5-CORRUPT-NOT-ABSENCE

    def test_a5_permission_denied_causes(self):
        for harness in ("pi", "devin"):
            rec = case(self.out, f"{harness}-permission-denied")
            self.assertEqual(rec["errno"], "EACCES")
            self.assertEqual(rec["result"]["failure"],
                             {"kind": "source_unreadable",
                              "detail": {"code": "EACCES"}})
        self.assertIs(case(self.out, "claude-permission-denied")["result"],
                      False)
        # mechanism check on a fixture sample: a 000-mode file really raises.
        # Under DAC override (root, CAP_DAC_OVERRIDE) the mode denies nothing —
        # the same privileged-runner guard the gate selftests apply
        # (scripts/test_guardrails.py). The committed EACCES outcomes above
        # still pin the contract in that environment.
        with tempfile.NamedTemporaryFile(delete=False) as fh:
            fh.write(self.sample("devin-session.json"))
            target = fh.name
        try:
            os.chmod(target, 0)
            if not os.access(target, os.R_OK):
                with self.assertRaises(PermissionError):
                    Path(target).read_bytes()
        finally:
            os.chmod(target, 0o600)
            os.unlink(target)

    def test_a5_vanish_before_open_unreadable(self):
        for harness in ("pi", "devin"):
            rec = case(self.out, f"{harness}-vanish-before-open")
            self.assertTrue(rec["path_absent"])
            self.assertEqual(rec["result"]["failure"]["detail"]["code"],
                             "ENOENT")
        self.assertIs(case(self.out, "claude-vanish-before-open")["result"],
                      False)

    def test_a5_vanish_after_open_stale_inode(self):
        for harness in ("pi", "devin"):
            rec = case(self.out, f"{harness}-vanish-after-open-before-read")
            self.assertTrue(rec["path_absent"])
            self.assertEqual(rec["result"]["events"], 1)
            self.assertIsNone(rec["result"]["failure"])
        # bytes from the opened inode are evidence of that pinned snapshot,
        # never proof the pathname is live or permission to reselect

    def test_a5_cursor_rewrite_detected(self):
        rec = case(self.out, "cursor-rewrites")
        self.assertEqual(rec["pi"]["failure"]["kind"], "source_rewritten")
        self.assertEqual(rec["devin"]["failure"]["detail"]["reason"],
                         "anchor_mismatch")
        self.assertIsNotNone(rec["pi"]["cursor"])
        self.assertIsNotNone(rec["devin"]["cursor"])

    def test_a5_oversized_record_gap_exposed(self):
        rec = case(self.out, "pi-oversized-scan")
        self.assertEqual(rec["failure"]["kind"], "record_exceeds_budget")
        self.assertTrue(rec["failure"]["detail"]["skipped"])
        self.assertGreater(rec["actual_bytes"], 16 * 1024 * 1024)
        nxt = case(self.out, "pi-after-skipped-record")
        self.assertEqual(nxt["events"], 1)
        step = case(self.out, "devin-oversized-step")
        self.assertEqual(step["skipped"]["failure"]["kind"],
                         "record_exceeds_budget")
        self.assertEqual(step["resumed"]["events"], 1)

    def test_a5_devin_source_ceiling(self):
        rec = case(self.out, "devin-source-ceiling")
        self.assertEqual(rec["failure"]["kind"], "source_exceeds_budget")
        self.assertEqual(rec["failure"]["detail"]["budget"], 8388608)
        self.assertGreater(rec["failure"]["detail"]["bytesAtLeast"], 8388608)

    def test_a5_symlink_policies_differ(self):
        rec = case(self.out, "symlink-behavior")
        self.assertTrue(rec["pi_followed"])
        self.assertTrue(rec["devin_followed_then_content_id_rejected"])
        self.assertTrue(rec["claude_refused"])

    def test_a5_structured_failure_no_terminal_fallback(self):
        rec = case(self.out, "source-selection-and-fallback")
        self.assertEqual(rec["structured_failure_terminal_calls"], 0)
        sel = {s["harness"]: s for s in rec["selections"]}
        for harness in ("pi", "devin"):
            self.assertEqual(sel[harness]["failure"]["kind"],
                             "source_unreadable")
        self.assertEqual(rec["total_terminal_calls"], 3)
        unchanged = rec["unchanged_terminal"]
        self.assertEqual((unchanged["events"], unchanged["failure"]), (0, None))
        self.assertEqual(rec["terminal_error"]["failure"]["detail"]["code"],
                         "EIO")

    def test_a5_agy_unqualified_terminal_only(self):
        sel = {s["harness"]: s for s in
               case(self.out, "source-selection-and-fallback")["selections"]}
        for harness in ("claude", "agy", "unknown"):
            self.assertEqual(sel[harness]["source"], "tmux-fallback")
        agy = self.obs["agy"]
        self.assertTrue(agy["binary_present"])
        self.assertTrue(agy["catalog_harness_key_present"])
        self.assertFalse(agy["dot_agy_dir_present"])
        self.assertFalse(agy["transcript_adapter_present"])

    def test_a5_native_default_locations(self):
        native = self.obs["native"]
        self.assertRegex(native["pi"]["path"],
                         r"\.pi/agent/sessions/--.*--/.*\.jsonl$")
        self.assertTrue(native["pi"]["ends_newline"])
        self.assertEqual(native["devin"]["schema_version"], "ATIF-v1.7")
        self.assertIn("/.local/share/devin/cli/transcripts/",
                      native["devin"]["path"])
        self.assertFalse(native["devin"]["ends_newline"])
        self.assertTrue(native["devin"]["sequential_step_ids"])
        self.assertRegex(native["claude"]["path"],
                         r"\.claude/projects/[^/]+/[0-9a-f-]+\.jsonl$")
        self.assertTrue(native["claude"]["record_session_ids_all_match_filename"])
        self.assertEqual(native["claude"]["exact_id_candidate_count"], 1)


class JevWireContract(unittest.TestCase):
    """contract-jev.md — Jev/System One wire contract evidence: five live
    judgment calls through the real TypeSafeSpecClient, the verbatim
    response envelope, triggered error classes, the credential seam, and
    the models surface. Confirmed-negative items (no local CLI or
    contract-introspection endpoint; untriggered 403/404/422/429/5xx
    error classes) are documented in the research file and have no test
    by design. Code-cited constants the report records but did not
    trigger live sit in the fixture's `code_cited_gates`/`auth_resolution`
    sections, labeled as code citations."""

    def setUp(self):
        self.ev = fixture_json("jev-wire-evidence.json")
        self.raw = fixture_json("jev-raw-response.json")
        self.launch = fixture_json("jev-launch-evaluation.json")
        self.review = fixture_json("jev-supervision-review.json")

    @staticmethod
    def transport_component(status, error_type):
        """The recorded mapping rule: http_<status>_<error_type> for a
        conforming snake_case error_type, else the http_<status> fallback."""
        if re.fullmatch(r"[a-z][a-z0-9_]{0,63}", error_type or ""):
            return f"http_{status}_{error_type}"
        return f"http_{status}"

    def call(self, call_id):
        for c in self.ev["calls"]:
            if c["id"] == call_id:
                return c
        raise AssertionError(f"no recorded jev call {call_id!r}")

    def error(self, probe):
        for e in self.ev["errors"]:
            if e["probe"] == probe:
                return e
        raise AssertionError(f"no recorded jev error probe {probe!r}")

    def check_choice(self, ans, labels, sum_bound):
        self.assertEqual(ans["type"], "choice")
        self.assertIn(ans["choice"], labels)
        self.assertTrue(0 <= ans["confidence"] <= 1)
        self.assertEqual(set(ans["probabilities"]), set(labels))
        for p in ans["probabilities"].values():
            self.assertTrue(0 <= p <= 1)
        self.assertLessEqual(abs(sum(ans["probabilities"].values()) - 1),
                             sum_bound)

    def test_jev_probe_calls_recorded(self):
        """Fixture completeness guard: the report's five live calls and
        seven error probes are present and every call returned a response."""
        self.assertEqual(len(self.ev["calls"]), 5)
        self.assertEqual(len(self.ev["errors"]), 7)
        for c in self.ev["calls"]:
            self.assertEqual(c["result"]["kind"], "response")
            self.assertGreater(c["latency_ms"], 0)

    def test_jev_request_wire_shape(self):
        """CT-JEV-REQ-1: every call is POST {base}/v1/systemone with a
        Bearer Authorization header and a JSON body; state is the
        semantic task projection only — no caller tier, no catalog."""
        for c in self.ev["calls"]:
            wire = c["wire"]
            self.assertEqual(wire["method"], "POST")
            self.assertEqual(wire["url"],
                             "https://api.typesafe.ai/v1/systemone")
            self.assertTrue(
                wire["headers"]["Authorization"].startswith("Bearer "))
            self.assertEqual(wire["headers"]["Content-Type"],
                             "application/json")
        body = self.launch["request"]["body"]
        self.assertEqual(set(body), {"model", "state", "questions"})
        self.assertEqual(body["model"], "jev-latest")
        self.assertEqual(set(body["state"]), {"task"})
        self.assertEqual(set(body["state"]["task"]),
                         {"objective", "scope", "doneWhen", "constraints"})
        self.assertNotIn("tier", json.dumps(body["state"]))
        self.assertEqual(set(body["questions"]),
                         {"done_when_verifiable", "intent",
                          "weakest_sufficient_tier"})
        # the reviewer's projection differs but also carries no tier
        self.assertNotIn("tier",
                         json.dumps(self.review["request"]["body"]["state"]))

    def test_jev_request_too_large_client_gate(self):
        """CT-JEV-REQ-2: a serialized request over 96 KiB abstains
        invalid_response/request_too_large before any socket write."""
        e = self.error("oversize-client-side")
        self.assertEqual(e["result"]["kind"], "abstained")
        self.assertEqual(e["result"]["reason"], "invalid_response")
        self.assertEqual(e["result"]["component"], "request_too_large")
        self.assertGreater(e["result"]["requestSize"]["bytes"], 96 * 1024)
        # no request was sent — no wire capture, status, or request id
        for absent in ("wire", "status", "request_id", "body"):
            self.assertNotIn(absent, e)
        self.assertEqual(
            self.ev["code_cited_gates"]["max_spec_request_bytes"], 96 * 1024)
        # every live call stayed under the client gate
        for c in self.ev["calls"]:
            self.assertLess(c["request_bytes"]["bytes"], 96 * 1024)

    def test_jev_response_envelope_metadata(self):
        """CT-JEV-RESP-4: model and usage{input_tokens,output_tokens} are
        present but non-semantic; the resolved model is opaque metadata,
        not the request alias."""
        for resp in (self.raw["raw_body"], self.launch["response"],
                     self.review["response"]):
            self.assertIsInstance(resp["model"], str)
            self.assertEqual(set(resp["usage"]),
                             {"input_tokens", "output_tokens"})
            for tokens in resp["usage"].values():
                self.assertIsInstance(tokens, int)
                self.assertGreaterEqual(tokens, 0)
        self.assertEqual(self.launch["response"]["model"], "jev-1.13.0")
        self.assertNotEqual(self.launch["response"]["model"],
                            self.launch["request"]["body"]["model"])

    def test_jev_noul_answer_shape(self):
        """CT-JEV-RESP-1: answers are keyed by question name; a noul
        answer is exactly {type:"noul", noul∈[0,1]} — a calibrated
        P(yes) with no confidence field."""
        for answers in (self.raw["raw_body"]["answers"],
                        self.launch["response"]["answers"],
                        self.review["response"]["answers"]):
            for ans in answers.values():
                if ans["type"] == "noul":
                    self.assertEqual(set(ans), {"type", "noul"})
                    self.assertTrue(0 <= ans["noul"] <= 1)
        for cap in (self.launch, self.review):
            self.assertEqual(set(cap["response"]["answers"]),
                             set(cap["request"]["body"]["questions"]))
        # done_when_verifiable is a mid-range P(yes), never a boolean
        for c in self.ev["calls"]:
            p = c["result"]["response"]["quality"]["done_when_verifiable"]
            self.assertTrue(0 < p < 1)

    def test_jev_choice_answer_shape(self):
        """CT-JEV-RESP-2: choice ∈ criteria labels, confidence∈[0,1],
        probabilities cover the exact label set. The router enforces
        |Σ−1|≤1e-6 on its question set; a recorded reviewer-path response
        sums to 0.99 — the service does not promise an exact-1 sum, so a
        governor contract must not require tighter than it observed."""
        raw_ans = self.raw["raw_body"]["answers"]["sized"]
        self.check_choice(raw_ans, raw_ans["probabilities"].keys(), 1e-6)
        for cap, bound in ((self.launch, 1e-6), (self.review, 0.05)):
            for name, q in cap["request"]["body"]["questions"].items():
                if q["type"] == "choice":
                    self.check_choice(cap["response"]["answers"][name],
                                      q["criteria"].keys(), bound)
        # pin the observed deviation exactly: Σ = 0.99 on the wire
        review_probs = (self.review["response"]["answers"]["reason"]
                        ["probabilities"])
        self.assertAlmostEqual(sum(review_probs.values()), 0.99)

    def test_jev_confidence_not_max_probability(self):
        """CT-JEV-RESP-3: confidence is a distinct calibrated field —
        recorded pairs contradict any alias to the top probability, and
        every divergent answer still parsed as a valid response."""
        pairs = []
        sized = self.raw["raw_body"]["answers"]["sized"]
        pairs.append((sized["confidence"], max(sized["probabilities"].values())))
        for cap in (self.launch, self.review):
            for ans in cap["response"]["answers"].values():
                if ans["type"] == "choice":
                    pairs.append((ans["confidence"],
                                  max(ans["probabilities"].values())))
        for c in self.ev["calls"]:
            for dim in ("intent", "tier"):
                r = c["result"]["response"][dim]
                pairs.append((r["confidence"],
                              max(r["probabilities"].values())))
        self.assertIn((0.96, 0.98), pairs)   # raw wire record
        self.assertIn((0.63, 0.69), pairs)   # router-normalized record
        self.assertIn((0.92, 0.93), pairs)   # reviewer-path record
        divergent = [p for p in pairs if p[0] != p[1]]
        self.assertGreaterEqual(len(divergent), 3)

    def test_jev_error_authentication_mapping(self):
        """CT-JEV-ERR-1: 401 authentication_error maps to
        http_401_authentication_error; the unresolvable-key abstain is
        code-cited evidence (authentication_unavailable/api_key, no
        request sent)."""
        e = self.error("bad-key")
        self.assertEqual(e["name"], "AuthenticationError")
        self.assertEqual(e["status"], 401)
        self.assertTrue(e["request_id"].startswith("req_"))
        detail = e["body"]["detail"]
        self.assertEqual(detail["error_type"], "authentication_error")
        self.assertEqual(
            self.transport_component(e["status"], detail["error_type"]),
            "http_401_authentication_error")
        gate = self.ev["code_cited_gates"]["unresolvable_key_abstain"]
        self.assertEqual(gate["reason"], "authentication_unavailable")
        self.assertEqual(gate["component"], "api_key")

    def test_jev_error_body_component_mapping(self):
        """CT-JEV-ERR-2: 400 bodies map by detail.error_type; a
        non-conforming error_type falls back to http_<status>."""
        e = self.error("malformed-question")
        self.assertEqual(e["status"], 400)
        self.assertEqual(
            self.transport_component(e["status"],
                                     e["body"]["detail"]["error_type"]),
            "http_400_api_usage_error")
        big = self.error("oversize-256KB-raw")
        self.assertEqual(big["status"], 400)
        detail = json.loads(big["body_prefix"])["detail"]
        self.assertEqual(
            self.transport_component(big["status"], detail["error_type"]),
            "http_400_max_tokens_exceeded")
        rule = self.ev["code_cited_gates"]["error_component_rule"]
        self.assertEqual(rule["fallback"], "http_<status>")
        self.assertEqual(rule["non_api_transport_component"], "transport")
        self.assertEqual(self.transport_component(418, "Weird Type!"),
                         "http_418")

    def test_jev_timeout_abort_no_retry(self):
        """CT-JEV-ERR-3: timeout surfaces APITimeoutError, caller abort
        APIUserAbortError. Only the pre-abort proves no request was
        sent; a timeout proves no response was received — send is not
        disproven. The configured policy is maxRetries:0 and no retry
        header was observed."""
        t = self.error("timeout-1ms")
        self.assertEqual(t["name"], "APITimeoutError")
        self.assertEqual(t["timeout_ms"], 1)
        # no response was received: no status, no request id. A
        # client-side timeout cannot disprove the request went out, so
        # the absence of a wire capture is not asserted here.
        for absent in ("status", "request_id"):
            self.assertNotIn(absent, t)
        a = self.error("pre-abort")
        self.assertEqual(a["name"], "APIUserAbortError")
        # aborted before send: no request went out
        for absent in ("status", "request_id", "wire"):
            self.assertNotIn(absent, a)
        self.assertEqual(self.error("empty-questions")["name"],
                         "TypeSafeError")
        self.assertEqual(
            self.ev["code_cited_gates"]["router_retry_max_retries"], 0)
        for c in self.ev["calls"]:
            self.assertNotIn("X-TypeSafe-Retry-Count",
                             c["wire"]["headers"])

    def test_jev_auth_resolution_order(self):
        """CT-JEV-AUTH-1: explicit option → auth.json typesafe api_key →
        TYPESAFE_API_KEY; indirected keys pass through verbatim; only
        presence metadata was captured, never a key value."""
        auth = self.ev["auth_resolution"]
        self.assertEqual(auth["order"],
                         ["explicit_option", "auth_store_typesafe_api_key",
                          "env_TYPESAFE_API_KEY"])
        self.assertTrue(auth["store_observed"]["exists"])
        self.assertEqual(auth["store_observed"]["mode"], "0600")
        self.assertEqual(auth["store_observed"]["typesafe_entry_type"],
                         "api_key")
        self.assertTrue(auth["env_typesafe_api_key_set"])
        self.assertTrue(auth["store_wins_over_env"])
        self.assertTrue(auth["indirected_key_passed_verbatim_never_executed"])
        self.assertFalse(auth["key_material_logged"])
        seam = self.ev["key_seam"]
        self.assertTrue(seam["resolved"])
        self.assertIsInstance(seam["length"], int)
        self.assertEqual(set(seam), {"resolved", "length"})

    def test_jev_spread_top_label_route(self):
        """CT-JEV-PROB-1: a sub-0.5-confidence spread still yields the
        verbatim top label as the route input — no abstain on low
        confidence."""
        sec = self.call("security-boundary")["result"]["response"]["intent"]
        self.assertEqual(sec["value"], "reason")
        self.assertLess(sec["confidence"], 0.5)
        self.assertEqual(
            sec["value"],
            max(sec["probabilities"], key=sec["probabilities"].get))
        long = self.call("long-running")["result"]["response"]["tier"]
        self.assertEqual(long["value"], "max")
        self.assertLess(long["confidence"], 0.5)
        self.assertEqual(
            long["value"],
            max(long["probabilities"], key=long["probabilities"].get))
        for cid in ("security-boundary", "long-running"):
            self.assertEqual(self.call(cid)["result"]["kind"], "response")

    def test_jev_models_surface(self):
        """GET /v1/models — the only introspection surface — returns
        ModelCards {name, description, release_date}."""
        models = self.ev["models"]
        self.assertEqual(len(models), 2)
        self.assertEqual({m["name"] for m in models},
                         {"jev-latest", "jev-preview"})
        for m in models:
            self.assertEqual(set(m), {"name", "description", "release_date"})

    def test_jev_no_credential_material(self):
        """Committed evidence carries presence metadata only — every
        recorded Authorization header is the redacted placeholder."""
        for c in self.ev["calls"]:
            self.assertEqual(c["wire"]["headers"]["Authorization"],
                             "Bearer <redacted>")
        self.assertEqual(set(self.ev["key_seam"]), {"resolved", "length"})


class A1PrimeNativeStdio(unittest.TestCase):
    """contract-a1prime.md (p0p1 artifact bundle, probe 2026-09-29) — the
    A1' native per-session stdio relay probe behind ADR-0004: each harness
    spawned the probe MCP server (one `probe_env` tool, newline-delimited
    JSON-RPC over stdio) from inside a Herdr pane. The fixture distills the
    probe's own spawn logs: event records kept verbatim, env names filtered
    to HERDR_*/PWD plus harness markers (dropped count recorded), env
    values limited to the five whitelisted names, paths scrubbed to
    /home/user/ placeholders; model-facing facts come from the recorded
    run transcripts."""

    HERDR_PWD = {"HERDR_PANE_ID", "HERDR_WORKSPACE_ID", "HERDR_TAB_ID",
                 "HERDR_ENV", "PWD"}

    def setUp(self):
        self.ev = fixture_json("a1prime-native-stdio.json")

    def sessions(self, harness):
        """Process groups in log order; each opens with its `start` record."""
        groups, cur = [], None
        for rec in self.ev["harnesses"][harness]["events"]:
            if rec["kind"] == "start":
                cur = [rec]
                groups.append(cur)
            elif cur is not None:
                cur.append(rec)
        return groups

    def methods(self, sess):
        return [r["method"] for r in sess if r["kind"] == "request"]

    def respawn_pairs(self, harness):
        """(dead, respawned) session pairs: an exit_after_calls session
        followed within 1 s by a fresh start under the same ppid."""
        pairs = []
        sess = self.sessions(harness)
        for prev, nxt in zip(sess, sess[1:]):
            exits = [r for r in prev if r["kind"] == "exit_after_calls"]
            if (exits and prev[0]["ppid"] == nxt[0]["ppid"]
                    and nxt[0]["t"] - exits[-1]["t"] < 1.0):
                pairs.append((prev, nxt))
        return pairs

    def call_session(self, harness):
        """The normal (die_after=0) session that served tools/calls."""
        for sess in self.sessions(harness):
            if (not sess[0]["die_after"]
                    and sum(1 for r in sess if r["kind"] == "tools_call") > 1):
                return sess
        raise AssertionError(f"no recorded {harness} session with calls")

    def assert_env_inherited(self, harness):
        for sess in self.sessions(harness):
            vals = sess[0]["env_values"]
            self.assertEqual(vals["HERDR_PANE_ID"], "w6:pKQ")
            self.assertEqual(vals["HERDR_WORKSPACE_ID"], "w6")
            self.assertEqual(vals["HERDR_TAB_ID"], "w6:tCR")
            self.assertEqual(vals["HERDR_ENV"], "1")
            self.assertTrue(vals["PWD"].startswith("/home/user/"))
            for rec in sess:
                if "env_names" in rec:
                    self.assertTrue(self.HERDR_PWD <= set(rec["env_names"]))
                    self.assertGreater(rec["env_names_dropped"], 0)

    def assert_spawn_at_session_start(self, harness):
        """start opens the session, initialize follows within a second,
        and the process stays resident seconds before the first call —
        one pid for every record in the session."""
        sess = self.call_session(harness)
        self.assertEqual(sess[0]["kind"], "start")
        init = next(r for r in sess if r.get("method") == "initialize")
        self.assertLess(init["t"] - sess[0]["t"], 1.0)
        first_call = next(r for r in sess
                          if r.get("method") == "tools/call")
        self.assertGreater(first_call["t"] - sess[0]["t"], 1.0)
        self.assertTrue(all(r["pid"] == sess[0]["pid"] for r in sess))
        self.assertGreater(
            self.ev["harnesses"][harness]
            ["observations"]["spawn_after_launch_s_approx"], 0)

    def assert_respawn_after_crash(self, harness, relisted):
        pairs = self.respawn_pairs(harness)
        self.assertEqual(len(pairs), 1)
        dead, new = pairs[0]
        self.assertNotEqual(dead[0]["pid"], new[0]["pid"])
        self.assertEqual(dead[0]["ppid"], new[0]["ppid"])
        methods = self.methods(new)
        self.assertEqual(methods[:2],
                         ["initialize", "notifications/initialized"])
        self.assertIn("tools/call", methods)
        self.assertEqual(
            self.ev["harnesses"][harness]
            ["observations"]["respawn_tools_list_rerun"], relisted)
        self.assertEqual("tools/list" in methods, relisted)

    def test_a1prime_pi_env_inherited(self):
        """(a) HERDR_* + PWD arrive verbatim; pi's own markers ride along."""
        self.assert_env_inherited("pi")
        names = set()
        for sess in self.sessions("pi"):
            names.update(sess[0]["env_names"])
        self.assertTrue(
            set(self.ev["harnesses"]["pi"]["observations"]["env_markers"])
            <= names)

    def test_a1prime_pi_spawn_at_session_start(self):
        """(c) spawn at session start, one process per session — the two
        recorded calls share the pid."""
        self.assert_spawn_at_session_start("pi")
        self.assertEqual(
            self.methods(self.call_session("pi")),
            ["initialize", "notifications/initialized", "tools/list",
             "tools/call", "tools/call"])

    def test_a1prime_pi_shutdown_stdin_eof(self):
        """(c) clean sessions end on stdin EOF; no signals were sent."""
        ev = self.ev["harnesses"]["pi"]["events"]
        self.assertFalse(any(r["kind"] == "signal" for r in ev))
        for sess in self.sessions("pi"):
            if not any(r["kind"] == "exit_after_calls" for r in sess):
                self.assertEqual(sess[-1]["kind"], "stdin_eof")
        self.assertEqual(self.ev["harnesses"]["pi"]
                         ["observations"]["shutdown"], "stdin_eof")

    def test_a1prime_pi_respawn_after_crash(self):
        """(e) next call after a crash runs on a new pid ~40 ms later with
        a full re-init — pi re-runs tools/list on the respawn."""
        self.assert_respawn_after_crash("pi", relisted=True)

    def test_a1prime_pi_cwd_config_key_honored(self):
        """(b) cwd defaults to the invocation cwd; the config `cwd` key is
        honored — spawn cwd diverged from PWD exactly once."""
        keyed = [s for s in self.sessions("pi")
                 if s[0]["cwd"] != s[0]["env_values"]["PWD"]]
        self.assertEqual(len(keyed), 1)
        self.assertEqual(keyed[0][0]["cwd"], "/home/user/probe")
        self.assertEqual(keyed[0][0]["env_values"]["PWD"],
                         "/home/user/probe/runs/pi")
        for sess in self.sessions("pi"):
            if sess not in keyed:
                self.assertEqual(sess[0]["cwd"],
                                 sess[0]["env_values"]["PWD"])
        self.assertEqual(self.ev["harnesses"]["pi"]
                         ["observations"]["cwd_config_key"], "honored")

    def test_a1prime_pi_codemode_surface_and_tools_flag(self):
        """(d) under codemode exposure the model reaches the tool as
        mcp__<server>__<tool> inside codemode and `--tools` cannot scope
        to it — but with direct exposure (toolExposure) `--tools` does
        allowlist mcp__<server>__<tool> (proven 2026-09-29 for
        mcp__executor__execute). The server still spawned and served
        tools/list under `--tools` restriction."""
        obs = self.ev["harnesses"]["pi"]["observations"]
        self.assertIn("codemode", obs["tool_surface"])
        self.assertEqual(obs["model_tool_name"], "mcp__govprobe__probe_env")
        scoping = obs["tools_flag_scoping"]
        self.assertIn("cannot scope", scoping["verdict"])
        self.assertIn("tool not available",
                      scoping["--tools mcp__govprobe__probe_env"])
        self.assertIn("ALL_TOOLS=0", scoping["--tools codemode"])
        self.assertIn("mcp__executor__execute",
                      scoping["direct_exposure"])
        self.assertTrue(obs["spawn_under_tools_restriction"])
        restricted = [s for s in self.sessions("pi")
                      if self.methods(s) == ["initialize",
                                             "notifications/initialized",
                                             "tools/list"]]
        self.assertTrue(restricted)

    def test_a1prime_claude_env_inherited(self):
        """(a) HERDR_* + PWD arrive verbatim; claude's markers ride along."""
        self.assert_env_inherited("claude")
        names = set()
        for sess in self.sessions("claude"):
            names.update(sess[0]["env_names"])
        self.assertTrue(
            {"CLAUDECODE", "CLAUDE_CODE_SESSION_ID"} <= names)

    def test_a1prime_claude_spawn_at_session_start(self):
        """(c) spawn at session start, one process per session."""
        self.assert_spawn_at_session_start("claude")
        self.assertEqual(
            self.methods(self.call_session("claude")),
            ["initialize", "notifications/initialized", "tools/list",
             "tools/call", "tools/call"])

    def test_a1prime_claude_shutdown_sigint(self):
        """(c) claude shuts the relay down with SIGINT (signum 2), never
        stdin EOF — a relay must exit on either."""
        ev = self.ev["harnesses"]["claude"]["events"]
        self.assertFalse(any(r["kind"] == "stdin_eof" for r in ev))
        for sess in self.sessions("claude"):
            if not any(r["kind"] == "exit_after_calls" for r in sess):
                self.assertEqual(sess[-1]["kind"], "signal")
                self.assertEqual(sess[-1]["signum"], 2)
        self.assertEqual(self.ev["harnesses"]["claude"]
                         ["observations"]["shutdown"], "sigint")

    def test_a1prime_claude_respawn_after_crash(self):
        """(e) respawn ~100 ms later on a new pid; the re-init skips
        tools/list (tools cached)."""
        self.assert_respawn_after_crash("claude", relisted=False)

    def test_a1prime_claude_cwd_is_invocation(self):
        """(b) cwd always equals the invocation cwd (PWD); the config
        `cwd` key is ignored."""
        for sess in self.sessions("claude"):
            self.assertEqual(sess[0]["cwd"],
                             sess[0]["env_values"]["PWD"])
        self.assertEqual(
            {s[0]["cwd"] for s in self.sessions("claude")},
            {"/home/user/probe", "/home/user/probe/runs/claude"})
        self.assertEqual(self.ev["harnesses"]["claude"]
                         ["observations"]["cwd_config_key"], "ignored")

    def test_a1prime_claude_first_class_allowlist(self):
        """(d) mcp__<server>__<tool> is a first-class tool and
        --allowedTools scopes exactly to it."""
        obs = self.ev["harnesses"]["claude"]["observations"]
        self.assertEqual(obs["tool_surface"], "first_class")
        self.assertEqual(obs["model_tool_name"], "mcp__govprobe__probe_env")
        self.assertIn("--allowedTools", obs["allowlist"])
        self.assertIn("mcp__govprobe__probe_env", obs["allowlist"])

    def test_a1prime_devin_env_inherited(self):
        """(a) HERDR_* + PWD arrive verbatim; devin injects no markers."""
        self.assert_env_inherited("devin")
        self.assertEqual(
            self.ev["harnesses"]["devin"]["observations"]["env_markers"],
            [])
        for sess in self.sessions("devin"):
            for n in sess[0]["env_names"]:
                self.assertTrue(n.startswith("HERDR_") or n == "PWD")

    def test_a1prime_devin_spawn_at_session_start(self):
        """(c) spawn at session start, one process per session."""
        self.assert_spawn_at_session_start("devin")
        self.assertEqual(
            self.methods(self.call_session("devin")),
            ["initialize", "notifications/initialized", "tools/list",
             "tools/call", "tools/call"])

    def test_a1prime_devin_tools_list_lazy(self):
        """(c) initialize runs at spawn but tools/list is lazy — ~6 s
        later, at first need."""
        sess = self.call_session("devin")
        init = next(r for r in sess if r.get("method") == "initialize")
        listed = next(r for r in sess if r.get("method") == "tools/list")
        self.assertGreater(listed["t"] - init["t"], 1.0)
        self.assertTrue(self.ev["harnesses"]["devin"]
                        ["observations"]["tools_list_lazy"])

    def test_a1prime_devin_shutdown_stdin_eof(self):
        """(c) clean sessions end on stdin EOF; no signals were sent."""
        ev = self.ev["harnesses"]["devin"]["events"]
        self.assertFalse(any(r["kind"] == "signal" for r in ev))
        for sess in self.sessions("devin"):
            if not any(r["kind"] == "exit_after_calls" for r in sess):
                self.assertEqual(sess[-1]["kind"], "stdin_eof")
        self.assertEqual(self.ev["harnesses"]["devin"]
                         ["observations"]["shutdown"], "stdin_eof")

    def test_a1prime_devin_respawn_after_crash(self):
        """(e) respawn ~107 ms later on a new pid; the re-init skips
        tools/list."""
        self.assert_respawn_after_crash("devin", relisted=False)

    def test_a1prime_devin_cwd_is_invocation(self):
        """(b) cwd always equals the invocation cwd — the config schema
        has no `cwd` key, and the project config discovered upward does
        not anchor the spawn cwd."""
        for sess in self.sessions("devin"):
            self.assertEqual(sess[0]["cwd"],
                             sess[0]["env_values"]["PWD"])
        self.assertEqual(
            {s[0]["cwd"] for s in self.sessions("devin")},
            {"/home/user/probe", "/home/user/probe/runs/devin"})
        self.assertEqual(self.ev["harnesses"]["devin"]
                         ["observations"]["cwd_config_key"], "ignored")

    def test_a1prime_devin_first_class_allowlist(self):
        """(d) mcp__<server>__<tool> is first-class; project
        permissions.allow patterns auto-approved it in print mode."""
        obs = self.ev["harnesses"]["devin"]["observations"]
        self.assertEqual(obs["tool_surface"], "first_class")
        self.assertEqual(obs["model_tool_name"], "mcp__govprobe__probe_env")
        for pat in ("mcp__<server>__<tool>", "mcp__<server>__*", "mcp__*"):
            self.assertIn(pat, obs["allowlist"])
        self.assertIn("--respect-workspace-trust false",
                      obs["print_mode_trust"])

    def test_a1prime_forwarder_footprint(self):
        """The Rust stdio->unix-socket forwarder is negligible against a
        daemon: 2000 kB RSS after 100 round-trips, ~463 KiB zero-dep
        binary, exit 0 on stdin EOF."""
        f = self.ev["forwarder"]
        self.assertEqual(f["vm_rss_kb"], 2000)
        self.assertEqual(f["vm_hwm_kb"], 2000)
        self.assertEqual(f["binary_bytes"], 474144)
        self.assertEqual(f["deps"], 0)
        self.assertEqual(f["exit_code_on_stdin_eof"], 0)

    def test_a1prime_user_configs_untouched(self):
        """No user-scope harness config changed across the probe; the one
        session-state churn carried zero probe references."""
        ci = self.ev["config_integrity"]
        self.assertEqual(len(ci["unchanged_user_configs"]), 4)
        self.assertTrue(
            ci["session_state_churn_without_probe_config"])
        self.assertTrue(ci["probe_configs_removed_after_measurement"])

    def test_a1prime_fixture_scrubbed(self):
        """Committed evidence carries no host paths and no env names
        beyond HERDR_*/PWD plus the recorded harness markers — env values
        only ever the five whitelisted names."""
        blob = fixture_bytes("a1prime-native-stdio.json").decode()
        for bad in ("/home/gabriel", "worktrees/", "/tmp/", "NPM_TOKEN",
                    "TYPESAFE_API_KEY", "_TOKEN", "SECRET", "PASSWORD"):
            self.assertNotIn(bad, blob)
        for harness, hv in self.ev["harnesses"].items():
            allowed = (set(hv["observations"]["env_markers"])
                       | self.HERDR_PWD)
            for rec in hv["events"]:
                for n in rec.get("env_names", []):
                    self.assertTrue(n.startswith("HERDR_") or n in allowed,
                                    f"{harness}: {n}")
                self.assertTrue(
                    set(rec.get("env_values", {})) <= self.HERDR_PWD)


if __name__ == "__main__":
    unittest.main(verbosity=2)

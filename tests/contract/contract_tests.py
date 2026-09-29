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
    """contract-a1.md — gateway/Executor Streamable HTTP transport trial
    against a purpose-built probe server. Wire frames come from the probe
    server's own log (two runs, split at its `listening` records);
    gateway-side strings come from the recorded observation fixture. The
    full OAuth round-trip against a real authorization server is the one
    remaining sub-case and has no test by design."""

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

    def test_a1_auth_is_oauth_metadata_discovery(self):
        on401 = self.obs["auth"]["on_401"]
        self.assertEqual(on401["www_authenticate"], "Bearer")
        self.assertIn("/.well-known/oauth-authorization-server",
                      on401["triggers"])
        self.assertEqual(on401["mcp_spec"], "2026-07-28")
        self.assertTrue(on401["observed_failure_prefix"]
                        .startswith("HTTP 501 trying to load OAuth metadata"))

    def test_a1_stale_loopback_blocks_reregistration(self):
        auth = self.obs["auth"]
        self.assertFalse(auth["uninstall_verb_present"])
        self.assertEqual(auth["loopback_name_derivation"],
                         "every loopback variant derives the source name "
                         "local-mcp")


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


if __name__ == "__main__":
    unittest.main(verbosity=2)

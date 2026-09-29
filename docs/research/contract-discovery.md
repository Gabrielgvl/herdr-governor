# Phase 2 contract discovery — assumption evidence register

Spec §10 Phase 2: "confirm or escalate A1–A6 against the real systems,
before the design depends on them." This register is the synthesis of the
four wave-1 evidence reports into per-assumption verdicts.

**Status vocabulary.** Each behavior below is marked:

- **confirmed** — observed on the real system and pinned by an executable
  check in this tree. The check ID is the `test_<id>` method in
  `tests/contract/contract_tests.py`, run by `just contract` against the
  committed fixtures under `tests/fixtures/contract/`. Offline only: no
  test contacts a live system.
- **confirmed-negative** — the evidence shows the capability does not
  exist or is refused. Documented here; no test encodes it.
- **needs-owner-decision** — evidence exists but does not resolve which
  contract the governor should adopt. Documented; no test.
- **untested** — not probed in wave 1. Nothing here is inferred.

**Sources** (evidence reports under the p0p1 artifact bundle;
`contract-a2-probe.mjs` is the probe source):
`contract-a4-a6.md`, `contract-a5.md`, `contract-a2.md`,
`contract-a2-probe.mjs`, `contract-a1.md`, `p1-clone-verify.md`. Proposed check IDs in
those reports (`CT-*`, `A5-*`) are labels for the recorded observations;
the test IDs in this document are the checks that actually exist in this
tree.

Evidence hygiene: every committed fixture is a mechanical extraction from
the raw capture, scrubbed of host paths, usernames, and lane names
(`/home/user/` and `/tmp/` placeholders). Transcript prose, tool
arguments, and credential material are excluded by construction.

---

## A1 — Executor transport

**Status: confirmed for the trial sub-behaviors below; two open sub-cases
stay owner-decision material (full OAuth round-trip; typed offline error).**

The trial registered a purpose-built minimal MCP Streamable HTTP server
(loopback) through the gateway's install action and drove it over the real
transport. The wire record is `tests/fixtures/contract/a1-transport-trace.jsonl`
(the probe server's own frame log, two server runs split at their
`listening` records); gateway-side strings are recorded in
`a1-gateway-observations.json`.

Confirmed behaviors and their tests:

| Behavior | Test ID |
|---|---|
| Install of a Streamable HTTP source connects and lists tools; `protocolVersion 2025-06-18` accepted | `a1_register_streamable_http_source` |
| Handshake: `initialize` → `notifications/initialized` → `tools/list` → `tools/call` | `a1_handshake_sequence` |
| Caller arguments forwarded byte-identical through `tools/call` | `a1_caller_arguments_forwarded_verbatim` |
| An SSE GET (`405`) during validation does not break the session; requests continue after it | `a1_sse_stream_optional` |
| Stateless mode: no `mcp-session-id` header on any request; calls are self-contained | `a1_stateless_session` |
| Reconnect after a server restart is transparent — new process, first call a self-contained `tools/call`, no reinstall/revalidation | `a1_reconnect_transparent_after_restart` |
| Offline failure surfaces as an **untyped** `Failed to call tool: fetch failed` — no error class separates transport-down from a tool error (FLAG) | `a1_offline_is_untyped_fetch_failure` |
| No per-caller/per-call process recreation: one persistent server process per run, all traffic from the gateway's in-process fetch client (`undici`), zero child spawns (the feared behavior does not occur) | `a1_no_per_caller_process_recreation` |
| Non-loopback registration must be HTTPS — a plain-LAN-HTTP source is refused at install | `a1_non_loopback_requires_https` |
| A 401 source with `WWW-Authenticate: Bearer` triggers OAuth metadata discovery at `/.well-known/oauth-authorization-server` (RFC 8414, MCP spec 2026-07-28) — auth is OAuth-based, not caller-supplied static tokens | `a1_auth_is_oauth_metadata_discovery` |
| The gateway has **no uninstall verb** and every loopback variant derives the same source name, so a dead loopback source permanently blocks re-registration in this gateway version (FLAG) | `a1_stale_loopback_blocks_reregistration` |

Flags and open sub-cases: the offline error is untyped (callers cannot
distinguish transport-down from tool failure — the spec should require a
typed transport-unavailable error from callers; Phase 3 contract work);
authentication is exercised up to OAuth metadata discovery only — a full
OAuth round-trip needs a real authorization server and stays owner-decision
material (§19); the one-way registration surface (no uninstall verb) is
recorded as-is. An unrelated isolation note: the trial's first port choice
was already held by another flow's server; the probe moved ports and the
gateway behavior was unaffected. The spec §9 `relay` fallback question is
unaffected by this trial and stays open.

Source: `contract-a1.md` (raw probe log `/tmp/mcp-probe-server.log`);
fixtures `a1-transport-trace.jsonl`, `a1-gateway-observations.json`.

## A2 — Socket concurrency

**Status: confirmed for the isolated herdr-tools daemon transport and for
the Herdr protocol-22 subscription leg (isolated named session); three
items need-owner-decision.**

The probe (`contract-a2-probe.mjs`, protocol fixture revision 22) stood up
a private herdr-tools daemon and drove its Unix socket directly. The
wire trace is committed as `tests/fixtures/contract/a2-tools-daemon-trace.jsonl`
(91 records).

Confirmed behaviors and their tests:

| Behavior | Test ID |
|---|---|
| Requests before hello are refused and the connection closes (`DAEMON_PROTOCOL_ERROR`) | `a2_hello_required` |
| Unsupported hello version is refused (`PROTOCOL_MISMATCH`), connection closes | `a2_version_refusal` |
| Concurrent requests multiplex on one connection; a later request completes while an earlier is held | `a2_same_connection_multiplex` |
| Held work is per-connection; a second connection is unaffected | `a2_cross_connection_isolation` |
| A request-level error reply is non-fatal; the connection stays usable | `a2_request_error_nonfatal` |
| Disconnect does not cancel held server work; it completes after a reconnect | `a2_disconnect_does_not_cancel` |
| Malformed JSON frame → protocol error + close | `a2_malformed_json_close` |
| Malformed `params` → protocol error + close | `a2_malformed_params_close` |
| Frame over `maxLineBytes` (262144) → protocol error + close | `a2_oversize_close` |
| A poisoned peer does not stall healthy connections | `a2_peer_failure_containment` |
| Client-side timeout rejects the request (`DAEMON_UNAVAILABLE`) without closing the client | `a2_client_timeout_nonfatal` |
| A late reply for a timed-out request is ignored; subsequent requests resolve normally | `a2_late_reply_ignored` |
| Closing the client rejects pending requests (`DAEMON_UNAVAILABLE`, "closed by the client") | `a2_client_close_rejects_pending` |
| Recorded probe verdict is PASS (fixture completeness guard) | `a2_recorded_probe_passed` |

Confirmed-negative (tools daemon): `events.subscribe` was rejected with
`DAEMON_UNKNOWN_METHOD` after the daemon hello/ack. This characterizes
the **tools daemon only**.

Source: `contract-a2.md`, `contract-a2-probe.mjs`;
fixture `a2-tools-daemon-trace.jsonl`.

### A2 subscription leg — Herdr protocol-22 socket (isolated named session)

The leg the tools-daemon probe could not cover was run against the real
Herdr server socket protocol (the one the pinned fixture describes,
protocol `22`) on the already-running isolated named session
`herdr-governor-contract`, raw NDJSON only — no live-socket connection,
no live-daemon signal, no restart. Probe source:
`contract-a2-herdr-probe.mjs` (artifact bundle); raw evidence
`contract-a2-subscription-evidence/` (304-record JSONL — contains owner
prompt lines inside pane text, kept out of the tree; the committed
fixture is the scrubbed distillation).

Isolation proof (gathered before any session-socket traffic): all `67`
`connect()` calls in the strace targeted the session socket, zero the
live socket; `gov-a2:`-prefixed request ids appear 0 times in the live
server log and 121 times in the session log; shells spawned by the
session server carry the session endpoint in their environment; the live
socket's stat is unchanged before/after; session snapshot is
`workspaces:[]` before and after. Two caveats stated rather than
absorbed: the session directory is Herdr's per-session private
directory, not a `/tmp` root (config shared read-only), and the session
server's own environment carries the live endpoint while its listener
and spawned shells use the session socket.

Confirmed behaviors and their tests:

| Behavior | Test ID |
|---|---|
| Isolation: every connect targets the session socket; live log untouched; live socket unchanged; clean session teardown | `a2sub_isolation_socket_scoped` |
| Framing: exactly one unary request per connection, closed ~100 ms after the reply; a pipelined second frame is dropped; only `events.subscribe` holds the connection | `a2sub_one_request_per_connection` |
| Subscription arm is acknowledged (`subscription_started`) and events carry the `event`+`data` envelope | `a2sub_subscription_ack` |
| Pane filter: an identical marker in the unsubscribed pane delivers nothing | `a2sub_subscription_pane_filter` |
| `pane.output_matched` fires at most once per subscription (state-triggered on arm, then dead); the connection stays open after the fire; arming while the marker is on screen fires immediately | `a2sub_output_match_one_shot` |
| Empty subscription list is accepted and the connection is held | `a2sub_subscribe_empty_ok` |
| Confirmed-negative: a per-subscription failure is reported with a **derived id** `<request id>:sub:<index>:probe`, not the request id, and closes the connection; a live-workspace pane id yields `pane_not_found` (the session cannot see live panes) | `a2sub_subscribe_error_id_derived` |
| Concurrency model: unary requests on separate connections proceed while a subscription is armed (4 unary + 1 event in 201 ms); the same request id on two connections is answered independently | `a2sub_concurrent_connections_independent` |
| Confirmed-negative: there is **no same-connection multiplexing** — a second frame on a subscription connection resets it silently; a pipelined second unary is dropped | `a2sub_no_multiplex` |
| Disconnect mid-stream: events are neither replayed nor buffered; re-subscription fires immediately from current `recent` text (state catch-up) and is silently lost if the text was cleared; no cursor/revision resume token (`read.revision` is constant 0) | `a2sub_reconnect_no_replay_state_catchup` |
| Every malformed frame (invalid JSON, empty line, non-object, missing id/method, unknown method, array params, subscription missing `pane_id`, 2 MiB line) closes the connection; on a fresh connection the correlated-less error carries `id:""` | `a2sub_malformed_closes_and_empty_error_id` |
| A line bound exists: 2 MiB is rejected server-side (`api request line is too large`); the exact bound was not measured | `a2sub_line_bound_oversize_rejected` |
| Peer failure containment: a poisoned connection never stalls the server or other connections (server healthy after abandoned waits and malformed peers) | `a2sub_peer_failure_containment` |
| A frame split across two writes 200 ms apart parses fine | `a2sub_partial_frame_reassembled` |
| `events.wait` / `pane.wait_for_output` honour server-side `timeout_ms` with a correlated `timeout` error, then close; a concurrent ping on another connection is answered during the wait | `a2sub_wait_server_timeout` |
| Without `timeout_ms` a wait does not reply (3 s observation); the client close is silent and the server stays healthy | `a2sub_wait_no_timeout_blocks` |
| An armed subscription idle 8 s still delivers (longer idle bounds untested) | `a2sub_subscription_survives_idle_8s` |
| Confirmed-negative: `events.wait` rejects every `EventMatch` except pane agent-status matches (`unsupported_event_wait_match`) although the fixture union lists 25+ kinds | `a2sub_events_wait_agent_status_only` |

Needs-owner-decision (documented, no test): **`A2-HERDR-SCROLL-CHANGED-UNVERIFIED`**
(a `pane.scroll_changed` subscription produced no event in 1.5 s — request
no-op vs non-delivery not distinguished; `pane.agent_status_changed` was
subscribed but never triggered, since starting an agent was outside the
read-only probe); **`A2-HERDR-MALFORMED-ON-STREAM-SILENT`** (a malformed
frame on a subscription connection tears the stream down with no error
frame — a governor must treat unexpected EOF on a subscription as
"re-arm and catch up", never as "server gone"); **`A2-HERDR-SCHEMA-DRIFT-GRAPHICS-STREAM`**
(server 0.9.1 enumerates 104 methods vs the fixture's 103, server-only
`pane.graphics.stream`, same protocol number — `just schema-live` is the
sanctioned check and was not run).

Implication for the spec's A2 assumption: the Herdr socket offers **no
durable, multiplexed event stream**. A governor needing "subscription
plus concurrent requests" holds one connection per armed one-shot
subscription and one per in-flight unary request, and re-arms after every
EOF with a state read (`pane.read`/`session.snapshot`) as the catch-up.

Source: `contract-a2.md` § subscription leg,
`contract-a2-herdr-probe.mjs`, `contract-a2-subscription-evidence/`;
fixture `a2-subscription-evidence.json`.

The two legs together close the A2 subscription question at evidence
level; the three need-owner-decision items above go to the §19 batch.

## A3 — Start and prompt semantics

**Status: untested.** No wave-1 report probed agent start, readiness, or
prompt delivery. One incidental observation exists in the A4/A6 capture:
an agent's `name`/readiness can appear on the agent-list surface before
its `agent_session` field does (the `first-native*` captures). That is a
single observation, not a start-contract check, and nothing in this tree
depends on it.

Source: none (incidental note from `contract-a4-a6.md`).

## A4 — Incarnation and child identity

**Status: incarnation proof confirmed-negative on protocol 22; child
identity and move/replacement behaviors confirmed.**

Confirmed-negative (documented, no test): protocol 22 `SessionSnapshot`,
`PaneInfo`, and the schema expose **no server instance ID, boot marker,
or equivalent incarnation proof** (report check `CT-A4-NO-INCARNATION`).
The committed `tests/fixtures/contract/protocol22-subset.json` is the
schema-side record. The spec's fallback stands: invalidate bare terminal
IDs after any discontinuity; re-prove identity through a unique native
session; otherwise identity is unprovable.

Confirmed behaviors and their tests:

| Behavior | Test ID |
|---|---|
| Snapshot, `pane get`, and `agent list` agree on `pane_id`/`tab_id`/`terminal_id`/`agent_session`/`tokens`; agent `name` exists only on the agent list, `label` only on the pane surfaces | `a46_surfaces_agree` |
| Tab move preserves every identity field while `tab_id` changes | `a4_tab_move_preserves_identity` |
| Cross-tab swap is an explicit no-op (`changed:false`, `reason:"cross_tab"`) | `a4_cross_tab_swap_noop` |
| Workspace move assigns a new `pane_id`/`workspace_id` while terminal, native session, name, label, and tokens survive | `a4_workspace_move_new_locator` |
| After a workspace move the stale locator diverges: `pane get` follows to the new pane, `agent get` errors `agent_not_found` | `a4_old_locator_apis_diverge` |
| Same-tab swap changes layout geometry only; identity unchanged | `a4_same_tab_swap_geometry_only` |
| `agent new` on a live pane replaces the native session tuple; terminal/pane/name/label/tokens persist | `a4_native_new_replaces_session` |
| Graceful native exit drops agent fields; a replacement native gets a new session on the same identity tuple | `a4_native_replace_new_session` |
| A closed pane disappears from all surfaces; a recreated pane carries new `pane_id`+`terminal_id` | `a4_pane_replacement_fields` |
| Restart history shows **one** restart window: the 19:12 `-03:00` record and the 22:12 `Z` records are the same instant | `a4_history_single_restart_window` |
| `job_terminal` mailbox records are terminal bookkeeping, not child death — runs later show `handed_off`/`daemon_restart_reattached` | `a4_history_job_terminal_not_death` |

Source: `contract-a4-a6.md`; fixtures `a46-identity-evidence.json`
(27 captures), `a46-restart-history.json`, `protocol22-subset.json`.

## A5 — Transcripts

**Status: confirmed per-harness resolution and failure contracts; AGY
unqualified (terminal-only).**

27 probe cases ran against the reference readers on synthetic inputs;
outcomes are committed as `a5-probe-outcomes.json`, native structural
observations as `a5-native-observations.json`, and the synthetic inputs
themselves under `a5-samples/`.

Confirmed behaviors and their tests:

| Behavior | Test ID |
|---|---|
| A Run binding pins a native-session tuple; the Run marker is not a transcript locator | `a5_run_binding_pinned_session` |
| Pi native location is `~/.pi/agent/sessions/--<cwd>--/<ts>_<id>.jsonl`, LF-terminated; Devin is `~/.local/share/devin/cli/transcripts/<id>.json` (`ATIF-v1.7`, unterminated tail); Claude is `~/.claude/projects/<slug>/<uuid>.jsonl` with per-record `sessionId` | `a5_native_default_locations` |
| Pi: exact-path selection wins over an id-only glob when duplicate id files exist | `a5_pi_exact_path_no_glob` |
| Devin: duplicate roots resolve by exact root | `a5_devin_duplicate_root_exact` |
| Devin: XDG transcript root resolves | `a5_devin_xdg_root_resolution` |
| Claude: two slugs with the same session file are ambiguous — no general resolver; the pinned slug yields zero progress | `a5_claude_ambiguous_candidates` |
| Unterminated tail (partial UTF-8) is retained without emission; appending the rest emits the record | `a5_partial_write_rejected` |
| Complete JSON without a trailing LF emits nothing — LF, not JSON validity, is the boundary | `a5_complete_json_without_newline_pending` |
| A malformed complete record produces typed `source_malformed` at a byte offset and fails the whole window | `a5_malformed_record_fails_window` |
| Pi: the reference reader does not validate header identity — the governor adapter must (proposed `A5-PI-HEADER`) | `a5_header_identity_not_validated_by_reference` |
| Devin: partial document → `source_malformed`/`invalid_json`, retryable after completion | `a5_devin_partial_document_retryable` |
| Devin: content `session_id` mismatch rejected; traversal/unsafe ids rejected | `a5_devin_identity_guards` |
| Claude: partial and corrupt-tail quota reads still report zero progress — corrupt input must defeat a no-progress proof (`A5-CORRUPT-NOT-ABSENCE`) | `a5_claude_partial_corrupt_quota` |
| Permission denied → typed `source_unreadable`/`EACCES` | `a5_permission_denied_causes` |
| Vanish-before-open → `source_unreadable`/`ENOENT` | `a5_vanish_before_open_unreadable` |
| Vanish-after-open still reads the pinned inode — bytes prove the opened snapshot, not path liveness | `a5_vanish_after_open_stale_inode` |
| Cursor-region rewrite → `source_rewritten` (Devin: `anchor_mismatch`) | `a5_cursor_rewrite_detected` |
| Oversized record skipped with `record_exceeds_budget`; the next record is still read (Pi scanned 17,825,973 actual bytes past the 16 MiB ceiling — the gap is exposed, not silent) | `a5_oversized_record_gap_exposed` |
| Devin source ceiling 8,388,608 bytes → `source_exceeds_budget` | `a5_devin_source_ceiling` |
| Symlink policies differ per harness: Pi and Devin follow, Claude refuses | `a5_symlink_policies_differ` |
| Structured-source failure does **not** auto-fall-back to the terminal in the reference implementation; unchanged terminal output proves nothing; a terminal read error surfaces `EIO`. The governor contract (typed failure + bounded terminal fallback) is the spec's requirement, layered on these typed outcomes | `a5_structured_failure_no_terminal_fallback` |

Confirmed-negative / unqualified: **AGY** — binary present and catalog
key present, but no `~/.agy` directory and no transcript adapter; AGY
sources resolve to terminal-fallback and its native transcript is
unqualified (test `a5_agy_unqualified_terminal_only`). Claude likewise
has no structured transcript reader — transcript resolution falls back
to the terminal; only its quota-reader was probed, and its zero-progress
result cannot serve as a progress proof (see the corrupt-tail row above).

Source: `contract-a5.md`; fixtures `a5-probe-outcomes.json`,
`a5-native-observations.json`, `a5-samples/` (18 sample files).

## A6 — Pane tagging and crash recognition

**Status: crash-recognition observations confirmed; the creation-time
atomic-tagging mechanism needs an owner decision.**

Confirmed behaviors and their tests:

| Behavior | Test ID |
|---|---|
| Killing the foreground native agent leaves a stale agent row for one read, then settles: pane, terminal, label, and tokens survive while `agent_session` disappears | `a6_kill_stale_read`, `a6_kill_pane_survives` |
| A shell kill removes the pane; a snapshot/get race exists (snapshot still lists it while `pane get` already errors `pane_not_found`), then it is absent everywhere | `a6_shell_kill_race`, `a6_shell_kill_absent` |
| `pane split --env GOV_RUN_ID=…` sets the process environment but surfaces nowhere in the required reads (no `env`, `label`, `tokens`, or `agent_session` field) | `a6_split_env_not_metadata` |
| `label` + `tokens` tags are not unique: two panes can carry identical tags on distinct terminals | `a6_tags_nonunique` |
| The advertised `agent_session.value` path is a reference, not a live file — it need not exist after the native dies | `a6_session_ref_not_file` |
| Schema surface: `PaneSplitParams` has no `label`/`tokens` field; the `LayoutNode` pane variant does (`label`, `cwd`, `env`, `command`, `pane_id`) | `a6_creation_tag_surface_absent_in_split` |

Needs-owner-decision (documented, no test): **the generic
creation-effect contract for atomic tagging/adoption.** The evidence
shows: (a) a labeled layout creation could be rediscovered after a lost
response, but the rebuild changed supplied IDs and topology; (b)
separate rename/report-metadata calls leave a crash window between
creation and tagging; (c) `--env` tags do not surface in reads. Which
mechanism the governor adopts — labeled layout apply vs post-hoc tagging
with a bounded window vs unique-token rediscovery — is an owner ruling,
not a confirmed behavior.

Source: `contract-a4-a6.md`; fixtures `a46-identity-evidence.json`,
`protocol22-subset.json`.

## Jev — untested

Not probed in wave 1 and deliberately without fixtures in Phase 2 (the
spec's structural Jev fixtures were scoped out). No claim is made about
Jev availability, latency, or judgment shape.

---

## Escalation register

| Item | Status | What unblocks it |
|---|---|---|
| A1 full OAuth round-trip | open sub-case — auth proven up to metadata discovery only | trial against a real authorization server (owner-decision material) |
| A1 offline error typing | confirmed untyped transport failure | governor/spec contract must require a typed transport-unavailable error (Phase 3 work) |
| A2 Herdr socket: scroll-changed delivery | needs-owner-decision | distinguished probe (scroll-inducing pane or agent-status transition in the session) |
| A2 Herdr socket: silent stream teardown on malformed input | needs-owner-decision | owner adopts the re-arm-and-catch-up contract for unexpected subscription EOF |
| A2 Herdr socket: schema drift (`pane.graphics.stream`) | needs-owner-decision | `just schema-live` against the session server, then re-pin the fixture |
| A3 start/prompt semantics | untested | wave-2 probe of agent start and prompt delivery |
| A4 incarnation proof | confirmed-negative | spec fallback already defined — no action needed |
| A5 AGY transcript source | unqualified | an AGY native-reader probe, or owner ruling that AGY stays terminal-only |
| A5 header validation / corrupt-vs-absence | confirmed reference-reader gaps | governor adapter must implement the stricter contract (Phase 3 work) |
| A6 creation-time atomic tagging | needs-owner-decision | owner picks the tagging/adoption mechanism |
| Jev | untested | wave-2 probe |

## Fixture inventory

| Fixture | Content | Source |
|---|---|---|
| `a1-transport-trace.jsonl` | 8-record probe-server frame log over two runs: handshake, headers, SSE GET, restart, self-contained re-call | `/tmp/mcp-probe-server.log` |
| `a1-gateway-observations.json` | recorded gateway-side strings: install result, echoed args, untyped offline error, OAuth discovery, HTTPS refusal, no-uninstall surface | `contract-a1.md` |
| `a2-tools-daemon-trace.jsonl` | 91-record wire log: hello/ack, multiplex, malformed frames, timeout, close | `/tmp/gov-p2-a2-evidence.jsonl` |
| `a2-subscription-evidence.json` | scrubbed distillation of the Herdr protocol-22 subscription leg: isolation counts, framing, subscription semantics, concurrency, reconnect, malformed, timeouts, schema drift | `contract-a2-subscription-evidence/evidence.jsonl` (artifact bundle) |
| `a46-identity-evidence.json` | 27 three-surface captures + 47 command receipts + kill/session records | `gov-p2-a46` lane `contract-a4-a6-raw.jsonl` |
| `a46-restart-history.json` | one restart window, mailbox record kinds, persisted run identities | `contract-a4-a6-historical-summary.json` |
| `a5-probe-outcomes.json` | 27 recorded case outcomes from the reference readers | `/tmp/gov-a5-*/results.json` |
| `a5-native-observations.json` | native location shape, identity fields, AGY/env presence | `/tmp/gov-a5-*/metadata.json` |
| `a5-samples/` | byte-faithful synthetic transcript inputs (Pi JSONL, Devin ATIF, Claude JSONL) | `/tmp/gov-a5-*/cases-*` |
| `protocol22-subset.json` | pinned schema subset: envelopes + objects the evidence exercised | `tests/fixtures/herdr-api-schema.json` |

`just contract` runs the suite — 84 checks, fail-closed on absent or
malformed fixtures.

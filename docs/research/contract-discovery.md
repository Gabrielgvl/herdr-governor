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
`contract-a4-a6.md`, `contract-a5.md`, `contract-a3.md`, `contract-a2.md`,
`contract-a2-probe.mjs`, `contract-jev.md`, `contract-a1.md`,
`contract-a1prime.md`, `p1-clone-verify.md`. Proposed check IDs in
those reports (`CT-*`, `A5-*`) are labels for the recorded observations;
the test IDs in this document are the checks that actually exist in this
tree.

Evidence hygiene: every committed fixture is a mechanical extraction from
the raw capture, scrubbed of host paths, usernames, and lane names
(`/home/user/` and `/tmp/` placeholders). Transcript prose, tool
arguments, and credential material are excluded by construction.

---

## A1 — Executor transport

**Status: gateway identity corrected.** Real callers reach integrations
through Executor's catalog daemon (integrations are catalog + connection
objects, surfaced as `tools.<slug>.<owner>.<conn>.*` / `executor call` —
not entries in a harness's MCP config). The earlier A1 trial — the first
table below and fixtures `a1-transport-trace.jsonl` /
`a1-gateway-observations.json` / class `A1ExecutorTransport` — actually
registered the probe into **pi's MCP adapter** (`pi-mcp-adapter`, retired
in pi 0.99): those results are re-labelled pi-adapter evidence, kept, and
do not describe the gateway real callers use. The Executor catalog path
and pi 0.99's native `pi mcp` client were probed on 2026-09-29 and are
pinned by the Executor-catalog-path and pi-native tables below. **A1
transport itself is owner-decided 2026-09-30 — native per-session stdio
relay (ADR-0004); the A1' spawn evidence is pinned by the last table
below.**

### pi-adapter evidence (the earlier A1 trial, re-labelled)

The trial registered a purpose-built minimal MCP Streamable HTTP server
(loopback) through the pi adapter's install action and drove it over that
adapter's transport. The wire record is `tests/fixtures/contract/a1-transport-trace.jsonl`
(the probe server's own frame log, two server runs split at their
`listening` records); adapter-side strings are recorded in
`a1-gateway-observations.json`.

Confirmed behaviors and their tests (pi adapter):

| Behavior | Test ID |
|---|---|
| Install of a Streamable HTTP source connects and lists tools; `protocolVersion 2025-06-18` accepted | `a1_register_streamable_http_source` |
| Handshake: `initialize` → `notifications/initialized` → `tools/list` → `tools/call` | `a1_handshake_sequence` |
| Caller arguments forwarded byte-identical through `tools/call` | `a1_caller_arguments_forwarded_verbatim` |
| An SSE GET (`405`) during validation does not break the session; requests continue after it | `a1_sse_stream_optional` |
| Stateless mode: no `mcp-session-id` header on any request; calls are self-contained | `a1_stateless_session` |
| Reconnect after a server restart is transparent — new process, first call a self-contained `tools/call`, no reinstall/revalidation | `a1_reconnect_transparent_after_restart` |
| Offline failure surfaces as an **untyped** `Failed to call tool: fetch failed` — no error class separates transport-down from a tool error (FLAG) | `a1_offline_is_untyped_fetch_failure` |
| No per-caller/per-call process recreation: one persistent server process per run, all traffic from the pi adapter's in-process fetch client (`undici`), zero child spawns (the feared behavior does not occur) | `a1_no_per_caller_process_recreation` |
| Non-loopback registration must be HTTPS — a plain-LAN-HTTP source is refused at install | `a1_non_loopback_requires_https` |
| A 401 source with `WWW-Authenticate: Bearer` names `/.well-known/oauth-authorization-server` (RFC 8414, MCP spec 2026-07-28) in its error — but the pi adapter's OAuth metadata loader is **unimplemented** (adapter-local `HTTP 501`; server-side request log proves no metadata, register, authorize, or token request is ever issued, even against a fully compliant authorization server): authenticated registration is **confirmed-negative on the pi adapter**, loopback + no-auth was the only supported source shape there | `a1_auth_round_trip_confirmed_negative` |
| The pi adapter has **no uninstall verb** and every loopback variant derives the same source name, so a dead loopback source permanently blocks re-registration in that adapter version (FLAG) | `a1_stale_loopback_blocks_reregistration` |

Flags and open sub-cases: the offline error is untyped (callers cannot
distinguish transport-down from tool failure — the spec should require a
typed transport-unavailable error from callers; Phase 3 contract work);
authentication on the pi adapter is **closed confirmed-negative**: the
wave-2 OAuth trial (compliant loopback AS: RFC 8414 metadata at 200,
dynamic client registration, PKCE authorize, token endpoints) reproduced
the identical adapter-local 501 with zero AS requests issued — the wave-1
reading that a real authorization server was the remaining sub-case is
corrected; the one-way registration surface (no uninstall verb) is made
harder by the write-once loopback name: a stale `local-mcp` blocks every
later loopback install at a different endpoint (exact refusal recorded;
removal required a config edit plus a fresh session — the in-session
registry does not re-read config). An unrelated isolation note: the
trial's first port choice was already held by another flow's server; the
probe moved ports and the adapter behavior was unaffected. These rows
constrain the pi adapter only; they do not constrain the Executor catalog
path measured below.

### Executor catalog path — measured 2026-09-29

Probed through the gateway real callers use: a Streamable HTTP server on
loopback requiring `Authorization: Bearer`, registered via
`mcp.addServer{transport:"remote", remoteTransport:"streamable-http",
authenticationTemplate:[{slug:"headers", type:"apiKey",
headers:{Authorization:["Bearer ", {type:"variable",name:"token"}]}}]}`
then `coreTools.connections.create` resolving `token` from a
file-provider credential item — arguments carry only variable/item names,
never a secret. Fixtures are mechanical request-log distillations (method/
path/header-name/auth/session fields only):
`a1-executor-bearer-requests.json`, `a1-executor-reconnect-requests.json`.

Confirmed behaviors and their tests:

| Behavior | Test ID |
|---|---|
| Registration is declarative — `mcp.addServer` logs zero requests; `connections.create` performs one health handshake (`server/discover` → `initialize` → `notifications/initialized` → `GET /mcp` → `tools/list`) and no `tools/call` | `a1_executor_registration_declarative` |
| `mcp.addServer` requires `name` in this daemon version (missing → `invalid_tool_arguments`) | `a1_executor_addserver_requires_name` |
| A static bearer via the apiKey `headers` template + `file` credential item `connection:org:<slug>:<conn>:token` is sent on **every** request (`Authorization` present, `bearer`, match); connection addresses camelize (`gov-a1-bearer-probe` → `govA1BearerProbe`) | `a1_executor_static_bearer_every_request`, `a1_executor_connection_address_camelized` |
| OAuth never engages on the authenticated path — zero `/.well-known` fetches, zero 401s | `a1_executor_authenticated_no_oauth_discovery` |
| An unauthenticated 401 still triggers OAuth/OIDC metadata discovery (`/.well-known/oauth-authorization-server/mcp`, `/.well-known/openid-configuration/mcp`, `/mcp/.well-known/openid-configuration`); metadata 501s → `{connected:false, requiresAuthentication:true, requiresOAuth:false}` | `a1_executor_401_triggers_oauth_discovery` |
| Reconnect after a server restart is transparent: the next call re-initializes on the new process — no manual action, and no stale session id is presented (a transport failure evicts the cached session) | `a1_executor_reconnect_after_restart`, `a1_executor_dead_session_evicted` |
| A stale `Mcp-Session-Id` answered 404 by a strict server is re-initialized within the same call (stale `tools/call` → 404 → discover/initialize → retry succeeds); the caller never sees the 404 | `a1_executor_stale_session_404_reinitialize` |
| Warm calls reuse the session: steady state is one `tools/call` per call carrying the cached `Mcp-Session-Id` | `a1_executor_session_reused_between_calls` |
| While the server is down the caller sees an **untyped** `Internal tool error [<correlation-id>]`, exit 1 — no typed envelope on the CLI path (FLAG) | `a1_executor_server_down_error_untyped` |
| Zero 401s / `.well-known` fetches across restarts — restart does not re-trigger OAuth discovery | `a1_executor_no_oauth_rediscovery` |

Notes (recorded, not separately tested): `server/discover` is a
non-standard Executor probe sent without a session id — a strict server
may 404 it and Executor proceeds regardless; `connections.remove` +
`integrations.remove` fully revert a registration (the earlier one-way
registration defect is fixed; registry snapshots byte-identical before/
after); the correlation id in the untyped error maps to the daemon log.
Contract rule recorded from a contained incident during the probe:
**never pass bearer material in `addServer` arguments** — the approval
gate echoes them; use `authenticationTemplate` + provider items, the
pattern real connections use.

### pi 0.99 native MCP client — measured 2026-09-29

With the adapter retired, pi's native `pi mcp` client was measured
against the same probe server (`pi mcp add`/`list`/`login` surface, not
`mcp.json` adapter entries).

| Behavior | Test ID |
|---|---|
| Configured static headers are sent on every request (`Authorization` present, `bearer`, match on `initialize`/`notifications/initialized`/`GET`/`tools/list`) | `a1_pi_native_static_headers_sent` |
| An unauthenticated 401 marks the server `needs sign-in` — exactly one unauthenticated request, **zero** `.well-known` fetches; OAuth is deferred to `pi mcp login` | `a1_pi_native_401_defers_oauth` |

**A1 transport: owner-decided 2026-09-30 — native per-session stdio
relay (ADR-0004); the A1' evidence is confirmed in the subsection
below.** The spec §9 `relay` fallback question is subsumed by that
decision. The untyped-offline-error flag holds on both measured paths
(pi adapter `fetch failed`, Executor `Internal tool error [<id>]`); the
Phase-3 requirement for a typed transport-unavailable error stands.

### A1' native stdio relay — measured 2026-09-29

Probed against the shape ADR-0004 decided: each harness spawning a
per-session stdio MCP server. The probe server spoke newline-delimited
JSON-RPC 2.0 with one `probe_env` tool (reporting pid/ppid/cwd, env
names, and the whitelisted `HERDR_*`/`PWD` values) and logged every
process start and inbound message itself — the committed fixture
`a1prime-native-stdio.json` is the scrubbed distillation of those spawn
logs plus the recorded run transcripts, so the rows below cite log
records, not model paraphrase. All runs executed inside a Herdr pane
(`HERDR_ENV=1`), in print mode, from a scratch repo subdirectory.

Confirmed behaviors and their tests:

| Behavior | Test ID |
|---|---|
| Env inheritance: `HERDR_PANE_ID`/`HERDR_WORKSPACE_ID`/`HERDR_TAB_ID`/`HERDR_ENV`/`PWD` reach the spawned relay verbatim on pi, claude, and devin; pi additionally injects `PI_CODING_AGENT` and extension/session vars, claude injects `CLAUDECODE`/`CLAUDE_CODE_*`, devin injects nothing | `a1prime_pi_env_inherited`, `a1prime_claude_env_inherited`, `a1prime_devin_env_inherited` |
| Spawn at session start (the `initialize` handshake rides process start, the first `tools/call` arrives seconds later), one process per session — repeat calls share the pid | `a1prime_pi_spawn_at_session_start`, `a1prime_claude_spawn_at_session_start`, `a1prime_devin_spawn_at_session_start` |
| Shutdown differs per harness: pi and devin close the relay via stdin EOF; claude sends SIGINT (signum 2) and no EOF — a relay must exit on either | `a1prime_pi_shutdown_stdin_eof`, `a1prime_devin_shutdown_stdin_eof`, `a1prime_claude_shutdown_sigint` |
| Crash respawn: after the server dies mid-session the next call lands on a NEW pid under the same parent ~40–110 ms later through a full `initialize` re-handshake; pi re-runs `tools/list` on the respawn, claude and devin skip it (tools cached) | `a1prime_pi_respawn_after_crash`, `a1prime_claude_respawn_after_crash`, `a1prime_devin_respawn_after_crash` |
| cwd: the relay's cwd is the invocation cwd on all three harnesses; only pi honors a `cwd` config key (spawn cwd diverged from `PWD`); claude ignores it, devin's schema has no such key (its project config is discovered upward but does not anchor the spawn cwd) | `a1prime_pi_cwd_config_key_honored`, `a1prime_claude_cwd_is_invocation`, `a1prime_devin_cwd_is_invocation` |
| Tool naming `mcp__<server>__<tool>` holds on all three; the allowlist mechanism differs — claude `--allowedTools 'mcp__<server>__<tool>'` scopes exactly (session ran with no other permissions), devin project `permissions.allow` patterns auto-approve | `a1prime_claude_first_class_allowlist`, `a1prime_devin_first_class_allowlist` |
| pi exposure caveat, reconciled: under codemode exposure `--tools` cannot scope a single MCP tool (`--tools probe_env` → zero tools; `--tools mcp__govprobe__probe_env` → `tool not available`; `--tools codemode` → codemode mounted, MCP registry empty `ALL_TOOLS=0`) — **with direct exposure (`toolExposure`) pi's `--tools` does allowlist `mcp__<server>__<tool>`, proven live 2026-09-29 for `mcp__executor__execute`**; the server still spawned and served `tools/list` under `--tools` restriction | `a1prime_pi_codemode_surface_and_tools_flag` |
| devin's `initialize` runs at spawn but `tools/list` is lazy (~6 s later, at first need) | `a1prime_devin_tools_list_lazy` |
| Rust stdio→unix-socket forwarder footprint: VmRSS/VmHWM 2000 kB after 100 round-trips, a 474,144-byte zero-dependency binary, exit 0 on stdin EOF | `a1prime_forwarder_footprint` |
| Config integrity: all four user-scope harness configs sha256-unchanged across the probe (the one session-state churn file carried zero probe references); probe configs removed after measurement | `a1prime_user_configs_untouched` |

Caveats for the relay contract (recorded, not separately tested): pi
surfaces project MCP tools only inside its `codemode` tool — a relay's
tools appear as `mcp__<server>__<tool>` inside a code-sandbox surface
unless `toolExposure` (direct exposure) is set; pi's `.pi/mcp.json` is
read at the session cwd only, so a relay registered for "the project"
must live in the directory the session launches from; claude's shutdown
is SIGINT — the relay must handle the signal rather than wait for EOF;
devin print mode in an untrusted workspace requires
`--respect-workspace-trust false` (or prior trust). Fixture hygiene is
pinned by `a1prime_fixture_scrubbed`.

Source: `contract-a1.md` (raw probe log `/tmp/mcp-probe-server.log`) —
pi-adapter trial; fixtures `a1-transport-trace.jsonl`,
`a1-gateway-observations.json`. Wave-2 OAuth trial evidence:
`contract-a1-oauth-evidence/` (compliant AS source, adapter install logs,
AS request log). Executor catalog + reconnect probes:
`contract-a1-bearer.md`, `contract-a1-bearer-evidence/request-log.jsonl`,
`reconnect/contract-a1-reconnect.md`, `reconnect/request-log.jsonl`,
`reconnect/per-call-analysis.txt` (p0p1 artifact bundle); fixtures
`a1-executor-bearer-requests.json`, `a1-executor-reconnect-requests.json`.
A1' stdio-relay probe: `contract-a1prime.md`,
`contract-a1prime-evidence/{pi,claude,devin}-spawn-log.jsonl`,
`*-run*.txt` (p0p1 artifact bundle); fixture `a1prime-native-stdio.json`.

## A2 — Socket concurrency

**Status: confirmed for the isolated herdr-tools daemon transport and for
the Herdr protocol-22 subscription leg (isolated named session); the last
open item (the silent stream teardown contract) is owner-decided — ruling
in spec §19.**

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
| A reply for a timed-out request lands after the recorded client-side rejection — it cannot re-settle the request, and the next request resolves to its own response. The wire log shows the reply is *inert*; the client's internal pending map is not observable on the wire, so discard is not directly proven | `a2_late_reply_inert_after_timeout` |
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
| `pane.scroll_changed` **is delivered** — on explicit scroll (offset 120 of max 267), on return to bottom, and on output growth while scrolled; the wave-1 non-delivery was a server-side no-op on a pane with no scrollback, not a delivery gap | `a2sub_scroll_changed_delivered` |

Observations recorded, no test (refusals and owner-decision evidence):
**command panes** created via `layout.apply` with a `command` appear in
`session.snapshot` but refuse subscriptions (`pane.scroll_changed` →
`pane_not_found`; `pane.output_matched` → `internal_error "failed to decode
pane read error"`) — PTY panes arm cleanly, so subscriptions target PTY
panes. **Silent stream teardown, evidence complete**: a malformed frame on
an armed subscription connection produces **zero bytes before an
ECONNRESET close, no error frame**, and the server logs a plain
`outcome="stream_closed"`; an immediate re-arm on a fresh connection
succeeds (`subscription_started`); state catch-up after re-arm is the
already-confirmed `recent`-text behavior.

**Schema note (resolves the wave-1 drift flag):** `just schema-live`
against the session server is **green** — the pinned
`herdr-api-schema.json` matches `herdr api schema --json` byte-for-byte,
and the pinned fixture already contains `pane.graphics.stream` (as a
doc entry). The wave-1 comparison measured the server's **runtime
unknown-method enum** (request methods, includes `pane.graphics.stream`)
against the schema doc's request surface (omits it): the drift is
**server-internal** (runtime enum vs its own doc), not fixture staleness.
No re-pin: the fixture's contract is to match the doc, and diverging it
would break `schema-live`. The governor must not treat the schema doc as
the exhaustive request surface.

Implication for the spec's A2 assumption: the Herdr socket offers **no
durable, multiplexed event stream**. A governor needing "subscription
plus concurrent requests" holds one connection per armed one-shot
subscription and one per in-flight unary request, and re-arms after every
EOF with a state read (`pane.read`/`session.snapshot`) as the catch-up.

Source: `contract-a2.md` § subscription leg,
`contract-a2-herdr-probe.mjs`, `contract-a2-subscription-evidence/`;
fixture `a2-subscription-evidence.json`.

The two legs together close the A2 subscription question at evidence
level; the silent stream teardown contract is owner-decided (spec §19):
an unexpected subscription EOF is re-armed with a state catch-up
(`pane.read`/`session.snapshot`), "server gone" is declared only when the
re-arm cannot connect, and every teardown emits a typed event.

## A3 — Start and prompt semantics

**Status: confirmed for the start/prompt sub-behaviors below; the "typed
pre-interactive error" assumption is **confirmed-negative** for runtime
startup failures (every runtime failure collapses to one untyped
`timeout`) — owner-decided 2026-09-29, ruling in spec §19; the
readiness-fidelity gap is a recorded owner decision; the claude
provider-limit start shape stays untested (not reproduced in-window).**

The probe ran live `agent start`/`agent prompt`/`agent get`/`agent list`
calls against pi, devin, agy, claude on dedicated panes in the isolated
named session `herdr-governor-contract` (`herdr 0.9.1`), timings measured
with `date +%s%N`. The committed fixture
`tests/fixtures/contract/a3-start-prompt-evidence.json` is the scrubbed
distillation; the raw report's verbatim CLI output stays artifact-side.

Confirmed behaviors and their tests:

| Behavior | Test ID |
|---|---|
| `agent start` on a healthy harness returns `type:"agent_started"` only after blocking to readiness — `interactive_ready:true`, `agent_status:"idle"`, session field populated (pi → `kind:path`/`herdr:pi`; devin, claude → `kind:id`); consistent ~3.0–3.7 s return | `a3_start_ready_01` |
| Registration is immediate: the agent appears in `agent list` during the start window with `agent_status:"unknown"` and **no** `interactive_ready` key; the status flips to `idle` in list records before `interactive_ready` appears, so the start response's flag is the authoritative return-time signal | `a3_start_ready_02` |
| Every runtime startup failure (missing binary, agent arg rejection, mid-start kill) returns the identical `{"error":{"code":"timeout"}}` after the full `--timeout` — the cause is erased for the caller; the pane falls back to shell showing the native error. Owner ruling (2026-09-29, spec §19): this untyped timeout is F15's "any other outcome" — stop falling back, record `failed{effectCertainty}`, and the Run settles by the transition rules; typed runtime startup errors are a non-blocking Herdr gap | `a3_start_timeout_01` |
| `agent start` on an agent-occupied pane — including the loser of two racing starts on one shell pane — returns typed `agent_pane_busy` and launches nothing (single winner, no double-launch). Typed pre-flight errors like this one are the only failure shape the F15 candidate fallback consumes | `a3_start_busy_01` |
| Killing the harness process mid-start leaves the caller observing only the generic timeout (no early return, no death notification); the pane settles at `agent_status:"unknown"` back at shell. Dead-in-flight and merely-slow starts are indistinguishable by cause — and indistinguishable from every other runtime startup failure, so the same untyped-timeout ruling settles them | `a3_start_inflight_kill_01` |
| `agent prompt` returns `type:"agent_prompted"` carrying the agent record — the committed fixture records `name`, `pane_id`, and `agent_session_kind` where present (pi, devin; devin also `agent_session_value`); the recorded agy and claude entries carry only the delivery word and `pane_id`, and agy's ack can carry no `agent_session` because none exists on any surface (`a3_agy_session_01`). Concurrent acks, incl. cross-harness, map unambiguously to their targets. The ack is a delivery/target snapshot, not a completion: `agent_status` inside still reads pre-dispatch `idle`, and no prompt-id or delivery sequence exists | `a3_prompt_ack_01` |
| `agent prompt`/`agent get` against a shell-only pane or unknown name returns typed `agent_not_found` | `a3_prompt_notfound_01` |
| Confirmed-negative: Herdr surfaces **no** `agent_session` for agy on any surface (start response, `agent get`, `agent list`) and agy has no `herdr:*` session source. The transcript source is qualified out-of-band (resolves the A5 `A5-AGY-UNQUALIFIED` gap): JSONL transcripts under `~/.gemini/antigravity-cli/brain/<uuid>/.system_generated/logs/`, SQLite trajectory store `conversations/<uuid>.db`, live `presence/<uuid>.lock` — all three share the conversation uuid; artifacts appear after the first turn, not at TUI open | `a3_agy_session_01` |
| `pane split --cwd <nonexistent>` does not fail — it silently creates the pane with `cwd` fallen back to `$HOME`; the pane record's `cwd`/`foreground_cwd` is the only tell | `a3_pane_cwd_01` |

Owner decision, recorded (`A3-READY-FIDELITY-01`): **readiness is
advisory; the identity-matched prompt ack plus qualification are the
signals; the Herdr detection gap is a non-blocking follow-up** — not a
failing test. The identity-matched ack proves **delivery to the pane,
not consumption**: a first-run gate that Herdr reports `idle` can
swallow the Task, and the Run then settles `no_handoff` via F25
(bounded — the 15-minute idle deadline caps the loss). The mitigations
in place are: catalog arguments that disable the gate (Devin
`--respect-workspace-trust false`), Claude's trust gate being reported
`blocked` and surfaced as an owner notice (spec §19, case 4), and F26
qualification — which detects gates only in its own qualification
directory, so a first-run gate in a different directory at Run time is
not covered. Evidence behind it: devin and agy both returned
`interactive_ready:true`/`agent_status:"idle"` while the pane sat at a
workspace-trust modal that cannot accept prompt text — the detection
rules (`workspace_trust_prompt`, `permission_prompt`) evaluated but did
not match, and `default_known_agent_idle_fallback` produced the idle.
For agy the fallback fires in every state, so `idle`/`interactive_ready`
cannot distinguish blocked from genuinely promptable; a prompt into a
gated modal would still ack `agent_prompted` (the ack carries no
screen-state validity). `agent_session` is absent until a gate is passed,
which is itself evidence the returned `idle` was pre-interactive.

Confirmed-negative adjunct: post-start agent death (kill or Esc-quit)
drops the agent from `agent list` entirely and returns the pane to
`agent_status:"unknown"` at a shell — observed for pi, devin, agy
(pinned inside `a3_start_inflight_kill_01`'s fixture record).

Untested / unpinned in this fold: `A3-CLAUDE-LIMIT-01` — the predicted
claude provider-limit screen did not reproduce (healthy provider; a real
prompt completed in ~3 s), so the limit failure shape remains
unobserved. `--timeout` default when omitted, `--wait`/`--until` prompt
variants, and harness kinds outside the four in scope were not
exercised. Adapter discovery for the qualified AGY transcript (which of
`presence/`/`brain/`/`conversations/` a governor adapter should follow)
is owner-decided — AGY is terminal-only; ruling in spec §19
(`A3-AGY-SESSION-01`).

Source: `contract-a3.md` (artifact bundle);
fixture `a3-start-prompt-evidence.json`.

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

**Status: confirmed per-harness resolution and failure contracts; the AGY
transcript-source gap is resolved by the A3 fold and owner-decided —
AGY is terminal-only (ruling in spec §19, `A3-AGY-SESSION-01`).**

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

Confirmed-negative / unqualified in wave 1: **AGY** — binary present and
catalog key present, but no `~/.agy` directory and no transcript adapter;
AGY sources resolve to terminal-fallback (test
`a5_agy_unqualified_terminal_only`). **Resolved by the A3 fold
(`contract-a3.md` §5, test `a3_agy_session_01`):** the native transcript
source is qualified — JSONL transcripts under
`~/.gemini/antigravity-cli/brain/<uuid>/.system_generated/logs/`, a
SQLite trajectory store `conversations/<uuid>.db`, and a
live-conversation `presence/<uuid>.lock`, all sharing the conversation
uuid. Adapter *discovery* is owner-decided (2026-09-29,
`A3-AGY-SESSION-01`; ruling in spec §19): AGY supervision is
**terminal-only** — terminal evidence via `agent.read`, no out-of-band
transcript/uuid discovery. A presence lock cannot be proven to belong
to a pane when AGY sessions run concurrently, and identity is never
guessed. The absent agy `agent_session` is a non-blocking Herdr gap;
per F28 an AGY Run without a native session settles
`unresolved(identity_unprovable)` after an unproven Herdr incarnation
change. Claude likewise has no structured transcript
reader — transcript resolution falls back
to the terminal; only its quota-reader was probed, and its zero-progress
result cannot serve as a progress proof (see the corrupt-tail row above).

Source: `contract-a5.md`; fixtures `a5-probe-outcomes.json`,
`a5-native-observations.json`, `a5-samples/` (18 sample files).

## A6 — Pane tagging and crash recognition

**Status: crash-recognition observations confirmed; the creation-time
tagging contract is owner-decided (spec §19).**

Confirmed behaviors and their tests:

| Behavior | Test ID |
|---|---|
| Killing the foreground native agent leaves a stale agent row for one read, then settles: pane, terminal, label, and tokens survive while `agent_session` disappears | `a6_kill_stale_read`, `a6_kill_pane_survives` |
| A shell kill removes the pane; a snapshot/get race exists (snapshot still lists it while `pane get` already errors `pane_not_found`), then it is absent everywhere | `a6_shell_kill_race`, `a6_shell_kill_absent` |
| `pane split --env GOV_RUN_ID=…` sets the process environment but surfaces nowhere in the required reads (no `env`, `label`, `tokens`, or `agent_session` field) | `a6_split_env_not_metadata` |
| `label` + `tokens` tags are not unique: two panes can carry identical tags on distinct terminals | `a6_tags_nonunique` |
| The advertised `agent_session.value` path is a reference, not a live file — it need not exist after the native dies | `a6_session_ref_not_file` |
| Schema surface: `PaneSplitParams` has no `label`/`tokens` field; the `LayoutNode` pane variant does (`label`, `cwd`, `env`, `command`, `pane_id`) | `a6_creation_tag_surface_absent_in_split` |

Owner-decided (spec §19; documented, no test): **the generic
creation-effect contract for atomic tagging/adoption.** The evidence
shows: (a) a labeled layout creation could be rediscovered after a lost
response, but the rebuild changed supplied IDs and topology; (b)
separate rename/report-metadata calls leave a crash window between
creation and tagging; (c) `--env` tags do not surface in reads. Ruling:
the governor adopts the F8 unconfirmed/no-adoption fallback — an
interrupted topology effect is reported `unconfirmed` and never adopted;
the F14 right-split stays; a labelled `layout.apply` is rejected.

Source: `contract-a4-a6.md`; fixtures `a46-identity-evidence.json`,
`protocol22-subset.json`.

## Jev — judgment service wire contract

**Status: confirmed for the request/response wire contract, the error
taxonomy, the credential seam, and the `GET /v1/models` surface;
confirmed-negative for any other machine-readable contract surface.**

Evidence boundary: five live judgment calls through the real
`TypeSafeSpecClient` (the router's client code, not a re-implementation)
all returned `kind:"response"` with complete probability distributions;
a raw `systemOne` call and a `score` call captured the verbatim
response envelope; three error classes were triggered live at the
service (401 `authentication_error`, 400 `api_usage_error`, 400
`max_tokens_exceeded`), four client-side probes produced SDK
error/abstain objects without a service response (request-too-large
abstain, timeout, caller abort, empty-questions validation), and six
further error classes are confirmed from SDK source only (the list in
the confirmed-negative paragraph below). SDK `@typesafe-ai/sdk` 0.6.0
(pinned by herdr-tools `package-lock.json`), probed against the
reference checkout at `922934e2`. No credential value appears in the
report or the fixtures — only presence metadata (key length, store
entry type, file mode).

Confirmed behaviors and their tests:

| Behavior | Test ID |
|---|---|
| Every judgment call is `POST {base}/v1/systemone` (default `https://api.typesafe.ai`) with `Authorization: Bearer` and a JSON body `{model, state, questions}`; `state` is the semantic task projection only — no caller tier, no catalog (`CT-JEV-REQ-1`) | `jev_request_wire_shape` |
| A serialized request over 96 KiB (`MAX_SPEC_REQUEST_BYTES`) abstains `invalid_response`/`request_too_large` before any socket write — measured 211,804 bytes with zero network traffic (`CT-JEV-REQ-2`) | `jev_request_too_large_client_gate` |
| `answers` is keyed by question name; a noul answer is exactly `{type:"noul", noul∈[0,1]}` — a calibrated P(yes) with no confidence field; `done_when_verifiable` returns mid-range probabilities (0.40–0.83 observed), never a boolean (`CT-JEV-RESP-1`) | `jev_noul_answer_shape` |
| A choice answer carries `choice` ∈ criteria labels, `confidence∈[0,1]`, and `probabilities` over the exact label set; the router enforces \|Σ−1\|≤1e-6 on its own question set — but a recorded reviewer-path response sums to 0.99, so the service does not promise an exact-1 sum (`CT-JEV-RESP-2`) | `jev_choice_answer_shape` |
| `confidence` is a distinct calibrated concentration measure, not `max(probabilities)` — recorded pairs (0.98→0.96 raw, 0.69→0.63 router, 0.93→0.92 reviewer) contradict any top-probability alias (`CT-JEV-RESP-3`) | `jev_confidence_not_max_probability` |
| `model` returns the resolved revision (`jev-1.13.0` for request alias `jev-latest`) and `usage{input_tokens,output_tokens}` is bookkeeping — recorded metadata, never a routing input (`CT-JEV-RESP-4`) | `jev_response_envelope_metadata` |
| 401 `authentication_error` body maps to transport component `http_401_authentication_error`; an unresolvable key abstains `authentication_unavailable`/`api_key` with no request sent (code-cited) (`CT-JEV-ERR-1`) | `jev_error_authentication_mapping` |
| 400 bodies map by `detail.error_type` (`api_usage_error`, `max_tokens_exceeded` observed live); a non-conforming `error_type` falls back to `http_<status>`; non-APIError transport failures map to `transport` (`CT-JEV-ERR-2`) | `jev_error_body_component_mapping` |
| Timeout surfaces `APITimeoutError`, caller abort `APIUserAbortError` — only the pre-abort case proves no request was sent; a client timeout proves no response was received, and does not disprove the request went out; the router/reviewer path configures `maxRetries:0` and no `X-TypeSafe-Retry-Count` header was ever observed (`CT-JEV-ERR-3`) | `jev_timeout_abort_no_retry` |
| Credential resolution order: explicit option → `auth.json["typesafe"]` `api_key` → `TYPESAFE_API_KEY` (the store wins over env); `!cmd`-/`$ENV`-indirected key values pass through verbatim and are never executed (`CT-JEV-AUTH-1`) | `jev_auth_resolution_order` |
| A sub-0.5-confidence spread still yields the verbatim top label as the route input — intent `reason` .36 at confidence .25, tier `max` .47 at .36 — no abstain on low confidence (`CT-JEV-PROB-1`) | `jev_spread_top_label_route` |
| `GET /v1/models` is the only introspection surface — two ModelCards `{name, description, release_date}` (`jev-latest`, `jev-preview`); all five live calls responded `kind:"response"` | `jev_models_surface`, `jev_probe_calls_recorded` |

Confirmed-negative: no local System One/Jev CLI, schema endpoint, or
contract-introspection endpoint exists — `command -v typesafe jev
systemone` found nothing and the SDK exposes exactly two resources
(`systemOne`, `models.list`); there is no machine-readable question
contract beyond the SDK's `.d.mts` type surface. The error classes
`PermissionDeniedError` (403), `NotFoundError` (404),
`UnprocessableEntityError` (422), `RateLimitError` (429, exposes
`retryAfterMs`), `InternalServerError` (≥500), and `APIConnectionError`
are confirmed from SDK source only — deliberately not triggered live.
Every non-2xx is an `APIError` subclass carrying
`{status, headers, body, requestId?}`; `x-typesafe-request-id` was
present on observed 200 and 4xx responses.

Operational notes for the owner (recorded, not tested): (a) the service
enforces its own unversioned size cap — `max_tokens_exceeded` at a raw
256 KiB state — above the router's 96 KiB client-side gate; the
governor should keep the client-side number as its contract and treat
the server 400 as a transport failure class, since the server threshold
is unversioned. (b) `jev-latest` is a floating alias — observed
resolution `jev-1.13.0` — so any golden output contract pins the
*shape* and accepts the resolved `model` string as opaque metadata.
(c) `confidence` is a separate calibrated measure — a contract that
aliases it to `max(probabilities)` is contradicted by recorded wire
data (see `jev_confidence_not_max_probability`).

Source: `contract-jev.md` (probe `jev-contract-probe.mjs`, raw
secret-scrubbed JSON in the artifact bundle); fixtures
`jev-wire-evidence.json`, `jev-raw-response.json`,
`jev-launch-evaluation.json`, `jev-supervision-review.json`.

---

## Escalation register

| Item | Status | What unblocks it |
|---|---|---|
| A1 transport | **owner-decided 2026-09-30** — native per-session stdio relay (ADR-0004); A1' evidence confirmed | closed — see section; ruling in ADR-0004 |
| A1 OAuth round-trip | pi adapter: confirmed-negative (metadata loader unimplemented); Executor catalog: 401 triggers real RFC 8414/OIDC discovery and a static bearer via apiKey template + provider item is confirmed | none at evidence level |
| A1 offline error typing | confirmed untyped transport failure on both measured paths (pi adapter `fetch failed`; Executor `Internal tool error [<id>]`) | governor/spec contract must require a typed transport-unavailable error (Phase 3 work) |
| A2 Herdr socket: silent stream teardown on malformed input | owner-decided 2026-09-29 | closed — the re-arm-and-catch-up contract is adopted for unexpected subscription EOF; ruling in spec §19 |
| A3 start/prompt semantics | confirmed — see section; the typed-runtime-startup-error assumption is confirmed-negative, owner-decided 2026-09-29 | closed — see section; ruling in spec §19 |
| A4 incarnation proof | confirmed-negative | spec fallback already defined — no action needed |
| A5 AGY transcript source | owner-decided 2026-09-29 — terminal-only | closed — AGY supervision uses terminal evidence (`agent.read`); no out-of-band transcript/uuid discovery; ruling in spec §19 |
| A5 header validation / corrupt-vs-absence | confirmed reference-reader gaps | governor adapter must implement the stricter contract (Phase 3 work) |
| A6 creation-time atomic tagging | owner-decided 2026-09-29 | closed — the F8 unconfirmed/no-adoption fallback is adopted; ruling in spec §19 |
| Jev | confirmed — see section | closed — see section |
| Herdr gaps (non-blocking) | recorded | agy `agent_session` absent (`A3-AGY-SESSION-01`); readiness reported at workspace-trust gates (`A3-READY-FIDELITY-01`); no typed runtime-startup failure causes — missing binary, rejected args, and mid-start death all surface as one untyped `timeout` (owner ruling 2026-09-29) — non-blocking Herdr follow-ups |

## Fixture inventory

| Fixture | Content | Source |
|---|---|---|
| `a1-transport-trace.jsonl` | 8-record probe-server frame log over two runs: handshake, headers, SSE GET, restart, self-contained re-call (pi MCP adapter trial) | `/tmp/mcp-probe-server.log` |
| `a1-gateway-observations.json` | recorded adapter-side strings: install result, echoed args, untyped offline error, OAuth discovery, HTTPS refusal, no-uninstall surface (pi MCP adapter trial) | `contract-a1.md` |
| `a1-executor-bearer-requests.json` | distilled request log: Executor catalog `probeEndpoint` 401 + OAuth-discovery leg, apiKey-template authenticated path (every request `Authorization: Bearer`), pi 0.99 native `pi mcp` leg (static headers; `needs sign-in` on 401); method/path/header-name/auth fields only | `contract-a1-bearer-evidence/request-log.jsonl` (artifact bundle) |
| `a1-executor-reconnect-requests.json` | distilled request log of the restart/stale-session probe: four server processes, `Mcp-Session-Id` issue/reuse/404-reinit, untyped server-down error; method/header-name/auth/session fields only | `contract-a1-bearer-evidence/reconnect/request-log.jsonl` (artifact bundle) |
| `a1prime-native-stdio.json` | scrubbed distillation of the A1' per-harness stdio spawn logs: event records (start/request/tools_call/eof/signal/exit) per pi/claude/devin session, env names filtered to `HERDR_*`/`PWD` + harness markers (dropped count recorded), env values whitelisted, paths as `/home/user/` placeholders; allowlist/codemode observations, forwarder RSS/binary footprint, config-integrity record | `contract-a1prime-evidence/` (`contract-a1prime.md`, `*-spawn-log.jsonl`, `*-run*.txt`; artifact bundle) |
| `a2-tools-daemon-trace.jsonl` | 91-record wire log: hello/ack, multiplex, malformed frames, timeout, close | `/tmp/gov-p2-a2-evidence.jsonl` |
| `a2-subscription-evidence.json` | scrubbed distillation of the Herdr protocol-22 subscription leg: isolation counts, framing, subscription semantics, concurrency, reconnect, malformed, timeouts, schema drift | `contract-a2-subscription-evidence/evidence.jsonl` (artifact bundle) |
| `a3-start-prompt-evidence.json` | scrubbed distillation of the `agent start`/`agent prompt` probe: per-harness start envelopes and readiness shape, registration window, timeout/busy/not-found envelopes, racing starts, in-flight kill, prompt acks, AGY transcript layout, pane-cwd fallback, unreproduced claude limit | `contract-a3.md` (artifact bundle) |
| `a46-identity-evidence.json` | 27 three-surface captures + 47 command receipts + kill/session records | `gov-p2-a46` lane `contract-a4-a6-raw.jsonl` |
| `a46-restart-history.json` | one restart window, mailbox record kinds, persisted run identities | `contract-a4-a6-historical-summary.json` |
| `a5-probe-outcomes.json` | 27 recorded case outcomes from the reference readers | `/tmp/gov-a5-*/results.json` |
| `a5-native-observations.json` | native location shape, identity fields, AGY/env presence | `/tmp/gov-a5-*/metadata.json` |
| `a5-samples/` | byte-faithful synthetic transcript inputs (Pi JSONL, Devin ATIF, Claude JSONL) | `/tmp/gov-a5-*/cases-*` |
| `protocol22-subset.json` | pinned schema subset: envelopes + objects the evidence exercised | `tests/fixtures/herdr-api-schema.json` |
| `jev-wire-evidence.json` | distilled Jev probe record: 5 live calls (wire headers, request sizes, results), 7 error probes, models list, key-seam presence metadata, code-cited gate constants and auth-resolution order | `jev-contract-probe.json` + `contract-jev.md` code citations (artifact bundle) |
| `jev-raw-response.json` | verbatim 200 response envelope: resolved `model`, typed `answers` (noul + choice), `usage` | `jev-raw-response.json` (artifact bundle) |
| `jev-launch-evaluation.json` | full request+response capture of one router-path judgment call: `{model, state, questions}` body, exact criteria labels, resolved answer envelope | `fixtures/jev/launch-evaluation.json` (artifact bundle) |
| `jev-supervision-review.json` | full request+response capture of one reviewer-path judgment call: 6 nouls + one 13-label choice, observed Σ=0.99 probabilities | `fixtures/jev/supervision-review.json` (artifact bundle) |

`just contract` runs the suite — 144 checks, fail-closed on absent or
malformed fixtures.

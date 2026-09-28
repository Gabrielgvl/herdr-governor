# herdr-tools failure & performance baseline

Evidence window requested: **2026-08-27 → 2026-09-27**. Scope: read-only. Nothing modified; every figure below was produced by a command run during this survey.

## Coverage and caveats

- **60 `.herdr` dirs** found under `~`, `<worktrees>`, `~/.pi` (includes nested `<repo>/.herdr/worktrees/*/.herdr`). 25 contain `diagnostics/tools.jsonl`, 7 `router/decisions.jsonl`, 6 `supervision/reviews.jsonl`, 5 `availability/cooldowns.jsonl`. The other dirs hold only artifacts/locks.
- **Actual data span is narrower than the window**: oldest record in any of these files is 2026-09-19; tools.jsonl spans 09-21→09-27, decisions 09-19→09-27, reviews 09-19→09-26, cooldowns 09-21→09-27. No source has pre-09-15 data at all.
- **journalctl** for `herdr-tools-daemon` contains entries only from **2026-09-26 13:25** onward — the systemd unit was created that day (commit `dbc2eb8`, "Durable supervisor daemon"). Earlier daemon history does not exist in the journal.
- **Daemon namespace** `~/.config/herdr/herdr-tools-daemon/` exists only since 2026-09-26 (intents/mailbox are daemon-era by construction; `transfers/` is empty).
- **Handoff records** at `~/.config/herdr/herdr-handoffs/*/​.tools/state.json` were used as an extra state-file source (in the spirit of "timestamps already recorded in the state files"); they cover 2026-09-15→09-27.
- tools.jsonl records carry no error text — only `phases` outcomes and `effectCertainty`. "Failure" here = a non-`success` phase. `operation: "invalid"` = caller input failed schema validation (a caller/tool-contract failure, not necessarily a runtime defect). `operation: "unknown"` = action outside the telemetry allowlist of the build that wrote it.
- `effectCertainty: "unknown"` on an execute failure means the effect is indeterminate — the prompt may or may not have landed. That is the worst kind of failure.

## Failure-class table (counts per source, 2026-08-27 → 2026-09-27)

Sources abbreviated: T=tools.jsonl, D=decisions.jsonl, R=reviews.jsonl, C=cooldowns.jsonl, I=daemon intents, M=daemon mailbox, J=journalctl, H=handoff state.json, G=git fix/hotfix/revert commits.

| Failure class | T | D | R | C | I | M | J | H | G |
|---|---|---|---|---|---|---|---|---|---|
| Prompt delivery (communicate.prompt/steer/interrupt/keys) | **93** | – | – | – | – | – | – | – | 8 |
| Supervision verdict quality (stalled/blocked/unknown/risk verdicts) | – | – | **227** (+100 violations) | – | – | 30 (reviewer_degraded 12, reviewer_attention 10, evidence_gap 8) | – | – | **34** |
| Handoff / completion tracking (runs stuck awaiting_handoff >24h) | – | – | – | – | – | – | – | **1030** stale, 44 recovery_pending | 11 |
| Routing / abstain (abstained+abstain+rejected) | – | **95** (13.8% of 687) | – | 12 quota | – | 1 provider_limit | – | – | 9 |
| Readiness / identity (spawn, pane/agent identity, transport) | 47 (launch 14, inspect.target 33) | – | – | 14 transport | – | – | – | – | covered in launch fixes |
| Tool-input validation (caller schema misuse) | **58** invalid + 5 unknown-op | – | – | – | – | – | – | – | – |
| Jobs (get/cancel failures) | 19 | – | – | – | – | – | – | – | – |
| Daemon / MCP connectivity & lifecycle | – | – | – | – | 0 failed/unresolved of 17 | **20** (downtime_gap 15, reconciliation_degraded 5) | **5** (1 shutdown FAILURE, 4 hint dropped) | – | 0 (daemon is new) |
| Pane layout / mutations | 18 (adopt 4, close 4, invalid 8, tab.create 1, tab.close 1) | – | – | – | – | – | – | – | 9 |
| Wait / turn control | 4 | – | – | – | – | – | – | – | 8 |
| Worktree / git | 0 (no telemetry stream exists) | – | – | – | – | – | – | – | ~2 |
| Inspect / status | 1 | – | – | – | – | – | – | – | 5 (inspect) |

Totals per source: **T 240 non-success of 7326** (182 execute-failure, 58 validate-failure) · **D 95 non-admitted of 687** · **R 227 non-progress of 481 reviews + 100 violation records** · **C 26 provider failures** · **I 0 bad of 17** · **M 57 of 150 unread; ≥41 failure-signal events** · **J 5 bad lines of 20** · **H 1912 runs: 616 handed_off, 1062 awaiting_handoff (1030 stale >24h), 190 cancelled, 44 recovery_pending** · **G 82 fix-keyword commits of 230 total (35.7%)**.

## Top examples per class (file:line or log timestamp)

### Prompt delivery — 93 execute failures, effectCertainty `unknown`
- `~/<repo>/.herdr/diagnostics/tools.jsonl:18` @2026-09-22T10:14:35.610Z — `herdr_communicate.prompt` execute failure
- `~/<repo>/canary/.herdr/diagnostics/tools.jsonl:16` @2026-09-21T18:39:02.659Z — `prompt` failure
- `~/<repo>/.herdr/diagnostics/tools.jsonl:371` @2026-09-22T15:02:39.660Z — `interrupt` failure
- Rates: prompt 44/539 = **8.2%**, steer 17/428 = 4.0%, interrupt 12/42 = **28.6%**, cancel 19/24 = **79.2%**.

### Supervision verdict quality — 227 non-progress verdicts, 100 evidence violations
- `unknown` verdicts: **151 of 481 reviews (31%)** — supervisor cannot classify. Top reasons: `none` 45, `no_output` 32, `scope_drift` 18, `tool_failure` 17, `external_dependency` 16. Sample: `~/<repo>/.herdr/supervision/reviews.jsonl:2` @2026-09-22T12:03:11.662Z (`unknown`, tool_failure, task-61fbd7c9-1).
- Contradictory verdicts: `unknown`/`risk`/`blocked`/`stalled` with reason `verification_passed` — **14 records** where evidence says done but verdict says otherwise. Sample: `~/<repo>/.herdr/supervision/reviews.jsonl:106` @2026-09-26T18:12:15.355Z (`unknown`, verification_passed); `~/<repo>/.herdr/supervision/reviews.jsonl:19` @2026-09-22T14:16:28.130Z (`blocked`, verification_passed).
- Evidence-collection failures: **96 × `evidence_budget_exceeded`** violations (e.g. `~/<repo>/.herdr/supervision/reviews.jsonl:4` @2026-09-22T12:16:41.657Z, `record_exceeds_budget`) + 3 × `read_only_dirty_workspace` (`~/<repo>/.herdr/supervision/reviews.jsonl:182-184` @2026-09-21T11:11Z).
- Verdict mix: progress 254, unknown 151, blocked 42, stalled 15, risk 17, appears_complete 2. blocked reasons led by `external_dependency` 34 (e.g. `reviews.jsonl:48` @2026-09-23T19:38:09.740Z).

### Handoff / completion tracking — 1062 runs never handed off
- **1030 of 1912 handoff runs (54%)** sit in `awaiting_handoff` older than 24h with no `handoff.md`; e.g. run `38d36c65-…` created 2026-09-19T17:23:22Z (worker-3), run `fc82bb69-…` 2026-09-22T19:48:51Z (task-7d046258-1), run `a9c982a9-…` 2026-09-18T16:01:09Z. Age buckets: >7d 665, 1–7d 365, <1h 6 (still-running).
- 26 runs wrote `handoff.md` anyway while lifecycle stayed `awaiting_handoff` — stale state, not just missing artifacts.
- 44 runs in `recovery_pending`, 190 `cancelled` (118 of those do carry handoff.md). (Note: "stuck" here means the recorded lifecycle never settled; whether the child died or the handoff mechanism lost it is not distinguishable from state.json alone — flagged as interpretation, not count.)

### Routing / abstain — 95 non-admitted of 687 decisions (13.8%)
- `low_confidence` 44 (intent 24, category 14, manager_useful 3, tools:* 3). e.g. `~/<repo>/.herdr/router/decisions.jsonl:1` @2026-09-21T17:45:59Z.
- `transport_failed` 23 — Jev/backend unreachable: http_400 11, transport 9, http_400_max_tokens_exceeded 3. e.g. `~/<repo>/.herdr/router/decisions.jsonl:1` @2026-09-22T10:12:28Z.
- `invalid_response` 16 (weakest_sufficient_tier 9, intent 3, bypass 1, manager_count 1, planner_count 1) — model returned unparseable/off-schema answers. e.g. `~/<repo>/.herdr/router/decisions.jsonl:23` @2026-09-24T13:25:27Z.
- `catalog_unavailable` 11 (e.g. `decisions.jsonl:6` @2026-09-23T12:09:03Z). `rejected` 2 × done_when_unverifiable (`<repo> …/decisions.jsonl:36,43` @2026-09-22). Two schema vintages coexist: `abstained` (83) and `abstain` (10).

### Readiness / identity — provider transport failures
- `cooldowns.jsonl` 26 records: transport 14 (`agent_pane_not_found` ×7, `timeout` ×7) + quota 12 (`CLAUDE_API_ERROR/rate_limit` ×10, `quota_exceeded` ×2).
- `agent_pane_not_found` examples: `~/<repo>/.herdr/availability/cooldowns.jsonl:1` @2026-09-22T14:24:51Z (anthropic), `…/<repo>/.herdr/availability/cooldowns.jsonl:3` @2026-09-22T14:19:07Z (cognition) — launch probed a pane identity that did not exist.
- `timeout` examples: `~/<repo>/.herdr/availability/cooldowns.jsonl:1` @2026-09-21T10:19:21Z (google/antigravity).
- `quota` example: `…/<project-b>/.herdr/availability/cooldowns.jsonl:1` @2026-09-24T11:32:43Z (anthropic quota_exceeded).
- Launch execute failures: 14/624 = 2.2% (12 `absent` + 2 `partial` certainty). e.g. `<repo> …/tools.jsonl:76` @2026-09-22T11:38:27Z.

### Daemon / MCP connectivity
- journal: `Sep 26 14:14:37` — "shutdown incomplete: Daemon instance release is indeterminate" → `status=1/FAILURE` on SIGTERM stop; `Sep 26 14:54:34–36` — 4× "hint dropped: Operation aborted".
- mailbox `downtime_gap` ×15 — daemon downtime windows detected and reported to managers (e.g. `mailbox/9b9d50ed…/unread/2026-09-26T171441.712Z-….json`, gap 17:14:29→17:14:39Z).
- `reconciliation_degraded` ×5 (e.g. `mailbox/f7dd8270…/unread/2026-09-26T175420.423Z-….json`) — snapshot reconciliation failures against the endpoint.
- intents: all 17 records `completed`, children disposition `bound`; **0 failed/unresolved/recorded residue** — but this only covers the ~1 day the daemon has existed.
- mailbox delivery backlog: **57 of 150 records unread** (38%), spread over 8 of 9 manager mailboxes — notifications managers never consumed.

### Pane layout / mutations
- `pane.invalid` ×8 (`<repo> …/tools.jsonl:63-64` @2026-09-21T18:13Z), `pane.adopt` ×4/19 = 21% (`<repo> …/tools.jsonl:34` @2026-09-22T10:20:47Z), `pane.close` ×4 (`<repo> …/tools.jsonl:72` @2026-09-21T18:24:31Z), `tab.create` ×1, `tab.close` ×1.

### Jobs / wait / status
- `jobs.invalid` ×32 (all validate failures), `jobs.get` ×9, `jobs.cancel` ×10/33 = 30%; `wait.wait` ×4 (`<repo> …/tools.jsonl:531` @2026-09-22T19:04:00Z); `status` ×1 (`<repo> …/tools.jsonl:1876` @2026-09-26T17:46:58Z).

## Fix/hotfix/revert commits per subsystem (230 commits in window; 82 fix-keyword commits = 35.7%)

Counted by non-test `src/` files touched; a commit touching several subsystems counts once per subsystem. Merge-PR commits classified by title. 1 explicit `hotfix:` (a455cca), 1 `Revert` pair (5baa4b2 reverts 5518940).

| Subsystem | Fix commits |
|---|---|
| supervision/review (verdicts, moves, lifecycle ordering, monitor) | **34** |
| launch (spawn/fallback, prompt transport, AGY, evidence redaction) | **27** |
| mcp/schema/tool surface | 13 |
| jobs/handoff | 11 |
| routing/jev (abstains, tiers, catalog) | 9 |
| pane/tab/mutation | 9 |
| prompt/communicate | 8 |
| wait | 8 |
| profiles | 7 |
| inspect | 5 |
| worktree/git | ~2 (node_modules-symlink untracks) |
| misc/unclassified | 3 |

Notable fix clusters: 09-01/09-03 pane-move × supervision-review races (8 commits in 3 days); 09-19→09-22 launch/routing dogfood fixes (transport-abstain bypass, catalog seam rewire, spawn-timeout fallback, tier-chain hotfix); 08-30→08-31 wait/detached-wait semantics (6 commits).

## Performance

### Timed CLI (10 runs each, wall time, this machine ~2026-09-27T10:2x UTC)

| Command | n | median | p95 | min–max |
|---|---|---|---|---|
| `herdr api snapshot` | 10 | **9 ms** | **10 ms** | 8–10 ms |
| `herdr status --json` | 10 | **2 ms** | **3 ms** | 2–3 ms |

(snapshot payload ≈ 78 KiB; measured with `date +%s%N` deltas around each invocation.)

### Tool-call latency recorded in tools.jsonl (server-side durationMs)

| Operation | n | median | p95 |
|---|---|---|---|
| herdr_launch.launch | 624 | **5 703 ms** | **16 332 ms** |
| herdr_communicate.prompt | 539 | 632 ms | 1 415 ms |
| herdr_communicate.steer | 428 | 600 ms | 1 314 ms |
| herdr_inspect.target | 2 227 | 86 ms | 501 ms |
| herdr_wait.wait | 967 | 54 ms | 286 ms |
| herdr_pane.close | 451 | 143 ms | 808 ms |

### Launch latency (tool call → child started), from persisted timestamps

| Measure | What it spans | n | median | p95 |
|---|---|---|---|---|
| A — `herdr_launch` `durationMs` | whole tool call (validate→route→…→prompt_verification→supervision_bind) | 624 | 5 703 ms | 16 332 ms |
| B — intent `recordedAt`→`updatedAt` (state `completed`) | daemon `begin` → children bound | 17 | **5 367 ms** | **71 713 ms** |
| C — decision `timestamp` → pi session-file timestamp | routing decision → child agent process start | 21 | **888 ms** | **1 365 ms** |
| D — decision `timestamp` → handoff `state.json createdAt` | decision → run record persisted (pre-spawn phase) | 583 | 44 ms | 202 ms |

Provenance: A/B/D use `timestamp`/`recordedAt`/`updatedAt`/`createdAt` fields in `.herdr/diagnostics/tools.jsonl`, daemon `intents/<key>/*.json`, and `herdr-handoffs/<runId>/.tools/state.json`. C joins `decisions.name` (= launchId) to `agentName task-<launchId[:8]>-N` and uses the session-file creation timestamp embedded in the pi session filename recorded in `reviews.jsonl` `evidence.agentSession.value` (pi children only — that's why n=21). Interpretation: ~0.9 s median from decision to spawned process; the remaining ~4.8 s of the typical 5.7 s call is pre-decision Jev evaluation plus post-spawn readiness/prompt/supervision-bind phases. B's p95 (71.7 s) is one slow launch — no failed intents exist to explain it in the file.

### Daemon process (systemd unit, PID 1706367)

- Binary: `node …/herdr-tools/dist/src/daemon/main.js`; listening on `~/.config/herdr/herdr-tools-daemon/daemon.sock`; uptime ~16 h 58 m.
- **RSS 180 068 KB (~176 MB)**, VSZ 1.58 GB; **CPU time 24 s** over uptime (~0.04% average). systemd peak-memory line for the previous instance reported 286.5 MB peak.
- A second, dev/test daemon runs from the worktree build (`<project-c>`, PID 2541467): RSS 92 616 KB, CPU 21 s over ~1 d 7 h — out of scope for the canonical namespace but it is a real resident process.
- `daemon.json` key names: `startedAt`, `heartbeat` (current), `capacity` (=ok), `lastStoppedAt`.

### Process sprawl (observed, not timed)

- **31 live `herdr-tools …/mcp-server.js` node processes** — one per agent pane/session — totaling **~1 588 MB RSS** (several >110 MB, ages up to ~2.7 days). The per-pane MCP host model is the dominant resident cost of the runtime, ~9× the daemon itself.

## What this baseline cannot show

- Pre-09-19 history: state files hold nothing older; journal holds nothing before 09-26.
- Error text: tools.jsonl stores no messages, so root causes for the 240 tool failures live only in the (unpersisted) tool responses.
- Per-failure attribution of `awaiting_handoff` runs (child died vs artifact lost vs monitor missed it) needs pane-liveness correlation across 1030 runs — not performed here.
- `reviews.jsonl` in project dirs goes quiet after 09-26 18:52 because the durable daemon took over supervision; its own `.herdr` has only 10 records.

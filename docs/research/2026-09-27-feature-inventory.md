# herdr-tools feature inventory & usage evidence

**Scope:** user-visible features of the live checkout `/home/user/.pi/agent/extensions/herdr-tools` (branch `main`, HEAD `8263684`, 2026-09-27, 4 commits ahead of origin). Read-only audit; 39,410 LOC of TS across `index.ts`, `src/` (104 files), plus `herdr-profiles/`, `skills/`, `bin/`, `deploy/`, `scripts/`.

**Usage window:** 2026-08-27 .. 2026-09-27 (dates inclusive). All numbers below are counts from commands run against on-disk state; estimates are labeled.

**State searched:** every `.herdr/` under `~`, `<worktrees>`, `~/.pi`, `~/.local/share/herdr-tools` (44 dirs found; 26 carry `diagnostics/tools.jsonl`, 14 carry `router/decisions.jsonl`, 12 carry `supervision/reviews.jsonl`, 10 carry `availability/cooldowns.jsonl`), plus the daemon namespace `~/.config/herdr/herdr-tools-daemon/` (`intents/`, `mailbox/`, `transfers/`, `.herdr/supervision/`), `~/.config/herdr/herdr-handoffs/` (universal disk handoffs), `~/.cache/herdr-tools/message-attachments/`, `~/.local/state/herdr-tools-autoupdate/`, `~/.herdr/{artifacts,worktrees}`, `~/.config/herdr/{recovery,sessions,herdr-client.log}`.

**Architecture state (drives interpretation):** ADR-038 collapsed the model surface to exactly three daemon-backed tools (`herdr_launch`, `herdr_run`, `herdr_status`); `index.ts` registers **nothing** in Pi (C7) — the tools reach hosts via the MCP adapter / executor gateway. The old seven-tool surface (`herdr_inspect`, `herdr_jobs`, `herdr_communicate`, `herdr_wait`, `herdr_pane`, `herdr_tab`, `turn-control`) still exists on disk but is imported only by tests and by `src/tools/launch.ts` internals — yet it kept receiving real calls through 2026-09-27 because older installed deployments still serve it (telemetry is per-project and version-stamped by behavior, not version).

## Feature table

| Feature | Owning src files (LOC) | ADR(s) | Last git change | Usage 2026-08-27→09-27 |
|---|---|---|---|---|
| `herdr_launch` (Task contract: objective/scope/doneWhen/constraints/tier/replicas/recoveryOf/label/cwd; idempotencyKey) | src/launch-schema.ts (131), src/tools/launch.ts (3,348 — impl shared by daemon handler), src/daemon/handlers/launch.ts (290), src/daemon/intents.ts (663) | 037, 038 | 2026-09-26 | 624 `herdr_launch:launch` calls; 592 admitted decisions; 3 live intent dirs under daemon ns |
| `herdr_run` (observe/reconcile/transfer/claim/ack) | src/tool-surface.ts (217, shared), src/daemon/handlers/run.ts (277), src/daemon/ownership.ts (314), src/handoff-resume.ts (127) | 038, 031 | 2026-09-26 | 21 calls: ack 15, reconcile 5, observe 1 (new surface, 09-26→09-27 only, <repo> project) |
| `herdr_status` (read-only daemon projection) | src/tool-surface.ts, src/daemon/handlers/status.ts (297) | 038 | 2026-09-26 | 16 calls (09-26→09-27, <repo> project only) |
| Daemon (namespace, socket server, intents, transfers, reattach, hints) | src/daemon/{main 482, server 328, client 506, protocol 187, namespace 82, intents 663, ownership 314, reattach 527, runtime 450, hints 154, instance 56}, bin/herdr-tools-daemon.mjs (8), deploy/systemd+executor (~335) | 038 | 2026-09-26 | daemon.json live at ~/.config/herdr/herdr-tools-daemon; 3 intents; 9 run mailboxes; 140 mailbox events |
| Mailbox (unread→acked events per run) | src/daemon/mailbox.ts (1,024) | 038 | 2026-09-26 | 140 events: 57 unread, 83 acked across 9 run mailboxes |
| Routing: quality tiers + workload-profile policy | src/routing-policy.ts (321), src/router.ts (371), src/router-log.ts (622) | 032, 037 | 2026-09-26 | 687 decisions in window (admitted 592, abstained 83, abstain 10, rejected 2). effectiveStartTier: strong 337, standard 137, frontier 68, economy 22, max 18, utility 2. Caller-set tier on 394/687 |
| Workload intent classification (Jev) | src/router.ts, src/supervision/model-service.ts (118), src/typesafe-spec.ts (191) | 032, 035, 037 | 2026-09-26 | intents: implement 302, unknown 70, review 64, debug 46, verify 45, explore 41, reason 40, coordinate 2, unlabeled 77 |
| Model catalog + contract compilation | src/catalog.ts (617), src/compile.ts (407), src/spec-baseline.ts (34) | 035, 037 | 2026-09-24/26 | every admitted decision carries a compiled operating-point chain; specLabels seen: task, catalog-scan, evid-scan, router-scan, probe |
| Runtimes: devin / claude / pi / agy | src/catalog.ts, src/compile.ts, src/profiles/adapters.ts (245), runner wiring in src/tools/launch.ts, src/supervision/{devin-trace 234, claude-quota 193} | 021, 022 (agy), 026, 037 | 2026-09-26 | selectedPoint: devin 456, claude 86, pi 42, agy 0 (agy present in 608 candidate chains, never selected) |
| Replicas (replicas>1 over isolated worktrees) | src/launch-schema.ts (field), src/worktree.ts (420) | 037 | 2026-09-19 | **0** of 592 admitted decisions had count>1 — unused in window |
| Recovery (`recoveryOf`) | src/launch-schema.ts, src/routing-policy.ts (recovery evidence), src/handoff-resume.ts (127) | 037, 031 | 2026-09-26 | 8 decisions with non-null recoveryOf |
| Workload tabs (`workload:<intent>` grammar, ≤4 panes/tab) | src/tools/launch.ts (D12 grammar ~lines 1892-1924), src/tools/tab.ts (218) | 037 | 2026-09-26 | every admitted launch evaluates the grammar (≤592 launches — estimate: tab placement itself leaves no per-launch record); explicit tab ops: create 36, close 29, focus 2, rename 0 |
| Supervision: Jev review loop, evidence, monitor, notify | src/supervision/* (19 files, 9,973 LOC incl. supervisor 2,653, evidence 1,557, review-log 807, monitor 438, vcc-view 523, trace-source 536), src/reviewer.ts (305), src/typesafe-reviewer.ts (258) | 019, 020, 033, 034, 036 | 2026-09-26 | 744 reviews logged (progress 248, unknown 245, blocked 84, risk 34, stalled 30, appears_complete 4, unlabeled 99); agentKinds: devin 452, claude 159, pi 133; +10 daemon-side reviews |
| Universal disk handoff (`herdr-handoffs/<run>/handoff.md`) | src/handoff.ts (1,013), src/handoff-gate.ts (349), src/handoff-resume.ts (127) | 031, 036, 037 | 2026-09-26 | 1,915 handoff dirs touched in window; 805 contain handoff.md; daily volume peaked 471 on 09-19 |
| Legacy `herdr_inspect` (target/context/health/collection) | src/tools/inspect.ts (492), src/context.ts (295), src/targets.ts (222), src/health.ts (247) | 017, 025 | 2026-09-19 | 2,514 calls: target 2,227, collection 153, context 73, health 51 — heaviest single feature; superseded by herdr_status (ADR-038) |
| Legacy `herdr_jobs` + job registry | src/tools/jobs.ts (95), src/job-registry.ts (1,795), src/jobs-schema.ts (70), src/job-notification.ts (55) | 002, 007, 012, 018, 034/036 (handoff-gated jobs) | 2026-09-26 | 1,419 calls: get 1,213, list 141, cancel 33, invalid 32; registry is in-memory — no per-job disk records exist |
| Legacy `herdr_communicate` (prompt/steer/keys/interrupt/cancel) | src/tools/communicate.ts (384), src/messages/prompt.ts (547), prompt-target.ts (84), recipients.ts (184), failure.ts (44), limits.ts (31), src/agent-prompt.ts (153, protocol-22 socket) | 003, 004, 005, 013, 024, 030 | 2026-09-23 | 1,203 calls: prompt 539, steer 428, keys 157, interrupt 42, cancel 24; replaced per ADR-038 by `herdr agent prompt` CLI + handoff followups |
| Turn control protocol (cancel/interrupt identity-bound) | src/tools/turn-control.ts (971) | 014 | 2026-09-14 | exercised via communicate interrupt 42 + cancel 24; file itself imported only by tests |
| Legacy `herdr_wait` (detached wait jobs) | src/tools/wait.ts (1,333), src/wait-schema.ts (107), src/wait-target-evidence.ts (83) | 002, 012, 018 | 2026-09-21 | 967 `wait` calls |
| Wait-jobs Pi footer/widget UI | src/wait-jobs-ui.ts (140), src/tui.ts (170) | 007, 012 | 2026-08-31 | no disk evidence; dead in Pi — index.ts registers nothing (C7); retained only as createRuntime seam |
| Legacy `herdr_pane` (close/adopt/split/move/rename/focus/resize/swap/zoom) | src/tools/pane.ts (356), src/ownership.ts (54), src/pane-write-lock.ts (467) | 003, 028 | 2026-09-19 | 495 calls: close 451, adopt 19, split 12, move 3, rename 2, invalid 8; **zoom/swap/resize/focus 0** |
| Pane adoption | src/tools/pane.ts (adopt op), src/agent-identity.ts (371) | 028, 016 | 2026-09-22 | 19 adopt calls |
| Legacy `herdr_tab` | src/tools/tab.ts (218) | — (pre-ADR; tab grammar now owned by launch) | 2026-09-19 | 67 calls: create 36, close 29, focus 2, rename 0 |
| Attachments (tools-owned message-attachment store) | src/messages/store.ts (589) | 008-tools | 2026-09-02 | 9 attachment bodies in ~/.cache/herdr-tools/message-attachments (all in window) |
| Devin queue flush (cross-pane composer) | src/messages/devin-queue-flush.ts (396) | 029 | 2026-09-12 | no disk evidence (in-memory coordinator); still wired into daemon/runtime |
| Caller policy (worker/manager cooperation) | src/caller-policy.ts (301) | 030 | 2026-09-16 | no dedicated counter; reachable only via legacy communicate/inspect |
| Provenance envelope | src/provenance.ts (123) | 005 | 2026-09-13 | rides every launch/communicate payload (deterministic renderTask) — no separate counter |
| Availability / cooldowns / quota probing | src/availability.ts (656), src/supervision/claude-quota.ts (193), src/supervision/auth-json-credential-store.ts (176) | 006, 035 | 2026-09-24 | 26 cooldown events: anthropic 16, cognition 4, openai-codex 3, google 2, opencode-go 1 |
| Tool telemetry | src/telemetry.ts (341) | — | 2026-09-26 | 7,326 tools.jsonl lines across 26 project .herdr dirs (recording began 09-21) |
| Profiles & role plugins (manager/planner/promoter/researcher/reviewer/scout/worker + executor profile) | src/profiles/* (1,525 LOC incl. skill-bundles 563), herdr-profiles/ + skills/ (~10,658 LOC of plugin/skill files), herdr-skill-bundles.json | 006, 007-role, 008, 010, 011, 022, 023, 026 | 2026-09-24 | no per-role counter in state files — usage is implicit via compiled catalog points on every launch; catalog-scan specLabel decisions exist |
| MCP host (stdio adapter, Claude/plugin/executor serve) | src/mcp/adapter.ts (424), host.ts (342), run.ts (248), src/mcp-server.ts (6) | 009, 017, 025, 027, 038 | 2026-09-26 | serves every MCP tool call (all 7,326 telemetry lines were emitted through the shared instrumented surface — estimate: MCP path carried the large majority pre-cutover) |
| Close/reconcile | src/close.ts (157), src/daemon/reattach.ts (527) | 003, 038 | 2026-09-19 | pane close 451 calls (legacy surface) |
| Health/preflight CLI gating | src/health.ts (247), src/cli.ts (229) | — | 2026-09-22 | inspect:health 51 calls + per-call preflight on every tool invocation |
| Context rebinding / target resolution | src/context.ts (295), src/targets.ts (222), src/topology-schema.ts (158), src/mutations.ts (121) | 017 | 2026-09-23 | inspect:context 73 calls; target resolution rides every targeted call |
| Redaction | src/redaction.ts (97) | 018-failure-evidence | 2026-08-17 | passive — applied to all retained evidence |
| Worktree isolation | src/worktree.ts (420) | 035 | 2026-09-19 | .herdr/worktrees present (<project-a>: 22 nested worktree dirs); replicas feature itself unused |
| Transcript delta | src/transcript-delta.ts (30) | — | 2026-08-31 | internal, no user surface |
| Autoupdate (systemd user timer, fast-forward+build) | scripts/herdr-tools-autoupdate.sh (66), test-autoupdate.sh (130), deploy/systemd units (37) | — | 2026-09-24 | last-success log: 5 successful update entries 09-24→09-25 |
| Pi extension host assembly | index.ts (119), src/settings.ts (77) | 038 (C7) | 2026-09-26 | registers nothing in production; createRuntime kept as test/harness seam |
| Owner recovery tooling (outside extension: ~/.config/herdr/recovery scripts) | — (host scripts) | — | 2026-09-09 | rebuild/resume-all artifacts dated 09-09 — inside window, one-off |

## Ranked deletion candidates

1. **Legacy seven-tool surface & its private plumbing** — `src/tools/{wait,communicate,inspect,jobs,pane,tab,turn-control}.ts` (3,849 LOC) + `job-registry` (1,795) + `wait-schema`/`jobs-schema`/`wait-target-evidence`/`wait-jobs-ui`/`tui`/`job-notification`/`caller-policy` (~2,100) ≈ **~7,700 LOC, ~20% of src**. Imported only by tests + `launch.ts` internals; superseded by ADR-038's three-tool daemon surface. Caveat: still taking live calls (5,665 in window) from older installed deployments — delete only after the cutover completes everywhere.
2. **`agy` runtime** — 0 selectedPoints in 687 decisions while present in 608 chains; ADR-021/022 machinery (adapter rows, quota, trace handling) carries a full runner for zero picks.
3. **`replicas` option + worktree-replica machinery** — 0/592 admitted launches used replicas>1; `worktree.ts` (420 LOC) exists for it (nested `.herdr/worktrees` dirs are old replica/artifacts outputs).
4. **Pane ops zoom/swap/resize/focus, tab rename** — 0 and 0 calls in window inside the already-dead pane/tab surface.
5. **`devin-queue-flush`** (396 LOC, ADR-029) — in-memory only, no state evidence; its purpose (cross-pane composer flush) is subsumed by the mailbox/follow-up path; verify no live call before deleting.
6. **Legacy wait-job UI seam in `createRuntime`** (waitJobsUi/jobs wiring in index.ts) — dead in Pi (registers nothing), exists only for tests.
7. **Attachments store** (589 LOC) — only 9 bodies in window; functional but marginal. Keep if `herdr agent prompt` follow-up files aren't the replacement (they overlap: both move large bodies off argv).
8. **`recoveryOf`** — 8 uses in window: real but rare; keep the contract, cheap to retain (evidence derivation already in routing-policy).
9. **Legacy `herdr_inspect` op breadth** — `collection` (153), `context` (73), `health` (51) are secondary to `target` (2,227); if any compat surface survives, target-only covers ~89% of calls.

## Heavy-use features a rewrite must keep

- **Launch pipeline end-to-end** (launch schema → router/Jev → compiled catalog → supervision): 624 launch calls, 687 routing decisions, 592 admitted — the product's core.
- **Supervision review loop + evidence**: 744 Jev reviews over devin/claude/pi children; largest code area (~10.5k LOC) and actively exercised.
- **Universal disk handoff**: 1,915 handoff dirs in window, 805 `handoff.md` — the completion contract every run writes.
- **Mailbox + ack flow (new)**: 140 events in the first ~2 days of daemon life; the mandated ADR-038 replacement for communicate/wait.
- **Prompt/steer delivery** (legacy `communicate`: 1,203 calls) — its replacement (`herdr agent prompt` + handoff follow-ups) must cover this demand; do not drop the capability, only the tool name.
- **Detached wait + jobs polling** (1,934 combined calls) — replacement must preserve "wait without blocking the turn".
- **`inspect:target`** (2,227 calls) — highest-volume read; `herdr_status` is its successor and must stay cheap.
- **Telemetry + router/review logs** — the evidence files themselves (7,326 telemetry lines, 687 decisions, 744 reviews) are how this audit exists; retain or migrate.
- **Availability/cooldowns** — 26 provider-limit events recorded; feeds routing exclusions.
- **Profiles/catalog/skill bundles** — every launch consumes a compiled operating point; the catalog is load-bearing even where role plugins aren't individually counted.
- **Workload tabs** — always-on placement for admitted launches (grammar in launch.ts D12); labels ride the intent classifier.

## Evidence caveats

- Telemetry/diagnostics recording began ~2026-09-21 and router/review logs ~2026-09-19 — "in-window" counts for tools/decisions/reviews effectively cover only the tail of the window; earlier usage leaves no record in these files.
- `job-registry` and `devin-queue-flush` keep no durable per-job records — job counts come from tool-call telemetry only.
- No per-role profile counter exists in state; role-plugin usage is inferred through compiled catalog points, not measured directly.
- Mailbox/intents reflect only the new daemon (first events ~09-26); the pre-daemon era's equivalent flow is the 1,915 handoff dirs + legacy tool calls.
- `~/.config/herdr/herdr-client.log` is only 25 lines (rotated/low-volume) — `herdr agent prompt` CLI usage is undercounted by design; evidence is structural (README runbook) not numeric.

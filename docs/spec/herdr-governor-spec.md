# herdr-governor implementation spec

**Status:** Approved for Implementation (Draft 2, approved by the owner on 2026-09-27). This draft addresses every finding of the [Oracle Pro review of Draft 1](../reviews/2026-09-27-oracle-pro-spec-review.md), which returned *Rework*. How each finding was handled is recorded in §19. The owner chose to skip a second review round, so the Phase 2 contract gate and the Phase 3 and Phase 5 test suites verify these fixes instead.
**Decisions:**
- [ADR-0001](../adr/0001-clean-break-harness-agnostic-rust-governor.md)
- [ADR-0002](../adr/0002-transcript-parsers-are-the-only-per-harness-code.md)
- [ADR-0003](../adr/0003-recovery-waits-for-proof-the-predecessor-stopped.md)
- [ADR-0004](../adr/0004-native-per-session-stdio-relay.md)
- glossary: [CONTEXT.md](../../CONTEXT.md)
- Decisions and rationale: `docs/adr/` and §19

**Evidence:**
- [feature inventory](../research/2026-09-27-feature-inventory.md)
- [failure and performance baseline](../research/2026-09-27-failure-perf-baseline.md)
- [ADR harvest](../research/2026-09-27-adr-harvest.md). `H#n` cites the harvest's invariant inventory; Appendix A disposes of each one normatively.

## 1. Context and Problem Statement

herdr-tools is the TypeScript runtime that launches coding agents into Herdr panes, routes them with Jev, and supervises them. Measured on 2026-09-27:

**Size**
- About 40k lines of source and 53k lines of tests.
- About 5.5k source lines (16 files) are unreachable from the three live entrypoints but still tested.
- 38 ADR files, layered with partial supersessions.
- A SPEC.md that still describes seven retired tools.

**Guardrails**
- 154 `c8 ignore` pragmas hold up the 100% coverage figure.
- CI runs neither lint nor coverage.
- No path is protected against agent edits.
- 36% of commits in the last month were fixes.

**Recorded outcomes, 2026-09-19 to 09-27.** These are observations, not causes. The baseline cannot tell child death, a lost artifact and a missed lifecycle event apart, and the durable daemon had only about one day of history.
- **Stale runs:** 54% of 1,912 handoff records had stale, unsettled state after 24 hours.
- **Indeterminate delivery:** 8.2% of prompts, 28.6% of interrupts and 79.2% of cancels ended with an indeterminate effect.
- **Indecisive supervision:** 31% of supervision verdicts were `unknown`.
- **Unread mailbox:** 38% of mailbox events were unread at collection time.
- **Routing:** 13.8% of routing decisions were not admitted. 49% of Tasks started at `strong` and 12.5% at `frontier` or `max`, against about a third at `strong` in blind gold labels.
- **Resources:** 31 per-caller MCP host processes held about 1.6 GB of RAM. That cost comes from the serving topology, not the language.

The owner chose a clean-break rewrite (ADR-0001). The main structural gain is removing the coupling between a caller's lifetime and its Runs, and making every state transition a single database transaction.

## 2. Goals and Non-Goals

**Goals.** When goals conflict, the earlier one wins.

1. **Reliable.** Every Run settles exactly once, with a truthful settlement. No effect is dispatched twice. Every effect's certainty is known or reported as unconfirmed.
2. **Agent-safe.** "Make the checks pass" can only mean doing the work.
3. **Owner-comprehensible.** One spec, one glossary, three tools, and two crates with an enforced dependency direction.
4. **Fast.** A non-regression guard against the baseline, never a design driver.

**Non-Goals**
- Compatibility with herdr-tools' contract or its state.
- Harness support through governor code (ADR-0002 is the one exception).
- Replicas, an attachment store, pane or tab tools, turn control, pane adoption, caller policy, wait or jobs tools, and Pi UI.
- Distributing roles and skills; they move to `Gabrielgvl/agent-skills`.
- Cryptographic identity: callers are cooperative processes of the same user.
- Automatic recovery before the predecessor has stopped (ADR-0003).

## 3. Scope and Out of Scope

**In scope**

- **The new repo `herdr-governor`:**
  - the daemon and its MCP surface;
  - `governor-core`;
  - the adapters;
  - the kit guardrails, extended as described in §10 Phase 1;
  - the installer and deploy timer;
  - one caller skill;
  - the contract, qualification and conformance suites.
- **herdr-tools:** remove replicas and the attachment store, and capture fixtures.
- **Cutover:**
  - a parallel `herdr-next`;
  - the swap, which drains the old daemon rather than abandoning it;
  - a 14-day rollback window;
  - archiving the old repo;
  - moving roles and skills to `agent-skills`.

**Out of scope**
- Changing Herdr or Executor. Upstream discussions are opened only with owner approval.
- Migrating old `.herdr/` state.
- Re-running the Jev tier calibration kit.

## 4. Users and Impact

- **Callers** (Claude, Devin and Pi manager sessions) use three MCP tools through a per-session stdio relay their own harness spawns (ADR-0004). Caller skills move to the new contract at the swap.
- **Children** receive a Task and follow-ups wrapped in the provenance envelope, and write a Handoff. They need no governor tools.
- **The owner:**
  - authors the local catalog and policy;
  - approves hard-path PRs;
  - registers the governor's tools per harness (ADR-0004) and the legacy integration in Executor;
  - decides each Herdr gap case by case;
  - disposes of any obligation left over after draining.
- **Resources:** the 31 per-caller MCP hosts are removed; one daemon remains, plus one stateless relay per caller session at most 8 MB RSS (ADR-0004).

## 5. Assumptions and Constraints

**Assumptions.** Each is confirmed or escalated in Phase 2, before any core code exists. A failed assumption becomes a case-by-case owner decision.

- **A1. Caller transport.** Each caller harness's native MCP spawns `herdr-governor relay` over stdio, once per session at session start, registered globally through the herdr-tools profile layer (ADR-0004). The relay is stateless and forwards to the daemon's 0600 unix socket; there is no Executor hop, no HTTP listener and no bearer. The A1′ evidence confirmed all three harnesses inherit `HERDR_*` and the invocation cwd, spawn one process per session, respawn it after a crash, and end it on stdin EOF or SIGINT.
- **A2. Socket concurrency.** Herdr's socket supports one subscription connection alongside concurrent short-lived request connections, and it behaves cleanly on disconnect, reconnect, malformed frames and timeouts.
- **A3. Start and prompt semantics.** `agent.start` returns only once the agent is ready. A failed start returns a typed pre-interactive error, with the pane back at its shell. `agent.prompt` acknowledgements identify their target. Phase 2 covers every enabled harness, AGY included.
- **A4. Herdr incarnation.** A Herdr server incarnation can be proven from protocol 22, for example a server identity or start marker that Phase 2 finds. Phase 2 must not invent a protocol field. If no proof exists, every server discontinuity invalidates bare terminal IDs, and Runs re-prove their identity by native session (F11).
- **A5. Transcripts.**
  - Pi's path comes from Herdr.
  - Devin's transcript is `$XDG_DATA_HOME/devin/cli/transcripts/<session_id>.json` (ATIF v1).
  - Claude's is `~/.claude/projects/*/<session_id>.jsonl`.
  - Phase 2 checks partial writes, ambiguous matches and unreadable sources.
- **A6. Pane tagging.** A pane the governor creates can be recognized after a crash, for example by a name or metadata set atomically at creation. If it can't, an interrupted topology effect is reported as unconfirmed and never adopted.

**Constraints**
- **Toolchain:** Rust 1.98.1, pinned, with `rust-version` equal to the pin and edition 2024. Kit defaults otherwise.
- **Herdr:** 0.9.1 or later, protocol 22, failing closed on a mismatch.
- **Jev:** TypeSafe `systemOne` over HTTPS with Bearer auth. The key lives in the governor's own 0600 credential file.
- **Public repo:** nothing host-specific is committed (home paths, hostnames, tokens). Research files are scrubbed before the first push.
- **Harness code:** no per-harness code outside `adapters::transcript` (ADR-0002).

## 6. Functional Requirements

### 6.1 Identity and ownership

- **F1 Caller identity.** `caller` is not a tool argument: the relay attaches a caller envelope `{paneId, projectRoot, relayInstanceId}` to every forwarded request, as part of the relay-to-daemon framing, so the strict tool schemas are unaffected (ADR-0004). `paneId` comes from the inherited `HERDR_PANE_ID`, `projectRoot` is realpath(`git rev-parse --show-toplevel`) of the relay's cwd, or realpath(cwd) outside a git worktree, and `relayInstanceId` is an immutable random 128-bit id the relay mints once at process start — never persisted by the relay, never configurable. The id is not upstream session state: the relay still reconnects per request and holds nothing mutable.
  - **Caller key:** `(agent kind, native session)`, resolved from one fresh `session.snapshot` that must contain exactly one pane with that ID, whose occupant must have a native session. The first call registers the caller; the idempotency scope stays `(caller key, projectRoot)`.
  - **Binding:** the first request carrying a new `relayInstanceId` resolves the caller key this way and persists the binding `relayInstanceId` → caller key. Every later request with that id must resolve, through a fresh locator check, to the same native session — a replaced occupant in the same pane (`native_session` changed, `pane_id` and `terminal_id` unchanged; `a4_native_new_replaces_session`) is refused instead of silently re-registered. A respawned relay mints a new id and binds afresh; a daemon restart keeps the persisted bindings.
  - **Refusals:** a malformed caller envelope is refused `CALLER_IDENTITY_INVALID`; a missing, duplicate or sessionless occupant, or a bound `relayInstanceId` re-resolving to a different native session, is refused `CALLER_IDENTITY_*` — the last case specifically `CALLER_IDENTITY_MISMATCH` — before any effect (H#21).
  - **`projectRoot`:** the daemon still requires it absolute, single-line, not `/`, existing and realpath-canonical. A relay-derived root that is invalid is refused `CALLER_IDENTITY_INVALID`, never re-anchored (H#3).
  - **Trust:** identity is cooperative between processes of the same user, and the docs say so (H#22).
- **F2 Child identity.** A child's identity has these parts:
  - `herdr_incarnation`, `terminal_id`, `agent_kind`, and `agent_name` (minted as `gov-<runId[0..8]>`, H#52);
  - `native_session`, once Herdr reports it.
  - The pane ID is only a current locator.
  - Before the first prompt, the first four parts are enough, for every harness.
- **F3 Observation classes.** A target-local read of a fresh snapshot is one of:
  - `unique`: exactly one pane matches the identity, wherever it is now. A move is followed, never treated as a loss (H#75).
  - `absent`: the snapshot is valid and no pane matches.
  - `invalid`: the snapshot is malformed, duplicated, unavailable, or ambiguous about the incarnation.
  - `invalid` never counts as absence and never settles a Run (H#74).
  - A new `native_session` or `terminal_id` in the Run's pane means the Run is `absent` from that pane, and that pane is someone else's.
- **F4 Ownership.** Every Run and mailbox operation requires the caller to be the current owner.
  - `observe`, `message`, `ack` and `cancel` refuse `NOT_OWNER` otherwise.
  - `handover` and `adopt` change the owner and bump `owner_generation` atomically.
  - A Run can never become its own caller — the attempt is refused `CALLER_IS_RUN` (H#24).

### 6.2 Tools (exactly three)

Strict schemas apply to every tool and every action: unknown fields are refused, with no aliases. Results are at most 60,000 bytes (H#103–104). Listings paginate with an opaque cursor; nothing is ever evicted. Caller identity is never a tool field; the relay derives and attaches it (F1).

- **F5 `herdr_launch {task, idempotencyKey}`.**
  - **Task fields:**
    - `objective` and `scope` are required;
    - `doneWhen` has 1 to 8 items;
    - `constraints` is optional, 0 to 8 items;
    - `tier`, `recoveryOf`, `label` and `cwd` are optional.
  - **`cwd`:** must be realpath-canonical and resolve inside `projectRoot`.
  - **Size:** the rendered Task is at most 64 KiB (H#40).
  - **`label`:** presentation only. It never enters Jev input or routing (H#41).
  - **Outcomes:**
    - `pending {launchId, runId?}` while another execution of the same Launch is in progress;
    - `launched {runId, operatingPointId, requestedOperatingPointId?, tier evidence}`;
    - `abstained {reason}` with zero effects;
    - `rejected` when doneWhen is not verifiable;
    - `failed {effectCertainty, runId?, createdTopology}` (H#55–56).
- **F6 `herdr_run`**, with these actions:
  - `observe {runId}`: state, settlement, handoff digest and path, per-item acceptance, and outbox entries (paginated).
  - `message {runId, messageKey, text}`: see F17.
  - `ack {eventId}`: idempotent.
  - `handover {runIds, successorPaneId}`: the current owner must be live, and the successor must be verified by F1.
  - `adopt {runIds}`: see F19.
  - `cancel {runId, closePane?}`: see F20.
- **F7 `herdr_status {eventId?, cursor?}`:**
  - health: daemon, Herdr connection freshness, config validity and the time of the last good config;
  - the caller's unsettled Runs and pending recoveries;
  - unread event IDs (paginated);
  - active cooldowns;
  - with `eventId`, one event body.

### 6.3 Effects (the one protocol for every external mutation)

- **F8 Effect journal.** Every Herdr mutation (tab create, pane split, agent start, prompt, close) and the launch-time Jev evaluation gets an `effects` row with a unique `effect_key`. The row records the operation, subject, captured target identity, payload digest and state.
  - **Dispatch:** commit `planned` → `dispatching`, invoke, then commit `acknowledged` or `failed` with its certainty.
  - **Restart:**
    - `dispatching` without a receipt becomes `unconfirmed` and is never dispatched again;
    - `planned` may be dispatched.
  - **What a snapshot proves:** it can establish identity or liveness. It never establishes that a prompt was, or was not, submitted.
  - **Transactions:** no SQLite transaction ever spans Herdr or Jev I/O.
- **F9 Per-target serialization.** All prompts to one captured identity (Task, follow-ups, nudges, hints) are dispatched one at a time, in order.
  - An `unconfirmed` prompt is an ordering barrier for that target. Transcript evidence can resolve it: the parser finds the envelope's delivery ID. Otherwise the Run's settlement ends it.
- **F10 Fresh verification.** Before every prompt and every close, including closing a settled Run's pane:
  - the daemon takes a fresh snapshot, and the target must be `unique` with the full captured identity;
  - the target must be in a state that is safe to write to (F17);
  - the remaining check-then-send race is documented, not hidden: Herdr has no conditional send (H#14).

### 6.4 Launch

- **F11 Admission.**
  - **Validate:** the Task (F5) and the caller (F1).
  - **Idempotency, scoped to `(caller, projectRoot)`:**
    - an existing Launch with the same digest returns its stored result, or `pending`;
    - a different digest returns `IDEMPOTENCY_KEY_CONFLICT` (H#90).
  - **Recording:** the Launch is recorded before anything else.
  - **Retention:** Launch rows and keys are kept indefinitely. Nothing garbage-collects a key back into use.
- **F12 Evaluation.** One `jev_evaluate` effect makes one `systemOne` request.
  - **Questions:**
    - `done_when_verifiable` (noul);
    - `weakest_sufficient_tier` (a choice over the policy tiers);
    - `changes_files` (none, few or broad);
    - `security_boundary`, `needs_external` and `long_running` (noul);
    - `related_tab` (a choice over the caller's open governor tabs plus `new`), asked only when such tabs exist.
  - **Failures become abstentions with zero effects:** transport, auth, HTTP errors, a malformed response, or a request over 96 KiB.
  - **Restart:** an evaluation left `dispatching` on restart abstains `interrupted_before_decision`. It is never evaluated twice.
  - Jev never sees operating points.
- **F13 Routing: one ordered function, persisted before any topology effect.** The steps run in this order:
  1. Validate the judgments.
  2. Apply the policy adjustments: a Task with no file changes and no security boundary caps the tier; a security boundary raises the floor; broad changes raise the floor.
  3. Apply the caller's uplift: at most one tier above the floor, never lower (H#49).
  4. Apply the recovery minimum and exclusions (F21): at least one tier above the predecessor's start, and none of the predecessor's provider's operating points. If that is impossible at the top tier, abstain `no_higher_tier`.
  5. Apply exploration. It applies only when the Task changes no files, touches no security boundary, is not a recovery, and `sha256(caller ‖ idempotencyKey)` falls below the policy rate (default 5%). It lowers the start by one tier, but never below the recovery minimum or the lowest tier.
  6. Select candidates:
     - a candidate is at or above the start tier, offers every required capability with a current qualification (F26), and belongs to a provider that is not cooling down;
     - candidates are ordered by cost class, then catalog order;
     - no candidates means abstain.
  - **Persisted, immutably:** the decision (every floor, the requested tier, the exploration assignment, the ordered candidates with their exact arguments) and the config version.
  - **In the same transaction:** a Run is reserved (state `reserved`, `max_age_deadline` fixed). The supervision obligation exists from this moment (H#36, H#44).
- **F14 Placement.**
  - The pane goes into the tab Jev picked if it holds fewer than four panes; otherwise into a new tab.
  - Right split, without focus (H#53).
  - The initial pane of a new tab is used, not orphaned (H#102).
  - Panes are tagged per A6.
- **F15 Start.** For each candidate in order:
  - recheck its availability and cooldown immediately beforehand (H#46);
  - dispatch `agent.start {kind, args from the persisted decision}` as an effect;
  - on a typed pre-interactive failure, with the pane observed back at its shell, move to the next candidate in the same pane;
  - on any other outcome, or an `unconfirmed` start, stop falling back, record `failed {effectCertainty}`, and leave the Run for the transition rules to settle (H#45);
  - on success, capture F2 identity parts 1–4 into the Run;
  - once one start succeeds, no later launch failure releases the Run's supervision (H#36);
  - `requestedOperatingPointId` is reported when fallback changed the point.
- **F16 Prompt.** One prompt effect carrying the Task, wrapped in the provenance envelope (H#25–27), plus the handoff instructions: the path and the end marker.
  - An acknowledgement must match the captured identity (H#30).
  - An acknowledged prompt sets `prompt_certainty = acknowledged`.
  - A stalled, timed-out or unconfirmed prompt sets `prompt_certainty = unconfirmed`, meaning possibly consumed: no resubmission, relaunch or cleanup; supervision continues; the caller is notified (H#29, H#31, H#34).
  - Either way the Run becomes `active`.

### 6.5 Delivery

- **F17 Follow-ups.** `message {runId, messageKey, text}` has these rules:
  - **Keys:** `messageKey` is unique within the Run. The same body digest returns the existing sequence number; a different digest returns `MESSAGE_KEY_CONFLICT`.
  - **After settlement:** refused `RUN_SETTLED`, with nothing enqueued.
  - **Large bodies:** a body over 16 KiB, up to 1 MiB, is first published as an immutable 0600 file (write, fsync, rename, verify size and digest). A failed publication enqueues nothing (H#62–64).
  - **States:** `queued` → `dispatching` → `submitted` or `unconfirmed`.
  - **When it's sent:** at the first safe moment. Immediately if the operating point has a qualified `mid_turn_input` capability; otherwise when Herdr reports the child idle or done. Never while the child is `blocked` (H#17).
  - **Expiry:** only a message that was never dispatched can become `expired`, with a reason. A message in `dispatching`, `submitted` or `unconfirmed` stays visible in that state.
  - **File retention:** files referenced by live or possibly consumed messages are kept until 7 days after the child's identity is observed absent (H#64).
- **F18 Mailbox and hints.**
  - **Contents:** actionable events only, each with a stable `dedup_key`, so repeated observations never create copies:
    - handoff accepted or rejected;
    - settled (any settlement);
    - stalled after the nudge;
    - blocked on input;
    - outside scope;
    - launch failed;
    - prompt or follow-up unconfirmed;
    - follow-up expired;
    - cooldown hit;
    - a recovery pending, blocked or dispatched.
  - **Destination:** always the Run's current owner, or for launch-only events the Launch's caller, derived when read. Adoption therefore redirects unread events automatically (H#84, H#91).
  - **Hints:** after an event commits, one hint prompt effect goes to the owner's pane.
    - Conditions: the pane is fresh and `unique`, idle or done, and still holds the owner's native session, and its harness has a qualified `hint_consumption` capability.
    - Limits: at most one per 5 s per owner, never retried, never sent to a busy pane (H#85–87).
- **F19 Adoption.**
  - **`adopt {runIds}`** requires a fresh snapshot to show the previous owner's native session gone; while it is still present the request is refused `ADOPT_OWNER_LIVE`.
  - **What can be adopted:**
    - an unsettled Run;
    - a settled Run with unread events or a pending recovery, adopted only for those.
  - **Limits:** adoption never reopens a settled Run, never re-keys the Launch's idempotency binding, and never creates a recovery opportunity by itself.

### 6.6 Lifecycle and settlement

- **F20 Settlement.** It is first-commit-wins and immutable.
  - **The update:** conditional on `settlement IS NULL AND version = :v`. In the same transaction it inserts the terminal event, expires queued follow-ups that were never dispatched, and records any recovery obligation and cooldown.
  - **Losing transitions:** they commit nothing and cause no effect.
  - **Async results:** every Jev result, observation and deadline carries the `(version, work_generation, evidence_generation)` it was requested against. It applies only if those still hold.
  - **`cancel {runId, closePane?}`:** settles an unsettled Run `cancelled`. With `closePane`, it dispatches a verified close effect (F10) and reports whether it was confirmed. On a settled Run it only closes the pane.
  - **Settlements:** `accepted`, `rejected`, `no_handoff`, `pane_lost`, `cancelled`, `provider_limited`, `unresolved(reason)`.
  - **Panes:** never closed automatically.
- **F21 Recovery (ADR-0003).**
  - **Trigger:** when Herdr reports a Run `blocked`, and Jev's `provider_limited` answer clears the policy threshold, the Run settles `provider_limited`.
  - **In the same transaction:**
    - every operating point of that provider enters cooldown, and cooldowns only ever lengthen;
    - a unique recovery obligation is recorded as `pending`;
    - an event tells the owner that closing the pane (`cancel` with `closePane`) triggers recovery.
  - **Dispatch** happens once a fresh snapshot shows the predecessor's identity `absent`. It creates a Launch keyed `recovery:<predecessorRunId>` that carries the predecessor's Task plus a preamble: continue from the observed git and transcript state, and don't repeat side effects that already happened.
  - **Status:** `pending` → `dispatched` (with successor references and its certainty), `blocked` (abstained, no candidates), or `failed`. An obligation still pending after the policy expiry (default 24 h) fails `expired`.
  - **Caller-requested recovery (`recoveryOf`):**
    - it requires the predecessor to be settled — an unsettled predecessor is refused `RECOVERY_PREDECESSOR_UNSETTLED`;
    - it claims the predecessor's obligation if one exists; a second recovery of the same predecessor is refused `RECOVERY_EXISTS`;
    - a `provider_limited` predecessor must be observed `absent` first, and any other predecessor must be observed idle, done or absent — while the observation gate is unmet the request is refused `RECOVERY_PREDECESSOR_ACTIVE`, retryable once the gate is met.
- **F22 Transitions.** Appendix C defines them as a total function: every state against every event, where the events are observation classes, child status, handoff, judgment, deadline, cancel, provider limit and restart. The core implements it as exhaustive `match` expressions without wildcard arms, so the compiler enforces totality under the kit's `wildcard_enum_match_arm = deny`.
  - **Deadlines:** stored as absolute times. They are not reset by repeated observations or restarts, and they are not suspended when paid review pauses. Every Run has `max_age_deadline` (default 24 h, set per policy).
  - **Liveness assumption:** the daemon runs eventually and storage is writable.
- **F23 Supervision.** Evidence is:
  - the transcript window from the parser (bounded, and the parser reads at most a configured ceiling), otherwise `agent.read`;
  - the git state against the base pinned before any effect (H#82);
  - the Task digest (`objective`, `doneWhen`, `constraints`).

  Questions that drive actions:
  - `blocked_on_input` → event.
  - `no_recent_progress` → one nudge per episode. An episode ends when the child works again, and stall and idle share the same episode.
  - `provider_limited` (asked when Herdr reports blocked) → F21.
  - `outside_scope`, which also receives `scope` → event.

  Other rules:
  - Periodic progress reviews pause while the owner's session is absent. Acceptance judgments and deadlines never pause (H#81).
  - Unchanged evidence (same `evidence_generation`) is never re-asked after a completed review (H#79).
  - The policy grants no permissions; `readOnly` and similar are code-owned (H#80).
- **F24 Handoff and acceptance.**
  - **Reading:** the handoff is read without following symlinks. It must be a regular file of at most 256 KiB, and its final non-whitespace content must be `<!-- herdr-governor handoff run=<runId> -->`. Anything else counts as not written yet.
  - **Freezing:** the bytes are copied and digested, as `handoffs (run_id, work_generation, digest)`.
  - **Judging:** Jev judges each doneWhen item (`handoff_meets_item_k`) against the frozen handoff plus the transcript and git evidence, so claims of execution are checked against execution. Each assessment is bound to the Task digest, handoff digest, work generation, question version and policy version. An unchanged digest is never re-judged after a completed assessment.
  - **All items met** → `accepted`.
  - **Otherwise** → state `repair`.
    - `repair_deadline` is 15 minutes after the first rejection in that work generation. Rewrites, re-rejections and restarts don't extend it.
    - A repair follow-up dispatched before the deadline starts a new work generation and invalidates acceptance work from the old one. It doesn't reset `max_age_deadline`.
    - When the deadline passes with no qualifying repair, the Run settles `rejected`.
  - **Jev unavailable** until `judgment_deadline` (default 30 minutes after freezing) → `unresolved(judgment_unavailable)`.
  - **Late artifacts** never reopen a settled Run.
- **F25 Idle and loss.**
  - **Idle without a handoff:** a child idle or done with no valid handoff gets the episode's nudge. `idle_deadline` is 15 minutes after the idle episode began. When it passes, the Run settles `no_handoff`.
  - **Pane gone:** an `absent` child with no frozen handoff settles `pane_lost`. With a frozen handoff that hasn't been judged, it goes to judgment first.
  - **Max age:** after `max_age_deadline`, the Run settles `unresolved` with the specific reason. The governor never invents an accepted or rejected verdict.

### 6.7 Configuration, qualification and restart

- **F26 Qualification.** `herdr-governor qualify <operatingPointId>` runs a small canned Task through the real harness. It records, keyed by `(operating point, args digest)`, a pass or fail for each of these:
  - start;
  - prompt acknowledgement;
  - `handoff_write` (it can write the handoff path);
  - `followup_read`;
  - `mid_turn_input`;
  - `hint_consumption`;
  - each routing capability it claims.

  How the records are used:
  - Routing and delivery use only capabilities with a current pass. Changing a point's args invalidates its qualification.
  - AGY is routable only after it qualifies.
  - This replaces unverified tags (H#110), as catalog data, never as harness code.
- **F27 Config.**
  - **Files:** `~/.config/herdr-governor/catalog.toml` holds the catalog and routing policy. `~/.config/herdr-governor/credentials` holds the Jev key, mode 0600.
  - **Parsing:** the config adapter decodes the TOML, and `governor-core` validates the typed values.
  - **Reload** happens on SIGHUP.
    - An invalid reload keeps the last good config and shows it in `herdr_status`.
    - An invalid config at startup refuses to start, with one sanitized stderr line of at most 500 characters (H#106).
  - **Decisions don't reload:** a Launch keeps the config version and args recorded in its decision.
- **F28 Restart.**
  1. Take the instance lock and probe for an existing daemon (H#4).
  2. Open the store: `foreign_keys=ON` (verified), WAL, `synchronous=FULL`.
  3. Move every `dispatching` effect to `unconfirmed` (F8).
  4. Take a fresh snapshot, and classify every unsettled Run and every pending recovery (F3).
  5. Apply the transitions for the elapsed deadlines.
  6. Only then bind the MCP listener (H#5).

  A Herdr incarnation change without proof (A4) marks bare terminal IDs untrusted:
  - Runs that have a `native_session` re-prove their identity by it;
  - Runs without one settle `unresolved(identity_unprovable)`.
- **F29 Shutdown**, in this fixed order (H#8):
  1. Stop admission.
  2. Finish in-flight effect receipts, within a bounded wait.
  3. Stop the event loop.
  4. Close the listener.
  5. Release the lock.
  6. Make no Herdr writes during shutdown.

## 7. Non-Functional Requirements

- **N1 Exactly-once effect dispatch.** No effect key is dispatched twice, enforced by the unique `effect_key` plus the F8 protocol. There are no duplicate Launches per key.
- **N2 Settlement.** Every Run settles no later than `max_age_deadline` plus one reconcile interval (30 s), given the F22 liveness assumption.
- **N3 Launch latency.**
  - **Measured:** from MCP request receipt to the `herdr_launch` response, on real harnesses during Phase 7, compared with baseline measure A (median 5.7 s, p95 16.3 s).
  - **Target:** no regression.
  - Timings against the fake Herdr measure only the governor's own overhead.
- **N4 Memory.** Daemon RSS stays at or under 176 MB. One stateless relay per caller session is allowed, at or under 8 MB RSS each (measured 2 MB); a relay holds no state or caches (ADR-0004).
- **N5 Bounds.** Every boundary has a size limit, and truncation happens only where this table says so.

  | Boundary | Limit | Rule |
  |---|---|---|
  | Rendered Task | 64 KiB | refuse |
  | Jev request | 96 KiB | abstain |
  | Inline follow-up | 16 KiB | publish as a file |
  | Follow-up file | 1 MiB | refuse |
  | Handoff | 256 KiB | treat as not written |
  | Transcript window | 32 KiB | take the tail, deterministically; the parser reads at most 16 MiB |
  | Socket frame | 1 MiB | treat the observation as `invalid` |
  | MCP result | 60,000 bytes | paginate |

  Authoritative fields (the Task digest, doneWhen) are never truncated.
- **N6 Crash safety.** SIGKILL at any statement boundary leaves the store consistent, and F28 converges. Power loss loses no committed effect marker (`synchronous=FULL`).
- **N7 Single instance and failing closed.** One daemon per user. When it isn't running, callers get `DAEMON_UNAVAILABLE`, with no fallback (H#6).
- **N8 Harness-agnostic behaviour**, enforced two ways:
  - a lexical tripwire: harness-kind literals are forbidden outside `adapters::transcript`;
  - a metamorphic test: renaming opaque harness and operating-point IDs while keeping their declared capabilities changes no non-transcript behaviour.

## 8. Current State

The TypeScript daemon (ADR-038) is live under systemd:
- Executor serves its three tools through a stdio proxy.
- Launch, routing and supervision all run through one 965-line `createLaunchTool`, which has had 89 commits.
- Pi registers nothing.
- About 5.7k legacy tool calls came from older deployments.

Appendix A gives the normative disposition of every harvested invariant. Nothing is carried over implicitly.

## 9. Proposed Approach

### Minimality check

- **Required:**
  - a durable process that owns Launches and Runs independent of callers;
  - an effect journal: without one, crash-interrupted prompts are either duplicated or abandoned (Oracle B1);
  - total, transactional settlement;
  - an idempotent, ordered outbox;
  - incarnation-aware identity;
  - Jev routing and supervision;
  - qualification-backed capabilities.
- **Removed:**
  - replicas, attachments, the seven legacy tools, turn control, adoption heuristics, caller policy;
  - the Devin queue flush, the watermark taxonomy, the provisional AGY path;
  - the governor's own readiness pollers;
  - three separate launch logs;
  - per-caller MCP hosts;
  - roles and skills, and a separate authoritative outcomes table (it becomes a view).
- **Rejected as over-built:** a general workflow engine, a plugin system, a crate per adapter, distributed leases, and compatibility layers. Effect rows cover only Herdr mutations and the launch evaluation.

### Architecture

Two crates. The split enforces the dependency direction, not where business rules are placed; placement is enforced as described below.

- **`governor-core`** (no I/O):
  - `task` (validation, rendering, the envelope);
  - `identity` (F2–F3);
  - `routing` (F13);
  - `lifecycle` (the Appendix C transition function, deadlines, settlement);
  - `delivery` (outbox eligibility, the per-target queue, the mailbox and hint rules);
  - `recovery` (F21);
  - `acceptance` (F24);
  - `config` (typed validation).

  Inputs are values, including time and entropy. Outputs are `Transition` values: state changes, events, and effects to plan. Allowed dependencies: `serde`, `thiserror`, `sha2`.
- **`herdr-governor`** (the binary):
  - `store`: SQLite. Its public API is `apply(Transition)` plus read queries. No lifecycle setter exists outside `store::transitions`, which a tripwire test checks.
  - `adapters::{herdr, jev, transcript, git, config}`.
  - `mcp`: the daemon's MCP endpoint, served on its 0600 unix socket; the per-session relays forward to it (ADR-0004).
  - `daemon`: one coordinator task owns every transition. I/O runs asynchronously and returns versioned results to the coordinator.
- **Subcommands:** `daemon`, `check-config`, `qualify`, and `relay` — the per-session stdio transport (ADR-0004).

### Data model

Appendix B holds the executable DDL, including constraints, triggers and the `outcomes` view. It also defines each transaction's contents.

### Starting dependency set (`Cargo.lock` is a hard path; approved once)

- **Runtime:** `tokio`, `serde`, `serde_json`, `toml`, `rusqlite` (bundled), `reqwest` (rustls), `rmcp` (server; the daemon's unix-socket endpoint and the relay's stdio transport), `schemars`, `thiserror`, `tracing`, `tracing-subscriber`, `uuid` (v7), `sha2`.
- **Dev:** `proptest`, `tempfile`.
- No CLI-parsing crate.

## 10. Implementation Phases

Phases 0 and 1 run in parallel. Every later phase starts only after the previous phase's DoD holds on a merged `main` commit.

### Phase 0 — Prepare the old repo

- **Objective:** cut the live features the owner removed, and capture the fixtures.
- **Scope:** herdr-tools only, on a branch in a separate worktree, never in the installed checkout.
- **Changes:**
  - Remove `replicas` and the attachment store, with their tests.
  - Capture Jev request/response pairs for each question type from `@typesafe-ai/sdk`.
  - Save `herdr api schema --json`.
  - Take one scrubbed transcript each from Devin, Claude and Pi.
- **Dependencies:** none.
- **Risks:** hidden callers. Telemetry shows 0 replica uses and 9 attachment bodies.
- **Validations:**
  - `npm run typecheck && npx eslint . && npx vitest run` pass.
  - `herdr_status` still reports the TypeScript daemon running after autoupdate.
- **DoD:** the PR is merged and deployed, and the fixtures are handed to Phase 2.

### Phase 1 — Scaffold and guardrails

- **Objective:** `just ci` is the definition of done, and cheating fails.
- **Scope:** kit adoption for a two-crate workspace.
- **Changes:**
  - **Repo and docs:** create the public `Gabrielgvl/herdr-governor` after scrubbing host paths from `docs/research/`, then push the docs.
  - **Kit files:** toolchain, workspace lints (each member declares `[lints] workspace = true`), kit configs, `justfile`, gates, protected paths, harness configs, hooks, CI, CODEOWNERS, required checks, branch protection, and grouped weekly Dependabot.
  - **Protected paths**, beyond the kit's: `tests/support/**`, proptest strategies, `tests/fixtures/**`, `.config/nextest.toml`, `.cargo/mutants.toml`, workspace `Cargo.toml` membership and features, and `build.rs`.
  - **New gates:**
    - a test-inventory ratchet: `cargo nextest list` at BASE against HEAD, fails on any missing, filtered or ignored required test;
    - `mutants-diff`: fails on an unexpected zero-mutant run when core source changed, and runs full mutation on `governor-core` for test-only changes;
    - core purity: a dependency-graph check plus a crate-root check;
    - the harness-literal tripwire;
    - a check that `[lints] workspace = true` is present in every member;
    - the Herdr schema fixture check.
  - **Trusted enforcement:** CI runs gate scripts and BASE inventories checked out from the base revision, not the PR's copies. `owner-approved` is removed automatically on every new push, and the gate accepts the label only if it was applied after the head commit.
  - **Seeded cheats** in `test_guardrails.py`:
    - a deleted, emptied or moved test, or a removed `mod` line;
    - a reasonless `#[expect]`;
    - a snapshot self-accept;
    - a lockfile changed without its manifest;
    - a weakened assertion helper, a narrowed generator, or a same-name no-op test;
    - an adapter bypass (a lifecycle write outside `store::transitions`);
    - release-only behaviour (`cfg(not(test))`);
    - altered workspace membership;
    - approval followed by another commit.
- **Dependencies:** the owner creates, or approves creating, the repo.
- **Risks:** over-denial (lessons §G). The owner label is the escape hatch.
- **Validations:**
  - `just ci` exits 0 on the skeleton and on a fresh clone.
  - The self-diff fails without approval and passes with it.
  - `just guard-selftest` catches every seeded cheat.
- **DoD:** the runbook §3 verification is recorded in `docs/guardrails.md`, and the owner approves the guardrail PR.

### Phase 2 — Contract discovery (gate before any core code)

- **Objective:** confirm or escalate A1–A6 against the real systems, before the design depends on them.
- **Scope:** spike tests against a real Herdr in the isolated session `herdr-governor-contract` (never the live workspace, H#108), a trial Executor registration, and TypeSafe.
- **Required evidence:** everything in the table below.

  | Assumption | Evidence |
  |---|---|
  | A1 | relay per-session spawn, `HERDR_*` + cwd inheritance, respawn after crash, exit on EOF or SIGINT, relay RSS; the Executor measurements are kept as history (ADR-0004) |
  | A2 | a subscription plus concurrent requests; disconnect and reconnect; malformed frames; timeouts |
  | A3 | ready, typed failure and in-flight or ambiguous start behaviour; identity-matched prompt acknowledgements; for every enabled harness, AGY included |
  | A4 | pane replacement, move, native-session replacement, Herdr restart, and how an incarnation is proven |
  | A5 | resolving each transcript path, partial writes, ambiguous matches, unreadable sources, fallback |
  | A6 | recognizing a pane after a crash |
  | Jev | structural wire compatibility, probabilities, error classes |
- **Changes:**
  - Record the evidence in `docs/research/contract-discovery.md`.
  - Commit the protocol-22 subset and the fixtures.
  - Encode each confirmed behaviour as a contract test that later phases must keep green.
- **Risks:** an assumption fails. It goes to the owner as a Herdr gap, and Phase 3 does not start until the owner decides.
- **Validations:** `just contract` passes on the confirmed behaviours.
- **DoD:** every assumption is marked confirmed, with a test ID, or owner-decided, in §19.

### Phase 3 — Pure core

- **Objective:** every domain rule, total and mutation-tested.
- **Scope:** `governor-core`.
- **Changes:** implement F2–F4, F11–F14, F16–F25 and F27's validation as pure transitions, following Appendix C.
- **Dependencies:** Phase 2.
- **Risks:** holes in the transition rules. Mitigated by exhaustive matches and property tests.
- **Validations:**
  - `just ci` passes.
  - `just mutants-diff` shows 0 unexcluded survivors.
  - Property tests (at least 10k cases) prove:
    - safety over arbitrary event prefixes: at most one settlement, never two prompts per effect key, no transition out of a settlement;
    - settlement once time advances past every applicable deadline;
    - a restart never extends a deadline;
    - a stale async result never applies.
  - The metamorphic test for N8 passes.
- **DoD:** every requirement the core implements has a test named after it (for example `f20_settlement_first_commit_wins`), and Appendix C's table is generated from the code's transition list and matches the spec.

### Phase 4 — Adapters and store

- **Objective:** real I/O behind typed interfaces.
- **Scope:** `store`, and `adapters::{herdr, jev, transcript, git, config}`.
- **Changes:**
  - Appendix B migrations.
  - `store::apply`.
  - The Herdr client over the confirmed protocol subset, plus a fake Herdr server.
  - The Jev client.
  - The three transcript parsers with terminal fallback.
  - Git evidence.
  - Config load and reload.
- **Dependencies:** Phase 3.
- **Risks:** transcript format drift. It returns a typed `unreadable`, which falls back to terminal evidence.
- **Validations:**
  - `just ci` passes.
  - `just contract`: the Herdr adapter's methods pass against both the fake and the real Herdr.
  - Jev fixture tests: structural shape, probability parsing, every error class.
  - Store crash tests: kill at every statement boundary of every Appendix B transaction, reopen, and check the invariants plus the F28 convergence.
  - Parser tests on the fixtures, including partial writes.
  - Config reload tests.
- **DoD:** every adapter has contract tests that fit its kind, and the store invariants hold under crash tests.

### Phase 5 — Daemon and MCP

- **Objective:** a daemon that serves F1–F29.
- **Scope:** `daemon`, `mcp` and the `relay` subcommand.
- **Changes:**
  - Startup and restart (F28), the coordinator, the event loop, and a 30 s reconcile.
  - The effect runner (F8–F10).
  - The launch pipeline, delivery, supervision, acceptance and recovery.
  - Status with pagination, and shutdown (F29).
  - The transport (ADR-0004): `mcp` binds the daemon's 0600 unix socket; `relay` serves one caller session over stdio, derives the F1 caller identity, and forwards each request on a fresh socket connection.
- **Dependencies:** Phase 4.
- **Risks:** a slow coordinator. Mitigated by a load test: 50 concurrent `herdr_status` calls plus 10 launches against the fake Herdr.
- **Validations:**
  - `just ci` passes.
  - End-to-end tests against the fake Herdr cover every F-requirement, including fault injection: a lost acknowledgement, a kill mid-launch at every effect boundary, pane replacement, adoption racing settlement, a stale review after adoption, cancel racing acceptance, publication failures, and the unconfirmed-barrier ordering.
- **DoD:** the daemon runs under a user unit on a non-production socket and state directory, and the fault-injection suite is green.

### Phase 6 — Qualification and conformance

- **Objective:** real-harness evidence.
- **Scope:** `herdr-governor qualify` for every catalog point, and `just conformance` in the isolated session `herdr-governor-conformance`.
- **Scenarios:**
  - acceptance;
  - rejection, then a repair, then acceptance;
  - rejection with no repair, settling `rejected` at the deadline;
  - idle, then a nudge, then `no_handoff`;
  - a follow-up held until idle;
  - a mid-turn follow-up on a qualified point;
  - `pane_lost`;
  - cancel with `closePane`;
  - handover;
  - adoption after the caller is gone;
  - a daemon SIGKILL at each effect boundary, then restart;
  - a Herdr restart during a Run (A4 behaviour);
  - an idempotent replay, a `pending` response and a key conflict;
  - a Jev transport failure, giving an abstention;
  - an explored launch, with both assignment and execution recorded;
  - a simulated provider limit, then cooldown, then a recovery held until the pane closes, then dispatched;
  - max age, settling `unresolved`.
- **Dependencies:** Phase 5.
- **Risks:** real-harness timing flakes. Mitigated by bounded waits, and the nextest `ci` profile treats a flaky pass as a failure.
- **Validations:** `just conformance` passes twice in a row on the same commit, and the report records the release binary hash.
- **DoD:** every enabled point has a current qualification (AGY is included, or stays unroutable), and the conformance report is committed.

### Phase 7 — Install and run in parallel

- **Objective:** `herdr-next` serving real callers.
- **Changes:**
  - **Installer:**
    - Each release declares the schema range it can read. Migrations are expand-only, so the previous release can still read the new schema.
    - The install runs behind an admission-and-effect barrier (the daemon starts in maintenance mode): `VACUUM INTO` backup, migrate, health check.
    - If the health check fails before the barrier lifts, it restores the backup and the previous binary.
    - Once the barrier has lifted, it only ever rolls back to a binary that can read the current schema, keeping the ledger as it is.
  - **Deploy timer:** installs the newest green `main`.
  - **The owner:** authors `catalog.toml` from `herdr-profiles/catalog.yaml`, and registers `herdr-next` per harness through the profile layer (ADR-0004).
  - **Caller skill:** `skills/herdr-governor/SKILL.md`.
- **Dependencies:** Phase 6.
- **Risks:** an agent edits the local catalog. Mitigated by validation on load plus last-good config and qualification invalidation; the owner accepted this trade-off.
- **Validations:**
  - Installer tests: a failing build rolls back before the barrier; a later rollback is refused when the schema range doesn't fit; the backup restores cleanly.
  - `just conformance` passes through `herdr-next`.
  - N3 is measured on real launches.
- **DoD:** parity, meaning the conformance suite is green on the installed build through the per-harness registrations.

### Phase 8 — Swap, drain, archive

- **Objective:** `herdr` is served by the governor, and no obligation is abandoned.
- **Changes:**
  - **Before the swap:** wait until the TypeScript daemon has no in-flight launch.
  - **The swap:**
    - rename the TypeScript integration to `herdr-legacy`: no new launches, but it keeps supervising and serving its mailbox;
    - rename `herdr-next` to `herdr`.
  - **Old daemon:** stop it only once its Runs, mailboxes and journals are drained, or the owner has disposed of each remaining one.
  - **Rollback within 14 days:** reverse the names, and keep the governor draining as `herdr-next`.
  - **After the window:**
    - archive herdr-tools;
    - move roles, skills and the Executor/Hindsight proxy to `Gabrielgvl/agent-skills`;
    - update caller skills;
    - open the owner-approved upstream Herdr discussions (transcripts under ADR-0002, plus the recorded gaps).
- **Validations:** `herdr_status` reports the governor; over 24 hours, no Run is unsettled past its deadlines; `herdr-legacy`'s drain report shows zero obligations left.
- **DoD:** the old daemon is drained or dispositioned, the rollback window has passed, and herdr-tools is archived.

## 11. Validation and Test Plan

| Layer | Command | Expected outcome |
|---|---|---|
| Guardrails | `just ci`, `just guard-selftest` | exit 0; every seeded cheat caught |
| Contracts | `just contract` | A1–A6 behaviour holds on the real Herdr |
| Core | `cargo nextest run -p governor-core --profile ci` | all pass, property tests included |
| Mutation | `just mutants-diff` (in CI), `just mutants` (weekly) | 0 unexcluded survivors; no unexpected zero-mutant run |
| Placement and purity | tripwires within `just ci` | no I/O crate in the core; no lifecycle writes outside `store::transitions`; no harness literals outside the transcript adapter |
| Crash | store crash suite within `just ci` | invariants hold, and F28 converges |
| Fault injection | end-to-end suite against the fake Herdr, within `just ci` | every Phase 5 fault scenario passes |
| Qualification | `herdr-governor qualify <id>` | a current pass for each claimed capability |
| Conformance | `just conformance` | green twice in a row; binary hash recorded |
| Performance | Phase 7 real-launch measurement | N3 and N4 hold |
| Coverage | `just cov` | informational only |

## 12. Observability and Operations

- **Logs:** `tracing` writes to stderr, which lands in the journal. No bodies are logged, only IDs, paths, sizes and digests (H#67).
- **`herdr_status`** reports health, config validity and the time of the last good config, Herdr connection freshness, cooldowns, pending recoveries and unsettled Runs.
- **`docs/operations.md`** holds:
  - `sqlite3` queries over the `outcomes` view: tier distribution, first-review acceptance by tier and point, explored against routed (by assignment and by execution), time to settle, nudge effectiveness;
  - the runbooks: install, rollback, reload, credential rotation, draining, and recording a Herdr gap.

## 13. Security and Privacy

- **Identity:** cooperative between processes of the same user, and documented as such. Ownership is enforced on every operation (F4).
- **Transport:** the stdio relay only (ADR-0004) — no network listener and no bearer token. The daemon's unix socket is 0600. The relay reads only `HERDR_*` and its cwd, never logs environment values, holds no upstream session state, reconnects to the socket per request, and exits on stdin EOF or SIGINT/SIGTERM. Every forwarded request carries the relay-attached caller envelope `{paneId, projectRoot, relayInstanceId}`, and the daemon binds each new `relayInstanceId` to one resolved caller, refusing later drift `CALLER_IDENTITY_MISMATCH` (F1).
- **Files:** the state directory is 0700; files and the database are 0600. Handoffs are read without following symlinks, with bounded size. Follow-up files are immutable.
- **Provenance:** the envelope's header values are generated and each is a single line (H#25–27).
- **Redaction:** environment-like keys are removed from evidence before it reaches Jev. Bodies never appear in argv, logs or errors (H#67, H#104).
- **Subprocesses:** `git` only, as argv arrays, never through a shell.

## 14. Rollout and Rollback Plan

- **Forward:** Phase 7 runs `herdr-next` in parallel; Phase 8 swaps at parity and drains `herdr-legacy`.
- **A bad release:** the installer's barrier-scoped rollback handles it (Phase 7). A ledger that later effects have touched is never restored from backup.
- **Cutover rollback (within 14 days):** swap the names back. The outgoing implementation stops admitting new work but keeps supervising what it already owns. No Run is ever left unsupervised.
- **After 14 days:** revert governor commits within the readable schema range, or fix forward.

## 15. Risks and Mitigations

| Risk | Mitigation |
|---|---|
| A1–A6 are wrong | Phase 2 gate before any core code; owner decides the gap |
| Herdr protocol drift | Startup check and the schema fixture; failing closed |
| Incarnation can't be proven | F28: re-prove by native session, otherwise `unresolved(identity_unprovable)` |
| Transcript drift | Typed `unreadable`, then the terminal fallback |
| No live canary before the swap (owner decision) | Drain-not-abandon cutover, 14-day rollback, the Phase 8 24-hour check |
| The local catalog is ungated | Validation, last-good config, qualification invalidated on an args change |
| Uncalibrated narrow questions | Exploration, with assignment and execution recorded; outcomes are operational labels, not gold |
| Tests weakened while the rewrite is under way | Protected test support, full mutation on test-only changes, trusted BASE, commit-bound approval, seeded cheats |
| Business logic bypasses the core | `store::apply(Transition)` only, plus the lifecycle-write tripwire |
| No spend ceiling | Cost-class ordering prefers free Devin |

## 16. Dependencies

- Herdr 0.9.1 or later (protocol 22) and its integrations.
- TypeSafe `systemOne`.
- The herdr-tools profile layer, which registers the relay per harness (ADR-0004); Executor remains only for the `herdr-legacy` drain. Registrations and swaps are owner steps.
- Rust 1.98.1 and the kit tools, already installed.
- GitHub: public repo, branch protection, Actions, Dependabot.
- The anti-slop-repo Rust pack, extended here.
- The new `Gabrielgvl/agent-skills` repo (Phase 8).

## 17. Definition of Done (Global)

- `herdr` is served by herdr-governor; `herdr-legacy` is drained or dispositioned; herdr-tools is archived.
- `just ci`, `just contract` and `just conformance` are green on `main`.
- Every F and N requirement traces to a named test, and Appendix A has no row left undisposed.
- 24 hours of live use show no unsettled Run past its deadlines and no duplicate effect key.
- Roles and skills live in `agent-skills`, and the caller skill is current.
- The docs are consistent: CONTEXT.md, the ADRs, this spec, `docs/operations.md` and `docs/guardrails.md`.

## 18. Definition of Done (Per Phase)

The DoD is stated in each phase in §10. It must hold on a merged `main` commit, with that phase's validations run on that exact commit.

## 19. Open Questions and Decisions

**Open until Phase 2:** A1–A6 and the Jev wire behaviour. Each is resolved as confirmed (with a test ID) or owner-decided.

**Decided by the owner (2026-09-27, including answers to the review):**
- **Stack:** all Rust, a pure core with adapters, a new public repo, a clean break.
- **Priorities:** reliable > agent-safe > owner-comprehensible > fast.
- **Integration:** the Herdr socket API; the daemon serves MCP itself; SQLite; its own Jev credential file.
- **Harnesses:** agnostic, except the transcript parsers (ADR-0002).
- **Identity and routing:**
  - Launch plus Run identity.
  - Jev judges Tasks, never models, using the four narrow questions plus the gate and the tier question.
  - An owner policy file with 5% exploration.
  - The cheapest qualified point wins.
  - Relatedness tabs; no intent question.
- **Supervision:** action-driving questions only, with one nudge per episode.
- **Lifecycle and delivery:** every Run settles; a durable outbox; an actionable-only mailbox; adoption; cancel that settles and optionally closes.
- **Task and config:** `constraints` optional; a free-Markdown handoff with an end marker; a local catalog.
- **Cuts:** replicas and attachments, deleted in the TypeScript first. AGY kept (routable once qualified).
- **Guardrails and delivery:** kit defaults with the latest pinned Rust; auto-merge except hard paths; deploy green `main`.
- **Cutover:** parallel, swap at parity, 14-day rollback.
- **Process:** no spend ceiling; Herdr gaps decided case by case; Herdr owns readiness and prompt confirmation; no reconcile action; 15-minute idle window.
- **Q1:** roles and skills move to `Gabrielgvl/agent-skills`.
- **Q2:** add `provider_limited` and `unresolved(reason)`; recovery is a separate obligation.
- **B6:** recovery waits for proof the predecessor stopped (ADR-0003).
- **Q3:** a 15-minute repair window that nothing extends; `unresolved(judgment_unavailable)` when Jev can't decide.
- **B2:** a 24-hour maximum age by default.
- **Herdr gap, case 3 (Devin workspace trust):** Devin operating points pass `--respect-workspace-trust false` in their catalog arguments. Devin's first-run "trust this directory?" dialog reads as ready to Herdr, so launch prompts landed on the dialog. Accepted trade-off: Devin's protection against malicious repository configuration is off for every launch. Phase 2 (A3) and qualification (F26) must detect any other first-run dialog a harness shows.

**What happened to each review finding:**
- **B1:** F8, F11, F12, F13, F15, F28.
- **B2:** F20, F22, F25, Appendix C.
- **B3:** F18, F19, F20.
- **B4:** F1–F4, F10, F28, A4.
- **B5:** F9, F17.
- **B6:** F21, ADR-0003.
- **B7:** Phase 7, Phase 8, §14.
- **M1:** F23, F24.
- **M2:** Appendix B.
- **M3:** Phase 2, A1–A6, the Phase 4 DoD, AGY qualification.
- **M4:** Phase 1, the §9 store API.
- **M5:** F13, F15.
- **M6:** F1, F5, F17, F26, N5, §13, the research scrub in Phase 1.
- **M7:** Appendix A.
- **m1:** §1, N3.
- **m2:** F27, N8.
- **Deletions:** the `outcomes` view (Appendix B), structural Jev fixtures (Phase 2), Phase 1 no longer blocked on Phase 0's deploy, and the unsupervised rollback and causal claims removed.

**Decided by the owner (Phase 2, 2026-09-28/29):**
- **A1 (Executor transport):** measured on the real caller gateway (Executor catalog daemon, 2026-09-29): `mcp.addServer` registration is declarative, a static bearer via an apiKey `headers` authenticationTemplate + credential-provider item is sent on every request with OAuth never engaging, an unauthenticated 401 still triggers RFC 8414/OIDC discovery, restart and stale-`Mcp-Session-Id` recovery re-initialize transparently, warm calls reuse the session, and a down server surfaces an untyped `Internal tool error [<id>]`; the earlier "loopback no-auth only" result measured pi's MCP adapter (retired in pi 0.99), not Executor. Transport decided: ADR-0004.
- **A2 (subscription EOF):** an unexpected subscription EOF is re-armed with a state catch-up (`pane.read`/`session.snapshot`); "server gone" is declared only when the re-arm cannot connect; every teardown emits a typed event.
- **A3 (readiness):** readiness is advisory — the identity-matched acknowledgement (F16) plus F26 qualification are the signals; the Herdr detection gap is a non-blocking follow-up. The acknowledgement proves delivery to the pane, not consumption: a first-run gate that Herdr reports `idle` can swallow the Task, and the Run then settles `no_handoff` via F25 (bounded). Mitigations: catalog arguments that disable the gate (Devin `--respect-workspace-trust false`), Claude's gate reported `blocked` as an owner notice, and F26 qualification — which detects gates only in its own qualification directory.
- **A3 (start failures):** the typed pre-interactive error assumption is confirmed-negative for runtime startup failures — a missing binary, rejected agent args, and mid-start death all return one untyped `timeout` after the full `--timeout`. A runtime start timeout is F15's "any other outcome": stop falling back, record `failed {effectCertainty}`, and the Run settles by the transition rules. Fallback happens only on typed pre-flight errors (`agent_pane_busy`); typed runtime startup errors are a non-blocking Herdr gap.
- **A4/A6 (identity):** native-session re-proof — after any server discontinuity, Runs re-prove identity through the unique native session.
- **A6 (pane tagging):** the F8 unconfirmed/no-adoption fallback — an interrupted topology effect is reported `unconfirmed` and never adopted; the F14 right-split stays; a labelled `layout.apply` is rejected.
- **Herdr gap, case 4 (Claude repository trust):** rely on Claude's native once-per-repository trust; when a child is blocked at startup, one actionable owner notice names the pane; no Claude-specific code.
- **Schema surface:** the Herdr schema doc is not the exhaustive request surface — `pane.graphics.stream` exists at runtime while absent from the doc.
- **Core purity:** `governor-core` is `#![no_std]` with `alloc`.
- **AGY supervision (A3/A5):** AGY is terminal-only — supervision uses terminal evidence (`agent.read`); there is no out-of-band transcript/uuid discovery (a presence lock cannot be proven to belong to a pane while AGY sessions run concurrently; identity is never guessed). The missing agy `agent_session` is a non-blocking Herdr gap; an AGY Run without a native session settles `unresolved(identity_unprovable)` after an unproven Herdr incarnation change (F28).

**Decided by the owner (2026-09-30):**
- **Caller transport:** the native per-session stdio relay (ADR-0004). Each caller harness's own MCP spawns `herdr-governor relay` once per session; the stateless relay derives the F1 caller identity (`paneId` from `HERDR_PANE_ID`, `projectRoot` from its cwd's git root) and forwards to the daemon's 0600 unix socket; registration is global per harness through the herdr-tools profile layer; N4 allows one relay per caller session at or under 8 MB RSS.

## 20. Approval

| Role | Name | Status |
|---|---|---|
| Owner | Gabriel | Approved Draft 2, 2026-09-27 |
| Reviewer | Oracle Pro | Draft 1 reviewed (Rework); owner waived a Draft 2 round |

Status: **Approved for Implementation.**

---

## Appendix A — Normative disposition of harvested invariants

`retained` = the property holds as stated. `replaced` = the property holds through the named requirement, and the old mechanism is gone. `retired` = removed by the named decision.

| H# | Disposition |
|---|---|
| 1, 7, 106 | replaced: activation is `systemctl --user enable` of the governor unit (a one-time owner step); later approved deploys need no fresh activation; startup failure behaviour follows F27 |
| 2, 21, 22 | replaced by F1 |
| 3 | retained, as F1's `projectRoot` rules plus F5's `cwd` inside the root |
| 4, 5, 6, 8 | retained: N7, F28, F29 |
| 9 | replaced by MCP (F5–F7); no internal handshake — the relay attaches the derived caller to each forwarded call (ADR-0004) |
| 10–13, 15 | replaced: typed socket calls, bounded timeouts, typed malformed-output failure, a fresh post-state read, and effect certainty (F8, F10) |
| 14 | retained (F10) |
| 16 | replaced by F17 |
| 17 | replaced by F17: a generic safe-state rule for every harness |
| 18, 19 | retired: no target references; tools address runIds |
| 20 | replaced by F2 |
| 23, 25–27 | retained (F16, F17 envelope; sender taken from the verified caller) |
| 24 | retained (F4) |
| 28 | retired: no key delivery |
| 29–31, 34 | retained (F16) |
| 30 | retained: identity-matched acknowledgements (F16, A3) |
| 32, 33, 43 | retired: Herdr owns readiness and confirmation (owner decision) |
| 35 | retired: no turn control |
| 36, 37, 44, 69 | replaced by the reserved Run (F13) and F15 |
| 38–41 | retained (F5); replicas retired |
| 42 | replaced: a bounded wait around `agent.start` (A3) |
| 45 | retained (F15) |
| 46 | retained (F13 chain; F15 reprobe) |
| 47, 48 | retained (F12, F13); the question set changed by owner decision |
| 49, 50 | retained (F13, F21) |
| 51 | replaced: the persisted decision (F13) in SQLite |
| 52 | retained (F2 minted names) |
| 53 | retained (F14); the four-pane cap kept |
| 54 | retired (replicas) |
| 55, 56 | retained (F5 failure results) |
| 57, 80 | replaced: evidence budgets (N5); required Task digest fields (F23) |
| 58 | replaced: the identity rules in F2 (relaxed before the prompt, by owner decision) |
| 59, 60 | retired: AGY provisional and Devin-specific paths; AGY qualifies generically (F26) |
| 61–68 | replaced for follow-up files (F17): bounds, atomic publication, retention while referenced, no bodies in argv or logs; leases, grants and add-dir capability logic retired, with readability proven by qualification (F26) |
| 70, 71 | retained: one subscription; a 30 s reconcile (F28, Phase 5) |
| 72, 76 | retired: watermark taxonomy (owner decision), replaced by F3 |
| 73, 74, 75 | retained (F3) |
| 77 | retired: one nudge per episode is allowed (owner decision) |
| 78 | replaced by F18's event list |
| 79 | retained (F23, F24 freshness and binding) |
| 81 | retained (F23) |
| 82 | retained (F23) |
| 83 | replaced by F21 |
| 84 | retained, except for pagination (F18, §6.2) |
| 85–88 | replaced by F18 and F26 (hint qualification; Devin's permission arguments are catalog data proven by qualification) |
| 89 | replaced by F28 |
| 90 | retained (F11), plus `pending` |
| 91 | replaced by F19 |
| 92 | replaced by F4 and F6 `observe` ownership |
| 93 | retained: no steer or cancel beyond F6; no daemon CLI surface for callers |
| 94 | replaced by F24 |
| 95–101 | retired: no waits, jobs or Pi UI |
| 102 | retained for close and initial-pane handling (F10, F14) |
| 103, 104 | retained (§6.2, N5, §13) |
| 105 | replaced by F9 plus the single coordinator |
| 107 | replaced by F27 |
| 108 | retained (Phases 2, 6) |
| 109 | retired: role conduct moves to `agent-skills` |
| 110 | replaced by F26 |

## Appendix B — Store DDL and transaction contents

```sql
-- Verified at open: PRAGMA foreign_keys (must read 1), journal_mode=WAL, synchronous=FULL.
-- Schema version in PRAGMA user_version; migrations are expand-only (Phase 7).

CREATE TABLE callers (
  caller_id      INTEGER PRIMARY KEY,
  agent_kind     TEXT NOT NULL,
  native_session TEXT NOT NULL,
  first_seen_at  TEXT NOT NULL,
  UNIQUE (agent_kind, native_session)
);

CREATE TABLE relay_bindings (
  relay_instance_id TEXT PRIMARY KEY,                     -- 128-bit random id minted by the relay, lowercase hex
  caller_id         INTEGER NOT NULL REFERENCES callers(caller_id),
  pane_id_at_bind   TEXT NOT NULL,
  bound_at          TEXT NOT NULL
);

CREATE TABLE launches (
  launch_id       TEXT PRIMARY KEY,                       -- uuid v7
  caller_id       INTEGER NOT NULL REFERENCES callers(caller_id),
  project_root    TEXT NOT NULL,
  idempotency_key TEXT NOT NULL,
  digest_version  INTEGER NOT NULL,
  task_digest     TEXT NOT NULL,
  task_json       TEXT NOT NULL,
  phase           TEXT NOT NULL CHECK (phase IN ('evaluating','routed','launching','done')),
  outcome         TEXT CHECK (outcome IN ('launched','abstained','rejected','failed')),
  outcome_reason  TEXT,
  decision_json   TEXT,          -- F13: floors, requested tier, exploration assignment, candidates with args
  config_version  TEXT,
  result_json     TEXT,
  created_at      TEXT NOT NULL,
  updated_at      TEXT NOT NULL,
  UNIQUE (caller_id, project_root, idempotency_key),
  CHECK ((phase = 'done') = (outcome IS NOT NULL))
);

CREATE TABLE runs (
  run_id              TEXT PRIMARY KEY,
  launch_id           TEXT NOT NULL UNIQUE REFERENCES launches(launch_id),
  owner_caller_id     INTEGER NOT NULL REFERENCES callers(caller_id),
  owner_generation    INTEGER NOT NULL DEFAULT 0,
  version             INTEGER NOT NULL DEFAULT 0,
  state               TEXT NOT NULL CHECK (state IN ('reserved','starting','prompting','active','judging','repair','settled')),
  prompt_certainty    TEXT CHECK (prompt_certainty IN ('acknowledged','unconfirmed')),
  child_name          TEXT NOT NULL UNIQUE,
  herdr_incarnation   TEXT,
  terminal_id         TEXT,
  agent_kind          TEXT,
  agent_name          TEXT,
  native_session      TEXT,
  pane_id             TEXT,                                -- current locator only
  operating_point_id  TEXT,
  provider            TEXT,
  tier_start          TEXT,
  cwd                 TEXT NOT NULL,
  base_commit         TEXT,
  work_generation     INTEGER NOT NULL DEFAULT 0,
  evidence_generation INTEGER NOT NULL DEFAULT 0,
  child_status        TEXT,
  idle_since          TEXT,
  idle_deadline       TEXT,
  repair_deadline     TEXT,
  judgment_deadline   TEXT,
  max_age_deadline    TEXT NOT NULL,
  nudge_episode       INTEGER NOT NULL DEFAULT 0,
  nudged_episode      INTEGER,
  settlement          TEXT CHECK (settlement IN ('accepted','rejected','no_handoff','pane_lost','cancelled','provider_limited','unresolved')),
  settlement_reason   TEXT,
  settled_at          TEXT,
  created_at          TEXT NOT NULL,
  updated_at          TEXT NOT NULL,
  CHECK ((state = 'settled') = (settlement IS NOT NULL)),
  CHECK ((settlement IS NULL) = (settled_at IS NULL)),
  CHECK (settlement IS NOT 'unresolved' OR settlement_reason IS NOT NULL)
);

CREATE TRIGGER runs_settlement_immutable
BEFORE UPDATE OF settlement, settlement_reason, settled_at ON runs
WHEN OLD.settlement IS NOT NULL
BEGIN SELECT RAISE(ABORT, 'settlement is immutable'); END;

CREATE TABLE effects (
  effect_id         TEXT PRIMARY KEY,
  effect_key        TEXT NOT NULL UNIQUE,   -- e.g. run:<id>:prompt:task, run:<id>:outbox:<seq>, run:<id>:nudge:<episode>, event:<id>:hint
  kind              TEXT NOT NULL CHECK (kind IN ('jev_evaluate','tab_create','pane_split','agent_start','prompt','close')),
  subject_launch_id TEXT REFERENCES launches(launch_id),
  subject_run_id    TEXT REFERENCES runs(run_id),
  target_json       TEXT,
  payload_digest    TEXT,
  state             TEXT NOT NULL CHECK (state IN ('planned','dispatching','acknowledged','failed','unconfirmed')),
  certainty         TEXT CHECK (certainty IN ('absent','unknown')),
  result_json       TEXT,
  planned_at        TEXT NOT NULL,
  dispatched_at     TEXT,
  completed_at      TEXT,
  CHECK (subject_launch_id IS NOT NULL OR subject_run_id IS NOT NULL),
  CHECK (state <> 'failed' OR certainty IS NOT NULL)
);

CREATE TABLE outbox (
  run_id           TEXT NOT NULL REFERENCES runs(run_id),
  seq              INTEGER NOT NULL,
  message_key      TEXT NOT NULL,
  sender_caller_id INTEGER NOT NULL REFERENCES callers(caller_id),
  body_digest      TEXT NOT NULL,
  body_inline      TEXT,
  body_path        TEXT,
  state            TEXT NOT NULL CHECK (state IN ('queued','dispatching','submitted','unconfirmed','expired')),
  effect_id        TEXT UNIQUE REFERENCES effects(effect_id),
  expiry_reason    TEXT,
  enqueued_at      TEXT NOT NULL,
  finished_at      TEXT,
  PRIMARY KEY (run_id, seq),
  UNIQUE (run_id, message_key),
  CHECK ((body_inline IS NULL) <> (body_path IS NULL)),
  CHECK (state <> 'expired' OR (effect_id IS NULL AND expiry_reason IS NOT NULL))
);

CREATE TABLE mailbox (
  event_id   TEXT PRIMARY KEY,
  dedup_key  TEXT NOT NULL UNIQUE,          -- e.g. run:<id>:settled, run:<id>:stalled:<episode>
  launch_id  TEXT REFERENCES launches(launch_id),
  run_id     TEXT REFERENCES runs(run_id),
  kind       TEXT NOT NULL,
  body_json  TEXT NOT NULL,
  acked_at   TEXT,
  created_at TEXT NOT NULL,
  CHECK (launch_id IS NOT NULL OR run_id IS NOT NULL)
);
-- The destination is derived when read: runs.owner_caller_id, or launches.caller_id for launch-only events.

CREATE TABLE handoffs (
  run_id          TEXT NOT NULL REFERENCES runs(run_id),
  work_generation INTEGER NOT NULL,
  digest          TEXT NOT NULL,
  frozen_path     TEXT NOT NULL,
  frozen_at       TEXT NOT NULL,
  PRIMARY KEY (run_id, work_generation, digest)
);

CREATE TABLE judgment_sets (
  set_id              TEXT PRIMARY KEY,
  purpose             TEXT NOT NULL CHECK (purpose IN ('launch','review','acceptance','provider_limit')),
  launch_id           TEXT REFERENCES launches(launch_id),
  run_id              TEXT REFERENCES runs(run_id),
  run_version         INTEGER,
  work_generation     INTEGER,
  evidence_generation INTEGER,
  task_digest         TEXT NOT NULL,
  handoff_digest      TEXT,
  evidence_digest     TEXT,
  model               TEXT NOT NULL,
  question_version    TEXT NOT NULL,
  policy_version      TEXT NOT NULL,
  outcome             TEXT NOT NULL CHECK (outcome IN ('answered','transport_failed','auth_failed','invalid_response','too_large','stale')),
  requested_at        TEXT NOT NULL,
  answered_at         TEXT,
  CHECK (launch_id IS NOT NULL OR run_id IS NOT NULL)
);

CREATE TABLE judgments (
  set_id             TEXT NOT NULL REFERENCES judgment_sets(set_id),
  question           TEXT NOT NULL,
  probabilities_json TEXT NOT NULL,
  answer             TEXT NOT NULL,
  threshold          REAL,
  PRIMARY KEY (set_id, question)
);

CREATE TABLE recoveries (
  predecessor_run_id  TEXT PRIMARY KEY REFERENCES runs(run_id),
  origin              TEXT NOT NULL CHECK (origin IN ('provider_limit','caller')),
  state               TEXT NOT NULL CHECK (state IN ('pending','blocked','dispatched','failed')),
  reason              TEXT,
  successor_launch_id TEXT UNIQUE REFERENCES launches(launch_id),
  expires_at          TEXT NOT NULL,
  created_at          TEXT NOT NULL,
  updated_at          TEXT NOT NULL,
  CHECK ((state = 'dispatched') = (successor_launch_id IS NOT NULL))
);

CREATE TABLE cooldowns (
  provider      TEXT PRIMARY KEY,
  until         TEXT NOT NULL,              -- upsert keeps max(existing, new): never shortened
  reason        TEXT NOT NULL,
  source_run_id TEXT REFERENCES runs(run_id),
  updated_at    TEXT NOT NULL
);

CREATE TABLE qualifications (
  operating_point_id TEXT NOT NULL,
  args_digest        TEXT NOT NULL,
  capability         TEXT NOT NULL,
  passed             INTEGER NOT NULL CHECK (passed IN (0,1)),
  evidence_json      TEXT NOT NULL,
  qualified_at       TEXT NOT NULL,
  PRIMARY KEY (operating_point_id, args_digest, capability)
);

CREATE VIEW outcomes AS
SELECT r.run_id,
       r.settlement,
       r.settlement_reason,
       r.tier_start,
       r.operating_point_id,
       json_extract(l.decision_json, '$.exploration.assigned') AS explored_assigned,
       json_extract(l.decision_json, '$.exploration.executed') AS explored_executed,
       (SELECT count(*) FROM effects e WHERE e.subject_run_id = r.run_id AND e.effect_key LIKE 'run:%:nudge:%') AS nudges,
       (SELECT count(*) FROM judgment_sets j WHERE j.run_id = r.run_id AND j.purpose = 'acceptance' AND j.outcome = 'answered') AS acceptance_rounds,
       rc.successor_launch_id AS recovered_by,
       (julianday(r.settled_at) - julianday(r.created_at)) * 86400 AS seconds_to_settle
FROM runs r
JOIN launches l ON l.launch_id = r.launch_id
LEFT JOIN recoveries rc ON rc.predecessor_run_id = r.run_id;
```

Foreign keys never cascade deletes. Nothing deletes launches, runs, effects, mailbox rows or judgments in v1.

**Transactions.** Each one below is a single `store::apply(Transition)`; none spans I/O.

| Transition | Rows written atomically |
|---|---|
| Bind a caller (F1) | the first request with a new `relayInstanceId`: the `callers` row if the caller key is new, plus the `relay_bindings` row — one transaction; rows are kept indefinitely like launch keys, one row per relay session |
| Admit Launch | launch (`evaluating`) plus the `jev_evaluate` effect (`planned`) |
| Route | the launch decision, config version and phase `routed`, plus the reserved run with `max_age_deadline` |
| Plan an effect | the effect (`planned`) |
| Dispatch an effect | the effect goes `planned` → `dispatching` |
| Effect result | the effect result, plus the dependent run fields (identity, `prompt_certainty`, state) and version+1 |
| Enqueue a follow-up | the outbox row, after the file is published and verified |
| Settle | runs (conditional on version and unsettled) plus the terminal mailbox event, the queued follow-ups expired, and the recovery and cooldown when the settlement is `provider_limited` |
| Handover or adopt | `runs.owner_caller_id` and `owner_generation+1`, conditional on the expected owner |
| Recovery dispatch | recovery `pending` → `dispatched`, plus the successor Launch admission |
| Freeze a handoff | the handoff row, `evidence_generation+1`, and `judgment_deadline` if not already set |

## Appendix C — Lifecycle transition rules

**States:** `reserved`, `starting`, `prompting`, `active`, `judging`, `repair`, `settled`.

**Events:**
- `obs(unique | absent | invalid)`, with the child status `working | idle | done | blocked`;
- `handoff(valid marked file)`;
- `judgment(accept | reject | unavailable)`;
- `deadline(idle | repair | judgment | max_age)`;
- `cancel`;
- `provider_limited`;
- `effect_result`;
- `restart`.

The rules below are generated from `lifecycle::TRANSITION_RULES` in governor-core — `*` reads "any state" and `unsettled` reads "any state but `settled`". `governor-core/tests/appendix_c.rs` proves the table matches `TRANSITION_RULES` byte for byte, and `governor-core/src/lifecycle/tests/appendix_c.rs` names the unit test that proves `transition` agrees with every row.

| State | Event | Outcome |
|---|---|---|
| `settled` | `cancel(closePane)` | close the pane only |
| `settled` | `any other event` | ignored, including a late handoff or judgment — settlement is immutable |
| `*` | `obs(invalid)` | no change except health reporting; deadlines still run |
| `unsettled` | `deadline(max_age)` | settle unresolved(max_age) |
| `unsettled` | `cancel` | settle cancelled; closePane also closes |
| `*` | `restart` | dispatching effects become unconfirmed (F8); every Run re-derived from its persisted state; deadlines unchanged |
| `unsettled` | `provider_limited` | settle provider_limited (F21) |
| `reserved` | `obs(absent)` | settle unresolved(launch_not_started) |
| `reserved` | `topology effect planned` | starting (the launch plan write moves it) |
| `reserved` | `launch abstains or fails before any effect` | unresolved(launch_not_started) via settle; the Launch reports its outcome |
| `starting` | `start acknowledged` | prompting; task prompt planned |
| `starting` | `typed pre-interactive failure with another candidate` | stays starting; next candidate planned in the same pane |
| `starting` | `failure with no candidate, or unconfirmed` | stays starting until obs(absent) or max_age |
| `starting` | `obs(absent)` | settle unresolved(launch_failed) |
| `prompting` | `prompt acknowledged` | active; prompt_certainty acknowledged |
| `prompting` | `prompt unconfirmed or failed` | active; prompt_certainty unconfirmed + prompt_unconfirmed event |
| `prompting` | `obs(absent)` | settle pane_lost |
| `active` | `obs(working)` | clear idle_since; the episode ends |
| `active` | `obs(idle|done) with no handoff` | open the idle episode; one nudge; idle_deadline set |
| `active` | `obs(blocked)` | ask blocked_on_input and provider_limited |
| `active` | `deadline(idle)` | settle no_handoff |
| `active` | `handoff(valid)` | freeze; judging |
| `active` | `obs(absent)` | one-shot handoff read: valid → freeze + judging; otherwise pane_lost |
| `judging` | `judgment(accept)` | settle accepted |
| `judging` | `judgment(reject)` | repair; repair_deadline armed once per work generation |
| `judging` | `judgment(unavailable)` | stays judging until judgment_deadline |
| `judging` | `deadline(judgment)` | settle unresolved(judgment_unavailable) |
| `judging` | `deadline(repair) armed and passed` | settle rejected |
| `judging` | `obs(absent)` | stays judging; the frozen handoff is judged |
| `judging` | `handoff(new digest)` | re-freeze; stays judging |
| `judging` | `stale judgment` | ignored (F20) |
| `repair` | `repair follow-up dispatched before repair_deadline` | work_generation+1; active |
| `repair` | `handoff(digest not yet judged)` | freeze; judging (repair_deadline kept) |
| `repair` | `deadline(repair)` | settle rejected |
| `repair` | `obs(absent)` | stays repair until the deadline |

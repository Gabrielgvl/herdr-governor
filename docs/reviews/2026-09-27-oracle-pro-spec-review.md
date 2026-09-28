# Oracle Pro review of herdr-governor-spec.md (Draft 1)

- **Source:** ChatGPT Latest, Pro thinking, run through Oracle 0.21.3 on the Mac bridge. The owner submitted the prompt manually after two automated attempts stalled on attachment upload before submit. The answer was pasted back into the session on 2026-09-27.
- **Status:** advisory. How each finding was disposed of is recorded in spec §19 and in Appendix A.
- **Citation markers:** `:chatgpt-content-reference` markers are ChatGPT's citations to the attached files and are kept as received.

---

# 1. Verdict

**Rework.** The accepted direction is defensible: Rust, a pure core, one daemon, SQLite, three tools, and harness-specific transcript parsers only. The implementation spec is not yet safe to execute:
- it leaves crash-interrupted effects without a durable dispatch protocol;
- it does not define a total settlement function;
- it permits ownership and settlement races to strand notifications;
- it treats provider recovery and rollback as safer than their specified behavior warrants.

These are correctness and data-integrity defects, not objections to the rewrite. I would retain the architecture and decisions, but require the lifecycle, effect journal, transaction boundaries, and fault-test matrix below before Phase 2.

This review concerns what the supplied specification permits. The implementation, actual kit configuration, protocol fixture, and SDK fixtures were not supplied.

# 2. Findings, ranked by severity

## Blockers

### B1. A launch record is not a crash-safe dispatch record

**Sections:** F4, F10–F12, F20; N1, N6; §9 data model.

F4 records a Launch before effects, but neither the schema nor F20 distinguishes three states: "effect planned," "dispatch may have begun," and "receipt durably recorded." A uniqueness constraint prevents two Launch records; it does not prevent the same record from issuing an effect twice. F20's snapshot reconciliation cannot recover prompt-dispatch history. :chatgpt-content-reference{index="0"} :chatgpt-content-reference{index="1"}

**Failure sequence:**
1. The daemon persists the Launch, creates the pane, starts the agent, and submits the Task.
2. Herdr accepts the submission.
3. The daemon dies before recording the acknowledgment.
4. After restart, the pane is idle. That observation fits an unsubmitted Task, a queued Task, or a completed turn. Resubmitting duplicates work; recording only `Launch.failed` can abandon the still-live Run.

The same ambiguity exists around pane creation and `agent.start`. A fresh shell-looking snapshot does not necessarily prove that a previously issued, still-in-flight start request cannot subsequently execute.

**Exact edit:** Replace F20's interrupted-launch clause with:

> Before every external mutation, persist a uniquely keyed effect record containing the intended operation, captured target identity, payload digest, and dispatch state. Commit `dispatching` before invoking Herdr. On restart, `dispatching` without a durable receipt becomes `unconfirmed` and is never dispatched again. A snapshot may establish current identity or liveness; it never establishes whether a prompt was submitted. Resume only effects durably recorded as not dispatched.

Also add:

> Reserve the Run identity and durable supervision obligation before the first topology mutation. Once a child may exist, a failed Launch retains a linked, observable Run until settlement. All failure results report the Run ID and known created topology.

Persist the routing decision and selected catalog arguments before effects. A crash after Jev dispatch but before recording its response must not silently issue a second evaluation under the "one request per Launch" contract.

For concurrent or repeated F1 calls, add a bounded **`pending`** response referring to the existing Launch/Run, or explicitly require joining its existing execution. A missing `result_json` must never authorize another executor.

---

### B2. "Every Run settles" is not implemented by the specified transitions

**Sections:** F12–F16, F20; N2; Phase 2.

The specification provides terminal outcomes for some observations, not every reachable state:
- `prompt_unconfirmed` lacks a complete transition definition;
- F14 mainly describes `running`;
- settlement depends on idle/done or pane disappearance. :chatgpt-content-reference{index="2"}

**Failure sequences:**
- A child remains `blocked` or `working` indefinitely, with no handoff.
- A caller disappears, paid review pauses, and a handoff needs a Jev decision.
- A pane remains present but has a replacement `terminal_id`. F11 says it is not the same Run, but F20 specifies neither reattachment nor settlement for that branch.
- A marked handoff exists, Jev remains unavailable, and the pane exits. "Closed with **no handoff**" does not apply.
- Repeated restarts recreate in-memory idle timers or nudge state, postponing settlement indefinitely.

**Exact edit:** Add a complete transition table. It must cover every operational state against `unique`, `absent`, and `invalid` observations; handoff presence and judgment status; deadlines; cancellation; and restart.

Persist `idle_since`, `idle_deadline`, the repair deadline, the nudge episode, and an absolute Run deadline. Add:

> Repeated observations and daemon restart do not reset deadlines. Advisory-review suspension never suspends lifecycle deadlines. Each Run has a finite maximum age, fixed when admitted. Deadline expiry with insufficient evidence settles `unresolved`, with a specific reason; it never fabricates `accepted`, `rejected`, or `pane_lost`.

A **24-hour default maximum age** is a concrete starting proposal, with an owner-authored longer value where required. This settles the accounting obligation; it does not kill the pane.

Define the liveness assumption honestly: eventual daemon execution and writable durable storage. Random finite event sequences cannot prove unconditional eventual settlement. Phase 2 must separately test:
- safety over arbitrary prefixes;
- settlement after explicitly advancing time beyond the applicable deadlines;
- restart without deadline extension.

---

### B3. Settlement, adoption, cancellation, and asynchronous reviews have no shared commit rule

**Sections:** F2, F15–F20; §9 store.

"One transaction per state transition" is insufficient without defining what the transaction contains and how stale results are handled. The current schema has neither an ownership generation nor a Run version. :chatgpt-content-reference{index="3"} :chatgpt-content-reference{index="4"}

**Failure sequence (review versus adoption):**
1. Jev reviews Run R for caller A.
2. Caller B adopts R.
3. The old review completes, settles R, and inserts its event for A.
4. R is now terminal, so B cannot adopt it under "unsettled Runs." A is absent, so the outcome is stranded.

**Failure sequence (cancel versus review):** cancellation commits while an earlier acceptance review is in flight. Without a conditional update, both outcomes or both terminal notifications may be written.

**Exact edit:**

> Every asynchronous result carries the Run version and evidence generation against which it was requested. Apply it only if those preconditions still hold. Settlement is first-commit-wins: conditionally update an unsettled Run, insert its terminal event, expire eligible queued follow-ups, and record its outcome in one transaction. A losing transition produces no terminal event or external effect.

Ownership transfer must atomically update current ownership and notification destinations. **Do not rekey the original Launch's idempotency binding during adoption.**

To close the settlement/adoption race, change adoption to cover outstanding obligations:

> An absent caller's explicitly named Run may be adopted while unsettled, or solely to receive its unread events and pending recovery obligations after settlement. Adoption never reopens a terminal Run.

This does not require resurrecting the old claim-file machinery.

---

### B4. `terminal_id` is not a sufficient identity boundary, especially across Herdr restart

**Sections:** A4, tool caller verification, F9–F11, F18–F20, F2 cancellation.

A4 promises uniqueness **within one server lifetime**, but restart matching uses terminal ID and agent name without a server-incarnation boundary. F20 also omits the native session once it has been acquired. The harvest explicitly requires stronger identity and distinguishes invalid observations from absence. :chatgpt-content-reference{index="5"} :chatgpt-content-reference{index="6"} :chatgpt-content-reference{index="7"}

**Failure sequence:** Herdr restarts and reuses a terminal ID, and another agent occupies the same pane and name. The governor reattaches and sends a follow-up, or closes that pane through `cancel(closePane=true)`.

Related gaps:
- A moved child can disappear from its old pane ID without actually being lost.
- A new session can start in the same terminal.
- The first caller request cannot compare against a "recorded caller session" that does not yet exist.
- Nothing explicitly requires `observe`, `ack`, `message`, and `cancel` to check ownership of the named object.
- Handover to the child's own pane permits self-targeting loops.

**Exact edit:**

> Identity contains a proven Herdr incarnation, terminal ID, agent kind, agent name, and the complete native session once available. Pane ID is a current locator. Before declaring loss, locate a unique identity-preserving move in a fresh valid snapshot. Duplicate, malformed, unavailable, or incarnation-ambiguous observations are not absence.

Require the Phase 0 spike to establish how incarnation is proven; **do not invent a protocol field**. Without such proof, do not reuse a bare terminal ID across a server discontinuity.

Specify caller bootstrap explicitly: prefer a claimed native-session identity verified against the fresh snapshot. Enforce current ownership on every Run/event operation. Mint child names from the Run identity. Reject transfers that make a Run its own caller.

Finally, require fresh target verification before **every** prompt and close, including closing settled Runs. Retain H#14's documented residual check/send race: without conditional mutation support in Herdr, preflight plus post-state is not atomic targeting.

---

### B5. The outbox can duplicate a follow-up, misreport uncertainty, or violate ordering

**Sections:** F2 `message`, F17–F18; §9 `outbox`.

The outbox specifies at-most-once submission but lacks caller-side message idempotency, durable dispatch states, uncertainty semantics, and settlement arbitration. Its acknowledgment reports only whether delivery happened immediately. :chatgpt-content-reference{index="8"} :chatgpt-content-reference{index="9"}

**Failure sequences:**
- Enqueue succeeds, the MCP response is lost, and the caller retries: two sequence numbers now hold the same logical follow-up.
- A prompt lands and the daemon dies before updating `delivered_at`: restart resends it or wrongly expires it.
- Follow-up 1 times out, and follow-up 2 is sent while request 1 may still complete. FIFO delivery is no longer established.
- Settlement marks an already-dispatching message "expired," concealing possible consumption.
- Stall review and idle-without-handoff handling each send their own "one nudge."

**Exact edit:**

> `message` requires a caller-authored `messageKey`, unique within the Run. Repetition with the same body digest returns the existing sequence; a different digest conflicts. Follow-ups expose `queued`, `dispatching`, `submitted`, `unconfirmed`, and `expired`, with effect certainty distinct from consumption.

Use B1's dispatch protocol for follow-ups, nudges, and hints. Persist each nudge episode and hint attempt.

> All governor writes to the same captured target identity are serialized across launch prompts, follow-ups, nudges, and hints. An unconfirmed dispatch is an ordering barrier unless authoritative evidence resolves that operation. Only definitely undispatched messages may expire as unsent.

A message arriving after settlement is rejected without being enqueued. A message already accepted into the outbox must end in a visible submitted, unconfirmed, or expired outcome, never vanish.

---

### B6. Automatic recovery can produce two live writers

**Sections:** F6 recovery, F14 provider limiting, Q2.

The owner's recovery decision has a **concrete correctness defect if read as permission to launch immediately on Jev's `provider_limited` answer**. Neither "blocked" nor a provider-limit judgment proves that the predecessor cannot resume. :chatgpt-content-reference{index="10"} :chatgpt-content-reference{index="11"}

**Failure sequence:** R edits a repository, hits a temporary usage limit, and appears blocked. The governor starts R2. R retries on its own later, and both edit or deploy the same Task.

**Exact edit:**

> A provider-limit decision creates a durable, uniquely keyed recovery obligation. Recovery dispatch requires authoritative proof that the predecessor can no longer execute, or an explicit owner resolution of the missing Herdr guarantee. `blocked`, a 429, a Jev probability, and lack of recent output do not constitute that proof.

Never auto-close the pane; keep that decision. An explicit `cancel(closePane=true)` or an independently observed exit can supply the required cessation evidence.

Unify automatic and caller-requested recovery under a unique predecessor constraint. Adoption must not create another recovery opportunity. Section 3 gives the recommended settlement semantics.

---

### B7. Both rollback plans can destroy the guarantees being introduced

**Sections:** Phases 6–7, §14, N6.

The whole-cutover rollback explicitly leaves governor Runs unsupervised, which also abandons queued follow-ups and future terminal outcomes. Stopping the old daemon during forward cutover has the matching problem for its outstanding obligations. :chatgpt-content-reference{index="12"} :chatgpt-content-reference{index="13"}

The build rollback changes the binary symlink but says nothing about a database migrated by the failed release.

**Failure sequence:** Release B migrates SQLite, accepts a message or dispatches a Task, then fails its health check. Reverting to A may make the database unreadable. Restoring an earlier database snapshot may erase the dispatch record and permit a duplicate submission.

**Exact edit:**

> Integration swaps stop new admissions to the outgoing implementation, not supervision of its existing obligations. Keep that daemon available for draining Runs, mailbox events, and outboxes. Stop/archive it only after drainage or explicit owner disposition of every remaining obligation.

Before swapping names, resolve in-flight Launch requests through their original implementation. Retrying an uncertain request against the other implementation is not an idempotent replay.

For release rollback:

> Every deployable release declares its readable schema range. Migration and health validation occur behind an admission-and-effect barrier. Before that barrier is released, a consistent pre-migration backup may be restored. After any new effect or acknowledged mutation, rollback preserves the current ledger and requires a schema-compatible binary.

Use a SQLite-aware backup, not a naive copy of a live database file. SQLite provides an online backup API and `VACUUM INTO` for consistent snapshots. :chatgpt-content-reference{index="14"}

This preserves parallel cutover, parity, automatic rollback, and the 14-day window. It removes abandonment, not the owner's rollout decision.

## Major findings

### M1. Handoff versions, repair generations, and evidence freshness are undefined

**Sections:** F14–F15, F22, Q3.

A Run-specific end marker identifies which Run an artifact is for; it does not identify which revision was judged. The specification neither freezes the judged bytes nor invalidates an in-flight judgment when a repair changes the work. Also, `outside_scope` is asked while the evidence deliberately omits `scope`. :chatgpt-content-reference{index="15"}

**Failure sequences:**
- Jev evaluates handoff A. A repair is delivered and the child writes B. The judgment for A returns and accepts the Run while `observe` points to B.
- Periodic rereading of the same rejected file eventually gets a different probabilistic answer and accepts unchanged work.

**Exact edit:**

> Read a bounded, regular, non-symlink handoff whose matching marker is the final non-whitespace content. Freeze its bytes or an immutable copy and digest. Bind each assessment to Task digest, handoff digest, work generation, question version, and policy version. Do not rejudge unchanged evidence after a completed assessment merely because another cadence elapsed.

A repair dispatch invalidates pre-repair acceptance work. Late artifacts never reopen settlement.

For scope:

> Keep `scope` outside the completion-authority digest, but supply it explicitly to `outside_scope`.

For factual acceptance, use the existing transcript/git evidence where a doneWhen item depends on execution. Do not silently equate "the child claims tests passed" with independently verified execution. Free Markdown need not be replaced with a rigid report schema.

---

### M2. The SQLite schema is missing the data needed to enforce its promises

**Section:** §9 data model.

`runs.launch_id` references an identifier not declared in `launches`. Almost all primary keys, foreign keys, uniqueness constraints, lifecycle checks, and transaction groupings are unspecified. Persisted timers, ownership generations, effect receipts, and evidence versions are absent. :chatgpt-content-reference{index="16"}

**Failure scenario:** an implementation can satisfy the listed columns while permitting two Runs for one Launch, duplicate settlement events, orphan outbox rows, and repeated post-restart nudges.

**Exact edit:** Replace the illustrative list with executable DDL and the following minimum contract:

| Entity | Required additions or constraints |
|---|---|
| `launches` | Declared primary key; immutable original caller binding; canonical project root; digest-format version; canonical Task; persisted evaluation/launch phase; immutable routing decision and catalog/policy snapshot; unique scoped idempotency key. |
| `runs` | Primary key; **unique** Launch foreign key; current owner binding and ownership generation; state version; complete captured identity; canonical cwd; persisted deadlines and nudge episodes; work/evidence generation; terminal reason and timestamp. |
| Effect records | Unique effect key; operation kind; subject; target identity; payload digest/reference; dispatch state; attempted/acknowledged timestamps; typed certainty/result. These cover topology/start/prompt/close effects, not a general workflow engine. |
| `outbox` | Primary key `(run_id, seq)`; unique `(run_id, message_key)`; immutable original sender and body digest; exactly one body representation; effect reference; explicit expiry reason. |
| `mailbox` | Primary key; stable deduplication/source key; current destination, or a destination derived from Run ownership; idempotent acknowledgment. Repeated snapshots must not generate new copies of one episode. |
| `judgments` | Judgment-set ID; unique question within that set; evidence and handoff digests; evaluated Run generation; model/question/policy version; applied threshold; request outcome, including failures. |
| Recovery | Unique predecessor Run; successor Launch/Run references; pending/blocked/dispatched outcome. Do not encode the only recovery relationship in a terminal analytics row. |
| `cooldowns` | Provider key with defined uniqueness and extension rules; new observations must not accidentally shorten an existing cooldown. |

Require state consistency: a terminal Run has exactly one settlement and timestamp, and terminal settlement is immutable. Use foreign keys without destructive cascades over live obligations.

Transaction contents:
- the settlement transaction includes the terminal event and the queued-message expirations;
- provider-limit handling includes the cooldown and the recovery obligation;
- adoption includes the routing destinations.

**Never hold a SQLite transaction open across Herdr or Jev I/O.**

Set and verify `foreign_keys=ON`. WAL alone is not a host-crash durability specification. Select `synchronous=FULL` for durable effect markers across power loss, and distinguish that from the narrower SIGKILL guarantee. SQLite documents both distinctions explicitly. :chatgpt-content-reference{index="17"}

Keep idempotency tombstones for the documented replay lifetime. Garbage collection must not silently make an old key executable again.

---

### M3. Assumptions are checked after the design they can invalidate

**Sections:** A1–A5; Phases 0–5; §11.

Phase 2 implements identity, lifecycle, and outbox semantics before Phase 3 verifies the Herdr assumptions they depend on. A1 is checked after the dependency set and most adapters exist. :chatgpt-content-reference{index="18"} :chatgpt-content-reference{index="19"}

**Failure scenarios:**
- A3 fails for AGY, or A4 lacks a server-incarnation boundary. The already-approved core state machine then needs redesign.
- A1 fails, and the unspecified stdio relay reintroduces per-caller processes, contradicting N4.

**Exact edit:** Move these checks into a **contract-discovery gate before Phase 2**:

| Assumption | Required evidence |
|---|---|
| A1 | Real Executor registration, authentication, caller forwarding, reconnect behavior, and process count. If stdio is selected, specify its daemon IPC and lifecycle, and prove it does not recreate per-pane hosts. |
| A2 | Subscription plus simultaneous unary requests, disconnect/reconnect, malformed frames, and bounded timeouts. |
| A3 | Ready success, typed pre-interactive failure, and ambiguous/in-flight start behavior for every enabled harness. |
| A4 | Replacement, move, native-session replacement, and Herdr restart, not merely uniqueness during one lifetime. |
| A5 | Actual transcript identity/path resolution, partial writes, ambiguous matches, unreadability, and fallback. |

AGY is retained by the owner but absent from the real-harness conformance scope. Add its generic start/prompt/handoff qualification, or keep its operating points unavailable until that qualification passes. :chatgpt-content-reference{index="20"} :chatgpt-content-reference{index="21"}

Correct the Phase 3 DoD: only **Herdr adapter methods** can run against both fake and real Herdr. Jev, SQLite, git, and config need their own contract tests.

Add conformance scenarios for every blocker above, especially lost acknowledgments, mid-launch kills, replacement, adoption versus settlement, and file/database publication boundaries. "Every F has a named test" and two green suite runs are not substitutes.

---

### M4. The stated guardrails leave semantic test weakening and adapter bypass open

**Sections:** Phases 1–5; §9 architecture; §11.

**Failure scenario (weakened assertions):**
1. An agent changes an assertion helper to return success, keeping every test name and a nonempty test body.
2. The test inventory is unchanged.
3. Core source is unchanged, so `mutants-diff` selects no mutants.
4. The weakened checks pass, and green `main` deploys before the weekly mutation run.

This is a documented limitation of diff-selected mutation testing: test-only changes can select zero mutants, and changes elsewhere can remove coverage of unchanged code. :chatgpt-content-reference{index="22"}

**Failure scenario (adapter bypass):** settlement decisions move into the daemon or the SQL adapter. Core mutation testing then proves a reducer that production no longer relies on.

**Exact edit:**

> Existing assertion logic, test-support helpers, property generators, expected fixtures, test selection/ignore configuration, mutation exclusions, workspace membership/features, build scripts, and gate scripts are protected against unapproved weakening. Test-only changes trigger full relevant mutation testing, not an empty source-diff run.

Also require:

> Approval is tied to the reviewed commit/diff, not a reusable label. BASE inventories and enforcement logic come from a trusted revision. CI fails on missing/filtered/ignored required tests and unexpected zero-mutant runs. The conformance report identifies the tested release binary hash.

Extend the seeded cheats to include weakened helper assertions, narrowed generators, same-name no-op tests, adapter bypass, release-only behavior, altered workspace membership, and approval followed by another commit.

**Two crates are the right minimal split**, but they only enforce dependency direction, not purity or correct placement of business rules. Make the store's public mutation API apply core-produced transitions, and do not expose arbitrary lifecycle setters throughout the binary.

Workspace lint settings also require each member to opt in with `[lints] workspace = true`; verify this mechanically. :chatgpt-content-reference{index="23"}

A `no_std` + `alloc` core can strengthen the boundary, but it is optional and not a complete sandbox: explicit or transitive `std` imports remain possible. Protect the dependency graph and crate root rather than relying on grep alone. :chatgpt-content-reference{index="24"}

---

### M5. Routing has undefined precedence, unstable replay inputs, and misleading exploration

**Sections:** F5–F8, F21–F22.

**Failure sequences:**
- Recovery raises `standard` to `strong`; exploration lowers it back to `standard`, violating the recovery floor.
- A candidate passes availability once, then cools down before its start attempt.
- A config reload changes arguments or provider identity between decision and start.
- Exploration lowers the nominal start tier, but cheapest-first selection still picks the same higher-tier free point. The log says "explored" without any lower-tier execution.

The relevant rules currently appear separately, without precedence or a pinned decision snapshot. :chatgpt-content-reference{index="25"}

**Exact edit:**

> Define one ordered routing function: validate judgment → policy adjustments → caller uplift → recovery minimum/exclusions → eligible exploration → candidate selection. Exploration never violates a recovery minimum. At the highest tier, an unsatisfiable strict recovery increase abstains; it does not silently clamp.

Persist:
- the policy and catalog version;
- the actual candidate arguments;
- the original and adjusted floors;
- the requested tier;
- the exploration assignment;
- the actual selected tier;
- the fallback result.

Recheck availability and cooldown immediately before each start attempt. Exclude the presentation-only `label` from routing evidence.

Keep exploration, but record both **assignment and execution**. If the actual point does not implement the lower-tier treatment, mark that explicitly. Jev acceptance rates are operational outcomes, not independent gold labels proving minimum sufficient capability.

---

### M6. Trust-boundary and resource limits are incomplete

**Sections:** F1–F3, F15–F18, F21; N4–N5; §13.

The spec gives a result cap and file modes, but not enough rules for safe body publication, bounded reads, complete retrieval, or path access. The old harvest contains requirements in exactly these areas. :chatgpt-content-reference{index="26"} :chatgpt-content-reference{index="27"}

**Failure scenarios:**
- A queued body points to an incompletely written file.
- A sandboxed child cannot write the handoff path.
- A huge transcript exhausts memory before truncation.
- Status exceeds 60,000 bytes and silently hides unread events.

**Exact edit:**

> Canonicalize and validate projectRoot and cwd before effects. Keep the trusted project anchor separate from caller-selected cwd. Apply strict schemas to every tool/action object, not just Task.

> Publish follow-up files immutably before committing an outbox reference. Verify size and digest before dispatch. A failed publication enqueues nothing. Retain files referenced by pending or possibly consumed messages; logical settlement alone does not prove that a still-live child no longer needs them.

Define numeric limits for input, file, frame, evidence, and serialized-response sizes, and reject or truncate according to a documented per-boundary rule. Preserve the mandatory Task authority when trimming evidence.

Add bounded, non-destructive pagination to the status and outbox projections. Record this as an explicit replacement of H#84's old no-cursor behavior, while keeping no unread eviction.

Require capability qualification for the **actual catalog arguments**: handoff-directory write access, follow-up read access, mid-turn input, and hint consumption. This is catalog and contract-test work, not a reason to restore per-harness launch code.

For HTTP, add Origin validation and test invalid origins. Loopback plus a bearer token does not fully state the MCP transport's security requirements. :chatgpt-content-reference{index="28"}

Finally, scrub the research files before the Phase 1 public push, not just the transcript fixtures. The supplied research itself contains host-specific home paths that §5 forbids. :chatgpt-content-reference{index="29"}

---

### M7. The claim that the harvest is carried over is materially too broad

**Sections:** §8, §9; harvest H#1–110.

The spec says the harvest's verdicts are carried over except where the owner overrode them. That statement hides important partial omissions. :chatgpt-content-reference{index="30"}

I treated an explicitly cited H# as incorporated unless the new text contradicts or materially narrows it. The table below names the **remaining silent or partial weakenings**. "Restore" means restore the property, not the old machinery.

| Harvested invariant(s) | Missing/weakened property; disposition |
|---|---|
| **H#1, #7, #106** | Activation boundary, initial owner activation, bounded sanitized startup failure. **Safely replace** the old `HERDR_ENV` gate for a user daemon, but explicitly define activation and startup behavior. Automatic approved deploys need not require fresh manual activation. |
| **H#3, #21, #23, #24, #52, #92** | Canonical root; complete caller binding; authoritative sender provenance; self-target refusal; runtime-minted names; observation ownership/artifact validation. **Restore** these properties through the new caller/Run contract. Old observation result flags are unnecessary. |
| **H#11, #12, #13, #14, #15, #30, #42** | Bounded operations; typed malformed-output failure; authoritative post-state; acknowledged non-atomic targeting; dispatch certainty; identity-matched acknowledgments; bounded start duration. **Restore** at the socket boundary. Do not restore CLI pollers. |
| **H#17, #34, #85, #86, #87, #88** | Fresh safe-state gating; restrictions after an unconfirmed initial submission; exact hint recipient; inert unqualified hints; real hint-consumption qualification; Devin permission/preapproval prerequisite. **Restore through catalog qualification and generic dispatch rules**, not harness branches. |
| **H#20, #74, #75, #89** | Full acquired identity; invalid-versus-absent observations; identity-preserving moves; exact restart attachment. **Unsafe to drop.** A separate downtime mailbox event may be replaced by health/freshness reporting. |
| **H#36, #37, #44, #69** | Supervision obligation before topology, safe binding, and retention after a later launch failure. **Unsafe to drop.** A durable Run reservation replaces the old supervisor/job reservation. |
| **H#41, #46, #49, #50, #51** | Label non-interference; immediate availability reprobe; complete tier evidence; recovery floor; durable decision before mutation. **Restore**, with the new cost ordering and question set explicitly retained. |
| **H#55, #56** | Failed-launch evidence and created-target reporting. **Restore Run/topology references and certainty.** Replica arrays, worktree fields, and supervisor-job identifiers can disappear. |
| **H#62, #63, #64, #66, #67** | Bounds, atomic publication, crash consistency, retention of live references, readable recipient paths, and body-leak restrictions beyond logs. **Restore the relevant properties for follow-up files.** Multi-process leases and launch-grant protocols can be deleted. |
| **H#79, #80, #82** | Freshness and in-flight review invalidation; deterministic policy authority rather than reviewer-granted permissions; pre-effect git baseline and accurate unavailable/dirty evidence. **Restore these properties.** The old predicate taxonomy and fixed thresholds need not survive. |
| **H#84, #90, #91, #105** | Durable non-evicting mailbox; in-flight idempotency behavior; crash-safe ownership transfer; serialized mutations. **Unsafe to drop.** SQLite replaces the journals; cooperative adoption can explicitly replace the owner-instruction claim file. |
| **H#102** | Internal close protections and handling the initial pane returned by tab creation. **Retain for the operations that remain**, especially `cancel(closePane)`. No pane/tab management tool is needed. |
| **H#103, #104** | Strict complete schemas, error-channel semantics, bounded transport/evidence/output, and sanitized failures. **Restore in the MCP/socket adapters.** Host-specific TypeBox and error-string shims are unnecessary. |
| **H#110** | Effective capability/permission claims. **Do not silently downgrade these to unverified tags.** The old role-specific hidden-delegation restrictions may be explicitly externalized or retired; any remaining safety claim must match the actual catalog arguments. |

The affected identity, effect, supervision, and persistence requirements are explicit in the harvest; they are not inferred legacy preferences. :chatgpt-content-reference{index="31"} :chatgpt-content-reference{index="32"} :chatgpt-content-reference{index="33"}

**Not silent omissions.** These are explicitly replaced, removed, or externalized:
- the old daemon handshake and CLI mutation implementation;
- friendly target resolution and `current`;
- raw keys and turn control;
- the governor readiness/confirmation pollers;
- replicas;
- the attachment-store/grant implementation;
- the AGY-only provisional machinery;
- the revision-gap taxonomy;
- observation-only supervision;
- the old provider detector;
- the six-section handoff;
- waits, jobs, and review ownership;
- role packaging.

This covers **H#9–10, #16, #18–19, #28, #32–33, #35, #43, #54, #57–61, #65, #68, #72, #76–78, #83, #93–99, and #109**. Their residual safety properties are identified above; their implementations should not be resurrected.

**Exact edit:** Replace §8's blanket carry-over claim with a normative disposition table: `retained`, `replaced by <F requirement>`, or `retired by <decision>`, including the residual properties above.

## Minor findings

### m1. The research does not establish the causal claims used to justify the rewrite

**Sections:** §1, §9 minimality, §15; N3 and performance validation.

The evidence is weaker than the spec's wording:
- The baseline explicitly cannot distinguish child death, artifact loss, and missed lifecycle tracking for the stale Runs.
- The durable daemon had only about one day of evidence and 17 completed intents, with no recorded failed or unresolved intents.
- "38% never read" overstates what is a point-in-time unread count. :chatgpt-content-reference{index="34"} :chatgpt-content-reference{index="35"}

**Failure scenario:** the rewrite "fixes" a presumed caller-lifetime cause while reproducing the actual handoff/identity defect, then declares success from incomparable metrics.

**Exact edit:** Say "54% had stale unsettled records; causal attribution was not established" and "38% were unread at collection." Present the daemon as eliminating caller-lifetime coupling, not as a proven explanation for that percentage.

Measure end-to-end launch latency with the same start/end points and comparable workloads. Fake-Herdr timings establish governor overhead, not the production p95 that includes routing and readiness. The baseline itself distinguishes several timing populations. :chatgpt-content-reference{index="36"}

### m2. Two small architectural statements should be tightened

**Sections:** §9 core/config and N8.

**Defects:**
- Core config is described as parsed from a string, but TOML parsing is absent from its allowed dependencies.
- The harness-literal grep can pass code that branches on concatenated names or operating-point IDs.

**Exact edit:** Put TOML decoding in the config adapter; the core receives typed values and validates them. Describe N8's lexical check as a tripwire, not proof. Add a metamorphic test: renaming opaque harness identifiers while keeping the declared capabilities cannot change non-transcript behavior.

# 3. Recommendations for Q2 and Q3

## Q2: Settle the predecessor as `provider_limited`; record recovery separately

I recommend **adding `provider_limited`**, rather than making `recovered` the predecessor's terminal state.

`recovered` makes settlement depend on a successor succeeding. That recreates an unsettled predecessor when no candidate is available, Jev abstains, the daemon crashes, or the successor's prompt is unconfirmed.

Use this contract:

> A sufficiently supported provider-limit decision settles the predecessor `provider_limited`. The same transaction records cooldown, the terminal notification, and a unique recovery obligation. Recovery status is independently `pending`, `blocked`, `dispatched`, or `failed`; `recovered_by` is populated only when an actual successor Run exists, with its launch certainty preserved.

Apply B6's cessation gate before successor dispatch. Preserve the predecessor's evidence and original Task. Recovery instructions must continue from observed state rather than blindly repeat side effects that already happened.

Pending predecessor follow-ups must receive an explicit disposition. The minimal rule is **expire and notify, never silently transfer**; the caller can address the successor explicitly.

This separates three facts that should not share one enum: why the old attempt ended, whether another attempt was requested, and whether another attempt actually started.

## Q3: A rejected handoff opens a bounded repair opportunity

Keep a nonterminal `handoff_rejected`, but make its timing precise:

> The first rejection in a work generation establishes a 15-minute repair deadline. Reobserving or rewriting the same rejected content, enqueueing an undeliverable follow-up, daemon restart, and repeated rejected assessments do not extend it.

A repair dispatched before the deadline:
- creates a new work generation and invalidates earlier acceptance work;
- does **not** reset the absolute Run deadline;
- if unconfirmed, remains explicitly uncertain and is not resent.

At the repair deadline, absent a qualifying repair transition, settle `rejected`. Expire only undispatched messages and preserve uncertain dispatch records. A later handoff cannot reopen the Run.

If judgment cannot be obtained at all, use `unresolved(reason=judgment_unavailable)`, not `rejected`: the lack of a decision is not a negative decision.

# 4. Two-track comparison and recommendation

## Current-system path: repair the TypeScript daemon in place

The smallest credible in-place solution is **not** a broad TypeScript refactor.

- **Keep:** the live daemon, the three-tool surface, the existing identity and socket code, the intent store, the ownership journal, and the file-based state.
- **Delete:** only code shown to be unreachable from the actual daemon/launch handler, not everything described as legacy by name. The supplied inventory itself distinguishes live older deployments and shared launch internals. :chatgpt-content-reference{index="37"} :chatgpt-content-reference{index="38"}
- **Lifecycle:** implement the corrected lifecycle as a small reducer.
- **State:** extend each authoritative Run state file to hold its outbox, event/ack state, deadlines, and outcome, published through one existing atomic-write implementation. Treat mailbox listings and outcome reports as projections, not additional authoritative ledgers.
- **Journals:** retain the existing pre-effect intent and ownership journals for operations spanning records. Recovery uses a deterministic predecessor-derived intent key. Startup reconstructs missing projections and finishes journaled transfers before admitting calls.
- **Serving:** move MCP serving into the daemon using the verified Executor transport.
- **Supervision:** replace the classifier-style supervisor with the required action-driving questions, retain the parsers, and move non-parser harness behavior into Herdr/catalog data.

The cost is explicit. File consistency across Launch, Run, ownership, cooldown, and recovery still requires hand-written recovery protocols. An equivalent TypeScript mutation/anti-weakening gate must also be built; the attached ADR says the kit's TypeScript pack is empty. :chatgpt-content-reference{index="39"}

This path can satisfy the **behavioral requirements**. It cannot literally satisfy "all Rust and SQLite"; that is the architectural exception built into the requested counterfactual comparison.

## Greenfield path: two crates, one state-transition owner, SQLite, typed effects

My independent greenfield design largely agrees with the accepted architecture, but its central abstraction would be the **durable transition plus external-effect protocol**, not separate launch and supervision pipelines.

**`governor-core`** holds typed Task validation, deterministic routing, identity comparison, total lifecycle transitions, deadlines, and the decisions to create events and effects.

**One binary crate** holds MCP, Herdr, Jev, the transcript/git/config adapters, and SQLite:
- one coordinator owns state transitions;
- external I/O runs asynchronously and returns versioned results;
- writes to an individual target are serialized;
- no database transaction spans network I/O.

**State:**
- SQLite holds authoritative state and effect receipts.
- Follow-up files and handoffs are the only child-facing files.
- The mailbox and outcome analysis are projections of durable facts.
- One shared subscription and coalesced snapshot reconciliation replace the old event-gap machinery.

There is no generic plugin framework, workflow engine, per-adapter crate, distributed lease system, or compatibility layer.

## Comparison

| Dimension | In-place TypeScript | Corrected greenfield |
|---|---|---|
| **Correctness** | Existing targeting behavior reduces rediscovery risk, but cross-file atomicity remains difficult. | Atomic Run/event/outbox/recovery transitions are simpler; effect uncertainty still needs explicit handling. |
| **Complexity** | Smaller initial patch; larger inherited conceptual surface and recovery protocols. | More initial implementation; smaller intended runtime model. |
| **Maintainability** | Keeps working code and history; equivalent guardrails require new work. | Better fit for the selected kit and an enforceable core boundary. |
| **Justified extensibility** | Existing seams suffice, but inherited harness paths need deletion. | New operating points are data; a new transcript format affects one adapter. No speculative extension system. |
| **Operational burden** | Familiar install and state; continued file-journal repair and inspection. | One database and binary, but schema-compatible rollback must be implemented correctly. |
| **Performance** | Removing per-pane MCP hosts addresses the largest observed resource cost without a language rewrite. | Likely lower baseline overhead, but not established by the supplied evidence; bounded I/O matters more. |
| **Delivery risk** | Lower adapter rediscovery risk; greater danger of interacting legacy semantics. | Higher protocol/behavior rediscovery risk; early contract spikes materially reduce it. |
| **Migration/cutover cost** | No state-format cutover if records evolve compatibly. | Parallel integration, explicit drainage, schema discipline, and two-system rollback handling. |

The measured per-pane hosts account for about 1.6 GB. That benefit comes from the serving topology, not automatically from Rust. :chatgpt-content-reference{index="40"}

**Recommendation: corrected greenfield.** It serves the stated priority order better, especially transaction comprehensibility and safety for agent maintenance. The evidence does **not** prove that the new TypeScript daemon is intrinsically unreliable; the recommendation rests on the desired long-term persistence and enforcement model.

**What would flip the recommendation:**
- The TypeScript path passes the *same* crash-boundary, ownership-race, uncertainty, conformance, and seeded-cheat suite; consolidates rather than adds authoritative persistence layers; and demonstrates an owner-acceptable validated guardrail equivalent.
- Or a Herdr contract assumption fails, and the owner declines the necessary upstream fix or an explicitly scoped exception. That would favor keeping an already-proven TypeScript behavior.

Merely being faster to patch would not flip it.

# 5. Deletions: what to cut

- **Cut the separate authoritative `outcomes` table.** Make it a view or rebuildable projection once Runs, judgments, and effect records hold the necessary immutable facts. This removes one consistency obligation.
- **Cut the claim that the two-crate split or the harness-literal grep proves architectural purity.** Keep the split and the cheap tripwire; enforce the meaningful boundaries described above.
- **Cut byte-for-byte Jev fixture equality as the sole wire oracle.** Keep frozen representative fixtures, but validate structural wire compatibility, probabilities, errors, and deliberate normalization of dynamic fields.
- **Cut the dependency that delays guardrail scaffolding until the old deletion PR is deployed.** Keep the owner-required TypeScript deletions before porting; fixture discovery and the protected Rust skeleton can proceed independently.
- **Cut "rollback means unsupervised Runs," the causal claims the baseline doesn't support, and the blanket "all harvest verdicts carried over" sentence.** They hide obligations rather than simplify the implementation.

Do **not** cut the two-crate boundary, the durable outbox, the identity checks, the effect journal, transactional settlement, or the crash/conformance tests. They are the minimum mechanisms that make this rewrite materially more reliable than the system it replaces.

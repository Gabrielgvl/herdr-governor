# Herdr Governor

A durable daemon that launches coding agents into Herdr panes, routes each Task to an operating point, and supervises the resulting Run until its handoff is accepted.

## Language

### Delegation

**Task**:
The caller-authored unit of delegated work: an `objective`, a `scope`, `doneWhen` conditions, and optional `constraints`.
_Avoid_: spec, assignment, profile, role, preset

**doneWhen**:
The Task's concrete, externally judgeable completion conditions; the only completion authority for admission, supervision, and handoff acceptance.
_Avoid_: verification, success spec, checklist

**Launch**:
One caller request to start a Task, identified by the caller's idempotency key within that caller's scope. A Launch has at most one Run; an abstained or rejected Launch has none.
_Avoid_: launchId, batch, intent (for the durable launch record)

**Run**:
One child agent executing a Launch's Task in its own pane, identified by its run ID everywhere.
_Avoid_: child target, child name, job, supervisor job

**Handoff**:
The completion artifact a Run writes for its caller; Jev judges it against the Task's `doneWhen`.
_Avoid_: result, report, output

**Settlement**:
The single, immutable terminal state every Run reaches: handoff accepted, handoff rejected, no handoff, pane lost, cancelled, provider limited, or unresolved (with a reason). A Run past its maximum age settles unresolved rather than being given a verdict nobody made.
_Avoid_: completion, close, done

**Repair window**:
The bounded time after a handoff is first rejected, during which a follow-up can repair the work before the Run settles as rejected.
_Avoid_: grace period, retry window

**Recovery**:
A Launch that continues a settled Run's Task at a higher tier and on another provider. It starts only once the predecessor is proven to have stopped.
_Avoid_: retry, fallback, relaunch

**Caller**:
The agent session that launches Tasks and owns the resulting Runs.
_Avoid_: manager, run owner, parent

**Adoption**:
A caller taking over unsettled Runs, either handed over by a live caller or named explicitly after Herdr shows the previous caller's session is gone.
_Avoid_: transfer, claim, takeover

**Owner**:
The human who authors the catalog and makes the decisions agents may not make.
_Avoid_: user, admin, operator

**Mailbox**:
A caller's durable list of actionable events about its Runs, such as a handoff ready for review or a Run settled without a handoff.
_Avoid_: inbox, notifications, wake

**Follow-up**:
A message a caller sends to one of its Runs after launch, held in that Run's outbox until it can be delivered safely.
_Avoid_: steer, prompt, communicate

**Outbox**:
A Run's durable, ordered queue of follow-ups awaiting delivery.
_Avoid_: queue, pending messages

**Provenance envelope**:
The fixed header that marks text delivered to an agent as agent-authored, so it is never mistaken for an instruction from the owner.
_Avoid_: wrapper, banner, signature

**Nudge**:
The single bounded prompt supervision sends to a Run it judges stalled or idle without a handoff.
_Avoid_: ping, reminder, steer

### Routing

**Jev**:
TypeSafe's decision-only model: typed questions in, calibrated probabilities out. Jev judges Tasks, Runs, and Handoffs; it never judges models.
_Avoid_: classifier, LLM, router

**Quality tier**:
The stable contract between Task judgments and operating points. Jev judges the weakest sufficient tier for a Task; the catalog assigns each operating point a tier.
_Avoid_: category, class, model size

**Harness**:
An agent CLI that Herdr can start and detect, such as claude, devin, or pi. The governor is agnostic to harnesses except when reading transcripts: everything else harness-specific belongs to Herdr's integrations or to catalog data.
_Avoid_: runner, runtime, adapter

**Transcript**:
A harness's own session record of a Run, located through the session identity Herdr reports and parsed per harness into evidence for supervision.
_Avoid_: trace, log, session file

**Operating point**:
A harness plus its launch arguments (model, reasoning setting, permissions), rated by the catalog by quality tier, capability tags, cost class, and provider.
_Avoid_: model, profile, runner default

**Catalog**:
The owner-authored list of operating points. Adding a model means adding one operating point with a provisional tier.
_Avoid_: registry, model list

**Capability**:
A property an operating point offers and a Task may require, such as file edits, web access, or long runs. A capability counts only while a qualification backs it.
_Avoid_: feature, permission, skill

**Qualification**:
A passing check that an operating point's exact launch arguments really provide the capabilities it claims.
_Avoid_: certification, smoke test

**Routing policy**:
The owner-authored rules that turn Jev's answers about a Task into a starting quality tier and the capabilities it requires.
_Avoid_: workload profile, intent, heuristics

**Provider**:
The service whose usage limits an operating point consumes; operating points sharing a provider cool down together.
_Avoid_: vendor, account

**Cooldown**:
A period during which every operating point of one provider is excluded from routing, entered when Jev judges a blocked Run to be stopped by that provider's usage limit.
_Avoid_: quota block, rate limit, availability

### Measurement

**Outcome**:
The observed result of a Run that labels the Jev judgments made about it, such as a handoff accepted on first review or a Run that needed recovery.
_Avoid_: gold label, ground truth, score

**Exploration**:
Deliberately starting a low-risk Task one quality tier below its routed start, so outcomes can reveal over-routing.
_Avoid_: experiment, A/B test, downgrade

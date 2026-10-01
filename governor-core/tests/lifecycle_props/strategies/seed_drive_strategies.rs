//! The scripted half of the seeded worlds: `Drive` replays each
//! `SeedInputs` through the real public transitions — `launch_plan` for
//! `reserved → starting`, then fresh-stamped effect results,
//! observations, handoffs and judgments — so the journal, the frozen
//! handoffs and every record field are consistent with how the seed's
//! state is actually reached.

use governor_core::config::Policy;
use governor_core::identity::{ChildIdentity, Digest, EffectId, EffectKey, Observation, Timestamp};
use governor_core::lifecycle::{
    Effect, EffectKind, EffectOutcome, EffectReceipt, EffectResult, EffectState, EffectTarget,
    Event, JudgmentVerdict, Run, launch_plan, transition,
};
use governor_core::routing::Decision;

use super::common_strategies::{FREEZE_PATH, Sim, test_policy};
use super::event_strategies::run_key;
use super::seed_strategies::{LaunchDepth, OutboxSeed, SeedInputs, SeedTarget, SeededPrefix};
use super::stamp_strategies::{StampSpec, stamped};

/// One scripted drive through the public transition function: `fire`
/// stamps an event fresh against the live row and commits the outcome to
/// `sim`; `commit_dispatches` plays the shell's F8 dispatch commit
/// (`planned` → `dispatching` at `now`); `advance` moves the clock. One
/// second elapses per fired event so journaled times stay ordered.
struct Drive {
    sim: Sim,
    now: Timestamp,
    policy: Policy,
}

/// The drive's tick — one second per scripted event.
const STEP_MS: u64 = 1_000;

impl Drive {
    fn new(reserved: Run, decision: Decision, start: Timestamp) -> Self {
        Self {
            sim: Sim {
                run: reserved,
                journal: Vec::new(),
                handoffs: Vec::new(),
                decision: Some(decision),
            },
            now: start,
            policy: test_policy(),
        }
    }

    /// Deliver `event` stamped fresh at the live row, commit the
    /// transition, and tick the clock.
    fn fire(&mut self, event: Event) {
        let stamped = stamped(&self.sim.run, event, StampSpec::Fresh);
        let outcome = transition(
            &self.sim.run,
            &stamped,
            self.now,
            &self.policy,
            (
                self.sim.decision.as_ref(),
                &self.sim.journal,
                &self.sim.handoffs,
            ),
            FREEZE_PATH,
        );
        self.sim.apply(&outcome);
        self.advance(STEP_MS);
    }

    /// The F8 dispatch commit: every still-`planned` row goes
    /// `dispatching` at `now`.
    fn commit_dispatches(&mut self) {
        for row in &mut self.sim.journal {
            if row.state == EffectState::Planned {
                row.state = EffectState::Dispatching;
                row.dispatched_at = Some(self.now);
            }
        }
    }

    /// Advance the drive clock by `ms`.
    fn advance(&mut self, ms: u64) {
        self.now = Timestamp(
            self.now
                .0
                .saturating_add(i64::try_from(ms).unwrap_or(i64::MAX)),
        );
    }
}

/// An `effect_result` event delivering `outcome` for `key`.
fn result(
    key: EffectKey,
    kind: EffectKind,
    outcome: EffectOutcome,
    receipt: Option<EffectReceipt>,
) -> Event {
    Event::EffectResult(EffectResult {
        key,
        kind,
        outcome,
        receipt,
    })
}

/// The receipt the acknowledged topology effect produces — `tab` yields
/// the tab and the pane that hosts the child (H#102), `split` the pane it
/// created.
fn topology_receipt(kind: EffectKind, inputs: &SeedInputs) -> EffectReceipt {
    match kind {
        EffectKind::TabCreate => EffectReceipt::TabCreated {
            tab: inputs.tab.clone(),
            pane: inputs.pane.clone(),
        },
        EffectKind::JevEvaluate
        | EffectKind::PaneSplit
        | EffectKind::AgentStart
        | EffectKind::Prompt
        | EffectKind::Close => EffectReceipt::PaneCreated {
            pane: inputs.pane.clone(),
        },
    }
}

/// The journaled repair follow-up (`run:<id>:outbox:<seq>` prompt) the
/// store committed `dispatching` at `dispatched_at` — landing inside
/// `[rejected_at, repair_deadline)` is what qualifies it (F24).
fn outbox_row(run: &Run, identity: &ChildIdentity, dispatched_at: Timestamp) -> Effect {
    let key = run_key("outbox:0");
    Effect {
        id: EffectId(format!("eff:{}", key.0)),
        key,
        kind: EffectKind::Prompt,
        subject_launch: Some(run.launch.clone()),
        subject_run: Some(run.id.clone()),
        target: Some(EffectTarget::Child(identity.clone())),
        payload_digest: None,
        state: EffectState::Dispatching,
        certainty: None,
        receipt: None,
        dispatched_at: Some(dispatched_at),
    }
}

/// `reserved → starting` (F13/F14): the atomic plan write journals the one
/// topology effect and moves the Run — then the dispatch commit, and
/// optionally the topology acknowledgement (which plans `start:0`) and a
/// pre-interactive start failure that walks to the next candidate.
fn drive_launch(drive: &mut Drive, inputs: &SeedInputs) {
    let write = launch_plan(
        &drive.sim.run,
        &inputs.decision,
        &inputs.plan,
        &inputs.caller_pane,
    );
    drive.sim.apply(&write);
    drive.commit_dispatches();
    if inputs.start_depth == LaunchDepth::TopologyInFlight {
        return;
    }
    let Some(topology) = drive.sim.journal.first() else {
        return;
    };
    let (key, kind) = (topology.key.clone(), topology.kind);
    let receipt = Some(topology_receipt(kind, inputs));
    drive.fire(result(key, kind, EffectOutcome::Acknowledged, receipt));
    drive.commit_dispatches();
    if inputs.start_depth == LaunchDepth::StartWalked {
        drive.fire(result(
            run_key("start:0"),
            EffectKind::AgentStart,
            EffectOutcome::PreInteractiveFailed,
            None,
        ));
        drive.commit_dispatches();
    }
}

/// `starting → prompting` (F15/F16): the pending `agent_start` result
/// acknowledges with its captured identity; the task prompt plans — and
/// usually dispatches.
fn drive_started(drive: &mut Drive, inputs: &SeedInputs) {
    let Some(started_row) = drive
        .sim
        .journal
        .iter()
        .rev()
        .find(|row| row.kind == EffectKind::AgentStart)
    else {
        return;
    };
    let key = started_row.key.clone();
    drive.fire(result(
        key,
        EffectKind::AgentStart,
        EffectOutcome::Acknowledged,
        Some(EffectReceipt::AgentStarted {
            identity: inputs.identity.clone(),
        }),
    ));
    if inputs.prompt_dispatched {
        drive.commit_dispatches();
    }
}

/// `prompting → active` (F16): the task prompt's result — acknowledged or
/// not, the Run goes `active` with its certainty recorded.
fn drive_prompt(drive: &mut Drive, inputs: &SeedInputs) {
    drive.fire(result(
        run_key("prompt:task"),
        EffectKind::Prompt,
        inputs.prompt_outcome,
        None,
    ));
    drive.commit_dispatches();
}

/// An optional `unique` observation in `active` — `idle`/`done` opens the
/// idle episode (`idle_deadline` arms) and earns its one nudge (F23/F25);
/// `blocked` opens the episode's `provider_limited` ask.
fn drive_observation(drive: &mut Drive, inputs: &SeedInputs) {
    let Some(status) = inputs.obs_status else {
        return;
    };
    drive.fire(Event::Obs {
        observation: Observation::Unique {
            status: Some(status),
            pane: inputs.pane.clone(),
            native_session: inputs.session.clone(),
        },
        handoff_reading: None,
    });
    drive.commit_dispatches();
}

/// `active → judging` (F24): freeze the handoff digest; the acceptance ask
/// plans and dispatches. `assessed` flips the frozen row's read-side flag.
fn drive_freeze(drive: &mut Drive, inputs: &SeedInputs) {
    drive.fire(Event::Handoff {
        digest: inputs.digest,
    });
    drive.commit_dispatches();
    if inputs.assessed
        && let Some(row) = drive.sim.handoffs.last_mut()
    {
        row.assessed = true;
    }
}

/// `judging → repair` (F24): the verdict rejects; `repair_deadline` and
/// `rejected_at` arm once for the work generation — then the optional
/// outbox follow-up the store already committed `dispatching`, on the
/// window's lower or upper edge.
fn drive_reject(drive: &mut Drive, inputs: &SeedInputs) {
    drive.fire(Event::Judgment(JudgmentVerdict::Reject));
    let Some((rejected, deadline)) = drive.sim.run.rejected_at.zip(drive.sim.run.repair_deadline)
    else {
        return;
    };
    let dispatched_at = match inputs.outbox {
        OutboxSeed::Absent => return,
        OutboxSeed::InWindow => rejected,
        OutboxSeed::PastWindow => deadline,
    };
    drive
        .sim
        .journal
        .push(outbox_row(&drive.sim.run, &inputs.identity, dispatched_at));
}

/// `repair → judging` (F24): a different digest re-freezes while
/// `repair_deadline` keeps running — a second freeze row joins the
/// handoffs.
fn drive_refreeze(drive: &mut Drive, inputs: &SeedInputs) {
    drive.fire(Event::Handoff {
        digest: inputs.re_digest,
    });
    drive.commit_dispatches();
    if inputs.assessed
        && let Some(row) = drive.sim.handoffs.last_mut()
    {
        row.assessed = true;
    }
}

/// Replay `inputs` through the real transition paths up to `target` —
/// every written field, journaled row and frozen handoff is whatever the
/// transitions themselves committed. `start` lands past every journaled
/// time; `post_ms` can push it past an armed deadline, so a prefix may
/// open with the deadline already overdue.
pub(crate) fn seed_world(mut inputs: SeedInputs) -> SeededPrefix {
    // deeper states need the topology acknowledged and the start answered
    if inputs.target >= SeedTarget::Prompting && inputs.start_depth == LaunchDepth::TopologyInFlight
    {
        inputs.start_depth = LaunchDepth::TopologyAcked;
    }
    // a re-freeze must name a different digest
    if inputs.re_digest == inputs.digest {
        inputs.re_digest = Digest(inputs.digest.0.map(|byte| byte.wrapping_add(1)));
    }
    let mut drive = Drive::new(
        inputs.reserved.clone(),
        inputs.decision.clone(),
        inputs.start,
    );
    if inputs.target >= SeedTarget::Starting {
        drive_launch(&mut drive, &inputs);
    }
    if inputs.target >= SeedTarget::Prompting {
        drive_started(&mut drive, &inputs);
    }
    if inputs.target >= SeedTarget::Active {
        drive_prompt(&mut drive, &inputs);
        drive_observation(&mut drive, &inputs);
    }
    match inputs.target {
        SeedTarget::Reserved
        | SeedTarget::Starting
        | SeedTarget::Prompting
        | SeedTarget::Active => {}
        SeedTarget::Judging => {
            drive_freeze(&mut drive, &inputs);
            if inputs.via_repair {
                drive_reject(&mut drive, &inputs);
                drive_refreeze(&mut drive, &inputs);
            }
        }
        SeedTarget::Repair => {
            drive_freeze(&mut drive, &inputs);
            drive_reject(&mut drive, &inputs);
        }
    }
    drive.advance(inputs.post_ms);
    SeededPrefix {
        run: drive.sim.run,
        journal: drive.sim.journal,
        handoffs: drive.sim.handoffs,
        decision: drive.sim.decision,
        start: drive.now,
        steps: inputs.steps,
    }
}

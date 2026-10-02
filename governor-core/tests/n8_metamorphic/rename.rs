//! The `Rename` application methods — the metamorphic relation itself.
//! Each method maps the harness kinds and operating-point ids that appear
//! in its input through the generated bijection; every other field is
//! preserved. The methods were file-private when the impl lived next to
//! the property; the multi-file layout needs `pub(crate)` for `tests.rs`.

use governor_core::config::{Catalog, Config, OperatingPoint, OperatingPointId, Qualification};
use governor_core::identity::{AgentKind, ChildIdentity};
use governor_core::lifecycle::{
    Effect, EffectReceipt, EffectResult, EffectTarget, EffectWrite, Event, Run, RunUpdate,
    StateChange, Transition, Versioned,
};
use governor_core::routing::{Candidate, Decision};
use governor_core::task::Launch;

use crate::strategies::{Rename, World};

/// The image of a name under a bijection; undeclared names pass through so
/// the map stays total over stray ids.
fn image(map: &std::collections::BTreeMap<String, String>, name: &str) -> String {
    map.get(name)
        .map_or_else(|| String::from(name), String::clone)
}

impl Rename {
    /// The image of a harness kind under the renaming.
    pub(crate) fn kind(&self, kind: &AgentKind) -> AgentKind {
        AgentKind(image(&self.kinds, kind.0.as_str()))
    }

    /// The image of an operating-point id under the renaming.
    pub(crate) fn op(&self, id: &OperatingPointId) -> OperatingPointId {
        OperatingPointId(image(&self.ops, id.0.as_str()))
    }

    /// The renamed config: ids and harness kinds map; capabilities, cost
    /// classes, tiers, args, providers and catalog order are preserved.
    pub(crate) fn config(&self, config: &Config) -> Config {
        Config {
            catalog: Catalog {
                operating_points: config
                    .catalog
                    .operating_points
                    .iter()
                    .map(|point| self.point(point))
                    .collect(),
            },
            ..config.clone()
        }
    }

    fn point(&self, point: &OperatingPoint) -> OperatingPoint {
        OperatingPoint {
            id: self.op(&point.id),
            harness: self.kind(&point.harness),
            ..point.clone()
        }
    }

    pub(crate) fn qualification(&self, qualification: &Qualification) -> Qualification {
        Qualification {
            operating_point: self.op(&qualification.operating_point),
            ..qualification.clone()
        }
    }

    pub(crate) fn launch(&self, launch: &Launch) -> Launch {
        Launch {
            decision: launch.decision.as_ref().map(|d| self.decision(d)),
            ..launch.clone()
        }
    }

    /// The renamed decision: candidate ids and harness kinds map.
    pub(crate) fn decision(&self, decision: &Decision) -> Decision {
        Decision {
            candidates: decision
                .candidates
                .iter()
                .map(|candidate| self.candidate(candidate))
                .collect(),
            ..decision.clone()
        }
    }

    fn candidate(&self, candidate: &Candidate) -> Candidate {
        Candidate {
            operating_point: self.op(&candidate.operating_point),
            harness: self.kind(&candidate.harness),
            ..candidate.clone()
        }
    }

    /// The renamed run record: the started operating point and the captured
    /// child's harness kind map; everything else stays.
    pub(crate) fn run(&self, run: &Run) -> Run {
        Run {
            operating_point: run.operating_point.as_ref().map(|id| self.op(id)),
            identity: run.identity.as_ref().map(|id| self.identity(id)),
            ..run.clone()
        }
    }

    fn identity(&self, identity: &ChildIdentity) -> ChildIdentity {
        ChildIdentity {
            agent_kind: self.kind(&identity.agent_kind),
            ..identity.clone()
        }
    }

    /// The renamed transition output: every run write, effect receipt and
    /// planned effect's child target maps; mailbox events carry no catalog
    /// ids and pass through.
    pub(crate) fn transition(&self, transition: &Transition) -> Transition {
        Transition {
            state_changes: transition
                .state_changes
                .iter()
                .map(|change| self.state_change(change))
                .collect(),
            events: transition.events.clone(),
            effects: transition
                .effects
                .iter()
                .map(|effect| self.effect(effect))
                .collect(),
        }
    }

    fn state_change(&self, change: &StateChange) -> StateChange {
        match change {
            StateChange::BindCaller(binding) => StateChange::BindCaller(binding.clone()),
            StateChange::RecordLaunch(launch) => StateChange::RecordLaunch(self.launch(launch)),
            StateChange::ReserveRun(run) => StateChange::ReserveRun(self.run(run)),
            StateChange::UpdateRun(update) => StateChange::UpdateRun(RunUpdate {
                expected_version: update.expected_version,
                record: self.run(&update.record),
            }),
            StateChange::ChangeOwner(owner) => StateChange::ChangeOwner(owner.clone()),
            StateChange::WriteEffect(write) => StateChange::WriteEffect(EffectWrite {
                key: write.key.clone(),
                state: write.state,
                certainty: write.certainty,
                receipt: write.receipt.as_ref().map(|r| self.receipt(r)),
            }),
            StateChange::WriteFollowUp(write) => StateChange::WriteFollowUp(write.clone()),
            StateChange::ExpireFollowUps { run, reason } => StateChange::ExpireFollowUps {
                run: run.clone(),
                reason: *reason,
            },
            StateChange::RecordRecovery(obligation) => {
                StateChange::RecordRecovery(obligation.clone())
            }
            StateChange::SetCooldown(cooldown) => StateChange::SetCooldown(cooldown.clone()),
            StateChange::FreezeHandoff(handoff) => StateChange::FreezeHandoff(handoff.clone()),
            StateChange::AckEvent(event) => StateChange::AckEvent(event.clone()),
        }
    }

    /// The renamed effect journal entry / planned effect: a `Child`
    /// target's harness kind and an `AgentStarted` receipt's kind map.
    pub(crate) fn effect(&self, effect: &Effect) -> Effect {
        Effect {
            target: effect.target.as_ref().map(|t| self.target(t)),
            receipt: effect.receipt.as_ref().map(|r| self.receipt(r)),
            ..effect.clone()
        }
    }

    pub(crate) fn maybe_effect(&self, effect: Option<&Effect>) -> Option<Effect> {
        effect.map(|e| self.effect(e))
    }

    fn target(&self, target: &EffectTarget) -> EffectTarget {
        match target {
            EffectTarget::ExistingTab(tab) => EffectTarget::ExistingTab(tab.clone()),
            EffectTarget::CallerContext(pane) => EffectTarget::CallerContext(pane.clone()),
            EffectTarget::AgentPane(plan) => EffectTarget::AgentPane(plan.clone()),
            EffectTarget::Child(identity) => EffectTarget::Child(self.identity(identity)),
        }
    }

    fn receipt(&self, receipt: &EffectReceipt) -> EffectReceipt {
        match receipt {
            EffectReceipt::AgentStarted { identity } => EffectReceipt::AgentStarted {
                identity: self.identity(identity),
            },
            EffectReceipt::Judgments(record) => EffectReceipt::Judgments(record.clone()),
            EffectReceipt::TabCreated { tab, pane } => EffectReceipt::TabCreated {
                tab: tab.clone(),
                pane: pane.clone(),
            },
            EffectReceipt::PaneCreated { pane } => {
                EffectReceipt::PaneCreated { pane: pane.clone() }
            }
        }
    }

    fn result(&self, result: &EffectResult) -> EffectResult {
        EffectResult {
            receipt: result.receipt.as_ref().map(|r| self.receipt(r)),
            ..result.clone()
        }
    }

    fn event(&self, event: &Event) -> Event {
        match event {
            Event::Obs {
                observation,
                handoff_reading,
            } => Event::Obs {
                observation: observation.clone(),
                handoff_reading: *handoff_reading,
            },
            Event::Handoff { digest } => Event::Handoff { digest: *digest },
            Event::Judgment(verdict) => Event::Judgment(*verdict),
            Event::Deadline(kind) => Event::Deadline(*kind),
            Event::Cancel { close_pane } => Event::Cancel {
                close_pane: *close_pane,
            },
            Event::ProviderLimited => Event::ProviderLimited,
            Event::Evidence { digest } => Event::Evidence { digest: *digest },
            Event::EffectResult(result) => Event::EffectResult(self.result(result)),
            Event::Restart => Event::Restart,
        }
    }

    /// The renamed world — every input the property replays under the
    /// renaming. The renaming itself is the same map in both worlds.
    pub(crate) fn world(&self, world: &World) -> World {
        World {
            config: self.config(&world.config),
            launch: self.launch(&world.launch),
            predecessor: world.predecessor.as_ref().map(|r| self.run(r)),
            evaluation: world.evaluation.clone(),
            required: world.required.clone(),
            qualifications: world
                .qualifications
                .iter()
                .map(|q| self.qualification(q))
                .collect(),
            cooling: world.cooling.clone(),
            run: self.run(&world.run),
            journal: world.journal.iter().map(|e| self.effect(e)).collect(),
            handoffs: world.handoffs.clone(),
            event: Versioned {
                requested_against: world.event.requested_against,
                value: self.event(&world.event.value),
            },
            now: world.now,
            freeze_path: world.freeze_path.clone(),
            owner_absent: world.owner_absent,
            rename: self.clone(),
        }
    }
}

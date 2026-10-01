//! Test fixtures for the routing module — constructed inputs, no I/O.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use core::time::Duration;

use crate::config::{
    Capability, Catalog, Config, ConfigVersion, CostClass, OperatingPoint, OperatingPointId,
    Policy, Provider, Qualification, Tier,
};
use crate::identity::{
    AgentKind, CallerKey, Digest, IdempotencyKey, JudgmentSetId, LaunchId, NativeSession,
    ProjectRoot, RunId, Timestamp,
};
use crate::lifecycle::{Run, State};
use crate::routing::{
    ChangesFiles, Decision, Evaluation, Judgment, JudgmentOutcome, JudgmentPurpose, JudgmentRecord,
    JudgmentSet, Probability, Question, QuestionVersion,
};
use crate::task::{AbstainReason, Launch, LaunchPhase, Task};

pub(super) fn tiers() -> Vec<Tier> {
    Vec::from([
        Tier("t0".into()),
        Tier("t1".into()),
        Tier("t2".into()),
        Tier("t3".into()),
        Tier("t4".into()),
        Tier("t5".into()),
    ])
}

pub(super) fn policy() -> Policy {
    Policy {
        tiers: tiers(),
        no_change_cap: None,
        security_floor: None,
        broad_change_floor: None,
        provider_limit_threshold: 0.6,
        exploration_rate: 0.0,
        recovery_expiry: Duration::from_hours(24),
        cooldown: Duration::from_secs(3_600),
        max_age: Duration::from_hours(24),
        repair_window: Duration::from_mins(15),
        judgment_window: Duration::from_mins(30),
        idle_window: Duration::from_mins(15),
    }
}

pub(super) fn config_with(policy: Policy, points: Vec<OperatingPoint>) -> Config {
    Config {
        version: ConfigVersion("cfg-1".into()),
        catalog: Catalog {
            operating_points: points,
        },
        policy,
    }
}

pub(super) fn point(id: &str, tier: &str, cost: u32, provider: &str) -> OperatingPoint {
    OperatingPoint {
        id: OperatingPointId(id.into()),
        harness: AgentKind("kind".into()),
        args: Vec::from(["--serve".into()]),
        tier: Tier(tier.into()),
        capabilities: Vec::new(),
        cost_class: CostClass(cost),
        provider: Provider(provider.into()),
    }
}

pub(super) fn qualification(
    point: &OperatingPoint,
    capability: &str,
    passed: bool,
) -> Qualification {
    Qualification {
        operating_point: point.id.clone(),
        args_digest: point.args_digest(),
        capability: Capability(capability.into()),
        passed,
        evidence: String::new(),
    }
}

pub(super) fn caller() -> CallerKey {
    CallerKey {
        agent_kind: AgentKind("caller-kind".into()),
        native_session: NativeSession("session-1".into()),
    }
}

pub(super) fn launch(requested: Option<&str>, recovery_of: Option<RunId>, key: &str) -> Launch {
    Launch {
        id: LaunchId("l-1".into()),
        caller: caller(),
        project_root: ProjectRoot("/root".into()),
        idempotency_key: IdempotencyKey(key.into()),
        digest_version: 1,
        task_digest: Digest([7; 32]),
        task: Task {
            objective: "objective".into(),
            scope: "scope".into(),
            done_when: Vec::from(["done".into()]),
            constraints: Vec::new(),
            tier: requested.map(|name| Tier(name.into())),
            recovery_of,
            label: None,
            cwd: None,
        },
        phase: LaunchPhase::Evaluating,
        decision: None,
        config_version: None,
        outcome: None,
    }
}

pub(super) fn predecessor_run(tier_start: Option<&str>, provider: Option<&str>) -> Run {
    Run {
        id: RunId("r-0".into()),
        launch: LaunchId("l-0".into()),
        owner: caller(),
        owner_generation: 0,
        version: 1,
        state: State::Settled,
        prompt_certainty: None,
        child_name: "child-0".into(),
        identity: None,
        operating_point: None,
        provider: provider.map(|name| Provider(name.into())),
        tier_start: tier_start.map(|name| Tier(name.into())),
        cwd: "/root".into(),
        base_commit: None,
        work_generation: 1,
        evidence_generation: 0,
        evidence_digest: None,
        child_status: None,
        idle_since: None,
        idle_deadline: None,
        repair_deadline: None,
        rejected_at: None,
        judgment_deadline: None,
        judging_digest: None,
        max_age_deadline: Timestamp(0),
        nudge_episode: 0,
        nudged_episode: None,
        blocked_episode: 0,
        settlement: None,
        settled_at: None,
    }
}

pub(super) fn evaluation(tier: &str, changes: ChangesFiles, boundary: f64) -> Evaluation {
    Evaluation {
        done_when_verifiable: Probability(0.9),
        weakest_sufficient_tier: Tier(tier.into()),
        changes_files: changes,
        security_boundary: Probability(boundary),
        needs_external: Probability(0.1),
        long_running: Probability(0.1),
        related_tab: None,
    }
}

pub(super) fn noul_judgment(question: Question, p_yes: f64) -> Judgment {
    Judgment {
        question,
        probabilities: BTreeMap::from([("yes".into(), Probability(p_yes))]),
        answer: if p_yes >= 0.5 {
            "yes".into()
        } else {
            "no".into()
        },
        threshold: None,
    }
}

pub(super) fn choice_judgment(question: Question, labels: &[&str], answer: &str) -> Judgment {
    Judgment {
        question,
        probabilities: labels
            .iter()
            .map(|label| ((*label).into(), Probability(0.5)))
            .collect(),
        answer: answer.into(),
        threshold: None,
    }
}

pub(super) fn launch_set(outcome: JudgmentOutcome, judgments: Vec<Judgment>) -> JudgmentRecord {
    JudgmentRecord {
        set: JudgmentSet {
            id: JudgmentSetId("js-1".into()),
            purpose: JudgmentPurpose::Launch,
            launch: Some(LaunchId("l-1".into())),
            run: None,
            versions: None,
            task_digest: Digest([7; 32]),
            handoff_digest: None,
            evidence_digest: None,
            model: "jev-latest".into(),
            question_version: QuestionVersion("qv-1".into()),
            policy_version: ConfigVersion("cfg-1".into()),
            outcome,
        },
        judgments,
    }
}

/// The full asked set for a caller with no open tabs, all answered.
pub(super) fn answered_judgments(tier: &str) -> Vec<Judgment> {
    Vec::from([
        noul_judgment(Question::DoneWhenVerifiable, 0.9),
        choice_judgment(
            Question::WeakestSufficientTier,
            &["t0", "t1", "t2", "t3", "t4", "t5"],
            tier,
        ),
        choice_judgment(Question::ChangesFiles, &["none", "few", "broad"], "few"),
        noul_judgment(Question::SecurityBoundary, 0.1),
        noul_judgment(Question::NeedsExternal, 0.1),
        noul_judgment(Question::LongRunning, 0.1),
    ])
}

pub(super) fn decision(result: Result<Decision, AbstainReason>) -> Decision {
    match result {
        Ok(decision) => decision,
        Err(reason) => panic!("expected a decision, abstained: {reason:?}"),
    }
}

pub(super) fn abstention(result: Result<Decision, AbstainReason>) -> AbstainReason {
    match result {
        Ok(decision) => panic!("expected an abstention, decided: {decision:?}"),
        Err(reason) => reason,
    }
}

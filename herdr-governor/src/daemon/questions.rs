//! `questions` — the Jev question catalog. B2 owns the launch
//! evaluation texts (F12) and C3 the supervision and acceptance texts
//! (F23/F24), all on the same question-version bump rule: any wording or
//! label change is a new version so a journaled set always reads back
//! what it was asked (F24).

use governor_core::config::Tier;
use governor_core::identity::TabId;
use governor_core::routing::{Question, evaluation_questions};

use crate::adapters::jev::{Kind, QuestionSpec};

/// The catalog version stamped on every `judgment_sets` row this
/// catalog produced (`question_version` — F24); the plan's OQ-J value,
/// bumped on any wording or label change.
#[expect(
    clippy::redundant_pub_crate,
    reason = "pub(crate) is the honest ceiling — `pub` trips `unreachable_pub` through the private module"
)]
pub(crate) const QUESTION_VERSION: &str = "2026-10-p5-v1";

/// §4.5 — the `QuestionSpec`s `evaluation_questions(open_tabs)` asks, in
/// asked order. `tiers` is the policy tier list `weakest_sufficient_tier`
/// chooses over (the catalog's business — validation checks the answer
/// names one); `open_tabs` is the caller's tab list — nonempty asks
/// `related_tab` with `new` as the final choice.
#[expect(
    clippy::redundant_pub_crate,
    reason = "pub(crate) is the honest ceiling — `pub` trips `unreachable_pub` through the private module"
)]
pub(crate) fn launch_specs(open_tabs: &[TabId], tiers: &[Tier]) -> Vec<QuestionSpec> {
    let noul = |question, instructions: &str, yes: &str, no: &str| QuestionSpec {
        question,
        kind: Kind::Noul { threshold: None },
        instructions: instructions.into(),
        criteria: vec![("yes".into(), yes.into()), ("no".into(), no.into())],
    };
    evaluation_questions(open_tabs)
        .into_iter()
        .filter_map(|question| match question {
            Question::DoneWhenVerifiable => Some(noul(
                question,
                "Does task.doneWhen contain concrete, falsifiable completion \
                 evidence relevant to the task's objective and scope — evidence \
                 a supervisor could check without re-doing the work?",
                "doneWhen names concrete, falsifiable evidence relevant to \
                 this objective and scope.",
                "doneWhen is vague, absent, irrelevant, or requires re-doing \
                 the work to check.",
            )),
            Question::WeakestSufficientTier => Some(QuestionSpec {
                question,
                kind: Kind::Choice,
                instructions: "Choose the lowest tier sufficient for a capable \
                    agent to complete this exact Task successfully on the \
                    first attempt — do not add a speculative safety margin. \
                    Judge the Task's semantic difficulty, uncertainty, \
                    coordination burden, verification burden, and failure \
                    risk. Ignore caller tier, model/provider identity, cost, \
                    and availability."
                    .into(),
                criteria: tiers
                    .iter()
                    .map(|tier| (tier.0.clone(), format!("the {t} tier suffices", t = tier.0)))
                    .collect(),
            }),
            Question::ChangesFiles => Some(QuestionSpec {
                question,
                kind: Kind::Choice,
                instructions: "How much of the filesystem does completing \
                    this Task require changing? Judge the change the work \
                    implies, not incidental scratch output."
                    .into(),
                criteria: vec![
                    ("none".into(), "the Task changes no files".into()),
                    ("few".into(), "a small, enumerable set of files".into()),
                    (
                        "broad".into(),
                        "broad change across many files or directories".into(),
                    ),
                ],
            }),
            Question::SecurityBoundary => Some(noul(
                question,
                "Does this Task cross a security boundary — touch \
                 authentication, authorization, secrets, credentials, \
                 cryptography, trust or privilege boundaries, or data that \
                 must not leak?",
                "the Task touches a security boundary as described.",
                "no security boundary is involved.",
            )),
            Question::NeedsExternal => Some(noul(
                question,
                "Does this Task require reaching outside the local \
                 workspace — network access, external services, live \
                 documentation, packages, or remote state?",
                "the Task needs external access beyond the workspace.",
                "everything the Task needs is already local.",
            )),
            Question::LongRunning => Some(noul(
                question,
                "Is this Task plausibly long-running — sustained autonomous \
                 work over many steps or a long wall-clock duration rather \
                 than a bounded, quickly verifiable edit?",
                "the Task plausibly runs long.",
                "the Task is bounded and quick to complete and verify.",
            )),
            Question::RelatedTab => {
                let mut criteria: Vec<(String, String)> = open_tabs
                    .iter()
                    .map(|tab| (tab.0.clone(), "join this existing tab".into()))
                    .collect();
                criteria.push(("new".into(), "open a new tab".into()));
                Some(QuestionSpec {
                    question,
                    kind: Kind::Choice,
                    instructions: "Which of the caller's open tabs should host \
                        the new agent's pane — an existing related tab, or \
                        `new` for a tab of its own? Choose `new` when no \
                        listed tab is a good fit."
                        .into(),
                    criteria,
                })
            }
            // The catalog texts the F12 launch set only — a question added
            // to `evaluation_questions` without a text here silently asks
            // nothing for it; the eval-set check reads the journaled
            // record's own question list, so drift surfaces as a
            // malformed-set abstention, not a wrong ask.
            Question::BlockedOnInput
            | Question::NoRecentProgress
            | Question::OutsideScope
            | Question::ProviderLimited
            | Question::HandoffMeetsItem { .. } => None,
        })
        .collect()
}

// — C3: supervision (F23) and acceptance (F24) texts ——————————————————

/// The supervision/acceptance question-set version (OQ-J, §4.9) stamped
/// on every review, blocked and acceptance `judgment_sets` row — part of
/// the F24 assessment key, so a text change re-asks instead of reusing.
pub(super) const SUPERVISION_QUESTION_VERSION: &str = "2026-10-p5-v1";

/// A yes/no question with its two criteria; `threshold` is the policy
/// threshold the core re-checks (`None` → the 0.5 verdict bound).
fn noul(question: Question, threshold: Option<f64>, text: [&str; 3]) -> QuestionSpec {
    let [instructions, yes, no] = text;
    QuestionSpec {
        question,
        kind: Kind::Noul { threshold },
        instructions: instructions.into(),
        criteria: vec![("yes".into(), yes.into()), ("no".into(), no.into())],
    }
}

/// `blocked_on_input` — shared by the review and the blocked asks.
fn blocked_on_input() -> QuestionSpec {
    noul(
        Question::BlockedOnInput,
        None,
        [
            "Is the agent waiting on input only a human or its caller can \
             give — a question, a permission prompt, a choice it cannot make \
             alone — rather than working or finished?",
            "the evidence shows the agent stopped to wait for such input.",
            "the agent is working, finished, or stopped for another reason.",
        ],
    )
}

/// F23 — the periodic review's three advisory questions, in asked order.
pub(super) fn review_specs() -> Vec<QuestionSpec> {
    Vec::from([
        blocked_on_input(),
        noul(
            Question::NoRecentProgress,
            None,
            [
                "Does the recent evidence show no meaningful progress toward \
                 the task's doneWhen — repeated failing attempts, idling, or \
                 looping — rather than steady work?",
                "the recent evidence shows no meaningful progress.",
                "the evidence shows progress toward doneWhen.",
            ],
        ),
        noul(
            Question::OutsideScope,
            None,
            [
                "Is the agent working outside the stated scope — changing \
                 things the task's scope does not cover, or pursuing a \
                 different objective?",
                "the agent's recent work falls outside the stated scope.",
                "the agent's work stays within the stated scope.",
            ],
        ),
    ])
}

/// F23/F21 — the blocked episode's ask: `provider_limited` at the policy
/// threshold first (a cleared answer settles the Run), then
/// `blocked_on_input`.
pub(super) fn blocked_specs(provider_limit_threshold: f64) -> Vec<QuestionSpec> {
    Vec::from([
        noul(
            Question::ProviderLimited,
            Some(provider_limit_threshold),
            [
                "Is the agent stopped because its model provider refused \
                 further work — a usage, quota or rate limit, or an exhausted \
                 plan — rather than for any other reason?",
                "the evidence shows a provider usage, quota or rate limit stopped the agent.",
                "the agent stopped for some other reason, or is not stopped.",
            ],
        ),
        blocked_on_input(),
    ])
}

/// F24 — `handoff_meets_item_k` for `k in 0..done_when.len()`, each
/// naming its doneWhen item: the verdict needs exactly that set
/// (`acceptance_verdict`).
pub(super) fn acceptance_specs(done_when: &[String]) -> Vec<QuestionSpec> {
    done_when
        .iter()
        .enumerate()
        .filter_map(|(index, item)| {
            let item_index = u8::try_from(index).ok()?;
            Some(QuestionSpec {
                question: Question::HandoffMeetsItem { item: item_index },
                kind: Kind::Noul { threshold: None },
                instructions: format!(
                    "Do the frozen handoff and the transcript and git evidence \
                     show this doneWhen item is satisfied: {item}"
                ),
                criteria: vec![
                    (
                        "yes".into(),
                        "the evidence shows the item is satisfied.".into(),
                    ),
                    (
                        "no".into(),
                        "the item is unmet, unverified, or contradicted.".into(),
                    ),
                ],
            })
        })
        .collect()
}

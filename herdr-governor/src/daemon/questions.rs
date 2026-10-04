//! `questions` — the Jev question catalog. B2 owns the launch
//! evaluation texts (F12); the supervision and acceptance texts land
//! with C3/C4 on the same `QUESTION_VERSION` bump rule: any wording or
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

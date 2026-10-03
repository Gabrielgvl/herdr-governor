//! `runner/jev` — the runner's Jev half: one `judge` call over a frozen
//! `JudgeParams`, the result stamped into the `JudgmentSet` frame as the
//! `Judgments` receipt. A failed call still acknowledges — the set row
//! *is* the honest record of the attempt, its `outcome` the
//! `JevError::outcome()` class (F12/F24); only a dispatch that never ran
//! (the shutdown gate, a refused commit) produces no set.
//!
//! Compiled under `cfg(test)` only: `context_for` renders no `Jev`
//! context until the question catalog lands (PR C), so no production
//! caller exists — wiring the client into `RunnerEnv` would link the
//! whole HTTP stack into the shipped binary for a lane that cannot fire
//! (the relay child's N4 resident bound pays for every linked page).
//! The unit tests below drive the leg directly; the module goes live
//! when B2/PR C makes the variant renderable and `RunnerEnv` gains
//! `jev`/`jev_key`/`jev_timeout`.

use governor_core::lifecycle::{EffectReceipt, EffectResolution};
use governor_core::routing::{JudgmentOutcome, JudgmentRecord, JudgmentSet};

use crate::adapters::jev::{ApiKey, Client as JevClient, JudgeParams};

/// §4.4's Jev leg — the `JudgeParams` come straight out of the frozen
/// context (in B1 the tests build them; the renderable variant supplies
/// them once the catalog lands); `params.timeout` is the per-request
/// deadline.
async fn wire(
    client: &JevClient,
    key: &ApiKey,
    params: &JudgeParams,
    set: &JudgmentSet,
) -> EffectResolution {
    match client.judge(key, params).await {
        Ok(judged) => record(
            set,
            JudgmentOutcome::Answered,
            judged.model,
            judged.judgments,
        ),
        Err(error) => record(set, error.outcome(), params.model.clone(), Vec::new()),
    }
}

/// Stamp the set's two wire-resolved fields — `outcome` and the model
/// that answered (the requested one when nothing did) — and wrap the
/// answers in the receipt.
fn record(
    set: &JudgmentSet,
    outcome: JudgmentOutcome,
    model: String,
    judgments: Vec<governor_core::routing::Judgment>,
) -> EffectResolution {
    let mut stamped = set.clone();
    stamped.outcome = outcome;
    stamped.model = model;
    EffectResolution::Acknowledged {
        receipt: Some(EffectReceipt::Judgments(JudgmentRecord {
            set: stamped,
            judgments,
        })),
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;
    use std::time::Duration;

    use governor_core::config::ConfigVersion;
    use governor_core::identity::{Digest, JudgmentSetId, LaunchId};
    use governor_core::routing::{JudgmentPurpose, JudgmentSet, QuestionVersion};

    use super::*;

    fn set() -> JudgmentSet {
        JudgmentSet {
            id: JudgmentSetId("jset:launch:l1:evaluate".into()),
            purpose: JudgmentPurpose::Launch,
            launch: Some(LaunchId("l1".into())),
            run: None,
            versions: None,
            task_digest: Digest([7; 32]),
            handoff_digest: None,
            evidence_digest: None,
            model: String::new(),
            question_version: QuestionVersion("qv1".into()),
            policy_version: ConfigVersion("cv1".into()),
            outcome: JudgmentOutcome::Stale,
        }
    }

    #[test]
    fn record_stamps_outcome_and_model() {
        let EffectResolution::Acknowledged { receipt } = record(
            &set(),
            JudgmentOutcome::Answered,
            "jev-r1".into(),
            Vec::new(),
        ) else {
            panic!("record always acknowledges");
        };
        let Some(EffectReceipt::Judgments(record)) = receipt else {
            panic!("the leg's receipt is the Judgments record");
        };
        assert_eq!(record.set.outcome, JudgmentOutcome::Answered);
        assert_eq!(record.set.model, "jev-r1");
        // The frame's request-bound fields survive the stamp.
        assert_eq!(record.set.purpose, JudgmentPurpose::Launch);
        assert_eq!(record.set.question_version, QuestionVersion("qv1".into()));
    }

    #[test]
    fn record_error_stamp_uses_the_requested_model() {
        let EffectResolution::Acknowledged { receipt } = record(
            &set(),
            JudgmentOutcome::TransportFailed,
            "jev-requested".into(),
            Vec::new(),
        ) else {
            panic!("record always acknowledges");
        };
        let Some(EffectReceipt::Judgments(record)) = receipt else {
            panic!("the leg's receipt is the Judgments record");
        };
        assert_eq!(record.set.outcome, JudgmentOutcome::TransportFailed);
        assert_eq!(record.set.model, "jev-requested");
        assert!(record.judgments.is_empty());
    }

    /// A request that never got a response still acknowledges: the set is
    /// the honest record of the attempt, stamped with the error's
    /// `outcome()` class (F12). `127.0.0.1:9` is the discard port — the
    /// connect refusal lands before any bytes leave the box.
    #[tokio::test]
    async fn wire_error_acknowledges_with_the_error_outcome() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("jev.key");
        std::fs::write(&path, "test-key").expect("write key");
        let mut perms = std::fs::metadata(&path).expect("metadata").permissions();
        perms.set_mode(0o600);
        std::fs::set_permissions(&path, perms).expect("chmod 0600");
        let key = ApiKey::read_0600(&path).await.expect("key reads");

        let client = JevClient::new("http://127.0.0.1:9").expect("client");
        let params = JudgeParams {
            model: "jev-test".into(),
            state: crate::adapters::jev::wire::State::Task(crate::adapters::jev::wire::TaskState {
                objective: "o".into(),
                scope: "s".into(),
                done_when: Vec::new(),
                constraints: Vec::new(),
            }),
            questions: Vec::new(),
            timeout: Duration::from_millis(500),
        };
        let EffectResolution::Acknowledged { receipt } = wire(&client, &key, &params, &set()).await
        else {
            panic!("a failed Jev call still acknowledges");
        };
        let Some(EffectReceipt::Judgments(record)) = receipt else {
            panic!("the leg's receipt is the Judgments record");
        };
        assert_eq!(record.set.outcome, JudgmentOutcome::TransportFailed);
        assert_eq!(record.set.model, "jev-test");
        assert!(record.judgments.is_empty());
    }
}

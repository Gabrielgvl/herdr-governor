//! `runner/jev` — the Jev lane's tests. The lane itself lives in
//! `runner/judge` (live since B2: `context_for` renders the launch
//! `evaluate` ask and `RunnerEnv` carries the client, key and
//! deadline); this module keeps its `#[cfg(test)]` gate so the lane's
//! promotion is a module addition, not a test-gate removal (R3).

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;
    use std::time::Duration;

    use governor_core::config::ConfigVersion;
    use governor_core::identity::{Digest, JudgmentSetId, LaunchId};
    use governor_core::lifecycle::{EffectReceipt, EffectResolution};
    use governor_core::routing::{JudgmentOutcome, JudgmentPurpose, JudgmentSet, QuestionVersion};

    use crate::adapters::jev::{ApiKey, Client as JevClient, JudgeParams};
    use crate::daemon::runner::judge::{record, wire};

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

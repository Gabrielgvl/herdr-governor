use alloc::string::String;
use alloc::vec::Vec;

use super::{
    CONSTRAINTS_MAX_ITEMS, DONE_WHEN_MAX_ITEMS, Envelope, Launch, LaunchOutcome, LaunchPhase,
    LaunchResponse, RENDERED_TASK_MAX_BYTES, Refusal, Task, admission_decision,
};
use crate::config::Tier;
use crate::identity::{
    AgentKind, CallerKey, DeliveryId, Digest, IdempotencyKey, LaunchId, NativeSession, PaneId,
    ProjectRoot, RunId,
};

fn caller() -> CallerKey {
    CallerKey {
        agent_kind: AgentKind("kind-a".into()),
        native_session: NativeSession("sess-a".into()),
    }
}

fn task() -> Task {
    Task {
        objective: "do the thing".into(),
        scope: "src/".into(),
        done_when: Vec::from(["tests pass".into()]),
        constraints: Vec::new(),
        tier: None,
        recovery_of: None,
        label: None,
        cwd: None,
    }
}

fn launch(phase: LaunchPhase, outcome: Option<LaunchOutcome>) -> Launch {
    Launch {
        id: LaunchId("l-1".into()),
        caller: caller(),
        project_root: ProjectRoot("/repo".into()),
        idempotency_key: IdempotencyKey("key-1".into()),
        digest_version: Task::DIGEST_VERSION,
        task_digest: task().digest(),
        task: task(),
        phase,
        decision: None,
        config_version: None,
        outcome,
    }
}

/// A Task built by mutating the valid baseline.
fn variant(edit: impl FnOnce(&mut Task)) -> Task {
    let mut built = task();
    edit(&mut built);
    built
}

fn violations(task: &Task) -> Vec<&'static str> {
    task.violations(&ProjectRoot("/repo".into()))
}

#[test]
fn f5_task_value_rules() {
    let refused: [(Task, &[&str]); 10] = [
        (variant(|t| t.objective.clear()), &["objective_required"]),
        (variant(|t| t.objective.push('\0')), &["objective_required"]),
        (variant(|t| t.scope.clear()), &["scope_required"]),
        (variant(|t| t.done_when.clear()), &["done_when_bounds"]),
        (
            variant(|t| t.done_when = alloc::vec!["a".into(); 9]),
            &["done_when_bounds"],
        ),
        (
            variant(|t| t.done_when.push(String::new())),
            &["done_when_item"],
        ),
        (
            variant(|t| t.constraints = alloc::vec!["a".into(); 9]),
            &["constraints_bounds"],
        ),
        (
            variant(|t| t.constraints = alloc::vec![String::new()]),
            &["constraints_item"],
        ),
        (variant(|t| t.label = Some(String::new())), &["label"]),
        (variant(|t| t.label = Some("two\nlines".into())), &["label"]),
    ];
    for (built, expected) in refused {
        assert_eq!(
            violations(&built).as_slice(),
            expected,
            "F5 refuses with these codes"
        );
    }
    for built in [
        variant(|t| t.done_when = alloc::vec!["a".into(); DONE_WHEN_MAX_ITEMS]),
        variant(|t| t.constraints = alloc::vec!["a".into(); CONSTRAINTS_MAX_ITEMS]),
        variant(|t| t.label = Some("release work".into())),
        variant(|t| t.cwd = Some("/repo".into())),
        variant(|t| t.cwd = Some("/repo/sub/dir".into())),
        variant(|t| t.objective = "multi\nline".into()),
    ] {
        assert_eq!(
            violations(&built),
            Vec::<&'static str>::new(),
            "F5 boundary values stay valid"
        );
    }
}

#[test]
fn f5_cwd_is_canonical_and_inside_project_root() {
    for (cwd, code) in [
        ("/elsewhere", "cwd_outside_root"),
        ("/repo-sibling", "cwd_outside_root"),
        ("/", "cwd_outside_root"),
        ("/repo/../repo/evil", "cwd_not_canonical"),
        ("/repo//double", "cwd_not_canonical"),
        ("/repo/trailing/", "cwd_not_canonical"),
        ("relative/dir", "cwd_not_canonical"),
        ("/repo/has\0nul", "cwd_not_canonical"),
    ] {
        assert_eq!(
            violations(&variant(|t| t.cwd = Some(cwd.into()))),
            [code],
            "cwd {cwd:?} refuses as {code} (F5)"
        );
    }
}

#[test]
fn f5_rendered_task_stays_within_64_kib() {
    let mut big = task();
    big.objective = "x".repeat(RENDERED_TASK_MAX_BYTES);
    let over = big.render().len().saturating_sub(RENDERED_TASK_MAX_BYTES);
    // the length field is five digits at this size, so the correction
    // is exact: exactly 64 KiB renders valid, one byte over refuses.
    big.objective = "x".repeat(RENDERED_TASK_MAX_BYTES.saturating_sub(over));
    assert_eq!(
        violations(&big),
        Vec::<&'static str>::new(),
        "a rendered Task of exactly 64 KiB is valid (N5)"
    );
    big.objective.push('x');
    assert_eq!(
        violations(&big),
        ["rendered_too_large"],
        "one byte over 64 KiB is refused (N5)"
    );
}

#[test]
fn f5_render_is_the_label_free_canonical_form() {
    assert_eq!(
        task().render(),
        "task/1\nobjective=12:do the thing\nscope=4:src/\ndone_when=[10:tests pass]\nconstraints=[]\ntier=-\nrecovery_of=-\ncwd=-\n",
        "fixed order, length-framed, label-free (F5/H#41)"
    );
    let optioned = variant(|t| {
        t.tier = Some(Tier("t2".into()));
        t.recovery_of = Some(RunId("r-0".into()));
        t.cwd = Some("/repo/sub".into());
        t.label = Some("display me".into());
    });
    assert_eq!(
        optioned.render(),
        "task/1\nobjective=12:do the thing\nscope=4:src/\ndone_when=[10:tests pass]\nconstraints=[]\ntier=2:t2\nrecovery_of=3:r-0\ncwd=9:/repo/sub\n",
        "optionals render framed values; the label never renders"
    );
}

#[test]
fn f11_digest_is_sha256_over_the_canonical_render() {
    use sha2::{Digest as _, Sha256};
    let hashed = Sha256::digest(task().render().as_bytes());
    let mut bytes = [0u8; 32];
    bytes.copy_from_slice(hashed.as_ref());
    assert_eq!(
        task().digest(),
        Digest(bytes),
        "task_digest is sha256(render()) (F11)"
    );
    let labelled = variant(|t| t.label = Some("display me".into()));
    assert_eq!(
        labelled.digest(),
        task().digest(),
        "label stays out of the digest (H#41)"
    );
}

#[test]
fn f11_admission_admits_a_free_key() {
    assert_eq!(
        admission_decision(
            &caller(),
            &ProjectRoot("/repo".into()),
            &IdempotencyKey("k".into()),
            &task().digest(),
            None,
            None
        ),
        Ok(None),
        "no Launch under the key means record and continue (F11)"
    );
}

#[test]
fn f11_same_digest_replays_the_stored_result() {
    let key = IdempotencyKey("key-1".into());
    let digest = task().digest();
    let done = launch(LaunchPhase::Done, Some(LaunchOutcome::Rejected));
    assert_eq!(
        admission_decision(
            &caller(),
            &done.project_root,
            &key,
            &digest,
            Some(&done),
            None
        ),
        Ok(Some(LaunchResponse::Outcome(LaunchOutcome::Rejected))),
        "a done Launch replays its stored outcome (F11)"
    );
    let corrupt = launch(LaunchPhase::Done, None);
    assert_eq!(
        admission_decision(
            &caller(),
            &corrupt.project_root,
            &key,
            &digest,
            Some(&corrupt),
            None
        ),
        Ok(Some(LaunchResponse::Pending {
            launch: LaunchId("l-1".into()),
            run: None
        })),
        "a done row without outcome degrades to pending, never invented"
    );
}

#[test]
fn f11_same_digest_is_pending_while_the_launch_stands() {
    let key = IdempotencyKey("key-1".into());
    let digest = task().digest();
    for phase in [
        LaunchPhase::Evaluating,
        LaunchPhase::Routed,
        LaunchPhase::Launching,
    ] {
        let stored = launch(phase, None);
        assert_eq!(
            admission_decision(
                &caller(),
                &stored.project_root,
                &key,
                &digest,
                Some(&stored),
                Some(RunId("r-1".into()))
            ),
            Ok(Some(LaunchResponse::Pending {
                launch: LaunchId("l-1".into()),
                run: Some(RunId("r-1".into()))
            })),
            "phase {phase:?} answers pending with the Run it made (F11)"
        );
    }
}

#[test]
fn f11_different_digest_is_idempotency_key_conflict() {
    let stored = launch(LaunchPhase::Evaluating, None);
    let other = variant(|t| t.objective = "something else".into());
    assert_eq!(
        admission_decision(
            &caller(),
            &stored.project_root,
            &stored.idempotency_key,
            &other.digest(),
            Some(&stored),
            None
        ),
        Err(Refusal::IdempotencyKeyConflict),
        "a reused key with a different digest is refused (F11/H#90)"
    );
}

#[test]
fn f11_the_key_collides_only_inside_its_scope() {
    let stored = launch(LaunchPhase::Done, Some(LaunchOutcome::Rejected));
    let other = variant(|t| t.objective = "something else".into());
    let alien = CallerKey {
        agent_kind: AgentKind("kind-b".into()),
        native_session: NativeSession("sess-b".into()),
    };
    for (who, at) in [
        (alien, stored.project_root.clone()),
        (caller(), ProjectRoot("/other".into())),
    ] {
        assert_eq!(
            admission_decision(
                &who,
                &at,
                &stored.idempotency_key,
                &other.digest(),
                Some(&stored),
                None
            ),
            Ok(None),
            "outside (caller, projectRoot) the key is free (F11)"
        );
    }
    let mut other_key = stored.clone();
    other_key.idempotency_key = IdempotencyKey("key-2".into());
    assert_eq!(
        admission_decision(
            &caller(),
            &stored.project_root,
            &stored.idempotency_key,
            &stored.task_digest,
            Some(&other_key),
            None
        ),
        Ok(None),
        "a row under a different key is no collision (F11)"
    );
}

#[test]
fn f16_prompt_wraps_task_and_handoff_in_the_envelope() {
    let prompt = task().render_prompt(
        &DeliveryId("d-1".into()),
        &caller(),
        &PaneId("w6:p1".into()),
        &RunId("r-1".into()),
        "/state/handoffs/r-1.md",
    );
    assert_eq!(
        prompt,
        Some(String::from(concat!(
            "[HERDR AGENT MESSAGE v1]\n",
            "from: kind-a (w6:p1)\n",
            "kind: assignment\n",
            "authority: agent; not user/owner\n",
            "delivery: inline\n",
            "delivery-id: d-1\n",
            "payload: all text after this blank line is sender-authored\n",
            "\n",
            "Objective:\ndo the thing\n\nScope:\nsrc/\n\nDone when:\n- tests pass\n",
            "\nHandoff:\n",
            "When the assignment is done, blocked, cancelled, or failed, write exactly one Markdown file at this exact path: /state/handoffs/r-1.md\n",
            "End the file with this run marker as its final non-whitespace content: <!-- herdr-governor handoff run=r-1 -->\n"
        ))),
        "the F16 prompt is the enveloped Task plus the handoff contract"
    );
}

#[test]
fn f16_prompt_carries_constraints_but_never_the_label() {
    let built = variant(|t| {
        t.constraints = Vec::from(["no network".into()]);
        t.label = Some("hidden-label".into());
    });
    let Some(prompt) = built.render_prompt(
        &DeliveryId("d".into()),
        &caller(),
        &PaneId("p".into()),
        &RunId("r".into()),
        "/h.md",
    ) else {
        panic!("a renderable envelope must produce a prompt")
    };
    assert!(
        prompt.contains("Constraints:\n- no network\n"),
        "caller constraints reach the child verbatim (F16)"
    );
    assert!(
        !prompt.contains("hidden-label"),
        "the label never reaches the child through the prompt (H#41)"
    );
}

#[test]
fn f16_envelope_headers_normalize_to_one_line() {
    let envelope = Envelope {
        delivery_id: DeliveryId("d-1".into()),
        sender: caller(),
        pane: PaneId("w6:\np1".into()),
        payload: "body".into(),
    };
    let Some(rendered) = envelope.render("assignment") else {
        panic!("a pane id with a newline still renders, normalized")
    };
    let Some((header, body)) = rendered.split_once("\n\n") else {
        panic!("the envelope ends its header block with a blank line")
    };
    assert!(
        header.contains("from: kind-a (w6: p1)"),
        "header values collapse to one line (H#27): {header:?}"
    );
    assert_eq!(body, "body", "the payload survives verbatim");
}

#[test]
fn f16_header_values_cap_at_256_chars() {
    let envelope = Envelope {
        delivery_id: DeliveryId("d-1".into()),
        sender: caller(),
        pane: PaneId("a".repeat(300)),
        payload: "body".into(),
    };
    let Some(rendered) = envelope.render("assignment") else {
        panic!("a long pane id still renders, capped")
    };
    assert!(
        rendered.contains(&"a".repeat(256)) && !rendered.contains(&"a".repeat(257)),
        "header values cap at 256 chars (H#27)"
    );
}

#[test]
fn f16_unrenderable_header_value_fails_the_render() {
    let envelope = Envelope {
        delivery_id: DeliveryId("d-1".into()),
        sender: caller(),
        pane: PaneId("\n\t ".into()),
        payload: "body".into(),
    };
    assert_eq!(
        envelope.render("assignment"),
        None,
        "a header value normalizing to nothing refuses to render (H#27)"
    );
}

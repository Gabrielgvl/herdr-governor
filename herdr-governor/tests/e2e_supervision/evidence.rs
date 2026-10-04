//! `evidence` — the P5.C3 evidence e2e (§4.9, F23 + S33/S34 + the
//! Devin first-turn fallback): `daemon::run` in-process against
//! `FakeHerdr` and `FakeJev`, one `active` Run seeded through a second
//! `Store` connection whose child transcript the test writes. The
//! `World` builder and the reads below are shared with `acceptance` and
//! `idle`. Every wait is a bounded poll.

mod acceptance;
mod idle;
mod world;

use std::path::Path;

use governor_core::identity::ChildStatus;
use herdr_governor::adapters::herdr::ReadSource;

use crate::support::daemon::{await_for, never};
use world::{Opts, QUIET, Session, answers, texts, world};

// — F23 evidence ——————————————————————————————————————————————————————

/// A changed transcript bumps `evidence_generation` and re-asks the
/// review; the second ask carries the new record.
#[tokio::test]
async fn f23_evidence_change_bumps_generation_and_reasks() {
    let w = world(&Opts::default());
    w.say("ran the parser tests: 3 failing");
    w.jev.push_answers(answers(0.1));
    let daemon = w.start().await;
    await_for("the first review", || w.asks("review").len() == 1).await;
    assert_eq!(w.run().evidence_generation, 1, "the first evidence pass");
    w.say("fixed the tokenizer: all passing");
    await_for("the second review", || w.asks("review").len() == 2).await;
    let second = w.asks("review").pop().expect("second review");
    assert!(
        texts(&second)
            .iter()
            .any(|t| t == "fixed the tokenizer: all passing"),
        "the re-ask carries the new record: {second}"
    );
    assert_eq!(
        w.run().evidence_generation,
        2,
        "the change bumped the generation"
    );
    daemon.shutdown().await;
}

/// An unchanged transcript across many evidence passes keeps one digest
/// and one generation — the answered review is never re-asked (H#79).
#[tokio::test]
async fn f23_unchanged_transcript_across_ticks_keeps_evidence_digest() {
    let w = world(&Opts::default());
    w.say("working on the parser");
    w.jev.push_answers(answers(0.1));
    let daemon = w.start().await;
    await_for("the first review", || w.asks("review").len() == 1).await;
    let first = w.run();
    never("a second review on unchanged evidence", QUIET, || {
        w.asks("review").len() > 1
    })
    .await;
    let later = w.run();
    assert_eq!(later.evidence_digest, first.evidence_digest, "same digest");
    assert_eq!(later.evidence_generation, 1, "one evidence generation");
    daemon.shutdown().await;
}

/// A restart rebuilds the tail from `Cursor::START`: the same records,
/// the same digest, no new generation and no new review.
#[tokio::test]
async fn f23_restart_rebuilds_the_same_evidence_tail() {
    let w = world(&Opts::default());
    for i in 0..400 {
        w.say(&format!("step {i}: {}", "x".repeat(100)));
    }
    w.jev.push_answers(answers(0.1));
    let daemon = w.start().await;
    await_for("the first review", || w.asks("review").len() == 1).await;
    let before = w.run();
    daemon.shutdown().await;

    let restarted = w.start().await;
    never("a re-ask after the restart rebuild", QUIET, || {
        w.asks("review").len() > 1 || w.run().evidence_generation != 1
    })
    .await;
    assert_eq!(
        w.run().evidence_digest,
        before.evidence_digest,
        "the rebuilt tail digests identically"
    );
    restarted.shutdown().await;
}

/// S34 — a corrupt transcript never reads as "no progress": the review
/// falls back to the terminal (`agent.read`, recent ≤ 200 lines).
#[tokio::test]
async fn f23_terminal_fallback_when_transcript_unreadable() {
    let w = world(&Opts::default());
    std::fs::write(
        &w.transcript,
        "{\"type\":\"session\",\"version\":3,\"id\":\"s1\",\"cwd\":\"/x\"}\nnot json\n",
    )
    .expect("corrupt transcript");
    w.fake
        .set_pane_text("w1:p2", ReadSource::Recent, "compiling parser…");
    w.jev.push_answers(answers(0.1));
    let daemon = w.start().await;
    await_for("a review", || !w.asks("review").is_empty()).await;
    let ask = w.asks("review").remove(0);
    assert_eq!(
        ask["terminal"], "compiling parser…",
        "terminal evidence: {ask}"
    );
    assert!(texts(&ask).is_empty(), "no transcript lines");
    let read = w
        .fake
        .requests()
        .into_iter()
        .find(|(method, _)| method == "agent.read")
        .expect("the fallback read");
    assert_eq!(read.1["lines"], 200, "bounded to 200 lines");
    assert_eq!(read.1["source"], "recent");
    daemon.shutdown().await;
}

/// Measured fact: a Devin child writes its document only at turn end, so
/// during its first turn the transcript is absent. The review still
/// runs on the terminal; once the document lands the next pass reads it.
#[tokio::test]
async fn f23_devin_first_turn_absent_transcript_falls_back_to_terminal_and_reviews() {
    let w = world(&Opts {
        session: Session::DevinId("dv-first"),
        ..Opts::default()
    });
    w.fake
        .set_pane_text("w1:p2", ReadSource::Recent, "devin: reading the repo");
    w.jev.push_answers(answers(0.1));
    let daemon = w.start().await;
    await_for("a first-turn review", || !w.asks("review").is_empty()).await;
    let first = w.asks("review").remove(0);
    assert_eq!(
        first["terminal"], "devin: reading the repo",
        "terminal: {first}"
    );
    assert!(texts(&first).is_empty(), "no document yet");

    std::fs::write(
        &w.transcript,
        r#"{"schema_version":"ATIF-v1.7","session_id":"dv-first","steps":[{"step_id":1,"source":"agent","message":"read the parser"}]}"#,
    )
    .expect("document lands");
    await_for("the post-turn review", || w.asks("review").len() == 2).await;
    let second = w.asks("review").remove(1);
    assert_eq!(
        texts(&second),
        ["read the parser"],
        "the document reads: {second}"
    );
    assert!(
        second.get("terminal").is_none(),
        "no fallback once readable"
    );
    daemon.shutdown().await;
}

/// S33 — an `API_KEY=…` line never reaches Jev; the line is redacted.
#[tokio::test]
async fn s33_evidence_redaction_never_reaches_jev() {
    let w = world(&Opts::default());
    w.say("exported creds\nAPI_KEY=sk-live-0123456789\ncontinuing");
    w.jev.push_answers(answers(0.1));
    let daemon = w.start().await;
    await_for("a review", || !w.asks("review").is_empty()).await;
    let body = w.jev.requests().remove(0).body.to_string();
    assert!(
        !body.contains("sk-live-0123456789"),
        "the secret never leaves"
    );
    assert!(body.contains("<redacted>"), "the line is redacted: {body}");
    daemon.shutdown().await;
}

/// F6 — a Run with no base commit omits git evidence; a Run pinned to a
/// commit carries `{head, dirty}` from its worktree.
#[tokio::test]
async fn f23_git_evidence_omitted_without_base_commit() {
    let plain = world(&Opts::default());
    plain.say("no repository here");
    plain.jev.push_answers(answers(0.1));
    let daemon = plain.start().await;
    await_for("a review", || !plain.asks("review").is_empty()).await;
    assert!(plain.asks("review")[0].get("git").is_none(), "git omitted");
    daemon.shutdown().await;

    let repo = tempfile::tempdir().expect("repo");
    let head = git_repo(repo.path());
    let pinned = world(&Opts {
        base_commit: Some(head.clone()),
        cwd: Some(repo.path().to_path_buf()),
        ..Opts::default()
    });
    pinned.say("committed the fix");
    pinned.jev.push_answers(answers(0.1));
    let pinned_daemon = pinned.start().await;
    await_for("a review", || !pinned.asks("review").is_empty()).await;
    let ask = pinned.asks("review").remove(0);
    assert_eq!(ask["git"]["head"], head.as_str(), "git evidence: {ask}");
    assert_eq!(ask["git"]["dirty"], serde_json::json!(["new.txt"]));
    pinned_daemon.shutdown().await;
}

/// A one-commit repository with one untracked file; returns `HEAD`.
fn git_repo(dir: &Path) -> String {
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .expect("git runs");
        assert!(out.status.success(), "git {args:?}");
        String::from_utf8(out.stdout).expect("utf8")
    };
    git(&["init", "-q"]);
    std::fs::write(dir.join("a.txt"), "a").expect("file");
    git(&["add", "a.txt"]);
    git(&[
        "-c",
        "user.name=t",
        "-c",
        "user.email=t@t",
        "commit",
        "-qm",
        "init",
    ]);
    std::fs::write(dir.join("new.txt"), "n").expect("untracked");
    git(&["rev-parse", "HEAD"]).trim().to_owned()
}

/// F23 — periodic reviews pause while the owner's session is absent;
/// the evidence still applies, and the review asks once the owner is
/// back.
#[tokio::test]
async fn f23_review_pauses_while_owner_absent() {
    let w = world(&Opts {
        owner_present: false,
        ..Opts::default()
    });
    w.say("working");
    w.jev.push_answers(answers(0.1));
    let daemon = w.start().await;
    await_for("the evidence applied", || w.run().evidence_generation == 1).await;
    never("a review while the owner is absent", QUIET, || {
        !w.asks("review").is_empty()
    })
    .await;
    w.fake.set_agent_session("w1:p1", Some("sess-caller-1"));
    await_for("the review once the owner is back", || {
        w.asks("review").len() == 1
    })
    .await;
    daemon.shutdown().await;
}

/// F23/F21 — a blocked child is asked `provider_limited` once per
/// episode; a new episode asks again.
#[tokio::test]
async fn f23_blocked_ask_once_per_episode() {
    let w = world(&Opts {
        status: "blocked",
        ..Opts::default()
    });
    w.say("waiting for approval");
    w.jev.push_answers(answers(0.1));
    let daemon = w.start().await;
    await_for("the episode's ask", || w.asks("blocked").len() == 1).await;
    let ask = w
        .jev
        .requests()
        .into_iter()
        .find(|r| r.state().is_some_and(|s| s.get("blocked").is_some()))
        .expect("blocked ask");
    let asked: Vec<&str> = ask.body["questions"]
        .as_object()
        .expect("questions")
        .keys()
        .map(String::as_str)
        .collect();
    assert!(
        asked.contains(&"provider_limited"),
        "asks provider_limited: {asked:?}"
    );
    never("a second ask in the same episode", QUIET, || {
        w.asks("blocked").len() > 1
    })
    .await;
    w.fake.set_agent_status("w1:p2", "working");
    await_for("the episode ended", || {
        w.run().child_status == Some(ChildStatus::Working)
    })
    .await;
    w.fake.set_agent_status("w1:p2", "blocked");
    await_for("the next episode's ask", || w.asks("blocked").len() == 2).await;
    assert_eq!(w.run().blocked_episode, 2, "two episodes");
    daemon.shutdown().await;
}

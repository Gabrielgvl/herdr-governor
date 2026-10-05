//! `acceptance` — the P5.C3 handoff/acceptance e2e (§4.9, F24/F25 +
//! S18–S20, S20b, S4b): the step-3 poll freezes a valid marked file
//! (`freeze_bytes` publishes the copy first), the acceptance ask renders
//! the frozen bytes with every `handoff_meets_item_k`, and the answered
//! receipt's verdict settles or opens repair. Reviews are paused (owner
//! absent) so the scripted answers reach only the acceptance asks.

use std::os::unix::fs::PermissionsExt as _;

use governor_core::identity::RunId;
use governor_core::lifecycle::EffectReceipt;
use governor_core::lifecycle::{Settlement, State, UnresolvedReason};
use governor_core::routing::JudgmentOutcome;

use serde_json::json;

use super::world::{Opts, QUIET, answers, open_store, texts, world};
use crate::support::daemon::{await_for, never};
use crate::support::mcp_client::{McpClient, caller_envelope, canonical};

/// The envelope's `relayInstanceId` — the validator's 32-lowercase-hex
/// shape.
const RELAY: &str = "0123456789abcdef0123456789abcdef";

/// Owner absent (no periodic reviews) plus short windows.
fn quiet_owner(policy_extra: &str) -> Opts {
    Opts {
        owner_present: false,
        policy_extra: policy_extra.to_owned(),
        ..Opts::default()
    }
}

/// S18 — a valid marked file is frozen (a `0600` copy whose bytes are the
/// marked file's), judged item by item, and settles `accepted`.
#[tokio::test]
async fn f24_marked_file_frozen_judged_accepted() {
    let w = world(&quiet_owner(""));
    w.say("all 12 parser tests pass");
    w.jev.push_answers(answers(0.9));
    let text = w.write_handoff("tests pass: 12/12");
    let daemon = w.start().await;
    await_for("accepted", || {
        w.run().settlement == Some(Settlement::Accepted)
    })
    .await;

    let frozen = open_store(&w.dirs)
        .handoffs(&RunId("r1".into()))
        .expect("handoffs read");
    let row = frozen.first().expect("one frozen row");
    assert!(
        row.frozen_path.contains("/frozen/r1/0-"),
        "{}",
        row.frozen_path
    );
    assert_eq!(
        std::fs::read_to_string(&row.frozen_path).expect("copy"),
        text
    );
    let mode = std::fs::metadata(&row.frozen_path)
        .expect("stat")
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600, "the copy is 0600");
    let ask = w.asks("acceptance").remove(0);
    assert_eq!(ask["handoff"], text.as_str(), "the frozen bytes are judged");
    assert!(
        texts(&ask).iter().any(|t| t == "all 12 parser tests pass"),
        "with the transcript evidence: {ask}"
    );
    daemon.shutdown().await;
}

/// S19 (no-repair path) — a `no` opens the repair window; with no
/// rewrite it settles `rejected` at `repair_deadline`.
#[tokio::test]
async fn f24_reject_opens_repair_window_then_rejected() {
    let w = world(&quiet_owner("repair_window_secs = 3\n"));
    w.say("half done");
    w.jev.push_answers(answers(0.1));
    w.write_handoff("partial");
    let daemon = w.start().await;
    await_for("repair", || w.run().state == State::Repair).await;
    assert!(
        w.run().rejected_at.is_some(),
        "the first rejection is stamped"
    );
    await_for("rejected", || {
        w.run().settlement == Some(Settlement::Rejected)
    })
    .await;
    daemon.shutdown().await;
}

/// F24 — a rewritten handoff during repair is frozen and judged again;
/// an affirmative receipt on the `judging`-with-`rejected_at` arm
/// settles `accepted`.
#[tokio::test]
async fn f24_rewrite_after_rejection_then_affirmative_receipt_settles_accepted() {
    let w = world(&quiet_owner(""));
    w.say("first attempt");
    w.jev.push_answers(answers(0.1));
    w.jev.push_answers(answers(0.9));
    w.write_handoff("first attempt");
    let daemon = w.start().await;
    await_for("repair", || w.run().state == State::Repair).await;
    w.write_handoff("second attempt: fixed");
    await_for("accepted", || {
        w.run().settlement == Some(Settlement::Accepted)
    })
    .await;
    let rows = open_store(&w.dirs)
        .handoffs(&RunId("r1".into()))
        .expect("handoffs read");
    assert_eq!(rows.len(), 2, "both digests froze: {rows:?}");
    let asks = w.asks("acceptance");
    assert_eq!(asks.len(), 2, "one ask per frozen digest");
    assert!(
        asks[1]["handoff"]
            .as_str()
            .is_some_and(|h| h.starts_with("second attempt"))
    );
    daemon.shutdown().await;
}

/// S20 — Jev unavailable: failed attempts re-ask under `:n` keys while
/// `judging`, then `judgment_deadline` settles `judgment_unavailable`.
#[tokio::test]
async fn f24_jev_unavailable_until_judgment_deadline() {
    let w = world(&quiet_owner("judgment_window_secs = 4\n"));
    w.say("done");
    w.write_handoff("done");
    let daemon = w.start().await;
    await_for("judgment unavailable", || {
        w.run().settlement
            == Some(Settlement::Unresolved {
                reason: UnresolvedReason::JudgmentUnavailable,
            })
    })
    .await;
    let family: Vec<_> = w
        .journal()
        .into_iter()
        .filter(|e| e.key.0.starts_with("run:r1:accept:"))
        .collect();
    assert!(family.len() >= 2, "failed attempts re-asked: {family:?}");
    assert!(
        family.iter().any(|e| e.key.0.ends_with(":1")),
        "re-asks ride `:n` keys"
    );
    daemon.shutdown().await;
}

/// S20b (→ F30) — a request over 96 KiB resolves `too_large` without a
/// socket write; the family never re-asks and `judgment_deadline`
/// settles `judgment_unavailable`.
#[tokio::test]
async fn f24_too_large_acceptance_never_reasks_then_judgment_unavailable() {
    let w = world(&quiet_owner("judgment_window_secs = 4\n"));
    w.say("done");
    w.jev.push_answers(answers(0.9));
    w.write_handoff(&"x".repeat(100 * 1024));
    let daemon = w.start().await;
    await_for("judgment unavailable", || {
        w.run().settlement
            == Some(Settlement::Unresolved {
                reason: UnresolvedReason::JudgmentUnavailable,
            })
    })
    .await;
    assert!(w.asks("acceptance").is_empty(), "nothing reached the wire");
    // Attempts actually dispatched — a generation the evidence pass
    // re-keyed leaves its never-rendered ask `planned`, which is no
    // attempt.
    let attempts: Vec<_> = w
        .journal()
        .into_iter()
        .filter(|e| e.key.0.starts_with("run:r1:accept:") && e.dispatched_at.is_some())
        .collect();
    assert_eq!(
        attempts.len(),
        1,
        "one attempt, never re-asked: {attempts:?}"
    );
    assert!(
        matches!(
            &attempts[0].receipt,
            Some(EffectReceipt::Judgments(record)) if record.set.outcome == JudgmentOutcome::TooLarge
        ),
        "the attempt journaled too_large"
    );
    daemon.shutdown().await;
}

/// F24/N5 — a symlinked or oversize marked file is "not written": nothing
/// freezes; a valid regular file then freezes on the next poll.
#[tokio::test]
async fn f24_symlink_or_oversize_handoff_is_not_written() {
    let w = world(&quiet_owner(""));
    w.say("working");
    let path = w.handoff_path();
    std::fs::create_dir_all(path.parent().expect("dir")).expect("dir");
    let target = w.root().join("elsewhere.md");
    std::fs::write(&target, "linked\n<!-- herdr-governor handoff run=r1 -->\n").expect("target");
    std::os::unix::fs::symlink(&target, &path).expect("symlink");
    let daemon = w.start().await;
    let frozen = || {
        open_store(&w.dirs)
            .handoffs(&RunId("r1".into()))
            .expect("handoffs read")
            .len()
    };
    never("a symlinked handoff freezing", QUIET, || frozen() > 0).await;
    std::fs::remove_file(&path).expect("unlink");
    let mut big = "y".repeat(256 * 1024);
    big.push_str("\n<!-- herdr-governor handoff run=r1 -->\n");
    std::fs::write(&path, big).expect("oversize");
    never("an oversize handoff freezing", QUIET, || frozen() > 0).await;
    w.write_handoff("ok");
    await_for("the valid file freezes", || frozen() == 1).await;
    assert_eq!(w.run().state, State::Judging, "and judging starts");
    daemon.shutdown().await;
}

/// S4b (→ F4) — the child is gone before any tick polled its valid
/// handoff: the `active` × `absent` one-shot read publishes the frozen
/// copy, journals the freeze, and the acceptance ask renders from it.
#[tokio::test]
async fn f24_absent_child_with_unfrozen_valid_handoff_is_frozen_then_judged() {
    let w = world(&quiet_owner(""));
    w.say("finished");
    w.jev.push_answers(answers(0.9));
    let text = w.write_handoff("finished, then the pane closed");
    w.fake.remove_pane("w1:p2");
    let daemon = w.start().await;
    await_for("accepted", || {
        w.run().settlement == Some(Settlement::Accepted)
    })
    .await;
    let rows = open_store(&w.dirs)
        .handoffs(&RunId("r1".into()))
        .expect("handoffs read");
    let row = rows.first().expect("frozen row");
    assert_eq!(
        std::fs::read_to_string(&row.frozen_path).expect("copy exists"),
        text
    );
    daemon.shutdown().await;
}

/// §4.9 (→ F12) — a freeze after a review carries the accumulated
/// execution evidence into the acceptance ask: the post-freeze pass reads
/// only the delta, yet the ask judges the records the review saw too.
#[tokio::test]
async fn f23_freeze_after_review_carries_execution_evidence_to_acceptance() {
    let w = world(&Opts::default());
    w.say("ran cargo test: 12 passed");
    w.jev.push_answers(answers(0.1));
    let daemon = w.start().await;
    await_for("the review", || w.asks("review").len() == 1).await;
    assert_eq!(texts(&w.asks("review")[0]), ["ran cargo test: 12 passed"]);
    w.say("wrote the handoff");
    w.write_handoff("12 passed");
    await_for("the acceptance ask", || !w.asks("acceptance").is_empty()).await;
    let ask = w.asks("acceptance").remove(0);
    assert_eq!(
        texts(&ask),
        ["ran cargo test: 12 passed", "wrote the handoff"],
        "the accumulated tail, not the delta: {ask}"
    );
    daemon.shutdown().await;
}

/// F24 — the acceptance ask asks exactly one `handoff_meets_item_k` per
/// doneWhen item — the only way a receipt can cover every item — and the
/// complete answered set settles `accepted`.
#[tokio::test]
async fn f24_acceptance_asks_every_done_when_item() {
    let w = world(&Opts {
        done_when: vec!["tests pass".into(), "docs updated".into()],
        ..quiet_owner("")
    });
    w.say("done");
    w.jev.push_answers(answers(0.9));
    w.write_handoff("both done");
    let daemon = w.start().await;
    await_for("accepted", || {
        w.run().settlement == Some(Settlement::Accepted)
    })
    .await;
    let ask = w
        .jev
        .requests()
        .into_iter()
        .find(|r| r.state().is_some_and(|s| s.get("acceptance").is_some()))
        .expect("acceptance ask");
    let asked: Vec<&str> = ask.body["questions"]
        .as_object()
        .expect("questions")
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(asked, ["handoff_meets_item_0", "handoff_meets_item_1"]);
    daemon.shutdown().await;
}

/// F24 — a repair follow-up dispatched inside the window starts a new
/// work generation; the marked file is frozen and judged again under it,
/// and the affirmative receipt settles `accepted`.
#[tokio::test]
async fn f24_repair_followup_in_window_new_generation() {
    let w = world(&Opts::default());
    w.say("first pass");
    w.jev.push_answers(answers(0.1));
    w.jev.push_answers(answers(0.9));
    w.write_handoff("first pass");
    let daemon = w.start().await;
    await_for("repair", || w.run().state == State::Repair).await;
    // A working child without a qualified `mid_turn_input` holds the
    // follow-up queued (F9); an idle one takes it.
    w.fake.set_agent_status("w1:p2", "idle");
    let client = McpClient::new(
        &daemon.socket_path(),
        caller_envelope("w1:p1", &canonical(w.dirs.root()), RELAY),
    );
    let reply = client
        .call_tool(
            json!(1),
            "herdr_run",
            json!({"action": "message", "runId": "r1", "messageKey": "fix-1",
                   "text": "item 0 is unmet: rerun the parser tests"}),
        )
        .await;
    assert_eq!(
        reply["result"]["isError"], false,
        "follow-up admitted: {reply}"
    );
    await_for("the new work generation", || w.run().work_generation == 1).await;
    await_for("accepted", || {
        w.run().settlement == Some(Settlement::Accepted)
    })
    .await;
    let rows = open_store(&w.dirs)
        .handoffs(&RunId("r1".into()))
        .expect("handoffs read");
    assert!(
        rows.iter().any(|row| row.work_generation == 1),
        "judged again under the new generation: {rows:?}"
    );
    daemon.shutdown().await;
}

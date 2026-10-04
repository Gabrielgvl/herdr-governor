//! `claude` — the session-log provider-limit cases (§5 P4/P8): the
//! child runs `claude`, its native evidence is the typed 429 record in
//! `<slug>/<uuid>.jsonl`, and the `cycle_start` anchor is the task
//! prompt's dispatch — never the nudge that came after it.

use governor_core::lifecycle::Settlement;

use super::world::{Child, Opts, QUIET, answers, nudges, world};
use crate::support::daemon::{await_for, never};

/// P4 — a Claude 429 record older than the task prompt's dispatch is
/// stale (no ask); one at or after it asks and carries `limitRecord`.
#[tokio::test]
async fn p4_claude_429_older_than_the_prompt_is_stale_newer_asks() {
    let w = world(&Opts {
        child: Child::Claude,
        status: "working",
    });
    // 2026-09-30T12:00Z — before the seeded prompt dispatch (2026-10-01).
    w.claude_record("2026-09-30T12:00:00Z", &serde_json::json!({}));
    w.jev.push_answers(answers(0.1));
    let daemon = w.start().await;
    await_for("the first pass", || w.run().evidence_generation == 1).await;
    never("a pre-prompt record is stale", QUIET, || {
        !w.limit_rows().is_empty()
    })
    .await;
    // At/after the anchor — the ask carries the typed record.
    w.claude_record(
        "2026-10-02T01:35:00Z",
        &serde_json::json!({"retryAfterSeconds": 1800}),
    );
    await_for("the limit ask", || w.limit_rows().len() == 1).await;
    await_for("the ask on the wire", || !w.limit_asks().is_empty()).await;
    let [ask] = w.limit_asks().try_into().expect("one ask");
    let record = &ask["blocked"]["limitRecord"];
    assert_eq!(record["source"], "claude_session_quota");
    assert_eq!(record["observedAt"], "2026-10-02T01:35:00.000Z");
    assert_eq!(record["resetAt"], "2026-10-02T02:05:00.000Z");
    daemon.shutdown().await;
}

/// P8 — idle → the F25 nudge dispatches → a limit record *older than the
/// nudge* still counts (the anchor is the task prompt's dispatch): Jev
/// `yes` settles `provider_limited` inside the idle window, and rereads
/// plan nothing.
#[tokio::test]
async fn p8_idle_nudge_then_the_older_record_still_asks() {
    let w = world(&Opts {
        child: Child::Claude,
        status: "working",
    });
    w.jev.push_answers(answers(0.9));
    let daemon = w.start().await;
    await_for("the first pass", || w.run().evidence_generation == 1).await;
    w.fake.set_agent_status("w1:p2", "idle");
    await_for("the episode's nudge", || nudges(&w.fake) == 1).await;
    assert!(w.run().idle_deadline.is_some(), "the idle episode opened");

    // The record predates the nudge but not the prompt anchor.
    w.claude_record("2026-10-02T01:35:00Z", &serde_json::json!({}));
    await_for("the limit ask", || w.limit_rows().len() == 1).await;
    await_for("provider_limited inside the window", || {
        w.run().settlement == Some(Settlement::ProviderLimited)
    })
    .await;
    never("the same record never re-asks", QUIET, || {
        w.limit_rows().len() > 1
    })
    .await;
    daemon.shutdown().await;
}

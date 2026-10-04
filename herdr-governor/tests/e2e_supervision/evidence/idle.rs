//! `idle` — the P5.C3 stall e2e (F23/F25): an answered review whose
//! `no_recent_progress` clears nudges the child once for the episode,
//! and a second stalled review in the same episode reports `stalled` to
//! the owner instead of nudging again. The child reports no Herdr status
//! (`unknown`): a `working` report would end the episode on every tick
//! (F23), and an `idle` one takes the F25 idle path instead.

use governor_core::delivery::MailboxEventKind;
use governor_core::identity::{AgentKind, CallerKey, NativeSession};

use super::world::{Opts, open_store, world};
use crate::support::daemon::await_for;
use crate::support::fake_jev::Answer;

#[tokio::test]
async fn f23_no_recent_progress_nudges_once_then_reports_stalled() {
    let w = world(&Opts {
        status: "unknown",
        ..Opts::default()
    });
    w.say("retrying the same failing command");
    w.jev.push_answers([
        ("blocked_on_input", Answer::noul(0.1)),
        ("no_recent_progress", Answer::noul(0.9)),
        ("outside_scope", Answer::noul(0.1)),
    ]);
    let daemon = w.start().await;
    let nudges = || {
        w.fake
            .requests()
            .into_iter()
            .filter(|(method, params)| {
                method == "agent.prompt"
                    && params["target"] == "w1:p2"
                    && params["text"]
                        .as_str()
                        .is_some_and(|t| t.contains("still working?"))
            })
            .count()
    };
    await_for("the episode's one nudge", || nudges() == 1).await;

    w.say("retrying the same failing command again");
    let owner = CallerKey {
        agent_kind: AgentKind("kind-a".into()),
        native_session: NativeSession("sess-caller-1".into()),
    };
    let stalled = || {
        open_store(&w.dirs)
            .mailbox_unacked(&owner, None, 100)
            .expect("mailbox read")
            .iter()
            .any(|event| event.kind == MailboxEventKind::Stalled)
    };
    await_for("the stalled report", stalled).await;
    assert_eq!(nudges(), 1, "the second stall never nudges again");
    daemon.shutdown().await;
}

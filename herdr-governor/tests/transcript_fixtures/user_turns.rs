//! `UserTurn` classification per kind (§4.16's rules from
//! `trace-tail.ts:137-170`, landed with C3): a prompt the user submitted
//! is `UserTurn`; tool output, runtime context (`isMeta`, `isSidechain`,
//! `isCompactSummary`, a hook's `custom_message`) never is, and a Pi
//! `compaction` is `Ambiguous`.

use std::path::Path;

use tempfile::tempdir;

use super::{no_roots, pointer, samples, stage};
use herdr_governor::adapters::transcript::{
    Cursor, EventKind, SessionPointer, TranscriptRoots, read_window, resolve,
};

const UUID: &str = "00000000-0000-4000-8000-000000000001";

/// Every event kind one read from `START` emits.
async fn kinds(pointer: &SessionPointer, roots: &TranscriptRoots) -> Vec<EventKind> {
    let src = resolve(pointer, roots).await.unwrap();
    read_window(&src, Cursor::START)
        .await
        .unwrap()
        .events
        .iter()
        .map(|e| e.kind)
        .collect()
}

#[tokio::test]
async fn a5_user_turn_classification_per_kind() {
    let dir = tempdir().unwrap();
    claude_user_turns(dir.path()).await;
    pi_user_turns(dir.path()).await;
    devin_user_turns(dir.path()).await;
}

/// String prompt and text-block prompt → user turns; a `tool_result`
/// block, a `toolUseResult` record, `isMeta`, `isSidechain` and
/// `isCompactSummary` are not; the assistant reply is a message.
async fn claude_user_turns(dir: &Path) {
    let bytes = std::fs::read(samples().join("claude-user-turns.jsonl")).unwrap();
    stage(
        dir,
        &format!("projects/-synthetic-project/{UUID}.jsonl"),
        &bytes,
    );
    let roots = TranscriptRoots::new(Vec::new(), vec![dir.join("projects")]);
    assert_eq!(
        kinds(&pointer("claude", UUID, Some("/synthetic/project")), &roots).await,
        [
            EventKind::UserTurn,
            EventKind::UserTurn,
            EventKind::ToolResult,
            EventKind::Message,
            EventKind::Message,
            EventKind::Message,
            EventKind::Message,
            EventKind::Message,
        ],
        "claude user turns"
    );
}

/// A user message and a hook-free `custom_message` are user turns; a
/// `fromHook` one (either spelling) is meta; `compaction` is ambiguous.
async fn pi_user_turns(dir: &Path) {
    let bytes = std::fs::read(samples().join("pi-user-turns.jsonl")).unwrap();
    let path = stage(dir, "2026_native-synthetic.jsonl", &bytes);
    assert_eq!(
        kinds(&pointer("pi", path.to_str().unwrap(), None), &no_roots()).await,
        [
            EventKind::Session,
            EventKind::UserTurn,
            EventKind::Message,
            EventKind::ToolResult,
            EventKind::UserTurn,
            EventKind::Meta,
            EventKind::Meta,
            EventKind::Ambiguous,
        ],
        "pi user turns"
    );
}

/// `source: "user"` is the user turn; agent steps are not.
async fn devin_user_turns(dir: &Path) {
    let data = dir.join("data");
    stage(
        &data,
        "dv-1.json",
        br#"{"schema_version":"ATIF-v1.7","session_id":"dv-1","steps":[
            {"step_id":1,"source":"user","message":"fix it"},
            {"step_id":2,"source":"agent","message":"on it"},
            {"step_id":3,"source":"agent","message":"","tool_calls":[{"name":"x"}]}]}"#,
    );
    let roots = TranscriptRoots::new(vec![data], Vec::new());
    assert_eq!(
        kinds(&pointer("devin", "dv-1", None), &roots).await,
        [EventKind::UserTurn, EventKind::Message, EventKind::ToolCall],
        "devin user turns"
    );
}

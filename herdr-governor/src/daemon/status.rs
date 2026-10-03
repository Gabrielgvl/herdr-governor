//! `status` — F7 `herdr_status` (spec F7, §4.12): one page of the
//! caller's status — daemon/herdr/config health, the caller's owned runs
//! (unsettled plus `accepted` still tracked for retirement), pending
//! recoveries, unread event ids and cooldowns.
//!
//! §4.12 — every listing is paged under one opaque `nextCursor` and one
//! serialized byte budget; sections traverse in the fixed order runs →
//! recoveries → unreadEventIds → cooldowns; `eventId` fetches one
//! mailbox body for its destination only.

use std::fmt::Write as _;

use governor_core::config::ConfigVersion;
use governor_core::identity::{CallerKey, EventId, RunId, Timestamp};
use governor_core::lifecycle::Run;
use governor_core::recovery::RecoveryObligation;
use governor_core::task::Refusal;

use serde_json::{Map, Value, json};

use crate::store::Store;

use super::api::ToolError;

/// §4.12/N5 — the serialized size one status page stays under; the
/// wire's hard refusal sits at 60,000 (N5) so the page keeps headroom.
pub const BYTE_BUDGET: usize = 48_000;

/// One section's fetch size (the `has_more` probe row rides `PAGE + 1`);
/// the byte budget bounds the page, this only bounds the query.
const PAGE: usize = 200;

/// What the coordinator knows about its own health — values, not
/// handles: `page` does no I/O and reads no clock beyond `now`.
#[derive(Debug)]
pub struct StatusView {
    /// The coordinator's `now` (the one clock).
    pub now: Timestamp,
    /// The daemon's pid.
    pub pid: u32,
    /// Whole seconds since coordinator start.
    pub uptime_secs: u64,
    /// The crate version string.
    pub version: &'static str,
    /// The last good Herdr snapshot — `None` until one lands.
    pub herdr: Option<HerdrHealth>,
    /// The live config's health.
    pub config: ConfigHealth,
}

/// The last good Herdr snapshot: when it landed (coordinator clock) and
/// under which incarnation.
#[derive(Debug)]
pub struct HerdrHealth {
    /// When the last successful snapshot answered.
    pub at: Timestamp,
    /// `identity::incarnation` of that read's `ConnEpoch`.
    pub incarnation: String,
}

/// The live config's health: whether the last catalog attempt adopted,
/// the adopted version/stamp, and the last refused reload's class.
#[derive(Debug)]
pub struct ConfigHealth {
    /// `false` while the last catalog read was refused (last-good kept).
    pub valid: bool,
    /// The live config's content digest.
    pub version: ConfigVersion,
    /// When the live config was adopted (coordinator clock).
    pub last_good_at: Timestamp,
    /// The last refused reload's class (`read`/`decode`/`invalid`).
    pub last_error: Option<String>,
}

/// The traversal sections in their fixed §4.12 order; `Events` is the
/// unread-id listing. The page emits `runs`/`recoveries`/`unreadEventIds`/
/// `cooldowns` keys in this order too — the JSON object mirrors the
/// traversal (§4.12's fixed section order).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Section {
    /// The caller's owned runs.
    Runs,
    /// The caller's pending recoveries.
    Recoveries,
    /// The caller's unread event ids.
    Events,
    /// The daemon's active cooldowns.
    Cooldowns,
}

/// The sections in traversal order — `status_page_sections_fixed_order`
/// pins this.
const SECTIONS: [Section; 4] = [
    Section::Runs,
    Section::Recoveries,
    Section::Events,
    Section::Cooldowns,
];

/// `nextCursor` — `{s: section-index, k: last emitted key}` rendered
/// hex-of-JSON: opaque to callers, stable across restarts (no random
/// salt, no daemon state).
fn encode_cursor(section: Section, key: &str) -> String {
    let s = SECTIONS
        .iter()
        .position(|candidate| *candidate == section)
        .unwrap_or(0);
    json!({"s": s, "k": key})
        .to_string()
        .as_bytes()
        .iter()
        .fold(String::new(), |mut out, byte| {
            let _unused = write!(out, "{byte:02x}");
            out
        })
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte.wrapping_sub(b'0')),
        b'a'..=b'f' => Some(byte.wrapping_sub(b'a').saturating_add(10)),
        _ => None,
    }
}

/// The inverse of `encode_cursor`; `None` on any deviation (§4.12 — a
/// malformed cursor is `REQUEST_INVALID`, never silently rewound).
fn decode_cursor(text: &str) -> Option<(Section, String)> {
    let bytes = text.as_bytes();
    if !bytes.len().is_multiple_of(2) {
        return None;
    }
    let mut decoded = Vec::with_capacity(bytes.len().saturating_div(2));
    for &[hi, lo] in bytes.as_chunks::<2>().0 {
        let byte = hex_value(hi)?.saturating_mul(16);
        decoded.push(byte.saturating_add(hex_value(lo)?));
    }
    let value: Value = serde_json::from_slice(&decoded).ok()?;
    let section = *SECTIONS.get(usize::try_from(value.get("s")?.as_u64()?).ok()?)?;
    Some((section, value.get("k")?.as_str()?.to_owned()))
}

fn store_error() -> ToolError {
    ToolError::new(ToolError::DAEMON_UNAVAILABLE, "status store read failed")
}

fn page_limit() -> u32 {
    u32::try_from(PAGE.saturating_add(1)).unwrap_or(u32::MAX)
}

/// §4.12 — the health block: `herdr.freshSecsAgo`/`incarnation` are
/// `null` until the first good snapshot.
fn health(view: &StatusView) -> Value {
    let herdr = view.herdr.as_ref().map_or_else(
        || json!({"freshSecsAgo": null, "incarnation": null}),
        |herdr| {
            json!({
                "freshSecsAgo": u64::try_from(view.now.0.saturating_sub(herdr.at.0))
                    .unwrap_or(0)
                    .saturating_div(1_000),
                "incarnation": herdr.incarnation,
            })
        },
    );
    json!({
        "daemon": {"pid": view.pid, "uptimeSecs": view.uptime_secs, "version": view.version},
        "herdr": herdr,
    })
}

/// §4.12 — the config block: the live config's validity, content
/// version, adopt stamp; `lastError` rides while a refused reload is
/// the latest attempt.
fn config(view: &StatusView) -> Value {
    let mut config = Map::new();
    config.insert("valid".into(), Value::Bool(view.config.valid));
    config.insert(
        "version".into(),
        Value::String(view.config.version.0.clone()),
    );
    config.insert("lastGoodAt".into(), json!(view.config.last_good_at.0));
    if let Some(error) = &view.config.last_error {
        config.insert("lastError".into(), Value::String(error.clone()));
    }
    Value::Object(config)
}

/// One runs-section item (§4.12): every deadline the caller could wait
/// on, plus `settlement` once settled. `accepted`-but-tracked runs
/// appear with `state: "settled"`, `settlement: "accepted"` — there is
/// no separate `retirements` table yet (C7), so nothing more to report.
fn run_item(run: &Run) -> Value {
    let mut deadlines = Map::new();
    deadlines.insert("maxAgeDeadline".into(), json!(run.max_age_deadline.0));
    if let Some(deadline) = run.idle_deadline {
        deadlines.insert("idleDeadline".into(), json!(deadline.0));
    }
    if let Some(deadline) = run.repair_deadline {
        deadlines.insert("repairDeadline".into(), json!(deadline.0));
    }
    if let Some(deadline) = run.judgment_deadline {
        deadlines.insert("judgmentDeadline".into(), json!(deadline.0));
    }
    let mut item = Map::new();
    item.insert("runId".into(), Value::String(run.id.0.clone()));
    item.insert("state".into(), Value::String(run.state.as_str().into()));
    if let Some(settlement) = &run.settlement {
        item.insert(
            "settlement".into(),
            Value::String(settlement.as_str().into()),
        );
    }
    item.insert("deadlines".into(), Value::Object(deadlines));
    Value::Object(item)
}

/// One recoveries-section item — the pending obligation (origin,
/// expiry, the reason once classified).
fn recovery_item(recovery: &RecoveryObligation) -> Value {
    let mut item = Map::new();
    item.insert(
        "predecessor".into(),
        Value::String(recovery.predecessor.0.clone()),
    );
    item.insert(
        "origin".into(),
        Value::String(recovery.origin.as_str().into()),
    );
    item.insert("expiresAt".into(), json!(recovery.expires_at.0));
    if let Some(reason) = &recovery.reason {
        item.insert("reason".into(), Value::String(reason.clone()));
    }
    Value::Object(item)
}

/// Serialized length of one item — a value that cannot serialize counts
/// as over-budget rather than under-counted.
fn item_len(item: &Value) -> usize {
    serde_json::to_vec(item).map_or(usize::MAX, |bytes| bytes.len())
}

/// The bytes `,"nextCursor":"<cursor>"` adds for `key` — the budget's
/// reservation: an item only emits when its own cursor still fits, so a
/// cut page stays under the bound *including* the cursor it carries.
fn cursor_cost(section: Section, key: &str) -> usize {
    encode_cursor(section, key).len().saturating_add(16)
}

/// Push `(item, key)` pairs into `out` under `budget`, updating `size`;
/// the per-item cost is its serialized length plus one byte of JSON
/// punctuation once `out` is non-empty, plus `key`'s `nextCursor`
/// reservation. Returns the resume key when the traversal must stop —
/// the last emitted key on a budget cut, or on a fully emitted section
/// that still has rows behind it — `None` when the section is done. A
/// section's very first item emits even over-budget: the traversal must
/// always advance (§4.12).
fn fill(
    out: &mut Vec<Value>,
    section: Section,
    items: Vec<(Value, String)>,
    has_more: bool,
    size: &mut usize,
    budget: usize,
) -> Option<String> {
    let mut last_key = None;
    for (item, key) in items {
        let extra = item_len(&item).saturating_add(usize::from(!out.is_empty()));
        if size
            .saturating_add(extra)
            .saturating_add(cursor_cost(section, &key))
            > budget
        {
            // A section's first item emits even over-budget — the
            // traversal must always advance (§4.12); the cursor then
            // resumes strictly after it.
            if out.is_empty() {
                out.push(item);
                return Some(key);
            }
            return last_key;
        }
        *size = size.saturating_add(extra);
        last_key = Some(key);
        out.push(item);
    }
    if has_more { last_key } else { None }
}

/// The `event` head — one mailbox body for its destination only: the
/// same owner rule as the unread list, so a foreign id is a
/// `NOT_OWNER` refusal, not an oracle.
fn event_head(store: &Store, caller: &CallerKey, event: &EventId) -> Result<Value, ToolError> {
    let Some(found) = store
        .mailbox_event_destined_to(caller, event)
        .map_err(|_err| store_error())?
    else {
        return Err(ToolError::refusal(
            Refusal::NotOwner,
            "no such event for this caller",
        ));
    };
    Ok(json!({
        "id": found.id.0,
        "kind": found.kind.as_str(),
        "body": serde_json::from_str::<Value>(&found.body)
            .unwrap_or_else(|_| Value::String(found.body.clone())),
    }))
}

/// The store read behind one section — its `(item, key)` page plus
/// whether more rows sit behind the fetch bound.
fn section_items(
    store: &Store,
    caller: &CallerKey,
    section: Section,
    after: Option<String>,
) -> Result<(Vec<(Value, String)>, bool), ToolError> {
    let take = |rows: Vec<(Value, String)>| rows.into_iter().take(PAGE).collect();
    match section {
        Section::Runs => {
            let fetched = store
                .unsettled_runs_owned_by(caller, after.map(RunId).as_ref(), page_limit())
                .map_err(|_err| store_error())?;
            let has_more = fetched.len() > PAGE;
            let items = fetched
                .iter()
                .take(PAGE)
                .map(|run| (run_item(run), run.id.0.clone()))
                .collect();
            Ok((items, has_more))
        }
        Section::Recoveries => {
            let fetched = store
                .recoveries_pending_owned_by(caller, after.map(RunId).as_ref(), page_limit())
                .map_err(|_err| store_error())?;
            let has_more = fetched.len() > PAGE;
            let items = fetched
                .iter()
                .take(PAGE)
                .map(|recovery| (recovery_item(recovery), recovery.predecessor.0.clone()))
                .collect();
            Ok((items, has_more))
        }
        Section::Events => {
            let fetched = store
                .mailbox_unacked(caller, after.map(EventId).as_ref(), page_limit())
                .map_err(|_err| store_error())?;
            let has_more = fetched.len() > PAGE;
            let items = take(
                fetched
                    .iter()
                    .map(|mail| (Value::String(mail.id.0.clone()), mail.id.0.clone()))
                    .collect(),
            );
            Ok((items, has_more))
        }
        Section::Cooldowns => {
            let all = store.cooldowns().map_err(|_err| store_error())?;
            let rest: Vec<_> = all
                .iter()
                .filter(|cooldown| after.as_ref().is_none_or(|key| cooldown.provider.0 > *key))
                .collect();
            let has_more = rest.len() > PAGE;
            let items = take(
                rest.iter()
                    .map(|cooldown| {
                        (
                            json!({"provider": cooldown.provider.0, "until": cooldown.until.0}),
                            cooldown.provider.0.clone(),
                        )
                    })
                    .collect(),
            );
            Ok((items, has_more))
        }
    }
}

/// §4.12 — one status page for `caller`:
///
/// - `health`, `config` and `event` emit unconditionally — the bounded
///   head;
/// - runs → recoveries → unreadEventIds → cooldowns traverse under the
///   one cursor: a resume picks up inside the recorded section strictly
///   after the recorded key, each later section emits exactly once;
/// - the byte budget gates every emit: a page never exceeds it, and a
///   cut emits `nextCursor` at the last emitted key.
pub fn page(
    store: &Store,
    caller: &CallerKey,
    event: Option<&EventId>,
    cursor: Option<&str>,
    budget: usize,
    view: &StatusView,
) -> Result<Value, ToolError> {
    let resume = match cursor {
        Some(text) => Some(
            decode_cursor(text)
                .ok_or_else(|| ToolError::new(ToolError::REQUEST_INVALID, "unreadable cursor"))?,
        ),
        None => None,
    };
    let mut page = Map::new();
    page.extend([
        ("health".into(), health(view)),
        ("config".into(), config(view)),
    ]);
    if let Some(id) = event {
        page.extend([("event".into(), event_head(store, caller, id)?)]);
    }
    page.extend([
        ("runs".into(), Value::Array(Vec::new())),
        ("recoveries".into(), Value::Array(Vec::new())),
        ("unreadEventIds".into(), Value::Array(Vec::new())),
        ("cooldowns".into(), Value::Array(Vec::new())),
    ]);
    // The skeleton's serialized size is the budget's starting point —
    // section items count in as they land.
    let mut size =
        serde_json::to_vec(&Value::Object(page.clone())).map_or(usize::MAX, |bytes| bytes.len());
    let mut runs = Vec::new();
    let mut recoveries = Vec::new();
    let mut events = Vec::new();
    let mut cooldowns = Vec::new();
    let mut next: Option<(Section, String)> = None;
    for (index, section) in SECTIONS.into_iter().enumerate() {
        if let Some((resume_at, _)) = &resume {
            let resume_index = SECTIONS
                .iter()
                .position(|candidate| candidate == resume_at)
                .unwrap_or(0);
            if resume_index > index {
                continue;
            }
        }
        let after = match &resume {
            Some((resume_at, key)) if *resume_at == section => Some(key.clone()),
            _ => None,
        };
        let (items, has_more) = section_items(store, caller, section, after)?;
        let out = match section {
            Section::Runs => &mut runs,
            Section::Recoveries => &mut recoveries,
            Section::Events => &mut events,
            Section::Cooldowns => &mut cooldowns,
        };
        if let Some(key) = fill(out, section, items, has_more, &mut size, budget) {
            next = Some((section, key));
            break;
        }
    }
    page.extend([
        ("runs".into(), Value::Array(runs)),
        ("recoveries".into(), Value::Array(recoveries)),
        ("unreadEventIds".into(), Value::Array(events)),
        ("cooldowns".into(), Value::Array(cooldowns)),
    ]);
    if let Some((section, key)) = next {
        page.extend([(
            "nextCursor".into(),
            Value::String(encode_cursor(section, &key)),
        )]);
    }
    Ok(Value::Object(page))
}

#[cfg(test)]
mod tests;

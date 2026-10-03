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

/// What `fill` did with one section's items.
enum Fill {
    /// Every fetched item emitted — `Some` is the last emitted key, kept
    /// so a later section's budget refusal can emit `nextCursor` at the
    /// section boundary: resuming inside THIS section after that key
    /// lands on the same next rows the cut hid.
    Done(Option<String>),
    /// The budget cut mid-section — resume inside this section strictly
    /// after this key.
    Cut(String),
    /// The section's first item cannot fit the remaining budget and the
    /// page already carries items: emit `nextCursor` at the boundary
    /// (the previous section's last emitted key) — never the
    /// over-budget item (F2).
    Boundary,
}

/// Push `(item, key)` pairs into `out` under `budget`, updating `size`;
/// the per-item cost is its serialized length plus one byte of JSON
/// punctuation once `out` is non-empty, plus `key`'s `nextCursor`
/// reservation. The byte budget is HARD (§4.12): a section's first item
/// emits over-budget only while the whole page is still empty — the one
/// case where no boundary cursor could ever resume past it.
fn fill(
    out: &mut Vec<Value>,
    section: Section,
    items: Vec<(Value, String)>,
    has_more: bool,
    size: &mut usize,
    budget: usize,
    page_empty: bool,
) -> Fill {
    let mut last_key = None;
    for (item, key) in items {
        let extra = item_len(&item).saturating_add(usize::from(!out.is_empty()));
        if size
            .saturating_add(extra)
            .saturating_add(cursor_cost(section, &key))
            > budget
        {
            if out.is_empty() {
                // A page that has emitted nothing yet must still advance:
                // the oversized item emits with its own cursor (§4.12).
                // Once the page carries items, a new section's first
                // item waits — the boundary cursor resumes to it.
                if page_empty {
                    *size = size.saturating_add(extra);
                    out.push(item);
                    return Fill::Cut(key);
                }
                return Fill::Boundary;
            }
            return match last_key {
                Some(cut) => Fill::Cut(cut),
                // `out` is non-empty here, so a key was recorded.
                None => Fill::Boundary,
            };
        }
        *size = size.saturating_add(extra);
        last_key = Some(key);
        out.push(item);
    }
    match last_key {
        Some(cut) if has_more => Fill::Cut(cut),
        last => Fill::Done(last),
    }
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
    now: Timestamp,
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
                .map(|run| (render::run_item(run), run.id.0.clone()))
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
                .map(|recovery| {
                    (
                        render::recovery_item(recovery),
                        recovery.predecessor.0.clone(),
                    )
                })
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
                // Live only — the same `until > now` boundary
                // `routing::cooling_down` draws: cooldown rows persist
                // past expiry, an expired one is not an active exclusion.
                .filter(|cooldown| cooldown.until > now)
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
/// - the byte budget gates every emit: a page never exceeds it — a cut
///   emits `nextCursor` at the last emitted key, a section that cannot
///   open emits it at the boundary (the previous section's last key),
///   and only a wholly empty page carries an oversized first item.
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
        ("health".into(), render::health(view)),
        ("config".into(), render::config(view)),
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
    // The last emitted (section, key) pair — a later section's budget
    // refusal emits `nextCursor` at this boundary: the resume drains the
    // remainder of that section (none — it completed) and lands on the
    // section the page had no room to open.
    let mut boundary: Option<(Section, String)> = None;
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
        let (items, has_more) = section_items(store, caller, section, after, view.now)?;
        let out = match section {
            Section::Runs => &mut runs,
            Section::Recoveries => &mut recoveries,
            Section::Events => &mut events,
            Section::Cooldowns => &mut cooldowns,
        };
        // The first-item exception applies only while the whole page is
        // still empty — `boundary` is unset exactly then.
        match fill(
            out,
            section,
            items,
            has_more,
            &mut size,
            budget,
            boundary.is_none(),
        ) {
            Fill::Done(last_key) => {
                if let Some(key) = last_key {
                    boundary = Some((section, key));
                }
            }
            Fill::Cut(key) => {
                next = Some((section, key));
                break;
            }
            Fill::Boundary => {
                next = boundary;
                break;
            }
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

mod render;

#[cfg(test)]
mod tests;

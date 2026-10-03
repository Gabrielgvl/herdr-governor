//! `render` — the §4.12 Value renderers `page` composes: the bounded
//! `health`/`config` head and the per-section item bodies. Pure maps
//! from view/store rows to `serde_json::Value` — no I/O, no clock.

use governor_core::lifecycle::Run;
use governor_core::recovery::RecoveryObligation;

use serde_json::{Map, Value, json};

use super::StatusView;

/// §4.12 — the health block: `herdr.freshSecsAgo`/`incarnation` are
/// `null` until the first good snapshot.
pub(super) fn health(view: &StatusView) -> Value {
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
pub(super) fn config(view: &StatusView) -> Value {
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
pub(super) fn run_item(run: &Run) -> Value {
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
pub(super) fn recovery_item(recovery: &RecoveryObligation) -> Value {
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

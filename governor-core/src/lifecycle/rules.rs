//! F22 — the Appendix C transition rules kept as data, so the rendered
//! table can never drift from the code that implements it.

/// F22 — the Appendix C transition rules as data `(state, event, outcome)`,
/// in the spec's spellings, so the table is rendered from the code and never
/// copied (Phase 3 DoD). `*` reads "any state"; `unsettled` reads "any state
/// but `settled`".
pub const TRANSITION_RULES: &[(&str, &str, &str)] = &[
    ("settled", "cancel(closePane)", "close the pane only"),
    (
        "settled",
        "any other event",
        "ignored, including a late handoff or judgment — settlement is immutable",
    ),
    (
        "*",
        "obs(invalid)",
        "no change except health reporting; deadlines still run",
    ),
    (
        "unsettled",
        "deadline(max_age)",
        "settle unresolved(max_age)",
    ),
    (
        "unsettled",
        "cancel",
        "settle cancelled; closePane also closes",
    ),
    (
        "*",
        "restart",
        "dispatching effects become unconfirmed (F8); every Run re-derived from its persisted state; deadlines unchanged",
    ),
    (
        "unsettled",
        "provider_limited",
        "settle provider_limited (F21)",
    ),
    (
        "*",
        "evidence(unchanged digest)",
        "ignored — the same digest re-keys nothing (F23)",
    ),
    (
        "unsettled",
        "evidence(new digest)",
        "recorded; evidence_generation+1 — in judging the pending acceptance ask is re-planned for judging_digest, no second freeze row; unanswered reviews re-ask (F23)",
    ),
    (
        "unsettled",
        "no_recent_progress answered on a blocked child",
        "no nudge — a blocked child is never prompted (F17); the episode's nudge stays unspent",
    ),
    (
        "reserved",
        "obs(absent)",
        "settle unresolved(launch_not_started)",
    ),
    (
        "reserved",
        "topology effect planned",
        "starting (the launch plan write moves it)",
    ),
    (
        "reserved",
        "launch abstains or fails before any effect",
        "unresolved(launch_not_started) via settle; the Launch reports its outcome",
    ),
    (
        "starting",
        "start acknowledged",
        "prompting; task prompt planned",
    ),
    (
        "starting",
        "typed pre-interactive failure with another candidate",
        "stays starting; next candidate planned in the same pane",
    ),
    (
        "starting",
        "failure with no candidate, or unconfirmed",
        "stays starting until obs(absent) or max_age",
    ),
    (
        "starting",
        "obs(absent)",
        "settle unresolved(launch_failed)",
    ),
    (
        "prompting",
        "prompt acknowledged",
        "active; prompt_certainty acknowledged",
    ),
    (
        "prompting",
        "prompt unconfirmed or failed",
        "active; prompt_certainty unconfirmed + prompt_unconfirmed event",
    ),
    ("prompting", "obs(absent)", "settle pane_lost"),
    (
        "active",
        "obs(working)",
        "clear idle_since; the episode ends",
    ),
    (
        "active",
        "obs(idle|done) with no handoff",
        "open the idle episode; one nudge; idle_deadline set",
    ),
    (
        "active",
        "obs(blocked)",
        "ask blocked_on_input and provider_limited — once per blocked episode (F21)",
    ),
    ("active", "deadline(idle)", "settle no_handoff"),
    ("active", "handoff(valid)", "freeze; judging"),
    (
        "active",
        "obs(absent)",
        "one-shot handoff read: valid → freeze + judging; otherwise pane_lost",
    ),
    ("judging", "judgment(accept)", "settle accepted"),
    (
        "judging",
        "judgment(reject)",
        "repair; repair_deadline armed once per work generation",
    ),
    (
        "judging",
        "judgment(unavailable)",
        "stays judging until judgment_deadline",
    ),
    (
        "judging",
        "judgment while a qualifying repair dispatch is in flight",
        "deferred — produces nothing; the pending dispatch's result decides (F24)",
    ),
    (
        "judging",
        "deadline(judgment)",
        "settle unresolved(judgment_unavailable)",
    ),
    (
        "judging",
        "deadline(repair) armed and passed",
        "settle rejected",
    ),
    (
        "judging",
        "deadline(repair) with a qualifying dispatch in flight",
        "stays judging — the pending dispatch's result decides",
    ),
    (
        "judging",
        "repair follow-up dispatched before repair_deadline",
        "work_generation+1; active — the journal's dispatched_at lands inside [rejected_at, repair_deadline); a provably-absent failure does not qualify; a fresh nudge episode opens (F25)",
    ),
    (
        "judging",
        "repair follow-up resolved past the deadline without qualifying",
        "settle rejected once no qualifying dispatch is still in flight",
    ),
    (
        "judging",
        "repair follow-up resolved provably-absent inside the window",
        "re-plan the acceptance ask for judging_digest — a deferred verdict cannot strand (F24)",
    ),
    (
        "judging",
        "obs(absent)",
        "stays judging; the frozen handoff is judged",
    ),
    (
        "judging",
        "obs(blocked)",
        "ask blocked_on_input and provider_limited — once per blocked episode (F21)",
    ),
    ("judging", "handoff(new digest)", "re-freeze; stays judging"),
    (
        "judging",
        "handoff(frozen digest, assessed)",
        "ignored — a completed assessment is never re-judged (F24)",
    ),
    (
        "judging",
        "handoff(frozen digest, ask in flight)",
        "ignored — its acceptance ask is still in flight (F20)",
    ),
    (
        "judging",
        "handoff(frozen digest, unassessed)",
        "resume judging — the ask in flight names a different digest; a fresh evidence_generation re-asks; no second freeze row",
    ),
    ("judging", "stale judgment", "ignored (F20)"),
    (
        "repair",
        "repair follow-up dispatched before repair_deadline",
        "work_generation+1; active — the journal's dispatched_at lands inside [rejected_at, repair_deadline); a provably-absent failure does not qualify; a fresh nudge episode opens (F25)",
    ),
    (
        "repair",
        "repair follow-up resolved past the deadline without qualifying",
        "settle rejected once no qualifying dispatch is still in flight",
    ),
    (
        "repair",
        "handoff(new digest)",
        "freeze; judging (repair_deadline kept)",
    ),
    (
        "repair",
        "handoff(frozen digest, assessed)",
        "stays repair — a completed assessment is never re-judged (F24)",
    ),
    (
        "repair",
        "handoff(frozen digest, unassessed)",
        "resume judging — a fresh evidence_generation re-asks; no second freeze row; repair_deadline kept",
    ),
    ("repair", "deadline(repair)", "settle rejected"),
    (
        "repair",
        "deadline(repair) with a qualifying dispatch in flight",
        "stays repair — the pending dispatch's result decides",
    ),
    ("repair", "obs(absent)", "stays repair until the deadline"),
    (
        "repair",
        "obs(blocked)",
        "ask blocked_on_input and provider_limited — once per blocked episode (F21)",
    ),
];

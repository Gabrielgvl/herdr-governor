-- Verified at open: PRAGMA foreign_keys (must read 1), journal_mode=WAL, synchronous=FULL.
-- Schema version in PRAGMA user_version; migrations are expand-only (Phase 7).

CREATE TABLE callers (
  caller_id      INTEGER PRIMARY KEY,
  agent_kind     TEXT NOT NULL,
  native_session TEXT NOT NULL,
  first_seen_at  TEXT NOT NULL,
  UNIQUE (agent_kind, native_session)
);

CREATE TABLE relay_bindings (
  relay_instance_id TEXT PRIMARY KEY,                     -- 128-bit random id minted by the relay, lowercase hex
  caller_id         INTEGER NOT NULL REFERENCES callers(caller_id),
  pane_id_at_bind   TEXT NOT NULL,
  bound_at          TEXT NOT NULL
);

CREATE TABLE launches (
  launch_id       TEXT PRIMARY KEY,                       -- uuid v7
  caller_id       INTEGER NOT NULL REFERENCES callers(caller_id),
  project_root    TEXT NOT NULL,
  idempotency_key TEXT NOT NULL,
  digest_version  INTEGER NOT NULL,
  task_digest     TEXT NOT NULL,
  task_json       TEXT NOT NULL,
  phase           TEXT NOT NULL CHECK (phase IN ('evaluating','routed','launching','done')),
  outcome         TEXT CHECK (outcome IN ('launched','abstained','rejected','failed')),
  outcome_reason  TEXT,
  decision_json   TEXT,          -- F13: floors, requested tier, exploration assignment, candidates with args
  config_version  TEXT,
  result_json     TEXT,
  created_at      TEXT NOT NULL,
  updated_at      TEXT NOT NULL,
  UNIQUE (caller_id, project_root, idempotency_key),
  CHECK ((phase = 'done') = (outcome IS NOT NULL))
);

CREATE TABLE runs (
  run_id              TEXT PRIMARY KEY,
  launch_id           TEXT NOT NULL UNIQUE REFERENCES launches(launch_id),
  owner_caller_id     INTEGER NOT NULL REFERENCES callers(caller_id),
  owner_generation    INTEGER NOT NULL DEFAULT 0,
  version             INTEGER NOT NULL DEFAULT 0,
  state               TEXT NOT NULL CHECK (state IN ('reserved','starting','prompting','active','judging','repair','settled')),
  prompt_certainty    TEXT CHECK (prompt_certainty IN ('acknowledged','unconfirmed')),
  child_name          TEXT NOT NULL UNIQUE,
  herdr_incarnation   TEXT,
  terminal_id         TEXT,
  agent_kind          TEXT,
  agent_name          TEXT,
  native_session      TEXT,
  pane_id             TEXT,                                -- current locator only
  operating_point_id  TEXT,
  provider            TEXT,
  tier_start          TEXT,
  cwd                 TEXT NOT NULL,
  base_commit         TEXT,
  work_generation     INTEGER NOT NULL DEFAULT 0,
  evidence_generation INTEGER NOT NULL DEFAULT 0,
  evidence_digest     TEXT,                           -- the last recorded transcript/git evidence digest (F23)
  child_status        TEXT,
  idle_since          TEXT,
  idle_deadline       TEXT,
  repair_deadline     TEXT,
  rejected_at         TEXT,
  judgment_deadline   TEXT,
  judging_digest      TEXT,                           -- the handoff digest the current acceptance ask assesses
  max_age_deadline    TEXT NOT NULL,
  nudge_episode       INTEGER NOT NULL DEFAULT 0,
  nudged_episode      INTEGER,
  blocked_episode     INTEGER NOT NULL DEFAULT 0,     -- the blocked-observation episode (F23)
  settlement          TEXT CHECK (settlement IN ('accepted','rejected','no_handoff','pane_lost','cancelled','provider_limited','unresolved')),
  settlement_reason   TEXT,
  settled_at          TEXT,
  created_at          TEXT NOT NULL,
  updated_at          TEXT NOT NULL,
  CHECK ((state = 'settled') = (settlement IS NOT NULL)),
  CHECK ((settlement IS NULL) = (settled_at IS NULL)),
  CHECK (settlement IS NOT 'unresolved' OR settlement_reason IS NOT NULL)
);

CREATE TRIGGER runs_settlement_immutable
BEFORE UPDATE OF settlement, settlement_reason, settled_at ON runs
WHEN OLD.settlement IS NOT NULL
BEGIN SELECT RAISE(ABORT, 'settlement is immutable'); END;

CREATE TABLE effects (
  effect_id         TEXT PRIMARY KEY,
  effect_key        TEXT NOT NULL UNIQUE,   -- e.g. run:<id>:prompt:task, run:<id>:outbox:<seq>, run:<id>:nudge:<episode>, event:<id>:hint
  kind              TEXT NOT NULL CHECK (kind IN ('jev_evaluate','tab_create','pane_split','agent_start','prompt','close')),
  subject_launch_id TEXT REFERENCES launches(launch_id),
  subject_run_id    TEXT REFERENCES runs(run_id),
  target_json       TEXT,
  payload_digest    TEXT,
  state             TEXT NOT NULL CHECK (state IN ('planned','dispatching','acknowledged','failed','unconfirmed')),
  certainty         TEXT CHECK (certainty IN ('absent','unknown')),
  result_json       TEXT,
  planned_at        TEXT NOT NULL,
  dispatched_at     TEXT,
  completed_at      TEXT,
  CHECK (subject_launch_id IS NOT NULL OR subject_run_id IS NOT NULL),
  CHECK (state <> 'failed' OR certainty IS NOT NULL)
);

CREATE TABLE outbox (
  run_id           TEXT NOT NULL REFERENCES runs(run_id),
  seq              INTEGER NOT NULL,
  message_key      TEXT NOT NULL,
  sender_caller_id INTEGER NOT NULL REFERENCES callers(caller_id),
  body_digest      TEXT NOT NULL,
  body_inline      TEXT,
  body_path        TEXT,
  state            TEXT NOT NULL CHECK (state IN ('queued','dispatching','submitted','unconfirmed','expired')),
  effect_id        TEXT UNIQUE REFERENCES effects(effect_id),
  expiry_reason    TEXT,
  enqueued_at      TEXT NOT NULL,
  finished_at      TEXT,
  PRIMARY KEY (run_id, seq),
  UNIQUE (run_id, message_key),
  CHECK ((body_inline IS NULL) <> (body_path IS NULL)),
  CHECK (state <> 'expired' OR (effect_id IS NULL AND expiry_reason IS NOT NULL))
);

CREATE TABLE mailbox (
  event_id   TEXT PRIMARY KEY,
  dedup_key  TEXT NOT NULL UNIQUE,          -- e.g. run:<id>:settled, run:<id>:stalled:<episode>
  launch_id  TEXT REFERENCES launches(launch_id),
  run_id     TEXT REFERENCES runs(run_id),
  kind       TEXT NOT NULL,
  body_json  TEXT NOT NULL,
  acked_at   TEXT,
  created_at TEXT NOT NULL,
  CHECK (launch_id IS NOT NULL OR run_id IS NOT NULL)
);
-- The destination is derived when read: runs.owner_caller_id, or launches.caller_id for launch-only events.

CREATE TABLE handoffs (
  run_id          TEXT NOT NULL REFERENCES runs(run_id),
  work_generation INTEGER NOT NULL,
  digest          TEXT NOT NULL,
  frozen_path     TEXT NOT NULL,
  frozen_at       TEXT NOT NULL,
  PRIMARY KEY (run_id, work_generation, digest)
);

CREATE TABLE judgment_sets (
  set_id              TEXT PRIMARY KEY,
  purpose             TEXT NOT NULL CHECK (purpose IN ('launch','review','acceptance','provider_limit')),
  launch_id           TEXT REFERENCES launches(launch_id),
  run_id              TEXT REFERENCES runs(run_id),
  run_version         INTEGER,
  work_generation     INTEGER,
  evidence_generation INTEGER,
  task_digest         TEXT NOT NULL,
  handoff_digest      TEXT,
  evidence_digest     TEXT,
  model               TEXT NOT NULL,
  question_version    TEXT NOT NULL,
  policy_version      TEXT NOT NULL,
  outcome             TEXT NOT NULL CHECK (outcome IN ('answered','transport_failed','auth_failed','invalid_response','too_large','stale')),
  requested_at        TEXT NOT NULL,
  answered_at         TEXT,
  CHECK (launch_id IS NOT NULL OR run_id IS NOT NULL)
);

CREATE TABLE judgments (
  set_id             TEXT NOT NULL REFERENCES judgment_sets(set_id),
  question           TEXT NOT NULL,
  probabilities_json TEXT NOT NULL,
  answer             TEXT NOT NULL,
  threshold          REAL,
  PRIMARY KEY (set_id, question)
);

CREATE TABLE recoveries (
  predecessor_run_id  TEXT PRIMARY KEY REFERENCES runs(run_id),
  origin              TEXT NOT NULL CHECK (origin IN ('provider_limit','caller')),
  state               TEXT NOT NULL CHECK (state IN ('pending','blocked','dispatched','failed')),
  reason              TEXT,
  successor_launch_id TEXT UNIQUE REFERENCES launches(launch_id),
  expires_at          TEXT NOT NULL,
  created_at          TEXT NOT NULL,
  updated_at          TEXT NOT NULL,
  CHECK ((state = 'dispatched') = (successor_launch_id IS NOT NULL))
);

CREATE TABLE cooldowns (
  provider      TEXT PRIMARY KEY,
  until         TEXT NOT NULL,              -- upsert keeps max(existing, new): never shortened
  reason        TEXT NOT NULL,
  source_run_id TEXT REFERENCES runs(run_id),
  updated_at    TEXT NOT NULL
);

CREATE TABLE qualifications (
  operating_point_id TEXT NOT NULL,
  args_digest        TEXT NOT NULL,
  capability         TEXT NOT NULL,
  passed             INTEGER NOT NULL CHECK (passed IN (0,1)),
  evidence_json      TEXT NOT NULL,
  qualified_at       TEXT NOT NULL,
  PRIMARY KEY (operating_point_id, args_digest, capability)
);

CREATE VIEW outcomes AS
SELECT r.run_id,
       r.settlement,
       r.settlement_reason,
       r.tier_start,
       r.operating_point_id,
       json_extract(l.decision_json, '$.exploration.assigned') AS explored_assigned,
       json_extract(l.decision_json, '$.exploration.executed') AS explored_executed,
       (SELECT count(*) FROM effects e WHERE e.subject_run_id = r.run_id AND e.effect_key LIKE 'run:%:nudge:%') AS nudges,
       (SELECT count(*) FROM judgment_sets j WHERE j.run_id = r.run_id AND j.purpose = 'acceptance' AND j.outcome = 'answered') AS acceptance_rounds,
       rc.successor_launch_id AS recovered_by,
       (julianday(r.settled_at) - julianday(r.created_at)) * 86400 AS seconds_to_settle
FROM runs r
JOIN launches l ON l.launch_id = r.launch_id
LEFT JOIN recoveries rc ON rc.predecessor_run_id = r.run_id;

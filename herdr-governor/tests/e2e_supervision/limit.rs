//! `limit` — the P5.C8 provider-limit e2e (§4.17, F31, §5 P1–P9): a real
//! `daemon::run` in-process against `FakeHerdr` + `FakeJev` with an
//! `active` Run seeded through a second `Store` connection. The evidence
//! pass scans the child's native provider-limit evidence — the Devin
//! process-log dir (`[daemon] devin_log_dir`, pinned to the test's dir)
//! or the Claude `sessionId`/`cwd`-bound 429 JSONL under
//! `transcript_project_dirs` — and plans the once-per-record
//! `limit:<record_id>` ask; the seeded `run:r1:prompt:task` journal row's
//! `dispatched_at` is the pass's `cycle_start` anchor. Every wait is a
//! bounded poll. `world` is the shared fixture; `devin`/`claude` hold the
//! provider-split cases.

mod claude;
mod devin;
mod world;

//! F13/Appendix B "Route" — the reserved `runs` row, whole. `UNIQUE
//! launch_id` is the one-Run-per-Launch rule; its collision (or the
//! `run_id`/`child_name` keys) is a [`ConflictKind::Run`] conflict.

use rusqlite::Transaction;

use governor_core::identity::Timestamp;
use governor_core::lifecycle::Run;

use crate::store::error::{ApplyError, ConflictKind};
use crate::store::rows::run::RunRow;
use crate::store::transitions::{insert_unique, resolve_caller};

pub(super) fn apply(tx: &Transaction<'_>, run: &Run, now: Timestamp) -> Result<(), ApplyError> {
    let owner = resolve_caller(tx, &run.owner)?;
    let row = RunRow::from_core(run, owner, now)?;
    insert_unique(tx, "runs", &row.params(), ConflictKind::Run, &run.id.0)
}

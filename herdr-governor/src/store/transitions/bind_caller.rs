//! F1/Appendix B "Bind caller" — the `callers` row when the key is new,
//! then the `relay_bindings` row. A relay id never rebinds: its primary-key
//! collision is a [`ConflictKind::RelayBinding`] conflict.

use rusqlite::Transaction;

use governor_core::identity::{CallerBinding, Timestamp};

use crate::store::error::{ApplyError, ConflictKind};
use crate::store::rows::caller::{CallerRow, RelayBindingRow};
use crate::store::transitions::{insert_dedup, insert_unique, resolve_caller};

pub(super) fn apply(
    tx: &Transaction<'_>,
    binding: &CallerBinding,
    now: Timestamp,
) -> Result<(), ApplyError> {
    let caller = CallerRow::from_core(&binding.caller, now)?;
    insert_dedup(
        tx,
        "callers",
        &caller.params(),
        &["agent_kind", "native_session"],
    )?;
    let caller_id = resolve_caller(tx, &binding.caller)?;
    let row = RelayBindingRow::from_core(binding, caller_id, now)?;
    insert_unique(
        tx,
        "relay_bindings",
        &row.params(),
        ConflictKind::RelayBinding,
        &binding.relay_instance.0,
    )
}

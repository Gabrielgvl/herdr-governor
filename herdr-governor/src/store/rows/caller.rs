//! `caller` — the `callers`/`relay_bindings` rows and the `CallerKey` join
//! helper. The caller's surrogate `caller_id` is the join key every
//! caller-owned row resolves through; the core vocabulary never sees it.

use rusqlite::{Connection, OptionalExtension as _, Row, params};

use governor_core::identity::{
    AgentKind, CallerBinding, CallerKey, NativeSession, PaneId, RelayInstanceId, Timestamp,
};

use crate::store::error::StoreError;
use crate::store::rows::{Params, read_col, ts_decode, ts_encode};

/// The `callers` row — `caller_id` is the surrogate primary key, never
/// encoded here: a writer binds `agent_kind`/`native_session`/`first_seen_at`
/// and reads the assigned id back with [`caller_id`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::store) struct CallerRow {
    agent_kind: String,
    native_session: String,
    first_seen_at: String,
}

impl CallerRow {
    /// Encode `key` for the `callers` write side; `first_seen_at` is the
    /// store stamp of the first bind.
    pub(in crate::store) fn from_core(
        key: &CallerKey,
        first_seen_at: Timestamp,
    ) -> Result<Self, StoreError> {
        Ok(Self {
            agent_kind: key.agent_kind.0.clone(),
            native_session: key.native_session.0.clone(),
            first_seen_at: ts_encode(first_seen_at, "callers", "first_seen_at")?,
        })
    }

    /// The store's first-seen stamp for this caller.
    pub(in crate::store) fn first_seen_at(&self) -> Result<Timestamp, StoreError> {
        ts_decode(&self.first_seen_at, "callers", "first_seen_at")
    }

    /// The row as bindable `(column, value)` pairs.
    pub(in crate::store) fn params(&self) -> Params {
        vec![
            ("agent_kind", self.agent_kind.clone().into()),
            ("native_session", self.native_session.clone().into()),
            ("first_seen_at", self.first_seen_at.clone().into()),
        ]
    }

    /// Pull the row's own columns out of a query row (joined caller columns
    /// go through [`key_from_row`]).
    pub(in crate::store) fn read(row: &Row<'_>) -> Result<Self, StoreError> {
        Ok(Self {
            agent_kind: read_col(row, "callers", "agent_kind")?,
            native_session: read_col(row, "callers", "native_session")?,
            first_seen_at: read_col(row, "callers", "first_seen_at")?,
        })
    }
}

/// The `callers.caller_id` of `key`, or `None` when no row names it — the
/// caller-bind writer resolves this after its `callers` write, and the read
/// API's caller-scoped lookups use it for `*_caller_id` predicates.
pub(in crate::store) fn caller_id(
    conn: &Connection,
    key: &CallerKey,
) -> Result<Option<i64>, StoreError> {
    conn.query_row(
        "SELECT caller_id FROM callers WHERE agent_kind = ?1 AND native_session = ?2",
        params![key.agent_kind.0, key.native_session.0],
        |row| row.get::<_, i64>(0),
    )
    .optional()
    .map_err(StoreError::Sqlite)
}

/// The `CallerKey` behind a joined row: read queries select the caller's
/// columns under role-prefixed aliases (`caller_` for `launches` and
/// `relay_bindings`, `owner_` for `runs`, `sender_` for `outbox`) so they
/// never collide with a row's own columns.
pub(in crate::store) fn key_from_row(
    row: &Row<'_>,
    table: &'static str,
    kind_column: &'static str,
    session_column: &'static str,
) -> Result<CallerKey, StoreError> {
    Ok(CallerKey {
        agent_kind: AgentKind(read_col(row, table, kind_column)?),
        native_session: NativeSession(read_col(row, table, session_column)?),
    })
}

/// The `relay_bindings` row (`bound_at` is store-stamped; the core
/// `CallerBinding` carries no time).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::store) struct RelayBindingRow {
    relay_instance_id: String,
    caller_id: i64,
    pane_id_at_bind: String,
    bound_at: String,
}

impl RelayBindingRow {
    /// Encode `binding` for the `relay_bindings` write side; `caller` is the
    /// surrogate [`caller_id`] the writer resolved for `binding.caller`.
    pub(in crate::store) fn from_core(
        binding: &CallerBinding,
        caller: i64,
        bound_at: Timestamp,
    ) -> Result<Self, StoreError> {
        Ok(Self {
            relay_instance_id: binding.relay_instance.0.clone(),
            caller_id: caller,
            pane_id_at_bind: binding.pane_at_bind.0.clone(),
            bound_at: ts_encode(bound_at, "relay_bindings", "bound_at")?,
        })
    }

    /// The binding this row names, given the joined caller key.
    pub(in crate::store) fn to_core(&self, caller: CallerKey) -> CallerBinding {
        CallerBinding {
            caller,
            relay_instance: RelayInstanceId(self.relay_instance_id.clone()),
            pane_at_bind: PaneId(self.pane_id_at_bind.clone()),
        }
    }

    /// The row as bindable `(column, value)` pairs.
    pub(in crate::store) fn params(&self) -> Params {
        vec![
            ("relay_instance_id", self.relay_instance_id.clone().into()),
            ("caller_id", self.caller_id.into()),
            ("pane_id_at_bind", self.pane_id_at_bind.clone().into()),
            ("bound_at", self.bound_at.clone().into()),
        ]
    }

    /// Pull the binding's own columns out of a query row.
    pub(in crate::store) fn read(row: &Row<'_>) -> Result<Self, StoreError> {
        Ok(Self {
            relay_instance_id: read_col(row, "relay_bindings", "relay_instance_id")?,
            caller_id: read_col(row, "relay_bindings", "caller_id")?,
            pane_id_at_bind: read_col(row, "relay_bindings", "pane_id_at_bind")?,
            bound_at: read_col(row, "relay_bindings", "bound_at")?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{CallerRow, RelayBindingRow, caller_id};
    use crate::store::Store;
    use governor_core::identity::{
        AgentKind, CallerBinding, CallerKey, NativeSession, PaneId, RelayInstanceId, Timestamp,
    };
    use tempfile::tempdir;

    fn key() -> CallerKey {
        CallerKey {
            agent_kind: AgentKind("kind-a".into()),
            native_session: NativeSession("sess-1".into()),
        }
    }

    #[test]
    fn caller_row_round_trips() {
        let row = CallerRow::from_core(&key(), Timestamp(1_759_276_800_000)).unwrap();
        assert_eq!(
            row.first_seen_at().unwrap(),
            Timestamp(1_759_276_800_000),
            "the first-seen stamp must survive"
        );
        assert_eq!(
            row.params().len(),
            3,
            "callers binds its three non-key columns"
        );
    }

    #[test]
    fn relay_binding_row_round_trips() {
        let binding = CallerBinding {
            caller: key(),
            relay_instance: RelayInstanceId("0123456789abcdef0123456789abcdef".into()),
            pane_at_bind: PaneId("pane-7".into()),
        };
        let row = RelayBindingRow::from_core(&binding, 42, Timestamp(5)).unwrap();
        assert_eq!(
            row.to_core(key()),
            binding,
            "relay_bindings decode must invert encode"
        );
        assert_eq!(row.params().len(), 4, "relay_bindings binds four columns");
    }

    #[test]
    fn missing_caller_row_is_none() {
        // The real schema, no rows: `None`, not an error — the writer
        // decides whether to bind a new caller.
        let dir = tempdir().unwrap();
        let store = Store::open(&dir.path().join("store.db")).unwrap();
        assert_eq!(
            caller_id(store.conn(), &key()).unwrap(),
            None,
            "no row → None"
        );
    }
}

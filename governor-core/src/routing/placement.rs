//! F14 — the placement plan: where the new pane goes once the routing
//! decision stands.

use crate::identity::TabId;

use super::{PlacementPlan, TAB_PANE_MAX, TabChoice};

/// F14 — where the new pane goes: the tab Jev picked while it still holds
/// fewer than `TAB_PANE_MAX` panes (the right split, no focus — H#53), else
/// a new tab. `open_tabs` is the caller's current open governor tabs with
/// their pane counts; a picked tab that has since closed or filled plans a
/// new tab, whose initial pane is used, never split (H#102).
#[must_use]
pub fn placement_plan(
    related_tab: Option<&TabChoice>,
    open_tabs: &[(TabId, usize)],
) -> PlacementPlan {
    match related_tab {
        Some(TabChoice::Tab(tab)) => {
            let fits = open_tabs
                .iter()
                .any(|(id, panes)| id == tab && *panes < TAB_PANE_MAX);
            if fits {
                PlacementPlan::ExistingTab { tab: tab.clone() }
            } else {
                PlacementPlan::NewTab
            }
        }
        Some(TabChoice::New) | None => PlacementPlan::NewTab,
    }
}

//! F14 — the placement plan: Jev's tab while it still fits, else a new tab
//! whose initial pane is used and never split.

use crate::identity::TabId;
use crate::routing::{PlacementPlan, TAB_PANE_MAX, TabChoice, placement_plan};

#[test]
fn f14_picked_tab_under_the_cap_splits() {
    let tab = TabId("tab-1".into());
    let plan = placement_plan(
        Some(&TabChoice::Tab(tab.clone())),
        &[(tab.clone(), TAB_PANE_MAX.saturating_sub(1))],
    );
    assert_eq!(
        plan,
        PlacementPlan::ExistingTab { tab },
        "under four panes Jev's tab takes the right split without focus"
    );
}

#[test]
fn f14_full_tab_plans_a_new_tab() {
    let tab = TabId("tab-1".into());
    let plan = placement_plan(
        Some(&TabChoice::Tab(tab.clone())),
        &[(tab.clone(), TAB_PANE_MAX)],
    );
    assert_eq!(
        plan,
        PlacementPlan::NewTab,
        "a tab at four panes plans a new tab, never a fifth pane"
    );
    let over = placement_plan(
        Some(&TabChoice::Tab(tab.clone())),
        &[(tab, TAB_PANE_MAX.saturating_add(1))],
    );
    assert_eq!(over, PlacementPlan::NewTab);
}

#[test]
fn f14_no_usable_tab_plans_a_new_tab() {
    // Jev picked `new`.
    assert_eq!(
        placement_plan(Some(&TabChoice::New), &[(TabId("tab-1".into()), 1)]),
        PlacementPlan::NewTab
    );
    // The question was never asked or Jev abstained from a choice.
    assert_eq!(
        placement_plan(None, &[(TabId("tab-1".into()), 1)]),
        PlacementPlan::NewTab
    );
    // The picked tab has since closed — a new tab, never a split.
    assert_eq!(
        placement_plan(
            Some(&TabChoice::Tab(TabId("gone".into()))),
            &[(TabId("tab-1".into()), 1)],
        ),
        PlacementPlan::NewTab
    );
}

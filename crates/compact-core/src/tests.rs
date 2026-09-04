use crate::chrome::{ChromeConfig, ChromeVisibility, Reveal, TabLayout};
use crate::command::{resolve, Action, Mods};
use crate::policy::{plan_eviction, prewarm_candidates, LiveBudget, Pressure};
use crate::tab::{TabId, TabMeta, TabState, ESTIMATED_LIVE_BYTES};
use crate::{App, Effect, Overlay};

const MB: u64 = 1024 * 1024;

/// Live tabs with explicit footprints, ordered oldest-first by `last_active`.
fn live_tabs(footprints_mb: &[u64]) -> Vec<TabMeta> {
    footprints_mb
        .iter()
        .enumerate()
        .map(|(i, mb)| {
            let mut t = TabMeta::new(TabId(i as u64 + 1), format!("tab {i}"), "example.test");
            t.state = TabState::Live;
            t.last_active = i as u64 + 1;
            t.footprint = Some(mb * MB);
            t
        })
        .collect()
}

fn budget(max_mb: u64, max_live: usize, min_live: usize) -> LiveBudget {
    LiveBudget { max_bytes: max_mb * MB, max_live, min_live }
}

// ---- eviction ------------------------------------------------------------

#[test]
fn under_budget_evicts_nothing() {
    let tabs = live_tabs(&[100, 100]);
    let plan = plan_eviction(&tabs, TabId(2), &budget(600, 8, 1), Pressure::Normal);
    assert!(plan.is_empty());
}

#[test]
fn evicts_coldest_first_until_within_byte_budget() {
    // 5 x 100 MB = 500 MB against a 250 MB ceiling: shed until <= 250.
    let tabs = live_tabs(&[100, 100, 100, 100, 100]);
    let plan = plan_eviction(&tabs, TabId(5), &budget(250, 8, 1), Pressure::Normal);
    // Coldest are ids 1,2,3 (last_active 1..3); dropping those leaves 200 MB.
    assert_eq!(plan.hibernate, vec![TabId(1), TabId(2), TabId(3)]);
}

#[test]
fn never_evicts_the_active_tab_even_when_it_is_coldest() {
    // Id 1 is both the coldest and the active tab — the state you land in
    // after restoring an old tab from the switcher.
    let tabs = live_tabs(&[100, 100, 100]);
    let plan = plan_eviction(&tabs, TabId(1), &budget(100, 8, 1), Pressure::Normal);
    assert!(!plan.hibernate.contains(&TabId(1)));
    assert_eq!(plan.hibernate, vec![TabId(2), TabId(3)]);
}

#[test]
fn respects_min_live_floor() {
    let tabs = live_tabs(&[100, 100, 100, 100]);
    let plan = plan_eviction(&tabs, TabId(4), &budget(1, 8, 3), Pressure::Normal);
    // Floor of 3 leaves exactly one eviction available despite a 1 MB ceiling.
    assert_eq!(plan.hibernate.len(), 1);
}

#[test]
fn count_ceiling_applies_even_when_bytes_fit() {
    // Ten cheap tabs fit the byte budget but blow the count ceiling; each web
    // content process still carries fixed overhead.
    let tabs = live_tabs(&[10; 10]);
    let plan = plan_eviction(&tabs, TabId(10), &budget(600, 4, 1), Pressure::Normal);
    assert_eq!(plan.hibernate.len(), 6);
}

#[test]
fn critical_pressure_collapses_to_the_floor() {
    let tabs = live_tabs(&[10, 10, 10, 10, 10]);
    let plan = plan_eviction(&tabs, TabId(5), &budget(600, 8, 1), Pressure::Critical);
    // Everything but the active tab goes.
    assert_eq!(plan.hibernate.len(), 4);
    assert!(!plan.hibernate.contains(&TabId(5)));
}

#[test]
fn warn_pressure_halves_the_budget() {
    let tabs = live_tabs(&[100, 100, 100, 100]);
    let normal = plan_eviction(&tabs, TabId(4), &budget(400, 8, 1), Pressure::Normal);
    let warn = plan_eviction(&tabs, TabId(4), &budget(400, 8, 1), Pressure::Warn);
    assert!(normal.is_empty());
    assert_eq!(warn.hibernate.len(), 2, "400 MB -> 200 MB ceiling sheds two");
}

#[test]
fn unmeasured_live_tabs_are_charged_not_ignored() {
    // A tab with no measurement must still be evictable, or the budget becomes
    // unenforceable exactly when measurement is broken.
    let mut tabs = live_tabs(&[100, 100]);
    tabs[0].footprint = None;
    assert_eq!(tabs[0].effective_footprint(), ESTIMATED_LIVE_BYTES);
    let plan = plan_eviction(&tabs, TabId(2), &budget(120, 8, 1), Pressure::Normal);
    assert_eq!(plan.hibernate, vec![TabId(1)]);
}

#[test]
fn hibernated_tabs_are_free() {
    let mut t = TabMeta::new(TabId(1), "x", "example.test");
    t.state = TabState::Hibernated;
    t.footprint = Some(100 * MB);
    assert_eq!(t.effective_footprint(), 0, "2.3 KB is noise against a byte budget");
}

#[test]
fn budget_scales_with_available_memory() {
    let tight = LiveBudget::from_available(1024 * MB);
    let roomy = LiveBudget::from_available(32 * 1024 * MB);
    assert!(tight.max_bytes < roomy.max_bytes);
    assert!(tight.max_live <= roomy.max_live);
    assert!(tight.max_bytes >= 256 * MB, "clamped to a usable floor");
    assert!(roomy.max_bytes <= 4096 * MB, "clamped to a ceiling");
}

// ---- prewarming ----------------------------------------------------------

#[test]
fn prewarms_the_nearest_neighbour_into_slack() {
    let mut tabs = live_tabs(&[100]);
    for i in 1..3 {
        let mut t = TabMeta::new(TabId(i + 1), "cold", "example.test");
        t.state = TabState::Hibernated;
        tabs.push(t);
    }
    let out = prewarm_candidates(&tabs, TabId(1), &budget(600, 8, 1), Pressure::Normal, 1);
    assert_eq!(out, vec![TabId(2)], "the adjacent tab, not an arbitrary one");
}

#[test]
fn no_prewarming_without_headroom() {
    let mut tabs = live_tabs(&[100, 100]);
    let mut cold = TabMeta::new(TabId(3), "cold", "example.test");
    cold.state = TabState::Hibernated;
    tabs.push(cold);
    // Budget exactly covers the two live tabs.
    let out = prewarm_candidates(&tabs, TabId(1), &budget(200, 2, 1), Pressure::Normal, 1);
    assert!(out.is_empty(), "restoring here would churn against the next eviction");
}

#[test]
fn no_prewarming_under_pressure() {
    let mut tabs = live_tabs(&[10]);
    let mut cold = TabMeta::new(TabId(2), "cold", "example.test");
    cold.state = TabState::Hibernated;
    tabs.push(cold);
    let out = prewarm_candidates(&tabs, TabId(1), &budget(600, 8, 1), Pressure::Warn, 1);
    assert!(out.is_empty());
}

// ---- chrome reveal -------------------------------------------------------

#[test]
fn reveal_has_hysteresis() {
    let mut r = Reveal::default();
    let size = (1000.0, 700.0);
    r.update(Some((500.0, 40.0)), size, 76.0);
    assert!(r.top, "inside the threshold reveals");

    // Between the reveal and retract thresholds nothing changes -- this is the
    // band a user's cursor sits in while reaching for a tab.
    r.update(Some((500.0, 150.0)), size, 76.0);
    assert!(r.top, "still latched between 76 and 228");

    r.update(Some((500.0, 300.0)), size, 76.0);
    assert!(!r.top, "past 3x the threshold it retracts");
}

#[test]
fn leaving_the_window_drops_all_chrome() {
    let mut r = Reveal { top: true, side: true };
    assert!(r.update(None, (1000.0, 700.0), 76.0));
    assert_eq!(r, Reveal { top: false, side: false });
}

#[test]
fn update_reports_whether_anything_changed() {
    let mut r = Reveal::default();
    let size = (1000.0, 700.0);
    assert!(r.update(Some((500.0, 10.0)), size, 76.0));
    assert!(!r.update(Some((500.0, 12.0)), size, 76.0), "no redraw for a no-op move");
}

#[test]
fn sidebar_answers_to_the_top_edge_too() {
    let cfg = ChromeConfig::default();
    let v = ChromeVisibility::derive(Reveal { top: true, side: false }, &cfg);
    assert!(v.sidebar, "reaching for the title area should not strand the tabs");
    assert!(v.address);
    assert!(!v.strip, "vertical layout has no strip");
}

#[test]
fn pinning_shows_chrome_with_no_cursor_nearby() {
    let cfg = ChromeConfig { pinned: true, ..Default::default() };
    let v = ChromeVisibility::derive(Reveal::default(), &cfg);
    assert!(v.address && v.sidebar);
}

#[test]
fn horizontal_layout_swaps_strip_for_sidebar() {
    let cfg = ChromeConfig { layout: TabLayout::Horizontal, ..Default::default() };
    let v = ChromeVisibility::derive(Reveal { top: true, side: true }, &cfg);
    assert!(v.strip && !v.sidebar);
    assert_eq!(v.address_top(), 58.0, "address bar clears the strip");
}

#[test]
fn threshold_is_clamped_to_the_slider_range() {
    let mut cfg = ChromeConfig::default();
    cfg.set_threshold(5.0);
    assert_eq!(cfg.threshold, 24.0);
    cfg.set_threshold(9000.0);
    assert_eq!(cfg.threshold, 180.0);
}

// ---- keymap --------------------------------------------------------------

#[test]
fn resolves_the_documented_bindings() {
    let cmd = Mods { cmd: true, ..Default::default() };
    assert_eq!(resolve("k", cmd, false), Some(Action::OpenPalette));
    assert_eq!(resolve("K", cmd, false), Some(Action::OpenPalette), "case-insensitive");
    assert_eq!(resolve("t", cmd, false), Some(Action::NewTab));
    assert_eq!(resolve(",", cmd, false), Some(Action::OpenSettings));
    assert_eq!(resolve(" ", cmd, false), Some(Action::OpenSwitcher));
    assert_eq!(resolve("2", cmd, false), Some(Action::SwitchWorkspace(1)));
    assert_eq!(resolve("Escape", Mods::default(), false), Some(Action::Dismiss));
}

#[test]
fn shift_j_jumps_rather_than_falling_through() {
    let m = Mods { cmd: true, shift: true, ..Default::default() };
    assert_eq!(resolve("j", m, false), Some(Action::JumpLatestNotification));
}

#[test]
fn question_mark_is_suppressed_while_typing() {
    assert_eq!(resolve("?", Mods::default(), false), Some(Action::OpenShortcuts));
    assert_eq!(resolve("?", Mods::default(), true), None, "typing ? into the address bar");
}

#[test]
fn unbound_keys_resolve_to_nothing() {
    assert_eq!(resolve("q", Mods { cmd: true, ..Default::default() }, false), None);
    assert_eq!(resolve("a", Mods::default(), false), None);
}

// ---- app behaviour -------------------------------------------------------

#[test]
fn seeded_state_matches_the_canvas() {
    let app = App::new();
    assert_eq!(app.workspaces.spaces.len(), 3);
    assert_eq!(app.workspaces.current().name, "Personal");
    assert_eq!(app.workspaces.current().tabs.len(), 3);
    assert_eq!(app.workspaces.current().active_tab().title, "Keyboard-first browsing");
    assert_eq!(app.workspaces.spaces[1].count_label(), "4 tabs");
}

#[test]
fn a_workspace_never_empties() {
    let mut app = App::new();
    let ws = app.workspaces.current;
    while app.workspaces.spaces[ws].tabs.len() > 1 {
        let id = app.workspaces.current().active;
        app.apply(Action::CloseTab(id));
    }
    let last = app.workspaces.current().active;
    assert!(app.apply(Action::CloseTab(last)).is_empty(), "the final tab is not closable");
    assert_eq!(app.workspaces.current().tabs.len(), 1);
}

#[test]
fn closing_the_active_tab_focuses_its_left_neighbour() {
    let mut app = App::new();
    let ids: Vec<TabId> = app.workspaces.current().tabs.iter().map(|t| t.id).collect();
    app.apply(Action::SelectTab(ids[1]));
    app.apply(Action::CloseActiveTab);
    assert_eq!(app.workspaces.current().active, ids[0]);
}

#[test]
fn closing_a_tab_destroys_its_web_view() {
    let mut app = App::new();
    let id = app.workspaces.current().active;
    assert!(app.apply(Action::CloseActiveTab).contains(&Effect::Destroy(id)));
}

#[test]
fn selecting_a_hibernated_tab_restores_it() {
    let mut app = App::new();
    let ids: Vec<TabId> = app.workspaces.current().tabs.iter().map(|t| t.id).collect();
    app.workspaces.set_state(ids[2], TabState::Hibernated);
    assert!(app.apply(Action::SelectTab(ids[2])).contains(&Effect::Restore(ids[2])));
}

#[test]
fn a_restored_tab_is_never_evicted_by_the_same_pass() {
    let mut app = App::new();
    // One live slot only: selecting a cold tab must not immediately shed it.
    app.budget = budget(100, 1, 1);
    let ids: Vec<TabId> = app.workspaces.current().tabs.iter().map(|t| t.id).collect();
    for id in &ids {
        app.workspaces.set_state(*id, TabState::Live);
    }
    let fx = app.apply(Action::SelectTab(ids[0]));
    assert!(!fx.contains(&Effect::Hibernate(ids[0])));
    assert!(fx.iter().any(|e| matches!(e, Effect::Hibernate(_))), "others still shed");
}

#[test]
fn critical_pressure_sheds_immediately() {
    let mut app = App::new();
    for id in app.workspaces.current().tabs.iter().map(|t| t.id).collect::<Vec<_>>() {
        app.workspaces.set_state(id, TabState::Live);
    }
    let fx = app.set_pressure(Pressure::Critical);
    assert_eq!(fx.iter().filter(|e| matches!(e, Effect::Hibernate(_))).count(), 2);
}

#[test]
fn switching_workspace_slides_in_the_travel_direction() {
    let mut app = App::new();
    assert!(app.apply(Action::SwitchWorkspace(2)).contains(&Effect::SlideWorkspace(1)));
    assert!(app.apply(Action::SwitchWorkspace(0)).contains(&Effect::SlideWorkspace(-1)));
}

#[test]
fn switching_to_the_current_workspace_is_a_no_op() {
    let mut app = App::new();
    assert!(app.apply(Action::SwitchWorkspace(0)).is_empty());
}

#[test]
fn budget_is_global_across_workspaces() {
    // Background workspaces hold memory too; a per-workspace budget would let
    // three "reasonable" live sets blow the machine together.
    let mut app = App::new();
    app.budget = budget(600, 2, 1);
    for w in &mut app.workspaces.spaces {
        for t in &mut w.tabs {
            t.state = TabState::Live;
            t.footprint = Some(100 * MB);
        }
    }
    let fx = app.reconcile();
    let live_after = app.memory().live;
    assert!(!fx.is_empty());
    assert_eq!(live_after, 2, "9 live tabs across 3 workspaces shed to the ceiling");
}

#[test]
fn address_commit_normalizes_and_navigates() {
    let mut app = App::new();
    app.set_address_draft("docs.internal/spec");
    let fx = app.commit_address();
    assert_eq!(fx.len(), 1);
    let Effect::Load(_, url) = &fx[0] else { panic!("expected a load") };
    assert_eq!(url, "https://docs.internal/spec");
    assert_eq!(app.workspaces.current().active_tab().host, "docs.internal/spec");
    assert_eq!(app.workspaces.current().active_tab().title, "Docs");
}

#[test]
fn address_field_mirrors_the_active_tab_until_edited() {
    let mut app = App::new();
    assert_eq!(app.address_text(), "developer.mozilla.org/en-US/docs/Web/API/KeyboardEvent");
    app.set_address_draft("part");
    assert_eq!(app.address_text(), "part");
    app.apply(Action::NewTab);
    assert_eq!(app.address_text(), "compact.app/new", "a new tab reclaims the field");
}

#[test]
fn empty_address_commit_does_nothing() {
    let mut app = App::new();
    app.set_address_draft("   ");
    assert!(app.commit_address().is_empty());
}

#[test]
fn palette_filters_and_runs() {
    let mut app = App::new();
    app.apply(Action::OpenPalette);
    assert_eq!(app.overlay, Overlay::Palette);
    app.set_query("workspace");
    assert_eq!(app.commands().len(), 1);
    app.run_selected();
    assert_eq!(app.overlay, Overlay::Switcher, "running a command closes the palette first");
}

#[test]
fn palette_selection_is_clamped() {
    let mut app = App::new();
    app.apply(Action::OpenPalette);
    app.move_selection(-5);
    assert_eq!(app.selection, 0);
    app.move_selection(500);
    assert_eq!(app.selection, app.commands().len() - 1);
}

#[test]
fn palette_labels_track_state() {
    let mut app = App::new();
    assert!(app.commands().iter().any(|c| c.label == "Switch to horizontal tabs"));
    app.apply(Action::ToggleLayout);
    assert!(app.commands().iter().any(|c| c.label == "Switch to vertical tabs"));
    app.apply(Action::TogglePin);
    assert!(app.commands().iter().any(|c| c.label == "Unpin chrome"));
}

#[test]
fn escape_closes_any_overlay() {
    let mut app = App::new();
    for open in [Action::OpenPalette, Action::OpenSettings, Action::OpenShortcuts, Action::OpenSwitcher] {
        app.apply(open);
        assert_ne!(app.overlay, Overlay::None);
        app.apply(Action::Dismiss);
        assert_eq!(app.overlay, Overlay::None);
    }
}

#[test]
fn the_switcher_binding_toggles() {
    let mut app = App::new();
    app.apply(Action::OpenSwitcher);
    assert_eq!(app.overlay, Overlay::Switcher);
    app.apply(Action::OpenSwitcher);
    assert_eq!(app.overlay, Overlay::None);
}

#[test]
fn focusing_the_address_bar_reveals_the_chrome_first() {
    let mut app = App::new();
    let fx = app.apply(Action::FocusAddress);
    assert!(app.visibility().address, "focus must not land on a hidden field");
    assert!(fx.contains(&Effect::FocusAddressField));
}

#[test]
fn the_hint_retires_on_first_interaction() {
    let mut app = App::new();
    assert!(app.hint_showing());
    app.on_cursor(Some((500.0, 10.0)), (1000.0, 700.0));
    assert!(!app.hint_showing());
}

// ---- notifications -------------------------------------------------------

#[test]
fn jumping_to_a_notification_crosses_workspaces_and_focuses_the_tab() {
    let mut app = App::new();
    let target = app.workspaces.spaces[1].tabs[2].id;
    app.notifications.push(1, "Release checklist updated", "docs.internal", target, 0);
    app.apply(Action::JumpLatestNotification);
    assert_eq!(app.workspaces.current, 1);
    assert_eq!(app.workspaces.current().active, target);
    assert!(app.notifications.is_empty(), "acting on a toast consumes it");
}

#[test]
fn jump_falls_back_to_unread_when_toasts_have_expired() {
    let mut app = App::new();
    app.workspaces.add_unread(2);
    app.apply(Action::JumpLatestNotification);
    assert_eq!(app.workspaces.current, 2);
    assert_eq!(app.workspaces.spaces[2].unread, 0, "arriving clears the badge");
}

#[test]
fn jump_with_nothing_pending_does_nothing() {
    let mut app = App::new();
    assert!(app.apply(Action::JumpLatestNotification).is_empty());
    assert_eq!(app.workspaces.current, 0);
}

#[test]
fn toasts_expire_and_cap_at_three() {
    let mut app = App::new();
    let tab = app.workspaces.spaces[1].tabs[0].id;
    for i in 0..5 {
        app.notifications.push(1, format!("event {i}"), "board.internal", tab, 0);
    }
    assert_eq!(app.notifications.items().len(), 3, "newest three survive");
    assert_eq!(app.notifications.items()[0].text, "event 2");

    assert!(app.notifications.expire(8_000));
    assert!(app.notifications.is_empty());
}

#[test]
fn toast_labels_read_correctly() {
    let mut app = App::new();
    let tab = app.workspaces.spaces[1].tabs[0].id;
    app.notifications.push(1, "x", "board.internal", tab, 1_000);
    let n = &app.notifications.items()[0];
    assert_eq!(n.age_label(13_000), "12s");
    assert_eq!(n.age_label(121_000), "2m");
    assert_eq!(n.action_label(0), "⌘⇧J JUMP");
    assert_eq!(n.action_label(1), "OPEN");
}

#[test]
fn arriving_in_a_workspace_clears_its_toasts() {
    let mut app = App::new();
    let tab = app.workspaces.spaces[1].tabs[0].id;
    app.notifications.push(1, "x", "board.internal", tab, 0);
    app.apply(Action::SwitchWorkspace(1));
    assert!(app.notifications.is_empty());
}

#[test]
fn elsewhere_lists_only_other_workspaces_with_unread() {
    let mut app = App::new();
    app.workspaces.add_unread(0);
    app.workspaces.add_unread(2);
    let e = app.workspaces.elsewhere();
    assert_eq!(e.len(), 1);
    assert_eq!(e[0].0, 2, "the current workspace is never 'elsewhere'");
}

#[test]
fn memory_report_labels_live_bytes_only() {
    let mut app = App::new();
    let ids: Vec<TabId> = app.workspaces.current().tabs.iter().map(|t| t.id).collect();
    app.workspaces.set_state(ids[0], TabState::Live);
    app.record_footprint(ids[0], 412 * MB);
    app.workspaces.set_state(ids[1], TabState::Hibernated);
    let m = app.memory();
    assert_eq!(m.live, 1);
    assert_eq!(m.hibernated, 1);
    // Estimates are marked as such; only a real measurement gets a bare number.
    assert_eq!(m.label(), "~412 MB");
    assert_eq!(app.memory_label(), "~412 MB");

    app.measured_footprint = Some(371 * MB);
    assert_eq!(app.memory_label(), "371 MB", "a measurement wins over the estimate");
}

use crate::chrome::{ChromeConfig, ChromeVisibility, Reveal, TabLayout};
use crate::command::{resolve, Action, Mods};
use crate::policy::{plan_eviction, prewarm_candidates, LiveBudget, Pressure};
use crate::tab::{TabId, TabMeta, TabState, ESTIMATED_LIVE_BYTES};
use crate::notify::Kind;
use crate::{App, Effect, Overlay};

/// Activity toasts only — a workspace switch always leaves a status
/// confirmation behind, which is not what these assertions are about.
fn activity(app: &App) -> Vec<&crate::notify::Notification> {
    app.notifications
        .items()
        .iter()
        .filter(|n| n.kind == Kind::Activity)
        .collect()
}

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
fn each_surface_answers_to_one_thing() {
    let cfg = ChromeConfig::default();
    // Left edge is the sidebar's business and nothing else's.
    let v = ChromeVisibility::derive(Reveal { top: false, side: true }, &cfg, false);
    assert!(v.sidebar);
    assert!(!v.address, "the sidebar must not drag the address bar out with it");

    // Top edge is the strip's business. It must not summon the address bar,
    // and in vertical layout it must not move the sidebar either.
    let v = ChromeVisibility::derive(Reveal { top: true, side: false }, &cfg, false);
    assert!(!v.sidebar, "reaching for the top edge should leave the tabs alone");
    assert!(!v.address);
}

#[test]
fn the_address_bar_never_appears_on_hover() {
    // It floats over the page, so on most sites it lands on their own header.
    // Tolerable when summoned; not when the pointer merely drifted upward.
    let cfg = ChromeConfig::default();
    for reveal in [
        Reveal { top: true, side: false },
        Reveal { top: false, side: true },
        Reveal { top: true, side: true },
    ] {
        assert!(!ChromeVisibility::derive(reveal, &cfg, false).address);
    }
    assert!(ChromeVisibility::derive(Reveal::default(), &cfg, true).address);
}

#[test]
fn the_strip_is_independent_of_the_address_bar() {
    // Horizontal tabs must still come out on hover even though the address bar
    // no longer does.
    let cfg = ChromeConfig { layout: TabLayout::Horizontal, ..Default::default() };
    let v = ChromeVisibility::derive(Reveal { top: true, side: false }, &cfg, false);
    assert!(v.strip);
    assert!(!v.address);
    assert!(!v.sidebar, "horizontal layout has no sidebar");
}

#[test]
fn pinning_shows_chrome_with_no_cursor_nearby() {
    let cfg = ChromeConfig { pinned: true, ..Default::default() };
    let v = ChromeVisibility::derive(Reveal::default(), &cfg, false);
    assert!(v.address && v.sidebar);
}

#[test]
fn the_address_bar_clears_the_strip_when_both_are_out() {
    let cfg = ChromeConfig { layout: TabLayout::Horizontal, ..Default::default() };
    let v = ChromeVisibility::derive(Reveal { top: true, side: false }, &cfg, true);
    assert!(v.strip && v.address);
    assert_eq!(v.address_top(), 58.0);
    let alone = ChromeVisibility::derive(Reveal::default(), &cfg, true);
    assert_eq!(alone.address_top(), 14.0, "nothing above it to clear");
}

#[test]
fn summoning_the_address_bar_does_not_move_other_chrome() {
    let mut app = App::new();
    app.apply(Action::FocusAddress);
    let v = app.visibility();
    assert!(v.address);
    assert!(!v.sidebar, "⌘L is about the address bar, not the tabs");
}

#[test]
fn the_address_bar_leaves_when_it_is_done() {
    // It covers the site's own header, so it must not outlive its purpose.
    let mut app = App::new();
    app.apply(Action::FocusAddress);
    assert!(app.visibility().address);
    app.navigate("example.com");
    assert!(!app.visibility().address, "navigating puts it away");

    app.apply(Action::FocusAddress);
    app.apply(Action::Dismiss);
    assert!(!app.visibility().address, "Escape puts it away");

    app.apply(Action::FocusAddress);
    assert!(app.dismiss_address(), "clicking away puts it away");
    assert!(!app.visibility().address);
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

// ---- address parsing -----------------------------------------------------

#[test]
fn bare_hosts_become_https_urls() {
    for (input, url, display) in [
        ("example.com", "https://example.com", "example.com"),
        ("docs.internal/spec", "https://docs.internal/spec", "docs.internal/spec"),
        ("https://a.test/x?y=1", "https://a.test/x?y=1", "a.test/x?y=1"),
        ("http://a.test", "http://a.test", "a.test"),
        ("localhost:8080", "https://localhost:8080", "localhost:8080"),
    ] {
        let t = crate::parse_address(input).expect(input);
        assert_eq!(t.url, url, "input {input}");
        assert_eq!(t.display, display, "input {input}");
    }
}

#[test]
fn plain_words_become_a_search() {
    // A single bar has to serve both, and "rust ownership" is unambiguously
    // not an address.
    let t = crate::parse_address("rust ownership").unwrap();
    assert_eq!(t.url, "https://duckduckgo.com/?q=rust+ownership");
    assert_eq!(t.display, "rust ownership", "the bar keeps what was typed");
}

#[test]
fn a_dotless_word_is_a_search_not_a_host() {
    let t = crate::parse_address("bookmarks").unwrap();
    assert!(t.url.starts_with("https://duckduckgo.com/?q="));
}

#[test]
fn search_queries_are_percent_encoded() {
    let t = crate::parse_address("c++ & rust").unwrap();
    assert_eq!(t.url, "https://duckduckgo.com/?q=c%2B%2B+%26+rust");
}

#[test]
fn a_url_with_spaces_is_treated_as_a_search() {
    // Whitespace rules out an address even when a dot is present.
    let t = crate::parse_address("what is example.com").unwrap();
    assert!(t.url.contains("duckduckgo.com"));
}

#[test]
fn malformed_hosts_are_searched_rather_than_loaded() {
    for input in [".com", "example."] {
        let t = crate::parse_address(input).unwrap();
        assert!(t.url.contains("duckduckgo.com"), "input {input}");
    }
}

#[test]
fn titles_drop_www_and_the_tld() {
    assert_eq!(crate::parse_address("www.github.com").unwrap().title, "Github");
    assert_eq!(crate::parse_address("rust-lang.org").unwrap().title, "Rust-lang");
}

#[test]
fn blank_input_goes_nowhere() {
    assert!(crate::parse_address("   ").is_none());
    assert!(crate::parse_address("").is_none());
}

#[test]
fn navigating_updates_the_tab_and_clears_the_draft() {
    let mut app = App::new();
    app.set_address_draft("half-typed");
    let fx = app.navigate("rust-lang.org");
    assert!(app.address_draft.is_none(), "the field returns to mirroring the tab");
    assert_eq!(app.workspaces.current().active_tab().host, "rust-lang.org");
    assert_eq!(app.address_text(), "rust-lang.org");
    let Some(Effect::Load(_, url)) = fx.first() else { panic!("expected a load") };
    assert_eq!(url, "https://rust-lang.org");
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
    assert!(activity(&app).is_empty(), "acting on a toast consumes it");
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
fn opening_a_toast_from_another_workspace_travels_to_it() {
    let mut app = App::new();
    let target = app.workspaces.spaces[2].tabs[1].id;
    let id = app.notifications.push(2, "Benchmark run finished", "bench.dev", target, 0);
    let fx = app.open_notification(id);
    assert_eq!(app.workspaces.current, 2);
    assert_eq!(app.workspaces.current().active, target);
    assert!(fx.contains(&Effect::SlideWorkspace(1)), "a cross-workspace open slides");
    assert!(activity(&app).is_empty());
}

#[test]
fn opening_a_toast_from_this_workspace_just_focuses_the_tab() {
    let mut app = App::new();
    let target = app.workspaces.current().tabs[2].id;
    let id = app.notifications.push(0, "Reading list synced", "read.later", target, 0);
    let fx = app.open_notification(id);
    assert_eq!(app.workspaces.current, 0, "no travel needed");
    assert_eq!(app.workspaces.current().active, target);
    assert!(!fx.iter().any(|e| matches!(e, Effect::SlideWorkspace(_))));
}

#[test]
fn opening_an_unknown_toast_is_harmless() {
    let mut app = App::new();
    assert!(app.open_notification(999).is_empty());
}

#[test]
fn other_workspaces_active_tabs_are_prewarmed() {
    // A workspace switch lands on a whole different page, so arriving at an
    // unpainted one is the most visible stall in the product.
    let mut app = App::new();
    let fx = app.reconcile();
    for (i, ws) in app.workspaces.spaces.iter().enumerate() {
        if i == app.workspaces.current {
            continue;
        }
        let active = ws.active;
        assert!(
            fx.iter().any(|e| matches!(e, Effect::Load(id, _) | Effect::Restore(id) if *id == active)),
            "workspace {i}'s active tab should be prewarmed"
        );
    }
}

#[test]
fn prewarming_stops_at_the_budget() {
    let mut app = App::new();
    // Room for the active tab and nothing else.
    app.budget = budget(100, 1, 1);
    let active = app.workspaces.current().active;
    app.workspaces.set_state(active, TabState::Live);
    let fx = app.reconcile();
    assert!(
        !fx.iter().any(|e| matches!(e, Effect::Load(..) | Effect::Restore(..))),
        "no headroom means no prewarming, however useful it would be"
    );
}

#[test]
fn arriving_in_a_workspace_clears_its_toasts() {
    let mut app = App::new();
    let tab = app.workspaces.spaces[1].tabs[0].id;
    app.notifications.push(1, "x", "board.internal", tab, 0);
    app.apply(Action::SwitchWorkspace(1));
    assert!(activity(&app).is_empty());
}

#[test]
fn arriving_in_a_workspace_confirms_where_you_are() {
    // After ⌘2 the workspace name may be nowhere on screen — the chrome that
    // carries it is hidden by default.
    let mut app = App::new();
    app.apply(Action::SwitchWorkspace(1));
    let items = app.notifications.items();
    assert_eq!(items.len(), 1);
    let n = &items[0];
    assert_eq!(n.kind, Kind::Status);
    assert_eq!(n.workspace, 1);
    assert_eq!(n.text, "Sprint board", "names the tab you landed on");
    assert_eq!(n.source, "4 tabs");
    assert_eq!(n.action_label(1), "⌘2", "advertises the binding");
}

#[test]
fn only_one_arrival_toast_survives_rapid_switching() {
    // Holding ⌘1/⌘2/⌘3 would otherwise leave three confirmations up, two of
    // which are already wrong.
    let mut app = App::new();
    app.apply(Action::SwitchWorkspace(1));
    app.apply(Action::SwitchWorkspace(2));
    app.apply(Action::SwitchWorkspace(0));
    let items = app.notifications.items();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].workspace, 0);
}

#[test]
fn an_arrival_toast_expires_sooner_than_activity() {
    let mut app = App::new();
    app.apply(Action::SwitchWorkspace(1));
    assert!(!app.notifications.expire(2_000), "still up at 2s");
    assert!(app.notifications.expire(3_000), "gone by 3s");
}

#[test]
fn an_arrival_toast_is_not_a_travel_target() {
    // The user is already there; ⌘⇧J must not treat it as somewhere to go.
    let mut app = App::new();
    app.apply(Action::SwitchWorkspace(1));
    let id = app.notifications.items()[0].id;
    assert!(app.notifications.latest_elsewhere(1).is_none());
    assert!(app.open_notification(id).is_empty(), "clicking it only dismisses");
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

// ---- bookmarks -----------------------------------------------------------

#[test]
fn the_star_reflects_the_active_tab() {
    let mut app = App::new();
    assert!(!app.is_bookmarked(), "the seeded active tab is not saved");
    assert_eq!(app.star_glyph(), "☆");
    app.apply(Action::ToggleBookmark);
    assert!(app.is_bookmarked());
    assert_eq!(app.star_glyph(), "★");
    app.apply(Action::ToggleBookmark);
    assert!(!app.is_bookmarked(), "⌘D toggles both ways");
}

#[test]
fn bookmarks_are_keyed_by_host_not_tab() {
    // Bookmarking is a statement about a place; the tab showing it may be
    // closed, or the same site may be open twice.
    let mut app = App::new();
    app.apply(Action::ToggleBookmark);
    let host = app.workspaces.current().active_tab().host.clone();
    let other = app.apply(Action::NewTab);
    assert!(!other.is_empty());
    app.set_address_draft(&host);
    app.commit_address();
    assert!(app.is_bookmarked(), "a different tab on the same host reads as saved");
}

#[test]
fn a_new_bookmark_takes_its_workspace_tint() {
    let mut app = App::new();
    app.apply(Action::SwitchWorkspace(1));
    // Not the workspace's first tab — that host ships as a seeded bookmark, so
    // toggling it would remove rather than add.
    let unsaved = app.workspaces.current().tabs[1].id;
    app.apply(Action::SelectTab(unsaved));
    assert!(!app.is_bookmarked());
    app.apply(Action::ToggleBookmark);
    let saved = app.bookmarks.items().last().unwrap();
    assert_eq!(saved.tint, crate::theme::WORKSPACE_DOTS[1], "tinted by where it was saved");
}

#[test]
fn opening_a_bookmark_navigates_the_active_tab() {
    let mut app = App::new();
    let b = app.bookmarks.items()[1].id;
    let host = app.bookmarks.get(b).unwrap().host.clone();
    let fx = app.apply(Action::OpenBookmark(b));
    assert_eq!(app.workspaces.current().active_tab().host, host);
    let Some(Effect::Load(_, url)) = fx.first() else { panic!("expected a load") };
    assert_eq!(url, &format!("https://{host}"));
}

#[test]
fn removing_a_bookmark_forgets_it() {
    let mut app = App::new();
    let before = app.bookmarks.items().len();
    let b = app.bookmarks.items()[0].id;
    app.apply(Action::RemoveBookmark(b));
    assert_eq!(app.bookmarks.items().len(), before - 1);
    assert!(app.bookmarks.get(b).is_none());
}

#[test]
fn cmd_d_is_bound_and_the_palette_tracks_it() {
    assert_eq!(
        resolve("d", Mods { cmd: true, ..Default::default() }, false),
        Some(Action::ToggleBookmark)
    );
    let mut app = App::new();
    assert!(app.commands().iter().any(|c| c.label == "Bookmark this tab"));
    app.apply(Action::ToggleBookmark);
    assert!(app.commands().iter().any(|c| c.label == "Remove bookmark"));
}

#[test]
fn a_bookmark_initial_is_its_first_letter() {
    let app = App::new();
    assert_eq!(app.bookmarks.items()[1].initial(), "S");
}

// ---- load progress -------------------------------------------------------

#[test]
fn progress_is_reported_only_while_loading() {
    let mut app = App::new();
    let id = app.workspaces.current().active;
    assert_eq!(app.load_progress(), None, "an idle tab shows no bar");
    app.record_progress(id, true, 0.4);
    assert_eq!(app.load_progress(), Some(0.4));
}

#[test]
fn a_starting_navigation_is_visible_immediately() {
    // WebKit reports 0 for the first moments of a load, and a bar of zero
    // width is indistinguishable from no bar — clicking a link would look
    // like nothing happened.
    let mut app = App::new();
    let id = app.workspaces.current().active;
    app.record_progress(id, true, 0.0);
    assert_eq!(app.load_progress(), Some(crate::MIN_VISIBLE_PROGRESS));
}

#[test]
fn the_bar_holds_at_full_before_it_leaves() {
    // Most navigations finish faster than the eye resolves; without the hold
    // the bar appears and disappears as a flicker.
    let mut app = App::new();
    let id = app.workspaces.current().active;
    app.clock = 10_000;
    app.record_progress(id, true, 0.6);
    app.record_progress(id, false, 1.0);
    assert_eq!(app.load_progress(), Some(1.0), "completes rather than vanishing");

    app.clock = 10_000 + crate::LOAD_FINISH_HOLD_MS - 1;
    assert_eq!(app.load_progress(), Some(1.0), "still held");
    app.clock = 10_000 + crate::LOAD_FINISH_HOLD_MS;
    assert_eq!(app.load_progress(), None, "gone once the hold elapses");
}

#[test]
fn an_idle_tab_stops_asking_for_redraws() {
    // The load poll runs at 12 Hz. If a settled tab keeps reporting a change,
    // the whole chrome redraws forever — the opposite of the point.
    let mut app = App::new();
    let id = app.workspaces.current().active;
    app.clock = 1_000;
    app.record_progress(id, true, 0.5);
    app.record_progress(id, false, 1.0);

    app.clock = 1_000 + crate::LOAD_FINISH_HOLD_MS;
    assert!(app.record_progress(id, false, 1.0), "one last redraw to fade out");
    for _ in 0..5 {
        app.clock += 80;
        assert!(
            !app.record_progress(id, false, 1.0),
            "a settled tab must report no change"
        );
    }
}

#[test]
fn a_new_navigation_cancels_a_pending_hold() {
    // Clicking a second link during the hold must restart the bar, not leave
    // it sitting at full.
    let mut app = App::new();
    let id = app.workspaces.current().active;
    app.clock = 5_000;
    app.record_progress(id, true, 0.9);
    app.record_progress(id, false, 1.0);
    app.record_progress(id, true, 0.1);
    assert_eq!(app.load_progress(), Some(0.1));
    app.clock = 5_000 + crate::LOAD_FINISH_HOLD_MS * 2;
    assert_eq!(app.load_progress(), Some(0.1), "the stale hold does not expire it");
}

#[test]
fn progress_updates_are_debounced() {
    // The ticker polls constantly; only meaningful movement should redraw.
    let mut app = App::new();
    let id = app.workspaces.current().active;
    assert!(app.record_progress(id, true, 0.40));
    assert!(!app.record_progress(id, true, 0.401), "sub-1% is noise");
    assert!(app.record_progress(id, true, 0.60));
}

#[test]
fn only_the_visible_tab_drives_the_bar() {
    let mut app = App::new();
    let ids: Vec<TabId> = app.workspaces.current().tabs.iter().map(|t| t.id).collect();
    app.record_progress(ids[0], true, 0.5);
    app.apply(Action::SelectTab(ids[2]));
    assert_eq!(app.load_progress(), None, "a background load is not this tab's bar");
}

// ---- chrome feedback -----------------------------------------------------

#[test]
fn toggling_layout_reveals_what_it_rearranged() {
    // Silently swapping hidden chrome gives no sign anything happened.
    let mut app = App::new();
    assert!(!app.visibility().any());
    app.apply(Action::ToggleLayout);
    assert!(app.visibility().any());
}

#[test]
fn the_hint_yields_to_a_toast() {
    let mut app = App::new();
    assert!(app.hint_showing());
    let tab = app.workspaces.spaces[1].tabs[0].id;
    app.notifications.push(1, "x", "board.internal", tab, 0);
    assert!(!app.hint_showing(), "both live in the same corner of attention");
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

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

/// The default keymap, which is what these bindings are documented against.
fn km() -> crate::settings::Keymap {
    crate::settings::Keymap::default()
}

#[test]
fn resolves_the_documented_bindings() {
    let cmd = Mods { cmd: true, ..Default::default() };
    assert_eq!(resolve(&km(), "k", cmd, false), Some(Action::OpenPalette));
    assert_eq!(resolve(&km(), "K", cmd, false), Some(Action::OpenPalette), "case-insensitive");
    assert_eq!(resolve(&km(), "t", cmd, false), Some(Action::NewTab));
    assert_eq!(resolve(&km(), ",", cmd, false), Some(Action::OpenSettings));
    let alt = Mods { alt: true, ..Default::default() };
    assert_eq!(resolve(&km(), " ", alt, false), Some(Action::OpenSwitcher));
    assert_eq!(resolve(&km(), "2", cmd, false), Some(Action::SwitchWorkspace(1)));
    assert_eq!(resolve(&km(), "Escape", Mods::default(), false), Some(Action::Dismiss));
}

#[test]
fn shift_j_jumps_rather_than_falling_through() {
    let m = Mods { cmd: true, shift: true, ..Default::default() };
    assert_eq!(resolve(&km(), "j", m, false), Some(Action::JumpLatestNotification));
}

#[test]
fn question_mark_is_suppressed_while_typing() {
    // `?` arrives as shift-slash, which is how the binding is stored.
    let q = Mods { shift: true, ..Default::default() };
    assert_eq!(resolve(&km(), "?", q, false), Some(Action::OpenShortcuts));
    assert_eq!(resolve(&km(), "?", q, true), None, "typing ? into the address bar");
}

#[test]
fn unbound_keys_resolve_to_nothing() {
    assert_eq!(resolve(&km(), "q", Mods { cmd: true, ..Default::default() }, false), None);
    assert_eq!(resolve(&km(), "a", Mods::default(), false), None);
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
fn bare_hosts_become_https_urls_and_loopback_stays_http() {
    for (input, url, display) in [
        ("example.com", "https://example.com", "example.com"),
        ("docs.internal/spec", "https://docs.internal/spec", "docs.internal/spec"),
        ("https://a.test/x?y=1", "https://a.test/x?y=1", "a.test/x?y=1"),
        ("http://a.test", "http://a.test", "a.test"),
        ("localhost:8080", "http://localhost:8080", "localhost:8080"),
        ("127.0.0.1:3000/app", "http://127.0.0.1:3000/app", "127.0.0.1:3000/app"),
        ("app.localhost:5173", "http://app.localhost:5173", "app.localhost:5173"),
        ("https://localhost:8443", "https://localhost:8443", "localhost:8443"),
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
    assert_eq!(app.address_text(), crate::page::NEW_TAB_URL, "a new tab reclaims the field");
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
        resolve(&km(), "d", Mods { cmd: true, ..Default::default() }, false),
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
    let vis = app.visibility();
    assert!(vis.strip, "horizontal shows the strip");
    // And leaves no latch behind for the edge the new layout does not use.
    // A stale `side` here would have the sidebar reappear the moment the
    // cursor grazed the left edge of a layout that has no sidebar.
    assert!(!app.reveal.side);

    app.apply(Action::ToggleLayout);
    assert!(app.visibility().sidebar, "vertical shows the sidebar");
    assert!(!app.reveal.top, "and no stale top latch behind it");
}

#[test]
fn the_layout_toggle_survives_the_mouse_move_that_follows_it() {
    // Clicking LAYOUT in the strip leaves the cursor at the top right — the
    // far side of the window from the sidebar it just revealed. Before the
    // hold, the next mouse move retracted that sidebar within a frame, the
    // toggle read as a no-op, and clicking again put the layout back.
    let mut app = App::new();
    let size = (1480.0, 940.0);
    app.chrome.layout = crate::chrome::TabLayout::Horizontal;

    app.apply(Action::ToggleLayout);
    assert!(app.visibility().sidebar);

    // Where the cursor actually is after that click: top right.
    app.on_cursor(Some((1380.0, 20.0)), size);
    assert!(app.visibility().sidebar, "the confirmation is still on screen");

    // And hover resumes once the hold is over.
    app.clock += crate::chrome::REVEAL_HOLD_MS;
    app.on_cursor(Some((1380.0, 500.0)), size);
    assert!(!app.visibility().sidebar);
}

#[test]
fn hovering_the_top_never_summons_a_strip_the_layout_does_not_have() {
    // The report was "hovering near the top switches me to horizontal tabs".
    // Whatever else is going on, reveal alone must never do it: the top edge
    // answers for the strip, and the strip only exists in horizontal.
    let mut app = App::new();
    let size = (1480.0, 940.0);
    for y in [900.0, 400.0, 200.0, 60.0, 4.0] {
        app.on_cursor(Some((740.0, y)), size);
        let vis = app.visibility();
        assert!(!vis.strip, "no strip in vertical at y={y}");
        assert!(!vis.sidebar, "and the top edge is not the sidebar's edge");
        assert_eq!(app.chrome.layout, crate::chrome::TabLayout::Vertical);
    }
    // Same along the top-left corner, where both latches are live at once.
    for x in [4.0, 60.0, 200.0, 400.0] {
        app.on_cursor(Some((x, 30.0)), size);
        assert!(!app.visibility().strip, "no strip in vertical at x={x}");
        assert_eq!(app.chrome.layout, crate::chrome::TabLayout::Vertical);
    }
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

// ---- internal pages ------------------------------------------------------

use crate::bridge::{parse, Incoming, PageMessage};
use crate::page::{ErrorKind, PageKind, NEW_TAB_URL};
use crate::settings::{BindingId, Chord, Keymap, NotifMode, Startup};

fn active(app: &App) -> &TabMeta {
    app.workspaces.current().active_tab()
}

#[test]
fn a_new_tab_is_drawn_not_fetched() {
    let mut app = App::new();
    let fx = app.apply(Action::NewTab);
    assert_eq!(active(&app).kind, PageKind::NewTab);
    assert_eq!(active(&app).host, NEW_TAB_URL);
    let id = active(&app).id;
    assert!(
        fx.iter().any(|e| matches!(e, Effect::RenderInternal(t, _) if *t == id)),
        "⌘T renders our own page: {fx:?}"
    );
    assert!(
        !fx.iter().any(|e| matches!(e, Effect::Load(t, _) if *t == id)),
        "and never asks the network for it"
    );
}

#[test]
fn the_settings_page_opens_as_a_tab() {
    let mut app = App::new();
    let before = app.workspaces.current().tabs.len();
    app.apply(Action::OpenSettingsPage);
    assert_eq!(app.workspaces.current().tabs.len(), before + 1);
    assert_eq!(active(&app).kind, PageKind::Settings);
    // The drawer and the page are different surfaces; opening one must not
    // leave the other on top of it.
    assert_eq!(app.overlay, Overlay::None);
}

#[test]
fn navigating_away_from_an_internal_page_makes_it_a_web_tab() {
    let mut app = App::new();
    app.apply(Action::NewTab);
    app.navigate("example.com");
    assert_eq!(active(&app).kind, PageKind::Web);
    assert_eq!(active(&app).error, None);
}

#[test]
fn a_failed_navigation_becomes_an_explained_error() {
    let mut app = App::new();
    let id = active(&app).id;
    app.navigate("nowhere.invalid");
    let fx = app.fail_navigation(id, -1003);
    assert_eq!(active(&app).kind, PageKind::Error);
    assert_eq!(active(&app).error, Some(ErrorKind::Dns));
    assert!(fx.iter().any(|e| matches!(e, Effect::RenderInternal(..))));
    // The failed host is kept: it is what the page reads out and what ⌘R
    // retries.
    assert_eq!(active(&app).host, "nowhere.invalid");
}

#[test]
fn a_cancelled_navigation_is_not_an_error() {
    let mut app = App::new();
    let id = active(&app).id;
    // -999 is NSURLErrorCancelled, which is what a redirect reports. Drawing an
    // error page for it would flash one on every ordinary redirect.
    assert!(app.fail_navigation(id, -999).is_empty());
    assert_eq!(active(&app).kind, PageKind::Web);
}

#[test]
fn retry_reloads_the_url_the_error_page_failed_at() {
    let mut app = App::new();
    let id = active(&app).id;
    app.navigate("example.org");
    app.fail_navigation(id, -1001);
    assert_eq!(active(&app).error, Some(ErrorKind::Timeout));

    let fx = app.apply(Action::Retry);
    assert_eq!(active(&app).kind, PageKind::Web, "no longer an error page");
    assert_eq!(active(&app).error, None);
    assert!(
        fx.iter()
            .any(|e| matches!(e, Effect::Load(t, u) if *t == id && u == "https://example.org")),
        "{fx:?}"
    );
}

#[test]
fn retry_on_a_web_page_reloads_rather_than_reloading_the_address() {
    let mut app = App::new();
    let id = active(&app).id;
    let fx = app.apply(Action::Retry);
    assert_eq!(fx, vec![Effect::Reload(id)]);
}

#[test]
fn an_internal_tab_is_redrawn_rather_than_restored() {
    let mut app = App::new();
    app.apply(Action::NewTab);
    let id = active(&app).id;
    app.workspaces.set_state(id, TabState::Hibernated);
    let fx = app.apply(Action::SelectTab(id));
    assert!(
        fx.iter().any(|e| matches!(e, Effect::RenderInternal(t, _) if *t == id)),
        "a session blob describes navigation history, not a page we generated"
    );
    assert!(!fx.iter().any(|e| matches!(e, Effect::Restore(t) if *t == id)));
}

#[test]
fn history_is_by_place_and_bounded() {
    let mut app = App::new();
    for host in ["a.com", "b.com", "a.com/deep", "c.com", "d.com", "e.com", "f.com", "g.com"] {
        app.navigate(host);
    }
    assert_eq!(app.history.len(), crate::HISTORY_LIMIT);
    let hosts: Vec<&str> = app.history.iter().map(|v| v.host.as_str()).collect();
    assert_eq!(hosts[0], "g.com", "most recent first");
    assert_eq!(
        hosts.iter().filter(|h| **h == "a.com").count(),
        1,
        "a.com/deep refreshed the existing a.com row rather than adding one"
    );
    assert!(!hosts.contains(&"b.com"), "the oldest fell off the end instead");
}

// ---- the page bridge -----------------------------------------------------

#[test]
fn messages_parse_into_the_same_actions_a_key_would_produce() {
    assert_eq!(
        parse("palette"),
        Some(Incoming { scroll: 0, message: PageMessage::Do(Action::OpenPalette) })
    );
    assert_eq!(
        parse("ws|2"),
        Some(Incoming { scroll: 0, message: PageMessage::SwitchWorkspace(2) })
    );
    assert_eq!(
        parse("error|dns"),
        Some(Incoming {
            scroll: 0,
            message: PageMessage::Do(Action::ShowError(ErrorKind::Dns))
        })
    );
}

#[test]
fn a_scroll_prefix_is_split_off_before_the_verb() {
    let m = parse("742\u{1f}block|pinned").expect("parsed");
    assert_eq!(m.scroll, 742);
    assert_eq!(m.message, PageMessage::ToggleBlock("pinned".into()));
}

#[test]
fn a_url_containing_a_pipe_survives_the_trip() {
    let m = parse("0\u{1f}nav|example.com/?a=1|2").expect("parsed");
    assert_eq!(m.message, PageMessage::Navigate("example.com/?a=1|2".into()));
}

#[test]
fn unrecognised_messages_are_dropped() {
    assert_eq!(parse("evaluate|alert(1)"), None);
    assert_eq!(parse(""), None);
    assert_eq!(parse("ws|not-a-number"), None);
    assert_eq!(parse("startup|whenever"), None);
}

#[test]
fn a_toggle_keeps_the_page_where_it_was() {
    let mut app = App::new();
    app.apply(Action::OpenSettingsPage);
    app.on_page_message(Incoming {
        scroll: 640,
        message: PageMessage::ToggleBlock("recents".into()),
    });
    assert!(!app.settings.blocks.recents);
    assert_eq!(active(&app).scroll, 640);
    let Some(Effect::RenderInternal(_, html)) = app.refresh_active_page().into_iter().next() else {
        panic!("the settings page redraws itself");
    };
    assert!(html.contains("window.scrollTo(0, 640)"), "and comes back scrolled");
}

// ---- the keymap ----------------------------------------------------------

#[test]
fn default_bindings_render_the_way_the_design_writes_them() {
    let km = Keymap::default();
    assert_eq!(km.label(BindingId::Palette), "⌘K");
    assert_eq!(km.label(BindingId::Jump), "⌘⇧J");
    assert_eq!(km.label(BindingId::QuickSettings), "⌘,");
    assert_eq!(km.label(BindingId::AllSettings), "⌘⇧,");
    assert_eq!(km.label(BindingId::Switcher), "⌥Space");
    // `?` is a key in its own right, not shift-slash spelled out.
    assert_eq!(km.label(BindingId::Shortcuts), "?");
    assert_eq!(km.label(BindingId::Workspace), "⌘1–3");
}

#[test]
fn shifted_punctuation_separates_the_two_settings_bindings() {
    let km = Keymap::default();
    let cmd = Mods { cmd: true, ..Default::default() };
    let cmd_shift = Mods { cmd: true, shift: true, ..Default::default() };
    assert_eq!(resolve(&km, ",", cmd, false), Some(Action::OpenSettings));
    // AppKit reports shift-comma as `<`; it has to fold back to the same key.
    assert_eq!(resolve(&km, "<", cmd_shift, false), Some(Action::OpenSettingsPage));
}

#[test]
fn rebinding_takes_effect_and_releases_the_old_key() {
    let mut app = App::new();
    let cmd = Mods { cmd: true, ..Default::default() };
    app.settings.recording = Some(BindingId::Palette);
    app.record_chord("p", cmd);

    assert_eq!(app.settings.recording, None, "one chord ends the recording");
    assert_eq!(app.resolve_key("p", cmd, false), Some(Action::OpenPalette));
    assert_eq!(app.resolve_key("k", cmd, false), None, "⌘K is free again");
}

#[test]
fn rebinding_onto_a_taken_chord_steals_it_visibly() {
    let mut km = Keymap::default();
    km.rebind(BindingId::Palette, Chord::new(true, false, false, "t"));
    assert_eq!(km.label(BindingId::Palette), "⌘T");
    assert_eq!(km.label(BindingId::NewTab), "", "displaced, and shown as unbound");
    assert_eq!(km.lookup("t", Mods { cmd: true, ..Default::default() }), Some(BindingId::Palette));
}

#[test]
fn the_workspace_range_cannot_be_recorded_over() {
    let mut km = Keymap::default();
    km.rebind(BindingId::Workspace, Chord::new(true, false, false, "z"));
    assert_eq!(km.label(BindingId::Workspace), "⌘1–3");
    assert_eq!(km.lookup("z", Mods { cmd: true, ..Default::default() }), None);
}

#[test]
fn escape_leaves_record_mode_without_binding_it() {
    let mut app = App::new();
    app.settings.recording = Some(BindingId::NewTab);
    app.record_chord("Escape", Mods::default());
    assert_eq!(app.settings.recording, None);
    assert_eq!(
        app.settings.keymap.label(BindingId::NewTab),
        "⌘T",
        "the binding is untouched"
    );
}

#[test]
fn bare_modifiers_do_not_end_a_recording() {
    let mut app = App::new();
    app.settings.recording = Some(BindingId::NewTab);
    app.record_chord("", Mods { cmd: true, ..Default::default() });
    assert_eq!(app.settings.recording, Some(BindingId::NewTab));
}

#[test]
fn the_shortcuts_overlay_reads_the_live_keymap() {
    let mut km = Keymap::default();
    km.rebind(BindingId::Palette, Chord::new(true, false, false, "p"));
    let rows = crate::command::shortcuts(&km);
    let palette = rows.iter().find(|r| r.label == "Command palette").expect("row");
    assert_eq!(palette.keys, "⌘P");
}

// ---- notification modes --------------------------------------------------

#[test]
fn muting_a_workspace_stops_the_toast_but_not_the_count() {
    let mut app = App::new();
    let tab = app.workspaces.spaces[1].tabs[1].id;
    app.settings.set_notif_mode(1, NotifMode::Muted);
    app.post_activity(1, "Sprint board".into(), "github.com".into(), tab, 0);
    assert!(activity(&app).is_empty(), "muted workspaces do not interrupt");
    assert_eq!(app.workspaces.spaces[1].unread, 1, "but the badge stays honest");
}

#[test]
fn mentions_mode_only_surfaces_the_workspace_foreground_tab() {
    let mut app = App::new();
    app.settings.set_notif_mode(1, NotifMode::Mentions);
    let foreground = app.workspaces.spaces[1].active;
    let background = app.workspaces.spaces[1]
        .tabs
        .iter()
        .map(|t| t.id)
        .find(|id| *id != foreground)
        .expect("more than one tab");

    app.post_activity(1, "Elsewhere".into(), "web.dev".into(), background, 0);
    assert!(activity(&app).is_empty());

    app.post_activity(1, "Sprint board".into(), "github.com".into(), foreground, 0);
    assert_eq!(activity(&app).len(), 1);
    assert_eq!(app.workspaces.spaces[1].unread, 2, "both counted");
}

// ---- markup --------------------------------------------------------------

#[test]
fn error_classification_names_what_actually_happened() {
    assert_eq!(ErrorKind::from_url_error(-1009), Some(ErrorKind::Offline));
    assert_eq!(ErrorKind::from_url_error(-1003), Some(ErrorKind::Dns));
    assert_eq!(ErrorKind::from_url_error(-1001), Some(ErrorKind::Timeout));
    assert_eq!(ErrorKind::from_url_error(-1202), Some(ErrorKind::Cert));
    assert_eq!(ErrorKind::from_url_error(-999), None);
    // An unrecognised failure claims nothing beyond "it did not arrive".
    assert_eq!(ErrorKind::from_url_error(-1234), Some(ErrorKind::NotFound));
}

#[test]
fn every_error_page_renders_its_own_copy() {
    for kind in ErrorKind::ALL {
        let html = crate::html::error(kind, "example.com", 0);
        let spec = kind.spec();
        assert!(html.contains(&crate::html::escape(spec.title)), "{kind}");
        assert!(html.contains(spec.code), "{kind}");
        assert!(html.contains("example.com"), "{kind} names the host that failed");
        for step in spec.steps {
            assert!(html.contains(step.label), "{kind} lists what was checked");
        }
    }
}

#[test]
fn a_hostile_title_cannot_escape_into_the_markup() {
    let mut app = App::new();
    app.navigate("example.com");
    let id = active(&app).id;
    app.workspaces.set_location(
        id,
        "</span><script>fetch('//evil')</script>",
        "example.com",
        "https://example.com",
    );
    // `example.com` ships bookmarked, so one toggle would remove it.
    app.apply(Action::ToggleBookmark);
    app.apply(Action::ToggleBookmark);
    let html = crate::html::new_tab(&app, 0);
    assert!(!html.contains("<script>fetch"), "page titles are data, not markup");
    assert!(html.contains("&lt;/span&gt;&lt;script&gt;"));
}

#[test]
fn turning_a_block_off_removes_it_from_the_page() {
    let mut app = App::new();
    assert!(crate::html::new_tab(&app, 0).contains(">Pinned<"));
    app.settings.blocks.pinned = false;
    let html = crate::html::new_tab(&app, 0);
    assert!(!html.contains(">Pinned<"));
    assert!(html.contains(">Workspaces<"), "the others are untouched");
}

#[test]
fn the_settings_page_lists_every_binding_and_error() {
    let mut app = App::new();
    app.settings.startup = Startup::RestoreSession;
    let html = crate::html::settings(&app, 0);
    for b in BindingId::ALL {
        assert!(html.contains(b.label()), "{b:?} is listed");
    }
    for k in ErrorKind::ALL {
        assert!(
            html.contains(&crate::html::escape(k.spec().name)),
            "{k} is previewable"
        );
    }
    for w in &app.workspaces.spaces {
        assert!(html.contains(&w.name), "every workspace gets a notification rule");
    }
}

// ---- persistence ---------------------------------------------------------

use crate::persist::{self, LoadError, FORMAT_VERSION};

/// Everything a restart is supposed to carry, as one comparable value.
fn durable(app: &App) -> Vec<String> {
    let mut v = vec![
        format!("current {}", app.workspaces.current),
        format!("chrome {:?}", app.chrome),
        format!("startup {:?} blocks {:?}", app.settings.startup, app.settings.blocks),
        format!("modes {:?}", app.settings.notif_modes),
    ];
    for b in BindingId::ALL {
        v.push(format!("bind {b:?} {:?}", app.settings.keymap.chord(b)));
    }
    for w in &app.workspaces.spaces {
        v.push(format!("ws {} active {}", w.name, w.active_index()));
        for t in &w.tabs {
            v.push(format!("  {:?} {} {} {}", t.kind, t.title, t.host, t.url));
        }
    }
    for b in app.bookmarks.items() {
        v.push(format!("bm {} {} {:?}", b.title, b.host, b.tint));
    }
    v
}

#[test]
fn a_restart_restores_what_the_user_left() {
    let mut app = App::new();
    app.settings.startup = Startup::RestoreSession;
    app.apply(Action::ToggleLayout);
    app.chrome.set_threshold(100.0);
    app.chrome.dim_page = true;
    app.settings.blocks.recents = false;
    app.settings.set_notif_mode(1, NotifMode::Muted);
    app.settings
        .keymap
        .rebind(BindingId::Switcher, Chord::new(true, false, true, "s"));
    app.apply(Action::SwitchWorkspace(1));
    app.apply(Action::NewTab);
    app.navigate("rust-lang.org");

    let back = persist::decode(&app.snapshot()).expect("own output reads back");
    assert_eq!(durable(&back), durable(&app));
}

#[test]
fn restored_tabs_come_back_cold_with_fresh_ids() {
    let mut app = App::new();
    app.settings.startup = Startup::RestoreSession;
    for w in &mut app.workspaces.spaces {
        for t in &mut w.tabs {
            t.state = TabState::Live;
        }
    }
    let back = persist::decode(&app.snapshot()).unwrap();
    let tabs = back.workspaces.all_tabs();
    assert!(tabs.iter().all(|t| t.state == TabState::Cold), "no web view survives a restart");
    let mut ids: Vec<_> = tabs.iter().map(|t| t.id).collect();
    ids.dedup();
    assert_eq!(ids.len(), tabs.len(), "ids are unique");
    assert!(back.workspaces.current().active_tab().last_active
        >= back.workspaces.current().tabs.iter().map(|t| t.last_active).max().unwrap(),
        "the front tab ranks most recent for the first eviction pass");
}

#[test]
fn history_is_not_written_down() {
    let mut app = App::new();
    app.navigate("secret-research.example");
    assert!(!app.history.is_empty());
    app.workspaces.current_mut().tabs.retain(|t| t.host != "secret-research.example");
    let text = app.snapshot();
    assert!(!text.contains("secret-research"), "history dies with the session");
}

#[test]
fn a_first_launch_shows_no_demo_data() {
    let app = App::fresh();
    assert!(app.bookmarks.is_empty());
    assert_eq!(app.workspaces.spaces.len(), 3, "⌘1–3 still has somewhere to go");
    for w in &app.workspaces.spaces {
        assert_eq!(w.tabs.len(), 1);
        assert_eq!(w.tabs[0].kind, PageKind::NewTab);
    }
    let text = app.snapshot();
    assert!(!text.contains("Sprint board") && !text.contains("github.com"));
}

#[test]
fn startup_new_tab_restores_tabs_and_adds_one_in_front() {
    let mut app = App::new();
    app.settings.startup = Startup::NewTab;
    app.apply(Action::SwitchWorkspace(2));
    let before = app.workspaces.spaces[2].tabs.len();

    let back = persist::decode(&app.snapshot()).unwrap();
    assert_eq!(back.workspaces.current, 2, "lands in the workspace you left");
    let ws = back.workspaces.current();
    assert_eq!(ws.tabs.len(), before + 1, "nothing was thrown away");
    assert_eq!(ws.active_tab().kind, PageKind::NewTab);
}

#[test]
fn startup_pinned_workspace_lands_on_the_first() {
    let mut app = App::new();
    app.settings.startup = Startup::PinnedWorkspace;
    app.apply(Action::SwitchWorkspace(2));
    let back = persist::decode(&app.snapshot()).unwrap();
    assert_eq!(back.workspaces.current, 0);
}

#[test]
fn an_error_page_is_saved_as_the_site_it_failed_to_reach() {
    let mut app = App::new();
    app.settings.startup = Startup::RestoreSession;
    let id = app.workspaces.current().active;
    app.navigate("nonexistent-host.invalid");
    app.fail_navigation(id, -1003);
    assert_eq!(app.workspaces.current().active_tab().kind, PageKind::Error);

    let back = persist::decode(&app.snapshot()).unwrap();
    let t = back.workspaces.current().active_tab();
    assert_eq!(t.kind, PageKind::Web, "a restart retries rather than apologising");
    assert!(t.url.contains("nonexistent-host.invalid"));
}

#[test]
fn awkward_titles_survive_the_round_trip() {
    let mut app = App::new();
    app.settings.startup = Startup::RestoreSession;
    let id = app.workspaces.current().active;
    let title = "tabs\there, lines\nthere, back\\slash and émoji 🦀";
    app.workspaces.set_location(id, title, "example.com", "https://example.com/?q=a\tb");
    let back = persist::decode(&app.snapshot()).unwrap();
    assert_eq!(back.workspaces.current().active_tab().title, title);
    assert_eq!(back.workspaces.current().active_tab().url, "https://example.com/?q=a\tb");
}

#[test]
fn a_newer_file_is_refused_so_it_is_not_overwritten() {
    let text = format!("compact-state\t{}\ncurrent\t0\n", FORMAT_VERSION + 1);
    assert_eq!(persist::decode(&text).err(), Some(LoadError::TooNew(FORMAT_VERSION + 1)));
}

#[test]
fn garbage_is_reported_not_guessed_at() {
    assert_eq!(persist::decode("").err(), Some(LoadError::NotState));
    assert_eq!(persist::decode("{\"json\": true}").err(), Some(LoadError::NotState));
    let truncated = "compact-state\t1\nworkspace\tPersonal\n";
    assert!(matches!(persist::decode(truncated), Err(LoadError::Corrupt { line: 2, .. })));
    let orphan = "compact-state\t1\ntab\tweb\tt\th\tu\n";
    assert!(matches!(persist::decode(orphan), Err(LoadError::Corrupt { .. })));
}

#[test]
fn unknown_records_and_out_of_range_indices_are_tolerated() {
    let text = "compact-state\t1\nfrom-the-future\tx\ncurrent\t9\n\
                workspace\tSolo\tall\t7\ntab\tweb\tA\ta.test\thttps://a.test\n\
                workspace\tEmpty\tmuted\t0\n";
    let app = persist::decode(text).unwrap();
    assert_eq!(app.workspaces.spaces.len(), 2);
    assert_eq!(app.workspaces.current, 1, "clamped");
    assert_eq!(app.workspaces.spaces[0].active_tab().host, "a.test", "clamped");
    assert_eq!(
        app.workspaces.spaces[1].tabs[0].kind,
        PageKind::NewTab,
        "an empty workspace reopens on a new tab rather than breaking the never-empty rule"
    );
}

#[test]
fn boot_renders_an_internal_front_tab_instead_of_fetching_it() {
    let mut app = App::fresh();
    let fx = app.boot();
    let active = app.workspaces.current().active;
    assert!(
        fx.iter().any(|e| matches!(e, Effect::RenderInternal(id, _) if *id == active)),
        "{fx:?}"
    );
    assert!(!fx.iter().any(|e| matches!(e, Effect::Load(_, u) if u.starts_with("compact://"))));
}

#[test]
fn boot_loads_a_web_front_tab_by_url() {
    let mut app = App::new();
    let active = app.workspaces.current().active;
    let url = app.workspaces.current().active_tab().url.clone();
    let fx = app.boot();
    assert!(fx.contains(&Effect::Load(active, url)));
    assert!(app.workspaces.current().active_tab().state.is_live());
}

#[test]
fn the_palette_advertises_remapped_keys() {
    let mut app = App::new();
    let hint = |app: &App| {
        app.commands().into_iter().find(|c| c.action == Action::OpenSwitcher).unwrap().hint
    };
    assert_eq!(hint(&app), "⌥Space", "not ⌘Space, which Spotlight owns");
    app.settings.keymap.rebind(BindingId::Switcher, Chord::new(true, false, false, "o"));
    assert_eq!(hint(&app), "⌘O");
}

#[test]
fn the_switcher_default_is_not_a_key_macos_reserves() {
    let app = App::new();
    assert_eq!(
        app.resolve_key(" ", Mods { cmd: false, shift: false, alt: true }, false),
        Some(Action::OpenSwitcher)
    );
}

#[test]
fn boot_counts_the_front_tab_before_drawing_the_memory_readout() {
    let mut app = App::fresh();
    let fx = app.boot();
    let html = fx
        .iter()
        .find_map(|e| match e { Effect::RenderInternal(_, m) => Some(m.clone()), _ => None })
        .expect("the new tab page is drawn");
    assert!(!html.contains("~0 MB"), "the front tab is already live when drawn");
}

// ---- agent workspaces ------------------------------------------------------

use crate::policy::plan_eviction_with;
use crate::workspace::{agent_workspace_name, Control};

#[test]
fn an_agent_workspace_is_named_after_its_agent() {
    assert_eq!(agent_workspace_name("Claude Code"), "Claude Code's workspace");
    assert_eq!(agent_workspace_name("opencode"), "opencode's workspace");
    assert_eq!(agent_workspace_name("Muse"), "Muse's workspace");
    assert_eq!(agent_workspace_name("Atlas"), "Atlas' workspace");
    assert_eq!(agent_workspace_name("  "), "Agent's workspace");
}

#[test]
fn agents_get_workspaces_after_yours_and_get_them_back_on_reconnect() {
    let mut app = App::fresh();
    let i = app.open_agent_workspace("Claude Code", "~/proj/site");
    assert_eq!(i, 3, "after Personal, Work and Research");
    let ws = &app.workspaces.spaces[i];
    assert!(ws.is_agent());
    assert_eq!(ws.name, "Claude Code's workspace");
    assert_eq!(ws.tabs.len(), 1);
    assert_eq!(app.workspaces.current, 0, "opening one does not take you there");

    let toast = activity(&app);
    assert_eq!(toast.len(), 1, "a new agent workspace announces itself");
    assert_eq!(toast[0].workspace, i);
    assert_eq!(toast[0].source, "Claude Code");

    assert_eq!(app.open_agent_workspace("Claude Code", "~/proj/site"), i, "same agent, same project");
    assert_eq!(activity(&app).len(), 1, "a reconnect is not news");
    assert_eq!(app.open_agent_workspace("Claude Code", "~/proj/other"), 4, "another project is another workspace");
    assert_eq!(app.open_agent_workspace("opencode", "~/proj/site"), 5, "another agent is another workspace");
}

#[test]
fn closing_an_agent_workspace_destroys_its_tabs_and_renumbers_what_came_after() {
    let mut app = App::fresh();
    let a = app.open_agent_workspace("Claude Code", "~/a");
    let b = app.open_agent_workspace("Muse", "~/b");
    let a_tab = app.workspaces.spaces[a].active;
    let b_tab = app.workspaces.spaces[b].active;
    app.settings.set_notif_mode(b, crate::settings::NotifMode::Muted);

    let fx = app.apply(Action::CloseWorkspace(a));
    assert!(fx.contains(&Effect::Destroy(a_tab)));
    assert_eq!(app.workspaces.spaces.len(), 4);
    assert_eq!(app.workspaces.spaces[3].name, "Muse's workspace", "Muse moved down one");
    assert_eq!(app.workspaces.spaces[3].active, b_tab);
    assert_eq!(
        app.settings.notif_mode(3),
        crate::settings::NotifMode::Muted,
        "its notification setting moved with it"
    );
    assert!(
        activity(&app).iter().all(|n| n.workspace == 3),
        "the closed workspace's toast is gone and Muse's points at its new index"
    );
}

#[test]
fn your_own_workspaces_cannot_be_closed() {
    let mut app = App::fresh();
    assert!(app.apply(Action::CloseWorkspace(1)).is_empty());
    assert_eq!(app.workspaces.spaces.len(), 3);
}

#[test]
fn closing_the_agent_workspace_you_are_in_takes_you_home() {
    let mut app = App::fresh();
    let a = app.open_agent_workspace("Claude Code", "~/a");
    app.apply(Action::SwitchWorkspace(a));
    assert_eq!(app.workspaces.current, a);
    let fx = app.apply(Action::CloseWorkspace(a));
    assert_eq!(app.workspaces.current, 0);
    assert!(fx.contains(&Effect::SlideWorkspace(-1)));
}

#[test]
fn taking_control_pauses_the_agent_until_you_hand_back() {
    let mut app = App::fresh();
    let a = app.open_agent_workspace("Claude Code", "~/a");
    assert!(app.agent_may_act(a));
    assert!(!app.agent_may_act(0), "your workspaces have no agent");

    app.apply(Action::SwitchWorkspace(a));
    app.apply(Action::TakeControl);
    assert!(!app.agent_may_act(a));
    assert_eq!(app.workspaces.spaces[a].agent.as_ref().unwrap().control, Control::User);
    assert!(!app.take_control(), "already yours");

    app.apply(Action::HandBack);
    assert!(app.agent_may_act(a));

    app.apply(Action::SwitchWorkspace(0));
    assert!(!app.take_control(), "nothing to take in your own workspace");
}

#[test]
fn the_palette_offers_agent_commands_only_in_an_agent_workspace() {
    let mut app = App::fresh();
    let has = |app: &App, label: &str| app.commands().iter().any(|c| c.label == label);
    let a = app.open_agent_workspace("Claude Code", "~/a");
    assert!(!has(&app, "Take control from Claude Code"));

    app.apply(Action::SwitchWorkspace(a));
    let first = &app.commands()[1];
    assert_eq!(first.label, "Take control from Claude Code", "agent rows come first, after review");
    assert!(has(&app, "Close Claude Code's workspace"));

    app.apply(Action::TakeControl);
    assert!(has(&app, "Hand back to Claude Code"));
    assert!(!has(&app, "Take control from Claude Code"));
}

#[test]
fn agent_tabs_are_evicted_before_yours() {
    // Ids 1–2 are yours and coldest; 3–4 are the agent's and warmer.
    let tabs = live_tabs(&[100, 100, 100, 100, 100]);
    let agent = [TabId(3), TabId(4)];
    let plan = plan_eviction_with(&tabs, TabId(5), &budget(300, 8, 1), Pressure::Normal, &agent, &[]);
    assert_eq!(plan.hibernate, vec![TabId(3), TabId(4)]);
}

#[test]
fn agents_are_held_to_their_own_slice_even_under_budget() {
    // Plenty of bytes, but max_live 6 gives agents a slice of 2.
    let tabs = live_tabs(&[50, 50, 50, 50, 50, 50]);
    let agent = [TabId(2), TabId(3), TabId(4), TabId(5)];
    let b = budget(4000, 6, 1);
    assert_eq!(b.agent_live(), 2);
    let plan = plan_eviction_with(&tabs, TabId(6), &b, Pressure::Normal, &agent, &[]);
    assert_eq!(plan.hibernate, vec![TabId(2), TabId(3)], "coldest agent tabs over the slice");
}

#[test]
fn the_active_tab_survives_even_in_an_agent_workspace() {
    let tabs = live_tabs(&[100, 100, 100]);
    let agent = [TabId(1), TabId(2), TabId(3)];
    let plan = plan_eviction_with(&tabs, TabId(1), &budget(100, 8, 1), Pressure::Normal, &agent, &[]);
    assert!(!plan.hibernate.contains(&TabId(1)));
}

#[test]
fn agent_workspaces_are_not_prewarmed() {
    let mut app = App::fresh();
    let a = app.open_agent_workspace("Claude Code", "~/a");
    let tab = app.workspaces.spaces[a].active;
    app.workspaces.set_page(tab, crate::page::PageKind::Web, None);
    app.workspaces.set_location(tab, "Site", "localhost:3000", "http://localhost:3000");
    let fx = app.reconcile();
    assert!(
        !fx.iter().any(|e| matches!(e, Effect::Load(id, _) if *id == tab)),
        "its tab waits for the agent to ask"
    );
}

#[test]
fn agent_workspaces_survive_a_restart() {
    let mut app = App::fresh();
    app.settings.startup = crate::settings::Startup::RestoreSession;
    let a = app.open_agent_workspace("Claude Code", "~/proj\tsite");
    app.workspaces.spaces[a].agent.as_mut().unwrap().control = Control::User;
    let text = app.snapshot();
    assert!(text.starts_with(&format!("compact-state\t{FORMAT_VERSION}\n")));

    let back = persist::decode(&text).unwrap();
    let ws = &back.workspaces.spaces[a];
    assert_eq!(ws.name, "Claude Code's workspace");
    let agent = ws.agent.as_ref().expect("still an agent's");
    assert_eq!(agent.project, "~/proj\tsite", "escaped and unescaped");
    assert_eq!(agent.control, Control::Agent, "control is not saved");
    assert!(!back.workspaces.spaces[0].is_agent());
}

#[test]
fn a_version_one_file_still_opens() {
    let v1 = "compact-state\t1\ncurrent\t0\nworkspace\tPersonal\tall\t0\ntab\tweb\tA\ta.test\thttps://a.test\n";
    let app = persist::decode(v1).unwrap();
    assert_eq!(app.workspaces.spaces.len(), 1);
    assert!(!app.workspaces.spaces[0].is_agent());
}

#[test]
fn a_new_tab_at_startup_never_lands_in_an_agent_workspace() {
    let mut app = App::fresh();
    let a = app.open_agent_workspace("Claude Code", "~/a");
    app.apply(Action::SwitchWorkspace(a));
    app.settings.startup = crate::settings::Startup::NewTab;
    let back = persist::decode(&app.snapshot()).unwrap();
    assert_eq!(back.workspaces.current, 0);
    assert_eq!(back.workspaces.spaces[a].tabs.len(), 1, "the agent's workspace is untouched");
}

#[test]
fn internal_pages_draw_a_robot_for_agent_workspaces() {
    let mut app = App::fresh();
    app.open_agent_workspace("Claude Code", "~/a");
    let page = crate::html::new_tab(&app, 0);
    assert!(page.contains("Claude Code&#39;s workspace") || page.contains("Claude Code's workspace"));
    assert_eq!(page.matches(r#"aria-label="Agent workspace""#).count(), 1);
}

#[test]
fn each_agent_workspace_has_its_own_profile() {
    use crate::workspace::Profile;
    let mut app = App::fresh();
    let a = app.open_agent_workspace("Claude Code", "~/a");
    let b = app.open_agent_workspace("Muse", "~/a");
    let mine = app.workspaces.spaces[0].active;
    let pa = app.workspaces.profile_of(app.workspaces.spaces[a].active);
    let pb = app.workspaces.profile_of(app.workspaces.spaces[b].active);
    assert_eq!(app.workspaces.profile_of(mine), Profile::Personal);
    assert!(matches!(pa, Profile::Agent(_)));
    assert_ne!(pa, pb, "agents never share cookies");
    assert_eq!(app.workspaces.profile_of(TabId(9999)), Profile::Personal, "unknown tabs get no agent's store");
}

// ---- agent requests --------------------------------------------------------

use crate::agent::{is_loopback, parse_tab_ref, tab_ref, url_host, AgentError, UrlCheck, MAX_AGENT_TABS};

fn agent_app() -> (App, usize) {
    let mut app = App::fresh();
    let ws = app.open_agent_workspace("Claude Code", "~/site");
    (app, ws)
}

#[test]
fn hosts_are_read_the_way_the_network_reads_them() {
    assert_eq!(url_host("http://localhost:3000/a").as_deref(), Some("localhost"));
    assert_eq!(url_host("https://Example.COM").as_deref(), Some("example.com"));
    assert_eq!(url_host("http://localhost@evil.test/").as_deref(), Some("evil.test"), "user info is not the host");
    assert_eq!(url_host("http://[::1]:8080/").as_deref(), Some("[::1]"));
    assert_eq!(url_host("ftp://x"), None);
    assert!(is_loopback("app.localhost"));
    assert!(!is_loopback("localhost.evil.test"));
    assert_eq!(parse_tab_ref(&tab_ref(TabId(12))), Some(TabId(12)));
    assert_eq!(parse_tab_ref("12"), Some(TabId(12)));
    assert_eq!(parse_tab_ref("tab"), None);
}

#[test]
fn loopback_is_allowed_and_everything_else_is_asked_once() {
    let (mut app, ws) = agent_app();
    assert_eq!(app.check_url(ws, "localhost:3000"), UrlCheck::Allowed("http://localhost:3000".into()));
    assert!(matches!(app.check_url(ws, "127.0.0.1:5173/x"), UrlCheck::Allowed(_)));
    match app.check_url(ws, "docs.rs/serde") {
        UrlCheck::Ask { host, url } => {
            assert_eq!(host, "docs.rs");
            assert_eq!(url, "https://docs.rs/serde");
        }
        other => panic!("{other:?}"),
    }
    assert!(matches!(app.check_url(ws, "http://localhost@evil.test"), UrlCheck::Ask { host, .. } if host == "evil.test"));
    app.allow_host(ws, "docs.rs");
    assert!(matches!(app.check_url(ws, "https://docs.rs/tokio"), UrlCheck::Allowed(_)));
    let other = app.open_agent_workspace("Muse", "~/site");
    assert!(matches!(app.check_url(other, "docs.rs"), UrlCheck::Ask { .. }), "a yes is per agent");
}

#[test]
fn schemes_past_the_web_are_refused() {
    let (app, ws) = agent_app();
    for bad in ["file:///etc/passwd", "javascript:alert(1)", "data:text/html,hi", "compact://settings", "ftp://x.test", "chrome://flags"] {
        assert!(matches!(app.check_url(ws, bad), UrlCheck::Refused(_)), "{bad}");
    }
}

#[test]
fn an_agent_opens_tabs_in_its_own_workspace_without_moving_you() {
    let (mut app, ws) = agent_app();
    let (first, fx) = app.agent_open(ws, "localhost:3000").unwrap();
    assert!(fx.contains(&Effect::Load(first, "http://localhost:3000".into())));
    assert_eq!(app.workspaces.spaces[ws].tabs.len(), 1, "the blank new-tab page was reused");
    assert_eq!(app.workspaces.current, 0, "you stay where you are");

    let (second, _) = app.agent_open(ws, "localhost:3000/pricing").unwrap();
    assert_ne!(first, second);
    assert_eq!(app.workspaces.spaces[ws].active, second);
    assert_eq!(app.workspaces.spaces[0].tabs.len(), 1, "your workspace is untouched");
}

#[test]
fn an_agent_cannot_touch_tabs_outside_its_workspace() {
    let (mut app, ws) = agent_app();
    let yours = app.workspaces.spaces[0].active;
    assert_eq!(app.agent_navigate(ws, yours, "localhost:1").unwrap_err(), AgentError::NoSuchTab(yours));
    assert_eq!(app.agent_close(ws, yours).unwrap_err(), AgentError::NoSuchTab(yours));
    assert_eq!(app.agent_wake(ws, yours).unwrap_err(), AgentError::NoSuchTab(yours));
    assert_eq!(app.agent_open(0, "localhost:1").unwrap_err(), AgentError::NotAgentWorkspace);
}

#[test]
fn an_agent_waits_while_you_have_control_but_may_still_look() {
    let (mut app, ws) = agent_app();
    let (tab, _) = app.agent_open(ws, "localhost:3000").unwrap();
    app.apply(Action::SwitchWorkspace(ws));
    app.apply(Action::TakeControl);
    assert!(matches!(app.agent_open(ws, "localhost:3000"), Err(AgentError::Paused { .. })));
    assert!(matches!(app.agent_navigate(ws, tab, "localhost:1"), Err(AgentError::Paused { .. })));
    assert!(app.agent_wake(ws, tab).is_ok(), "looking is not acting");
}

#[test]
fn unapproved_sites_are_not_loaded() {
    let (mut app, ws) = agent_app();
    assert_eq!(
        app.agent_open(ws, "example.com").unwrap_err(),
        AgentError::NeedsApproval { host: "example.com".into() }
    );
    assert_eq!(app.workspaces.spaces[ws].tabs[0].kind, crate::page::PageKind::NewTab, "nothing changed");
}

#[test]
fn an_agent_hits_a_wall_on_tabs() {
    let (mut app, ws) = agent_app();
    for i in 0..MAX_AGENT_TABS {
        app.agent_open(ws, &format!("localhost:{}", 3000 + i)).unwrap();
    }
    assert_eq!(app.agent_open(ws, "localhost:9999").unwrap_err(), AgentError::TooManyTabs);
}

#[test]
fn the_last_agent_tab_stays() {
    let (mut app, ws) = agent_app();
    let only = app.workspaces.spaces[ws].active;
    assert!(matches!(app.agent_close(ws, only), Err(AgentError::Refused(_))));
}

#[test]
fn approvals_survive_a_restart_and_revoking_closes_the_workspace() {
    let (mut app, ws) = agent_app();
    app.approve("Claude Code", "~/site");
    app.approve("Claude Code", "~/site");
    assert_eq!(app.approvals.len(), 1);
    let back = persist::decode(&app.snapshot()).unwrap();
    assert!(back.is_approved("Claude Code", "~/site"));
    assert!(!back.is_approved("Claude Code", "~/other"));

    let fx = app.apply(Action::RevokeAgent(ws));
    assert!(fx.iter().any(|e| matches!(e, Effect::Destroy(_))));
    assert!(!app.is_approved("Claude Code", "~/site"));
    assert_eq!(app.agent_workspace("Claude Code", "~/site"), None);
}

#[test]
fn a_reconnecting_agent_finds_its_workspace() {
    let (app, ws) = agent_app();
    assert_eq!(app.agent_workspace("Claude Code", "~/site"), Some(ws));
    assert_eq!(app.agent_workspace("Claude Code", "~/elsewhere"), None);
}

#[test]
fn a_connected_agents_working_tab_outlives_your_prewarmed_ones() {
    // You are on 1; 2 and 3 are prewarmed; 4 is the tab an agent just opened.
    let tabs = live_tabs(&[100, 100, 100, 100]);
    let agent = [TabId(4)];
    let mut b = budget(4000, 3, 1);
    let plan = plan_eviction_with(&tabs, TabId(1), &b, Pressure::Normal, &agent, &[]);
    assert_eq!(plan.hibernate, vec![TabId(4)], "a disconnected agent's tab goes first");
    let plan = plan_eviction_with(&tabs, TabId(1), &b, Pressure::Normal, &agent, &agent);
    assert_eq!(plan.hibernate, vec![TabId(2)], "a speculative prewarm yields to work in progress");
    b.max_live = 8;
    let plan = plan_eviction_with(&tabs, TabId(1), &b, Pressure::Critical, &agent, &agent);
    assert!(plan.hibernate.contains(&TabId(4)), "under critical pressure only your tab is sure");
}

#[test]
fn only_connected_agents_hold_a_working_tab() {
    let (mut app, ws) = agent_app();
    assert!(app.workspaces.agent_working_tabs().is_empty());
    assert!(app.set_agent_connected("Claude Code", "~/site", true));
    assert_eq!(app.workspaces.agent_working_tabs(), vec![app.workspaces.spaces[ws].active]);
    app.apply(Action::SwitchWorkspace(ws));
    app.apply(Action::TakeControl);
    assert!(app.workspaces.agent_working_tabs().is_empty(), "not while you drive");
}

// ---- review mode -------------------------------------------------------------

use crate::review::{page_key, Anchor, CommentId, CommentKind, CommentState};

fn reviewing() -> (App, usize, TabId) {
    let (mut app, ws) = agent_app();
    let (tab, _) = app.agent_open(ws, "localhost:3000/pricing").unwrap();
    app.apply(Action::SwitchWorkspace(ws));
    (app, ws, tab)
}

fn anchor_on(url: &str) -> Anchor {
    Anchor {
        url: url.into(),
        selector: "main > button.cta".into(),
        text: "Start free trial".into(),
        role: "button".into(),
        rect: [24.0, 610.0, 342.0, 48.0],
        viewport: [390.0, 844.0],
        ..Anchor::default()
    }
}

#[test]
fn review_mode_is_for_agent_workspaces() {
    let mut app = App::fresh();
    assert!(app.apply(Action::ToggleReview).is_empty());
    assert_eq!(app.review, None);
    let (mut app, _, tab) = reviewing();
    assert_eq!(app.apply(Action::ToggleReview), vec![Effect::SetReview(tab, true)]);
    assert_eq!(app.review, Some(tab));
    assert_eq!(app.apply(Action::ToggleReview), vec![Effect::SetReview(tab, false)], "the same key leaves");
    assert_eq!(app.review, None);
}

#[test]
fn review_mode_is_bound_to_cmd_shift_r_and_offered_in_the_palette() {
    let (mut app, _, _) = reviewing();
    let m = Mods { cmd: true, shift: true, alt: false };
    assert_eq!(app.resolve_key("r", m, false), Some(Action::ToggleReview));
    assert_eq!(app.commands()[0].label, "Review this page");
    assert_eq!(app.commands()[0].hint, "⌘⇧R");
    app.apply(Action::ToggleReview);
    assert!(app.review.is_some());
}

#[test]
fn comments_reach_the_agent_once() {
    let (mut app, ws, tab) = reviewing();
    let id = app.add_comment(tab, CommentKind::Change, "  Text is cut off on mobile ", anchor_on("http://localhost:3000/pricing")).unwrap();
    assert_eq!(id.label(), "c1");
    assert_eq!(CommentId::parse("c1"), Some(id));
    assert!(app.has_undelivered(ws));
    let got = app.take_undelivered(ws);
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].body, "Text is cut off on mobile", "trimmed");
    assert!(app.take_undelivered(ws).is_empty(), "handed over once");
    assert!(app.add_comment(tab, CommentKind::Change, "   ", Anchor::default()).is_none(), "empty is not a comment");
}

#[test]
fn comments_cannot_be_pinned_on_your_own_tabs() {
    let (mut app, _, _) = reviewing();
    let yours = app.workspaces.spaces[0].active;
    assert!(app.add_comment(yours, CommentKind::Change, "hi", Anchor::default()).is_none());
}

#[test]
fn pins_follow_the_page_not_the_tab() {
    let (mut app, ws, tab) = reviewing();
    app.add_comment(tab, CommentKind::Change, "a", anchor_on("http://localhost:3000/pricing#plans")).unwrap();
    app.add_comment(tab, CommentKind::Add, "b", anchor_on("http://localhost:3000/about")).unwrap();
    assert_eq!(app.pins_for(tab).len(), 1, "only this page's");
    let (other, _) = app.agent_open(ws, "localhost:3000/pricing/").unwrap();
    assert_eq!(app.pins_for(other).len(), 1, "a second tab on the same page shows them too");
    assert_eq!(page_key("http://x/a/#b"), "http://x/a");
}

#[test]
fn comments_survive_a_restart_with_their_anchors() {
    let (mut app, ws, tab) = reviewing();
    let mut a = anchor_on("http://localhost:3000/pricing");
    a.data = "data-testid=cta".into();
    a.styles = "font-size: 18px; overflow: hidden".into();
    a.framework = "react".into();
    a.component = "PricingCard".into();
    a.source = "/src/Pricing.tsx:42:7".into();
    let id = app.add_comment(tab, CommentKind::Question, "Why\tthis\ncolour?", a.clone()).unwrap();
    app.set_comment_crop(id, "c1.png");
    app.take_undelivered(ws);
    app.workspaces.spaces[ws].agent.as_mut().unwrap().comments[0]
        .replies
        .push(crate::review::Remark { from_agent: true, body: "Brand purple.".into() });

    let back = persist::decode(&app.snapshot()).unwrap();
    let c = &back.workspaces.spaces[ws].agent.as_ref().unwrap().comments[0];
    assert_eq!(c.body, "Why\tthis\ncolour?");
    assert_eq!(c.kind, CommentKind::Question);
    assert!(c.delivered);
    assert_eq!(c.anchor.crop.as_deref(), Some("c1.png"));
    assert_eq!(c.anchor.rect, a.rect);
    assert_eq!(c.anchor.data, a.data);
    assert_eq!((c.anchor.framework.as_str(), c.anchor.component.as_str(), c.anchor.source.as_str()), ("react", "PricingCard", "/src/Pricing.tsx:42:7"));
    assert_eq!(c.replies[0].body, "Brand purple.");
    assert_eq!(back.comment_seq, 1, "ids carry on from where they were");

    let mut back = back;
    let (t, _) = back.agent_open(ws, "localhost:3000/pricing").unwrap();
    let next = back.add_comment(t, CommentKind::Change, "again", Anchor::default()).unwrap();
    assert_eq!(next, CommentId(2));
}

#[test]
fn resolved_comments_have_no_pins_and_are_trimmed_first() {
    let (mut app, ws, tab) = reviewing();
    let url = "http://localhost:3000/pricing";
    let first = app.add_comment(tab, CommentKind::Change, "old", anchor_on(url)).unwrap();
    app.workspaces.spaces[ws].agent.as_mut().unwrap().comments[0].state = CommentState::Resolved;
    assert!(app.pins_for(tab).is_empty());
    for i in 0..crate::review::MAX_COMMENTS {
        app.add_comment(tab, CommentKind::Change, &format!("n{i}"), anchor_on(url)).unwrap();
    }
    let all = app.comments_in(ws, None);
    assert_eq!(all.len(), crate::review::MAX_COMMENTS);
    assert!(all.iter().all(|c| c.id != first), "the resolved one went first");
}

#[test]
fn an_agent_asking_for_review_tells_you_and_readies_the_overlay() {
    let (mut app, ws) = agent_app();
    let (tab, _) = app.agent_open(ws, "localhost:3000").unwrap();
    let fx = app.request_review(ws, tab, Some("New pricing cards"));
    assert_eq!(fx, vec![Effect::SetReview(tab, true)]);
    assert_eq!(app.review, Some(tab));
    let toast = activity(&app);
    assert_eq!(toast.last().unwrap().text, "New pricing cards");
    assert!(app.request_review(0, tab, None).is_empty(), "not from your own workspace");
}

#[test]
fn reading_comments_marks_them_seen() {
    let (mut app, ws, tab) = reviewing();
    app.add_comment(tab, CommentKind::Change, "x", Anchor::default()).unwrap();
    app.mark_delivered(ws);
    assert!(!app.has_undelivered(ws));
}

#[test]
fn the_review_loop_runs_comment_to_fix_to_resolved() {
    let (mut app, ws, tab) = reviewing();
    let id = app.add_comment(tab, CommentKind::Change, "Cut off on mobile", anchor_on("http://localhost:3000/pricing")).unwrap();
    app.take_undelivered(ws);

    let fx = app.mark_addressed(ws, id, Some("Wrapped the label")).unwrap();
    assert_eq!(fx, vec![Effect::Reload(tab)], "the page reloads so the fix shows");
    let c = app.comments_in(ws, None)[0].clone();
    assert_eq!(c.state, CommentState::Addressed);
    assert_eq!(c.replies[0].body, "Wrapped the label");
    assert!(c.replies[0].from_agent);
    assert!(activity(&app).last().unwrap().text.contains("addressed c1"), "you hear about it");
    assert_eq!(app.pins_for(tab).len(), 1, "still pinned until you look");

    assert!(app.resolve_comment(id));
    assert!(!app.resolve_comment(id), "once");
    assert!(app.pins_for(tab).is_empty());
    assert!(app.mark_addressed(ws, id, None).is_err(), "a resolved comment stays resolved");
}

#[test]
fn replying_reopens_the_thread_for_the_agent() {
    let (mut app, ws, tab) = reviewing();
    let id = app.add_comment(tab, CommentKind::Change, "Too small", anchor_on("http://localhost:3000/pricing")).unwrap();
    app.take_undelivered(ws);
    app.mark_addressed(ws, id, Some("Bigger now")).unwrap();
    assert!(!app.has_undelivered(ws));

    assert!(app.reply_comment(id, "Still too small on mobile"));
    let again = app.take_undelivered(ws);
    assert_eq!(again.len(), 1, "back on the agent's list");
    assert_eq!(again[0].state, CommentState::Open);
    let bodies: Vec<_> = again[0].replies.iter().map(|r| (r.from_agent, r.body.as_str())).collect();
    assert_eq!(bodies, [(true, "Bigger now"), (false, "Still too small on mobile")], "the whole thread");
    assert!(!app.reply_comment(id, "  "), "empty replies are ignored");
}

#[test]
fn an_agent_answers_questions_in_the_thread() {
    let (mut app, ws, tab) = reviewing();
    let id = app.add_comment(tab, CommentKind::Question, "Why purple?", anchor_on("http://localhost:3000/pricing")).unwrap();
    app.agent_reply_comment(ws, id, "It is the brand colour.").unwrap();
    assert_eq!(app.comments_in(ws, None)[0].replies[0].body, "It is the brand colour.");
    assert!(activity(&app).last().unwrap().text.contains("It is the brand colour."));
}

#[test]
fn an_agent_can_only_touch_its_own_comments() {
    let (mut app, ws, tab) = reviewing();
    let id = app.add_comment(tab, CommentKind::Change, "x", anchor_on("http://localhost:3000/pricing")).unwrap();
    let other = app.open_agent_workspace("Muse", "~/site");
    assert_eq!(app.mark_addressed(other, id, None).unwrap_err(), crate::agent::AgentError::NoSuchComment(id));
    assert!(app.agent_reply_comment(other, id, "hi").is_err());
    assert!(app.mark_addressed(0, id, None).is_err(), "nor can your workspace");
    assert!(app.agent_reply_comment(ws, CommentId(99), "hi").is_err());
}

#[test]
fn comments_saved_before_source_lookup_still_open() {
    let (mut app, ws, tab) = reviewing();
    app.add_comment(tab, CommentKind::Change, "x", anchor_on("http://localhost:3000/pricing")).unwrap();
    // Cut the three source fields off the end, as an older build wrote it.
    let text: String = app
        .snapshot()
        .lines()
        .map(|l| if l.starts_with("comment\t") { l.split('\t').take(16).collect::<Vec<_>>().join("\t") } else { l.to_string() })
        .collect::<Vec<_>>()
        .join("\n");
    let back = persist::decode(&text).unwrap();
    let c = &back.workspaces.spaces[ws].agent.as_ref().unwrap().comments[0];
    assert_eq!(c.body, "x");
    assert!(c.anchor.source.is_empty());
}

//! Compact's platform-free core.
//!
//! This crate holds the entire behaviour of the browser except for drawing and
//! the web engine itself. It has no dependencies, no `unsafe`, and — critically
//! — **no engine handles**. It decides *that* a tab should become live; the
//! engine host decides *how* and owns the resulting web view.
//!
//! That boundary is the main defence against the classic browser leak, where a
//! closed tab stays alive because some view still references it. Here the tab
//! model is structurally incapable of holding a reference to a web view, so a
//! leak has exactly one place it can hide, and it is a place with a test.
//!
//! The shell drives this by calling [`App::apply`] with an [`Action`], then
//! draining the returned [`Effect`]s.

pub mod bookmark;
pub mod chrome;
pub mod command;
pub mod notify;
pub mod policy;
pub mod tab;
pub mod theme;
pub mod workspace;

#[cfg(test)]
mod tests;

use bookmark::Bookmarks;
use chrome::{ChromeConfig, ChromeVisibility, Reveal};
use command::{Action, Command};
use notify::Notifications;
use policy::{LiveBudget, MemoryReport, Pressure};
use tab::{TabId, TabState};
use workspace::WorkspaceSet;

/// Which modal surface, if any, is on top.
///
/// Exactly one can be open: they all capture the keyboard, and the canvas's ESC
/// handler closes all of them together rather than unwinding a stack.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Overlay {
    #[default]
    None,
    Palette,
    Shortcuts,
    Switcher,
    Settings,
}

/// Work the shell must perform against the engine or the window server.
///
/// The core never performs these itself — returning them keeps every side
/// effect explicit and lets the whole state machine be tested without WebKit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Effect {
    /// Serialize this tab's session and tear down its web view.
    Hibernate(TabId),
    /// Rebuild a web view from this tab's stored session blob.
    Restore(TabId),
    /// Create a web view and load `url` (a tab that has no blob yet).
    Load(TabId, String),
    /// Permanently drop this tab's web view and stored blob.
    Destroy(TabId),
    /// Move keyboard focus into the address field and select its contents.
    FocusAddressField,
    /// Move keyboard focus into the palette query field.
    FocusPaletteField,
    /// Begin a workspace slide; `+1` travels right, `-1` left.
    SlideWorkspace(i8),
}

/// Floor for a freshly-started navigation, so the bar is visible immediately.
pub const MIN_VISIBLE_PROGRESS: f64 = 0.08;
/// How long the bar stays at full after a load completes.
pub const LOAD_FINISH_HOLD_MS: u64 = 420;

/// Where an address-bar entry points.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    /// Full URL to load.
    pub url: String,
    /// What the address bar shows — the URL without its scheme.
    pub display: String,
    /// Provisional title, replaced by the real one once the page loads.
    pub title: String,
}

/// Search used when input is not an address.
///
/// DuckDuckGo rather than Google: it needs no cookies or account to work, which
/// suits a browser whose whole premise is not accumulating state.
const SEARCH_URL: &str = "https://duckduckgo.com/?q=";

/// Decide whether `input` is an address or a search query.
///
/// The rule is deliberately conservative about calling something a URL. A user
/// typing `rust ownership` means a search; a user typing `rust-lang.org` means a
/// site. The discriminator is a dot in the first path segment with no spaces
/// around it — plus an explicit scheme or `localhost`, which are unambiguous.
pub fn parse_address(input: &str) -> Option<Target> {
    let raw = input.trim();
    if raw.is_empty() {
        return None;
    }

    let has_scheme = raw.starts_with("http://") || raw.starts_with("https://");
    let stripped = raw
        .strip_prefix("https://")
        .or_else(|| raw.strip_prefix("http://"))
        .unwrap_or(raw);
    let head = stripped.split('/').next().unwrap_or(stripped);
    // Strip any port before judging whether the host looks like a domain.
    let hostname = head.split(':').next().unwrap_or(head);

    let looks_like_host = !raw.contains(char::is_whitespace)
        && (hostname == "localhost"
            || (hostname.contains('.')
                && !hostname.starts_with('.')
                && !hostname.ends_with('.')));

    if has_scheme || looks_like_host {
        let url = if has_scheme {
            raw.to_string()
        } else {
            format!("https://{stripped}")
        };
        let title = hostname
            .strip_prefix("www.")
            .unwrap_or(hostname)
            .split('.')
            .next()
            .filter(|s| !s.is_empty())
            .map(capitalize)
            .unwrap_or_else(|| hostname.to_string());
        return Some(Target { url, display: stripped.to_string(), title });
    }

    let query = percent_encode(raw);
    Some(Target {
        url: format!("{SEARCH_URL}{query}"),
        display: raw.to_string(),
        title: raw.to_string(),
    })
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

/// Percent-encode a query string. Hand-rolled to keep the core dependency-free.
fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*b as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// The whole application state.
pub struct App {
    pub workspaces: WorkspaceSet,
    pub chrome: ChromeConfig,
    pub reveal: Reveal,
    pub overlay: Overlay,
    pub notifications: Notifications,
    pub bookmarks: Bookmarks,
    pub budget: LiveBudget,
    pub pressure: Pressure,

    /// Palette query and highlighted row.
    pub query: String,
    pub selection: usize,

    /// In-progress address edit. `None` means the field mirrors the active
    /// tab's host; `Some` means the user has typed and owns the field until
    /// they commit or the active tab changes.
    pub address_draft: Option<String>,

    /// Whether the address bar has been asked for (⌘L, the palette, a click on
    /// it). Cleared as soon as the user is done with it — on commit, on Escape,
    /// or on clicking away — so it never lingers over the page.
    pub address_summoned: bool,

    /// The onboarding hint, dismissed permanently on first real interaction.
    pub hint_visible: bool,

    /// Monotonic milliseconds, supplied by the shell.
    ///
    /// The core never reads a clock itself, so toast ages and expiry stay
    /// exactly testable.
    pub clock: u64,

    /// Real footprint of the browser and its web content processes, in bytes,
    /// when the platform has measured one. Preferred over the per-tab estimate
    /// for anything shown to the user.
    pub measured_footprint: Option<u64>,
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

impl App {
    pub fn new() -> Self {
        Self {
            workspaces: WorkspaceSet::seeded(),
            chrome: ChromeConfig::default(),
            reveal: Reveal::default(),
            overlay: Overlay::None,
            notifications: Notifications::new(),
            bookmarks: Bookmarks::seeded(),
            budget: LiveBudget::default(),
            pressure: Pressure::Normal,
            query: String::new(),
            selection: 0,
            address_draft: None,
            address_summoned: false,
            hint_visible: true,
            clock: 0,
            measured_footprint: None,
        }
    }

    // ---- derived view state -------------------------------------------------

    pub fn visibility(&self) -> ChromeVisibility {
        ChromeVisibility::derive(self.reveal, &self.chrome, self.address_summoned)
    }

    /// Put the address bar away without navigating.
    pub fn dismiss_address(&mut self) -> bool {
        let was = self.address_summoned;
        self.address_summoned = false;
        self.address_draft = None;
        was
    }

    /// What the address field should display right now.
    pub fn address_text(&self) -> &str {
        match &self.address_draft {
            Some(d) => d,
            None => &self.workspaces.current().active_tab().host,
        }
    }

    pub fn commands(&self) -> Vec<Command> {
        command::filter(
            command::command_list(self.chrome.layout, self.chrome.pinned, self.is_bookmarked()),
            &self.query,
        )
    }

    /// Whether the active tab's host is saved.
    pub fn is_bookmarked(&self) -> bool {
        self.bookmarks
            .contains_host(&self.workspaces.current().active_tab().host)
    }

    /// `★` when the active tab is saved, `☆` otherwise.
    pub fn star_glyph(&self) -> &'static str {
        if self.is_bookmarked() { "★" } else { "☆" }
    }

    /// What the load bar should show for the active tab, if anything.
    ///
    /// Two departures from raw `estimatedProgress`, both about the bar being
    /// believable rather than merely accurate:
    ///
    /// - It never starts below [`MIN_VISIBLE_PROGRESS`]. WebKit reports 0 for
    ///   the first moments of a navigation, and a bar of zero width is
    ///   indistinguishable from no bar — so clicking a link would look like
    ///   nothing happened.
    /// - It holds at full for [`LOAD_FINISH_HOLD_MS`] after the load ends.
    ///   Most navigations finish faster than the eye resolves; without the hold
    ///   the bar appears and disappears as a flicker.
    pub fn load_progress(&self) -> Option<f64> {
        let t = self.workspaces.current().active_tab();
        if t.loading {
            return Some(t.progress.max(MIN_VISIBLE_PROGRESS));
        }
        match t.completed_at {
            Some(at) if self.clock.saturating_sub(at) < LOAD_FINISH_HOLD_MS => Some(1.0),
            _ => None,
        }
    }

    pub fn memory(&self) -> MemoryReport {
        MemoryReport::of(&self.workspaces.all_tabs())
    }

    /// The memory readout for the settings footer.
    ///
    /// Reports what was actually measured when a measurement exists, and falls
    /// back to a tilde-marked estimate otherwise.
    pub fn memory_label(&self) -> String {
        match self.measured_footprint {
            Some(b) => format!("{} MB", b / (1024 * 1024)),
            None => self.memory().label(),
        }
    }

    pub fn hint_showing(&self) -> bool {
        self.chrome.show_hint
            && self.hint_visible
            && self.overlay == Overlay::None
            // A toast occupies the same corner of the user's attention and is
            // actually about something; the hint yields to it.
            && self.notifications.is_empty()
    }

    // ---- input --------------------------------------------------------------

    /// Feed cursor motion in. Returns `true` when chrome visibility changed.
    pub fn on_cursor(&mut self, cursor: Option<(f64, f64)>, size: (f64, f64)) -> bool {
        let changed = self.reveal.update(cursor, size, self.chrome.threshold);
        if changed && (self.reveal.top || self.reveal.side) {
            self.hint_visible = false;
        }
        changed
    }

    /// Apply an action, returning the engine work it implies.
    pub fn apply(&mut self, action: Action) -> Vec<Effect> {
        let mut fx = Vec::new();
        match action {
            Action::Dismiss => {
                self.overlay = Overlay::None;
                self.address_summoned = false;
                self.address_draft = None;
                self.query.clear();
                self.selection = 0;
            }
            Action::OpenPalette => {
                self.overlay = Overlay::Palette;
                self.query.clear();
                self.selection = 0;
                self.hint_visible = false;
                fx.push(Effect::FocusPaletteField);
            }
            Action::OpenSettings => {
                self.overlay = Overlay::Settings;
                self.hint_visible = false;
            }
            Action::OpenShortcuts => {
                self.overlay = Overlay::Shortcuts;
                self.hint_visible = false;
            }
            Action::OpenSwitcher => {
                self.overlay = if self.overlay == Overlay::Switcher {
                    Overlay::None
                } else {
                    Overlay::Switcher
                };
                self.hint_visible = false;
            }
            Action::FocusAddress => {
                // Summon it first — focusing a hidden field would put the caret
                // somewhere the user cannot see.
                self.address_summoned = true;
                self.hint_visible = false;
                fx.push(Effect::FocusAddressField);
            }
            Action::ToggleLayout => {
                self.chrome.layout = self.chrome.layout.toggled();
                // Show the result. Silently swapping hidden chrome gives no
                // feedback that anything happened. These are hover latches, so
                // they clear again on the next mouse move.
                self.reveal.top = true;
                self.reveal.side = true;
                self.hint_visible = false;
            }
            Action::TogglePin => self.chrome.pinned = !self.chrome.pinned,
            Action::NewTab => {
                let id = self.workspaces.new_tab();
                self.address_draft = None;
                self.hint_visible = false;
                fx.push(Effect::Load(id, "https://compact.app/new".into()));
                self.reconcile_into(&mut fx);
            }
            Action::CloseActiveTab => {
                let id = self.workspaces.current().active;
                fx.extend(self.close_tab(id));
            }
            Action::CloseTab(id) => fx.extend(self.close_tab(id)),
            Action::SelectTab(id) => fx.extend(self.select_tab(id)),
            Action::SwitchWorkspace(i) => fx.extend(self.switch_workspace(i, None)),
            Action::JumpLatestNotification => fx.extend(self.jump_latest()),
            Action::ToggleBookmark => {
                let tint = self.workspaces.current().dot;
                let tab = self.workspaces.current().active_tab().clone();
                self.bookmarks.toggle(&tab, tint);
            }
            Action::OpenBookmark(id) => {
                if let Some(b) = self.bookmarks.get(id) {
                    let (host, title) = (b.host.clone(), b.title.clone());
                    let url = format!("https://{host}");
                    let active = self.workspaces.current().active;
                    self.workspaces.set_location(active, &title, &host, &url);
                    self.address_draft = None;
                    self.overlay = Overlay::None;
                    fx.push(Effect::Load(active, url));
                }
            }
            Action::RemoveBookmark(id) => self.bookmarks.remove(id),
        }
        fx
    }

    fn close_tab(&mut self, id: TabId) -> Vec<Effect> {
        let mut fx = Vec::new();
        if self.workspaces.close(id).is_some() {
            fx.push(Effect::Destroy(id));
            self.address_draft = None;
            // Closing frees a slot, so a neighbour may now be worth prewarming.
            self.reconcile_into(&mut fx);
        }
        fx
    }

    fn select_tab(&mut self, id: TabId) -> Vec<Effect> {
        let mut fx = Vec::new();
        self.workspaces.select(id);
        self.address_draft = None;
        // Bring the tab back before running the budget, so the newly-focused
        // tab is never the one the same pass decides to evict.
        if let Some((_, t)) = self.workspaces.locate(id) {
            match t.state {
                TabState::Hibernated => fx.push(Effect::Restore(id)),
                TabState::Cold => fx.push(Effect::Load(id, t.url.clone())),
                TabState::Live => {}
            }
        }
        self.workspaces.set_state(id, TabState::Live);
        self.reconcile_into(&mut fx);
        fx
    }

    fn switch_workspace(&mut self, to: usize, focus: Option<TabId>) -> Vec<Effect> {
        let mut fx = Vec::new();
        let Some(dir) = self.workspaces.switch(to, focus) else {
            return fx;
        };
        self.overlay = Overlay::None;
        self.hint_visible = false;
        self.address_draft = None;
        self.notifications.clear_workspace(to);
        fx.push(Effect::SlideWorkspace(dir));
        let active = self.workspaces.current().active;
        fx.extend(self.select_tab(active));
        self.post_arrival(to);
        fx
    }

    /// Confirm which workspace the user just landed in.
    ///
    /// Workspaces are otherwise identified only by an accent colour and a name
    /// in chrome that is hidden by default, so after a ⌘2 the answer to "where
    /// am I" can be nowhere on screen. The toast carries the workspace's own
    /// colour and name, plus what is actually in front of you.
    ///
    /// Posted after `select_tab`, so it names the tab the switch settled on
    /// rather than the one that was active before it.
    fn post_arrival(&mut self, to: usize) {
        let Some(ws) = self.workspaces.spaces.get(to) else { return };
        let active = ws.active_tab();
        let (text, source, tab) = (active.title.clone(), ws.count_label(), active.id);
        self.notifications
            .push_status(to, text, source, tab, self.clock);
    }

    fn jump_latest(&mut self) -> Vec<Effect> {
        let current = self.workspaces.current;
        let target = self
            .notifications
            .latest_elsewhere(current)
            .map(|n| (n.id, n.workspace, n.tab));

        if let Some((id, ws, tab)) = target {
            self.notifications.dismiss(id);
            return self.switch_workspace(ws, Some(tab));
        }
        // No pending toast: fall back to any workspace with unread activity, so
        // the binding still does something useful after toasts have expired.
        let unread = self
            .workspaces
            .spaces
            .iter()
            .position(|w| w.unread > 0)
            .filter(|i| *i != current);
        match unread {
            Some(i) => self.switch_workspace(i, None),
            None => Vec::new(),
        }
    }

    /// Open a notification: jump to its workspace and focus its tab.
    pub fn open_notification(&mut self, id: u64) -> Vec<Effect> {
        let Some(n) = self.notifications.items().iter().find(|n| n.id == id) else {
            return Vec::new();
        };
        // A status toast is a confirmation, not a destination — the user is
        // already where it points. Clicking it just gets it out of the way.
        if n.kind == notify::Kind::Status {
            self.notifications.dismiss(id);
            return Vec::new();
        }
        let (ws, tab) = (n.workspace, n.tab);
        self.notifications.dismiss(id);
        if ws == self.workspaces.current {
            self.select_tab(tab)
        } else {
            self.switch_workspace(ws, Some(tab))
        }
    }

    // ---- address bar --------------------------------------------------------

    pub fn set_address_draft(&mut self, text: impl Into<String>) {
        self.address_draft = Some(text.into());
    }

    /// Commit the address field (Enter).
    ///
    /// Anything that is not plausibly an address is treated as a search, which
    /// is what makes a single bar usable for both. See [`parse_address`].
    pub fn commit_address(&mut self) -> Vec<Effect> {
        let Some(raw) = self.address_draft.take() else {
            return Vec::new();
        };
        self.navigate(&raw)
    }

    /// Navigate the active tab to whatever `input` denotes.
    pub fn navigate(&mut self, input: &str) -> Vec<Effect> {
        let Some(target) = parse_address(input) else {
            return Vec::new();
        };
        let id = self.workspaces.current().active;
        self.workspaces
            .set_location(id, &target.title, &target.display, &target.url);
        self.address_draft = None;
        // Its job is done; leaving it up would cover the page you just asked for.
        self.address_summoned = false;
        vec![Effect::Load(id, target.url)]
    }

    // ---- palette ------------------------------------------------------------

    pub fn set_query(&mut self, q: impl Into<String>) {
        self.query = q.into();
        self.selection = 0;
    }

    pub fn move_selection(&mut self, delta: isize) {
        let n = self.commands().len();
        if n == 0 {
            self.selection = 0;
            return;
        }
        let next = self.selection as isize + delta;
        self.selection = next.clamp(0, n as isize - 1) as usize;
    }

    /// Run the highlighted palette row.
    pub fn run_selected(&mut self) -> Vec<Effect> {
        let Some(cmd) = self.commands().get(self.selection).map(|c| c.action) else {
            return Vec::new();
        };
        self.apply(Action::Dismiss);
        self.apply(cmd)
    }

    // ---- memory -------------------------------------------------------------

    /// Record navigation progress reported by the engine.
    ///
    /// Returns whether anything worth redrawing changed.
    pub fn record_progress(&mut self, id: TabId, loading: bool, progress: f64) -> bool {
        let now = self.clock;
        for w in &mut self.workspaces.spaces {
            if let Some(t) = w.tab_mut(id) {
                let changed = t.loading != loading || (t.progress - progress).abs() > 0.01;
                if t.loading && !loading {
                    // Falling edge: start the hold that lets a fast load be seen.
                    t.completed_at = Some(now);
                } else if !t.loading && loading {
                    t.completed_at = None;
                }
                t.loading = loading;
                t.progress = progress;

                // The completion hold is the only state that changes without
                // the engine reporting anything, so it has to be aged here —
                // and, crucially, retired. Leaving `completed_at` set forever
                // makes this return true on every poll, which pins the whole
                // chrome to a 12 Hz redraw for the life of the process.
                if let Some(at) = t.completed_at {
                    if now.saturating_sub(at) >= LOAD_FINISH_HOLD_MS {
                        t.completed_at = None;
                        // One last redraw, to run the fade out.
                        return true;
                    }
                    return true;
                }
                return changed;
            }
        }
        false
    }

    /// Record a measured footprint for a tab.
    pub fn record_footprint(&mut self, id: TabId, bytes: u64) {
        self.workspaces.set_footprint(id, bytes);
    }

    /// React to a system pressure change, shedding tabs immediately if needed.
    pub fn set_pressure(&mut self, p: Pressure) -> Vec<Effect> {
        self.pressure = p;
        let mut fx = Vec::new();
        self.reconcile_into(&mut fx);
        fx
    }

    /// Bring the live set in line with the budget, then spend any slack on
    /// prewarming. Safe to call after any state change; it is a no-op when the
    /// live set already fits.
    pub fn reconcile(&mut self) -> Vec<Effect> {
        let mut fx = Vec::new();
        self.reconcile_into(&mut fx);
        fx
    }

    fn reconcile_into(&mut self, fx: &mut Vec<Effect>) {
        let active = self.workspaces.current().active;
        let tabs = self.workspaces.all_tabs();

        let plan = policy::plan_eviction(&tabs, active, &self.budget, self.pressure);
        for id in &plan.hibernate {
            self.workspaces.set_state(*id, TabState::Hibernated);
            fx.push(Effect::Hibernate(*id));
        }

        // Only prewarm once the live set is already within budget; otherwise a
        // single pass could evict and restore in the same breath.
        if plan.is_empty() {
            // Other workspaces' active tabs come first. A workspace switch is a
            // single keystroke onto a whole different page, so arriving at an
            // unpainted one is the most visible stall in the product — more so
            // than the next tab along, which is one click away and cheap to
            // recover from.
            let mut wanted: Vec<TabId> = self
                .workspaces
                .spaces
                .iter()
                .enumerate()
                .filter(|(i, _)| *i != self.workspaces.current)
                .map(|(_, w)| w.active)
                .filter(|id| {
                    self.workspaces
                        .locate(*id)
                        .is_some_and(|(_, t)| t.state.needs_restore())
                })
                .collect();

            let tabs = self.workspaces.current().tabs.clone();
            wanted.extend(policy::prewarm_candidates(
                &tabs,
                active,
                &self.budget,
                self.pressure,
                1,
            ));

            // Respect the same headroom rule the in-workspace prewarm uses:
            // never spend more than the budget has spare.
            let all = self.workspaces.all_tabs();
            let live_bytes: u64 = all
                .iter()
                .filter(|t| t.state.is_live())
                .map(tab::TabMeta::effective_footprint)
                .sum();
            let live_count = all.iter().filter(|t| t.state.is_live()).count();
            let ceiling = self.budget.effective_bytes(self.pressure);
            let room = (((ceiling.saturating_sub(live_bytes)) / tab::ESTIMATED_LIVE_BYTES) as usize)
                .min(self.budget.max_live.saturating_sub(live_count));
            wanted.truncate(room);

            for id in wanted {
                if let Some((_, t)) = self.workspaces.locate(id) {
                    let eff = match t.state {
                        TabState::Hibernated => Effect::Restore(id),
                        TabState::Cold => Effect::Load(id, t.url.clone()),
                        TabState::Live => continue,
                    };
                    self.workspaces.set_state(id, TabState::Live);
                    fx.push(eff);
                }
            }
        }
    }
}

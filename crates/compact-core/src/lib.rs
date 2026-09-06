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
pub mod bridge;
pub mod chrome;
pub mod command;
pub mod html;
pub mod notify;
pub mod page;
pub mod policy;
pub mod settings;
pub mod tab;
pub mod theme;
pub mod workspace;

#[cfg(test)]
mod tests;

use bookmark::Bookmarks;
use bridge::{Incoming, PageMessage};
use chrome::{ChromeConfig, ChromeVisibility, Reveal};
use command::{Action, Command, Mods};
use notify::Notifications;
use page::{ErrorKind, PageKind};
use policy::{LiveBudget, MemoryReport, Pressure};
use settings::{NotifMode, Settings};
use tab::{TabId, TabState};
use theme::Rgba;
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
    /// Render one of Compact's own pages into this tab's web view.
    ///
    /// Carries the finished markup rather than a page identifier so the shell
    /// has no say in what an internal page contains — [`html`] is the only
    /// place that decides, and it is testable without a window.
    RenderInternal(TabId, String),
    /// Reload whatever the tab is already pointing at.
    Reload(TabId),
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

/// One row of the new-tab page's RECENT list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Visit {
    pub title: String,
    pub host: String,
    pub url: String,
}

impl Visit {
    /// Tint for the square chip, derived from the host so a site keeps the
    /// same colour across sessions without anything being stored.
    pub fn tint(&self) -> Rgba {
        let dots = theme::WORKSPACE_DOTS;
        dots[self.host.len() % dots.len()]
    }
}

/// How many visits the new-tab page remembers.
///
/// Deliberately small and in-memory only. A browser whose pitch is not
/// accumulating state should not quietly grow a history file; this is a
/// convenience for the current session, and it dies with it.
pub const HISTORY_LIMIT: usize = 6;

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
    /// The full settings page's state: startup, new-tab blocks, per-workspace
    /// notification modes, and the keymap.
    pub settings: Settings,
    /// Most-recently-visited first, deduplicated by host.
    pub history: Vec<Visit>,

    /// Palette query and highlighted row.
    pub query: String,
    pub selection: usize,

    /// In-progress address edit. `None` means the field mirrors the active
    /// tab's host; `Some` means the user has typed and owns the field until
    /// they commit or the active tab changes.
    pub address_draft: Option<String>,

    /// Whether a text field *inside* an internal page holds the keyboard.
    ///
    /// The shell cannot see into a web view, so the page says so itself. Without
    /// it, typing `?` into the new-tab search box would open the shortcuts
    /// overlay instead of the character — the bare bindings have to yield to a
    /// focused field wherever that field lives.
    pub page_field_focused: bool,

    /// Whether the address bar has been asked for (⌘L, the palette, a click on
    /// it). Cleared as soon as the user is done with it — on commit, on Escape,
    /// or on clicking away — so it never lingers over the page.
    pub address_summoned: bool,

    /// The onboarding hint, dismissed permanently on first real interaction.
    pub hint_visible: bool,

    /// Clock time until which cursor movement cannot retract chrome.
    ///
    /// Set when the user changes something whose result *is* the chrome —
    /// toggling the layout — so the confirmation survives long enough to be
    /// read. See [`chrome::REVEAL_HOLD_MS`].
    pub reveal_hold_until: u64,

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
            settings: Settings::default(),
            history: Vec::new(),
            query: String::new(),
            selection: 0,
            address_draft: None,
            address_summoned: false,
            page_field_focused: false,
            hint_visible: true,
            reveal_hold_until: 0,
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
            // The new-tab page is its own onboarding — it names ⌘K and ⌘D and
            // ⌘1–3 on the page itself. A floating hint over it is one more
            // thing to read saying less.
            && self.workspaces.current().active_tab().kind != PageKind::NewTab
    }

    // ---- input --------------------------------------------------------------

    /// Feed cursor motion in. Returns `true` when chrome visibility changed.
    pub fn on_cursor(&mut self, cursor: Option<(f64, f64)>, size: (f64, f64)) -> bool {
        // Chrome the user just asked for outranks where the pointer happens to
        // be. Without this, a click on the strip's LAYOUT button revealed the
        // sidebar and the same gesture's next mouse move immediately retracted
        // it, because the cursor was still at the top right.
        if self.clock < self.reveal_hold_until {
            return false;
        }
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
                // Show the result: silently swapping hidden chrome gives no
                // feedback that anything happened. Latch only the edge the new
                // layout actually answers to, and clear the other.
                //
                // Setting both left a latch behind that the cursor never set —
                // switch to vertical and `top` stayed true, so the strip was
                // one ⌘E away from appearing over a layout that has no strip.
                // The stale latch survives until the cursor crosses the retract
                // threshold, which it may never do if the pointer is already
                // parked near that edge.
                let horizontal = self.chrome.layout == chrome::TabLayout::Horizontal;
                self.reveal.top = horizontal;
                self.reveal.side = !horizontal;
                // And hold it there: the pointer is almost never near the edge
                // that owns the surface the tabs just moved to.
                self.reveal_hold_until = self.clock + chrome::REVEAL_HOLD_MS;
                self.hint_visible = false;
            }
            Action::TogglePin => self.chrome.pinned = !self.chrome.pinned,
            Action::NewTab => {
                let id = self.workspaces.new_tab();
                self.address_draft = None;
                self.hint_visible = false;
                self.render_internal(id, &mut fx);
                self.reconcile_into(&mut fx);
            }
            Action::OpenSettingsPage => {
                self.overlay = Overlay::None;
                self.hint_visible = false;
                let id = self.workspaces.new_tab();
                self.workspaces.set_page(id, PageKind::Settings, None);
                self.address_draft = None;
                self.render_internal(id, &mut fx);
                self.reconcile_into(&mut fx);
            }
            Action::GoHome => {
                let id = self.workspaces.current().active;
                self.workspaces.set_page(id, PageKind::NewTab, None);
                self.address_draft = None;
                self.render_internal(id, &mut fx);
            }
            Action::ShowError(kind) => {
                self.overlay = Overlay::None;
                let id = self.workspaces.current().active;
                self.workspaces.set_page(id, PageKind::Error, Some(kind));
                self.address_draft = None;
                self.render_internal(id, &mut fx);
            }
            Action::Retry => {
                let id = self.workspaces.current().active;
                let Some((_, tab)) = self.workspaces.locate(id) else {
                    return fx;
                };
                match tab.kind {
                    // Re-attempt the navigation that failed. `Load` rather than
                    // `Reload`: the view is showing our error markup, so there
                    // is nothing there to reload.
                    PageKind::Error => {
                        let url = tab.url.clone();
                        self.workspaces.set_page(id, PageKind::Web, None);
                        fx.push(Effect::Load(id, url));
                    }
                    PageKind::Web => fx.push(Effect::Reload(id)),
                    // Redrawing an internal page is the honest meaning of
                    // reload here, and it picks up any setting changed since.
                    PageKind::NewTab | PageKind::Settings => self.render_internal(id, &mut fx),
                }
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

    // ---- internal pages -----------------------------------------------------

    /// Queue a redraw of `id` if it is one of Compact's own pages.
    fn render_internal(&mut self, id: TabId, fx: &mut Vec<Effect>) {
        let Some((_, tab)) = self.workspaces.locate(id) else {
            return;
        };
        if !tab.kind.is_internal() {
            return;
        }
        let tab = tab.clone();
        // Replacing the visible document destroys whatever field held the
        // keyboard. A background prewarm does not, so the check is on the
        // active tab rather than on rendering at all.
        if id == self.workspaces.current().active {
            self.page_field_focused = false;
        }
        if let Some(markup) = html::render(self, &tab) {
            fx.push(Effect::RenderInternal(id, markup));
        }
    }

    /// Redraw the visible internal page, if any.
    ///
    /// Called after every settings mutation. The settings page is itself an
    /// internal page, so a toggle has to repaint the thing you toggled it on —
    /// there is no widget tree to update in place.
    pub fn refresh_active_page(&mut self) -> Vec<Effect> {
        let mut fx = Vec::new();
        let id = self.workspaces.current().active;
        self.render_internal(id, &mut fx);
        fx
    }

    /// Show an error page for a navigation that failed.
    ///
    /// `None` from [`ErrorKind::from_url_error`] means the failure was not one
    /// — a cancelled load, most often a redirect — and nothing is drawn.
    pub fn fail_navigation(&mut self, id: TabId, code: isize) -> Vec<Effect> {
        let Some(kind) = ErrorKind::from_url_error(code) else {
            return Vec::new();
        };
        self.show_error(id, kind)
    }

    /// Show `kind` in `id`, wherever that tab lives.
    pub fn show_error(&mut self, id: TabId, kind: ErrorKind) -> Vec<Effect> {
        let mut fx = Vec::new();
        if self.workspaces.locate(id).is_none() {
            return fx;
        }
        self.workspaces.set_page(id, PageKind::Error, Some(kind));
        self.render_internal(id, &mut fx);
        fx
    }

    // ---- the page bridge ----------------------------------------------------

    /// Act on a message from an internal page.
    pub fn on_page_message(&mut self, incoming: Incoming) -> Vec<Effect> {
        // Record where the page was before doing anything: acting on the
        // message usually regenerates the document, and the offset is the one
        // thing that cannot be rebuilt from app state.
        let active = self.workspaces.current().active;
        for w in &mut self.workspaces.spaces {
            if let Some(t) = w.tab_mut(active) {
                t.scroll = incoming.scroll;
                break;
            }
        }
        match incoming.message {
            PageMessage::Navigate(input) => self.navigate(&input),
            PageMessage::Do(action) => self.apply(action),
            PageMessage::OpenBookmark(id) => self.apply(Action::OpenBookmark(id)),
            PageMessage::SwitchWorkspace(i) => self.apply(Action::SwitchWorkspace(i)),
            PageMessage::SetStartup(v) => {
                self.settings.startup = v;
                self.refresh_active_page()
            }
            PageMessage::ToggleBlock(slug) => {
                self.settings.blocks.toggle(&slug);
                self.refresh_active_page()
            }
            PageMessage::SetNotifMode(ws, mode) => {
                self.settings.set_notif_mode(ws, mode);
                self.refresh_active_page()
            }
            PageMessage::FieldFocus(on) => {
                self.page_field_focused = on;
                Vec::new()
            }
            PageMessage::Record(id) => {
                self.settings.recording =
                    if self.settings.recording == Some(id) { None } else { Some(id) };
                self.refresh_active_page()
            }
        }
    }

    // ---- keymap -------------------------------------------------------------

    /// Feed a keystroke to an in-progress binding recording.
    ///
    /// Returns the effects of accepting it, or `None` when nothing is being
    /// recorded and the key should be resolved normally. Bare modifiers are
    /// swallowed while recording — you cannot bind "⌘" on its own, and treating
    /// the press as a chord would end the recording before the real key landed.
    pub fn record_chord(&mut self, key: &str, mods: Mods) -> Option<Vec<Effect>> {
        let id = self.settings.recording?;
        if key == "Escape" {
            self.settings.recording = None;
            return Some(self.refresh_active_page());
        }
        if key.is_empty() || matches!(key, "Meta" | "Control" | "Alt" | "Shift") {
            return Some(Vec::new());
        }
        self.settings
            .keymap
            .rebind(id, settings::Chord::from_event(key, mods));
        self.settings.recording = None;
        Some(self.refresh_active_page())
    }

    /// Resolve a keystroke against the live keymap.
    pub fn resolve_key(&self, key: &str, mods: Mods, editing: bool) -> Option<Action> {
        command::resolve(&self.settings.keymap, key, mods, editing)
    }

    // ---- notifications ------------------------------------------------------

    /// Record background activity, subject to the workspace's notification mode.
    ///
    /// The unread count is incremented under every mode — a badge that lied
    /// about how much you had missed would be worse than no badge — but only
    /// [`NotifMode::All`], and [`NotifMode::Mentions`] for the workspace's own
    /// foreground tab, actually interrupt.
    pub fn post_activity(
        &mut self,
        ws: usize,
        title: String,
        host: String,
        tab: TabId,
        now: u64,
    ) {
        self.workspaces.add_unread(ws);
        let foreground = self.workspaces.spaces.get(ws).map(|w| w.active) == Some(tab);
        let allowed = match self.settings.notif_mode(ws) {
            NotifMode::All => true,
            NotifMode::Mentions => foreground,
            NotifMode::Muted => false,
        };
        if allowed {
            self.notifications.push(ws, title, host, tab, now);
        }
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
        if let Some(e) = self.activation_effect(id) {
            fx.push(e);
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
        self.remember(&target);
        self.page_field_focused = false;
        self.address_draft = None;
        // Its job is done; leaving it up would cover the page you just asked for.
        self.address_summoned = false;
        vec![Effect::Load(id, target.url)]
    }

    /// Push a visit to the front of the recents list, deduplicated by host.
    ///
    /// By host rather than by URL: RECENT is a list of places, and five rows of
    /// the same site's article pages would push everywhere else off it.
    fn remember(&mut self, target: &Target) {
        let host = target
            .display
            .split('/')
            .next()
            .unwrap_or(&target.display)
            .to_string();
        self.history.retain(|v| v.host != host);
        self.history.insert(
            0,
            Visit { title: target.title.clone(), host, url: target.url.clone() },
        );
        self.history.truncate(HISTORY_LIMIT);
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

    /// Adopt the real page title once a navigation settles.
    ///
    /// Also corrects the recents list. The address bar can only guess a title
    /// from the hostname at the moment you press return — "Rust-lang" for
    /// `rust-lang.org` — and leaving that guess in RECENT would make the
    /// new-tab page the one surface still showing it.
    pub fn adopt_title(&mut self, id: TabId, title: &str, host: &str, url: &str) {
        self.workspaces.set_location(id, title, host, url);
        if let Some(v) = self.history.iter_mut().find(|v| v.host == host) {
            v.title = title.to_string();
            v.url = url.to_string();
        }
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

    /// What it takes to put `id` back on screen, or `None` if it is already
    /// there.
    ///
    /// Internal pages are always redrawn rather than restored. Their markup is
    /// generated from current state, and `interactionState` — which serializes
    /// a back/forward list, not a document — has nothing useful to say about a
    /// page that was never fetched.
    fn activation_effect(&self, id: TabId) -> Option<Effect> {
        let (_, t) = self.workspaces.locate(id)?;
        if t.kind.is_internal() {
            let tab = t.clone();
            return html::render(self, &tab).map(|m| Effect::RenderInternal(id, m));
        }
        match t.state {
            TabState::Hibernated => Some(Effect::Restore(id)),
            TabState::Cold => Some(Effect::Load(id, t.url.clone())),
            TabState::Live => None,
        }
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
                let Some(eff) = self.activation_effect(id) else { continue };
                self.workspaces.set_state(id, TabState::Live);
                fx.push(eff);
            }
        }
    }
}

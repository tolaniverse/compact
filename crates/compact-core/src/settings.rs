//! Preferences that live on the full settings page.
//!
//! The quick drawer (`⌘,`) covers appearance — layout, density, accent, reveal
//! distance. This module is the deeper set the canvas added: what happens at
//! startup, which blocks the new-tab page shows, how loudly each workspace is
//! allowed to interrupt you, and the keymap itself.
//!
//! Bindings are data over [`Action`] rather than a match arm per key, which is
//! the only reason "everything is remappable" costs nothing to honour.

use crate::command::{Action, Mods};

/// What Compact does when it opens.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Startup {
    NewTab,
    RestoreSession,
    PinnedWorkspace,
}

impl Startup {
    pub const ALL: [Startup; 3] =
        [Startup::NewTab, Startup::RestoreSession, Startup::PinnedWorkspace];

    pub const fn label(self) -> &'static str {
        match self {
            Startup::NewTab => "Open a new tab",
            Startup::RestoreSession => "Restore last session",
            Startup::PinnedWorkspace => "Jump to pinned workspace",
        }
    }

    pub const fn slug(self) -> &'static str {
        match self {
            Startup::NewTab => "newtab",
            Startup::RestoreSession => "restore",
            Startup::PinnedWorkspace => "pinned",
        }
    }

    pub fn from_slug(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|v| v.slug() == s)
    }
}

/// How much a background workspace is allowed to interrupt.
///
/// The canvas draws three modes but only distinguishes *muted*. Given three
/// buttons, three behaviours: `All` toasts anything that changes, `Mentions`
/// toasts only the workspace's own foreground tab — the page you deliberately
/// left in front of you — and `Muted` never toasts at all. Every mode still
/// counts unread, so the sidebar badge stays truthful under all three.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NotifMode {
    All,
    Mentions,
    Muted,
}

impl NotifMode {
    pub const ALL: [NotifMode; 3] = [NotifMode::All, NotifMode::Mentions, NotifMode::Muted];

    pub const fn label(self) -> &'static str {
        match self {
            NotifMode::All => "All",
            NotifMode::Mentions => "Mentions",
            NotifMode::Muted => "Muted",
        }
    }

    pub const fn slug(self) -> &'static str {
        match self {
            NotifMode::All => "all",
            NotifMode::Mentions => "mentions",
            NotifMode::Muted => "muted",
        }
    }

    pub fn from_slug(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|v| v.slug() == s)
    }
}

/// Which blocks the new-tab page renders.
#[derive(Clone, Copy, Debug)]
pub struct Blocks {
    pub recents: bool,
    pub pinned: bool,
    pub workspaces: bool,
}

impl Default for Blocks {
    fn default() -> Self {
        Self { recents: true, pinned: true, workspaces: true }
    }
}

impl Blocks {
    pub fn get(&self, slug: &str) -> Option<bool> {
        match slug {
            "recents" => Some(self.recents),
            "pinned" => Some(self.pinned),
            "workspaces" => Some(self.workspaces),
            _ => None,
        }
    }

    /// Returns whether the slug was recognised.
    pub fn toggle(&mut self, slug: &str) -> bool {
        match slug {
            "recents" => self.recents = !self.recents,
            "pinned" => self.pinned = !self.pinned,
            "workspaces" => self.workspaces = !self.workspaces,
            _ => return false,
        }
        true
    }
}

/// A keystroke, normalised.
///
/// `key` is stored lowercase for letters so a binding recorded with caps lock
/// on still fires. Shift is kept separately because `⌘⇧,` and `⌘,` are two
/// different bindings in this design.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Chord {
    pub cmd: bool,
    pub shift: bool,
    pub alt: bool,
    pub key: String,
}

impl Chord {
    pub fn new(cmd: bool, shift: bool, alt: bool, key: &str) -> Self {
        Self { cmd, shift, alt, key: normalize_key(key) }
    }

    pub fn from_event(key: &str, mods: Mods) -> Self {
        Self::new(mods.cmd, mods.shift, mods.alt, key)
    }

    pub fn matches(&self, key: &str, mods: Mods) -> bool {
        *self == Self::from_event(key, mods)
    }

    /// The canvas's rendering: modifiers in ⌘⌥⇧ order, then the key.
    ///
    /// `?` is the one exception. It is written as a key in its own right
    /// everywhere in this design — and everywhere else — so its shift is folded
    /// into the glyph rather than printed as `⇧/`.
    pub fn label(&self) -> String {
        let mut s = String::new();
        if self.cmd {
            s.push('⌘');
        }
        if self.alt {
            s.push('⌥');
        }
        let folds_shift = self.key == "/";
        if self.shift && !folds_shift {
            s.push('⇧');
        }
        s.push_str(&display_key(&self.key));
        s
    }
}

/// Fold a raw key into its stored form.
///
/// Shifted punctuation arrives as the shifted character (`<` for `⌘⇧,`), which
/// would record a binding no unshifted lookup could ever match. Folding the few
/// that matter back to their base key makes `⌘⇧,` and `⌘,` distinguishable by
/// the shift flag alone, which is how the canvas writes them.
fn normalize_key(key: &str) -> String {
    match key {
        " " => return "Space".to_string(),
        "<" => return ",".to_string(),
        ">" => return ".".to_string(),
        "?" => return "/".to_string(),
        _ => {}
    }
    if key.chars().count() == 1 {
        key.to_lowercase()
    } else {
        key.to_string()
    }
}

fn display_key(key: &str) -> String {
    match key {
        "Space" => "Space".to_string(),
        "/" => "?".to_string(),
        k if k.chars().count() == 1 => k.to_uppercase(),
        k => k.to_string(),
    }
}

/// Every remappable command, in the canvas's row order.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BindingId {
    Palette,
    Address,
    NewTab,
    CloseTab,
    Reload,
    Workspace,
    Switcher,
    Jump,
    Layout,
    Bookmark,
    QuickSettings,
    AllSettings,
    Shortcuts,
}

impl BindingId {
    pub const ALL: [BindingId; 13] = [
        BindingId::Palette,
        BindingId::Address,
        BindingId::NewTab,
        BindingId::CloseTab,
        BindingId::Reload,
        BindingId::Workspace,
        BindingId::Switcher,
        BindingId::Jump,
        BindingId::Layout,
        BindingId::Bookmark,
        BindingId::QuickSettings,
        BindingId::AllSettings,
        BindingId::Shortcuts,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            BindingId::Palette => "Command palette",
            BindingId::Address => "Address bar",
            BindingId::NewTab => "New tab",
            BindingId::CloseTab => "Close tab",
            BindingId::Reload => "Reload page",
            BindingId::Workspace => "Workspace 1–3",
            BindingId::Switcher => "Workspace switcher",
            BindingId::Jump => "Jump to notification",
            BindingId::Layout => "Switch layout",
            BindingId::Bookmark => "Bookmark tab",
            BindingId::QuickSettings => "Quick settings",
            BindingId::AllSettings => "All settings",
            BindingId::Shortcuts => "Shortcuts",
        }
    }

    pub const fn slug(self) -> &'static str {
        match self {
            BindingId::Palette => "palette",
            BindingId::Address => "addr",
            BindingId::NewTab => "newtab",
            BindingId::CloseTab => "close",
            BindingId::Reload => "reload",
            BindingId::Workspace => "ws",
            BindingId::Switcher => "switcher",
            BindingId::Jump => "jump",
            BindingId::Layout => "layout",
            BindingId::Bookmark => "bookmark",
            BindingId::QuickSettings => "quick",
            BindingId::AllSettings => "deep",
            BindingId::Shortcuts => "keys",
        }
    }

    pub fn from_slug(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|b| b.slug() == s)
    }

    /// What running this binding does. `None` for [`BindingId::Workspace`],
    /// whose action depends on which digit was pressed.
    pub const fn action(self) -> Option<Action> {
        match self {
            BindingId::Palette => Some(Action::OpenPalette),
            BindingId::Address => Some(Action::FocusAddress),
            BindingId::NewTab => Some(Action::NewTab),
            BindingId::CloseTab => Some(Action::CloseActiveTab),
            BindingId::Reload => Some(Action::Retry),
            BindingId::Workspace => None,
            BindingId::Switcher => Some(Action::OpenSwitcher),
            BindingId::Jump => Some(Action::JumpLatestNotification),
            BindingId::Layout => Some(Action::ToggleLayout),
            BindingId::Bookmark => Some(Action::ToggleBookmark),
            BindingId::QuickSettings => Some(Action::OpenSettings),
            BindingId::AllSettings => Some(Action::OpenSettingsPage),
            BindingId::Shortcuts => Some(Action::OpenShortcuts),
        }
    }

    /// Whether the settings page lets this row be re-recorded.
    ///
    /// `⌘1–3` is a family of bindings over a digit range, not one chord, so
    /// there is nothing coherent to record into it. Showing it and refusing to
    /// edit it is more honest than hiding it — the canvas lists it, and the
    /// user needs to know the digits are taken.
    pub const fn remappable(self) -> bool {
        !matches!(self, BindingId::Workspace)
    }

    fn default_chord(self) -> Chord {
        let c = |k: &str| Chord::new(true, false, false, k);
        match self {
            BindingId::Palette => c("k"),
            BindingId::Address => c("l"),
            BindingId::NewTab => c("t"),
            BindingId::CloseTab => c("w"),
            BindingId::Reload => c("r"),
            // Placeholder: never consulted, `⌘1–3` is matched by range.
            BindingId::Workspace => c("1"),
            // ⌥Space, not the canvas's ⌘Space: macOS gives ⌘Space to Spotlight
            // and the keystroke never reaches the app.
            BindingId::Switcher => Chord::new(false, false, true, "Space"),
            BindingId::Jump => Chord::new(true, true, false, "j"),
            BindingId::Layout => c("e"),
            BindingId::Bookmark => c("d"),
            BindingId::QuickSettings => c(","),
            BindingId::AllSettings => Chord::new(true, true, false, ","),
            // `?` is shift-slash, and that is exactly how it arrives.
            BindingId::Shortcuts => Chord::new(false, true, false, "/"),
        }
    }
}

/// The keymap: one chord per command, editable.
#[derive(Clone, Debug)]
pub struct Keymap {
    entries: Vec<(BindingId, Chord)>,
}

impl Default for Keymap {
    fn default() -> Self {
        Self {
            entries: BindingId::ALL
                .into_iter()
                .map(|b| (b, b.default_chord()))
                .collect(),
        }
    }
}

impl Keymap {
    pub fn chord(&self, id: BindingId) -> &Chord {
        &self.entries.iter().find(|(b, _)| *b == id).expect("every binding is present").1
    }

    /// Rendered shortcut for a command, e.g. `⌘⇧J`.
    pub fn label(&self, id: BindingId) -> String {
        if id == BindingId::Workspace {
            return "⌘1–3".to_string();
        }
        self.chord(id).label()
    }

    pub fn rows(&self) -> impl Iterator<Item = (BindingId, String)> + '_ {
        BindingId::ALL.into_iter().map(|b| (b, self.label(b)))
    }

    /// Bind `id` to `chord`, releasing it from whatever held it before.
    ///
    /// Stealing rather than refusing matters: a user remapping ⌘K to ⌘P should
    /// not have to first discover that ⌘P was taken and clear it. The displaced
    /// row is left unbound, visibly, so nothing silently stops working.
    pub fn rebind(&mut self, id: BindingId, chord: Chord) {
        if !id.remappable() {
            return;
        }
        for (other, c) in &mut self.entries {
            if *other != id && *c == chord {
                *c = Chord::new(false, false, false, "");
            }
        }
        if let Some((_, c)) = self.entries.iter_mut().find(|(b, _)| *b == id) {
            *c = chord;
        }
    }

    /// Which command a keystroke runs, if any.
    pub fn lookup(&self, key: &str, mods: Mods) -> Option<BindingId> {
        self.entries
            .iter()
            .find(|(b, c)| b.remappable() && !c.key.is_empty() && c.matches(key, mods))
            .map(|(b, _)| *b)
    }
}

/// The full settings page's state.
#[derive(Clone, Debug)]
pub struct Settings {
    pub startup: Startup,
    pub blocks: Blocks,
    /// One mode per workspace, indexed the same way.
    pub notif_modes: Vec<NotifMode>,
    pub keymap: Keymap,
    /// The row currently swallowing keystrokes to record a new chord.
    pub recording: Option<BindingId>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            startup: Startup::NewTab,
            blocks: Blocks::default(),
            // Matches the canvas's seed: Research starts on Mentions, so the
            // three modes are visibly distinct on first run.
            notif_modes: vec![NotifMode::All, NotifMode::All, NotifMode::Mentions],
            keymap: Keymap::default(),
            recording: None,
        }
    }
}

impl Settings {
    pub fn notif_mode(&self, ws: usize) -> NotifMode {
        self.notif_modes.get(ws).copied().unwrap_or(NotifMode::All)
    }

    pub fn set_notif_mode(&mut self, ws: usize, mode: NotifMode) {
        if ws >= self.notif_modes.len() {
            self.notif_modes.resize(ws + 1, NotifMode::All);
        }
        self.notif_modes[ws] = mode;
    }
}

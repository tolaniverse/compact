//! Actions, the command palette, and the keymap.
//!
//! Every user-reachable behaviour is an [`Action`]. The palette, the keymap and
//! the chrome's click targets all produce `Action`s and nothing else, so there
//! is exactly one place where behaviour is defined and three thin ways to reach
//! it. The canvas's own promise — "everything is remappable" — only stays cheap
//! if key bindings are data over this enum rather than a match arm per widget.

use crate::bookmark::BookmarkId;
use crate::chrome::TabLayout;
use crate::page::ErrorKind;
use crate::settings::{BindingId, Keymap};
use crate::tab::TabId;
use crate::workspace::{AgentInfo, Control};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
    NewTab,
    CloseActiveTab,
    CloseTab(TabId),
    SelectTab(TabId),
    ToggleLayout,
    TogglePin,
    FocusAddress,
    OpenPalette,
    OpenSettings,
    /// The full settings page (⌘⇧,), which opens as a tab rather than a drawer.
    OpenSettingsPage,
    OpenShortcuts,
    OpenSwitcher,
    SwitchWorkspace(usize),
    JumpLatestNotification,
    /// Save or unsave the active tab (⌘D).
    ToggleBookmark,
    /// Navigate the active tab to a saved bookmark.
    OpenBookmark(BookmarkId),
    /// Forget a bookmark.
    RemoveBookmark(BookmarkId),
    /// Reload the active tab, or re-attempt whatever the error page failed at.
    Retry,
    /// Turn the active tab back into a new-tab page.
    GoHome,
    /// Render an error page in the active tab. Reached from the settings
    /// page's preview chips and from the palette — the same affordance the
    /// canvas ships, and the only way to read six of these pages on a machine
    /// whose network is working.
    ShowError(ErrorKind),
    /// ESC / scrim click: close whatever overlay is on top.
    Dismiss,
    /// Stop the agent acting in the current agent workspace; you drive.
    TakeControl,
    /// Let the current workspace's agent act again.
    HandBack,
    /// Close an agent workspace and its tabs. Ignored for your own.
    CloseWorkspace(usize),
    /// Close an agent workspace and take back its agent's access.
    RevokeAgent(usize),
    /// Comment on the page in front for its agent (⌘⇧R).
    ToggleReview,
    /// The console, network and performance drawer (⌘⌥I).
    ToggleDevTools,
}

/// A palette row.
#[derive(Clone, Debug)]
pub struct Command {
    pub icon: &'static str,
    pub label: String,
    /// Rendered shortcut hint, empty when the command has no binding.
    ///
    /// Read from the live keymap, like the shortcuts overlay. A fixed string
    /// here would keep advertising a default the user has since remapped.
    pub hint: String,
    pub action: Action,
}

/// Build the palette contents. Two rows are stateful — the layout toggle and
/// the pin toggle name the state they move *to*, not the state they are in.
///
/// `agent` is the current workspace's agent, if it has one: its rows come
/// first, because in an agent's workspace they are what you are most likely
/// looking for.
pub fn command_list(
    keymap: &Keymap,
    layout: TabLayout,
    pinned: bool,
    bookmarked: bool,
    agent: Option<(usize, &AgentInfo)>,
) -> Vec<Command> {
    let key = |b: BindingId| keymap.chord(b).label();
    let agent_rows = agent.into_iter().flat_map(|(ws, a)| {
        let control = match a.control {
            Control::Agent => Command {
                icon: "✋",
                label: format!("Take control from {}", a.client),
                hint: String::new(),
                action: Action::TakeControl,
            },
            Control::User => Command {
                icon: "↩",
                label: format!("Hand back to {}", a.client),
                hint: String::new(),
                action: Action::HandBack,
            },
        };
        let close = Command {
            icon: "×",
            label: format!("Close {}", crate::workspace::agent_workspace_name(&a.client)),
            hint: String::new(),
            action: Action::CloseWorkspace(ws),
        };
        let revoke = Command {
            icon: "⊘",
            label: format!("Revoke {}'s access", a.client),
            hint: String::new(),
            action: Action::RevokeAgent(ws),
        };
        let review = Command {
            icon: "✎",
            label: "Review this page".into(),
            hint: key(BindingId::Review),
            action: Action::ToggleReview,
        };
        let devtools = Command {
            icon: "⌥",
            label: "Developer tools".into(),
            hint: key(BindingId::DevTools),
            action: Action::ToggleDevTools,
        };
        [review, devtools, control, close, revoke]
    });
    agent_rows.chain(vec![
        Command { icon: "+", label: "New tab".into(), hint: key(BindingId::NewTab), action: Action::NewTab },
        Command { icon: "×", label: "Close tab".into(), hint: key(BindingId::CloseTab), action: Action::CloseActiveTab },
        Command {
            icon: "▤",
            label: match layout {
                TabLayout::Vertical => "Switch to horizontal tabs".into(),
                TabLayout::Horizontal => "Switch to vertical tabs".into(),
            },
            hint: key(BindingId::Layout),
            action: Action::ToggleLayout,
        },
        Command { icon: "◫", label: "Switch workspace".into(), hint: key(BindingId::Switcher), action: Action::OpenSwitcher },
        Command { icon: "◈", label: "Jump to latest notification".into(), hint: key(BindingId::Jump), action: Action::JumpLatestNotification },
        Command { icon: "⌁", label: "Focus address bar".into(), hint: key(BindingId::Address), action: Action::FocusAddress },
        Command {
            icon: if bookmarked { "★" } else { "☆" },
            label: if bookmarked { "Remove bookmark".into() } else { "Bookmark this tab".into() },
            hint: key(BindingId::Bookmark),
            action: Action::ToggleBookmark,
        },
        Command {
            icon: if pinned { "◉" } else { "○" },
            label: if pinned { "Unpin chrome".into() } else { "Pin chrome open".into() },
            hint: String::new(),
            action: Action::TogglePin,
        },
        Command { icon: "↻", label: "Reload page".into(), hint: key(BindingId::Reload), action: Action::Retry },
        Command { icon: "⌂", label: "New tab page".into(), hint: String::new(), action: Action::GoHome },
        Command { icon: "⚙", label: "Quick settings".into(), hint: key(BindingId::QuickSettings), action: Action::OpenSettings },
        Command { icon: "☰", label: "All settings".into(), hint: key(BindingId::AllSettings), action: Action::OpenSettingsPage },
        Command { icon: "⌨", label: "Keyboard shortcuts".into(), hint: key(BindingId::Shortcuts), action: Action::OpenShortcuts },
    ])
    // The error pages are previewable from the palette, exactly as the canvas
    // has them. They are the one part of the product you cannot otherwise see
    // on demand, because seeing them for real requires something to go wrong.
    .chain(ErrorKind::ALL.into_iter().map(|k| Command {
        icon: "⚠",
        label: format!("Preview error — {}", k.spec().name),
        hint: String::new(),
        action: Action::ShowError(k),
    }))
    .collect()
}

/// Filter the palette by query.
///
/// Case-insensitive substring match, matching the canvas. Deliberately not
/// fuzzy: the command set is nine items, where subsequence matching mostly
/// produces surprising hits ("st" reaching "Switch to horizontal tabs") rather
/// than fewer keystrokes.
pub fn filter(commands: Vec<Command>, query: &str) -> Vec<Command> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return commands;
    }
    commands
        .into_iter()
        .filter(|c| c.label.to_lowercase().contains(&q))
        .collect()
}

/// Modifier state for a key event.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Mods {
    /// Command on macOS. The canvas accepts Control as an equivalent so the
    /// prototype works on non-Apple keyboards; we keep that.
    pub cmd: bool,
    pub shift: bool,
    pub alt: bool,
}

/// Resolve a keystroke to an action against the live keymap.
///
/// Three things are matched outside the table, because none of them is a single
/// chord: `Escape` (always dismisses, and must keep working even if the user
/// rebinds everything else), `⌘1–9` (a digit range, see
/// [`BindingId::Workspace`](crate::settings::BindingId::Workspace)), and the
/// bare `?` — which is in the table, but must not fire while a text field has
/// the keyboard or typing a question mark would open the shortcuts overlay.
pub fn resolve(keymap: &Keymap, key: &str, mods: Mods, text_field_focused: bool) -> Option<Action> {
    if key == "Escape" {
        return Some(Action::Dismiss);
    }

    if mods.cmd && !mods.shift && key.len() == 1 {
        if let Some(d) = key.chars().next().filter(|c| c.is_ascii_digit() && *c != '0') {
            return Some(Action::SwitchWorkspace(d as usize - '1' as usize));
        }
    }

    let id = keymap.lookup(key, mods)?;
    // An unmodified binding belongs to the focused field first.
    if text_field_focused && !mods.cmd && !mods.alt {
        return None;
    }
    id.action()
}

/// One row of the shortcuts overlay.
pub struct Shortcut {
    pub label: &'static str,
    pub keys: String,
}

/// The shortcuts overlay contents, read from the live keymap.
///
/// Previously a static table, which quietly became a lie the moment bindings
/// became editable: the overlay whose entire job is telling you which key does
/// what must not be the one surface that shows the defaults.
pub fn shortcuts(keymap: &Keymap) -> Vec<Shortcut> {
    keymap
        .rows()
        .map(|(b, keys)| Shortcut { label: b.label(), keys })
        .chain(std::iter::once(Shortcut { label: "Dismiss", keys: "ESC".to_string() }))
        .collect()
}

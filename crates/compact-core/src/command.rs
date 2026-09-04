//! Actions, the command palette, and the keymap.
//!
//! Every user-reachable behaviour is an [`Action`]. The palette, the keymap and
//! the chrome's click targets all produce `Action`s and nothing else, so there
//! is exactly one place where behaviour is defined and three thin ways to reach
//! it. The canvas's own promise — "everything is remappable" — only stays cheap
//! if key bindings are data over this enum rather than a match arm per widget.

use crate::bookmark::BookmarkId;
use crate::chrome::TabLayout;
use crate::tab::TabId;

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
    /// ESC / scrim click: close whatever overlay is on top.
    Dismiss,
}

/// A palette row.
#[derive(Clone, Debug)]
pub struct Command {
    pub icon: &'static str,
    pub label: String,
    /// Rendered shortcut hint, empty when the command has no binding.
    pub hint: &'static str,
    pub action: Action,
}

/// Build the palette contents. Two rows are stateful — the layout toggle and
/// the pin toggle name the state they move *to*, not the state they are in.
pub fn command_list(layout: TabLayout, pinned: bool, bookmarked: bool) -> Vec<Command> {
    vec![
        Command { icon: "+", label: "New tab".into(), hint: "⌘T", action: Action::NewTab },
        Command { icon: "×", label: "Close tab".into(), hint: "⌘W", action: Action::CloseActiveTab },
        Command {
            icon: "▤",
            label: match layout {
                TabLayout::Vertical => "Switch to horizontal tabs".into(),
                TabLayout::Horizontal => "Switch to vertical tabs".into(),
            },
            hint: "⌘E",
            action: Action::ToggleLayout,
        },
        Command { icon: "◫", label: "Switch workspace".into(), hint: "⌘Space", action: Action::OpenSwitcher },
        Command { icon: "◈", label: "Jump to latest notification".into(), hint: "⌘⇧J", action: Action::JumpLatestNotification },
        Command { icon: "⌁", label: "Focus address bar".into(), hint: "⌘L", action: Action::FocusAddress },
        Command {
            icon: if bookmarked { "★" } else { "☆" },
            label: if bookmarked { "Remove bookmark".into() } else { "Bookmark this tab".into() },
            hint: "⌘D",
            action: Action::ToggleBookmark,
        },
        Command {
            icon: if pinned { "◉" } else { "○" },
            label: if pinned { "Unpin chrome".into() } else { "Pin chrome open".into() },
            hint: "",
            action: Action::TogglePin,
        },
        Command { icon: "⚙", label: "Settings".into(), hint: "⌘,", action: Action::OpenSettings },
        Command { icon: "⌨", label: "Keyboard shortcuts".into(), hint: "?", action: Action::OpenShortcuts },
    ]
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

/// One row of the shortcuts overlay.
pub struct Shortcut {
    pub label: &'static str,
    pub keys: &'static str,
}

/// The shortcuts overlay contents, in the canvas's order.
pub const SHORTCUTS: &[Shortcut] = &[
    Shortcut { label: "Command palette", keys: "⌘K" },
    Shortcut { label: "Address bar", keys: "⌘L" },
    Shortcut { label: "New tab", keys: "⌘T" },
    Shortcut { label: "Close tab", keys: "⌘W" },
    Shortcut { label: "Workspace 1–3", keys: "⌘1–3" },
    Shortcut { label: "Workspace switcher", keys: "⌘Space" },
    Shortcut { label: "Jump to notification", keys: "⌘⇧J" },
    Shortcut { label: "Bookmark tab", keys: "⌘D" },
    Shortcut { label: "Toggle tab layout", keys: "⌘E" },
    Shortcut { label: "Settings", keys: "⌘," },
    Shortcut { label: "Shortcuts", keys: "?" },
    Shortcut { label: "Dismiss", keys: "ESC" },
];

/// Modifier state for a key event.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Mods {
    /// Command on macOS. The canvas accepts Control as an equivalent so the
    /// prototype works on non-Apple keyboards; we keep that.
    pub cmd: bool,
    pub shift: bool,
    pub alt: bool,
}

/// Resolve a keystroke to an action.
///
/// `text_field_focused` suppresses the bare `?` binding — otherwise typing a
/// question mark into the address bar would open the shortcuts overlay.
pub fn resolve(key: &str, mods: Mods, text_field_focused: bool) -> Option<Action> {
    if key == "Escape" {
        return Some(Action::Dismiss);
    }

    if mods.cmd {
        // ⌘⇧J is checked before the plain-⌘ bindings because `j` alone is not
        // bound; ordering only matters if that changes.
        if mods.shift && key.eq_ignore_ascii_case("j") {
            return Some(Action::JumpLatestNotification);
        }
        if key == " " || key == "Space" {
            return Some(Action::OpenSwitcher);
        }
        if let Some(d) = key.chars().next().filter(|c| c.is_ascii_digit() && *c != '0') {
            if key.len() == 1 {
                return Some(Action::SwitchWorkspace(d as usize - '1' as usize));
            }
        }
        return match key.to_lowercase().as_str() {
            "k" => Some(Action::OpenPalette),
            "l" => Some(Action::FocusAddress),
            "t" => Some(Action::NewTab),
            "w" => Some(Action::CloseActiveTab),
            "d" => Some(Action::ToggleBookmark),
            "e" => Some(Action::ToggleLayout),
            "," => Some(Action::OpenSettings),
            _ => None,
        };
    }

    if key == "?" && !text_field_focused {
        return Some(Action::OpenShortcuts);
    }
    None
}

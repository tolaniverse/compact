//! Split view: two workspaces side by side.
//!
//! One side is focused, and it is simply the current workspace: the address
//! bar, the sidebar, the palette and every key act on it exactly as they do
//! without a split. The other side is a second workspace shown beside it,
//! kept live so it stays painted. `⌘\` opens and closes the split, `⌘⌥\` (or
//! a click) moves focus across, and `⌘1–3` change what the focused side shows
//! — or move focus, when the workspace asked for is already on the other side.
//!
//! The obvious pairing is your work beside an agent's workspace, so opening a
//! split puts an agent's workspace on the other side when there is one.

use crate::tab::TabId;
use crate::{App, Effect};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Side {
    Left,
    Right,
}

impl Side {
    pub const fn flip(self) -> Self {
        match self {
            Side::Left => Side::Right,
            Side::Right => Side::Left,
        }
    }

    pub const fn slug(self) -> &'static str {
        match self {
            Side::Left => "left",
            Side::Right => "right",
        }
    }

    pub fn from_slug(s: &str) -> Option<Self> {
        match s {
            "left" => Some(Side::Left),
            "right" => Some(Side::Right),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Split {
    /// The workspace on the side that is not focused.
    pub other: usize,
    /// Which side the focused (current) workspace is on.
    pub side: Side,
    /// How much of the width the left side gets, from [`MIN_RATIO`] to
    /// [`MAX_RATIO`]. Belongs to the sides, not to focus: switching sides
    /// leaves the divider where it is.
    pub ratio: f64,
}

/// The narrowest a side can be dragged, as a share of the width.
pub const MIN_RATIO: f64 = 0.25;
/// The widest a side can be dragged.
pub const MAX_RATIO: f64 = 0.75;

/// A ratio brought within bounds; anything that is not a number is an even
/// split.
pub fn clamp_ratio(r: f64) -> f64 {
    if r.is_finite() {
        r.clamp(MIN_RATIO, MAX_RATIO)
    } else {
        0.5
    }
}

/// What each side shows, for the shell to lay out.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Pane {
    pub side: Side,
    pub workspace: usize,
    pub tab: TabId,
    pub focused: bool,
}

impl App {
    /// Open split view beside the current workspace, or close it (`⌘\`).
    pub fn toggle_split(&mut self) -> Vec<Effect> {
        let mut fx = Vec::new();
        let ws = self.workspaces.current;
        let tab = self.workspaces.current().active;
        if self.split.take().is_some() {
            self.reconcile_into(&mut fx);
            return fx;
        }
        let n = self.workspaces.spaces.len();
        if n < 2 {
            return fx;
        }
        let other = self
            .workspaces
            .spaces
            .iter()
            .position(|w| w.is_agent())
            .filter(|i| *i != ws)
            .unwrap_or((ws + 1) % n);
        self.split = Some(Split { other, side: Side::Left, ratio: 0.5 });
        self.overlay = crate::Overlay::None;
        self.wake_other(&mut fx);
        let name = self.workspaces.spaces[other].name.clone();
        self.notifications.push_status(ws, format!("Split with {name}"), "⌘⌥\\ to switch sides · ⌘\\ to close".to_string(), tab, self.clock);
        fx
    }

    /// Make sure the unfocused side has a live tab to show.
    fn wake_other(&mut self, fx: &mut Vec<Effect>) {
        let Some(s) = self.split else { return };
        let tab = self.workspaces.spaces[s.other].active;
        if let Some(e) = self.activation_effect(tab) {
            fx.push(e);
        }
        self.workspaces.set_state(tab, crate::tab::TabState::Live);
        self.reconcile_into(fx);
    }

    /// Move focus to the other side (`⌘⌥\`, or a click on it).
    pub fn focus_other_side(&mut self) -> Vec<Effect> {
        let Some(s) = self.split else { return Vec::new() };
        let was = self.workspaces.current;
        self.workspaces.current = s.other;
        self.workspaces.spaces[s.other].unread = 0;
        self.notifications.clear_workspace(s.other);
        self.split = Some(Split { other: was, side: s.side.flip(), ratio: s.ratio });
        self.overlay = crate::Overlay::None;
        self.address_draft = None;
        let tab = self.workspaces.current().active;
        let mut fx = self.select_tab(tab);
        self.wake_other(&mut fx);
        fx
    }

    /// Focus whichever side `side` is, if it is not focused already.
    pub fn focus_side(&mut self, side: Side) -> Vec<Effect> {
        match self.split {
            Some(s) if s.side != side => self.focus_other_side(),
            _ => Vec::new(),
        }
    }

    /// Move the divider so the left side gets `ratio` of the width.
    pub fn set_split_ratio(&mut self, ratio: f64) {
        if let Some(s) = self.split.as_mut() {
            s.ratio = clamp_ratio(ratio);
        }
    }

    /// Both sides, left first, or `None` without a split.
    pub fn panes(&self) -> Option<[Pane; 2]> {
        let s = self.split?;
        let focused = Pane {
            side: s.side,
            workspace: self.workspaces.current,
            tab: self.workspaces.current().active,
            focused: true,
        };
        let other = Pane {
            side: s.side.flip(),
            workspace: s.other,
            tab: self.workspaces.spaces.get(s.other)?.active,
            focused: false,
        };
        Some(match s.side {
            Side::Left => [focused, other],
            Side::Right => [other, focused],
        })
    }

    /// The tab on the unfocused side, which the budget keeps live.
    pub fn other_side_tab(&self) -> Option<TabId> {
        let s = self.split?;
        self.workspaces.spaces.get(s.other).map(|w| w.active)
    }

    /// Keep the split pointing at the right workspaces after workspace
    /// `removed` is gone. The current workspace has already been fixed up.
    pub(crate) fn split_after_remove(&mut self, removed: usize) {
        let Some(mut s) = self.split else { return };
        if s.other == removed {
            self.split = None;
            return;
        }
        if s.other > removed {
            s.other -= 1;
        }
        self.split = (s.other != self.workspaces.current).then_some(s);
    }

    /// Drop a split that no longer makes sense, such as one read from a
    /// file whose workspaces changed.
    pub(crate) fn validate_split(&mut self) {
        if let Some(s) = self.split {
            if s.other >= self.workspaces.spaces.len() || s.other == self.workspaces.current {
                self.split = None;
            }
        }
    }
}

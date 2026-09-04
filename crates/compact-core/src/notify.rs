//! Cross-workspace notifications.
//!
//! Time enters as an explicit millisecond value rather than being read from a
//! clock inside these functions, so expiry and age formatting are testable
//! without sleeping.

use crate::tab::TabId;

/// How many toasts stack before the oldest is dropped (canvas: `.slice(-3)`).
pub const MAX_VISIBLE: usize = 3;
/// Auto-dismiss delay, in milliseconds.
pub const LIFETIME_MS: u64 = 7_000;

#[derive(Clone, Debug)]
pub struct Notification {
    pub id: u64,
    /// Index of the workspace the event came from.
    pub workspace: usize,
    pub text: String,
    /// Origin host, shown under the message.
    pub source: String,
    /// Tab to focus when the toast is clicked.
    pub tab: TabId,
    /// Monotonic milliseconds at which this arrived.
    pub at: u64,
}

impl Notification {
    /// `"12s"` / `"3m"` — the age readout in the toast header.
    pub fn age_label(&self, now: u64) -> String {
        let secs = ((now.saturating_sub(self.at)) / 1000).max(1);
        if secs < 60 {
            format!("{secs}s")
        } else {
            format!("{}m", (secs as f64 / 60.0).round() as u64)
        }
    }

    /// Call-to-action text: notifications from the current workspace open in
    /// place, others advertise the jump binding.
    pub fn action_label(&self, current_workspace: usize) -> &'static str {
        if self.workspace == current_workspace {
            "OPEN"
        } else {
            "⌘⇧J JUMP"
        }
    }
}

#[derive(Debug, Default)]
pub struct Notifications {
    items: Vec<Notification>,
    seq: u64,
}

impl Notifications {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn items(&self) -> &[Notification] {
        &self.items
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn push(&mut self, workspace: usize, text: impl Into<String>, source: impl Into<String>, tab: TabId, now: u64) -> u64 {
        self.seq += 1;
        self.items.push(Notification {
            id: self.seq,
            workspace,
            text: text.into(),
            source: source.into(),
            tab,
            at: now,
        });
        // Keep the newest few; the stack is a peripheral cue, not an inbox.
        if self.items.len() > MAX_VISIBLE {
            let excess = self.items.len() - MAX_VISIBLE;
            self.items.drain(..excess);
        }
        self.seq
    }

    pub fn dismiss(&mut self, id: u64) {
        self.items.retain(|n| n.id != id);
    }

    /// Drop toasts past their lifetime. Returns true if anything went away.
    pub fn expire(&mut self, now: u64) -> bool {
        let before = self.items.len();
        self.items.retain(|n| now.saturating_sub(n.at) < LIFETIME_MS);
        self.items.len() != before
    }

    /// Clear everything belonging to a workspace — called on arrival, since a
    /// notification about the place you are now standing is just noise.
    pub fn clear_workspace(&mut self, ws: usize) {
        self.items.retain(|n| n.workspace != ws);
    }

    /// Newest notification from a workspace other than the current one (⌘⇧J).
    pub fn latest_elsewhere(&self, current: usize) -> Option<&Notification> {
        self.items.iter().rev().find(|n| n.workspace != current)
    }
}

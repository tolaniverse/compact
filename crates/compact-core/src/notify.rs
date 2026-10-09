//! Cross-workspace notifications.
//!
//! Time enters as an explicit millisecond value rather than being read from a
//! clock inside these functions, so expiry and age formatting are testable
//! without sleeping.

use crate::tab::TabId;

/// How many toasts stack before the oldest is dropped (canvas: `.slice(-3)`).
pub const MAX_VISIBLE: usize = 3;
/// Auto-dismiss delay for activity, in milliseconds.
pub const LIFETIME_MS: u64 = 7_000;
/// Auto-dismiss delay for a status confirmation.
///
/// Much shorter: it answers a question the user asked a moment ago by pressing
/// a key, and it has answered it by the time they have read it. Leaving it up
/// as long as real activity would make the stack noisy for people who move
/// between workspaces often — which is the whole point of workspaces.
pub const STATUS_LIFETIME_MS: u64 = 2_500;

/// What a toast is for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    /// Something happened somewhere you were not looking. Actionable: clicking
    /// travels to it.
    Activity,
    /// Confirmation of something the user just did. Not actionable — they are
    /// already where it is telling them they are.
    Status,
}

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
    pub kind: Kind,
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

    /// How long this toast stays up before auto-dismissing.
    pub fn lifetime_ms(&self) -> u64 {
        match self.kind {
            Kind::Activity => LIFETIME_MS,
            Kind::Status => STATUS_LIFETIME_MS,
        }
    }

    /// Call-to-action text.
    ///
    /// A status toast advertises the binding that produced it, which is the one
    /// piece of genuinely new information it can offer someone who reached the
    /// workspace by clicking a card in the switcher.
    pub fn action_label(&self, current_workspace: usize) -> String {
        match self.kind {
            Kind::Status => format!("⌘{}", self.workspace + 1),
            Kind::Activity if self.workspace == current_workspace => "OPEN".into(),
            Kind::Activity => "⌘⇧J JUMP".into(),
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
        self.push_kind(Kind::Activity, workspace, text, source, tab, now)
    }

    /// Post a status confirmation, replacing any status toast already up.
    ///
    /// Replacing rather than stacking matters: holding ⌘1/⌘2/⌘3 in sequence
    /// would otherwise leave three confirmations on screen, two of which are
    /// already wrong.
    pub fn push_status(&mut self, workspace: usize, text: impl Into<String>, source: impl Into<String>, tab: TabId, now: u64) -> u64 {
        self.items.retain(|n| n.kind != Kind::Status);
        self.push_kind(Kind::Status, workspace, text, source, tab, now)
    }

    fn push_kind(&mut self, kind: Kind, workspace: usize, text: impl Into<String>, source: impl Into<String>, tab: TabId, now: u64) -> u64 {
        self.seq += 1;
        self.items.push(Notification {
            id: self.seq,
            workspace,
            text: text.into(),
            source: source.into(),
            tab,
            at: now,
            kind,
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
        self.items.retain(|n| now.saturating_sub(n.at) < n.lifetime_ms());
        self.items.len() != before
    }

    /// Clear activity belonging to a workspace — called on arrival, since a
    /// notification about the place you are now standing is just noise.
    ///
    /// Leaves status toasts alone: the confirmation of *arriving here* is
    /// posted by the same switch that clears this workspace's activity, and
    /// clearing it too would delete the message as it was written.
    pub fn clear_workspace(&mut self, ws: usize) {
        self.items.retain(|n| n.workspace != ws || n.kind == Kind::Status);
    }

    /// A workspace was removed: drop its toasts, and renumber the ones from
    /// workspaces after it so they still point where they came from.
    pub fn forget_workspace(&mut self, ws: usize) {
        self.items.retain(|n| n.workspace != ws);
        for n in &mut self.items {
            if n.workspace > ws {
                n.workspace -= 1;
            }
        }
    }

    /// Newest actionable notification from a workspace other than the current
    /// one (⌘⇧J). Status toasts are never actionable.
    pub fn latest_elsewhere(&self, current: usize) -> Option<&Notification> {
        self.items
            .iter()
            .rev()
            .find(|n| n.kind == Kind::Activity && n.workspace != current)
    }
}

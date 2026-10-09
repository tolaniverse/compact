//! Bookmarks.
//!
//! Identity is the **host**, not the tab: bookmarking is a statement about a
//! place, and the tab showing it is incidental — it may be closed, or the same
//! site may be open twice. That is also what lets the address bar's star know
//! its state without tracking which tab produced the bookmark.

use crate::tab::TabMeta;
use crate::theme::Rgba;

/// Process-unique bookmark identifier.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct BookmarkId(pub u64);

#[derive(Clone, Debug)]
pub struct Bookmark {
    pub id: BookmarkId,
    pub title: String,
    pub host: String,
    /// Accent inherited from the workspace it was saved in, so the sidebar and
    /// strip chips carry a hint of where a link belongs.
    pub tint: Rgba,
}

impl Bookmark {
    /// First character of the title, for the square chip in the tab strip.
    pub fn initial(&self) -> String {
        self.title
            .chars()
            .next()
            .map(|c| c.to_uppercase().to_string())
            .unwrap_or_else(|| "?".into())
    }
}

#[derive(Debug, Default)]
pub struct Bookmarks {
    items: Vec<Bookmark>,
    seq: u64,
}

impl Bookmarks {
    /// The three the canvas ships with, tinted per workspace.
    pub fn seeded() -> Self {
        let mut b = Self::default();
        for (title, host, tint) in [
            ("Compact — Home", "example.com", crate::theme::WORKSPACE_DOTS[0]),
            ("Sprint board", "github.com", crate::theme::WORKSPACE_DOTS[1]),
            ("Renderer benchmarks", "browserbench.org", crate::theme::WORKSPACE_DOTS[2]),
        ] {
            b.add(title, host, tint);
        }
        b
    }

    pub fn items(&self) -> &[Bookmark] {
        &self.items
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn contains_host(&self, host: &str) -> bool {
        self.items.iter().any(|b| b.host == host)
    }

    pub fn get(&self, id: BookmarkId) -> Option<&Bookmark> {
        self.items.iter().find(|b| b.id == id)
    }

    fn add(&mut self, title: impl Into<String>, host: impl Into<String>, tint: Rgba) -> BookmarkId {
        self.seq += 1;
        let id = BookmarkId(self.seq);
        self.items.push(Bookmark {
            id,
            title: title.into(),
            host: host.into(),
            tint,
        });
        id
    }

    /// Add a bookmark read back from a state file.
    pub(crate) fn insert(&mut self, title: String, host: String, tint: Rgba) {
        if !self.contains_host(&host) {
            self.add(title, host, tint);
        }
    }

    pub fn remove(&mut self, id: BookmarkId) {
        self.items.retain(|b| b.id != id);
    }

    /// Save or unsave `tab`. Returns true if it is bookmarked afterwards.
    pub fn toggle(&mut self, tab: &TabMeta, tint: Rgba) -> bool {
        if self.contains_host(&tab.host) {
            self.items.retain(|b| b.host != tab.host);
            false
        } else {
            self.add(tab.title.clone(), tab.host.clone(), tint);
            true
        }
    }
}

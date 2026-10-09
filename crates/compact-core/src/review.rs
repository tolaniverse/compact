//! Review mode: comments you pin to a page for its agent.
//!
//! You turn review mode on in an agent's workspace (⌘⇧R), point at part of
//! the page, and write what should change — or drag out an empty area and say
//! what belongs there. Each comment carries enough about where it was pinned
//! ([`Anchor`]) for the agent to find the code behind it, and reaches the
//! agent the next time it asks for feedback.
//!
//! Comments belong to the agent workspace, not to a tab: a tab is reopened
//! with a new id after a restart, but the page and the conversation about it
//! are the same. A comment finds its page again by URL.

use crate::tab::TabId;
use crate::workspace::AgentInfo;
use crate::{App, Effect};

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct CommentId(pub u64);

impl CommentId {
    /// How a comment is named to people and agents: `c14`.
    pub fn label(self) -> String {
        format!("c{}", self.0)
    }

    pub fn parse(s: &str) -> Option<CommentId> {
        s.trim().trim_start_matches('c').parse().ok().map(CommentId)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CommentKind {
    /// Fix this.
    Change,
    /// Something new belongs in the area you drew.
    Add,
    /// The agent answers; no change expected.
    Question,
}

impl CommentKind {
    pub const fn slug(self) -> &'static str {
        match self {
            CommentKind::Change => "change",
            CommentKind::Add => "add",
            CommentKind::Question => "question",
        }
    }

    pub fn from_slug(s: &str) -> Option<Self> {
        [CommentKind::Change, CommentKind::Add, CommentKind::Question].into_iter().find(|k| k.slug() == s)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CommentState {
    Open,
    /// The agent says it is done; waiting for you to look.
    Addressed,
    Resolved,
}

impl CommentState {
    pub const fn slug(self) -> &'static str {
        match self {
            CommentState::Open => "open",
            CommentState::Addressed => "addressed",
            CommentState::Resolved => "resolved",
        }
    }

    pub fn from_slug(s: &str) -> Option<Self> {
        [CommentState::Open, CommentState::Addressed, CommentState::Resolved].into_iter().find(|k| k.slug() == s)
    }
}

/// Where a comment was pinned. Several independent clues, so that when one
/// stops matching after an edit the others still point the right way.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Anchor {
    pub url: String,
    /// A CSS selector for the element; empty for a drawn region.
    pub selector: String,
    /// The element's own text, cut short.
    pub text: String,
    /// Its role, as the snapshot names it.
    pub role: String,
    /// `data-*` attributes such as `data-testid`, as `name=value` pairs.
    pub data: String,
    /// x, y, width, height in CSS pixels, relative to the viewport at the
    /// moment you pinned it. For a drawn region, the region.
    pub rect: [f64; 4],
    /// The viewport's width and height then.
    pub viewport: [f64; 2],
    /// How far the page was scrolled then, so a drawn region can be put
    /// back where it was on the page rather than on the screen.
    pub scroll: [f64; 2],
    /// A few computed styles, `name: value; …`.
    pub styles: String,
    /// The cropped screenshot's file name, once it has been taken.
    pub crop: Option<String>,
}

/// One message in a comment's thread after the first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Remark {
    pub from_agent: bool,
    pub body: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Comment {
    pub id: CommentId,
    pub kind: CommentKind,
    pub state: CommentState,
    pub body: String,
    pub anchor: Anchor,
    pub replies: Vec<Remark>,
    /// Whether the agent has been handed this comment.
    pub delivered: bool,
}

/// The most comments one agent workspace keeps. Resolved ones go first.
pub const MAX_COMMENTS: usize = 500;

impl AgentInfo {
    pub fn comment(&self, id: CommentId) -> Option<&Comment> {
        self.comments.iter().find(|c| c.id == id)
    }
}

impl App {
    /// Turn review mode on or off for the tab in front (⌘⇧R).
    ///
    /// Only in an agent's workspace: a comment needs someone to read it.
    pub fn toggle_review(&mut self) -> Vec<Effect> {
        let ws = self.workspaces.current;
        let w = self.workspaces.current();
        let tab = w.active;
        let Some(agent) = &w.agent else {
            self.notifications.push_status(
                ws,
                "Review mode is for agent workspaces".to_string(),
                "Open the site in an agent's workspace to comment on it".to_string(),
                tab,
                self.clock,
            );
            return Vec::new();
        };
        let client = agent.client.clone();
        let mut fx = Vec::new();
        if let Some(prev) = self.review.take() {
            fx.push(Effect::SetReview(prev, false));
            if prev == tab {
                return fx;
            }
        }
        self.review = Some(tab);
        self.overlay = crate::Overlay::None;
        fx.push(Effect::SetReview(tab, true));
        self.notifications.push_status(
            ws,
            "Review mode".to_string(),
            format!("Click to comment for {client} · drag to mark a space · Esc to leave"),
            tab,
            self.clock,
        );
        fx
    }

    /// The page asked to leave review mode (Esc with nothing open).
    pub fn end_review(&mut self, tab: TabId) -> Vec<Effect> {
        if self.review == Some(tab) {
            self.review = None;
            return vec![Effect::SetReview(tab, false)];
        }
        Vec::new()
    }

    /// Pin a comment from tab `tab`. Returns its id, or `None` if the tab is
    /// not in an agent's workspace or the comment is empty.
    pub fn add_comment(&mut self, tab: TabId, kind: CommentKind, body: &str, anchor: Anchor) -> Option<CommentId> {
        let body = body.trim();
        if body.is_empty() {
            return None;
        }
        let (ws, _) = self.workspaces.locate(tab)?;
        self.comment_seq += 1;
        let id = CommentId(self.comment_seq);
        let agent = self.workspaces.spaces[ws].agent.as_mut()?;
        agent.comments.push(Comment {
            id,
            kind,
            state: CommentState::Open,
            body: body.to_string(),
            anchor,
            replies: Vec::new(),
            delivered: false,
        });
        trim_comments(&mut agent.comments);
        let client = agent.client.clone();
        self.notifications.push_status(
            ws,
            format!("Comment {} for {client}", id.label()),
            "Waiting for the agent to pick it up".to_string(),
            tab,
            self.clock,
        );
        Some(id)
    }

    /// Record the cropped screenshot taken for a comment.
    pub fn set_comment_crop(&mut self, id: CommentId, file: &str) {
        if let Some(c) = self.comment_mut(id) {
            c.anchor.crop = Some(file.to_string());
        }
    }

    fn comment_mut(&mut self, id: CommentId) -> Option<&mut Comment> {
        self.workspaces
            .spaces
            .iter_mut()
            .filter_map(|w| w.agent.as_mut())
            .flat_map(|a| a.comments.iter_mut())
            .find(|c| c.id == id)
    }

    /// The comments to pin on `tab`: those made on the page it shows that
    /// are not yet resolved.
    pub fn pins_for(&self, tab: TabId) -> Vec<&Comment> {
        let Some((ws, t)) = self.workspaces.locate(tab) else { return Vec::new() };
        let Some(a) = &self.workspaces.spaces[ws].agent else { return Vec::new() };
        let page = page_key(&t.url);
        a.comments
            .iter()
            .filter(|c| c.state != CommentState::Resolved && page_key(&c.anchor.url) == page)
            .collect()
    }

    /// Workspace `ws`'s comments, optionally only those in `state`.
    pub fn comments_in(&self, ws: usize, state: Option<CommentState>) -> Vec<&Comment> {
        self.workspaces
            .spaces
            .get(ws)
            .and_then(|w| w.agent.as_ref())
            .map(|a| a.comments.iter().filter(|c| state.is_none_or(|s| c.state == s)).collect())
            .unwrap_or_default()
    }

    /// Hand workspace `ws`'s agent the comments it has not seen yet, and mark
    /// them seen.
    pub fn take_undelivered(&mut self, ws: usize) -> Vec<Comment> {
        let Some(a) = self.workspaces.spaces.get_mut(ws).and_then(|w| w.agent.as_mut()) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for c in a.comments.iter_mut().filter(|c| !c.delivered && c.state == CommentState::Open) {
            c.delivered = true;
            out.push(c.clone());
        }
        out
    }

    /// Mark every open comment in workspace `ws` as seen by its agent, after
    /// it has read them some other way than waiting for them.
    pub fn mark_delivered(&mut self, ws: usize) {
        if let Some(a) = self.workspaces.spaces.get_mut(ws).and_then(|w| w.agent.as_mut()) {
            for c in a.comments.iter_mut().filter(|c| c.state == CommentState::Open) {
                c.delivered = true;
            }
        }
    }

    /// Workspace `ws`'s agent says `tab` is ready: tell you, and have the
    /// overlay ready when you get there.
    pub fn request_review(&mut self, ws: usize, tab: TabId, note: Option<&str>) -> Vec<Effect> {
        let Some(client) = self.workspaces.spaces.get(ws).and_then(|w| w.agent.as_ref()).map(|a| a.client.clone()) else {
            return Vec::new();
        };
        let mut fx = Vec::new();
        if let Some(prev) = self.review.replace(tab) {
            if prev != tab {
                fx.push(Effect::SetReview(prev, false));
            }
        }
        fx.push(Effect::SetReview(tab, true));
        let text = note.map(str::trim).filter(|n| !n.is_empty()).unwrap_or("Ready for review").to_string();
        let now = self.clock;
        self.post_activity(ws, text, format!("{client} · ⌘⇧R to leave review mode"), tab, now);
        fx
    }

    /// Whether workspace `ws` has comments its agent has not seen.
    pub fn has_undelivered(&self, ws: usize) -> bool {
        self.comments_in(ws, Some(CommentState::Open)).iter().any(|c| !c.delivered)
    }
}

/// A URL with its fragment dropped: `#section` does not make a new page.
pub fn page_key(url: &str) -> &str {
    url.split('#').next().unwrap_or(url).trim_end_matches('/')
}

/// Keep a workspace's comments bounded, dropping resolved ones oldest first,
/// then the oldest of the rest.
fn trim_comments(comments: &mut Vec<Comment>) {
    while comments.len() > MAX_COMMENTS {
        let i = comments.iter().position(|c| c.state == CommentState::Resolved).unwrap_or(0);
        comments.remove(i);
    }
}

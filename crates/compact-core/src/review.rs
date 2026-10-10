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
    /// Which framework reported the element's source: `react`, `vue`,
    /// `svelte`, `angular`, or empty when none did.
    pub framework: String,
    /// The component that rendered it, as the framework names it.
    pub component: String,
    /// `file:line:column` where it was written, as far as the dev build
    /// knows. The page's own claim; a lead to check, not a fact.
    pub source: String,
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

    /// Open or close the developer tools on the tab in front (⌘⌥I).
    ///
    /// Agent workspaces only for now: those are the tabs whose console and
    /// network activity Compact captures.
    pub fn toggle_devtools(&mut self) -> Vec<Effect> {
        let ws = self.workspaces.current;
        let w = self.workspaces.current();
        let tab = w.active;
        if !w.is_agent() {
            self.notifications.push_status(
                ws,
                "Developer tools are for agent workspaces".to_string(),
                "Their tabs are the ones whose console and network Compact records".to_string(),
                tab,
                self.clock,
            );
            return Vec::new();
        }
        let mut fx = Vec::new();
        if let Some(prev) = self.devtools.take() {
            fx.push(Effect::SetDevTools(prev, false, None));
            if prev == tab {
                return fx;
            }
        }
        self.devtools = Some(tab);
        self.overlay = crate::Overlay::None;
        fx.push(Effect::SetDevTools(tab, true, None));
        fx
    }

    /// Open the drawer on `tab` at entry `seq`, for workspace `ws`'s agent
    /// pointing you at something. Tells you why.
    pub fn show_in_devtools(&mut self, ws: usize, tab: TabId, seq: Option<u64>, note: Option<&str>) -> Vec<Effect> {
        let Some(client) = self.workspaces.spaces.get(ws).and_then(|w| w.agent.as_ref()).map(|a| a.client.clone()) else {
            return Vec::new();
        };
        let mut fx = Vec::new();
        if let Some(prev) = self.devtools.replace(tab) {
            if prev != tab {
                fx.push(Effect::SetDevTools(prev, false, None));
            }
        }
        fx.push(Effect::SetDevTools(tab, true, seq));
        let text = note.map(str::trim).filter(|n| !n.is_empty()).unwrap_or("Take a look in developer tools").to_string();
        let now = self.clock;
        self.post_activity(ws, text, format!("{client} · ⌘⌥I"), tab, now);
        fx
    }

    /// The drawer closed itself (its close button, or Esc).
    pub fn end_devtools(&mut self, tab: TabId) -> Vec<Effect> {
        if self.devtools == Some(tab) {
            self.devtools = None;
            return vec![Effect::SetDevTools(tab, false, None)];
        }
        Vec::new()
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

// ---- the conversation ----------------------------------------------------------

use crate::agent::AgentError;

impl App {
    /// The comment `id` in workspace `ws`, for its agent to act on.
    fn agent_comment(&mut self, ws: usize, id: CommentId) -> Result<(&mut Comment, String), AgentError> {
        let a = self
            .workspaces
            .spaces
            .get_mut(ws)
            .and_then(|w| w.agent.as_mut())
            .ok_or(AgentError::NotAgentWorkspace)?;
        let client = a.client.clone();
        let c = a.comments.iter_mut().find(|c| c.id == id).ok_or(AgentError::NoSuchComment(id))?;
        Ok((c, client))
    }

    /// The tab you would want to see a comment on: one showing its page,
    /// preferring the one under review.
    fn tab_for_page(&self, ws: usize, url: &str) -> Option<TabId> {
        let w = self.workspaces.spaces.get(ws)?;
        let page = page_key(url);
        let on_page: Vec<TabId> = w.tabs.iter().filter(|t| page_key(&t.url) == page).map(|t| t.id).collect();
        on_page.iter().copied().find(|t| Some(*t) == self.review).or_else(|| on_page.first().copied())
    }

    /// Workspace `ws`'s agent answers in a comment's thread. You hear about
    /// it, since a question was asked of it or it is asking one back.
    pub fn agent_reply_comment(&mut self, ws: usize, id: CommentId, body: &str) -> Result<(), AgentError> {
        let body = body.trim();
        if body.is_empty() {
            return Err(AgentError::Refused("a reply needs a body".into()));
        }
        let (c, client) = self.agent_comment(ws, id)?;
        c.replies.push(Remark { from_agent: true, body: body.to_string() });
        let url = c.anchor.url.clone();
        let tab = self.tab_for_page(ws, &url).unwrap_or(self.workspaces.spaces[ws].active);
        let now = self.clock;
        self.post_activity(ws, format!("{client} replied on {}: {body}", id.label()), client, tab, now);
        Ok(())
    }

    /// Workspace `ws`'s agent says comment `id` is dealt with. It waits for
    /// you to look: the page reloads so the change shows, and its pin pulses.
    pub fn mark_addressed(&mut self, ws: usize, id: CommentId, note: Option<&str>) -> Result<Vec<Effect>, AgentError> {
        let (c, client) = self.agent_comment(ws, id)?;
        if c.state == CommentState::Resolved {
            return Err(AgentError::Refused(format!("{} is already resolved", id.label())));
        }
        c.state = CommentState::Addressed;
        let note = note.map(str::trim).filter(|n| !n.is_empty());
        if let Some(n) = note {
            c.replies.push(Remark { from_agent: true, body: n.to_string() });
        }
        let url = c.anchor.url.clone();
        let mut fx = Vec::new();
        let tab = self.tab_for_page(ws, &url);
        if let Some(t) = tab {
            if self.workspaces.locate(t).is_some_and(|(_, m)| m.state.is_live()) {
                fx.push(Effect::Reload(t));
            }
        }
        let tab = tab.unwrap_or(self.workspaces.spaces[ws].active);
        let text = match note {
            Some(n) => format!("{client} addressed {}: {n}", id.label()),
            None => format!("{client} addressed {} — take a look", id.label()),
        };
        let now = self.clock;
        self.post_activity(ws, text, client, tab, now);
        Ok(fx)
    }

    /// You are happy with comment `id`. Its pin goes away.
    pub fn resolve_comment(&mut self, id: CommentId) -> bool {
        match self.comment_mut(id) {
            Some(c) if c.state != CommentState::Resolved => {
                c.state = CommentState::Resolved;
                true
            }
            _ => false,
        }
    }

    /// You answer in comment `id`'s thread. That reopens it and puts it back
    /// on the agent's list, so its next `wait_for_feedback` hands the whole
    /// thread over again.
    pub fn reply_comment(&mut self, id: CommentId, body: &str) -> bool {
        let body = body.trim();
        if body.is_empty() {
            return false;
        }
        let Some(c) = self.comment_mut(id) else { return false };
        c.replies.push(Remark { from_agent: false, body: body.to_string() });
        c.state = CommentState::Open;
        c.delivered = false;
        true
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

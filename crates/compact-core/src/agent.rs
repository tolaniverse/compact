//! What an agent may do in its workspace, and the rules that decide it.
//!
//! The protocol lives elsewhere (`compact-agent`, `compact-mcp`); this is the
//! part worth testing without a socket. Every request an agent makes ends up
//! as one of the methods here, and each one checks the same three things:
//! that the workspace is an agent's, that you have not taken control of it,
//! and that the tab it names is one of that workspace's own.
//!
//! **Where an agent may go.** Loopback (`localhost`, `127.0.0.1`,
//! `*.localhost`, `[::1]`) is always allowed — that is the agent's own dev
//! server. Anything else is asked about once per site and remembered for the
//! session. Schemes that reach past the web (`file:`, `javascript:`, `data:`
//! and the like) are refused outright.

use crate::page::PageKind;
use crate::tab::{TabId, TabMeta, TabState};
use crate::workspace::Control;
use crate::{App, Effect};

/// The most tabs one agent workspace may hold. An agent in a loop should hit
/// a wall, not your memory.
pub const MAX_AGENT_TABS: usize = 20;

/// An agent you allowed to work in a project, kept across restarts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Approval {
    pub client: String,
    pub project: String,
}

/// Whether an agent may open a URL.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UrlCheck {
    /// Go ahead; this is the URL to load.
    Allowed(String),
    /// You have to say yes to this site first.
    Ask { host: String, url: String },
    /// Never, whatever you say.
    Refused(String),
}

/// Why an agent's request was not carried out. The text is written for the
/// agent to read, so it says what to do next.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentError {
    NotAgentWorkspace,
    /// You have taken control; the agent waits.
    Paused { client: String },
    NoSuchTab(TabId),
    TooManyTabs,
    /// The site needs your approval first.
    NeedsApproval { host: String },
    NoSuchComment(crate::review::CommentId),
    Refused(String),
}

impl core::fmt::Display for AgentError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            AgentError::NotAgentWorkspace => write!(f, "this workspace does not belong to an agent"),
            AgentError::Paused { client } => write!(
                f,
                "the user has taken control of {client}'s workspace; wait for them to hand it back"
            ),
            AgentError::NoSuchTab(id) => write!(f, "no tab {} in this workspace; call list_tabs", tab_ref(*id)),
            AgentError::TooManyTabs => {
                write!(f, "this workspace already has {MAX_AGENT_TABS} tabs; close some with close_tab")
            }
            AgentError::NeedsApproval { host } => {
                write!(f, "the user has not allowed {host}; they were asked in Compact")
            }
            AgentError::NoSuchComment(id) => {
                write!(f, "no comment {} in this workspace; call get_comments", id.label())
            }
            AgentError::Refused(why) => write!(f, "{why}"),
        }
    }
}

/// How a tab is named to an agent: `t12`.
pub fn tab_ref(id: TabId) -> String {
    format!("t{}", id.0)
}

/// Read a tab name back. Accepts `t12` and `12`.
pub fn parse_tab_ref(s: &str) -> Option<TabId> {
    s.trim().trim_start_matches('t').parse().ok().map(TabId)
}

/// The host part of an `http(s)` URL, lowercased, without user info or port.
pub fn url_host(url: &str) -> Option<String> {
    let rest = url.strip_prefix("https://").or_else(|| url.strip_prefix("http://"))?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    // `http://localhost@evil.test/` goes to evil.test: the host is what
    // follows the last `@`, never what precedes it.
    let host_port = authority.rsplit('@').next().unwrap_or(authority);
    let host = if let Some(v6) = host_port.strip_prefix('[') {
        format!("[{}]", v6.split(']').next().unwrap_or(""))
    } else {
        host_port.split(':').next().unwrap_or(host_port).to_string()
    };
    (!host.is_empty()).then(|| host.to_ascii_lowercase())
}

/// Your own machine.
pub fn is_loopback(host: &str) -> bool {
    host == "localhost" || host.ends_with(".localhost") || host == "127.0.0.1" || host == "[::1]"
}

/// Schemes an agent may never open, whatever the user says.
const REFUSED_SCHEMES: [&str; 8] = ["file:", "javascript:", "data:", "blob:", "about:", "compact:", "ftp:", "view-source:"];

impl App {
    /// The workspace `client` uses for `project`, if it has one open.
    pub fn agent_workspace(&self, client: &str, project: &str) -> Option<usize> {
        self.workspaces.spaces.iter().position(|w| {
            w.agent.as_ref().is_some_and(|a| a.client == client && a.project == project)
        })
    }

    pub fn is_approved(&self, client: &str, project: &str) -> bool {
        self.approvals.iter().any(|a| a.client == client && a.project == project)
    }

    /// Remember that `client` may work in `project`.
    pub fn approve(&mut self, client: &str, project: &str) {
        if !self.is_approved(client, project) {
            self.approvals.push(Approval { client: client.into(), project: project.into() });
        }
    }

    /// Take away workspace `ws`'s agent's approval and close the workspace.
    /// Its next request is refused and it has to be allowed again.
    pub fn revoke_agent(&mut self, ws: usize) -> Vec<Effect> {
        let Some(a) = self.workspaces.spaces.get(ws).and_then(|w| w.agent.clone()) else {
            return Vec::new();
        };
        self.approvals.retain(|x| !(x.client == a.client && x.project == a.project));
        self.close_agent_workspace(ws)
    }

    /// Decide whether workspace `ws`'s agent may open `input`, which may be a
    /// bare host like `localhost:3000` as well as a full URL.
    pub fn check_url(&self, ws: usize, input: &str) -> UrlCheck {
        let raw = input.trim();
        let lower = raw.to_ascii_lowercase();
        if let Some(s) = REFUSED_SCHEMES.iter().find(|s| lower.starts_with(**s)) {
            return UrlCheck::Refused(format!("{s} URLs are not available to agents"));
        }
        if let Some((scheme, _)) = lower.split_once("://") {
            if scheme != "http" && scheme != "https" {
                return UrlCheck::Refused(format!("{scheme}: URLs are not available to agents"));
            }
        }
        let Some(target) = crate::parse_address(raw) else {
            return UrlCheck::Refused("an empty address".into());
        };
        let Some(host) = url_host(&target.url) else {
            return UrlCheck::Refused(format!("not a usable address: {raw}"));
        };
        let known = self
            .workspaces
            .spaces
            .get(ws)
            .and_then(|w| w.agent.as_ref())
            .is_some_and(|a| a.hosts.contains(&host));
        if is_loopback(&host) || known {
            UrlCheck::Allowed(target.url)
        } else {
            UrlCheck::Ask { host, url: target.url }
        }
    }

    /// Record whether `client`'s agent for `project` is connected. Returns
    /// whether that changed anything.
    pub fn set_agent_connected(&mut self, client: &str, project: &str, connected: bool) -> bool {
        let Some(ws) = self.agent_workspace(client, project) else { return false };
        let Some(a) = self.workspaces.spaces[ws].agent.as_mut() else { return false };
        let changed = a.connected != connected;
        a.connected = connected;
        changed
    }

    /// You said yes to `host` for workspace `ws`'s agent, for this session.
    pub fn allow_host(&mut self, ws: usize, host: &str) {
        if let Some(a) = self.workspaces.spaces.get_mut(ws).and_then(|w| w.agent.as_mut()) {
            if !a.hosts.iter().any(|h| h == host) {
                a.hosts.push(host.to_string());
            }
        }
    }

    /// The checks every agent request starts with.
    fn agent_ready(&self, ws: usize) -> Result<(), AgentError> {
        let a = self
            .workspaces
            .spaces
            .get(ws)
            .and_then(|w| w.agent.as_ref())
            .ok_or(AgentError::NotAgentWorkspace)?;
        match a.control {
            Control::Agent => Ok(()),
            Control::User => Err(AgentError::Paused { client: a.client.clone() }),
        }
    }

    fn agent_owns(&self, ws: usize, tab: TabId) -> Result<(), AgentError> {
        let owns = self.workspaces.spaces.get(ws).is_some_and(|w| w.tab(tab).is_some());
        owns.then_some(()).ok_or(AgentError::NoSuchTab(tab))
    }

    /// Resolve `input` to a URL the agent may load now.
    fn agent_url(&self, ws: usize, input: &str) -> Result<String, AgentError> {
        match self.check_url(ws, input) {
            UrlCheck::Allowed(url) => Ok(url),
            UrlCheck::Ask { host, .. } => Err(AgentError::NeedsApproval { host }),
            UrlCheck::Refused(why) => Err(AgentError::Refused(why)),
        }
    }

    /// Workspace `ws`'s tabs, as its agent sees them.
    pub fn agent_tabs(&self, ws: usize) -> Result<Vec<&TabMeta>, AgentError> {
        self.workspaces
            .spaces
            .get(ws)
            .filter(|w| w.is_agent())
            .map(|w| w.tabs.iter().collect())
            .ok_or(AgentError::NotAgentWorkspace)
    }

    /// Open `input` in a new tab in workspace `ws`.
    pub fn agent_open(&mut self, ws: usize, input: &str) -> Result<(TabId, Vec<Effect>), AgentError> {
        self.agent_ready(ws)?;
        let url = self.agent_url(ws, input)?;
        let w = &self.workspaces.spaces[ws];
        // A workspace starts on a new-tab page; the agent's first page takes
        // its place rather than leaving an empty tab behind.
        let blank = (w.tabs.len() == 1 && w.tabs[0].kind == PageKind::NewTab).then(|| w.tabs[0].id);
        if blank.is_none() && w.tabs.len() >= MAX_AGENT_TABS {
            return Err(AgentError::TooManyTabs);
        }
        let host = url_host(&url).unwrap_or_default();
        let mut fx = Vec::new();
        let id = match blank {
            Some(id) => {
                self.workspaces.set_location(id, &host, &host, &url);
                self.workspaces.focus_in(ws, id);
                id
            }
            None => self
                .workspaces
                .new_tab_in(ws, &host, &host, &url)
                .ok_or(AgentError::NotAgentWorkspace)?,
        };
        self.workspaces.set_state(id, TabState::Live);
        fx.push(Effect::Load(id, url));
        self.reconcile_into(&mut fx);
        Ok((id, fx))
    }

    /// Point an existing tab somewhere else.
    pub fn agent_navigate(&mut self, ws: usize, tab: TabId, input: &str) -> Result<Vec<Effect>, AgentError> {
        self.agent_ready(ws)?;
        self.agent_owns(ws, tab)?;
        let url = self.agent_url(ws, input)?;
        let host = url_host(&url).unwrap_or_default();
        self.workspaces.set_location(tab, &host, &host, &url);
        self.workspaces.focus_in(ws, tab);
        self.workspaces.set_state(tab, TabState::Live);
        let mut fx = vec![Effect::Load(tab, url)];
        self.reconcile_into(&mut fx);
        Ok(fx)
    }

    /// Reload a tab, bringing it back first if it was asleep.
    pub fn agent_reload(&mut self, ws: usize, tab: TabId) -> Result<Vec<Effect>, AgentError> {
        self.agent_ready(ws)?;
        self.agent_owns(ws, tab)?;
        let was_live = self.workspaces.locate(tab).is_some_and(|(_, t)| t.state.is_live());
        let mut fx = self.agent_wake(ws, tab)?;
        if was_live {
            fx.push(Effect::Reload(tab));
        }
        Ok(fx)
    }

    /// Close a tab. The workspace keeps at least one, as yours do.
    pub fn agent_close(&mut self, ws: usize, tab: TabId) -> Result<Vec<Effect>, AgentError> {
        self.agent_ready(ws)?;
        self.agent_owns(ws, tab)?;
        let w = &mut self.workspaces.spaces[ws];
        if w.tabs.len() <= 1 {
            return Err(AgentError::Refused("the last tab in a workspace cannot be closed".into()));
        }
        let idx = w.tabs.iter().position(|t| t.id == tab).ok_or(AgentError::NoSuchTab(tab))?;
        w.tabs.remove(idx);
        if w.active == tab {
            w.active = w.tabs[idx.saturating_sub(1)].id;
        }
        let mut fx = vec![Effect::Destroy(tab)];
        self.reconcile_into(&mut fx);
        Ok(fx)
    }

    /// Make sure a tab is live so it can be looked at, and make it the one
    /// the agent is working in.
    ///
    /// Looking does not need [`Control::Agent`]: an agent may still read a
    /// page while you drive, it just may not change it.
    pub fn agent_wake(&mut self, ws: usize, tab: TabId) -> Result<Vec<Effect>, AgentError> {
        if !self.workspaces.spaces.get(ws).is_some_and(|w| w.is_agent()) {
            return Err(AgentError::NotAgentWorkspace);
        }
        self.agent_owns(ws, tab)?;
        self.workspaces.focus_in(ws, tab);
        let mut fx = Vec::new();
        if let Some(e) = self.activation_effect(tab) {
            fx.push(e);
        }
        self.workspaces.set_state(tab, TabState::Live);
        self.reconcile_into(&mut fx);
        Ok(fx)
    }
}

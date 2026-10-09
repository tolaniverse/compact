//! Workspaces: independent tab sets with their own active tab and unread count.
//!
//! Tab mutation semantics follow the canvas exactly, including the parts that
//! look like edge cases but are load-bearing: a workspace never drops to zero
//! tabs, and closing the active tab focuses its left neighbour rather than
//! jumping to the end.

use crate::page::{ErrorKind, PageKind};
use crate::persist::SavedSpace;
use crate::tab::{TabId, TabIdGen, TabMeta, TabState};
use crate::theme::{Rgba, WORKSPACE_DOTS};

#[derive(Clone, Debug)]
pub struct Workspace {
    pub name: String,
    pub dot: Rgba,
    pub tabs: Vec<TabMeta>,
    pub active: TabId,
    pub unread: u32,
    /// Set when an outside agent owns this workspace. Drawn with a robot in
    /// place of the colour dot, so it is never mistaken for one of yours.
    pub agent: Option<AgentInfo>,
}

/// Who is driving an agent workspace's tabs right now.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Control {
    Agent,
    /// You took over. The agent's actions wait until you hand back.
    User,
}

/// The agent behind an agent workspace.
#[derive(Clone, Debug, PartialEq)]
pub struct AgentInfo {
    /// The name the client introduced itself with: "Claude Code", "opencode".
    pub client: String,
    /// The directory the agent works in. With `client`, this is what a
    /// reconnecting agent is matched on, so it gets its own tabs back.
    pub project: String,
    pub control: Control,
    /// Sites beyond loopback you let this agent open, this session. Not
    /// saved: a restart asks again.
    pub hosts: Vec<String>,
    /// Whether the agent is connected right now. While it is, the tab it is
    /// working in is kept live like the one you are looking at. Not saved.
    pub connected: bool,
    /// What you said about its pages in review mode.
    pub comments: Vec<crate::review::Comment>,
}

impl AgentInfo {
    /// The key the engine's website data store is kept under. Stable across a
    /// reconnect, so an agent finds the logins it made earlier in the session.
    pub fn profile_key(&self) -> String {
        format!("{}\u{1f}{}", self.client, self.project)
    }
}

/// Whose cookies and storage a tab uses.
///
/// Every agent workspace has its own, so an agent never sees what you are
/// logged in to, and two agents never see each other's.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Profile {
    /// Yours: the default, persistent website data store.
    Personal,
    /// An agent workspace's, by [`AgentInfo::profile_key`].
    Agent(String),
}

/// `"Claude Code's workspace"` — an agent workspace is named after its agent.
pub fn agent_workspace_name(client: &str) -> String {
    let client = client.trim();
    let client = if client.is_empty() { "Agent" } else { client };
    if client.ends_with('s') || client.ends_with('S') {
        format!("{client}' workspace")
    } else {
        format!("{client}'s workspace")
    }
}

impl Workspace {
    pub fn active_index(&self) -> usize {
        self.tabs.iter().position(|t| t.id == self.active).unwrap_or(0)
    }

    pub fn active_tab(&self) -> &TabMeta {
        let i = self.active_index();
        &self.tabs[i]
    }

    pub fn is_agent(&self) -> bool {
        self.agent.is_some()
    }

    pub fn tab(&self, id: TabId) -> Option<&TabMeta> {
        self.tabs.iter().find(|t| t.id == id)
    }

    pub fn tab_mut(&mut self, id: TabId) -> Option<&mut TabMeta> {
        self.tabs.iter_mut().find(|t| t.id == id)
    }

    /// `"4 tabs"` — the count readout in the sidebar header and switcher cards.
    pub fn count_label(&self) -> String {
        match self.tabs.len() {
            1 => "1 tab".to_string(),
            n => format!("{n} tabs"),
        }
    }
}

/// All workspaces plus which one is showing.
#[derive(Debug)]
pub struct WorkspaceSet {
    pub spaces: Vec<Workspace>,
    pub current: usize,
    ids: TabIdGen,
    /// Logical activation clock backing [`TabMeta::last_active`].
    clock: u64,
}

impl WorkspaceSet {
    /// The three workspaces the canvas ships with, populated with its tabs.
    pub fn seeded() -> Self {
        let mut ids = TabIdGen::new();
        let mut clock = 0u64;
        // Titles come from the canvas; hosts are real. The canvas's own hosts
        // (`compact.app`, `board.internal`, `figma.local`) are placeholders
        // that do not resolve, so a build using them would open to a blank
        // page. Titles here are provisional anyway — the shell replaces each
        // one with the real page title once its navigation commits.
        let seed: [(&str, &[(&str, &str)]); 3] = [
            (
                "Personal",
                &[
                    ("Compact — Home", "example.com"),
                    ("Keyboard-first browsing", "developer.mozilla.org/en-US/docs/Web/API/KeyboardEvent"),
                    ("Reading list", "en.wikipedia.org/wiki/Web_browser"),
                ],
            ),
            (
                "Work",
                &[
                    ("Sprint board", "github.com"),
                    ("Perf budget notes", "web.dev"),
                    ("Release checklist", "example.org"),
                    ("Design review", "developer.apple.com/design"),
                ],
            ),
            (
                "Research",
                &[
                    ("Memory profiling", "webkit.org"),
                    ("Renderer benchmarks", "browserbench.org"),
                ],
            ),
        ];

        // The canvas focuses the second Personal tab and the first of the others.
        let active_seed = [1usize, 0, 0];

        let spaces = seed
            .iter()
            .enumerate()
            .map(|(wi, (name, tabs))| {
                let tabs: Vec<TabMeta> = tabs
                    .iter()
                    .map(|(title, host)| {
                        clock += 1;
                        let mut t = TabMeta::new(ids.next(), *title, *host);
                        t.last_active = clock;
                        t
                    })
                    .collect();
                Workspace {
                    name: (*name).to_string(),
                    dot: WORKSPACE_DOTS[wi % WORKSPACE_DOTS.len()],
                    active: tabs[active_seed[wi].min(tabs.len() - 1)].id,
                    tabs,
                    unread: 0,
                    agent: None,
                }
            })
            .collect();

        Self { spaces, current: 0, ids, clock }
    }

    /// What a first launch opens on: the canvas's three workspaces, each with
    /// nothing but a new-tab page. The names stay because `⌘1–3`, the switcher
    /// and the notification settings are all built around three workspaces,
    /// and there is no way to create one yet.
    pub fn fresh() -> Self {
        let spaces: Vec<SavedSpace> = ["Personal", "Work", "Research"]
            .into_iter()
            .map(|name| SavedSpace {
                name: name.to_string(),
                active: 0,
                tabs: vec![crate::persist::new_tab_entry()],
                agent: None,
            })
            .collect();
        Self::from_saved(&spaces, 0)
    }

    /// Rebuild from a state file. Every tab gets a new id and comes back
    /// `Cold`; out-of-range indices are clamped rather than trusted.
    ///
    /// `spaces` must be non-empty and every workspace must hold a tab — the
    /// decoder guarantees both.
    pub fn from_saved(spaces: &[SavedSpace], current: usize) -> Self {
        let mut ids = TabIdGen::new();
        let mut clock = 0u64;
        let spaces: Vec<Workspace> = spaces
            .iter()
            .enumerate()
            .map(|(wi, s)| {
                let mut tabs: Vec<TabMeta> = s
                    .tabs
                    .iter()
                    .map(|saved| {
                        clock += 1;
                        let mut t = if saved.kind.is_internal() {
                            TabMeta::internal(ids.next(), saved.kind)
                        } else {
                            TabMeta::new(ids.next(), saved.title.clone(), saved.host.clone())
                        };
                        if !saved.kind.is_internal() {
                            t.url = saved.url.clone();
                        }
                        t.last_active = clock;
                        t
                    })
                    .collect();
                let active_idx = s.active.min(tabs.len() - 1);
                // The tab that was in front is the most recently used one, so
                // the eviction policy ranks it accordingly on the first pass.
                clock += 1;
                tabs[active_idx].last_active = clock;
                Workspace {
                    name: s.name.clone(),
                    dot: WORKSPACE_DOTS[wi % WORKSPACE_DOTS.len()],
                    active: tabs[active_idx].id,
                    tabs,
                    unread: 0,
                    agent: s.agent.clone(),
                }
            })
            .collect();
        let current = current.min(spaces.len() - 1);
        Self { spaces, current, ids, clock }
    }

    pub fn current(&self) -> &Workspace {
        &self.spaces[self.current]
    }

    pub fn current_mut(&mut self) -> &mut Workspace {
        &mut self.spaces[self.current]
    }

    /// Find a tab anywhere, returning which workspace holds it.
    pub fn locate(&self, id: TabId) -> Option<(usize, &TabMeta)> {
        self.spaces
            .iter()
            .enumerate()
            .find_map(|(i, w)| w.tab(id).map(|t| (i, t)))
    }

    /// Bump a tab to the front of the LRU order and make it active.
    pub fn select(&mut self, id: TabId) {
        self.clock += 1;
        let clock = self.clock;
        let ws = self.current_mut();
        if let Some(t) = ws.tab_mut(id) {
            t.last_active = clock;
            ws.active = id;
        }
    }

    /// Open a tab in workspace `ws` for its agent and make it that
    /// workspace's active tab. Focus moves only if you are looking at `ws`.
    pub fn new_tab_in(&mut self, ws: usize, title: &str, host: &str, url: &str) -> Option<TabId> {
        let id = self.ids.next();
        self.clock += 1;
        let clock = self.clock;
        let w = self.spaces.get_mut(ws)?;
        let mut tab = TabMeta::new(id, title, host);
        tab.url = url.to_string();
        tab.last_active = clock;
        w.tabs.push(tab);
        w.active = id;
        Some(id)
    }

    /// Make `id` workspace `ws`'s active tab and the most recently used.
    pub fn focus_in(&mut self, ws: usize, id: TabId) {
        self.clock += 1;
        let clock = self.clock;
        if let Some(w) = self.spaces.get_mut(ws) {
            if let Some(t) = w.tab_mut(id) {
                t.last_active = clock;
                w.active = id;
            }
        }
    }

    /// Append a blank tab and focus it (⌘T).
    pub fn new_tab(&mut self) -> TabId {
        let id = self.ids.next();
        self.clock += 1;
        let mut tab = TabMeta::internal(id, PageKind::NewTab);
        tab.last_active = self.clock;
        let ws = self.current_mut();
        ws.tabs.push(tab);
        ws.active = id;
        id
    }

    /// Close a tab (⌘W). Refuses to empty a workspace, matching the canvas —
    /// a workspace with no tabs has no page to render and no address to show.
    /// Returns the id actually removed.
    pub fn close(&mut self, id: TabId) -> Option<TabId> {
        let ws = self.current_mut();
        if ws.tabs.len() <= 1 {
            return None;
        }
        let idx = ws.tabs.iter().position(|t| t.id == id)?;
        ws.tabs.remove(idx);
        if ws.active == id {
            // Focus the left neighbour, not the list head.
            ws.active = ws.tabs[idx.saturating_sub(1)].id;
        }
        Some(id)
    }

    /// Switch workspaces, optionally focusing a specific tab on arrival (used
    /// by ⌘⇧J, which jumps straight to the tab a notification came from).
    /// Returns the direction of travel for the slide animation, or `None` if
    /// the switch was a no-op.
    pub fn switch(&mut self, to: usize, focus: Option<TabId>) -> Option<i8> {
        if to >= self.spaces.len() || to == self.current {
            return None;
        }
        let dir = if to > self.current { 1i8 } else { -1 };
        self.current = to;
        self.spaces[to].unread = 0;
        if let Some(f) = focus {
            if self.spaces[to].tab(f).is_some() {
                self.select(f);
            }
        }
        Some(dir)
    }

    /// Every tab across every workspace — what the policy sees.
    ///
    /// Live tabs in background workspaces are as expensive as foreground ones,
    /// so the budget has to be global. Scoping eviction per-workspace would let
    /// three workspaces each hold a "reasonable" live set and blow the machine.
    pub fn all_tabs(&self) -> Vec<TabMeta> {
        self.spaces.iter().flat_map(|w| w.tabs.iter().cloned()).collect()
    }

    /// Apply a state transition to a tab wherever it lives.
    pub fn set_state(&mut self, id: TabId, state: TabState) {
        for w in &mut self.spaces {
            if let Some(t) = w.tab_mut(id) {
                t.state = state;
                if !state.is_live() {
                    t.footprint = None;
                }
                return;
            }
        }
    }

    pub fn set_footprint(&mut self, id: TabId, bytes: u64) {
        for w in &mut self.spaces {
            if let Some(t) = w.tab_mut(id) {
                t.footprint = Some(bytes);
                return;
            }
        }
    }

    /// Update title and host after a navigation commits.
    ///
    /// A navigation always lands on the web: any tab that was showing one of
    /// Compact's own pages stops being internal here, which is what keeps the
    /// error page from re-rendering over the site you just retried.
    pub fn set_location(&mut self, id: TabId, title: &str, host: &str, url: &str) {
        for w in &mut self.spaces {
            if let Some(t) = w.tab_mut(id) {
                t.title = title.to_string();
                t.host = host.to_string();
                t.url = url.to_string();
                t.kind = PageKind::Web;
                t.error = None;
                return;
            }
        }
    }

    /// Turn a tab into one of Compact's own pages.
    ///
    /// The failed host is deliberately preserved for an error page: it is what
    /// the page's header reads out, and what ⌘R retries.
    pub fn set_page(&mut self, id: TabId, kind: PageKind, error: Option<ErrorKind>) {
        for w in &mut self.spaces {
            if let Some(t) = w.tab_mut(id) {
                t.kind = kind;
                t.error = error;
                match kind {
                    PageKind::NewTab => {
                        t.title = "New tab".into();
                        t.host = crate::page::NEW_TAB_URL.into();
                        t.url = crate::page::NEW_TAB_URL.into();
                    }
                    PageKind::Settings => {
                        t.title = "Settings".into();
                        t.host = crate::page::SETTINGS_URL.into();
                        t.url = crate::page::SETTINGS_URL.into();
                    }
                    PageKind::Error => {
                        if let Some(e) = error {
                            t.title = e.spec().name.into();
                        }
                    }
                    PageKind::Web => {}
                }
                return;
            }
        }
    }

    pub fn add_unread(&mut self, ws: usize) {
        if let Some(w) = self.spaces.get_mut(ws) {
            w.unread += 1;
        }
    }

    /// Find the workspace `client` uses for `project`, or open one for it.
    ///
    /// Agent workspaces always sit after yours, so `⌘1–3` keep meaning what
    /// they meant. Returns the workspace's index and whether it is new.
    pub fn open_agent(&mut self, client: &str, project: &str) -> (usize, bool) {
        let existing = self.spaces.iter().position(|w| {
            w.agent
                .as_ref()
                .is_some_and(|a| a.client == client && a.project == project)
        });
        if let Some(i) = existing {
            return (i, false);
        }
        let id = self.ids.next();
        self.clock += 1;
        let mut tab = TabMeta::internal(id, PageKind::NewTab);
        tab.last_active = self.clock;
        let n = self.spaces.len();
        self.spaces.push(Workspace {
            name: agent_workspace_name(client),
            dot: WORKSPACE_DOTS[n % WORKSPACE_DOTS.len()],
            active: id,
            tabs: vec![tab],
            unread: 0,
            agent: Some(AgentInfo {
                client: client.to_string(),
                project: project.to_string(),
                control: Control::Agent,
                hosts: Vec::new(),
                connected: false,
                comments: Vec::new(),
            }),
        });
        (n, true)
    }

    /// Remove an agent workspace, returning the tabs that went with it.
    ///
    /// Your own workspaces cannot be removed this way: the keyboard, the
    /// switcher and the settings page are all built around them existing.
    pub fn close_agent(&mut self, index: usize) -> Option<Vec<TabId>> {
        if !self.spaces.get(index)?.is_agent() {
            return None;
        }
        let gone = self.spaces.remove(index);
        if self.current == index {
            self.current = 0;
        } else if self.current > index {
            self.current -= 1;
        }
        Some(gone.tabs.iter().map(|t| t.id).collect())
    }

    /// Which website data store `id` must be built with.
    pub fn profile_of(&self, id: TabId) -> Profile {
        self.locate(id)
            .and_then(|(i, _)| self.spaces[i].agent.as_ref())
            .map_or(Profile::Personal, |a| Profile::Agent(a.profile_key()))
    }

    /// The tab each connected agent is working in, unless you have taken
    /// over its workspace.
    pub fn agent_working_tabs(&self) -> Vec<TabId> {
        self.spaces
            .iter()
            .filter(|w| w.agent.as_ref().is_some_and(|a| a.connected && a.control == Control::Agent))
            .map(|w| w.active)
            .collect()
    }

    /// Every tab that belongs to an agent workspace — what the policy ranks
    /// below yours.
    pub fn agent_tabs(&self) -> Vec<TabId> {
        self.spaces
            .iter()
            .filter(|w| w.is_agent())
            .flat_map(|w| w.tabs.iter().map(|t| t.id))
            .collect()
    }

    /// Workspaces other than the current one that have unread activity — the
    /// sidebar's `ELSEWHERE` section.
    pub fn elsewhere(&self) -> Vec<(usize, &Workspace)> {
        self.spaces
            .iter()
            .enumerate()
            .filter(|(i, w)| *i != self.current && w.unread > 0)
            .collect()
    }
}

//! Workspaces: independent tab sets with their own active tab and unread count.
//!
//! Tab mutation semantics follow the canvas exactly, including the parts that
//! look like edge cases but are load-bearing: a workspace never drops to zero
//! tabs, and closing the active tab focuses its left neighbour rather than
//! jumping to the end.

use crate::page::{ErrorKind, PageKind};
use crate::tab::{TabId, TabIdGen, TabMeta, TabState};
use crate::theme::{Rgba, WORKSPACE_DOTS};

#[derive(Clone, Debug)]
pub struct Workspace {
    pub name: String,
    pub dot: Rgba,
    pub tabs: Vec<TabMeta>,
    pub active: TabId,
    pub unread: u32,
}

impl Workspace {
    pub fn active_index(&self) -> usize {
        self.tabs.iter().position(|t| t.id == self.active).unwrap_or(0)
    }

    pub fn active_tab(&self) -> &TabMeta {
        let i = self.active_index();
        &self.tabs[i]
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
                }
            })
            .collect();

        Self { spaces, current: 0, ids, clock }
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

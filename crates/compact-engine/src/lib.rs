//! The web engine seam.
//!
//! [`TabEngine`] is the single boundary between Compact's platform-free core
//! and a real web engine. Everything below it is platform code; everything
//! above it is testable without WebKit.
//!
//! The trait is shaped around **hibernation**, not around navigation, because
//! hibernation is the thing Compact exists to do and the thing platforms
//! disagree about. That disagreement is exposed rather than hidden — see
//! [`Hibernation`].

use compact_core::tab::TabId;
use std::fmt;

pub mod capture;
pub mod inspect;
pub mod review;

#[cfg(target_os = "macos")]
pub mod bridge;
#[cfg(target_os = "macos")]
pub mod macos;

#[cfg(target_os = "macos")]
pub use bridge::{drain_events, is_internal_origin, on_event, EngineEvent};
#[cfg(target_os = "macos")]
pub use macos::WebKitEngine;

/// How a platform can put a tab to sleep.
///
/// This is public and explicit because the platforms genuinely differ, and a
/// trait that pretended otherwise would be lying to the policy layer:
///
/// - **macOS** and **Linux** serialize to a blob (`WKWebView.interactionState`,
///   `webkit_web_view_get_session_state`). The web view is destroyed outright
///   and the tab costs kilobytes — 2.3 KB, measured.
/// - **Windows** has no blob. WebView2 offers `TrySuspend`, which parks the
///   renderer and drops its memory-usage target but keeps the object alive, so
///   a suspended tab still costs more than a serialized one.
///
/// The policy layer reads this to know whether "hibernated" means *gone* or
/// merely *quiet*.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Hibernation {
    /// Session serialized; web view destroyed. Cost per tab: kilobytes.
    Blob,
    /// Renderer suspended; object retained. Cost per tab: megabytes.
    Suspend,
}

#[derive(Debug)]
pub enum EngineError {
    /// No tab with this id is known to the engine.
    UnknownTab(TabId),
    /// Asked to restore a tab with no stored session.
    NoSession(TabId),
    /// A URL the platform would not accept.
    BadUrl(String),
    /// The engine must be driven from the main thread and was not.
    WrongThread,
}

impl fmt::Display for EngineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EngineError::UnknownTab(id) => write!(f, "{id} is not known to the engine"),
            EngineError::NoSession(id) => write!(f, "{id} has no stored session to restore"),
            EngineError::BadUrl(u) => write!(f, "not a usable URL: {u}"),
            EngineError::WrongThread => write!(f, "engine called off the main thread"),
        }
    }
}

impl std::error::Error for EngineError {}

pub type Result<T> = std::result::Result<T, EngineError>;

/// Backing store for one browser's worth of tabs.
///
/// Implementors own every live web view. The core holds only [`TabId`]s, so the
/// set of things that can leak a web view is exactly the set of implementors of
/// this trait.
pub trait TabEngine {
    fn capability(&self) -> Hibernation;

    /// Create (or reuse) a live view for `id` and navigate it to `url`.
    fn load(&mut self, id: TabId, url: &str) -> Result<()>;

    /// Serialize `id`'s session and release its live resources.
    ///
    /// Returns the serialized session size in bytes — the number the 50 KB
    /// hibernation gate is checked against.
    fn hibernate(&mut self, id: TabId) -> Result<usize>;

    /// Rebuild a live view for `id` from its stored session, restoring scroll
    /// position and back/forward history.
    fn restore(&mut self, id: TabId) -> Result<()>;

    /// Drop everything for `id`: live view and stored session alike.
    fn destroy(&mut self, id: TabId);

    fn is_live(&self, id: TabId) -> bool;

    /// Number of live views. The invariant worth asserting in tests: after
    /// hibernating everything, this is zero.
    fn live_count(&self) -> usize;

    /// Number of stored sessions.
    fn session_count(&self) -> usize;

    /// Total bytes of all stored sessions — the aggregate hibernation cost.
    fn session_bytes(&self) -> usize;
}

/// An in-memory `TabEngine` for tests, with no WebKit involved.
///
/// Exists so the policy and app layers can be exercised end to end on any
/// platform and in CI, and so tests can assert the *shape* of engine traffic
/// (what got hibernated, in what order) rather than just its effects.
#[derive(Debug, Default)]
pub struct FakeEngine {
    live: Vec<TabId>,
    sessions: Vec<(TabId, usize)>,
    /// Every call, in order — the assertion surface for tests.
    pub log: Vec<String>,
    /// Bytes each fake session claims to occupy.
    pub session_size: usize,
}

impl FakeEngine {
    pub fn new() -> Self {
        Self { session_size: 2_355, ..Default::default() }
    }
}

impl TabEngine for FakeEngine {
    fn capability(&self) -> Hibernation {
        Hibernation::Blob
    }

    fn load(&mut self, id: TabId, url: &str) -> Result<()> {
        self.log.push(format!("load {id} {url}"));
        if !self.live.contains(&id) {
            self.live.push(id);
        }
        Ok(())
    }

    fn hibernate(&mut self, id: TabId) -> Result<usize> {
        if !self.live.contains(&id) {
            return Err(EngineError::UnknownTab(id));
        }
        self.log.push(format!("hibernate {id}"));
        self.live.retain(|t| *t != id);
        self.sessions.retain(|(t, _)| *t != id);
        self.sessions.push((id, self.session_size));
        Ok(self.session_size)
    }

    fn restore(&mut self, id: TabId) -> Result<()> {
        if !self.sessions.iter().any(|(t, _)| *t == id) {
            return Err(EngineError::NoSession(id));
        }
        self.log.push(format!("restore {id}"));
        self.sessions.retain(|(t, _)| *t != id);
        self.live.push(id);
        Ok(())
    }

    fn destroy(&mut self, id: TabId) {
        self.log.push(format!("destroy {id}"));
        self.live.retain(|t| *t != id);
        self.sessions.retain(|(t, _)| *t != id);
    }

    fn is_live(&self, id: TabId) -> bool {
        self.live.contains(&id)
    }

    fn live_count(&self) -> usize {
        self.live.len()
    }

    fn session_count(&self) -> usize {
        self.sessions.len()
    }

    fn session_bytes(&self) -> usize {
        self.sessions.iter().map(|(_, b)| b).sum()
    }
}

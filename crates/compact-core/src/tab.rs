//! Tab identity and metadata.
//!
//! Nothing here holds an engine handle. A tab is a record; whether it is
//! *currently backed by a live web view* is a question only the engine host can
//! answer, and it answers it with a map lookup keyed by [`TabId`]. That split is
//! deliberate: eviction cannot leak a web view if the tab model was never able
//! to hold one.

use core::fmt;

/// Opaque, stable, process-unique tab identifier.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct TabId(pub u64);

impl fmt::Display for TabId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "tab#{}", self.0)
    }
}

/// Monotonic [`TabId`] source. Ids are never reused within a process, so a
/// stale id resolves to "gone" rather than to some unrelated tab.
#[derive(Debug, Default)]
pub struct TabIdGen(u64);

impl TabIdGen {
    pub const fn new() -> Self {
        Self(0)
    }

    pub fn next(&mut self) -> TabId {
        self.0 += 1;
        TabId(self.0)
    }
}

/// Where a tab's content currently lives.
///
/// The three states carry no payload — the session blob lives in the engine
/// host's store and the live handle lives in its handle map. Keeping the enum
/// payload-free is what makes it `Copy` and keeps the core allocation-free on
/// the eviction path.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TabState {
    /// Backed by a live web view. Costs real memory (~100 MB, site-dependent).
    Live,
    /// Serialized to a session blob. Costs ~2.3 KB (measured, M0 2026-09-03).
    Hibernated,
    /// Known but never loaded — a restored session entry. Costs only metadata.
    Cold,
}

impl TabState {
    /// Whether this tab is consuming engine memory right now.
    pub const fn is_live(self) -> bool {
        matches!(self, TabState::Live)
    }

    /// Whether activating this tab requires a restore round-trip.
    pub const fn needs_restore(self) -> bool {
        !self.is_live()
    }
}

/// Everything the UI needs to draw a tab, plus what the policy needs to rank it.
#[derive(Clone, Debug)]
pub struct TabMeta {
    pub id: TabId,
    pub title: String,
    /// Display host, e.g. `docs.internal` — what the canvas shows in the
    /// address bar and the page breadcrumb.
    pub host: String,
    /// Full URL, used to reload a `Cold` tab that has no session blob.
    pub url: String,
    pub state: TabState,
    /// Monotonic activation counter. Higher means more recently focused; this
    /// is the LRU key. A logical clock rather than a timestamp, so ordering is
    /// exact and testable with no wall-clock dependency.
    pub last_active: u64,
    /// Last measured `phys_footprint` delta for this tab, in bytes.
    ///
    /// `None` means never measured. The policy charges unmeasured live tabs
    /// [`ESTIMATED_LIVE_BYTES`] rather than zero — assuming zero would make the
    /// budget unenforceable exactly when measurement is failing.
    pub footprint: Option<u64>,
    /// Whether a navigation is in flight.
    pub loading: bool,
    /// Navigation progress, 0.0 to 1.0.
    ///
    /// Fed from `WKWebView.estimatedProgress` — a real measurement, not the
    /// canvas's scripted 0 → 62 → 100 timeline, which exists only because a
    /// static mock has no page to report on.
    pub progress: f64,
    /// When the last navigation finished, in monotonic milliseconds.
    ///
    /// The bar holds at full for a moment afterwards rather than vanishing, so
    /// a fast load reads as *completed* rather than as a flicker.
    pub completed_at: Option<u64>,
}

/// Fallback per-live-tab cost when measurement has not run yet.
///
/// Derived from M0: 593 MB across 4 live tabs, less the ~58 MB idle floor, is
/// ~134 MB/tab. Rounded down to 100 MB — underestimating biases toward keeping
/// tabs live, which degrades gracefully, whereas overestimating would evict
/// tabs the machine could comfortably hold.
pub const ESTIMATED_LIVE_BYTES: u64 = 100 * 1024 * 1024;

impl TabMeta {
    pub fn new(id: TabId, title: impl Into<String>, host: impl Into<String>) -> Self {
        let host = host.into();
        Self {
            id,
            title: title.into(),
            url: format!("https://{host}"),
            host,
            state: TabState::Cold,
            last_active: 0,
            footprint: None,
            loading: false,
            progress: 0.0,
            completed_at: None,
        }
    }

    /// Bytes this tab is assumed to cost the engine right now.
    pub fn effective_footprint(&self) -> u64 {
        match self.state {
            TabState::Live => self.footprint.unwrap_or(ESTIMATED_LIVE_BYTES),
            // A hibernated blob is ~2.3 KB. Against a budget denominated in
            // hundreds of megabytes that is noise, and counting it would imply
            // hibernation has a cost worth optimizing. It does not.
            TabState::Hibernated | TabState::Cold => 0,
        }
    }
}

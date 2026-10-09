//! Live-set policy: which tabs get to hold a web view.
//!
//! This is the only lever that meaningfully moves Compact's memory use. M0
//! measured 593 MB for 4 live tabs against 2.3 KB for a hibernated one, so
//! 1000 hibernated tabs cost ~2.3 MB — hibernation is effectively free and the
//! entire budget is decided by how many tabs are live at once.
//!
//! The policy is therefore denominated in **bytes, not tab count**. A count cap
//! is the wrong primitive: a heavy web app and a text article differ by an order
//! of magnitude, so "4 live tabs" is either wasteful or oppressive depending on
//! what is open. A byte budget spends the same memory on more tabs when they are
//! cheap, and fewer when they are not.

use crate::tab::{TabId, TabMeta, TabState};

/// System memory pressure, mirroring the levels macOS reports through
/// `DISPATCH_SOURCE_TYPE_MEMORYPRESSURE`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Pressure {
    #[default]
    Normal,
    Warn,
    Critical,
}

impl Pressure {
    /// Fraction of the nominal budget available at this level.
    ///
    /// `Critical` returns 0.0, which collapses the live set to `min_live`. On
    /// this 8 GB machine that is the difference between a stutter and a wedge.
    pub const fn budget_scale(self) -> f64 {
        match self {
            Pressure::Normal => 1.0,
            Pressure::Warn => 0.5,
            Pressure::Critical => 0.0,
        }
    }
}

/// How much engine memory the live set may occupy.
#[derive(Clone, Copy, Debug)]
pub struct LiveBudget {
    /// Nominal ceiling on summed live-tab footprint, in bytes.
    pub max_bytes: u64,
    /// Hard ceiling on live tab count, independent of bytes.
    ///
    /// Bytes are the real constraint; this exists because each web content
    /// process carries ~20-30 MB of fixed overhead before any page content, so
    /// a large number of individually cheap tabs still costs real memory that
    /// per-tab measurement attributes late.
    pub max_live: usize,
    /// Floor the live set is never driven below, even under critical pressure.
    /// Always at least 1 in practice, since the active tab cannot be evicted.
    pub min_live: usize,
}

impl Default for LiveBudget {
    fn default() -> Self {
        // Conservative default sized for the 8 GB development machine, which
        // routinely runs Chrome at ~5 GB and is often already in swap.
        Self {
            max_bytes: 600 * 1024 * 1024,
            max_live: 8,
            min_live: 1,
        }
    }
}

impl LiveBudget {
    /// Derive a budget from currently-available system memory.
    ///
    /// Takes 40% of what is free. Deliberately not a fixed number: the same
    /// build has to behave on an 8 GB laptop already deep into swap and on a
    /// 64 GB desktop, and a constant would be wrong on both.
    pub fn from_available(available_bytes: u64) -> Self {
        const MIN: u64 = 256 * 1024 * 1024;
        const MAX: u64 = 4 * 1024 * 1024 * 1024;
        let budget = ((available_bytes as f64 * 0.40) as u64).clamp(MIN, MAX);
        Self {
            max_bytes: budget,
            // One slot per ~150 MB, bounded — keeps process overhead in check
            // without pretending we know each tab's cost before measuring it.
            max_live: ((budget / (150 * 1024 * 1024)) as usize).clamp(2, 12),
            min_live: 1,
        }
    }

    /// How many tabs agent workspaces may keep live between them.
    ///
    /// A third of the live set, and at least one. An agent working in the
    /// background should not be able to push the tabs you are using out of
    /// memory, however many pages it opens.
    pub fn agent_live(&self) -> usize {
        (self.max_live / 3).max(1)
    }

    /// The effective byte ceiling once pressure is applied.
    pub fn effective_bytes(&self, pressure: Pressure) -> u64 {
        (self.max_bytes as f64 * pressure.budget_scale()) as u64
    }
}

/// The set of tabs the policy wants hibernated, LRU-coldest first.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EvictionPlan {
    pub hibernate: Vec<TabId>,
}

impl EvictionPlan {
    pub fn is_empty(&self) -> bool {
        self.hibernate.is_empty()
    }
}

/// Decide which live tabs to hibernate.
///
/// `active` is never evicted regardless of budget — hibernating the tab the
/// user is looking at would be a visible failure, not a memory saving. Tabs are
/// shed coldest-first until the live set fits both the byte budget and the
/// count ceiling, stopping at `min_live`.
pub fn plan_eviction(tabs: &[TabMeta], active: TabId, budget: &LiveBudget, pressure: Pressure) -> EvictionPlan {
    plan_eviction_with(tabs, active, budget, pressure, &[])
}

/// [`plan_eviction`], with `agent` naming the tabs that belong to agent
/// workspaces.
///
/// Agent tabs are held to [`LiveBudget::agent_live`] on their own, and when
/// the whole live set is over budget they go first, coldest first, before any
/// of yours. The active tab is still never evicted, whoever owns it.
pub fn plan_eviction_with(
    tabs: &[TabMeta],
    active: TabId,
    budget: &LiveBudget,
    pressure: Pressure,
    agent: &[TabId],
) -> EvictionPlan {
    let is_agent = |t: &TabMeta| agent.contains(&t.id);
    // Live tabs, agents' first, then coldest first. The active tab is excluded
    // up front rather than filtered later so it can never be chosen even when
    // it is the coldest -- which happens after restoring an old tab from the
    // switcher.
    let mut candidates: Vec<&TabMeta> = tabs
        .iter()
        .filter(|t| t.state.is_live() && t.id != active)
        .collect();
    candidates.sort_by_key(|t| (!is_agent(t), t.last_active, t.id));

    let live_count = tabs.iter().filter(|t| t.state.is_live()).count();
    let mut bytes: u64 = tabs
        .iter()
        .filter(|t| t.state.is_live())
        .map(TabMeta::effective_footprint)
        .sum();

    let ceiling = budget.effective_bytes(pressure);
    let min_live = budget.min_live.max(1);

    let mut hibernate = Vec::new();
    let mut remaining = live_count;

    // Agents over their own slice shed first, whatever the totals say.
    let mut agent_live = tabs.iter().filter(|t| t.state.is_live() && is_agent(t)).count();
    for tab in candidates.iter().filter(|t| is_agent(t)) {
        if agent_live <= budget.agent_live() || remaining <= min_live {
            break;
        }
        bytes = bytes.saturating_sub(tab.effective_footprint());
        remaining -= 1;
        agent_live -= 1;
        hibernate.push(tab.id);
    }
    candidates.retain(|t| !hibernate.contains(&t.id));

    for tab in candidates {
        if remaining <= min_live {
            break;
        }
        if bytes <= ceiling && remaining <= budget.max_live {
            break;
        }
        bytes = bytes.saturating_sub(tab.effective_footprint());
        remaining -= 1;
        hibernate.push(tab.id);
    }

    EvictionPlan { hibernate }
}

/// Tabs worth restoring *before* the user asks for them.
///
/// Hibernation's cost is not memory, it is the latency when you switch back.
/// Prewarming the likely-next tab is what lets an aggressive live set still feel
/// instant. Prediction is deliberately dumb — immediate neighbors in tab order,
/// which is what ⌘⌥→ / a click actually reaches for — because a wrong guess
/// spends ~100 MB, and a clever predictor that is wrong more often is worse than
/// a simple one that is right most of the time.
///
/// Returns at most `slack` ids, nearest-neighbor first, and yields nothing when
/// the budget has no room to spare.
pub fn prewarm_candidates(
    tabs: &[TabMeta],
    active: TabId,
    budget: &LiveBudget,
    pressure: Pressure,
    slack: usize,
) -> Vec<TabId> {
    if slack == 0 || pressure != Pressure::Normal {
        return Vec::new();
    }
    let Some(idx) = tabs.iter().position(|t| t.id == active) else {
        return Vec::new();
    };

    let live_count = tabs.iter().filter(|t| t.state.is_live()).count();
    let bytes: u64 = tabs
        .iter()
        .filter(|t| t.state.is_live())
        .map(TabMeta::effective_footprint)
        .sum();

    // Only prewarm into genuinely spare capacity. Restoring a tab that the very
    // next eviction pass would immediately hibernate again is pure churn.
    let ceiling = budget.effective_bytes(pressure);
    let room_bytes = ceiling.saturating_sub(bytes) / crate::tab::ESTIMATED_LIVE_BYTES;
    let room_count = budget.max_live.saturating_sub(live_count);
    let room = (room_bytes as usize).min(room_count).min(slack);
    if room == 0 {
        return Vec::new();
    }

    let mut out = Vec::with_capacity(room);
    // Walk outward from the active tab: right, left, right+1, left-1, ...
    for step in 1..tabs.len() {
        for cand in [idx.checked_add(step), idx.checked_sub(step)]
            .into_iter()
            .flatten()
        {
            if out.len() == room {
                return out;
            }
            if let Some(t) = tabs.get(cand) {
                if t.state.needs_restore() {
                    out.push(t.id);
                }
            }
        }
        if out.len() == room {
            break;
        }
    }
    out
}

/// Summary of where memory currently sits, for the settings footer readout
/// (the canvas shows a `ramLabel` there).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MemoryReport {
    pub live: usize,
    pub hibernated: usize,
    pub cold: usize,
    pub live_bytes: u64,
}

impl MemoryReport {
    pub fn of(tabs: &[TabMeta]) -> Self {
        let mut r = Self::default();
        for t in tabs {
            match t.state {
                TabState::Live => {
                    r.live += 1;
                    r.live_bytes += t.effective_footprint();
                }
                TabState::Hibernated => r.hibernated += 1,
                TabState::Cold => r.cold += 1,
            }
        }
        r
    }

    /// `"~412 MB"` — the estimated readout, when nothing has been measured.
    ///
    /// The tilde is not decoration: this sums per-tab estimates, and a number
    /// presented without one would claim a precision the estimate does not
    /// have. [`App::memory_label`] prefers a real measurement when one exists.
    pub fn label(&self) -> String {
        format!("~{} MB", self.live_bytes / (1024 * 1024))
    }
}

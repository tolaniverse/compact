//! Auto-hiding chrome: the reveal state machine.
//!
//! Compact's chrome is absent until the cursor approaches an edge. The
//! thresholds are asymmetric on purpose — chrome appears within `threshold` of
//! an edge but does not retract until the cursor is well past it. Without that
//! hysteresis the strip flickers whenever the pointer hovers near the boundary,
//! which is exactly where a user aiming at a tab leaves it.

/// Where tabs are drawn.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum TabLayout {
    #[default]
    Vertical,
    Horizontal,
}

impl TabLayout {
    pub fn toggled(self) -> Self {
        match self {
            TabLayout::Vertical => TabLayout::Horizontal,
            TabLayout::Horizontal => TabLayout::Vertical,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            TabLayout::Vertical => "Vertical",
            TabLayout::Horizontal => "Horizontal",
        }
    }
}

/// Default cursor distance that triggers a reveal, in points.
pub const DEFAULT_REVEAL_THRESHOLD: f64 = 76.0;
/// Slider bounds from the settings panel.
pub const REVEAL_THRESHOLD_MIN: f64 = 24.0;
pub const REVEAL_THRESHOLD_MAX: f64 = 180.0;
pub const REVEAL_THRESHOLD_STEP: f64 = 4.0;

/// Multiplier past which the top chrome retracts (canvas: `y > T * 3`).
const RETRACT_TOP: f64 = 3.0;
/// Multiplier past which the sidebar retracts (canvas: `x > T * 3.4`).
///
/// Wider than the vertical one because the sidebar is 246 pt: retracting at 3x
/// a 76 pt threshold would hide it while the cursor was still over it.
const RETRACT_SIDE: f64 = 3.4;

/// Latched reveal state. Updated from cursor motion; read by the renderer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Reveal {
    pub top: bool,
    pub side: bool,
}

/// User-controlled chrome configuration (the settings panel's contents).
#[derive(Clone, Copy, Debug)]
pub struct ChromeConfig {
    pub layout: TabLayout,
    pub threshold: f64,
    /// Keep chrome permanently visible, bypassing reveal entirely.
    pub pinned: bool,
    pub smooth_motion: bool,
    pub dim_page: bool,
    pub show_hint: bool,
}

impl Default for ChromeConfig {
    fn default() -> Self {
        Self {
            layout: TabLayout::Vertical,
            threshold: DEFAULT_REVEAL_THRESHOLD,
            pinned: false,
            smooth_motion: true,
            dim_page: false,
            show_hint: true,
        }
    }
}

impl ChromeConfig {
    pub fn set_threshold(&mut self, v: f64) {
        self.threshold = v.clamp(REVEAL_THRESHOLD_MIN, REVEAL_THRESHOLD_MAX);
    }

    /// `"76 px"` — the readout beside the slider.
    pub fn threshold_label(&self) -> String {
        format!("{} px", self.threshold.round() as i64)
    }
}

impl Reveal {
    /// Fold a cursor position (window-local, origin top-left) into the latch.
    ///
    /// Returns `true` if anything changed, so the caller can skip redrawing on
    /// the overwhelming majority of mouse-move events that change nothing.
    pub fn update(&mut self, cursor: Option<(f64, f64)>, size: (f64, f64), threshold: f64) -> bool {
        let before = *self;
        match cursor {
            // Cursor left the window: drop all chrome. The canvas allows 4 px of
            // slop so that grazing the window edge does not retract chrome the
            // user is actively reaching for.
            None => {
                self.top = false;
                self.side = false;
            }
            Some((x, y)) => {
                let inside = x > -4.0 && x < size.0 + 4.0 && y > -4.0 && y < size.1 + 4.0;
                if !inside {
                    self.top = false;
                    self.side = false;
                } else {
                    if y < threshold {
                        self.top = true;
                    } else if y > threshold * RETRACT_TOP {
                        self.top = false;
                    }
                    if x < threshold {
                        self.side = true;
                    } else if x > threshold * RETRACT_SIDE {
                        self.side = false;
                    }
                }
            }
        }
        *self != before
    }
}

/// What the renderer should actually show, derived from reveal + config.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChromeVisibility {
    /// Address bar and traffic lights.
    pub address: bool,
    /// Horizontal tab strip.
    pub strip: bool,
    /// Vertical sidebar.
    pub sidebar: bool,
}

impl ChromeVisibility {
    pub fn derive(reveal: Reveal, cfg: &ChromeConfig) -> Self {
        let address = cfg.pinned || reveal.top;
        Self {
            address,
            strip: cfg.layout == TabLayout::Horizontal && address,
            // The sidebar also answers to the top edge: approaching the title
            // area is a reach for chrome generally, and revealing only the
            // address bar there would strand the tabs.
            sidebar: cfg.layout == TabLayout::Vertical && (cfg.pinned || reveal.side || reveal.top),
        }
    }

    pub fn any(&self) -> bool {
        self.address || self.strip || self.sidebar
    }

    /// Address bar's distance from the window top, in points. It sits below the
    /// strip when the strip is out, and near the top edge otherwise.
    pub fn address_top(&self) -> f64 {
        if self.strip {
            58.0
        } else {
            14.0
        }
    }
}

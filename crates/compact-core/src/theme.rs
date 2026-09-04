//! Design tokens transcribed from `Compact Browser.dc.html`.
//!
//! Colors are sRGB with straight (non-premultiplied) alpha, matching how the
//! canvas expresses them. Keeping them in one place means the AppKit chrome and
//! any future backend render the same design without re-deriving constants.

/// sRGB color with straight alpha, components in `0.0..=1.0`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rgba {
    pub r: f64,
    pub g: f64,
    pub b: f64,
    pub a: f64,
}

impl Rgba {
    pub const fn new(r: f64, g: f64, b: f64, a: f64) -> Self {
        Self { r, g, b, a }
    }

    /// `0xRRGGBB` at full opacity — mirrors the canvas's `#RRGGBB` literals.
    pub const fn hex(v: u32) -> Self {
        Self {
            r: ((v >> 16) & 0xFF) as f64 / 255.0,
            g: ((v >> 8) & 0xFF) as f64 / 255.0,
            b: (v & 0xFF) as f64 / 255.0,
            a: 1.0,
        }
    }

    /// White at `a` alpha — the canvas builds nearly every border and fill this
    /// way (`rgba(255,255,255,.085)`).
    pub const fn white(a: f64) -> Self {
        Self::new(1.0, 1.0, 1.0, a)
    }

    pub const fn with_alpha(self, a: f64) -> Self {
        Self { a, ..self }
    }
}

/// Backdrop colors.
pub mod bg {
    use super::Rgba;
    /// Page behind the window.
    pub const VOID: Rgba = Rgba::hex(0x050506);
    /// The browser window itself.
    pub const WINDOW: Rgba = Rgba::hex(0x0D0D10);
    /// Web content backdrop.
    pub const PAGE: Rgba = Rgba::hex(0x0F0F13);
    /// Horizontal tab strip (canvas: `rgba(16,16,20,.82)`).
    pub const STRIP: Rgba = Rgba::new(16.0 / 255.0, 16.0 / 255.0, 20.0 / 255.0, 0.82);
    /// Vertical sidebar (canvas: `rgba(16,16,20,.86)`).
    pub const SIDEBAR: Rgba = Rgba::new(16.0 / 255.0, 16.0 / 255.0, 20.0 / 255.0, 0.86);
    /// Address bar / toast / palette surfaces.
    pub const RAISED: Rgba = Rgba::new(24.0 / 255.0, 24.0 / 255.0, 29.0 / 255.0, 0.92);
    pub const TOAST: Rgba = Rgba::new(24.0 / 255.0, 24.0 / 255.0, 29.0 / 255.0, 0.94);
    pub const PALETTE: Rgba = Rgba::new(22.0 / 255.0, 22.0 / 255.0, 27.0 / 255.0, 0.96);
    pub const SETTINGS: Rgba = Rgba::new(18.0 / 255.0, 18.0 / 255.0, 23.0 / 255.0, 0.97);
    pub const HINT: Rgba = Rgba::hex(0x15151A);
    /// Full-screen scrim behind the palette.
    pub const SCRIM: Rgba = Rgba::new(5.0 / 255.0, 5.0 / 255.0, 7.0 / 255.0, 0.62);
    /// Heavier scrim behind the workspace switcher.
    pub const SCRIM_HEAVY: Rgba = Rgba::new(5.0 / 255.0, 5.0 / 255.0, 7.0 / 255.0, 0.72);
}

/// Foreground colors.
pub mod fg {
    use super::Rgba;
    pub const PRIMARY: Rgba = Rgba::hex(0xEDEDEF);
    pub const INPUT: Rgba = Rgba::hex(0xE4E4E9);
    pub const TITLE: Rgba = Rgba::hex(0xE9E9ED);
    pub const SECONDARY: Rgba = Rgba::hex(0x9A9AA4);
    pub const MUTED: Rgba = Rgba::hex(0x75757F);
    pub const DIM: Rgba = Rgba::hex(0x5E5E68);
    pub const FAINT: Rgba = Rgba::hex(0x4E4E58);
}

/// Accent + semantic colors.
pub mod accent {
    use super::Rgba;
    /// The palette / selection accent (`PALETTE_ACCENT` in the canvas).
    pub const PRIMARY: Rgba = Rgba::hex(0x9DB4FF);
    pub const LINK_HOVER: Rgba = Rgba::hex(0xC7D4FF);
    /// macOS traffic lights, drawn by us because the window is borderless.
    pub const CLOSE: Rgba = Rgba::hex(0xFF5F57);
    pub const MINIMIZE: Rgba = Rgba::hex(0xFEBC2E);
    pub const ZOOM: Rgba = Rgba::hex(0x28C840);
}

/// Per-workspace identity colors, in the canvas's declaration order.
pub const WORKSPACE_DOTS: [Rgba; 3] = [
    Rgba::hex(0x9DB4FF), // Personal
    Rgba::hex(0xA8E6B8), // Work
    Rgba::hex(0xF0D49A), // Research
];

/// Hairline borders, all white-on-dark at low alpha.
pub mod border {
    use super::Rgba;
    pub const WINDOW: Rgba = Rgba::white(0.085);
    pub const CHROME: Rgba = Rgba::white(0.07);
    pub const RAISED: Rgba = Rgba::white(0.11);
    pub const SUBTLE: Rgba = Rgba::white(0.06);
    pub const TAB_ACTIVE: Rgba = Rgba::white(0.10);
    pub const CARD: Rgba = Rgba::white(0.08);
    pub const CARD_ACTIVE: Rgba = Rgba::white(0.22);
}

/// Fill colors for interactive states.
pub mod fill {
    use super::Rgba;
    pub const TAB_ACTIVE: Rgba = Rgba::white(0.085);
    pub const HOVER: Rgba = Rgba::white(0.075);
    pub const HOVER_SOFT: Rgba = Rgba::white(0.06);
    pub const TRACK_OFF: Rgba = Rgba::white(0.12);
    pub const INACTIVE_DOT: Rgba = Rgba::white(0.18);
}

/// Corner radii, in points.
pub mod radius {
    pub const WINDOW: f64 = 15.0;
    pub const TAB_STRIP: f64 = 7.0;
    pub const TAB_SIDE: f64 = 8.0;
    pub const ADDRESS: f64 = 11.0;
    pub const PALETTE: f64 = 14.0;
    pub const CARD: f64 = 13.0;
    pub const TOAST: f64 = 11.0;
    pub const HINT: f64 = 10.0;
}

/// Fixed metrics, in points.
pub mod metric {
    pub const STRIP_HEIGHT: f64 = 46.0;
    pub const SIDEBAR_WIDTH: f64 = 246.0;
    pub const SETTINGS_WIDTH: f64 = 344.0;
    pub const TOAST_WIDTH: f64 = 308.0;
    pub const ADDRESS_HEIGHT: f64 = 40.0;
    pub const ADDRESS_MAX_WIDTH: f64 = 560.0;
    /// Address bar occupies this fraction of the window when narrower than max.
    pub const ADDRESS_WIDTH_FRACTION: f64 = 0.64;
    pub const TAB_HEIGHT_STRIP: f64 = 29.0;
    pub const TAB_HEIGHT_SIDE: f64 = 32.0;
    pub const TAB_MAX_WIDTH: f64 = 190.0;
    pub const TAB_MIN_WIDTH: f64 = 64.0;
    /// Left inset on the strip that clears the traffic lights.
    pub const TRAFFIC_LIGHT_INSET: f64 = 88.0;
    pub const PALETTE_WIDTH: f64 = 600.0;
    pub const PALETTE_ROW_HEIGHT: f64 = 38.0;
    pub const PALETTE_MAX_LIST_HEIGHT: f64 = 334.0;
    pub const WS_CARD_WIDTH: f64 = 258.0;
    pub const WS_CARD_HEIGHT: f64 = 172.0;
}

/// Animation durations, in seconds, matching the canvas transitions.
pub mod duration {
    /// Sidebar / strip tear-in.
    pub const CHROME: f64 = 0.44;
    /// Address bar tear-off.
    pub const ADDRESS: f64 = 0.52;
    /// Settings drawer.
    pub const SETTINGS: f64 = 0.46;
    /// Workspace slide-out and slide-in halves.
    pub const SLIDE_OUT: f64 = 0.24;
    pub const SLIDE_IN: f64 = 0.50;
    /// Hover tint.
    pub const HOVER: f64 = 0.16;
    /// Toast lifetime before auto-dismiss.
    pub const TOAST_LIFETIME: f64 = 7.0;
}

/// Font families. Falls back to the system face when the webfont is absent.
pub mod font {
    pub const UI: &str = "Instrument Sans";
    pub const MONO: &str = "JetBrains Mono";
    pub const UI_FALLBACK: &str = "SF Pro Text";
    pub const MONO_FALLBACK: &str = "SF Mono";
}

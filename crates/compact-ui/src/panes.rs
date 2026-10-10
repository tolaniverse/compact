//! Where pages go: one page, or two side by side in split view, with the
//! developer tools docked inside the focused one.
//!
//! The chrome floats above all of this and belongs to the focused side; only
//! the page views and the drawer's view are placed here. The container is
//! not flipped, so every frame below runs up from the bottom.

use objc2::rc::Retained;
use objc2::MainThreadMarker;
use objc2_app_kit::NSView;
use objc2_core_foundation::CGRect;

use compact_core::settings::Dock;
use compact_core::split::{Pane, Side};
use compact_core::tab::TabId;
use compact_core::theme::Rgba;

use super::Inner;
use crate::widgets::{container, rect, set_host_fill};

/// Width of the line between the two sides.
const DIVIDER: f64 = 1.0;
/// Height of the bar marking the focused side.
const FOCUS_BAR: f64 = 2.0;

/// The divider and the focused side's bar, made on first use.
pub(super) struct SplitMarks {
    divider: Retained<NSView>,
    focus: Retained<NSView>,
}

fn side_rect(side: Side, w: f64, h: f64) -> CGRect {
    let half = ((w - DIVIDER) / 2.0).floor();
    match side {
        Side::Left => rect(0.0, 0.0, half, h),
        Side::Right => rect(half + DIVIDER, 0.0, w - half - DIVIDER, h),
    }
}

/// Which side a point in window coordinates (from the top left) is on.
pub(super) fn side_at(x: f64, w: f64) -> Side {
    if x < w / 2.0 {
        Side::Left
    } else {
        Side::Right
    }
}

impl Inner {
    /// Place the page or pages, and the drawer beside whichever page it is
    /// inspecting.
    pub(super) fn layout(&mut self) {
        let (w, h) = self.size();
        let panes = self.app.panes();
        let focused = panes.and_then(|p| p.into_iter().find(|q| q.focused));
        let front = self.app.workspaces.current().active;
        let whole = rect(0.0, 0.0, w, h);

        // Every page on screen with the area it owns, focused first.
        let shown: Vec<(TabId, CGRect)> = match panes {
            None => vec![(front, whole)],
            Some(p) => {
                let mut v: Vec<(TabId, CGRect)> = p.iter().map(|q| (q.tab, side_rect(q.side, w, h))).collect();
                v.sort_by_key(|(t, _)| *t != front);
                v
            }
        };
        // The drawer stays with its page, focused or not; it hides only when
        // its page is not on screen at all.
        let mut tools = None;
        let frames: Vec<(TabId, CGRect)> = shown
            .iter()
            .map(|(tab, area)| {
                let (page, t) = self.dock_tools(*area, *tab);
                if t.is_some() {
                    tools = t;
                }
                (*tab, page)
            })
            .collect();

        match panes {
            None => {
                self.engine.set_page_frame(tools.is_some().then_some(frames[0].1));
                self.engine.show(front);
            }
            Some(_) => self.engine.show_panes(&frames, front),
        }
        // Opened after the pages are mounted, so it sits on top of them.
        match tools {
            Some(t) => {
                if self.engine.open_tools(t) {
                    self.tools_ready = false;
                }
            }
            None => self.engine.hide_tools(),
        }
        self.draw_split_marks(focused, w, h);

        // In split view the keyboard follows focus, so typing goes to the
        // side the chrome now speaks for.
        if panes.is_some() && self.last_focus != Some(front) {
            if let Some(v) = self.engine.view(front) {
                self.window.makeFirstResponder(Some(v));
            }
        }
        self.last_focus = Some(front);
    }

    /// Split `area` between the page and the drawer, if the drawer is open
    /// on `tab`.
    fn dock_tools(&self, area: CGRect, tab: TabId) -> (CGRect, Option<CGRect>) {
        if self.app.devtools != Some(tab) {
            return (area, None);
        }
        let (x, y, w, h) = (area.origin.x, area.origin.y, area.size.width, area.size.height);
        let side = (w * 0.4).clamp(300.0, 560.0).min(w - 200.0).max(0.0);
        let bottom = (h * 0.38).max(220.0).min(h - 160.0).max(0.0);
        let (page, tools) = match self.app.settings.devtools_dock {
            Dock::Bottom => (rect(x, y + bottom, w, h - bottom), rect(x, y, w, bottom)),
            Dock::Right => (rect(x, y, w - side, h), rect(x + w - side, y, side, h)),
            Dock::Left => (rect(x + side, y, w - side, h), rect(x, y, side, h)),
        };
        (page, Some(tools))
    }

    fn draw_split_marks(&mut self, focused: Option<Pane>, w: f64, h: f64) {
        let Some(f) = focused else {
            if let Some(m) = &self.split_marks {
                m.divider.setHidden(true);
                m.focus.setHidden(true);
            }
            return;
        };
        if self.split_marks.is_none() {
            let mtm = MainThreadMarker::new().expect("main thread");
            let zero = rect(0.0, 0.0, 0.0, 0.0);
            let marks = SplitMarks { divider: container(mtm, zero), focus: container(mtm, zero) };
            set_host_fill(crate::widgets::host_layer(&*marks.divider), Rgba::white(0.14));
            self.split_marks = Some(marks);
        }
        let dot = self.app.workspaces.current().dot;
        let m = self.split_marks.as_ref().expect("just made");
        let half = ((w - DIVIDER) / 2.0).floor();
        m.divider.setFrame(rect(half, 0.0, DIVIDER, h));
        let s = side_rect(f.side, w, h);
        m.focus.setFrame(rect(s.origin.x, h - FOCUS_BAR, s.size.width, FOCUS_BAR));
        set_host_fill(crate::widgets::host_layer(&*m.focus), dot);
        for v in [&m.divider, &m.focus] {
            v.setHidden(false);
            // Above the pages, which may have just been mounted over them.
            self.page.addSubview(v);
        }
    }
}

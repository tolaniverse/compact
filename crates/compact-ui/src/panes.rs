//! Where pages go: one page, or two side by side in split view, with the
//! developer tools docked beside the page they inspect.
//!
//! In split view each side is a card: rounded, set apart from the other by a
//! gap, outlined in its workspace's colour when focused, and labelled
//! underneath with the workspace and what it is showing. None of that exists
//! outside a split — a single page stays full-bleed and square.
//!
//! The chrome floats above all of this and belongs to the focused side; only
//! the page views, the drawer's view and the labels are placed here. The
//! container is not flipped, so frames run up from the bottom.

use objc2::rc::Retained;
use objc2::{msg_send, MainThreadMarker};
use objc2_app_kit::NSView;
use objc2_core_foundation::CGRect;

use compact_core::settings::Dock;
use compact_core::split::{Pane, Side};
use compact_core::tab::TabId;
use compact_core::theme::{bg, fg, Rgba};

use super::Inner;
use crate::widgets::{
    add_sublayer, container, host_layer, layer, panel, rect, set_color, set_frame, set_host_border, set_host_fill,
    set_text, show_marker, text, Align, Layer, Robot,
};

/// Space around the cards and between them.
const PAD: f64 = 8.0;
/// Height of the label row under each card.
const LABEL: f64 = 26.0;
/// Corner radius of a card.
const RADIUS: f64 = 10.0;
/// Space between a page and the drawer docked beside it, inside one side.
const INNER_GAP: f64 = 6.0;

/// The label under one side.
struct PaneLabel {
    view: Retained<NSView>,
    /// Hosted inside the view's own layer, so it needs its frame set.
    root: Layer,
    dot: Layer,
    robot: Robot,
    name: Layer,
    title: Layer,
}

impl PaneLabel {
    fn new(mtm: MainThreadMarker) -> Self {
        let view = container(mtm, rect(0.0, 0.0, 0.0, 0.0));
        // A flipped root, so the label is laid out top-down like the chrome.
        let root = layer();
        unsafe {
            let _: () = msg_send![&*root, setGeometryFlipped: true];
        }
        crate::widgets::set_view_layer(&view, &root);
        let dot = panel(rect(0.0, 0.0, 7.0, 7.0), Rgba::white(0.5), 3.5, None);
        let name = text("", 12.0, fg::SECONDARY, false, Align::Left);
        let title = text("", 11.5, fg::DIM, false, Align::Right);
        for l in [&dot, &name, &title] {
            add_sublayer(&root, l);
        }
        let robot = Robot::new(&root);
        PaneLabel { view, root, dot, robot, name, title }
    }
}

/// The two labels, made on first use.
pub(super) struct SplitMarks {
    labels: [PaneLabel; 2],
}

/// How far either side of the gap's centre a press still grabs the divider.
const GRAB: f64 = 6.0;

/// The width the two cards share between them.
fn shared(w: f64) -> f64 {
    (w - 3.0 * PAD).max(0.0)
}

/// A side's whole area: its card on top, its label underneath. `ratio` is
/// the left side's share of the width.
fn side_area(side: Side, ratio: f64, w: f64, h: f64) -> CGRect {
    let left = (shared(w) * ratio).floor();
    match side {
        Side::Left => rect(PAD, PAD, left, h - 2.0 * PAD),
        Side::Right => rect(2.0 * PAD + left, PAD, shared(w) - left, h - 2.0 * PAD),
    }
}

/// The centre of the gap between the cards, in window coordinates.
pub(super) fn divider_x(ratio: f64, w: f64) -> f64 {
    PAD + (shared(w) * ratio).floor() + PAD / 2.0
}

/// Show the cursor for dragging the divider.
///
/// `resizeLeftRightCursor` is deprecated in favour of
/// `columnResizeCursorInDirections:`, which only exists from macOS 15;
/// Compact supports macOS 11, so the older one stays.
#[allow(deprecated)]
pub(super) fn divider_cursor() {
    objc2_app_kit::NSCursor::resizeLeftRightCursor().set();
}

/// Whether a point `x` is on the divider.
pub(super) fn on_divider(x: f64, ratio: f64, w: f64) -> bool {
    (x - divider_x(ratio, w)).abs() <= GRAB
}

/// The ratio that puts the divider's centre at `x`.
pub(super) fn ratio_at(x: f64, w: f64) -> f64 {
    let s = shared(w);
    if s <= 0.0 {
        return 0.5;
    }
    compact_core::split::clamp_ratio((x - PAD - PAD / 2.0) / s)
}

fn card_of(area: CGRect) -> CGRect {
    rect(area.origin.x, area.origin.y + LABEL, area.size.width, area.size.height - LABEL)
}

fn label_of(area: CGRect) -> CGRect {
    rect(area.origin.x, area.origin.y, area.size.width, LABEL)
}

/// Which side a point in window coordinates (from the top left) is on.
pub(super) fn side_at(x: f64, ratio: f64, w: f64) -> Side {
    if x < divider_x(ratio, w) {
        Side::Left
    } else {
        Side::Right
    }
}

/// Round and outline a web view's layer, or square it off again.
fn style(view: &NSView, radius: f64, border: Option<(Rgba, f64)>) {
    view.setWantsLayer(true);
    let l = host_layer(view);
    unsafe {
        let _: () = msg_send![l, setCornerRadius: radius];
        let _: () = msg_send![l, setMasksToBounds: radius > 0.0];
    }
    let (c, w) = border.unwrap_or((Rgba::white(0.0), 0.0));
    set_host_border(l, c, w);
}

impl Inner {
    /// Place the page or pages, and the drawer beside whichever page it is
    /// inspecting.
    pub(super) fn layout(&mut self) {
        let (w, h) = self.size();
        let panes = self.app.panes();
        let front = self.app.workspaces.current().active;
        let split = panes.is_some();
        let ratio = self.app.split.map_or(0.5, |s| s.ratio);

        // Every page on screen with the card it owns, focused first.
        let shown: Vec<(Option<Pane>, TabId, CGRect)> = match panes {
            None => vec![(None, front, rect(0.0, 0.0, w, h))],
            Some(p) => {
                let mut v: Vec<_> = p.iter().map(|q| (Some(*q), q.tab, card_of(side_area(q.side, ratio, w, h)))).collect();
                v.sort_by_key(|(_, t, _)| *t != front);
                v
            }
        };
        // The drawer stays with its page, focused or not; it hides only when
        // its page is not on screen at all.
        let mut tools = None;
        let frames: Vec<(TabId, CGRect)> = shown
            .iter()
            .map(|(_, tab, area)| {
                let (page, t) = self.dock_tools(*area, *tab, split);
                if t.is_some() {
                    tools = t;
                }
                (*tab, page)
            })
            .collect();

        if split {
            self.engine.show_panes(&frames, front);
        } else {
            self.engine.set_page_frame(tools.is_some().then_some(frames[0].1));
            self.engine.show(front);
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

        // Cards in split view, full bleed otherwise.
        set_host_fill(host_layer(&*self.page), if split { bg::WINDOW } else { bg::PAGE });
        for (pane, tab, _) in &shown {
            let Some(v) = self.engine.view(*tab) else { continue };
            match pane {
                Some(q) if q.focused => style(v, RADIUS, Some((self.app.workspaces.spaces[q.workspace].dot, 1.5))),
                Some(_) => style(v, RADIUS, Some((Rgba::white(0.08), 1.0))),
                None => style(v, 0.0, None),
            }
        }
        if let Some(t) = self.engine.tools_view() {
            style(t, if split { RADIUS } else { 0.0 }, None);
        }
        self.draw_labels(panes, ratio, w, h);

        // In split view the keyboard follows focus, so typing goes to the
        // side the chrome now speaks for.
        if split && self.last_focus != Some(front) {
            if let Some(v) = self.engine.view(front) {
                self.window.makeFirstResponder(Some(v));
            }
        }
        self.last_focus = Some(front);
    }

    /// Split `area` between the page and the drawer, if the drawer is open
    /// on `tab`. In split view the two are set slightly apart, as cards.
    fn dock_tools(&self, area: CGRect, tab: TabId, split: bool) -> (CGRect, Option<CGRect>) {
        if self.app.devtools != Some(tab) {
            return (area, None);
        }
        let gap = if split { INNER_GAP } else { 0.0 };
        let (x, y, w, h) = (area.origin.x, area.origin.y, area.size.width, area.size.height);
        let side = (w * 0.4).clamp(300.0, 560.0).min(w - 200.0).max(0.0);
        let bottom = (h * 0.38).max(220.0).min(h - 160.0).max(0.0);
        let (page, tools) = match self.app.settings.devtools_dock {
            Dock::Bottom => (rect(x, y + bottom + gap, w, h - bottom - gap), rect(x, y, w, bottom)),
            Dock::Right => (rect(x, y, w - side - gap, h), rect(x + w - side, y, side, h)),
            Dock::Left => (rect(x + side + gap, y, w - side - gap, h), rect(x, y, side, h)),
        };
        (page, Some(tools))
    }

    /// The label under each side: its marker, its workspace and what it
    /// shows. Hidden whenever there is no split.
    fn draw_labels(&mut self, panes: Option<[Pane; 2]>, ratio: f64, w: f64, h: f64) {
        let Some(panes) = panes else {
            if let Some(m) = &self.split_marks {
                for l in &m.labels {
                    l.view.setHidden(true);
                }
            }
            return;
        };
        if self.split_marks.is_none() {
            let mtm = MainThreadMarker::new().expect("main thread");
            self.split_marks = Some(SplitMarks { labels: [PaneLabel::new(mtm), PaneLabel::new(mtm)] });
        }
        let marks = self.split_marks.as_ref().expect("just made");
        for (q, l) in panes.iter().zip(&marks.labels) {
            let ws = &self.app.workspaces.spaces[q.workspace];
            let area = label_of(side_area(q.side, ratio, w, h));
            l.view.setFrame(area);
            l.view.setHidden(false);
            let width = area.size.width;
            set_frame(&l.root, rect(0.0, 0.0, width, LABEL));
            show_marker(&l.dot, &l.robot, ws, rect(6.0, 10.0, 7.0, 7.0), 13.0);
            let name_w = (width * 0.45).min(260.0);
            set_frame(&l.name, rect(21.0, 6.0, name_w, 15.0));
            set_text(&l.name, &ws.name);
            set_color(&l.name, if q.focused { fg::PRIMARY } else { fg::SECONDARY });
            let title = ws.tab(q.tab).map(|t| t.title.clone()).unwrap_or_default();
            // The page's title sits at the card's far edge, out of the name's way.
            set_frame(&l.title, rect(21.0 + name_w + 8.0, 7.0, (width - name_w - 35.0).max(0.0), 14.0));
            set_text(&l.title, &title);
            // Above the pages, which may have just been mounted.
            self.page.addSubview(&l.view);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dragging_the_divider_to_a_point_puts_it_there() {
        let w = 1480.0;
        for r in [0.3, 0.5, 0.62] {
            let x = divider_x(r, w);
            assert!((ratio_at(x, w) - r).abs() < 0.002, "ratio {r}");
            assert!(on_divider(x, r, w));
            assert!(on_divider(x + GRAB, r, w) && !on_divider(x + GRAB + 1.0, r, w));
        }
    }

    #[test]
    fn the_divider_stops_at_a_quarter_either_way() {
        let w = 1480.0;
        assert_eq!(ratio_at(0.0, w), compact_core::split::MIN_RATIO);
        assert_eq!(ratio_at(w, w), compact_core::split::MAX_RATIO);
    }

    #[test]
    fn the_sides_and_gaps_fill_the_width_exactly() {
        let (w, h) = (1480.0, 900.0);
        for r in [0.25, 0.5, 0.7] {
            let l = side_area(Side::Left, r, w, h);
            let rt = side_area(Side::Right, r, w, h);
            assert_eq!(l.origin.x, PAD);
            assert_eq!(rt.origin.x - (l.origin.x + l.size.width), PAD, "one gap between");
            assert_eq!(rt.origin.x + rt.size.width, w - PAD, "the right edge's margin");
            assert_eq!(side_at(divider_x(r, w) - 1.0, r, w), Side::Left);
            assert_eq!(side_at(divider_x(r, w) + 1.0, r, w), Side::Right);
        }
    }
}

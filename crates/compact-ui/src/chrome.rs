//! The always-present chrome: sidebar, tab strip, address bar, hint, toasts.
//!
//! Layers are allocated once and mutated thereafter. Rows are pooled and grown
//! on demand rather than rebuilt per sync — a tab strip rebuilt on every
//! keystroke would allocate and free hundreds of layers a second for no visible
//! benefit.

use objc2::rc::Retained;
use objc2::MainThreadMarker;
use objc2_app_kit::{NSAnimatablePropertyContainer, NSTextField, NSView, NSVisualEffectView};
use objc2_core_foundation::CGRect;

use compact_core::theme::{accent, bg, border, fg, fill, metric, radius, Rgba};
use compact_core::{App, Overlay};

use crate::widgets::*;

/// One reusable tab row (sidebar or strip).
struct TabRow {
    root: Layer,
    dot: Layer,
    title: Layer,
    close: Layer,
}

impl TabRow {
    fn new(parent: &Layer, r: f64) -> Self {
        let root = panel(rect(0.0, 0.0, 0.0, 0.0), Rgba::white(0.0), r, None);
        let dot = panel(rect(0.0, 0.0, 5.0, 5.0), fill::INACTIVE_DOT, 2.5, None);
        let title = text("", 12.5, fg::SECONDARY, false, Align::Left);
        let close = text("×", 13.0, fg::DIM, false, Align::Center);
        add_sublayer(&root, &dot);
        add_sublayer(&root, &title);
        add_sublayer(&root, &close);
        add_sublayer(parent, &root);
        Self { root, dot, title, close }
    }
}

/// One reusable toast.
struct Toast {
    root: Layer,
    dot: Layer,
    ws: Layer,
    age: Layer,
    body: Layer,
    source: Layer,
    action: Layer,
    close: Layer,
}

impl Toast {
    fn new(parent: &Layer) -> Self {
        let root = panel(
            rect(0.0, 0.0, metric::TOAST_WIDTH, 0.0),
            bg::TOAST,
            radius::TOAST,
            Some((border::RAISED, 1.0)),
        );
        let dot = panel(rect(13.0, 14.0, 6.0, 6.0), accent::PRIMARY, 3.0, None);
        let ws = text("", 9.5, accent::PRIMARY, true, Align::Left);
        let age = text("", 9.5, fg::FAINT, true, Align::Right);
        let body = text("", 12.5, fg::INPUT, false, Align::Left);
        let source = text("", 11.0, fg::MUTED, false, Align::Left);
        let action = text("", 9.5, fg::SECONDARY, true, Align::Right);
        let close = text("×", 13.0, fg::DIM, false, Align::Center);
        for l in [&dot, &ws, &age, &body, &source, &action, &close] {
            add_sublayer(&root, l);
        }
        add_sublayer(parent, &root);
        Self { root, dot, ws, age, body, source, action, close }
    }
}

pub struct Chrome {
    size: (f64, f64),
    /// What the cursor is currently over, for hover tinting.
    hover: Option<Hit>,
    /// Toast ids presented last sync, so arrivals animate and the rest do not.
    shown_toasts: Vec<u64>,
    /// Last visibility pushed to the layers.
    ///
    /// Chrome position animates, but content does not: a tab title must not
    /// cross-fade on every keystroke. Comparing against the previous frame is
    /// what separates "the sidebar is tearing in" from "the sidebar is where it
    /// was and a title changed".
    last_vis: Option<compact_core::chrome::ChromeVisibility>,

    // Vertical sidebar.
    pub sidebar: Retained<NSVisualEffectView>,
    sidebar_root: Layer,
    ws_dot: Layer,
    ws_name: Layer,
    ws_count: Layer,
    side_rows: Vec<TabRow>,
    side_new_tab: Layer,
    side_new_label: Layer,
    side_new_hint: Layer,
    side_layout_btn: Layer,
    side_layout_label: Layer,
    side_gear: Layer,
    side_gear_label: Layer,
    elsewhere_label: Layer,
    elsewhere_hint: Layer,
    elsewhere_rows: Vec<TabRow>,

    // Horizontal strip.
    pub strip: Retained<NSVisualEffectView>,
    strip_root: Layer,
    strip_rows: Vec<TabRow>,
    strip_plus: Layer,
    strip_layout: Layer,
    strip_layout_label: Layer,
    strip_gear: Layer,

    // Address bar.
    pub address: Retained<NSVisualEffectView>,
    address_root: Layer,
    address_icon: Layer,
    pub address_field: Retained<NSTextField>,
    address_kbd: Layer,
    address_kbd_label: Layer,

    // Hint.
    pub hint: Retained<NSView>,
    hint_root: Layer,
    hint_text: Layer,
    hint_keys: Layer,

    // Toasts.
    pub toast_host: Retained<NSView>,
    toast_root: Layer,
    toasts: Vec<Toast>,
}

/// Layer host: a view whose backing layer we own and flip to top-left origin,
/// matching how the design expresses every coordinate.
fn host(mtm: MainThreadMarker, frame: CGRect) -> (Retained<NSView>, Layer) {
    let v = container(mtm, frame);
    let root = layer();
    unsafe {
        use objc2::msg_send;
        let _: () = msg_send![&*root, setGeometryFlipped: true];
    }
    set_view_layer(&v, &root);
    (v, root)
}

/// Move a chrome panel to `frame`, fading it as it goes.
///
/// Position and opacity run on different clocks by design: chrome reaches full
/// opacity well before it stops travelling, so it is readable the moment it is
/// worth reading. Hidden panels keep their frame set rather than being
/// `setHidden`, so there is something to animate *from* next time.
fn slide(view: &NSVisualEffectView, frame: CGRect, visible: bool, travel: f64, fade: f64, motion: bool) {
    let alpha = if visible { 1.0 } else { 0.0 };
    if motion {
        animate(travel, || view.animator().setFrame(frame));
        animate(fade, || view.animator().setAlphaValue(alpha));
    } else {
        without_animation(|| {
            view.setFrame(frame);
            view.setAlphaValue(alpha);
        });
    }
}

/// A view's backing layer, as the untyped pointer AppKit hands back.
fn host_layer<T: objc2::Message>(view: &T) -> *mut objc2::runtime::AnyObject {
    unsafe { objc2::msg_send![view, layer] }
}

fn flip_host(view: &NSVisualEffectView) -> Layer {
    let root = layer();
    unsafe {
        use objc2::msg_send;
        let _: () = msg_send![&*root, setGeometryFlipped: true];
        view.setWantsLayer(true);
        let hostl: *mut objc2::runtime::AnyObject = msg_send![view, layer];
        let _: () = msg_send![hostl, addSublayer: &*root];
    }
    root
}

impl Chrome {
    pub fn new(mtm: MainThreadMarker, size: (f64, f64)) -> Self {
        let _ = mtm;
        let zero = rect(0.0, 0.0, 0.0, 0.0);

        // ---- sidebar ----
        let sidebar = blur_view(mtm, zero, 0.0);
        let sidebar_root = flip_host(&sidebar);
        let ws_dot = panel(rect(6.0, 52.0, 7.0, 7.0), accent::PRIMARY, 3.5, None);
        let ws_name = text("", 12.5, Rgba::hex(0xD6D6DC), false, Align::Left);
        let ws_count = text("", 10.0, fg::DIM, true, Align::Right);
        let side_new_tab = panel(zero, Rgba::white(0.0), radius::TAB_SIDE, Some((Rgba::white(0.11), 1.0)));
        let side_new_label = text("+  New tab", 12.5, fg::MUTED, false, Align::Left);
        let side_new_hint = text("⌘T", 10.0, fg::FAINT, true, Align::Right);
        let side_layout_btn = panel(zero, Rgba::white(0.0), 7.0, Some((border::CARD, 1.0)));
        let side_layout_label = text("LAYOUT", 9.5, fg::MUTED, true, Align::Center);
        let side_gear = panel(zero, Rgba::white(0.0), 7.0, Some((border::CARD, 1.0)));
        let side_gear_label = text("⚙", 13.0, fg::MUTED, false, Align::Center);
        let elsewhere_label = text("ELSEWHERE", 9.5, fg::DIM, true, Align::Left);
        let elsewhere_hint = text("⌘⇧J", 9.5, fg::FAINT, true, Align::Right);
        for l in [
            &ws_dot, &ws_name, &ws_count, &side_new_tab, &side_new_label, &side_new_hint,
            &side_layout_btn, &side_layout_label, &side_gear, &side_gear_label,
            &elsewhere_label, &elsewhere_hint,
        ] {
            add_sublayer(&sidebar_root, l);
        }

        // ---- strip ----
        let strip = blur_view(mtm, zero, 0.0);
        let strip_root = flip_host(&strip);
        let strip_plus = text("+", 15.0, fg::MUTED, false, Align::Center);
        let strip_layout = panel(zero, Rgba::white(0.0), 7.0, Some((border::CARD, 1.0)));
        let strip_layout_label = text("LAYOUT", 10.0, fg::MUTED, true, Align::Center);
        let strip_gear = text("⚙", 13.0, fg::MUTED, false, Align::Center);
        for l in [&strip_plus, &strip_layout, &strip_layout_label, &strip_gear] {
            add_sublayer(&strip_root, l);
        }

        // ---- address bar ----
        let address = blur_view(mtm, zero, radius::ADDRESS);
        let address_root = flip_host(&address);
        let address_icon = text("⌁", 10.0, fg::DIM, false, Align::Center);
        let address_kbd = panel(zero, Rgba::white(0.0), 7.0, Some((Rgba::white(0.09), 1.0)));
        let address_kbd_label = text("⌘K", 10.0, fg::MUTED, true, Align::Center);
        for l in [&address_icon, &address_kbd, &address_kbd_label] {
            add_sublayer(&address_root, l);
        }
        let address_field = field(mtm, zero, 12.5, true, fg::INPUT);
        address.addSubview(&address_field);

        // ---- hint ----
        let (hint, hint_root) = host(mtm, zero);
        let hint_bg = panel(zero, bg::HINT, radius::HINT, Some((border::CHROME, 1.0)));
        add_sublayer(&hint_root, &hint_bg);
        let hint_text = text(
            "Move the cursor to an edge to reveal chrome",
            11.5,
            Rgba::hex(0x8A8A94),
            false,
            Align::Left,
        );
        let hint_keys = text("? shortcuts", 10.0, Rgba::hex(0x5A5A64), true, Align::Left);
        add_sublayer(&hint_bg, &hint_text);
        add_sublayer(&hint_bg, &hint_keys);
        // `cpBreathe`: the prompt pulses so it reads as a live hint rather than
        // a static label the eye stops seeing.
        breathe(&hint_text, 0.34, 0.62, compact_core::theme::duration::BREATHE);

        // ---- toasts ----
        let (toast_host, toast_root) = host(mtm, zero);

        Self {
            size,
            hover: None,
            shown_toasts: Vec::new(),
            last_vis: None,
            sidebar,
            sidebar_root,
            ws_dot,
            ws_name,
            ws_count,
            side_rows: Vec::new(),
            side_new_tab,
            side_new_label,
            side_new_hint,
            side_layout_btn,
            side_layout_label,
            side_gear,
            side_gear_label,
            elsewhere_label,
            elsewhere_hint,
            elsewhere_rows: Vec::new(),
            strip,
            strip_root,
            strip_rows: Vec::new(),
            strip_plus,
            strip_layout,
            strip_layout_label,
            strip_gear,
            address,
            address_root,
            address_icon,
            address_field,
            address_kbd,
            address_kbd_label,
            hint,
            hint_root: hint_bg,
            hint_text,
            hint_keys,
            toast_host,
            toast_root,
            toasts: Vec::new(),
        }
    }

    /// Views to mount, back to front.
    pub fn views(&self) -> Vec<Retained<NSView>> {
        vec![
            Retained::into_super(self.sidebar.clone()),
            Retained::into_super(self.strip.clone()),
            Retained::into_super(self.address.clone()),
            self.hint.clone(),
            self.toast_host.clone(),
        ]
    }

    pub fn set_size(&mut self, size: (f64, f64)) {
        self.size = size;
    }

    /// Update what the cursor is over. Returns true if it changed.
    pub fn set_hover(&mut self, h: Option<Hit>) -> bool {
        let changed = self.hover != h;
        self.hover = h;
        changed
    }

    /// Background tint for a row, given whether it is active and hovered.
    ///
    /// Hover only reads on inactive rows — tinting an already-tinted active row
    /// makes the selection ambiguous rather than responsive.
    fn row_fill(&self, active: bool, hovered: bool) -> Rgba {
        if active {
            fill::TAB_ACTIVE
        } else if hovered {
            fill::HOVER
        } else {
            Rgba::white(0.0)
        }
    }

    /// Push the whole of `app` into the layer tree.
    pub fn sync(&mut self, app: &App, now: u64) {
        let (w, h) = self.size;
        let vis = app.visibility();
        let cfg = &app.chrome;
        let moved = self.last_vis != Some(vis);
        self.last_vis = Some(vis);

        // The canvas tears chrome in over 0.44 s; honour that only when
        // visibility actually changed, and only when the user wants motion.
        let motion = moved && cfg.smooth_motion;

        let sw = metric::SIDEBAR_WIDTH;
        let sh = metric::STRIP_HEIGHT;
        let aw = (w * metric::ADDRESS_WIDTH_FRACTION).min(metric::ADDRESS_MAX_WIDTH);
        let ah = metric::ADDRESS_HEIGHT;
        let ax = (w - aw) / 2.0;
        let atop = vis.address_top();
        use compact_core::theme::duration as dur;

        // Sidebar slides in from the left edge.
        slide(
            &self.sidebar,
            rect(if vis.sidebar { 0.0 } else { -sw - 4.0 }, 0.0, sw, h),
            vis.sidebar,
            dur::CHROME,
            dur::CHROME_FADE,
            motion,
        );
        // Strip drops down from the top edge.
        slide(
            &self.strip,
            rect(0.0, if vis.strip { h - sh } else { h + 4.0 }, w, sh),
            vis.strip,
            dur::CHROME,
            dur::CHROME_FADE,
            motion,
        );
        // Address bar tears off from above the window edge, on its own longer
        // curve — the canvas gives it .52s against the chrome's .44s.
        slide(
            &self.address,
            rect(ax, if vis.address { h - atop - ah } else { h + ah }, aw, ah),
            vis.address,
            dur::ADDRESS,
            dur::ADDRESS_FADE,
            motion,
        );
        // ...and squashes vertically while it is away (`scaleY(.86)`), which is
        // what makes it read as torn off rather than merely moved.
        let squash = |l: &Layer| set_scale_y(l, if vis.address { 1.0 } else { 0.86 });
        if motion {
            animated(dur::ADDRESS, || squash(&self.address_root));
        } else {
            without_animation(|| squash(&self.address_root));
        }

        without_animation(|| {
            set_frame(&self.sidebar_root, rect(0.0, 0.0, sw, h));
            set_host_fill(host_layer(&*self.sidebar), bg::SIDEBAR);

            set_frame(&self.strip_root, rect(0.0, 0.0, w, sh));
            set_frame(&self.address_root, rect(0.0, 0.0, aw, ah));
            set_host_fill(host_layer(&*self.address), bg::RAISED);
            set_host_border(host_layer(&*self.address), border::RAISED, 1.0);
            set_frame(&self.address_icon, rect(9.0, 14.0, 12.0, 14.0));
            let kbd_w = 44.0;
            set_frame(&self.address_kbd, rect(aw - kbd_w - 6.0, 6.5, kbd_w, 27.0));
            set_frame(&self.address_kbd_label, rect(aw - kbd_w - 6.0, 14.0, kbd_w, 13.0));
            self.address_field
                .setFrame(rect(28.0, (ah - 18.0) / 2.0, aw - kbd_w - 42.0, 18.0));
            // Only overwrite the field while the user is not editing it.
            if app.address_draft.is_none() {
                let s = objc2_foundation::NSString::from_str(app.address_text());
                self.address_field.setStringValue(&s);
            }

            self.sync_sidebar(app, h);
            self.sync_strip(app, w);
            self.sync_hint(app, w, h);
            self.sync_toasts(app, now, w, h);

            let _ = cfg;
        });
    }

    fn sync_sidebar(&mut self, app: &App, h: f64) {
        let sw = metric::SIDEBAR_WIDTH;
        let inner = sw - 20.0;
        let ws = app.workspaces.current();

        set_fill(&self.ws_dot, ws.dot);
        set_frame(&self.ws_name, rect(27.0, 49.0, inner - 60.0, 16.0));
        set_text(&self.ws_name, &ws.name);
        set_frame(&self.ws_count, rect(sw - 70.0, 50.0, 60.0, 13.0));
        set_text(&self.ws_count, &ws.count_label());

        // Tab rows.
        let n = ws.tabs.len();
        while self.side_rows.len() < n {
            let r = TabRow::new(&self.sidebar_root, radius::TAB_SIDE);
            self.side_rows.push(r);
        }
        let rh = metric::TAB_HEIGHT_SIDE;
        let mut y = 76.0;
        for (i, row) in self.side_rows.iter().enumerate() {
            if i >= n {
                set_hidden(&row.root, true);
                continue;
            }
            let t = &ws.tabs[i];
            let on = t.id == ws.active;
            let hovered = matches!(self.hover, Some(Hit::SelectTab(h) | Hit::CloseTab(h)) if h == t.id);
            set_hidden(&row.root, false);
            set_frame(&row.root, rect(10.0, y, inner, rh));
            animated(compact_core::theme::duration::HOVER, || {
                set_fill(&row.root, self.row_fill(on, hovered));
            });
            set_border(&row.root, if on { border::TAB_ACTIVE } else { Rgba::white(0.0) }, 1.0);
            set_frame(&row.dot, rect(9.0, (rh - 5.0) / 2.0, 5.0, 5.0));
            set_fill(&row.dot, if on { ws.dot } else { fill::INACTIVE_DOT });
            set_frame(&row.title, rect(23.0, (rh - 15.0) / 2.0, inner - 50.0, 15.0));
            set_text(&row.title, &t.title);
            set_color(&row.title, if on { fg::PRIMARY } else { fg::SECONDARY });
            set_frame(&row.close, rect(inner - 24.0, (rh - 15.0) / 2.0, 16.0, 15.0));
            // A hibernated tab is a normal tab that happens to cost 2.3 KB, so
            // it is deliberately not badged — surfacing it would invite the user
            // to manage memory manually, which is the browser's job.
            y += rh + 3.0;
        }

        // ELSEWHERE.
        let elsewhere = app.workspaces.elsewhere();
        let show_elsewhere = !elsewhere.is_empty();
        set_hidden(&self.elsewhere_label, !show_elsewhere);
        set_hidden(&self.elsewhere_hint, !show_elsewhere);

        let footer_h = 27.0;
        let new_tab_y = h - footer_h - 12.0 - 32.0 - 8.0;
        let mut ey = new_tab_y - 10.0;

        while self.elsewhere_rows.len() < elsewhere.len() {
            let r = TabRow::new(&self.sidebar_root, radius::TAB_SIDE);
            self.elsewhere_rows.push(r);
        }
        if show_elsewhere {
            ey -= elsewhere.len() as f64 * 37.0;
            set_frame(&self.elsewhere_label, rect(16.0, ey - 18.0, 100.0, 13.0));
            set_frame(&self.elsewhere_hint, rect(sw - 60.0, ey - 18.0, 44.0, 13.0));
        }
        for (i, row) in self.elsewhere_rows.iter().enumerate() {
            match elsewhere.get(i) {
                None => set_hidden(&row.root, true),
                Some((_, w)) => {
                    set_hidden(&row.root, false);
                    set_frame(&row.root, rect(10.0, ey + i as f64 * 37.0, inner, 34.0));
                    set_fill(&row.root, Rgba::white(0.0));
                    set_border(&row.root, Rgba::white(0.0), 0.0);
                    set_frame(&row.dot, rect(9.0, 14.5, 5.0, 5.0));
                    set_fill(&row.dot, w.dot);
                    set_frame(&row.title, rect(23.0, 9.5, inner - 70.0, 15.0));
                    set_text(&row.title, &w.name);
                    set_color(&row.title, fg::SECONDARY);
                    set_frame(&row.close, rect(inner - 40.0, 8.0, 30.0, 18.0));
                    set_text(&row.close, &w.unread.to_string());
                    set_color(&row.close, bg::WINDOW);
                }
            }
        }

        // New tab + footer.
        set_frame(&self.side_new_tab, rect(10.0, new_tab_y, inner, 32.0));
        animated(compact_core::theme::duration::HOVER, || {
            set_fill(&self.side_new_tab, self.row_fill(false, self.hover == Some(Hit::NewTab)));
            set_fill(&self.side_layout_btn, self.row_fill(false, self.hover == Some(Hit::ToggleLayout)));
            set_fill(&self.side_gear, self.row_fill(false, self.hover == Some(Hit::Settings)));
        });
        set_frame(&self.side_new_label, rect(20.0, new_tab_y + 8.0, inner - 60.0, 16.0));
        set_frame(&self.side_new_hint, rect(sw - 50.0, new_tab_y + 9.0, 30.0, 14.0));

        let fy = h - footer_h - 12.0;
        set_frame(&self.side_layout_btn, rect(14.0, fy, inner - 39.0, footer_h));
        set_frame(&self.side_layout_label, rect(14.0, fy + 8.0, inner - 39.0, 13.0));
        set_frame(&self.side_gear, rect(sw - 14.0 - footer_h, fy, footer_h, footer_h));
        set_frame(&self.side_gear_label, rect(sw - 14.0 - footer_h, fy + 5.0, footer_h, 17.0));
    }

    fn sync_strip(&mut self, app: &App, w: f64) {
        let ws = app.workspaces.current();
        let n = ws.tabs.len();
        while self.strip_rows.len() < n {
            let r = TabRow::new(&self.strip_root, radius::TAB_STRIP);
            self.strip_rows.push(r);
        }
        let left = metric::TRAFFIC_LIGHT_INSET;
        let right_controls = 110.0;
        let avail = (w - left - right_controls - 40.0).max(0.0);
        let tw = (avail / n.max(1) as f64 - 8.0)
            .clamp(metric::TAB_MIN_WIDTH, metric::TAB_MAX_WIDTH);
        let rh = metric::TAB_HEIGHT_STRIP;
        let y = (metric::STRIP_HEIGHT - rh) / 2.0;

        for (i, row) in self.strip_rows.iter().enumerate() {
            if i >= n {
                set_hidden(&row.root, true);
                continue;
            }
            let t = &ws.tabs[i];
            let on = t.id == ws.active;
            let hovered = matches!(self.hover, Some(Hit::SelectTab(h) | Hit::CloseTab(h)) if h == t.id);
            let x = left + i as f64 * (tw + 8.0);
            set_hidden(&row.root, false);
            set_frame(&row.root, rect(x, y, tw, rh));
            animated(compact_core::theme::duration::HOVER, || {
                set_fill(&row.root, self.row_fill(on, hovered));
            });
            set_border(&row.root, if on { border::TAB_ACTIVE } else { Rgba::white(0.0) }, 1.0);
            set_frame(&row.dot, rect(9.0, (rh - 5.0) / 2.0, 5.0, 5.0));
            set_fill(&row.dot, if on { ws.dot } else { fill::INACTIVE_DOT });
            set_frame(&row.title, rect(22.0, (rh - 15.0) / 2.0, tw - 46.0, 15.0));
            set_text(&row.title, &t.title);
            set_color(&row.title, if on { fg::PRIMARY } else { fg::SECONDARY });
            set_frame(&row.close, rect(tw - 22.0, (rh - 15.0) / 2.0, 15.0, 15.0));
        }

        let plus_x = left + n as f64 * (tw + 8.0);
        set_frame(&self.strip_plus, rect(plus_x, y + 4.0, 26.0, 20.0));
        set_frame(&self.strip_layout, rect(w - 108.0, y + 1.5, 62.0, 26.0));
        set_frame(&self.strip_layout_label, rect(w - 108.0, y + 8.0, 62.0, 13.0));
        set_frame(&self.strip_gear, rect(w - 38.0, y + 5.0, 26.0, 18.0));
    }

    fn sync_hint(&mut self, app: &App, w: f64, _h: f64) {
        let show = app.hint_showing();
        self.hint.setHidden(!show);
        if !show {
            return;
        }
        let hw = 372.0;
        let hh = 34.0;
        let x = (w - hw) / 2.0;
        // Bottom-anchored: view frames use AppKit's bottom-left origin, unlike
        // the flipped layers inside them.
        self.hint.setFrame(rect(x, 22.0, hw, hh));
        set_frame(&self.hint_root, rect(0.0, 0.0, hw, hh));
        set_frame(&self.hint_text, rect(15.0, 10.0, 268.0, 15.0));
        set_frame(&self.hint_keys, rect(291.0, 11.0, 70.0, 13.0));
    }

    fn sync_toasts(&mut self, app: &App, now: u64, w: f64, _h: f64) {
        let items = app.notifications.items();
        let show = !items.is_empty() && app.overlay == Overlay::None;
        self.toast_host.setHidden(!show);
        if !show {
            return;
        }
        let tw = metric::TOAST_WIDTH;
        let th = 86.0;
        let stack_h = items.len() as f64 * (th + 9.0);
        let x = w - tw - 16.0;
        self.toast_host.setFrame(rect(x, 16.0, tw, stack_h));
        set_frame(&self.toast_root, rect(0.0, 0.0, tw, stack_h));

        while self.toasts.len() < items.len() {
            let t = Toast::new(&self.toast_root);
            self.toasts.push(t);
        }
        let previously = std::mem::take(&mut self.shown_toasts);
        self.shown_toasts = items.iter().map(|n| n.id).collect();
        for (i, toast) in self.toasts.iter().enumerate() {
            match items.get(i) {
                None => set_hidden(&toast.root, true),
                Some(n) => {
                    // `column-reverse`: the newest toast is on top and older
                    // ones settle beneath it.
                    let ty = (items.len() - 1 - i) as f64 * (th + 9.0);
                    // An arriving toast rises into place; ones already on
                    // screen just move as the stack reflows.
                    if !previously.contains(&n.id) {
                        without_animation(|| {
                            set_frame(&toast.root, rect(0.0, ty + 18.0, tw, th));
                            set_opacity(&toast.root, 0.0);
                        });
                    }
                    animated(compact_core::theme::duration::TOAST_IN, || {
                        set_frame(&toast.root, rect(0.0, ty, tw, th));
                        set_opacity(&toast.root, 1.0);
                    });
                    let dot = app.workspaces.spaces[n.workspace].dot;
                    set_hidden(&toast.root, false);
                    set_fill(&toast.dot, dot);
                    set_frame(&toast.ws, rect(27.0, 12.0, 120.0, 13.0));
                    set_text(&toast.ws, &app.workspaces.spaces[n.workspace].name.to_uppercase());
                    set_color(&toast.ws, dot);
                    // Age sits left of the dismiss glyph, which is the
                    // rightmost thing in the header.
                    set_frame(&toast.age, rect(tw - 76.0, 12.0, 46.0, 13.0));
                    set_text(&toast.age, &n.age_label(now));
                    set_frame(&toast.close, rect(tw - 29.0, 11.0, 16.0, 16.0));
                    set_frame(&toast.body, rect(13.0, 32.0, tw - 26.0, 18.0));
                    set_text(&toast.body, &n.text);
                    set_frame(&toast.source, rect(13.0, 58.0, tw - 110.0, 15.0));
                    set_text(&toast.source, &n.source);
                    set_frame(&toast.action, rect(tw - 100.0, 58.0, 87.0, 15.0));
                    set_text(&toast.action, &n.action_label(app.workspaces.current));
                }
            }
        }
    }

    /// Hit-test the toast stack. Returns the notification and whether the
    /// click landed on its dismiss affordance rather than its body.
    ///
    /// Tested before the rest of the chrome because toasts float above
    /// everything else and are the topmost thing under the cursor.
    pub fn toast_hit(&self, app: &App, x: f64, y: f64) -> Option<(u64, bool)> {
        let (w, h) = self.size;
        let items = app.notifications.items();
        if items.is_empty() || app.overlay != Overlay::None {
            return None;
        }
        let tw = metric::TOAST_WIDTH;
        let th = 86.0;
        let stack_h = items.len() as f64 * (th + 9.0);
        let x0 = w - tw - 16.0;
        // The host view is bottom-anchored; convert to a top-left offset.
        let top = h - 16.0 - stack_h;
        if x < x0 || x > x0 + tw || y < top || y > top + stack_h {
            return None;
        }
        let lx = x - x0;
        for (i, n) in items.iter().enumerate() {
            // Newest sits at the bottom, so index maps to reverse order.
            let ty = top + (items.len() - 1 - i) as f64 * (th + 9.0);
            if y >= ty && y < ty + th {
                let dismiss = lx > tw - 32.0 && (y - ty) < 30.0;
                return Some((n.id, dismiss));
            }
        }
        None
    }

    /// Hit-test the chrome. Returns what the click landed on, in window
    /// coordinates with a top-left origin.
    pub fn hit(&self, app: &App, x: f64, y: f64) -> Option<Hit> {
        let (w, h) = self.size;
        let vis = app.visibility();
        let ws = app.workspaces.current();

        if vis.sidebar && x < metric::SIDEBAR_WIDTH {
            let inner = metric::SIDEBAR_WIDTH - 20.0;
            let rh = metric::TAB_HEIGHT_SIDE;
            let mut ry = 76.0;
            for t in &ws.tabs {
                if y >= ry && y < ry + rh && x >= 10.0 {
                    return Some(if x > 10.0 + inner - 26.0 {
                        Hit::CloseTab(t.id)
                    } else {
                        Hit::SelectTab(t.id)
                    });
                }
                ry += rh + 3.0;
            }
            let footer_h = 27.0;
            let new_tab_y = h - footer_h - 12.0 - 32.0 - 8.0;
            if y >= new_tab_y && y < new_tab_y + 32.0 {
                return Some(Hit::NewTab);
            }
            let fy = h - footer_h - 12.0;
            if y >= fy && y < fy + footer_h {
                return Some(if x > metric::SIDEBAR_WIDTH - 14.0 - footer_h {
                    Hit::Settings
                } else {
                    Hit::ToggleLayout
                });
            }
            return Some(Hit::Chrome);
        }

        if vis.strip && y < metric::STRIP_HEIGHT {
            let n = ws.tabs.len();
            let left = metric::TRAFFIC_LIGHT_INSET;
            let avail = (w - left - 110.0 - 40.0).max(0.0);
            let tw = (avail / n.max(1) as f64 - 8.0)
                .clamp(metric::TAB_MIN_WIDTH, metric::TAB_MAX_WIDTH);
            for (i, t) in ws.tabs.iter().enumerate() {
                let tx = left + i as f64 * (tw + 8.0);
                if x >= tx && x < tx + tw {
                    return Some(if x > tx + tw - 24.0 {
                        Hit::CloseTab(t.id)
                    } else {
                        Hit::SelectTab(t.id)
                    });
                }
            }
            let plus_x = left + n as f64 * (tw + 8.0);
            if x >= plus_x && x < plus_x + 26.0 {
                return Some(Hit::NewTab);
            }
            if x > w - 108.0 && x < w - 46.0 {
                return Some(Hit::ToggleLayout);
            }
            if x > w - 38.0 {
                return Some(Hit::Settings);
            }
            return Some(Hit::Chrome);
        }

        if vis.address {
            let aw = (w * metric::ADDRESS_WIDTH_FRACTION).min(metric::ADDRESS_MAX_WIDTH);
            let ax = (w - aw) / 2.0;
            let atop = vis.address_top();
            if y >= atop && y < atop + metric::ADDRESS_HEIGHT && x >= ax && x < ax + aw {
                return Some(if x > ax + aw - 56.0 { Hit::Palette } else { Hit::Address });
            }
        }
        None
    }

}

/// What a click in the chrome resolved to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    SelectTab(compact_core::tab::TabId),
    CloseTab(compact_core::tab::TabId),
    NewTab,
    ToggleLayout,
    Settings,
    Palette,
    Address,
    /// Landed on chrome but not on a control — swallow it so it does not reach
    /// the page underneath.
    Chrome,
}
